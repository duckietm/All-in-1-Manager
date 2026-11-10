//! Asset tools: decompile SWF files (debugging), decompile and compile HAB files (editing), and SWF / NITRO to HAB
//! per asset type, all in Habbo_Assets/Assets_tools on this computer. (All-in-1 Converter: Hotel Tools) And the
//! converter's drop folder: .swf and .nitro in Habbo_Assets/Convert/import/<type>, their .hab in Convert/compiled/<type>.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aio_assets::workshop::{self, Outcome};
use aio_assets::AssetKind;
use eframe::egui::{self, RichText};
use parking_lot::Mutex;

use crate::gui::log_view::{self, LogLines};
use crate::gui::theme::Palette;
use crate::gui::widgets::{self, ResultKind};
use crate::gui::{Shared, View};
use crate::i18n::{n, t, tf};

/// The folder of the asset tools below the assets folder.
pub const TOOLS_FOLDER: &str = "Assets_tools";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    DecompileSwf,
    DecompileHab,
    CompileHab,
    Convert(AssetKind, Source),
    /// Convert/import to Convert/compiled: one type, or None for every type.
    Import(Option<AssetKind>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Swf,
    Nitro,
}

fn kind_title(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Furniture => n("Furniture"),
        AssetKind::Clothes => n("Clothes"),
        AssetKind::Pets => n("Pets"),
        AssetKind::Effects => n("Effects"),
    }
}

/// The folder a retro profile keeps its .swf files of a type in (custom/<name>/source/<folder>).
fn retro_source_folder(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Furniture => "furniture",
        AssetKind::Clothes => "clothes",
        AssetKind::Pets => "pets",
        AssetKind::Effects => "effect",
    }
}

#[derive(Default)]
struct State {
    running: bool,
    stop: Arc<AtomicBool>,
    log: LogLines,
    result: Option<(ResultKind, String, String)>,
}

pub struct AssetToolsView {
    state: Arc<Mutex<State>>,
    /// None: Original_SWF_Files; Some(name): the .swf a Nitro retro profile kept.
    swf_source: Option<String>,
    overwrite: bool,
}

fn open_folder(path: &Path) {
    let _ = std::fs::create_dir_all(path);
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
    #[cfg(not(windows))]
    crate::gui::open_url(&path.display().to_string());
}

impl AssetToolsView {
    pub fn new(_shared: &Shared) -> Self {
        Self { state: Arc::new(Mutex::new(State { log: LogLines::with_limit(4000), ..State::default() })), swf_source: None, overwrite: false }
    }

    fn root() -> PathBuf {
        aio_core::InstallerSettings::load().assets_folder().join(TOOLS_FOLDER)
    }

    /// The drop folder: Convert/import and Convert/compiled.
    fn convert_root() -> PathBuf {
        aio_core::InstallerSettings::load().assets_folder().join(workshop::CONVERT)
    }

    fn title(tool: Tool) -> String {
        match tool {
            Tool::DecompileSwf => t("Decompile SWF files").into(),
            Tool::DecompileHab => t("Decompile HAB files").into(),
            Tool::CompileHab => t("Compile HAB files").into(),
            Tool::Convert(kind, Source::Swf) => tf("SWF {type} to HAB", &[("type", t(kind_title(kind)))]),
            Tool::Convert(kind, Source::Nitro) => tf("NITRO {type} to HAB", &[("type", t(kind_title(kind)))]),
            Tool::Import(None) => t("Convert everything").into(),
            Tool::Import(Some(kind)) => t(kind_title(kind)).into(),
        }
    }

    fn start(&mut self, shared: &Shared, tool: Tool) {
        let stop = Arc::new(AtomicBool::new(false));
        {
            let mut state = self.state.lock();
            if state.running {
                return;
            }
            state.running = true;
            state.stop = stop.clone();
            state.result = None;
            state.log.clear();
        }
        let root = Self::root();
        let _ = workshop::create_layout(&root);
        let convert_root = Self::convert_root();
        let _ = workshop::create_convert_layout(&convert_root);
        let assets = aio_core::InstallerSettings::load().assets_folder();
        let retro = self.swf_source.clone().and_then(|name| {
            aio_core::InstallerSettings::load().retro_profiles().into_iter().find(|p| p.name == name)
        });
        let overwrite = self.overwrite;
        let title = Self::title(tool);
        let (state, ctx, task_shared) = (self.state.clone(), shared.ctx.clone(), shared.clone());
        shared.set_status(&tf("{title} ...", &[("title", &title)]));
        shared.spawn(async move {
            let log_state = state.clone();
            let log = move |line: &str| {
                log_state.lock().log.push(line);
                ctx.request_repaint();
            };
            let stopped = {
                let stop = stop.clone();
                move || stop.load(Ordering::Relaxed)
            };
            let work_title = title.clone();
            let outcome = tokio::task::spawn_blocking(move || -> (String, Outcome) {
                log(&format!("> {work_title}"));
                let mut total = Outcome::default();
                let mut add = |o: Outcome| {
                    total.done += o.done;
                    total.skipped += o.skipped;
                    total.failed += o.failed;
                    total.errors.extend(o.errors);
                };
                let what = match tool {
                    Tool::DecompileSwf | Tool::DecompileHab | Tool::CompileHab => {
                        let (from, to) = match tool {
                            Tool::DecompileSwf => (workshop::ORIGINAL_SWF, workshop::DECOMPILED_SWF),
                            Tool::DecompileHab => (workshop::CONVERTED, workshop::DECOMPILED_HAB),
                            _ => (workshop::DECOMPILED_HAB, workshop::COMPILED_HAB),
                        };
                        for (_, folder) in workshop::TYPES {
                            let (source, output) = (root.join(from).join(folder), root.join(to).join(folder));
                            log(&format!("  {} → {}", source.display(), output.display()));
                            add(match tool {
                                Tool::DecompileSwf => workshop::decompile_swf_folder(&source, &output, &stopped, &log),
                                Tool::DecompileHab => workshop::decompile_hab_folder(&source, &output, &stopped, &log),
                                _ => workshop::compile_hab_folder(&source, &output, &stopped, &log),
                            });
                        }
                        match tool {
                            Tool::DecompileSwf => n("SWF files decompiled"),
                            Tool::DecompileHab => n("HAB files decompiled"),
                            _ => n("HAB files compiled"),
                        }
                    }
                    Tool::Convert(kind, source) => {
                        let folder = workshop::type_folder(kind);
                        let (input, extension) = match (source, &retro) {
                            (Source::Swf, Some(profile)) => {
                                (aio_remote::habbo_assets::retro::custom_folder(&assets, profile).join("source").join(retro_source_folder(kind)), "swf")
                            }
                            (Source::Swf, None) => (root.join(workshop::ORIGINAL_SWF).join(folder), "swf"),
                            (Source::Nitro, _) => (root.join(workshop::ORIGINAL_NITRO).join(folder), "nitro"),
                        };
                        let output = root.join(workshop::CONVERTED).join(folder);
                        log(&format!("  {} → {}", input.display(), output.display()));
                        add(workshop::convert_folder(&input, &output, kind, extension, overwrite, &stopped, &log));
                        n("converted to .hab")
                    }
                    Tool::Import(only) => {
                        for (kind, folder) in workshop::TYPES.into_iter().filter(|(k, _)| only.is_none_or(|o| o == *k)) {
                            let input = convert_root.join(workshop::IMPORT).join(folder);
                            let output = convert_root.join(workshop::COMPILED).join(folder);
                            log(&format!("  {} → {}", input.display(), output.display()));
                            add(workshop::convert_import_folder(&input, &output, kind, overwrite, &stopped, &log));
                        }
                        n("converted to .hab")
                    }
                };
                (what.to_string(), total)
            })
            .await;
            let stopped = stop.load(Ordering::Relaxed);
            let mut s = state.lock();
            s.result = Some(match outcome {
                Ok((what, o)) => {
                    let mut message = o.summary(t(&what));
                    if o.done + o.skipped + o.failed == 0 {
                        message = t("Nothing found: put the files in the folder shown in the log and run it again.").into();
                    }
                    let kind = if stopped || o.failed > 0 || o.done + o.skipped == 0 { ResultKind::Warning } else { ResultKind::Success };
                    let heading = if stopped { t("Stopped").to_string() } else { title.clone() };
                    (kind, heading, message)
                }
                Err(e) => (ResultKind::Error, title.clone(), e.to_string()),
            });
            s.running = false;
            drop(s);
            task_shared.set_status(t("Ready."));
        });
    }
}

impl View for AssetToolsView {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        let running = self.state.lock().running;
        let root = Self::root();
        let margin = egui::Margin::symmetric(24, 20);
        let mut start = None;

        let convert_root = Self::convert_root();
        widgets::card(ui, palette, margin, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(widgets::title_text(t("Convert SWF and NITRO to HAB"), 15.0));
            widgets::muted(
                ui,
                palette,
                &tf(
                    "Put .swf and .nitro files in {import} (one folder per type: furni, clothes, pets, effects). The .hab files come in {compiled}, with everything the client needs (the JSON and the sprite sheet). A .nitro and a .swf of the same name: the .nitro is used.",
                    &[
                        ("import", &convert_root.join(workshop::IMPORT).join("<type>").display().to_string()),
                        ("compiled", &convert_root.join(workshop::COMPILED).join("<type>").display().to_string()),
                    ],
                ),
                12.5,
            );
            ui.add_enabled_ui(!running, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.add(widgets::primary_button(&Self::title(Tool::Import(None)))).clicked() {
                        start = Some(Tool::Import(None));
                    }
                    for (kind, _) in workshop::TYPES {
                        let tool = Tool::Import(Some(kind));
                        if ui.add(widgets::secondary_button(&Self::title(tool)).min_size(egui::vec2(110.0, 34.0))).clicked() {
                            start = Some(tool);
                        }
                    }
                });
                ui.checkbox(&mut self.overwrite, t("Replace .hab files that are already there"));
            });
            ui.horizontal(|ui| {
                for (half, label) in [(workshop::IMPORT, t("Open import")), (workshop::COMPILED, t("Open compiled"))] {
                    if ui.add(widgets::secondary_button(label)).clicked() {
                        let _ = workshop::create_convert_layout(&convert_root);
                        open_folder(&convert_root.join(half));
                    }
                }
            });
        });
        ui.add_space(18.0);

        widgets::card(ui, palette, margin, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(widgets::title_text(t("Folders"), 15.0));
            widgets::muted(ui, palette, &tf("Everything happens in {folder}, on this computer:", &[("folder", &root.display().to_string())]), 12.5);
            let tree = [
                (workshop::ORIGINAL_SWF, t("put .swf files here, per type (furni, clothes, pets, effects)")),
                (workshop::ORIGINAL_NITRO, t("put .nitro files here, per type")),
                (workshop::CONVERTED, t("the .hab files SWF / NITRO to HAB makes, per type")),
                (workshop::DECOMPILED_SWF, t("debugging: the images, XML and symbols of each .swf")),
                (workshop::DECOMPILED_HAB, t("a folder per .hab: its .json and image, to edit")),
                (workshop::COMPILED_HAB, t("the edited folders packed as .hab again")),
            ];
            egui::Grid::new("asset_tools_tree").num_columns(2).spacing(egui::vec2(14.0, 4.0)).show(ui, |ui| {
                for (folder, what) in tree {
                    ui.label(RichText::new(format!("{folder}\\")).monospace().size(12.0));
                    ui.label(RichText::new(what).size(12.0).color(palette.muted));
                    ui.end_row();
                }
            });
            if ui.add(widgets::secondary_button(t("Open the folder"))).clicked() {
                let _ = workshop::create_layout(&root);
                open_folder(&root);
            }
        });
        ui.add_space(18.0);

        widgets::card(ui, palette, margin, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(widgets::title_text(t("Debug and edit"), 15.0));
            for (tool, what) in [
                (Tool::DecompileSwf, t("Original_SWF_Files → Decompiled_SWF_Files: every image, XML and symbol of each .swf, for debugging.")),
                (Tool::DecompileHab, t("converted → Decompiled_HAB_Files: each .hab as its .json and image, to edit.")),
                (Tool::CompileHab, t("Decompiled_HAB_Files → Compiled_HAB_Files: each edited folder packed as .hab again.")),
            ] {
                ui.horizontal(|ui| {
                    if ui.add_enabled(!running, widgets::secondary_button(&Self::title(tool)).min_size(egui::vec2(190.0, 34.0))).clicked() {
                        start = Some(tool);
                    }
                    ui.add(egui::Label::new(RichText::new(what).size(12.0).color(palette.muted)).wrap());
                });
            }
        });
        ui.add_space(18.0);

        widgets::card(ui, palette, margin, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(widgets::title_text(t("Convert to .hab from other folders"), 15.0));
            widgets::muted(ui, palette, t("The .hab files go to converted\\<type>. The .swf can come from Original_SWF_Files or from a Nitro retro profile (the .swf it kept)."), 12.5);
            let profiles = aio_core::InstallerSettings::load().retro_profiles();
            ui.add_enabled_ui(!running, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(t("SWF from:")).size(12.5));
                    ui.radio_value(&mut self.swf_source, None, t("Original (Original_SWF_Files)"));
                    for profile in &profiles {
                        ui.radio_value(&mut self.swf_source, Some(profile.name.clone()), tf("Custom: {name}", &[("name", &profile.name)]));
                    }
                });
                if self.swf_source.as_ref().is_some_and(|name| !profiles.iter().any(|p| &p.name == name)) {
                    self.swf_source = None;
                }
                ui.checkbox(&mut self.overwrite, t("Replace .hab files that are already there"));
            });
            egui::Grid::new("asset_tools_convert").num_columns(3).spacing(egui::vec2(12.0, 8.0)).show(ui, |ui| {
                for (kind, _) in workshop::TYPES {
                    ui.label(RichText::new(t(kind_title(kind))).size(13.5).strong());
                    for source in [Source::Swf, Source::Nitro] {
                        let tool = Tool::Convert(kind, source);
                        if ui.add_enabled(!running, widgets::secondary_button(&Self::title(tool)).min_size(egui::vec2(210.0, 32.0))).clicked() {
                            start = Some(tool);
                        }
                    }
                    ui.end_row();
                }
            });
        });
        ui.add_space(18.0);

        let has_log = !self.state.lock().log.is_empty();
        if has_log || running {
            widgets::card(ui, palette, margin, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let result = self.state.lock().result.clone();
                if let Some((kind, heading, message)) = result.filter(|_| !running) {
                    widgets::result_banner(ui, palette, kind, &heading, &message);
                }
                ui.horizontal(|ui| {
                    if running {
                        if ui.add(widgets::secondary_button(t("Stop"))).clicked() {
                            self.state.lock().stop.store(true, Ordering::Relaxed);
                        }
                        ui.spinner();
                    } else if ui.add(widgets::secondary_button(t("Done"))).on_hover_text(t("Clears the result and the log, so you can start over.")).clicked() {
                        let mut state = self.state.lock();
                        state.log.clear();
                        state.result = None;
                    }
                });
                let mut state = self.state.lock();
                log_view::show(ui, "asset_tools_log", &mut state.log, Some(300.0), t("The log appears here."));
            });
        }
        if let Some(tool) = start {
            self.start(shared, tool);
        }
    }

    fn busy(&self) -> bool {
        self.state.lock().running
    }
}

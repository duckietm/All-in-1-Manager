//! What every tool of the Tools page does, in the order of ToolsView.cs: its question, the check list of its run
//! panel, the remote calls, the log lines and the result. A tool returns an error to fail; the page shows it on the
//! run panel (or as a banner when nothing ran yet).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use aio_core::config_dir;
use aio_core::layout::AtomCmsPresence;
use aio_core::{backup, DatabaseCredentials, ServerLayout};
use aio_remote::fail2ban::Fail2BanSettings;
use aio_remote::plus_import::PlusImportMode;
use aio_remote::{
    atom_cms, crowdsec, emulator, emulator_assets, fail, fail2ban, git_clone, gitea, harden, hotel_site, mariadb, octane, plus_import,
    server_status, server_tools, web_access, Server, ServerError,
};

use super::modals::{self, WhitelistQuestion};
use super::{check_quietly, confirm, load_images, progress_step, show_updates, size, Host, SharedState, Tool};
use crate::gui::dialogs::Choice;
use crate::gui::step::{Banner, StepUi};
use crate::gui::widgets::ResultKind;
use crate::gui::Shared;
use crate::i18n::{n, t, tf};

type Result<T> = anyhow::Result<T>;

/// A running tool: the login, the page and the tool's title.
pub struct Job {
    pub shared: Shared,
    pub state: SharedState,
    pub server: Arc<dyn Server>,
    pub title: String,
}

impl Job {
    fn server(&self) -> &dyn Server {
        self.server.as_ref()
    }

    fn host(&self) -> String {
        self.server.login().host.clone()
    }

    /// The run panel under the tiles, with these checks (English keys). (StartRun)
    fn start_run(&self, checks: &[&str]) -> StepUi {
        let run = StepUi::new(
            self.shared.ctx.clone(),
            self.shared.dialogs.clone(),
            Arc::new(std::sync::Mutex::new(String::new())),
            &self.title,
            checks.iter().map(|c| t(c).to_string()).collect(),
        );
        run.start();
        let mut state = self.state.lock().unwrap();
        state.host = Some(Host::Run { title: self.title.clone(), run: run.clone() });
        state.current_run = Some(run.clone());
        state.scroll_to_result = true;
        run
    }

    /// For a tool that was cancelled or refused before anything ran. (Show)
    pub fn show(&self, kind: ResultKind, heading: &str, message: &str) {
        let mut state = self.state.lock().unwrap();
        state.host = Some(Host::Banner { title: self.title.clone(), banner: Banner { kind, heading: heading.into(), message: message.into() } });
        state.scroll_to_result = true;
        drop(state);
        self.shared.ctx.request_repaint();
    }

    async fn confirm(&self, heading: &str, message: &str, go: &str) -> bool {
        confirm(&self.shared.dialogs, heading, message, go).await
    }

    async fn check_quietly(&self) {
        check_quietly(&self.shared, &self.state, &self.server).await;
    }
}

/// Logs a transfer at every 10 %.
fn progress(run: &StepUi, total: i64) -> impl Fn(u64) + Send + Sync + '_ {
    let reported = AtomicI64::new(0);
    move |done: u64| {
        if let Some(step) = progress_step(done, total, reported.load(Ordering::Relaxed)) {
            reported.store(step, Ordering::Relaxed);
            run.log(&format!("  {step} %  ({} of {})", size(done as i64), size(total)));
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn file_size(path: &Path) -> i64 {
    std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0)
}

/// ".sql.gz" for a compressed dump, ".sql" otherwise.
fn dump_extension(path: &Path) -> &'static str {
    if path.to_string_lossy().to_lowercase().ends_with(".gz") {
        ".sql.gz"
    } else {
        ".sql"
    }
}

async fn pick_dump(title: &str) -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_title(title)
        .add_filter(t("SQL dumps"), &["gz", "sql"])
        .add_filter(t("All files"), &["*"])
        .pick_file()
        .await
        .map(|f| f.path().to_path_buf())
}

pub async fn perform(tool: Tool, job: &Job) -> Result<()> {
    match tool {
        Tool::HealthCheck | Tool::ServerStatus | Tool::EmulatorMonitor | Tool::Terminal => Ok(()),
        Tool::RestartEmulator => restart_emulator(job).await,
        Tool::RestartMariaDb => restart_mariadb(job).await,
        Tool::RestartNginx => restart_nginx(job).await,
        Tool::RestartTraefik => restart_traefik(job).await,
        Tool::RestartAll => restart_all(job).await,
        Tool::FixVisitorIps => fix_visitor_ips(job).await,
        Tool::EnableFirewall => enable_firewall(job).await,
        Tool::DisableFirewall => disable_firewall(job).await,
        Tool::Fail2Ban => fail2ban_tool(job).await,
        Tool::CrowdSec => crowdsec_tool(job).await,
        Tool::BackupDatabase => backup_database(job).await,
        Tool::ImportDatabase => import_database(job).await,
        Tool::ImportPlus => import_plus(job).await,
        Tool::FurnitureLanguage => furniture_language_tool(job).await,
        Tool::RecoverSqlPassword => recover_sql_password(job).await,
        Tool::CheckUpdates => check_updates(job).await,
        Tool::UpdateSystem => update_system(job).await,
        Tool::UpdateEmulator => update_emulator(job).await,
        Tool::UpdateOctane => update_octane(job).await,
        Tool::UpdateAtomCms => update_atom_cms(job).await,
        Tool::UpdateTraefik => update_traefik(job).await,
        Tool::UpdateGitea => update_gitea(job).await,
        Tool::UpdateCrowdSec => update_crowdsec(job).await,
    }
}

async fn restart_emulator(job: &Job) -> Result<()> {
    if !job
        .confirm(
            t("Restart the emulator?"),
            t("Everyone in the hotel is disconnected until it has loaded again (about a minute)."),
            t("Restart the emulator"),
        )
        .await
    {
        return Ok(());
    }
    let run = job.start_run(&[n("Restart the emulator")]);
    let log = run.logger();
    server_tools::restart_emulator(job.server(), &log).await?;
    run.done(t("loaded"));
    run.result(ResultKind::Success, t("The emulator is running again"), t("It has loaded; players can log in again."));
    Ok(())
}

async fn restart_mariadb(job: &Job) -> Result<()> {
    let run = job.start_run(&[n("Restart MariaDB")]);
    let log = run.logger();
    server_tools::restart_mariadb(job.server(), &log).await?;
    run.done(t("running"));
    run.result(
        ResultKind::Success,
        t("MariaDB is running again"),
        t("The emulator and AtomCMS reconnect by themselves. If the hotel acts up, restart the emulator too."),
    );
    Ok(())
}

async fn restart_nginx(job: &Job) -> Result<()> {
    let run = job.start_run(&[n("Restart NGINX and PHP-FPM")]);
    let log = run.logger();
    server_tools::restart_nginx(job.server(), &log).await?;
    run.done(t("running"));
    run.result(ResultKind::Success, t("NGINX is running again"), &tf("The site answers on 127.0.0.1:{port}.", &[("port", hotel_site::PORT)]));
    Ok(())
}

async fn restart_traefik(job: &Job) -> Result<()> {
    let run = job.start_run(&[n("Restart Traefik")]);
    let log = run.logger();
    server_tools::restart_traefik(job.server(), &log).await?;
    run.done(t("running"));
    run.result(ResultKind::Success, t("Traefik is running again"), t("It answers on ports 80 and 443 again."));
    Ok(())
}

async fn enable_firewall(job: &Job) -> Result<()> {
    let run = job.start_run(&[n("Enable the firewall")]);
    let log = run.logger();
    let fresh = harden::enable_firewall(job.server(), &log).await?;
    run.done(t("active"));
    let first = if fresh { t("The Harden step's rules are set up: ") } else { t("The Harden step's rules are active again: ") };
    let rest = tf(
        "SSH (port {port}, rate limited), HTTP and HTTPS (only from Cloudflare when the site uses it) are open; MariaDB and the emulator's ports are closed (HeidiSQL works through an SSH tunnel).",
        &[("port", &job.server.login().port.to_string())],
    );
    run.result(ResultKind::Success, t("The firewall is on"), &format!("{first}{rest}"));
    Ok(())
}

async fn disable_firewall(job: &Job) -> Result<()> {
    if !job
        .confirm(
            t("Switch the firewall off?"),
            t("Every port on the server can be reached from the internet again, also the emulator's own ports (3000, 2096) and anything else that listens. Use it to test a connection problem, and switch it on again afterwards. The rules are kept."),
            t("Disable firewall"),
        )
        .await
    {
        return Ok(());
    }
    let run = job.start_run(&[n("Disable the firewall")]);
    let log = run.logger();
    harden::disable_firewall(job.server(), &log).await?;
    run.done(t("inactive"));
    run.result(ResultKind::Warning, t("The firewall is off"), t("All ports are open. Click Enable firewall to switch the same rules on again."));
    Ok(())
}

async fn restart_all(job: &Job) -> Result<()> {
    if !job
        .confirm(
            t("Restart everything?"),
            t("MariaDB, the emulator, NGINX (with PHP-FPM) and Traefik restart one after the other. Everyone in the hotel is disconnected."),
            t("Restart all"),
        )
        .await
    {
        return Ok(());
    }
    let run = job.start_run(&[n("MariaDB"), n("Emulator"), n("NGINX and PHP-FPM"), n("Traefik")]);
    let log = run.logger();
    let server = job.server();
    // MariaDB first, so the emulator starts with fresh connections.
    server_tools::restart_mariadb(server, &log).await?;
    run.done(t("running"));
    server_tools::restart_emulator(server, &log).await?;
    run.done(t("loaded"));
    server_tools::restart_nginx(server, &log).await?;
    run.done(t("running"));
    server_tools::restart_traefik(server, &log).await?;
    run.done(t("running"));
    run.result(ResultKind::Success, t("Everything is running again"), t("MariaDB, the emulator, NGINX and Traefik were restarted."));
    Ok(())
}

/// The result text of the whitelist tools.
fn whitelist_message(saved: Option<&[String]>, unbanned: &[String], client_missing: Option<&str>, crowdsec: bool) -> String {
    let mut message = String::new();
    if let Some(whitelist) = saved {
        message += &if whitelist.is_empty() {
            if crowdsec { t("Nothing is whitelisted. ") } else { t("Only the server itself is never banned. ") }.to_string()
        } else if crowdsec {
            tf("Never banned: {list} (Traefik reads it without a restart). ", &[("list", &whitelist.join(", "))])
        } else {
            tf("Never banned: {list}. ", &[("list", &whitelist.join(", "))])
        };
    }
    if !unbanned.is_empty() {
        let list = unbanned.join(", ");
        message += &if crowdsec {
            tf("Let in again: {list} (the bouncer knows within a minute). ", &[("list", &list)])
        } else {
            tf("Let in again: {list}. ", &[("list", &list)])
        };
    }
    if let Some(ip) = client_missing {
        message += &tf("This computer ({ip}) is not on the whitelist.", &[("ip", ip)]);
    }
    message.trim_end().to_string()
}

async fn fail2ban_tool(job: &Job) -> Result<()> {
    let server = job.server();
    let state = fail2ban::inspect(server).await?;
    if !state.installed || !state.running {
        job.show(
            ResultKind::Warning,
            if state.installed { t("Fail2ban does not run") } else { t("Fail2ban is not installed") },
            t("Install it first: Addons → Fail2Ban → Install. It protects SSH and sets up the whitelist."),
        );
        return Ok(());
    }
    let ports = if state.ssh_ports.is_empty() { "22".to_string() } else { state.ssh_ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ") };
    let status = tf(
        "Fail2ban {version} protects SSH (port {ports}): {banned} banned now, {total} since its last restart.",
        &[
            ("version", state.version.as_deref().unwrap_or_default()),
            ("ports", &ports),
            ("banned", &state.banned.len().to_string()),
            ("total", &state.total_banned.to_string()),
        ],
    );
    let current = state.settings.as_ref().map(|s| s.whitelist.clone()).unwrap_or_default();
    let question = WhitelistQuestion {
        title: "Fail2Ban",
        status,
        current: current.clone(),
        configured: state.configured,
        banned: state.banned.clone(),
        client_ip: state.client_ip.clone(),
    };
    let Some(changes) = modals::ask_whitelist(&job.state, &job.shared.ctx, question).await else { return Ok(()) };
    // Not set up by the installer yet: the defaults, with the whitelist from the dialog.
    let settings = Fail2BanSettings {
        whitelist: changes.whitelist.clone().unwrap_or(current),
        ..state.settings.clone().unwrap_or_else(fail2ban::defaults)
    };
    let ssh_ports: Vec<i32> = if state.ssh_ports.is_empty() { vec![job.server.login().port as i32] } else { state.ssh_ports.clone() };
    let mut checks = Vec::new();
    if changes.whitelist.is_some() {
        checks.push(n("Save the whitelist and restart fail2ban"));
    }
    if !changes.unban.is_empty() {
        checks.push(n("Unban"));
    }
    let run = job.start_run(&checks);
    let log = run.logger();
    if changes.whitelist.is_some() {
        fail2ban::save(server, &settings, &ssh_ports, &log).await?;
        run.done(&tf("{count} entries", &[("count", &settings.whitelist.len().to_string())]));
    }
    for ip in &changes.unban {
        fail2ban::unban(server, ip, &log).await?;
    }
    if !changes.unban.is_empty() {
        run.done(&changes.unban.join(", "));
    }
    let client_missing = state.client_ip.as_deref().filter(|ip| !settings.whitelist.iter().any(|w| w == ip));
    run.result(
        if client_missing.is_some() { ResultKind::Warning } else { ResultKind::Success },
        if changes.whitelist.is_some() { t("The whitelist is saved") } else { t("Unbanned") },
        &whitelist_message(changes.whitelist.as_ref().map(|_| settings.whitelist.as_slice()), &changes.unban, client_missing, false),
    );
    Ok(())
}

async fn crowdsec_tool(job: &Job) -> Result<()> {
    let server = job.server();
    let state = crowdsec::inspect(server).await?;
    let site = match &state.site {
        Some(site) if state.installed && state.running => site.clone(),
        _ => {
            job.show(
                ResultKind::Warning,
                if state.installed { t("CrowdSec does not run") } else { t("CrowdSec is not installed") },
                t("Install it first: Addons → CrowdSec → Install. It protects every route through Traefik and sets up the whitelist."),
            );
            return Ok(());
        }
    };
    let status = tf(
        "CrowdSec {version} protects every route through Traefik: {count} address(es) banned by this server (the community blocklist comes on top, and is not listed here).",
        &[("version", state.version.as_deref().unwrap_or_default()), ("count", &state.banned.len().to_string())],
    );
    let question = WhitelistQuestion {
        title: "CrowdSec",
        status,
        current: state.whitelist.clone(),
        configured: true,
        banned: state.banned.clone(),
        client_ip: state.client_ip.clone(),
    };
    let Some(changes) = modals::ask_whitelist(&job.state, &job.shared.ctx, question).await else { return Ok(()) };
    let mut checks = Vec::new();
    if changes.whitelist.is_some() {
        checks.push(n("Save the whitelist"));
    }
    if !changes.unban.is_empty() {
        checks.push(n("Unban"));
    }
    let run = job.start_run(&checks);
    let log = run.logger();
    let whitelist = changes.whitelist.clone().unwrap_or_else(|| state.whitelist.clone());
    if changes.whitelist.is_some() {
        crowdsec::save_whitelist(server, &site, state.port, &whitelist, &log).await?;
        run.done(&tf("{count} entries", &[("count", &whitelist.len().to_string())]));
    }
    for value in &changes.unban {
        crowdsec::unban(server, value, &log).await?;
    }
    if !changes.unban.is_empty() {
        run.done(&changes.unban.join(", "));
    }
    let client_missing = state.client_ip.as_deref().filter(|ip| !whitelist.iter().any(|w| w == ip));
    run.result(
        if client_missing.is_some() { ResultKind::Warning } else { ResultKind::Success },
        if changes.whitelist.is_some() { t("The whitelist is saved") } else { t("Unbanned") },
        &whitelist_message(changes.whitelist.as_ref().map(|_| whitelist.as_slice()), &changes.unban, client_missing, true),
    );
    Ok(())
}

async fn backup_database(job: &Job) -> Result<()> {
    let suggested = format!("habbo-{}-{}.sql.gz", job.host(), config_dir::timestamp());
    let Some(target) = rfd::AsyncFileDialog::new()
        .set_title(t("Save the database backup as"))
        .set_file_name(&suggested)
        .add_filter(t("Compressed SQL dump"), &["gz"])
        .save_file()
        .await
    else {
        return Ok(());
    };
    let local = target.path().to_path_buf();
    let server = job.server();

    let run = job.start_run(&[n("Dump the database on the server"), n("Download to this computer")]);
    let log = run.logger();
    let backup = server_tools::backup_database(server, &log).await?;
    run.done(&format!("{}, {}", backup.database, size(backup.bytes)));

    log(&format!("> Downloading {} to {}", backup.remote_path, local.display()));
    server.download(&backup.remote_path, &local, &progress(&run, backup.bytes)).await?;
    // The dump holds every password hash of the hotel.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o600));
    }
    log(&format!("  {} saved", size(file_size(&local))));
    run.done(&file_name(&local));
    run.result(
        ResultKind::Success,
        t("The database backup is on this computer"),
        &tf(
            "{file} ({size}). A copy stays on the server in {directory} (the newest {kept} are kept). Restore it with Import full database.",
            &[
                ("file", &local.display().to_string()),
                ("size", &size(backup.bytes)),
                ("directory", &server_tools::backup_directory()),
                ("kept", &server_tools::KEPT_BACKUPS.to_string()),
            ],
        ),
    );
    Ok(())
}

async fn import_database(job: &Job) -> Result<()> {
    let Some(local) = pick_dump(t("Select the database backup to import")).await else { return Ok(()) };
    let bytes = file_size(&local);
    let name = file_name(&local);
    let directory = server_tools::backup_directory();
    if !job
        .confirm(
            t("Replace the hotel database?"),
            &tf(
                "The hotel database on {host} is deleted and replaced with {file} ({size}). A safety backup of the current database is made first, in {directory}. The emulator is stopped during the import.",
                &[("host", &job.host()), ("file", &name), ("size", &size(bytes)), ("directory", &directory)],
            ),
            t("Replace the database"),
        )
        .await
    {
        return Ok(());
    }
    let server = job.server();
    let run = job.start_run(&[n("Upload to the server"), n("Safety backup, import, start the emulator")]);
    let log = run.logger();
    let remote = format!("{directory}/import-{}{}", config_dir::timestamp(), dump_extension(&local));
    server.run_as_root(&format!("install -d -m 700 {directory}")).await?;
    log(&format!("> Uploading {name} ({}) to {remote}", size(bytes)));
    server.upload(&local, &remote, &progress(&run, bytes)).await?;
    run.done(&size(bytes));

    server_tools::import_database(server, &remote, &log).await?;
    run.done(t("imported"));
    run.result(
        ResultKind::Success,
        t("The database is imported"),
        &tf(
            "{file} is now the hotel database; the emulator has started with it. The database as it was is in {directory}/*-before-import-*.sql.gz.",
            &[("file", &name), ("directory", &directory)],
        ),
    );
    Ok(())
}

async fn import_plus(job: &Job) -> Result<()> {
    let Some(local) = pick_dump(t("Select the PlusEMU database dump (for example its fresh install)")).await else { return Ok(()) };
    let bytes = file_size(&local);
    let name = file_name(&local);
    let mode = job
        .shared
        .dialogs
        .choose(
            t("Import Plus database"),
            t("What should come from the Plus database?"),
            &tf(
                "{file} ({size}) is converted to Polaris: furniture Polaris already has keeps its id and settings, the rest is added with the matching Polaris interaction. A safety backup is made first, and the emulator is stopped during the import.",
                &[("file", &name), ("size", &size(bytes))],
            ),
            None,
            vec![
                Choice::new(
                    t("Furniture and catalog"),
                    t("Add the new furniture and replace the normal catalog with the Plus catalog. Builders Club and the Habbicons shop stay as they are."),
                    Some(PlusImportMode::FurnitureAndCatalog),
                )
                .primary(),
                Choice::new(
                    t("Furniture only"),
                    t("Add the new furniture and keep your catalog: put the new furniture in the catalog yourself."),
                    Some(PlusImportMode::FurnitureOnly),
                ),
                Choice::new(t("Cancel"), t("Change nothing."), None),
            ],
        )
        .await;
    let Some(mode) = mode else { return Ok(()) };

    let server = job.server();
    let directory = server_tools::backup_directory();
    let run = job.start_run(&[n("Upload to the server"), n("Safety backup, convert to Polaris, start the emulator")]);
    let log = run.logger();
    let remote = format!("{directory}/plus-import-{}{}", config_dir::timestamp(), dump_extension(&local));
    server.run_as_root(&format!("install -d -m 700 {directory}")).await?;
    log(&format!("> Uploading {name} ({}) to {remote}", size(bytes)));
    server.upload(&local, &remote, &progress(&run, bytes)).await?;
    run.done(&size(bytes));

    let result = plus_import::import(server, &remote, mode, &log).await?;
    run.done(&tf("{count} new furniture", &[("count", &result.furniture_new.to_string())]));
    let catalog = if mode == PlusImportMode::FurnitureAndCatalog {
        tf(" The catalog has {pages} pages with {items} items.", &[("pages", &result.pages.to_string()), ("items", &result.catalog_items.to_string())])
    } else {
        String::new()
    };
    let eggs = if result.crackables_without_prizes > 0 {
        tf(
            " {count} new crackables have no prizes yet (items_crackable): until you add them, clicking them does nothing.",
            &[("count", &result.crackables_without_prizes.to_string())],
        )
    } else {
        String::new()
    };
    let added = tf(
        "{new} furniture added, {existing} were already there.",
        &[("new", &result.furniture_new.to_string()), ("existing", &result.furniture_existing.to_string())],
    );
    let client = tf(
        "The client needs the new furniture in its furnidata and assets (Deploy Assets). The database as it was is in {backup}.",
        &[("backup", &result.safety_backup)],
    );
    run.result(ResultKind::Success, t("The Plus database is imported"), &format!("{added}{catalog}{eggs} {client}"));
    Ok(())
}

/// Tools → Furniture language: another hotel's furnidata, and the database brought in line with it.
async fn furniture_language_tool(job: &Job) -> Result<()> {
    use aio_remote::external_texts;
    use aio_remote::furniture_language::{self, LANGUAGES};
    let server = job.server();
    let current = furniture_language::installed(server).await?;
    let current_name = current.as_deref().and_then(furniture_language::language).map(crate::views::default_catalog::native_name);
    let options: Vec<Choice<Option<&'static str>>> = LANGUAGES
        .iter()
        .map(|l| {
            let name = crate::views::default_catalog::native_name(*l);
            let choice = Choice::new(name, &tf("The furnidata of {domain}", &[("domain", l.domain)]), Some(l.code));
            if current.as_deref() == Some(l.code) {
                choice.primary()
            } else {
                choice
            }
        })
        .chain([Choice::new(t("Cancel"), t("Change nothing."), None)])
        .collect();
    let message = format!(
        "{} {}",
        match current_name {
            Some(name) => tf("The furniture is in {language} now.", &[("language", name)]),
            None => t("The installer has not set the furniture language on this server yet.").to_string(),
        },
        t("The chosen hotel's furnidata is downloaded on this computer and put in gamedata/config; the database follows it (sprite ids, catalogue items, offer ids). The hotel's own furni keep their names. A backup comes first and the emulator restarts.")
    );
    let Some(code) = job.shared.dialogs.choose(t("Furniture language"), t("Which language should the furniture be in?"), &message, None, options).await else {
        return Ok(());
    };
    let language = furniture_language::language(code).unwrap_or(LANGUAGES[0]);
    let name = crate::views::default_catalog::native_name(language);
    let Some(with_texts) = job
        .shared
        .dialogs
        .choose(
            t("Furniture language"),
            &tf("Also ExternalTexts.json (the client's texts) in {language}?", &[("language", name)]),
            &tf(
                "Downloaded from {domain}; texts the hotel has but {domain} does not (custom badges and the like) stay. The old file is kept as .bak-<time>.",
                &[("domain", language.domain)],
            ),
            None,
            vec![
                Choice::new(t("Furniture and texts"), t("FurnitureData.json and ExternalTexts.json."), Some(true)).primary(),
                Choice::new(t("Only the furniture"), t("ExternalTexts.json stays as it is."), Some(false)),
                Choice::new(t("Cancel"), t("Change nothing."), None),
            ],
        )
        .await
    else {
        return Ok(());
    };

    let mut stages = vec![n("Download"), n("Stop the emulator"), n("Furniture language")];
    if with_texts {
        stages.push(n("ExternalTexts.json"));
    }
    stages.push(n("Start the emulator"));
    let run = job.start_run(&stages);
    let log = run.logger();
    // Downloaded first, while the hotel still runs: a failed download changes nothing.
    let hotel = furniture_language::current_furniture_data(server).await?;
    let prepared = furniture_language::prepare(language, hotel.as_ref(), &log).await?;
    let texts = if with_texts { Some(external_texts::prepare(server, language, &log).await?) } else { None };
    run.done(language.domain);

    // One stop of the emulator, and one start at the end.
    let stopped = server_tools::stop_emulator(server, &log).await?;
    run.done(if stopped { t("stopped") } else { t("not installed") });
    let work = async {
        let switched = furniture_language::finish(server, prepared, false, &log).await?;
        run.done(name);
        let old_texts = match &texts {
            Some(texts) => {
                let old = external_texts::put(server, texts, &log).await?;
                run.done(&tf("{count} texts", &[("count", &texts.count().to_string())]));
                Some(old)
            }
            None => None,
        };
        anyhow::Ok((switched, old_texts))
    }
    .await;
    let (switched, old_texts) = match work {
        Ok(done) => done,
        Err(error) => {
            if stopped {
                if let Err(start) = server_tools::start_emulator(server, &log).await {
                    log(&format!("! {start}"));
                }
            }
            return Err(error);
        }
    };
    if stopped {
        if let Err(error) = server_tools::start_emulator(server, &log).await {
            log("> The emulator does not start with the new furniture data");
            furniture_language::restore(
                server,
                &switched.applied.safety_backup,
                switched.applied.old_furniture_data.as_deref(),
                current.as_deref(),
                &log,
            )
            .await?;
            if let Some(old) = &old_texts {
                external_texts::restore(server, old.as_deref(), &log).await?;
            }
            server_tools::start_emulator(server, &log).await?;
            return Err(anyhow::anyhow!(
                "{}",
                tf(
                    "The emulator does not start with the new furniture data ({error}). The database, FurnitureData.json and ExternalTexts.json are as they were before.",
                    &[("error", &error.to_string())],
                )
            ));
        }
    }
    run.done(if stopped { t("loaded") } else { t("not installed") });
    let texts_line = match &texts {
        Some(texts) => format!(
            " {}",
            tf("ExternalTexts.json is in that language too: {count} texts ({kept} of the hotel's own kept).", &[("count", &texts.count().to_string()), ("kept", &texts.kept.to_string())])
        ),
        None => String::new(),
    };
    run.result(
        ResultKind::Success,
        &tf("The furniture is in {language} now", &[("language", name)]),
        &format!(
            "{}{texts_line}",
            tf(
                "From {domain}: {furni} furni ({custom} of the hotel's own kept). Changed in the database: {sprites} sprite ids, {items} catalogue items, {offers} offer ids. Players see it after they reload the hotel. The database as it was is in {backup}.",
                &[
                    ("domain", language.domain),
                    ("furni", &switched.furni.to_string()),
                    ("custom", &switched.custom_kept.to_string()),
                    ("sprites", &switched.plan.sprite_ids.len().to_string()),
                    ("items", &switched.plan.item_ids.len().to_string()),
                    ("offers", &switched.plan.offer_ids.len().to_string()),
                    ("backup", &switched.applied.safety_backup),
                ],
            )
        ),
    );
    Ok(())
}

async fn recover_sql_password(job: &Job) -> Result<()> {
    let host = job.host();
    let path = config_dir::database_credentials_path(&host);
    let saved = config_dir::load_database_credentials(&host);
    let message = format!(
        "{}{}{}",
        tf("root on {host} gets a new random password, saved in {path}", &[("host", &host), ("path", &path.display().to_string())]),
        if saved.is_none() { "." } else { t(" (the old file is kept next to it as .bak-<time>).") },
        t(" The emulator and AtomCMS log in with their own accounts and keep working. HeidiSQL and other tools need the new password. When root cannot log in through the socket any more, MariaDB restarts for a moment without privilege checks to change it.")
    );
    if !job.confirm(t("Reset the MariaDB root password?"), &message, t("Reset the password")).await {
        return Ok(());
    }

    let server = job.server();
    let run = job.start_run(&[n("Reset the root password on the server"), n("Test the new password"), n("Save it in the configuration folder")]);
    let log = run.logger();
    let credentials = match saved {
        Some(saved) => DatabaseCredentials { root_password: aio_core::credentials::new_password(), ..saved },
        None => {
            // No file yet: take the other accounts from the server so the new file is complete.
            log("> No saved MariaDB file for this server: reading the CMS and emulator accounts from the server");
            let accounts = mariadb::read_accounts(server).await?;
            let generated = DatabaseCredentials::generate(&host);
            let credentials = DatabaseCredentials {
                database: accounts.database.clone().unwrap_or(generated.database.clone()),
                cms_user: accounts.cms_user.clone().unwrap_or(generated.cms_user.clone()),
                cms_password: accounts.cms_password.clone().unwrap_or(generated.cms_password.clone()),
                emulator_user: accounts.emulator_user.clone().unwrap_or(generated.emulator_user.clone()),
                emulator_password: accounts.emulator_password.clone().unwrap_or_default(),
                ..generated
            };
            match &accounts.cms_password {
                None => log("  CMS account: not found, a new password is saved (the MariaDB step sets it)"),
                Some(_) => log(&format!("  CMS account: {} from AtomCMS' .env", credentials.cms_user)),
            }
            match &accounts.emulator_password {
                None => log("  Emulator account: not found"),
                Some(_) => log(&format!("  Emulator account: {} from config.ini", credentials.emulator_user)),
            }
            credentials
        }
    };

    mariadb::reset_root_password(server, &credentials, &log).await?;
    run.done(t("changed"));
    log("> Logging in with the new password");
    // 127.0.0.1 (SSH tunnels) and the socket must work. root@'%' only exists on servers this installer set up.
    if let Some(problem) = mariadb::test_login(server, &credentials.root_user, &credentials.root_password, None, true, &log, false).await? {
        return Err(ServerError(problem).into());
    }
    if let Some(problem) = mariadb::test_login(server, &credentials.root_user, &credentials.root_password, None, false, &log, true).await? {
        log(&format!("  ({problem}: not needed for SSH tunnels)"));
    }
    run.done(t("works"));

    let file = config_dir::save_database_credentials(&credentials)?;
    log(&format!("  Saved in {}", file.display()));
    run.done(&file_name(&file));
    run.result(
        ResultKind::Success,
        t("The MariaDB root password is reset"),
        &tf(
            "The new password of {user} is in {file}. The SQL Editor uses it from now on; for HeidiSQL, connect through an SSH tunnel to 127.0.0.1:3306 with that password.",
            &[("user", &credentials.root_user), ("file", &file.display().to_string())],
        ),
    );
    Ok(())
}

/// "Check for updates": fetches every clone and reports what a git pull would bring in. Nothing on the server is
/// changed, so this is safe while the hotel is open.
async fn check_updates(job: &Job) -> Result<()> {
    let server = job.server();
    let run = job.start_run(&[n("git fetch every clone")]);
    let log = run.logger();
    let updates = server_status::updates(server).await?;
    let images = load_images(server).await?;
    for image in &images {
        let state = match &image.problem {
            Some(problem) => problem.clone(),
            None if image.newer_on_hub => format!(
                "a newer image on Docker Hub{}",
                image.newest_version.as_ref().map(|v| format!(" ({v})")).unwrap_or_default()
            ),
            None if image.not_restarted => "a newer image is downloaded, but the container still runs the old one (Update starts it)".into(),
            None => "up to date".into(),
        };
        log(&format!("  {} {} ({}): {state}", image.name, image.version.as_deref().unwrap_or("?"), image.image));
    }
    for update in &updates {
        log(&format!(
            "  {}: on {}, counted against {} — {}{}",
            update.name,
            update.branch.as_deref().unwrap_or("?"),
            update.counted().unwrap_or("?"),
            if update.switches_branch() { format!("updating switches it to {}; ", update.target.as_deref().unwrap_or_default()) } else { String::new() },
            if update.has_update() {
                format!("{} new commit(s), newest {}", update.behind, update.newest.as_deref().unwrap_or_default())
            } else {
                "up to date".into()
            }
        ));
    }
    if updates.is_empty() {
        log("  No hotel clones found on this server.");
    }
    show_updates(&job.shared, &job.state, &job.server, updates.clone(), images);

    let behind: Vec<_> = updates.iter().filter(|u| u.has_update()).collect();
    let switching: Vec<_> = updates.iter().filter(|u| u.switches_branch()).collect();
    let anything = !behind.is_empty() || !switching.is_empty();
    let count = updates.iter().filter(|u| u.has_update() || u.switches_branch()).count();
    run.done(&if anything { tf("{count} to update", &[("count", &count.to_string())]) } else { t("up to date").to_string() });
    let message = if anything {
        let mut message = String::new();
        if !behind.is_empty() {
            let list = behind.iter().map(|u| format!("{} ({})", u.name, u.behind)).collect::<Vec<_>>().join(", ");
            message += &tf("New commits for {list}. ", &[("list", &list)]);
        }
        if !switching.is_empty() {
            message += &tf("Waiting to move to the chosen branch: {list}. ", &[("list", &super::switch_list(&switching))]);
        }
        message + t("Nothing was changed yet: update with the tools here.")
    } else {
        t("Every clone is on its chosen branch and has its newest commits.").to_string()
    };
    run.result(
        if anything { ResultKind::Info } else { ResultKind::Success },
        if anything { t("There are updates") } else { t("Everything is up to date") },
        &message,
    );
    Ok(())
}

async fn fix_visitor_ips(job: &Job) -> Result<()> {
    let server = job.server();
    let state = harden::inspect(server).await?;
    let Some(site) = state.site else { fail!("The hotel's address is not known: run the Web access step first.") };
    if !site.cloudflare {
        job.show(
            ResultKind::Warning,
            t("The hotel is set up without Cloudflare"),
            &tf(
                "Without Cloudflare, Traefik writes every visitor's address itself and Polaris and NGINX already read it. If {address} is behind Cloudflare (the orange cloud in Cloudflare's DNS), run Install New Server → Web access again with Cloudflare on: then Traefik, NGINX and Polaris trust Cloudflare's ranges and see the players' real IP.",
                &[("address", &site.address)],
            ),
        );
        return Ok(());
    }
    if !job
        .confirm(
            t("Fix the visitor IPs?"),
            &tf(
                "Polaris, NGINX and Traefik get Cloudflare's current IP ranges, so they read the player's address past Cloudflare. The emulator restarts when its setting changes (everyone is disconnected for a moment), and Traefik when its list changes. A weekly refresh ({script}, Sunday 04:00) keeps the ranges current.",
                &[("script", web_access::CLOUDFLARE_REFRESH_SCRIPT)],
            ),
            t("Fix the visitor IPs"),
        )
        .await
    {
        return Ok(());
    }
    let run = job.start_run(&[n("Polaris: trust Cloudflare's ranges"), n("NGINX: real IP past Cloudflare"), n("Weekly refresh, Traefik")]);
    let log = run.logger();
    let polaris = web_access::configure_polaris(server, &site, &log).await?;
    run.done(if polaris { "ws.ip.header.trusted" } else { t("emulator not set up yet") });
    web_access::configure_nginx_real_ip(server, &site, &log).await?;
    run.done(hotel_site::REAL_IP_FILE);
    web_access::install_cloudflare_refresh(server, &site, &log).await?;
    run.done(web_access::CLOUDFLARE_REFRESH_CRON);
    run.result(
        ResultKind::Success,
        t("The players' real IP is passed on"),
        &tf(
            "Polaris logs \"logged in from IP <the player>\" from the next login; NGINX's access log shows the visitor too (Debug → NGINX access). The ranges are refreshed every Sunday at 04:00 ({script}, output in /var/log/cf-refresh.log).",
            &[("script", web_access::CLOUDFLARE_REFRESH_SCRIPT)],
        ),
    );
    Ok(())
}

async fn update_system(job: &Job) -> Result<()> {
    let run = job.start_run(&[n("apt-get update, upgrade and dist-upgrade")]);
    let log = run.logger();
    let reboot = server_tools::update_system(job.server(), &log).await?;
    run.done(if reboot { t("reboot needed") } else { t("up to date") });
    if reboot {
        run.result(
            ResultKind::Warning,
            t("The system is updated; a reboot is needed"),
            t("Reboot the server when it suits you (sudo reboot). The emulator, MariaDB, NGINX and Traefik start again by themselves."),
        );
    } else {
        run.result(ResultKind::Success, t("The system is up to date"), t("All packages are updated (changed configuration files were kept)."));
    }
    Ok(())
}

async fn update_emulator(job: &Job) -> Result<()> {
    let repository = emulator::repository();
    // The branch from the Settings page, so this tool follows the same choice as the clone step.
    let branch = server_status::chosen_branch(&repository);
    let message = format!(
        "{}{}{}",
        tf("git pull in {directory}", &[("directory", &repository)]),
        branch
            .as_ref()
            .map(|b| tf(" on branch {branch} (Settings → Repository branches; a clone on another branch is switched over first)", &[("branch", b)]))
            .unwrap_or_default(),
        t(". When there is something new, the emulator is built again and restarted with the new version: everyone in the hotel is disconnected for a moment.")
    );
    if !job.confirm(t("Update the emulator?"), &message, t("Update the emulator")).await {
        return Ok(());
    }
    let server = job.server();
    let run = job.start_run(&[n("git pull"), n("build.sh"), n("Start the new version")]);
    let log = run.logger();
    let (old_commit, old_version) = server_tools::emulator_version(server).await?;
    let commit = git_clone::update(server, &repository, branch.as_deref(), &log).await?;
    // New images or badge parts in the emulator's Assets/ go to gamedata (missing files only).
    emulator_assets::install(server, &log).await?;
    let (new_commit, _) = server_tools::emulator_version(server).await?;
    run.done(&commit);

    let old_commit_text = old_commit.clone().unwrap_or_default();
    if new_commit == old_commit {
        if let Some(old_version) = &old_version {
            log(&format!("  No new commits: {old_version} stays."));
            run.done(t("not needed"));
            run.done(t("not needed"));
            run.result(
                ResultKind::Success,
                t("The emulator is up to date"),
                &tf(
                    "{version} ({commit}) is the newest version; nothing was rebuilt or restarted.",
                    &[("version", old_version), ("commit", &old_commit_text)],
                ),
            );
            job.check_quietly().await;
            return Ok(());
        }
    }

    server_tools::fix_emulator_owner(server, &log).await?;
    let java_home = server_tools::java_home(server).await?;
    let jar = emulator::build(server, &java_home, &log).await?;
    let (_, new_version) = server_tools::emulator_version(server).await?;
    run.done(&new_version.clone().unwrap_or_else(|| file_name(Path::new(&jar))));

    // The start script runs the newest target/*-jar-with-dependencies.jar, so a new version needs no changes.
    server_tools::restart_emulator(server, &log).await?;
    run.done(t("loaded"));
    let new_commit_text = new_commit.unwrap_or_default();
    let new_version_text = new_version.clone().unwrap_or_default();
    let message = if old_version == new_version {
        tf(
            "{version} was rebuilt with the new commits ({old} → {new}) and is running.",
            &[("version", &new_version_text), ("old", &old_commit_text), ("new", &new_commit_text)],
        )
    } else {
        tf(
            "{from} → {to} ({old} → {new}) is running.",
            &[("from", old_version.as_deref().unwrap_or("?")), ("to", &new_version_text), ("old", &old_commit_text), ("new", &new_commit_text)],
        )
    };
    run.result(ResultKind::Success, t("The emulator is updated"), &message);
    job.check_quietly().await;
    Ok(())
}

async fn update_octane(job: &Job) -> Result<()> {
    let server = job.server();
    let run = job.start_run(&[
        n("git pull Octane-Renderer and Octane"),
        n("Octane-Renderer: fresh yarn install"),
        n("Octane: fresh yarn install"),
        n("Configuration for the site"),
        n("yarn build"),
        n("Serve and test"),
    ]);
    let log = run.logger();
    let state = octane::inspect(server).await?;
    let Some(site) = state.site.clone() else { fail!("The hotel's address is not known: run the Web access step first.") };
    if !state.octane_present || !state.renderer_present {
        fail!("Octane or Octane-Renderer is not cloned on this server.");
    }
    let (renderer_dir, octane_dir) = (octane::renderer(), octane::octane());
    // Both follow the branch chosen on the Settings page, like the clone steps do.
    let renderer = git_clone::update(server, &renderer_dir, server_status::chosen_branch(&renderer_dir).as_deref(), &log).await?;
    let octane_commit = git_clone::update(server, &octane_dir, server_status::chosen_branch(&octane_dir).as_deref(), &log).await?;
    run.done(&octane_commit);

    octane::yarn_install(server, &renderer_dir, &log, true).await?;
    run.done(t("done"));
    octane::yarn_install(server, &octane_dir, &log, true).await?;
    run.done(t("done"));

    // Also puts back the /client/ --base in package.json that AtomCMS needs.
    octane::configure(server, &site, &log, false).await?;
    run.done(&site.client_url());
    octane::build(server, &site, &log).await?;
    run.done("dist/");
    octane::publish(server, &site, &log).await?;
    run.done(&site.client_url());
    run.result(
        ResultKind::Success,
        &tf("Octane is updated on {url}", &[("url", &site.client_url())]),
        &tf(
            "Octane-Renderer {renderer}, Octane {octane}. Players get the new client when they reload the page.",
            &[("renderer", &renderer), ("octane", &octane_commit)],
        ),
    );
    job.check_quietly().await;
    Ok(())
}

async fn update_traefik(job: &Job) -> Result<()> {
    if !job
        .confirm(
            t("Update Traefik?"),
            t("Downloads the newest traefik:v3 image and restarts Traefik when it changed. Its routes, settings and certificates stay; everyone in the hotel is disconnected for a few seconds."),
            t("Update Traefik"),
        )
        .await
    {
        return Ok(());
    }
    let run = job.start_run(&[n("docker compose pull and up -d")]);
    let log = run.logger();
    server_tools::update_traefik(job.server(), &log).await?;
    run.done(t("runs"));
    run.result(ResultKind::Success, t("Traefik is updated"), t("It runs with the newest traefik:v3 image; its routes and certificates are the same."));
    job.check_quietly().await;
    Ok(())
}

async fn update_gitea(job: &Job) -> Result<()> {
    let server = job.server();
    let state = gitea::inspect(server).await?;
    let Some(domain) = state.domain else { fail!("Gitea is not installed on this server: install it with Addons → Gitea.") };
    if !job
        .confirm(
            t("Update Gitea?"),
            &tf(
                "Downloads the newest {image} image and restarts Gitea when it changed. The repositories, accounts and settings in {directory}/data stay; {domain} is away for a few seconds.",
                &[("image", gitea::IMAGE), ("directory", gitea::DIRECTORY), ("domain", &domain)],
            ),
            t("Update Gitea"),
        )
        .await
    {
        return Ok(());
    }
    let run = job.start_run(&[n("docker compose pull and up -d")]);
    let log = run.logger();
    gitea::update(server, &log).await?;
    run.done(t("runs"));
    run.result(
        ResultKind::Success,
        t("Gitea is updated"),
        &tf("{domain} runs the newest {image} image; the repositories and accounts are the same.", &[("domain", &domain), ("image", gitea::IMAGE)]),
    );
    job.check_quietly().await;
    Ok(())
}

async fn update_crowdsec(job: &Job) -> Result<()> {
    let server = job.server();
    let state = crowdsec::inspect(server).await?;
    if !state.installed {
        fail!("CrowdSec is not installed on this server: install it with Addons → CrowdSec.");
    }
    if !job
        .confirm(
            t("Update CrowdSec?"),
            &tf(
                "Downloads the newest {image} image (CrowdSec restarts when it changed) and the newest detection rules. The bans, the whitelist and the enrollment stay; the hotel stays reachable while CrowdSec restarts (Traefik is not restarted).",
                &[("image", crowdsec::IMAGE)],
            ),
            t("Update CrowdSec"),
        )
        .await
    {
        return Ok(());
    }
    let run = job.start_run(&[n("docker compose pull and up -d, then the detection rules")]);
    let log = run.logger();
    crowdsec::update(server, &log).await?;
    run.done(t("runs"));
    run.result(
        ResultKind::Success,
        t("CrowdSec is updated"),
        t("It runs the newest image with the newest detection rules; the bans, whitelist and enrollment are the same."),
    );
    job.check_quietly().await;
    Ok(())
}

async fn update_atom_cms(job: &Job) -> Result<()> {
    if ServerLayout::current().atom_cms_installed == AtomCmsPresence::NotInstalled {
        fail!("Settings → Server layout says AtomCMS is not installed on this server. Change that there to update it.");
    }
    let server = job.server();
    let directory = atom_cms::directory();
    let state = atom_cms::inspect(server).await?;
    if !state.vendor_present || !state.migrated {
        fail!("AtomCMS is not installed on this server ({directory}): install it with Addons → AtomCMS.");
    }
    if !job
        .confirm(
            t("Update AtomCMS?"),
            &tf(
                "git pull in {directory}, then composer install, the new database migrations and the theme build. The site keeps running; visitors may see an error for a moment while the files change. Octane is not changed.",
                &[("directory", &directory)],
            ),
            t("Update AtomCMS"),
        )
        .await
    {
        return Ok(());
    }
    let run = job.start_run(&[n("git pull"), n("composer install"), n("Database migrations"), n("Theme build")]);
    let log = run.logger();
    let commit = git_clone::update(server, &directory, None, &log).await?;
    run.done(&commit);
    atom_cms::composer_install(server, &log).await?;
    run.done("vendor/");
    // An installed AtomCMS only runs the new migrations; its settings and the hotel name stay as they are.
    atom_cms::migrate(server, state.hotel_name.as_deref().unwrap_or_default(), &log).await?;
    run.done(t("up to date"));
    atom_cms::build_theme(server, &log).await?;
    run.done("public/build");
    let message = match &state.site {
        Some(site) => tf("{commit} is live on {url}.", &[("commit", &commit), ("url", &site.url())]),
        None => tf("{commit} is live.", &[("commit", &commit)]),
    };
    run.result(ResultKind::Success, t("AtomCMS is updated"), &message);
    job.check_quietly().await;
    Ok(())
}

fn settings_banner(state: &SharedState, shared: &Shared, heading: &str, message: &str) {
    state.lock().unwrap().settings_result = Some(Banner { kind: ResultKind::Success, heading: heading.into(), message: message.into() });
    shared.ctx.request_repaint();
}

fn settings_filter() -> &'static str {
    backup::EXTENSION.trim_start_matches('.')
}

pub async fn export_settings(shared: &Shared, state: &SharedState) -> Result<()> {
    let suggested = format!("all-in-1-settings-{}{}", config_dir::timestamp(), backup::EXTENSION);
    let Some(target) = rfd::AsyncFileDialog::new()
        .set_title(t("Save the encrypted settings as"))
        .set_file_name(&suggested)
        .add_filter(t("All-in-1 settings"), &[settings_filter()])
        .save_file()
        .await
    else {
        return Ok(());
    };
    let file = target.path().to_path_buf();
    let Some(password) = modals::ask_password(
        state,
        &shared.ctx,
        t("Encrypt the settings"),
        t("Choose a password"),
        t("The file holds your SSH keys and database passwords: it can only be opened with this password. Keep it somewhere safe; it cannot be recovered."),
        true,
        backup::MINIMUM_PASSWORD_LENGTH,
    )
    .await
    else {
        return Ok(());
    };
    let path = file.clone();
    let count = tokio::task::spawn_blocking(move || backup::export(&path, &password)).await??;
    settings_banner(
        state,
        shared,
        t("The settings are exported"),
        &tf(
            "{count} files, encrypted, in {file}. Import them on the other computer with Import encrypted settings and the same password.",
            &[("count", &count.to_string()), ("file", &file.display().to_string())],
        ),
    );
    Ok(())
}

pub async fn import_settings(shared: &Shared, state: &SharedState) -> Result<()> {
    let Some(source) = rfd::AsyncFileDialog::new()
        .set_title(t("Select the encrypted settings"))
        .add_filter(t("All-in-1 settings"), &[settings_filter()])
        .pick_file()
        .await
    else {
        return Ok(());
    };
    let file = source.path().to_path_buf();
    let Some(password) = modals::ask_password(
        state,
        &shared.ctx,
        t("Decrypt the settings"),
        t("Password of the settings file"),
        &tf("The password chosen when {file} was exported.", &[("file", &file_name(&file))]),
        false,
        1,
    )
    .await
    else {
        return Ok(());
    };
    let directory = config_dir::directory().display().to_string();
    if !confirm(
        &shared.dialogs,
        t("Replace the settings on this computer?"),
        &tf(
            "The SSH keys and database passwords in {directory} are replaced with those in the file. The current ones are kept next to it as configuration.bak-<time>.",
            &[("directory", &directory)],
        ),
        t("Import the settings"),
    )
    .await
    {
        return Ok(());
    }
    let (previous, count) = tokio::task::spawn_blocking(move || backup::import(&file, &password)).await??;
    let mut message = tf("{count} files in {directory}.", &[("count", &count.to_string()), ("directory", &directory)]);
    if let Some(previous) = previous {
        message += &tf(" The previous settings are in {previous}.", &[("previous", &previous.display().to_string())]);
    }
    settings_banner(state, shared, t("The settings are imported"), &message);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_extensions() {
        assert_eq!(dump_extension(Path::new("C:/x/backup.SQL.GZ")), ".sql.gz");
        assert_eq!(dump_extension(Path::new("dump.gz")), ".sql.gz");
        assert_eq!(dump_extension(Path::new("dump.sql")), ".sql");
    }

    #[test]
    fn whitelist_results() {
        let list = vec!["1.2.3.4".to_string(), "10.0.0.0/8".to_string()];
        assert_eq!(whitelist_message(Some(&list), &[], None, false), "Never banned: 1.2.3.4, 10.0.0.0/8.");
        assert_eq!(whitelist_message(Some(&[]), &[], None, false), "Only the server itself is never banned.");
        assert_eq!(whitelist_message(Some(&[]), &[], None, true), "Nothing is whitelisted.");
        assert_eq!(
            whitelist_message(None, &["5.6.7.8".to_string()], Some("9.9.9.9"), true),
            "Let in again: 5.6.7.8 (the bouncer knows within a minute). This computer (9.9.9.9) is not on the whitelist."
        );
    }
}

//! The SDK for All-in-1 Manager plugins.
//!
//! A plugin is a WebAssembly component (`wasm32-wasip2`) for the Manager's plugin API `aio:plugin@1.1.0`. It
//! never draws anything itself: the Manager asks it to describe its page ([`Plugin::render`]), shows that page,
//! and hands back what the user did ([`Plugin::on_event`]), after which it asks for the page again. A long job
//! runs in steps between pages: [`host::keep_working`] asks for one more [`Plugin::tick`].
//!
//! A plugin is a type implementing [`Plugin`], exported with [`export_plugin!`]:
//!
//! ```
//! use aio_plugin_sdk::{export_plugin, Event, Plugin, Ui};
//!
//! #[derive(Default)]
//! struct Greeter {
//!     name: String,
//!     greeted: bool,
//! }
//!
//! impl Plugin for Greeter {
//!     fn new() -> Self {
//!         Self::default()
//!     }
//!
//!     fn render(&mut self, ui: &mut Ui) {
//!         ui.heading("Hello");
//!         ui.input("name", "Name", self.name.as_str());
//!         ui.primary_button("greet", "Greet");
//!         if self.greeted {
//!             ui.text(format!("Hello {}!", self.name));
//!         }
//!     }
//!
//!     fn on_event(&mut self, event: Event) -> Result<(), String> {
//!         match event {
//!             Event::Changed { id, value } if id == "name" => self.name = value,
//!             Event::Clicked { id } if id == "greet" => self.greeted = true,
//!             _ => {}
//!         }
//!         Ok(())
//!     }
//! }
//!
//! export_plugin!(Greeter);
//! ```
//!
//! The crate is a `cdylib` built with `cargo build --target wasm32-wasip2 --release`. Off that target (for
//! example in `cargo test` on the development machine) [`export_plugin!`] expands to nothing, so the plugin's own
//! logic and its pages ([`Ui`], [`ui::UiTree`]) can be tested natively.

mod error;
#[doc(hidden)]
pub mod export;
pub mod files;
pub mod folders;
pub mod host;
pub mod http;
pub mod i18n;
pub mod script;
pub mod sql;
pub mod tcp;
pub mod ui;

pub use error::{Error, ErrorKind, Result};
pub use ui::Ui;

/// The bindings generated from the plugin API (`source/wit/aio-plugin`). Private: plugins see only the SDK's
/// own types.
mod bindings {
    wit_bindgen::generate!({
        path: "../../wit/aio-plugin",
        world: "plugin",
        // The export macro (`__export_plugin_impl!`) is invoked by `export_plugin!` in the plugin crate.
        pub_export_macro: true,
    });
}

/// A plugin. The Manager keeps one instance per open plugin page, made with [`new`](Plugin::new) on first use,
/// and calls it from a single thread.
///
/// Implement it and export the type with [`export_plugin!`].
pub trait Plugin: 'static {
    /// The plugin's starting state, made before the first [`render`](Plugin::render) or
    /// [`on_event`](Plugin::on_event). Its memory lives while the page is open; anything that must survive longer
    /// goes into the plugin's private files.
    fn new() -> Self
    where
        Self: Sized;

    /// Describes the page with `ui`. Called when the page opens and after every [`on_event`](Plugin::on_event).
    ///
    /// If the page breaks a [rule of the builder](ui#rules), the Manager shows a single error banner with the
    /// reason instead.
    fn render(&mut self, ui: &mut Ui);

    /// Handles what the user did on the page. An `Err` is shown to the user and written to the plugin log; the
    /// page is rendered again either way. SDK [`Error`]s convert with `?`.
    fn on_event(&mut self, event: Event) -> std::result::Result<(), String>;

    /// One step of a long job: called after a render when the last [`on_event`](Plugin::on_event) or `tick` called
    /// [`host::keep_working`]. Call [`host::keep_working`] again for another step; without it the ticks stop. Events
    /// the user sent meanwhile (a Cancel button) come first. Each tick is a call of its own, with its own time limit,
    /// and the page is rendered again afterwards. Ticks run only while the page is open.
    ///
    /// An `Err` is shown like one from [`on_event`](Plugin::on_event). The default does nothing.
    fn tick(&mut self) -> std::result::Result<(), String> {
        Ok(())
    }
}

/// What the user did on the plugin page.
///
/// New events may be added in a later 1.x version: match with a `_` arm. New data for an event arrives as new
/// variants, never as new fields on `Clicked` or `Changed` (plugin tests construct those two).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// A [button](Ui::button) was pressed.
    Clicked {
        /// The button's id.
        id: String,
    },
    /// An [input](Ui::input) (when it loses the keyboard focus with a changed text), [checkbox](Ui::checkbox) or
    /// [select](Ui::select) changed.
    Changed {
        /// The control's id.
        id: String,
        /// The input's whole text, `"true"` or `"false"` for a checkbox, or the chosen option's index (`"0"`,
        /// `"1"`, ...) for a select.
        value: String,
    },
}

impl Event {
    /// The id of the control the event came from.
    pub fn id(&self) -> &str {
        match self {
            Event::Clicked { id } | Event::Changed { id, .. } => id,
        }
    }
}

/// Exports `$plugin`, a type implementing [`Plugin`], as the component's plugin: the Manager's `render`,
/// `on-event` and `tick` calls go to one instance of it, made with [`Plugin::new`] on first use.
///
/// Use it once, at the top level of the plugin crate. On targets other than `wasm32` it expands to nothing.
///
/// ```
/// # use aio_plugin_sdk::{Event, Plugin, Ui};
/// struct MyPlugin;
/// # impl Plugin for MyPlugin {
/// #     fn new() -> Self { MyPlugin }
/// #     fn render(&mut self, ui: &mut Ui) { ui.heading("Mine"); }
/// #     fn on_event(&mut self, _: Event) -> Result<(), String> { Ok(()) }
/// # }
/// aio_plugin_sdk::export_plugin!(MyPlugin);
/// ```
#[macro_export]
macro_rules! export_plugin {
    ($plugin:ty) => {
        #[cfg(target_arch = "wasm32")]
        const _: () = {
            ::std::thread_local! {
                static __AIO_PLUGIN: $crate::export::Slot<$plugin> = const { $crate::export::Slot::new() };
            }

            struct __AioPluginExport;

            impl $crate::export::abi::Guest for __AioPluginExport {
                fn render() -> $crate::export::WitTree {
                    __AIO_PLUGIN.with($crate::export::Slot::render)
                }

                fn on_event(event: $crate::export::WitEvent) -> ::std::result::Result<(), ::std::string::String> {
                    __AIO_PLUGIN.with(|slot| slot.on_event(event))
                }

                fn tick() -> ::std::result::Result<(), ::std::string::String> {
                    __AIO_PLUGIN.with($crate::export::Slot::tick)
                }
            }

            $crate::__export_plugin_impl!(__AioPluginExport with_types_in $crate::export::abi);
        };
    };
}

/// Renders `plugin` into a page; a page that breaks the builder's rules becomes a single error banner.
pub(crate) fn render_page<P: Plugin>(plugin: &mut P) -> ui::UiTree {
    let mut ui = Ui::new();
    plugin.render(&mut ui);
    ui.finish().unwrap_or_else(|error| {
        // Logged too, so that the reason is in the plugin log during development.
        host::log(host::Level::Warn, &format!("the page was refused: {}", error.message()));
        ui::UiTree::banner_page(ui::BannerKind::Error, error.message())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::{BannerKind, Element};

    struct Pages(usize);

    impl Plugin for Pages {
        fn new() -> Self {
            Pages(0)
        }

        fn render(&mut self, ui: &mut Ui) {
            for _ in 0..self.0 {
                ui.heading("h");
            }
        }

        fn on_event(&mut self, _event: Event) -> std::result::Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn a_refused_page_renders_as_one_error_banner() {
        let tree = render_page(&mut Pages(1));
        assert_eq!(tree.elements().len(), 2);

        let tree = render_page(&mut Pages(ui::limits::MAX_ELEMENTS));
        assert_eq!(tree.root(), 0);
        match tree.elements() {
            [Element::Banner { kind: BannerKind::Error, text }] => assert!(text.contains("2000 elements"), "{text}"),
            other => panic!("expected one error banner, got {other:?}"),
        }
    }

    #[test]
    fn event_id_names_the_control() {
        assert_eq!(Event::Clicked { id: "a".into() }.id(), "a");
        assert_eq!(Event::Changed { id: "b".into(), value: "x".into() }.id(), "b");
    }

    #[test]
    fn error_converts_into_its_message() {
        let text: String = Error::new(ErrorKind::Denied, "no permission").into();
        assert_eq!(text, "no permission");
    }
}

//! The glue behind [`export_plugin!`](crate::export_plugin): the plugin instance and the conversions between the
//! SDK's types and the generated WIT types.
//!
//! Not part of the public API: only the macro's expansion uses it, and it may change in any release.

use std::cell::RefCell;

use crate::bindings::aio::plugin::types as wit;
use crate::ui::{BannerKind, Element, UiTree};
use crate::{render_page, Event, Plugin};

/// What the generated export macro (`__export_plugin_impl!`, invoked `with_types_in $crate::export::abi`) refers
/// to: the `Guest` trait and the per-function ABI shims.
pub mod abi {
    pub use crate::bindings::*;
}

/// The page tree in the WIT types.
pub type WitTree = wit::UiTree;
/// A page event in the WIT types.
pub type WitEvent = wit::UiEvent;

/// The one plugin instance of the component, made with [`Plugin::new`] on first use. Lives in a `thread_local`:
/// a component instance is single threaded.
pub struct Slot<P>(RefCell<Option<P>>);

impl<P: Plugin> Default for Slot<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: Plugin> Slot<P> {
    /// An empty slot.
    pub const fn new() -> Self {
        Self(RefCell::new(None))
    }

    /// Runs `f` on the instance, making it first if needed. `None` if the instance is already in use (a host call
    /// never re-enters the guest, so this does not happen).
    fn with<R>(&self, f: impl FnOnce(&mut P) -> R) -> Option<R> {
        let mut slot = self.0.try_borrow_mut().ok()?;
        Some(f(slot.get_or_insert_with(P::new)))
    }

    /// The `render` export: the plugin's page, or a single error banner if the builder refused it.
    pub fn render(&self) -> WitTree {
        let tree = self
            .with(render_page)
            .unwrap_or_else(|| UiTree::banner_page(BannerKind::Error, "the plugin is busy"));
        to_wit(tree)
    }

    /// The `on-event` export.
    pub fn on_event(&self, event: WitEvent) -> Result<(), String> {
        let event = from_wit(event);
        self.with(|plugin| plugin.on_event(event)).unwrap_or_else(|| Err("the plugin is busy".to_string()))
    }
}

/// Converts an event from the WIT types.
fn from_wit(event: WitEvent) -> Event {
    match event {
        wit::UiEvent::Clicked(id) => Event::Clicked { id },
        wit::UiEvent::Changed(wit::ChangedData { id, value }) => Event::Changed { id, value },
    }
}

/// An index into the page's elements; a finished page has at most `MAX_ELEMENTS` (2,000) elements, so it fits.
fn index(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

fn indices(list: Vec<usize>) -> Vec<u32> {
    list.into_iter().map(index).collect()
}

/// Converts a finished page to the WIT types.
fn to_wit(tree: UiTree) -> WitTree {
    let (root, elements) = tree.into_parts();
    let elements = elements
        .into_iter()
        .map(|e| match e {
            Element::Card { title, children } => wit::UiElement::Card(wit::CardData { title, children: indices(children) }),
            Element::Heading(text) => wit::UiElement::Heading(text),
            Element::Text(text) => wit::UiElement::Text(text),
            Element::Button { id, label, primary } => wit::UiElement::Button(wit::ButtonData { id, label, primary }),
            Element::Input { id, label, value } => wit::UiElement::Input(wit::InputData { id, label, value }),
            Element::Checkbox { id, label, checked } => wit::UiElement::Checkbox(wit::CheckboxData { id, label, checked }),
            Element::Select { id, options, selected } => {
                wit::UiElement::Select(wit::SelectData { id, options, selected: index(selected) })
            }
            Element::Table { columns, rows } => wit::UiElement::Table(wit::TableData { columns, rows }),
            Element::Progress { done, total } => wit::UiElement::Progress(wit::ProgressData { done, total }),
            Element::Log(lines) => wit::UiElement::Log(lines),
            Element::Banner { kind, text } => {
                let kind = match kind {
                    BannerKind::Info => wit::BannerKind::Info,
                    BannerKind::Success => wit::BannerKind::Success,
                    BannerKind::Warning => wit::BannerKind::Warning,
                    BannerKind::Error => wit::BannerKind::Error,
                };
                wit::UiElement::Banner(wit::BannerData { kind, text })
            }
            Element::Row(children) => wit::UiElement::Row(indices(children)),
            Element::Column(children) => wit::UiElement::Column(indices(children)),
        })
        .collect();
    wit::UiTree { root: index(root), elements }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ui;

    #[test]
    fn page_converts_to_wit_with_the_same_indices() {
        let mut ui = Ui::new();
        ui.card("c", |ui| ui.row(|ui| ui.select("s", ["a", "b"], 1)));
        ui.banner(BannerKind::Success, "ok");
        let tree = to_wit(ui.finish().unwrap());
        assert_eq!(tree.root, 0);
        assert!(matches!(&tree.elements[0], wit::UiElement::Column(c) if *c == [1, 4]));
        assert!(matches!(&tree.elements[1], wit::UiElement::Card(c) if c.children == [2]));
        assert!(matches!(&tree.elements[2], wit::UiElement::Row(c) if *c == [3]));
        assert!(matches!(&tree.elements[3], wit::UiElement::Select(s) if s.selected == 1 && s.options == ["a", "b"]));
        assert!(matches!(&tree.elements[4], wit::UiElement::Banner(b) if b.kind == wit::BannerKind::Success));
    }

    #[test]
    fn events_convert_from_wit() {
        assert_eq!(from_wit(wit::UiEvent::Clicked("go".into())), Event::Clicked { id: "go".into() });
        let changed = wit::UiEvent::Changed(wit::ChangedData { id: "n".into(), value: "v".into() });
        assert_eq!(from_wit(changed), Event::Changed { id: "n".into(), value: "v".into() });
    }

    struct Counter(u32);

    impl Plugin for Counter {
        fn new() -> Self {
            Counter(0)
        }
        fn render(&mut self, ui: &mut Ui) {
            ui.text(self.0.to_string());
        }
        fn on_event(&mut self, _: Event) -> Result<(), String> {
            self.0 += 1;
            Ok(())
        }
    }

    #[test]
    fn slot_makes_one_instance_and_keeps_it() {
        let slot = Slot::<Counter>::new();
        slot.on_event(wit::UiEvent::Clicked("x".into())).unwrap();
        slot.on_event(wit::UiEvent::Clicked("x".into())).unwrap();
        let tree = slot.render();
        assert!(matches!(&tree.elements[1], wit::UiElement::Text(t) if t == "2"));
    }
}

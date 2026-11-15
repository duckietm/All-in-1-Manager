//! The UI builder, on the host target: the tree it emits and the Manager's limits it enforces.

use aio_plugin_sdk::ui::{limits, BannerKind, Element, Ui};
use aio_plugin_sdk::ErrorKind;

/// The children of a container element.
fn children(e: &Element) -> &[usize] {
    match e {
        Element::Card { children, .. } | Element::Row(children) | Element::Column(children) => children,
        _ => &[],
    }
}

#[test]
fn nested_containers_produce_flat_tree_with_child_indices() {
    let mut ui = Ui::new();
    ui.heading("Title");
    ui.card("Settings", |ui| {
        ui.row(|ui| {
            ui.input("name", "Name", "Ann");
            ui.primary_button("save", "Save");
        });
        ui.checkbox("dry", "Dry run", true);
    });
    ui.column(|ui| ui.text("a\nb"));
    let tree = ui.finish().expect("a small page is accepted");

    let e = tree.elements();
    assert_eq!(tree.root(), 0);
    assert_eq!(e.len(), 9);
    // The page itself is a column holding the top-level elements, in order.
    assert_eq!(e[0], Element::Column(vec![1, 2, 7]));
    assert_eq!(e[1], Element::Heading("Title".into()));
    assert!(matches!(&e[2], Element::Card { title, children, .. } if title == "Settings" && *children == [3, 6]));
    assert_eq!(e[3], Element::Row(vec![4, 5]));
    assert!(matches!(&e[4], Element::Input { id, label, value, .. } if id == "name" && label == "Name" && value == "Ann"));
    assert!(matches!(&e[5], Element::Button { id, label, primary: true, .. } if id == "save" && label == "Save"));
    assert!(matches!(&e[6], Element::Checkbox { id, label, checked: true, .. } if id == "dry" && label == "Dry run"));
    assert_eq!(e[7], Element::Column(vec![8]));
    assert_eq!(e[8], Element::Text("a\nb".into()));
    // Every element but the root is the child of exactly one container, after it.
    let mut parents = vec![0; e.len()];
    for (i, el) in e.iter().enumerate() {
        for &c in children(el) {
            assert!(c > i);
            parents[c] += 1;
        }
    }
    assert_eq!(parents[0], 0);
    assert!(parents[1..].iter().all(|&p| p == 1));
}

#[test]
fn every_leaf_element_is_emitted() {
    let mut ui = Ui::new();
    ui.button("b", "B");
    ui.select("s", ["x", "y"], 1);
    ui.table(["k", "v"], [["1", "one"], ["2", "two"]]);
    ui.progress(3, 10);
    ui.log(["l1", "l2"]);
    ui.banner(BannerKind::Warning, "careful");
    let tree = ui.finish().unwrap();
    let e = tree.elements();
    assert!(matches!(&e[1], Element::Button { id, label, primary: false, .. } if id == "b" && label == "B"));
    assert!(matches!(&e[2], Element::Select { id, options, selected: 1, .. } if id == "s" && *options == ["x", "y"]));
    assert!(matches!(
        &e[3],
        Element::Table { columns, rows, .. } if *columns == ["k", "v"] && *rows == [["1", "one"], ["2", "two"]]
    ));
    assert!(matches!(&e[4], Element::Progress { done: 3, total: 10, .. }));
    assert_eq!(e[5], Element::Log(vec!["l1".into(), "l2".into()]));
    assert!(matches!(&e[6], Element::Banner { kind: BannerKind::Warning, text, .. } if text == "careful"));
}

/// `finish` fails with a message naming the limit.
fn assert_refused(ui: Ui, what: &str) {
    let err = ui.finish().expect_err("the page is past a Manager limit");
    assert_eq!(err.kind(), ErrorKind::Other);
    assert!(err.message().contains(what), "{err}");
}

#[test]
fn builder_enforces_manager_limits() {
    assert_eq!((limits::MAX_ELEMENTS, limits::MAX_DEPTH, limits::MAX_CELLS, limits::MAX_LINES), (2_000, 32, 20_000, 5_000));

    // Elements: the page column plus 1,999 headings is exactly 2,000; one more is 2,001.
    let mut ui = Ui::new();
    for _ in 0..limits::MAX_ELEMENTS - 1 {
        ui.heading("h");
    }
    assert_eq!(ui.clone().finish().unwrap().elements().len(), limits::MAX_ELEMENTS);
    ui.heading("one too many");
    assert_refused(ui, "2000 elements");

    // Depth: the page is depth 0, so 32 nested columns reach depth 32; a 33rd is too deep.
    fn nest(ui: &mut Ui, levels: usize) {
        if levels > 0 {
            ui.column(|ui| nest(ui, levels - 1));
        }
    }
    let mut ui = Ui::new();
    nest(&mut ui, limits::MAX_DEPTH);
    assert_eq!(ui.finish().unwrap().elements().len(), limits::MAX_DEPTH + 1);
    let mut ui = Ui::new();
    nest(&mut ui, limits::MAX_DEPTH + 1);
    assert_refused(ui, "nested");

    // Cells: table headers and cells plus select options, over the whole page.
    let row = vec!["c"; 100];
    let mut ui = Ui::new();
    ui.table(row.clone(), vec![row.clone(); 199]); // 100 + 19,900 = 20,000
    assert!(ui.clone().finish().is_ok());
    ui.select("pick", ["one more"], 0); // 20,001
    assert_refused(ui, "20000 table cells");

    // Lines: a text counts its lines (at least 1), a log its entries.
    let mut ui = Ui::new();
    ui.log(vec!["line"; limits::MAX_LINES - 3]);
    ui.text("a\nb\nc");
    assert!(ui.clone().finish().is_ok());
    ui.text(""); // an empty text is one line: 5,001
    assert_refused(ui, "5000 lines");
}

#[test]
fn the_first_error_wins_and_the_rest_is_ignored() {
    let mut ui = Ui::new();
    ui.button("a", "A");
    ui.button("a", "again");
    nest_too_deep(&mut ui);
    let err = ui.finish().unwrap_err();
    assert!(err.message().contains("\"a\""), "{err}");
}

fn nest_too_deep(ui: &mut Ui) {
    fn nest(ui: &mut Ui, levels: usize) {
        if levels > 0 {
            ui.row(|ui| nest(ui, levels - 1));
        }
    }
    nest(ui, limits::MAX_DEPTH + 5);
}

#[test]
fn ids_must_be_unique() {
    let mut ui = Ui::new();
    ui.button("go", "Go");
    ui.card("", |ui| ui.button("go", "Go again"));
    let err = ui.finish().expect_err("a duplicate id is refused");
    assert_eq!(err.kind(), ErrorKind::Other);
    assert!(err.message().contains("\"go\""), "{err}");

    // Ids are shared by every kind of control.
    let mut ui = Ui::new();
    ui.input("x", "X", "");
    ui.checkbox("x", "X", false);
    assert!(ui.finish().is_err());

    // An empty id is refused too: the Manager could not tell the control's events apart.
    let mut ui = Ui::new();
    ui.select("", ["a"], 0);
    assert!(ui.finish().is_err());

    // Distinct ids are fine.
    let mut ui = Ui::new();
    ui.button("a", "A");
    ui.input("b", "B", "");
    ui.checkbox("c", "C", false);
    ui.select("d", ["1"], 0);
    assert!(ui.finish().is_ok());
}

#[test]
fn select_index_must_name_an_option() {
    let mut ui = Ui::new();
    ui.select("s", ["a", "b"], 2);
    assert!(ui.finish().unwrap_err().message().contains("option 2"));
    // An empty select has nothing to choose: index 0 is the only valid one.
    let mut ui = Ui::new();
    ui.select("s", Vec::<String>::new(), 0);
    assert!(ui.finish().is_ok());
}

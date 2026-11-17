use std::path::PathBuf;

use wit_parser::{Resolve, WorldItem, WorldKey};

#[test]
fn wit_package_parses() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../wit/aio-plugin");
    let mut resolve = Resolve::default();
    let (pkg, _) = resolve.push_dir(&dir).expect("the WIT package parses");

    assert_eq!(resolve.packages[pkg].name.to_string(), "aio:plugin@1.1.0");

    let world_id = resolve.packages[pkg].worlds["plugin"];
    let world = &resolve.worlds[world_id];
    for export in ["render", "on-event", "tick"] {
        assert!(
            matches!(world.exports.get(&WorldKey::Name(export.into())), Some(WorldItem::Function(_))),
            "world plugin must export {export}"
        );
    }

    let host = world
        .imports
        .values()
        .find_map(|item| match item {
            WorldItem::Interface { id, .. } if resolve.interfaces[*id].name.as_deref() == Some("host") => Some(*id),
            _ => None,
        })
        .expect("world plugin imports interface host");
    let host = &resolve.interfaces[host];
    for func in [
        "prompt-text",
        "pick-open-file",
        "pick-save-file",
        "open-import-preview",
        "[method]picked-file.name",
        "[method]picked-file.read",
        "[method]picked-file.write",
        "keep-working",
        "[method]tcp-connection.recv-timeout",
        "pick-folder",
        "[method]picked-folder.name",
        "[method]picked-folder.write",
        "[method]picked-folder.append",
        "[method]picked-folder.reveal",
        "[method]picked-folder.open-import-preview",
        "[method]picked-folder.read",
        "[method]picked-folder.list",
        "language",
        "translate",
        "http-get-many",
        "open-manager-page",
    ] {
        assert!(host.functions.contains_key(func), "host must import {func}");
    }
    assert!(host.types.contains_key("picked-file"), "host must define resource picked-file");
    assert!(host.types.contains_key("picked-folder"), "host must define resource picked-folder");
    assert!(host.types.contains_key("folder-entry"), "host must define record folder-entry");
    assert!(host.types.contains_key("manager-page"), "host must define enum manager-page");
}

/// The 1.0 text is kept verbatim for the backward-compatibility tests: 1.1 only adds items.
#[test]
fn wit_1_0_is_kept_and_1_1_only_adds() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../wit");
    let mut old = Resolve::default();
    let (old_pkg, _) = old.push_dir(root.join("aio-plugin-1.0")).expect("the 1.0 WIT package parses");
    assert_eq!(old.packages[old_pkg].name.to_string(), "aio:plugin@1.0.0");
    let mut new = Resolve::default();
    let (new_pkg, _) = new.push_dir(root.join("aio-plugin")).unwrap();

    let host = |r: &Resolve, pkg| r.interfaces[r.packages[pkg].interfaces["host"]].functions.keys().cloned().collect::<Vec<_>>();
    let (old_host, new_host) = (host(&old, old_pkg), host(&new, new_pkg));
    for f in &old_host {
        assert!(new_host.contains(f), "1.1 keeps host.{f}");
    }
    let added: Vec<_> = new_host.iter().filter(|f| !old_host.contains(f)).collect();
    assert_eq!(
        added,
        [
            "http-get-many",
            "[method]tcp-connection.recv-timeout",
            "[method]picked-folder.name",
            "[method]picked-folder.write",
            "[method]picked-folder.append",
            "[method]picked-folder.read",
            "[method]picked-folder.list",
            "[method]picked-folder.reveal",
            "[method]picked-folder.open-import-preview",
            "pick-folder",
            "open-manager-page",
            "keep-working",
            "language",
            "translate",
        ]
    );
    // Every added item says since when it exists.
    let host_id = new.packages[new_pkg].interfaces["host"];
    for f in &added {
        let stability = &new.interfaces[host_id].functions[f.as_str()].stability;
        assert!(matches!(stability, wit_parser::Stability::Stable { .. }), "host.{f} needs @since: {stability:?}");
    }
    for added_type in ["picked-folder", "folder-entry", "manager-page"] {
        let id = new.interfaces[host_id].types[added_type];
        assert!(matches!(new.types[id].stability, wit_parser::Stability::Stable { .. }), "{added_type} needs @since");
    }
    let types = |r: &Resolve, pkg| r.interfaces[r.packages[pkg].interfaces["types"]].types.keys().cloned().collect::<Vec<_>>();
    assert_eq!(types(&old, old_pkg), types(&new, new_pkg), "the types are unchanged");
}

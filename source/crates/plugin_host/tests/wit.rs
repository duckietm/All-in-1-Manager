use std::path::PathBuf;

use wit_parser::{Resolve, WorldItem, WorldKey};

#[test]
fn wit_package_parses() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../wit/aio-plugin");
    let mut resolve = Resolve::default();
    let (pkg, _) = resolve.push_dir(&dir).expect("the WIT package parses");

    assert_eq!(resolve.packages[pkg].name.to_string(), "aio:plugin@1.0.0");

    let world_id = resolve.packages[pkg].worlds["plugin"];
    let world = &resolve.worlds[world_id];
    for export in ["render", "on-event"] {
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
    ] {
        assert!(host.functions.contains_key(func), "host must import {func}");
    }
    assert!(host.types.contains_key("picked-file"), "host must define resource picked-file");
}

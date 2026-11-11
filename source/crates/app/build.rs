//! The exe icon and version information on Windows. The client images (source/Assets) and the default catalogue are
//! not in the program any more: they are data packs of the Plugins page (src/plugins/packs.rs), published with each
//! release as plugin-client-assets.zip and plugin-default-catalog.zip (source/tools/make_packs.py).

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=resources/app.rc");
        println!("cargo:rerun-if-changed=resources/app.ico");
        embed_resource::compile("resources/app.rc", embed_resource::NONE).manifest_optional().unwrap();
    }
}

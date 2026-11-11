//! Host side of the external plugin API `aio:plugin@1.0.0`.

/// Generated bindings for the WIT world `plugin` (see `source/wit/aio-plugin/plugin.wit`).
pub mod bindings {
    wasmtime::component::bindgen!({
        world: "plugin",
        path: "../../wit/aio-plugin",
        imports: { default: async },
        exports: { default: async },
    });
}

pub mod manifest;
pub mod package;
pub mod permissions;

pub use manifest::{Manifest, Tile};
pub use package::{inspect, install, install_failing_after_move_aside, installed, remove, Package};
pub use permissions::Permission;

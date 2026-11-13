//! Host side of the external plugin API `aio:plugin@1.0.0`.

/// Generated bindings for the WIT world `plugin` (see `source/wit/aio-plugin/plugin.wit`).
pub mod bindings {
    wasmtime::component::bindgen!({
        world: "plugin",
        path: "../../wit/aio-plugin",
        imports: { default: async },
        exports: { default: async },
        // The resources are host structs in `runtime`, kept in the store's `ResourceTable`.
        with: {
            "aio:plugin/host.picked-file": crate::runtime::PickedFile,
            "aio:plugin/host.tcp-connection": crate::runtime::TcpConnection,
        },
    });
}

pub mod approvals;
pub mod manifest;
pub mod net;
pub mod package;
pub mod permissions;
pub mod runtime;
pub mod services;
pub mod sql;

pub use async_trait::async_trait;

pub use manifest::{display_safe, Manifest, Tile};
#[cfg(feature = "test-hooks")]
pub use package::{install_failing_after_move_aside, install_failing_with_data_restore_failure, install_with_fault, Fault, Step};
pub use package::{inspect, install, installed, remove, Package};
pub use permissions::Permission;

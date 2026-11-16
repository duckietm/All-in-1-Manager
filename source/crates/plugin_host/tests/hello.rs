mod support;

use aio_plugin_host::permissions::Permission;
use aio_plugin_host::inspect;

#[test]
fn hello_package_is_valid() {
    let pkg = inspect(&support::hello_aiop(), &["default-catalog", "cms"]).unwrap();
    assert_eq!(pkg.manifest.id, "hello");
    assert_eq!(pkg.manifest.permissions, vec![Permission::Files]);
    assert!(pkg.wasm.starts_with(b"\0asm"));
}

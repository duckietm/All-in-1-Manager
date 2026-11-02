mod common;

use aio_remote::nginx;
use common::{check, compare, log};

#[tokio::test]
async fn prelude() {
    compare("prelude", aio_remote::script::PRELUDE);
}

#[tokio::test]
async fn nginx_scripts() {
    check("nginx_inspect", None, |s| async move { nginx::inspect(&*s).await }).await;
    check("nginx_add_repository", None, |s| async move { nginx::add_repository(&*s, &log).await }).await;
    check("nginx_remove_distro", None, |s| async move { nginx::remove_distro_nginx(&*s, &log).await }).await;
    check("nginx_install_package", None, |s| async move { nginx::install_package(&*s, &log).await }).await;
    check("nginx_debian_layout", None, |s| async move { nginx::set_up_debian_layout(&*s, &log).await }).await;
    check("nginx_start_and_test", None, |s| async move { nginx::start_and_test(&*s, &log).await }).await;
}

//! Group A: php, mariadb, dev_tools, git_clone against tools/parity/Scenarios_A.cs.

mod common;

use aio_core::DatabaseCredentials;
use aio_remote::{dev_tools, git_clone, mariadb, php, CommandResult};
use common::{check, compare, log};

type Responder = Option<Box<dyn Fn(&str, &str, Option<&str>) -> CommandResult + Send + Sync>>;

/// Answers every command with this exit code, output and error (the harness' Answer).
fn reply(exit_code: i32, output: &'static str, error: &'static str) -> Responder {
    Some(Box::new(move |_: &str, _: &str, _: Option<&str>| CommandResult { exit_code, output: output.into(), error: error.into() }))
}

fn credentials(database: &str, root_user: &str, root_password: &str, cms_user: &str, cms_password: &str) -> DatabaseCredentials {
    DatabaseCredentials {
        host: "192.0.2.10".into(),
        port: 3306,
        database: database.into(),
        root_user: root_user.into(),
        root_password: root_password.into(),
        cms_user: cms_user.into(),
        cms_password: cms_password.into(),
        emulator_user: "emulator".into(),
        emulator_password: String::new(),
    }
}

fn or_null(value: Option<String>) -> String {
    value.unwrap_or_else(|| "null".into())
}

#[tokio::test]
async fn php_scripts() {
    check("php_inspect", None, |s| async move { php::inspect(&*s).await }).await;
    check("php_inspect_failed", reply(1, "", " permission denied \n"), |s| async move { php::inspect(&*s).await }).await;
    check("php_add_repository", None, |s| async move { php::add_repository(&*s, &log).await }).await;
    check("php_install_packages", reply(0, "  installing\nVERSION=8.5.13\n", ""), |s| async move {
        let v = php::install_packages(&*s, &log).await?;
        s.note(&format!("result: {v}"));
        Ok(())
    })
    .await;
    check("php_check_extensions", None, |s| async move { php::check_extensions(&*s, &log).await }).await;
    check("php_check_extensions_failed", reply(1, "  mysqli ok\nERROR: These PHP extensions are not loaded: gd\n", ""), |s| async move {
        php::check_extensions(&*s, &log).await
    })
    .await;
    check("php_start_and_connect", reply(0, "NGINX=yes\n", ""), |s| async move {
        let nginx = php::start_and_connect(&*s, &log).await?;
        s.note(&format!("result: {}", if nginx { "True" } else { "False" }));
        Ok(())
    })
    .await;
    let versions = ["8.5.12", "8.5.13-1+ubuntu24.04.1+deb.sury.org+1", "8.6.0", "8.5.12.1", "8.5.11", "8.5", "8", "", "8.5.", "8.5.1.2.3", "9.0"];
    let text: Vec<String> =
        versions.iter().map(|v| format!("{v}={}", if php::is_at_least_minimum(v) { "True" } else { "False" })).collect();
    compare("php_is_at_least_minimum", &(text.join("\n") + "\n"));
}

#[tokio::test]
async fn mariadb_scripts() {
    let creds = credentials("habbo", "root", "R00t'pass\\1", "cms", "Cms\"pass2");
    let odd = credentials("hab`bo", "ad'min", "pw1", "c\"ms", "pw2");

    check("mariadb_inspect", None, |s| async move { mariadb::inspect(&*s).await }).await;
    check("mariadb_inspect_failed", reply(1, "", "boom"), |s| async move { mariadb::inspect(&*s).await }).await;
    check("mariadb_add_repository", None, |s| async move { mariadb::add_repository(&*s, &log).await }).await;
    check("mariadb_backup", reply(0, "BACKUP=/root/mariadb-backup-1.sql.gz\n", ""), |s| async move {
        let path = mariadb::backup(&*s, &log).await?;
        s.note(&format!("result: {path}"));
        Ok(())
    })
    .await;
    check("mariadb_install", reply(0, "VERSION=12.3.2+maria~ubu2404\n", ""), |s| async move {
        let v = mariadb::install(&*s, &log).await?;
        s.note(&format!("result: {v}"));
        Ok(())
    })
    .await;
    check("mariadb_secure_and_listen", None, |s| async move { mariadb::secure_and_listen(&*s, &log).await }).await;
    let c = creds.clone();
    check("mariadb_allow_tunnel_root_login", None, |s| async move { mariadb::allow_tunnel_root_login(&*s, &c, &log).await }).await;
    let c = creds.clone();
    check("mariadb_reset_root_password", None, |s| async move { mariadb::reset_root_password(&*s, &c, &log).await }).await;
    check("mariadb_read_accounts", reply(0, "DB=habbo\nCMSUSER=cms\nCMSPASS=secret\nEMUUSER=\nEMUPASS=pw\n", ""), |s| async move {
        let a = mariadb::read_accounts(&*s).await?;
        let f = |v: Option<String>| v.unwrap_or_default();
        s.note(&format!(
            "result: {}|{}|{}|{}|{}",
            f(a.database),
            f(a.cms_user),
            f(a.cms_password),
            f(a.emulator_user),
            f(a.emulator_password)
        ));
        Ok(())
    })
    .await;
    let c = creds.clone();
    check("mariadb_create_users", None, |s| async move { mariadb::create_users(&*s, &c, &log).await }).await;
    let c = odd.clone();
    check("mariadb_create_users_quoted", None, |s| async move { mariadb::create_users(&*s, &c, &log).await }).await;
    check("mariadb_test_login_root", None, |s| async move {
        let r = mariadb::test_login(&*s, "root", "R00t'pass\\1", None, false, &log, true).await?;
        s.note(&format!("result: {}", or_null(r)));
        Ok(())
    })
    .await;
    check("mariadb_test_login_cms_local", None, |s| async move {
        let r = mariadb::test_login(&*s, "cms", "Cms\"pass2", Some("habbo"), true, &log, true).await?;
        s.note(&format!("result: {}", or_null(r)));
        Ok(())
    })
    .await;
    check("mariadb_test_login_local_only", None, |s| async move {
        let r = mariadb::test_login(&*s, "cms", "pw", Some("hab`bo"), true, &log, false).await?;
        s.note(&format!("result: {}", or_null(r)));
        Ok(())
    })
    .await;
    check("mariadb_test_login_failed", reply(1, "ERROR: Login as cms (--protocol=socket) failed: denied\n", ""), |s| async move {
        let r = mariadb::test_login(&*s, "cms", "pw", Some("habbo"), true, &log, true).await?;
        s.note(&format!("result: {}", or_null(r)));
        Ok(())
    })
    .await;
    check("mariadb_test_local_root", None, |s| async move {
        let r = mariadb::test_local_root(&*s, &log).await?;
        s.note(&format!("result: {}", or_null(r)));
        Ok(())
    })
    .await;
    check("mariadb_test_local_root_failed", reply(2, "some output\n", ""), |s| async move {
        let r = mariadb::test_local_root(&*s, &log).await?;
        s.note(&format!("result: {}", or_null(r)));
        Ok(())
    })
    .await;
    compare("mariadb_local_root_password_sql", &(mariadb::local_root_password_sql(&creds) + "\n"));
}

#[tokio::test]
async fn dev_tools_scripts() {
    check("dev_tools_inspect", None, |s| async move { dev_tools::inspect(&*s).await }).await;
    check("dev_tools_add_node_source", None, |s| async move { dev_tools::add_node_source(&*s, &log).await }).await;
    check("dev_tools_install_node", reply(0, "VERSION=v26.1.0\n", ""), |s| async move {
        let v = dev_tools::install_node(&*s, &log).await?;
        s.note(&format!("result: {v}"));
        Ok(())
    })
    .await;
    check("dev_tools_install_composer", None, |s| async move { dev_tools::install_composer(&*s, &log).await }).await;
    check("dev_tools_install_yarn", None, |s| async move { dev_tools::install_yarn(&*s, &log).await }).await;
    check(
        "dev_tools_versions",
        reply(0, "NODE=v26.1.0\nNPM=11.0.0\nYARN=4.9.1\nCOMPOSER=2.8.10\nSHADOWING=/usr/local/bin/node /usr/local/bin/npm \n", ""),
        |s| async move {
            let v = dev_tools::versions(&*s, &log).await?;
            s.note(&format!("result: {}|{}|{}|{}|{}", v.node, v.npm, v.yarn, v.composer, v.shadowing.join(",")));
            Ok(())
        },
    )
    .await;
    check("dev_tools_versions_missing", reply(0, "NODE=v26.1.0\nNPM=\n", ""), |s| async move { dev_tools::versions(&*s, &log).await })
        .await;
}

const URL: &str = "https://github.com/ObjectRetros/atomcms.git";
const TARGET: &str = "/var/www/it's here";

fn clone_state(s: &git_clone::CloneState) -> String {
    let f = |v: &Option<String>| v.clone().unwrap_or_default();
    format!("result: {:?}|{}|{}|{}|{}|{}", s.kind, f(&s.origin), f(&s.branch), f(&s.commit), s.changed_files, s.behind)
}

#[tokio::test]
async fn git_clone_scripts() {
    check(
        "git_clone_inspect",
        reply(0, "origin=git@github.com:objectretros/atomcms\nkind=repo\nbranch=main\ncommit=abc1234 2026-01-01 Fix\nchanged=      3\nbehind=2\n", ""),
        |s| async move {
            let state = git_clone::inspect(&*s, URL, TARGET).await?;
            s.note(&clone_state(&state));
            Ok(())
        },
    )
    .await;
    check("git_clone_inspect_other", reply(0, "origin=https://github.com/someone/else\nkind=repo\nchanged=x\n", ""), |s| async move {
        let state = git_clone::inspect(&*s, URL, TARGET).await?;
        s.note(&clone_state(&state));
        Ok(())
    })
    .await;
    check("git_clone_move_aside", reply(0, "BACKUP=/var/www/x.bak-1\n", ""), |s| async move {
        let path = git_clone::move_aside(&*s, TARGET, &log).await?;
        s.note(&format!("result: {path}"));
        Ok(())
    })
    .await;
    check("git_clone_clone_default", reply(0, "COMMIT=main abc1234 2026-01-01 Fix\n", ""), |s| async move {
        let commit = git_clone::clone(&*s, URL, TARGET, None, &log).await?;
        s.note(&format!("result: {commit}"));
        Ok(())
    })
    .await;
    check("git_clone_clone_branch", None, |s| async move { git_clone::clone(&*s, URL, TARGET, Some("De'v"), &log).await }).await;
    check("git_clone_update_default", None, |s| async move { git_clone::update(&*s, TARGET, None, &log).await }).await;
    check("git_clone_update_branch", None, |s| async move { git_clone::update(&*s, TARGET, Some("dev"), &log).await }).await;
    check(
        "git_clone_update_failed",
        reply(1, " M README.md\nERROR: Files tracked by git were changed in /x. Commit or undo those changes first, so they are not lost.\n", ""),
        |s| async move { git_clone::update(&*s, TARGET, None, &log).await },
    )
    .await;
}

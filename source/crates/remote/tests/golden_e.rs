//! Group E parity: AtomCMS, your own CMS, EpicNext and Gitea (tools/parity/Scenarios_E.cs).

mod common;

use aio_core::DatabaseCredentials;
use aio_remote::atom_cms::{self, AtomCmsState};
use aio_remote::custom_cms::{self, CustomCmsState};
use aio_remote::gitea::{self, GiteaSettings, GiteaState};
use aio_remote::recorder::Recorder;
use aio_remote::web_access::WebAccessSettings;
use aio_remote::{epicnext, CommandResult};
use common::{answer, check, compare, log};

fn yes(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn opt(value: Option<&str>) -> String {
    value.unwrap_or("<none>").to_string()
}

fn opt_bool(value: Option<bool>) -> &'static str {
    match value {
        None => "<none>",
        Some(v) => yes(v),
    }
}

const ATOM_INSPECT_OUTPUT: &str = "conf_ADDRESS=hotel.example.com\nconf_SCHEME=https\nconf_CLOUDFLARE=yes\nconf_CMS=atom\nconf_CMS_PORT=86\nphp=8.5.1\ncomposer=yes\nnode=v26.12.1\noctane=yes\nemulator=yes\nname=My Hotel\nvendor=yes\ntheme=yes\nmigrated=yes\nkey=abc-123\ncompleted=1\nsite=yes\n";

fn note_atom(r: &Recorder, s: &AtomCmsState) {
    r.note(&format!(
        "site={} atom={} port={} php={} composer={} node={} octane={} emulator={} name={} vendor={} theme={} migrated={} serves={} key={} completed={} complete={}",
        opt(s.site.as_ref().map(|s| s.url()).as_deref()),
        yes(s.site.as_ref().is_some_and(|s| s.atom_cms)),
        s.site.as_ref().map(|s| s.cms_port).unwrap_or(-1),
        opt(s.php_version.as_deref()),
        yes(s.composer_present),
        opt(s.node_version.as_deref()),
        yes(s.octane_built),
        yes(s.emulator_configured),
        opt(s.hotel_name.as_deref()),
        yes(s.vendor_present),
        yes(s.theme_built),
        yes(s.migrated),
        yes(s.site_serves_atom),
        opt(s.installation_key.as_deref()),
        yes(s.installation_completed),
        yes(s.is_complete()),
    ));
}

fn note_custom(r: &Recorder, s: &CustomCmsState) {
    r.note(&format!(
        "site={} name={} root={} php={} content={}",
        opt(s.site().map(|s| s.url()).as_deref()),
        opt(s.current_name.as_deref()),
        opt(s.current_root.as_deref()),
        opt_bool(s.current_php),
        yes(s.folder_has_content)
    ));
}

fn note_gitea(r: &Recorder, s: &GiteaState) {
    r.note(&format!(
        "site={} docker={} traefik={} domain={} cloudflare={} port={} running={} version={} installed={}",
        opt(s.site.as_ref().map(|s| s.url()).as_deref()),
        yes(s.docker_running),
        yes(s.traefik_running),
        opt(s.domain.as_deref()),
        opt_bool(s.cloudflare),
        s.port,
        yes(s.running),
        opt(s.version.as_deref()),
        yes(s.installed())
    ));
}

fn problems(results: impl IntoIterator<Item = (String, Option<String>)>) -> String {
    results.into_iter().map(|(input, result)| format!("[{input}] -> {}\n", result.as_deref().unwrap_or("<ok>"))).collect()
}

fn https() -> WebAccessSettings {
    WebAccessSettings { atom_cms: true, ..WebAccessSettings::new("www.example.com", true, true) }
}

fn ip() -> WebAccessSettings {
    WebAccessSettings::new("192.0.2.10", false, false)
}

fn custom() -> WebAccessSettings {
    WebAccessSettings { custom_cms: Some("mycms".into()), cms_port: 87, ..WebAccessSettings::new("hotel.example.com", true, false) }
}

fn credentials() -> DatabaseCredentials {
    DatabaseCredentials::new("127.0.0.1", 3306, "hotel", "root", "rootpw", "cmsuser", "p'w$\"x\\y")
}

#[tokio::test]
async fn atom_cms_scripts() {
    check("atom_cms_inspect_empty", None, |s| async move {
        let state = atom_cms::inspect(&*s).await?;
        note_atom(&s, &state);
        Ok(())
    })
    .await;
    check("atom_cms_inspect_installed", answer(&[("php8.5", ATOM_INSPECT_OUTPUT)]), |s| async move {
        let state = atom_cms::inspect(&*s).await?;
        note_atom(&s, &state);
        Ok(())
    })
    .await;
    check("atom_cms_configure_env_https", None, |s| async move {
        atom_cms::configure_env(&*s, &https(), "My \"Hotel\" $1 it's", Some(&credentials()), &log).await
    })
    .await;
    check("atom_cms_configure_env_ip", None, |s| async move { atom_cms::configure_env(&*s, &ip(), "Hotel", None, &log).await }).await;
    check("atom_cms_composer_install", None, |s| async move { atom_cms::composer_install(&*s, &log).await }).await;
    check("atom_cms_migrate_first", answer(&[("FIRST=", "  migrated\nFIRST=yes\n")]), |s| async move {
        let first = atom_cms::migrate(&*s, "It's \\ Hotel", &log).await?;
        s.note(&format!("first={}", yes(first)));
        Ok(())
    })
    .await;
    check("atom_cms_migrate_again", None, |s| async move {
        let first = atom_cms::migrate(&*s, "Hotel", &log).await?;
        s.note(&format!("first={}", yes(first)));
        Ok(())
    })
    .await;
    check(
        "atom_cms_migrate_fails",
        Some(Box::new(|_: &str, _: &str, _: Option<&str>| CommandResult {
            exit_code: 1,
            output: "something\nERROR: boom\nlast line\n".into(),
            error: String::new(),
        })),
        |s| async move { atom_cms::migrate(&*s, "Hotel", &log).await },
    )
    .await;
    check("atom_cms_installation_key_https", answer(&[("KEY=", "KEY=abc-123\nCOMPLETED=1\n")]), |s| async move {
        let (key, completed) = atom_cms::installation_key(&*s, &https(), &log).await?;
        s.note(&format!("key={} completed={}", opt(key.as_deref()), yes(completed)));
        Ok(())
    })
    .await;
    check("atom_cms_installation_key_ip", None, |s| async move {
        let (key, completed) = atom_cms::installation_key(&*s, &ip(), &log).await?;
        s.note(&format!("key={} completed={}", opt(key.as_deref()), yes(completed)));
        Ok(())
    })
    .await;
    check("atom_cms_test_https", None, |s| async move { atom_cms::test(&*s, &https(), &log).await }).await;
    check("atom_cms_test_ip", None, |s| async move { atom_cms::test(&*s, &ip(), &log).await }).await;
}

const CUSTOM_ENV: &str = "conf_ADDRESS=hotel.example.com\nconf_SCHEME=https\nconf_CMS=custom\nconf_CMS_NAME=oldcms\nconf_CMS_PORT=87\n";

#[tokio::test]
async fn custom_cms_scripts() {
    check("custom_cms_inspect_none", None, |s| async move {
        let state = custom_cms::inspect(&*s, None).await?;
        note_custom(&s, &state);
        Ok(())
    })
    .await;
    check("custom_cms_inspect_invalid_name", None, |s| async move {
        let state = custom_cms::inspect(&*s, Some("Bad Name")).await?;
        note_custom(&s, &state);
        Ok(())
    })
    .await;
    check(
        "custom_cms_inspect_name",
        answer(&[("php8.5", CUSTOM_ENV), ("fastcgi_pass", "root=/var/www/mycms/public\nphp=yes\ncontent=yes\n")]),
        |s| async move {
            let state = custom_cms::inspect(&*s, Some("mycms")).await?;
            note_custom(&s, &state);
            Ok(())
        },
    )
    .await;
    check("custom_cms_inspect_current", answer(&[("php8.5", CUSTOM_ENV), ("fastcgi_pass", "root=/var/www/oldcms\nphp=no\n")]), |s| async move {
        let state = custom_cms::inspect(&*s, Some("")).await?;
        note_custom(&s, &state);
        Ok(())
    })
    .await;
    check("custom_cms_inspect_other_root", answer(&[("fastcgi_pass", "root=/srv/elsewhere\nphp=maybe\n")]), |s| async move {
        let state = custom_cms::inspect(&*s, Some("mycms")).await?;
        note_custom(&s, &state);
        Ok(())
    })
    .await;
    check("custom_cms_check_git_access", None, |s| async move {
        custom_cms::check_git_access(&*s, "https://github.com/owner/it's.git", &log).await
    })
    .await;
    check("custom_cms_extract_zip", None, |s| async move { custom_cms::extract_zip(&*s, "/tmp/my cms.zip", "mycms", &log).await }).await;
    check("custom_cms_prepare_php", None, |s| async move { custom_cms::prepare(&*s, "mycms", " /public/ ", true, &log).await }).await;
    check("custom_cms_prepare_static", None, |s| async move { custom_cms::prepare(&*s, "mycms", "/", false, &log).await }).await;
    check("custom_cms_test_https", None, |s| async move { custom_cms::test(&*s, &custom(), &log).await }).await;
    check("custom_cms_test_ip", None, |s| async move { custom_cms::test(&*s, &ip(), &log).await }).await;

    let mut results: Vec<(String, Option<String>)> = Vec::new();
    for n in ["", "mycms", "My CMS", "abcdefghijklm", "hotel", "gamedata", "cms2"] {
        results.push((format!("name {n}"), custom_cms::name_problem(n)));
    }
    for r in ["/", "/public", "/public/", "public", "/../etc", "/a/./b", "/a b", "//"] {
        results.push((format!("root {r}"), custom_cms::root_problem(r)));
    }
    for u in ["https://github.com/owner/cms.git", "https://git.example.com:3000/o/c", "http://github.com/o/c", "git@github.com:o/c.git"] {
        results.push((format!("git {u}"), custom_cms::git_url_problem(u)));
    }
    for u in ["https://github.com/owner/My-CMS.git", "https://github.com/owner/Very_Long-Name-Of-CMS.GIT/", "x"] {
        results.push((format!("name-from-url {u}"), Some(custom_cms::name_from_url(u))));
    }
    for r in [" /public/ ", "", "/", "a", "/web/dist//"] {
        results.push((format!("normalize {r}"), Some(format!("{} {}", custom_cms::normalize_root(r), custom_cms::root_path("mycms", r)))));
    }
    compare("custom_cms_problems", &problems(results));
}

#[tokio::test]
async fn epicnext_scripts() {
    check(
        "epicnext_inspect",
        answer(&[
            ("php8.5", "conf_ADDRESS=hotel.example.com\nconf_SCHEME=https\nconf_CMS=epicnext\nconf_CMS_PORT=88\n"),
            ("HOTEL_NAME", "name=Epic\norigin=https://example.com/repo.git\nbranch=main\nfolder=NextCMS/epicnext-cms\nbuilt=yes\nservice=yes\nrunning=yes\natom=\n"),
        ]),
        |s| async move {
            let st = epicnext::inspect(&*s).await?;
            s.note(&format!(
                "site={} epicnext={} name={} origin={} built={} service={} running={} atom={} branch={} folder={} installed={}",
                opt(st.site().map(|s| s.url()).as_deref()),
                yes(st.site().is_some_and(|s| s.epic_next)),
                opt(st.hotel_name.as_deref()),
                opt(st.origin.as_deref()),
                yes(st.built),
                yes(st.service_installed),
                yes(st.service_running),
                yes(st.atom_tables),
                opt(st.source_branch.as_deref()),
                opt(st.source_folder.as_deref()),
                yes(st.installed())
            ));
            Ok(())
        },
    )
    .await;
    check("epicnext_get_from_folder", answer(&[("COMMIT=", "COMMIT=main abc1234 2026-01-01 First\n")]), |s| async move {
        let commit = epicnext::get_from_folder(&*s, "https://github.com/owner/it's.git", "main", "NextCMS/epicnext-cms", &log).await?;
        s.note(&format!("commit={commit}"));
        Ok(())
    })
    .await;
    check("epicnext_get_from_folder_default_branch", None, |s| async move {
        let commit = epicnext::get_from_folder(&*s, epicnext::REPOSITORY, "", "", &log).await?;
        s.note(&format!("commit={commit}"));
        Ok(())
    })
    .await;
    check("epicnext_prepare_tools", None, |s| async move { epicnext::prepare_tools(&*s, &log).await }).await;
    check("epicnext_configure_env_credentials", None, |s| async move {
        epicnext::configure_env(&*s, &https(), "My \"Hotel\" $1 it's", Some(&credentials()), &log).await
    })
    .await;
    check("epicnext_configure_env_ip", None, |s| async move { epicnext::configure_env(&*s, &ip(), "Hotel", None, &log).await }).await;
    check("epicnext_build", None, |s| async move { epicnext::build(&*s, &log).await }).await;
    check("epicnext_start_service", None, |s| async move { epicnext::start_service(&*s, &log).await }).await;
    check("epicnext_remove_service_keep", None, |s| async move { epicnext::remove_service(&*s, false, &log).await }).await;
    check("epicnext_remove_service_delete", None, |s| async move { epicnext::remove_service(&*s, true, &log).await }).await;
    check("epicnext_test_https", None, |s| async move { epicnext::test(&*s, &https(), &log).await }).await;
    check("epicnext_test_ip", None, |s| async move { epicnext::test(&*s, &ip(), &log).await }).await;

    let mut results: Vec<(String, Option<String>)> = Vec::new();
    for f in ["", "NextCMS/epicnext-cms", "/NextCMS", "a/../b", "a/", "a b"] {
        results.push((format!("folder {f}"), epicnext::folder_problem(f)));
    }
    let long = "b".repeat(101);
    for b in ["", "main", "feature/x", "-x", "a..b", "x/", "/x", "a b", long.as_str()] {
        results.push((format!("branch {b}"), epicnext::branch_problem(b)));
    }
    compare("epicnext_problems", &problems(results));
}

fn gitea_https() -> GiteaSettings {
    GiteaSettings { domain: "git.example.com".into(), https: true, cloudflare: true, admin_user: "gitadmin".into(), admin_email: "admin@example.com".into() }
}

fn gitea_http() -> GiteaSettings {
    GiteaSettings { domain: "git.example.org".into(), https: false, cloudflare: false, admin_user: "o'neil".into(), admin_email: "o'neil@example.org".into() }
}

#[tokio::test]
async fn gitea_scripts() {
    check("gitea_inspect_empty", None, |s| async move {
        let state = gitea::inspect(&*s).await?;
        note_gitea(&s, &state);
        Ok(())
    })
    .await;
    check(
        "gitea_inspect_installed",
        answer(&[(
            "gitea_",
            "conf_ADDRESS=example.com\nconf_SCHEME=https\ndocker=yes\ntraefik=true\ngitea_DOMAIN=git.example.com\ngitea_CLOUDFLARE=no\ngitea_PORT=89\nrunning=true\nversion=1.25.1\n",
        )]),
        |s| async move {
            let state = gitea::inspect(&*s).await?;
            note_gitea(&s, &state);
            Ok(())
        },
    )
    .await;
    check("gitea_check_dns_cloudflare", None, |s| async move { gitea::check_dns(&*s, &gitea_https(), &log).await }).await;
    check("gitea_check_dns_plain", None, |s| async move { gitea::check_dns(&*s, &gitea_http(), &log).await }).await;
    check("gitea_install_https_cloudflare", answer(&[("PORT=", "PORT=86\nPASSWORD=Secr3t\n")]), |s| async move {
        let r = gitea::install(&*s, &gitea_https(), &log).await?;
        s.note(&format!("port={} password={} existed={} cert={}", r.port, opt(r.admin_password.as_deref()), yes(r.admin_existed), yes(r.certificate_valid)));
        Ok(())
    })
    .await;
    check("gitea_install_http", answer(&[("PORT=", "ADMIN=existed\nPORT=87\n")]), |s| async move {
        let r = gitea::install(&*s, &gitea_http(), &log).await?;
        s.note(&format!("port={} password={} existed={} cert={}", r.port, opt(r.admin_password.as_deref()), yes(r.admin_existed), yes(r.certificate_valid)));
        Ok(())
    })
    .await;
    check("gitea_update", None, |s| async move { gitea::update(&*s, &log).await }).await;
    check("gitea_remove_domain_delete", None, |s| async move { gitea::remove(&*s, Some("git.example.com"), true, true, &log).await }).await;
    check("gitea_remove_no_domain_keep", None, |s| async move { gitea::remove(&*s, None, false, false, &log).await }).await;
    check("gitea_test_https", answer(&[("CERT=", "CERT=yes\n")]), |s| async move {
        let cert = gitea::test(&*s, &gitea_https(), &log).await?;
        s.note(&format!("cert={}", yes(cert)));
        Ok(())
    })
    .await;
    check("gitea_test_http", None, |s| async move {
        let cert = gitea::test(&*s, &gitea_http(), &log).await?;
        s.note(&format!("cert={}", yes(cert)));
        Ok(())
    })
    .await;

    let hotel = WebAccessSettings::new("Example.com", true, false);
    let long = format!("{}.com", vec!["a".repeat(60); 5].join("."));
    let mut results: Vec<(String, Option<String>)> = Vec::new();
    for d in ["git.example.com", "example", "-a.example.com", "a.b", "example.com", "x.y.example.co.uk", long.as_str()] {
        results.push((format!("domain {d}"), gitea::domain_problem(d, Some(&hotel))));
    }
    let long_user = "u".repeat(41);
    for u in ["gitadmin", "Admin", ".x", "a", long_user.as_str(), "user.name"] {
        results.push((format!("user {u}"), gitea::user_problem(u)));
    }
    for e in ["a@b.cd", "a@b", "a b@c.de"] {
        results.push((format!("email {e}"), gitea::email_problem(e)));
    }
    let sites = [None, Some(WebAccessSettings::new("www.example.com", true, false)), Some(WebAccessSettings::new("example.com", true, false)), Some(ip())];
    for site in &sites {
        results.push((
            format!("suggest {}", site.as_ref().map(|s| s.address.as_str()).unwrap_or("")),
            Some(gitea::suggested_domain(site.as_ref())),
        ));
    }
    compare("gitea_problems", &problems(results));
}

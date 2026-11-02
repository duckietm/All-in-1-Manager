//! Group C: web_access, hotel_site, octane, local_ports, cms_site (tools/parity/Scenarios_C.cs).

mod common;

use aio_remote::web_access::{self, WebAccessSettings};
use aio_remote::{cms_site, hotel_site, local_ports, octane, CommandResult};
use common::{answer, check, compare, log};

fn site(address: &str, https: bool, cloudflare: bool) -> WebAccessSettings {
    WebAccessSettings::new(address, https, cloudflare)
}

fn http_ip() -> WebAccessSettings {
    site("192.0.2.10", false, false)
}
fn https_domain() -> WebAccessSettings {
    site("hotel.example.com", true, false)
}
fn https_www_cloudflare() -> WebAccessSettings {
    site("www.hotel.example.com", true, true)
}
fn atom_own_port() -> WebAccessSettings {
    WebAccessSettings { atom_cms: true, cms_port: 86, ..site("hotel.example.com", true, true) }
}
fn custom_own_port() -> WebAccessSettings {
    WebAccessSettings { custom_cms: Some("mycms".into()), cms_port: 87, ..site("hotel.example.com", false, false) }
}
fn custom_no_port() -> WebAccessSettings {
    WebAccessSettings { custom_cms: Some("mycms".into()), ..site("hotel.example.com", true, false) }
}
fn epic_next() -> WebAccessSettings {
    WebAccessSettings { epic_next: true, cms_port: 88, ..site("www.hotel.example.com", true, true) }
}

fn sites() -> Vec<(&'static str, WebAccessSettings)> {
    vec![
        ("http_ip", http_ip()),
        ("http_ipv6", site("2001:db8::1", false, false)),
        ("https_domain", https_domain()),
        ("https_www_cloudflare", https_www_cloudflare()),
        ("atom_legacy", WebAccessSettings { atom_cms: true, ..site("hotel.example.com", true, false) }),
        ("atom_own_port", atom_own_port()),
        ("custom_own_port", custom_own_port()),
        ("custom_no_port", custom_no_port()),
        ("epicnext", epic_next()),
    ]
}

const RANGES: &str = "RANGES=173.245.48.0/20,2400:cb00::/32\n";

#[tokio::test]
async fn local_ports_and_hotel_site() {
    compare("local_ports_functions", &local_ports::functions());
    compare("hotel_site_php_locations", &hotel_site::php_locations());
    compare("hotel_site_real_ip_plain", &hotel_site::real_ip_script(&https_domain()));
    compare("hotel_site_real_ip_cloudflare", &hotel_site::real_ip_script(&https_www_cloudflare()));
    for (name, site) in sites() {
        compare(&format!("hotel_site_write_{name}"), &hotel_site::write_script(&site, None));
        compare(&format!("hotel_site_config_{name}"), &hotel_site::config(&site));
    }
    compare("hotel_site_write_root", &hotel_site::write_script(&https_domain(), Some("/var/www/site")));
}

#[tokio::test]
async fn cms_site_scripts() {
    compare("cms_site_write_static", &cms_site::write_script(&https_domain(), "mysite", "/var/www/mysite", false, None));
    compare("cms_site_write_php", &cms_site::write_script(&http_ip(), "atomcms", "/var/www/atomcms/public", true, None));
    compare(
        "cms_site_write_proxy",
        &cms_site::write_script(&epic_next(), "epicnext", "/var/www/epicnext", false, Some("http://127.0.0.1:3000")),
    );
    check("cms_site_apply", Some(Box::new(|_: &str, _: &str, _: Option<&str>| CommandResult::ok("PORT=87\n"))), |s| async move {
        let port = cms_site::apply(&*s, &custom_own_port(), "mycms", "/var/www/mycms", true, &log, None).await?;
        assert_eq!(port, 87);
        Ok(())
    })
    .await;
    check("cms_site_apply_no_port", None, |s| async move {
        cms_site::apply(&*s, &custom_own_port(), "mycms", "/var/www/mycms", true, &log, None).await
    })
    .await;
    check("cms_site_remove", None, |s| async move { cms_site::remove(&*s, None, None, &log).await }).await;
    check("cms_site_remove_delete", None, |s| async move {
        cms_site::remove(&*s, Some("mycms"), Some("/var/www/my cms"), &log).await
    })
    .await;
}

#[tokio::test]
async fn web_access_scripts() {
    compare("web_access_wait_for_traefik", &web_access::wait_for_traefik_script());
    compare("web_access_compose", &web_access::compose_script());
    for (name, site) in sites() {
        compare(&format!("web_access_static_config_{name}"), &web_access::static_config_script(&site));
    }
    check("web_access_inspect", None, |s| async move { web_access::inspect(&*s).await }).await;
    check("web_access_install_docker", None, |s| async move { web_access::install_docker(&*s, &log).await }).await;
    check("web_access_configure_nginx", None, |s| async move { web_access::configure_nginx(&*s, &https_domain(), &log).await }).await;
    for (name, site) in sites() {
        let s1 = site.clone();
        check(&format!("web_access_configure_traefik_{name}"), None, |s| async move {
            web_access::configure_traefik(&*s, &s1, &log).await
        })
        .await;
        let s2 = site.clone();
        check(&format!("web_access_configure_routes_{name}"), None, |s| async move {
            web_access::configure_routes(&*s, &s2, &log).await
        })
        .await;
        check(&format!("web_access_test_{name}"), None, |s| async move { web_access::test(&*s, &site, &log).await }).await;
    }
    check("web_access_configure_nginx_real_ip_plain", None, |s| async move {
        web_access::configure_nginx_real_ip(&*s, &http_ip(), &log).await
    })
    .await;
    check("web_access_configure_nginx_real_ip_cloudflare", None, |s| async move {
        web_access::configure_nginx_real_ip(&*s, &https_www_cloudflare(), &log).await
    })
    .await;
    check("web_access_cloudflare_refresh_off", None, |s| async move {
        web_access::install_cloudflare_refresh(&*s, &https_domain(), &log).await
    })
    .await;
    check("web_access_cloudflare_ranges", answer(&[("RANGES=", RANGES)]), |s| async move {
        let ranges = web_access::cloudflare_ranges(&*s).await?;
        assert_eq!(ranges, vec!["173.245.48.0/20", "2400:cb00::/32"]);
        Ok(())
    })
    .await;
    check("web_access_polaris_trusted_proxies_plain", None, |s| async move {
        let trusted = web_access::polaris_trusted_proxies(&*s, &https_domain()).await?;
        assert_eq!(trusted, "");
        Ok(())
    })
    .await;
    check("web_access_polaris_trusted_proxies_cloudflare", None, |s| async move {
        web_access::polaris_trusted_proxies(&*s, &https_www_cloudflare()).await
    })
    .await;
}

// These need the emulator module (EmulatorInstaller.ControlFunctions, ReadSettingAsync, SetSettingsAsync, imager keys).
#[tokio::test]
async fn web_access_with_emulator() {
    check("web_access_cloudflare_refresh_on", None, |s| async move {
        web_access::install_cloudflare_refresh(&*s, &https_www_cloudflare(), &log).await
    })
    .await;
    check(
        "web_access_configure_polaris",
        answer(&[("RANGES=", RANGES), ("= 'ws.whitelist'", "other.example.com\n")]),
        |s| async move { web_access::configure_polaris(&*s, &https_www_cloudflare(), &log).await },
    )
    .await;
    check("web_access_configure_polaris_star", answer(&[("= 'ws.whitelist'", "*\n")]), |s| async move {
        web_access::configure_polaris(&*s, &http_ip(), &log).await
    })
    .await;
    check("octane_inspect", None, |s| async move { octane::inspect(&*s).await }).await;
}

#[tokio::test]
async fn octane_scripts() {
    check("octane_yarn_install", None, |s| async move { octane::yarn_install(&*s, &octane::renderer(), &log, false).await }).await;
    check("octane_yarn_install_fresh", None, |s| async move { octane::yarn_install(&*s, &octane::octane(), &log, true).await }).await;
    check("octane_configure_plain", None, |s| async move { octane::configure(&*s, &http_ip(), &log, false).await }).await;
    check("octane_configure_cms", None, |s| async move { octane::configure(&*s, &atom_own_port(), &log, false).await }).await;
    check("octane_configure_restore_login", None, |s| async move { octane::configure(&*s, &https_domain(), &log, true).await }).await;
    check("octane_configure_cms_restore_login", None, |s| async move {
        octane::configure(&*s, &custom_no_port(), &log, true).await
    })
    .await;
    check("octane_build_plain", None, |s| async move { octane::build(&*s, &https_domain(), &log).await }).await;
    check("octane_build_cms", None, |s| async move { octane::build(&*s, &epic_next(), &log).await }).await;
    for (name, site) in sites() {
        check(&format!("octane_publish_{name}"), None, |s| async move { octane::publish(&*s, &site, &log).await }).await;
    }
}

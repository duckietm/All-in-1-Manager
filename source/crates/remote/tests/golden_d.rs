//! Group D: harden, ssh_login, root_authorized_keys, fail2ban, crowdsec against tools/parity/Scenarios_D.cs.

mod common;

use aio_core::keys::AuthorizedKey;
use aio_remote::crowdsec::{self, CrowdSecSettings, CrowdSecState};
use aio_remote::fail2ban::{self, Fail2BanSettings, Fail2BanState};
use aio_remote::harden::{self, HardenState};
use aio_remote::recorder::Recorder;
use aio_remote::root_authorized_keys::{self, AuthorizedKeysMode};
use aio_remote::ssh_login::{self, SshLoginState};
use aio_remote::web_access::WebAccessSettings;
use aio_remote::CommandResult;
use common::{answer, check, compare, log};

type Responder = Option<Box<dyn Fn(&str, &str, Option<&str>) -> CommandResult + Send + Sync>>;

fn result(recorder: &Recorder, text: &str) {
    recorder.note(&format!("result: {text}"));
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn or_null(value: Option<&str>) -> &str {
    value.unwrap_or("null")
}

fn list<T: ToString>(items: &[T]) -> String {
    items.iter().map(T::to_string).collect::<Vec<_>>().join(",")
}

fn describe_fail2ban(s: &Fail2BanState) -> String {
    let settings = match &s.settings {
        Some(c) => format!("{}/{}/{}/{}/[{}]", c.max_retry, c.find_time_minutes, c.ban_time_minutes, yes_no(c.increment), list(&c.whitelist)),
        None => "null".into(),
    };
    format!(
        "installed={} version={} running={} configured={} settings={settings} ssh=[{}] banned=[{}] total={} client={} complete={}",
        yes_no(s.installed),
        or_null(s.version.as_deref()),
        yes_no(s.running),
        yes_no(s.configured),
        list(&s.ssh_ports),
        list(&s.banned),
        s.total_banned,
        or_null(s.client_ip.as_deref()),
        yes_no(s.is_complete())
    )
}

fn describe_crowdsec(s: &CrowdSecState) -> String {
    format!(
        "site={} docker={} traefik={} installed={} running={} bouncer={} enrolled={} version={} instance={} port={} whitelist=[{}] banned=[{}] \
dns={} dnscf={} client={} complete={} problem={}",
        or_null(s.site.as_ref().map(|s| s.address.as_str())),
        yes_no(s.docker_running),
        yes_no(s.traefik_running),
        yes_no(s.installed),
        yes_no(s.running),
        yes_no(s.bouncer_configured),
        yes_no(s.enrolled),
        or_null(s.version.as_deref()),
        or_null(s.instance_name.as_deref()),
        s.port,
        list(&s.whitelist),
        list(&s.banned),
        or_null(s.dns_addresses.as_deref()),
        s.dns_behind_cloudflare.map_or("null", yes_no),
        or_null(s.client_ip.as_deref()),
        yes_no(s.is_complete()),
        or_null(crowdsec::problem(s).as_deref())
    )
}

fn describe_harden(s: &HardenState) -> String {
    format!(
        "site={} active={} deny={} ssh=[{}] mariadb={} sysctl={} whitelist={} ipheader={} ufwlog={} trusted={} realipcf={} expected={} readable={} complete={}",
        or_null(s.site.as_ref().map(|s| s.address.as_str())),
        yes_no(s.ufw_active),
        yes_no(s.deny_incoming),
        list(&s.ssh_ports),
        yes_no(s.maria_db_open),
        yes_no(s.sysctl_configured),
        or_null(s.whitelist.as_deref()),
        or_null(s.ip_header.as_deref()),
        yes_no(s.firewall_log_separate),
        or_null(s.trusted_proxies.as_deref()),
        yes_no(s.nginx_real_ip_cloudflare),
        or_null(s.expected_whitelist().as_deref()),
        yes_no(s.visitor_ips_readable()),
        yes_no(s.is_complete())
    )
}

fn describe_ssh(s: &SshLoginState) -> String {
    format!(
        "password={} keyboard={} permit={} ours={} saved={} keysonly={}",
        yes_no(s.password_login),
        yes_no(s.keyboard_interactive),
        or_null(s.permit_root_login.as_deref()),
        yes_no(s.our_file),
        yes_no(s.root_has_saved_key),
        yes_no(s.keys_only())
    )
}

fn failing(output: &'static str, error: &'static str) -> Responder {
    Some(Box::new(move |_: &str, _: &str, _: Option<&str>| CommandResult { exit_code: 1, output: output.into(), error: error.into() }))
}

/// Root commands get `root`, commands as the login user get `client`.
fn by_kind(root: String, client: &'static str) -> Responder {
    Some(Box::new(move |kind: &str, _: &str, _: Option<&str>| CommandResult::ok(if kind == "run" { client } else { &root })))
}

fn plain() -> WebAccessSettings {
    WebAccessSettings::new("www.hotel.example.com", true, false)
}

fn cloudflare() -> WebAccessSettings {
    WebAccessSettings::new("hotel.example.com", false, true)
}

const HARDEN_OUTPUT: &str = "conf_ADDRESS=www.hotel.example.com\nconf_SCHEME=https\nconf_CLOUDFLARE=yes\nactive=yes\ndeny=yes\n\
ssh=22 2222 abc 0 \nmariadb=\nsysctl=yes\nufwlog=yes\nrealipcf=yes\nwhitelist=hotel.example.com\nipheader=X-Forwarded-For\ntrusted=173.245.48.0/20\n";

#[tokio::test]
async fn harden_scenarios() {
    check("harden_inspect", answer(&[("sysctl=", HARDEN_OUTPUT)]), |s| async move {
        let state = harden::inspect(&*s).await?;
        result(&s, &describe_harden(&state));
        Ok(())
    })
    .await;
    check("harden_inspect_empty", None, |s| async move {
        let state = harden::inspect(&*s).await?;
        result(&s, &describe_harden(&state));
        Ok(())
    })
    .await;
    check(
        "harden_inspect_open",
        answer(&[(
            "sysctl=",
            "conf_ADDRESS=hotel.example.com\nconf_CLOUDFLARE=no\nactive=yes\ndeny=yes\nmariadb=open\nsysctl=yes\nufwlog=\nwhitelist=hotel.example.com\nipheader=X-Forwarded-For\n",
        )]),
        |s| async move {
            let state = harden::inspect(&*s).await?;
            result(&s, &describe_harden(&state));
            Ok(())
        },
    )
    .await;
    check("harden_configure_firewall", None, |s| async move { harden::configure_firewall(&*s, &plain(), &[2222, 22, 2222], &log).await }).await;
    check("harden_configure_firewall_cloudflare", None, |s| async move { harden::configure_firewall(&*s, &cloudflare(), &[], &log).await }).await;
    check("harden_enable_firewall_fresh", answer(&[("sysctl=", "conf_ADDRESS=hotel.example.com\nconf_CLOUDFLARE=yes\nssh=22 2222\n")]), |s| async move {
        let fresh = harden::enable_firewall(&*s, &log).await?;
        result(&s, yes_no(fresh));
        Ok(())
    })
    .await;
    check("harden_enable_firewall_no_site", answer(&[("marker=yes", "ufw=yes\nmarker=yes\nrules=0\n")]), |s| async move {
        let fresh = harden::enable_firewall(&*s, &log).await?;
        result(&s, yes_no(fresh));
        Ok(())
    })
    .await;
    check("harden_enable_firewall_existing", answer(&[("marker=yes", "ufw=yes\nmarker=yes\nrules=4\n"), ("sysctl=", "ssh=2222 22\n")]), |s| async move {
        let fresh = harden::enable_firewall(&*s, &log).await?;
        result(&s, yes_no(fresh));
        Ok(())
    })
    .await;
    check("harden_disable_firewall", None, |s| async move { harden::disable_firewall(&*s, &log).await }).await;
    check("harden_configure_sysctl", None, |s| async move { harden::configure_sysctl(&*s, &log).await }).await;
}

/// Needs web_access::polaris_trusted_proxies and emulator::set_settings from other groups.
#[tokio::test]
async fn harden_polaris() {
    check("harden_configure_polaris", None, |s| async move {
        let changed = harden::configure_polaris(&*s, &plain(), &log).await?;
        result(&s, yes_no(changed));
        Ok(())
    })
    .await;
    check("harden_configure_polaris_cloudflare", None, |s| async move {
        let changed = harden::configure_polaris(&*s, &cloudflare(), &log).await?;
        result(&s, yes_no(changed));
        Ok(())
    })
    .await;
}

#[tokio::test]
async fn ssh_login_scenarios() {
    check(
        "ssh_login_inspect",
        answer(&[(
            "sshd -T",
            "passwordauthentication=no\nkbdinteractiveauthentication=no\npermitrootlogin=prohibit-password\nours=yes\nkeys=AAAAX AAAAB3NzaC1yc2EAAAADAQAB\n",
        )]),
        |s| async move {
            let saved = saved_keys();
            let state = ssh_login::inspect(&*s, &saved).await?;
            result(&s, &describe_ssh(&state));
            Ok(())
        },
    )
    .await;
    check("ssh_login_inspect_empty", None, |s| async move {
        let saved = saved_keys();
        let state = ssh_login::inspect(&*s, &saved).await?;
        result(&s, &describe_ssh(&state));
        Ok(())
    })
    .await;
    check("ssh_login_disable", None, |s| async move { ssh_login::disable_password_login(&*s, &log).await }).await;
    check("ssh_login_allow", None, |s| async move { ssh_login::allow_password_login(&*s, &log).await }).await;
}

fn saved_keys() -> Vec<AuthorizedKey> {
    vec![AuthorizedKey::new("ssh-ed25519", "AAAAC3NzaC1lZDI1NTE5AAAAIB", "hotel"), AuthorizedKey::new("ssh-rsa", "AAAAB3NzaC1yc2EAAAADAQAB", "")]
}

fn key() -> AuthorizedKey {
    AuthorizedKey::new("ssh-ed25519", "AAAAC3NzaC1lZDI1NTE5AAAAIB", "it's me")
}

#[tokio::test]
async fn root_authorized_keys_scenarios() {
    check(
        "root_authorized_keys_read",
        answer(&[("cat", "# comment\nssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIB hotel\n\nssh-rsa AAAAB3NzaC1yc2EAAAADAQAB\n")]),
        |s| async move {
            let keys = root_authorized_keys::read(&*s).await?;
            result(&s, &keys.iter().map(AuthorizedKey::line).collect::<Vec<_>>().join(","));
            Ok(())
        },
    )
    .await;
    check("root_authorized_keys_read_failed", failing("", " cat: permission denied \n"), |s| async move { root_authorized_keys::read(&*s).await })
        .await;
    check(
        "root_authorized_keys_install_replace",
        answer(&[("set -e", " /root/.ssh/authorized_keys.bak-20260101-000000-abcd \n")]),
        |s| async move {
            let backup = root_authorized_keys::install(&*s, &key(), AuthorizedKeysMode::Replace).await?;
            result(&s, or_null(backup.as_deref()));
            Ok(())
        },
    )
    .await;
    check("root_authorized_keys_install_add", None, |s| async move {
        let backup = root_authorized_keys::install(&*s, &key(), AuthorizedKeysMode::Add).await?;
        result(&s, or_null(backup.as_deref()));
        Ok(())
    })
    .await;
    check("root_authorized_keys_install_failed", failing("", "\n  install: cannot create /root/.ssh \nmore\n"), |s| async move {
        root_authorized_keys::install(&*s, &key(), AuthorizedKeysMode::Add).await
    })
    .await;
    check("root_authorized_keys_install_failed_silent", failing("", " \n"), |s| async move {
        root_authorized_keys::install(&*s, &key(), AuthorizedKeysMode::Replace).await
    })
    .await;
    check("root_authorized_keys_permit_root_login", answer(&[("permitrootlogin", "without-password\n")]), |s| async move {
        let permit = root_authorized_keys::permit_root_login(&*s).await?;
        result(&s, or_null(permit.as_deref()));
        Ok(())
    })
    .await;
    check("root_authorized_keys_permit_root_login_unknown", None, |s| async move {
        let permit = root_authorized_keys::permit_root_login(&*s).await?;
        result(&s, or_null(permit.as_deref()));
        Ok(())
    })
    .await;
}

/// The inputs of fail2ban_entry_problems, as the golden file lists them (" => " separates the result).
fn golden_inputs(name: &str) -> Vec<String> {
    let text = std::fs::read_to_string(common::golden_dir().join(format!("{name}.txt"))).unwrap();
    text.lines().map(|l| l.split(" => ").next().unwrap().to_string()).collect()
}

#[tokio::test]
async fn fail2ban_texts() {
    let entries = golden_inputs("fail2ban_entry_problems");
    assert!(entries.len() > 100);
    let text: String = entries.iter().map(|e| format!("{e} => {}\n", or_null(fail2ban::entry_problem(e).as_deref()))).collect();
    compare("fail2ban_entry_problems", &text);

    let many: String = (0..101).map(|i| format!("10.0.{}.{}\n", i / 256, i % 256)).collect();
    let whitelists = [
        "203.0.113.5, 198.51.100.0/24 # office\r\n# a comment\n2001:db8::1\t203.0.113.5\n2001:DB8::1",
        "1.2.3.4 nope 5.6.7.8",
        "",
        "#1.2.3.4\n,,, \t",
        &many,
        "1.2.3.4/33 5.6.7.8",
    ];
    let text: String = whitelists
        .iter()
        .map(|w| {
            let (entries, problem) = fail2ban::parse_whitelist(w);
            format!("[{}] {}\n", list(&entries), or_null(problem.as_deref()))
        })
        .collect();
    compare("fail2ban_parse_whitelist", &text);

    let s = |max_retry, find_time_minutes, ban_time_minutes, increment| Fail2BanSettings {
        max_retry,
        find_time_minutes,
        ban_time_minutes,
        increment,
        whitelist: Vec::new(),
    };
    let settings = [
        fail2ban::defaults(),
        s(0, 10, 60, true),
        s(101, 10, 60, true),
        s(5, 0, 60, true),
        s(5, 10081, 60, true),
        s(5, 10080, 0, true),
        s(5, 10, 525601, true),
        s(100, 1, 525600, false),
    ];
    let text: String = settings.iter().map(|s| format!("{}\n", or_null(fail2ban::settings_problem(s).as_deref()))).collect();
    compare("fail2ban_settings_problems", &text);
}

fn f2b_settings() -> Fail2BanSettings {
    Fail2BanSettings {
        max_retry: 3,
        find_time_minutes: 15,
        ban_time_minutes: 120,
        increment: true,
        whitelist: vec!["203.0.113.5".into(), "198.51.100.0/24".into(), "2001:db8::1".into()],
    }
}

#[tokio::test]
async fn fail2ban_scenarios() {
    let inspections: [(&str, &str, &str); 4] = [
        (
            "fail2ban_inspect",
            "version=1.0.2\ninstalled=yes\nrunning=yes\nconfigured=yes\nignoreip=127.0.0.1/8 ::1 203.0.113.5 198.51.100.0/24\nmaxretry=3\n\
findtime=600\nbantime=2h\nincrement=false\nssh=22 2222 \nbanned=198.51.100.1 198.51.100.2\ntotal=7\n",
            "client=203.0.113.9\n",
        ),
        ("fail2ban_inspect_units", "installed=yes\nconfigured=yes\nmaxretry=abc\nfindtime=1w\nbantime=30\nincrement=true\n", "client=localhost\n"),
        ("fail2ban_inspect_bad_units", "installed=yes\nconfigured=yes\nmaxretry=-2\nfindtime=1y\nbantime=m\n", "client=\n"),
        ("fail2ban_inspect_days", "configured=yes\nfindtime=2D\nbantime= 10m \n", "client=2001:db8::5\n"),
    ];
    for (name, root, client) in inspections {
        check(name, by_kind(root.to_string(), client), |s| async move {
            let state = fail2ban::inspect(&*s).await?;
            result(&s, &describe_fail2ban(&state));
            Ok(())
        })
        .await;
    }
    check("fail2ban_inspect_empty", None, |s| async move {
        let state = fail2ban::inspect(&*s).await?;
        result(&s, &describe_fail2ban(&state));
        Ok(())
    })
    .await;
    check("fail2ban_install", None, |s| async move { fail2ban::install(&*s, &f2b_settings(), &[22, 2222], &log).await }).await;
    check("fail2ban_install_defaults", None, |s| async move {
        fail2ban::install(&*s, &Fail2BanSettings { increment: false, ..fail2ban::defaults() }, &[], &log).await
    })
    .await;
    check("fail2ban_save", None, |s| async move { fail2ban::save(&*s, &f2b_settings(), &[2222], &log).await }).await;
    check("fail2ban_save_empty", None, |s| async move { fail2ban::save(&*s, &fail2ban::defaults(), &[22], &log).await }).await;
    check("fail2ban_unban", None, |s| async move { fail2ban::unban(&*s, "203.0.113.5", &log).await }).await;
    check("fail2ban_unban_network", None, |s| async move { fail2ban::unban(&*s, "198.51.100.0/24", &log).await }).await;
    check("fail2ban_unban_invalid", None, |s| async move { fail2ban::unban(&*s, "1.2.3", &log).await }).await;
    check("fail2ban_remove", None, |s| async move { fail2ban::remove(&*s, &log).await }).await;
}

const CROWDSEC_OUTPUT: &str = "conf_ADDRESS=hotel.example.com\nconf_SCHEME=https\ndocker=yes\ntraefik=true\ncs_LAPI_PORT=8081\ncs_ENROLLED=yes\n\
cs_INSTANCE=hotel\ncs_WHITELIST=203.0.113.5 198.51.100.0/24\ninstalled=yes\nbouncer=yes\nrunning=true\nversion=v1.6.4\n\
banned=192.0.2.1 192.0.2.2\ndns=104.21.1.1\ndnscf=yes\n";

fn cs_settings() -> CrowdSecSettings {
    CrowdSecSettings {
        enroll_key: "cm1abcdefghijklmnop".into(),
        instance_name: "my-hotel".into(),
        whitelist: vec!["203.0.113.5".into(), "198.51.100.0/24".into()],
    }
}

#[tokio::test]
async fn crowdsec_texts() {
    let a64 = "a".repeat(64);
    let a65 = "a".repeat(65);
    let keys = [
        "sudo cscli console enroll -e context cm1abcdefghijklmnop",
        "  'cm1abcdefghijklmnop'\r\n",
        "\"cm1abcdefghijklmnop\"",
        "short",
        "-e",
        "",
        "  ",
        "cm1abcdefghijklmnop --name x",
        "cm1abc-defghijklmnop",
        &a64,
        &a65,
        "cm1abcdefghijklmnop -e",
    ];
    let text: String = keys
        .iter()
        .map(|t| {
            format!(
                "[{t}] key={} enrolled={} new={}\n",
                or_null(crowdsec::key_from(t).as_deref()),
                or_null(crowdsec::key_problem(t, true).as_deref()),
                or_null(crowdsec::key_problem(t, false).as_deref())
            )
        })
        .collect();
    compare("crowdsec_keys", &text.replace("\r\n", "\n"));
    let names = ["my-hotel_1.eu", "hotel\n", "", "my hotel", &a64, &a65, "hötel", "a\n\n"];
    let text: String = names.iter().map(|n| format!("[{n}] {}\n", or_null(crowdsec::name_problem(n).as_deref()))).collect();
    compare("crowdsec_names", &text);
}

#[tokio::test]
async fn crowdsec_scenarios() {
    let inspections: [(&str, String, &str); 4] = [
        ("crowdsec_inspect", CROWDSEC_OUTPUT.to_string(), "client=203.0.113.9\n"),
        ("crowdsec_inspect_cloudflare", CROWDSEC_OUTPUT.replace("conf_SCHEME=https", "conf_CLOUDFLARE=yes"), "client=nope\n"),
        ("crowdsec_inspect_no_docker", "conf_ADDRESS=hotel.example.com\ntraefik=false\n".to_string(), ""),
        ("crowdsec_inspect_no_traefik", "conf_ADDRESS=hotel.example.com\ndocker=yes\ntraefik=false\ndnscf=no\n".to_string(), ""),
    ];
    for (name, root, client) in inspections {
        check(name, by_kind(root, client), |s| async move {
            let state = crowdsec::inspect(&*s).await?;
            result(&s, &describe_crowdsec(&state));
            Ok(())
        })
        .await;
    }
    check("crowdsec_inspect_empty", None, |s| async move {
        let state = crowdsec::inspect(&*s).await?;
        result(&s, &describe_crowdsec(&state));
        Ok(())
    })
    .await;
    check("crowdsec_install_engine", None, |s| async move { crowdsec::install_engine(&*s, &cs_settings(), &log).await }).await;
    check("crowdsec_install_engine_empty", None, |s| async move {
        crowdsec::install_engine(&*s, &CrowdSecSettings { whitelist: Vec::new(), ..cs_settings() }, &log).await
    })
    .await;
    check("crowdsec_enroll", None, |s| async move {
        crowdsec::enroll(&*s, "sudo cscli console enroll -e context cm1abcdefghijklmnop", "my-hotel", &log).await
    })
    .await;
    check("crowdsec_enroll_invalid", None, |s| async move { crowdsec::enroll(&*s, "nope", "my-hotel", &log).await }).await;
    check("crowdsec_test", answer(&[("probe", "  fetched\nBOUNCER=yes\nLOGREAD=no\n")]), |s| async move {
        let r = crowdsec::test(&*s, &plain(), &log).await?;
        result(&s, &format!("{} {}", yes_no(r.bouncer_connected), yes_no(r.log_read)));
        Ok(())
    })
    .await;
    check("crowdsec_test_http", answer(&[("probe", "LOGREAD=yes\n")]), |s| async move {
        let r = crowdsec::test(&*s, &cloudflare(), &log).await?;
        result(&s, &format!("{} {}", yes_no(r.bouncer_connected), yes_no(r.log_read)));
        Ok(())
    })
    .await;
    check("crowdsec_save_whitelist", None, |s| async move { crowdsec::save_whitelist(&*s, &plain(), 8081, &cs_settings().whitelist, &log).await })
        .await;
    check("crowdsec_save_whitelist_cloudflare", None, |s| async move { crowdsec::save_whitelist(&*s, &cloudflare(), 8080, &[], &log).await }).await;
    check("crowdsec_unban", None, |s| async move { crowdsec::unban(&*s, "192.0.2.1", &log).await }).await;
    check("crowdsec_unban_range", None, |s| async move { crowdsec::unban(&*s, "192.0.2.0/24", &log).await }).await;
    check("crowdsec_unban_invalid", None, |s| async move { crowdsec::unban(&*s, "1.2.3", &log).await }).await;
    check("crowdsec_remove_without_site", None, |s| async move { crowdsec::remove(&*s, None, true, &log).await }).await;
}

/// Needs web_access (static_config_script, compose_script, wait_for_traefik_script) and server_tools::update_image
/// from other groups.
#[tokio::test]
async fn crowdsec_with_other_groups() {
    check("crowdsec_configure_traefik", None, |s| async move { crowdsec::configure_traefik(&*s, &plain(), &cs_settings().whitelist, &log).await })
        .await;
    check("crowdsec_configure_traefik_cloudflare", None, |s| async move { crowdsec::configure_traefik(&*s, &cloudflare(), &[], &log).await }).await;
    check("crowdsec_update", None, |s| async move { crowdsec::update(&*s, &log).await }).await;
    check("crowdsec_remove", None, |s| async move { crowdsec::remove(&*s, Some(&plain()), false, &log).await }).await;
}

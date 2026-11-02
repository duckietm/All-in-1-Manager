//! Group F: server_tools, plus_import, server_status, emulator_monitor, server_users against
//! tools/parity/Scenarios_F.cs.

mod common;

use aio_core::InstallerSettings;
use aio_remote::emulator_monitor;
use aio_remote::plus_import::{self, PlusImportMode};
use aio_remote::recorder::Recorder;
use aio_remote::{server_status, server_tools, server_users, CommandResult};
use common::{check, compare, log};

type Responder = Option<Box<dyn Fn(&str, &str, Option<&str>) -> CommandResult + Send + Sync>>;

/// Answers every command with this exit code, output and error (the harness' ReplyF).
fn reply(exit_code: i32, output: &'static str, error: &'static str) -> Responder {
    Some(Box::new(move |_: &str, _: &str, _: Option<&str>| CommandResult { exit_code, output: output.into(), error: error.into() }))
}

fn result(s: &Recorder, text: &str) {
    s.note(&format!("result: {text}"));
}

fn n(value: Option<impl ToString>) -> String {
    value.map(|v| v.to_string()).unwrap_or_else(|| "-".into())
}

fn b(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

const STATUS_OUTPUT: &str = "cpu cpu  4705 150 1120 16250 520 0 25 0 0 0\ncores 4\nmem MemTotal: 8148108\nmem MemAvailable: 5023412\n\
mem SwapTotal: 2097148\nmem SwapFree: 1048576\nnet 123456789 98765432\nload 0.42 0.31 0.25\nuptime 864123.57\n\
disk / /dev/sda1 81032015872 21474836480 55985274880\ndisk /boot/efi /dev/sda15 109422592 6291456 103131136\n\
user root pts/0 2026-10-31_09:12 203.0.113.7\nuser simone tty1 2026-10-30_18:00\nplayers 12\nemulator yes\n";

const MONITOR_OUTPUT: &str = "clk 100\ncores 4\nmachine 2345678\npid 4242\nproc 81234 1500000\nrss 1610612736\nxmx 2 g\nboot 20000\n\
t 4242 S 12 java\nt 4250 R 5400 WiredTickCoordi\nt 4251 S 2100 WiredTickShard-\nt 4260 S 700 C2 CompilerThre\n";

const WIRED_OUTPUT: &str = "room 12 3 85 190 2026-10-31T09:12:01 0,2\nroom 7 5 40 160 2026-10-31T09:10:44 1\n\
room 99 1 300 300 2026-10-31T09:13:00\ntotals 9 2\n\
recent 2026-10-31T09:13:00 WARN Slow wired task: shard=1, room=99, took=300ms\n\
recent 2026-10-31T09:13:05 WARN Room 99 has 64 wired tasks waiting\nname 12\tThe Casino\nname 7\tWired lab\n";

const PLUS_OUTPUT: &str = "> Safety backup of habbo first\nSAFETY=/var/backups/all-in-1/habbo-before-plus-import-1.sql.gz\n\
FURNI_EXISTING=5120\nFURNI_NEW=312\nCRACKABLE_WITHOUT_PRIZES=4\nPAGES=180\nCATALOG_ITEMS=7400\n";

#[tokio::test]
async fn server_tools_restarts() {
    check("server_tools_restart_emulator", None, |s| async move { server_tools::restart_emulator(&*s, &log).await }).await;
    check("server_tools_restart_mariadb", None, |s| async move { server_tools::restart_mariadb(&*s, &log).await }).await;
    check(
        "server_tools_restart_mariadb_failed",
        reply(1, "> Restarting MariaDB\nERROR: MariaDB does not answer after the restart (journalctl -u mariadb).\n", ""),
        |s| async move { server_tools::restart_mariadb(&*s, &log).await },
    )
    .await;
    check("server_tools_restart_nginx", None, |s| async move { server_tools::restart_nginx(&*s, &log).await }).await;
    check("server_tools_restart_traefik", None, |s| async move { server_tools::restart_traefik(&*s, &log).await }).await;
    check("server_tools_update_traefik", None, |s| async move { server_tools::update_traefik(&*s, &log).await }).await;
}

#[tokio::test]
async fn server_tools_database() {
    check(
        "server_tools_backup_database",
        common::answer(&[(
            "mariadb-dump",
            "> mariadb-dump habbo (120 tables)\n  4.2M in 3 seconds\nFILE=/var/backups/all-in-1/habbo-20261031-120000.sql.gz\nDB=habbo\nBYTES=4404019\n",
        )]),
        |s| async move {
            let backup = server_tools::backup_database(&*s, &log).await?;
            result(&s, &format!("{}|{}|{}", backup.remote_path, backup.database, backup.bytes));
            Ok(())
        },
    )
    .await;
    check(
        "server_tools_backup_database_failed",
        reply(1, "ERROR: Database habbo does not exist or MariaDB does not run.\n", ""),
        |s| async move { server_tools::backup_database(&*s, &log).await },
    )
    .await;
    check("server_tools_import_database", None, |s| async move {
        server_tools::import_database(&*s, "/var/backups/all-in-1/import-20261031-120000.sql.gz", &log).await
    })
    .await;
    check("server_tools_import_database_quote", None, |s| async move {
        server_tools::import_database(&*s, "/tmp/it's a dump.sql", &log).await
    })
    .await;
}

#[tokio::test]
async fn server_tools_updates() {
    check("server_tools_update_system", None, |s| async move {
        let reboot = server_tools::update_system(&*s, &log).await?;
        result(&s, b(reboot));
        Ok(())
    })
    .await;
    check("server_tools_update_system_reboot", reply(0, "> apt-get update\n  A reboot is needed\nREBOOT=yes\n", ""), |s| async move {
        let reboot = server_tools::update_system(&*s, &log).await?;
        result(&s, b(reboot));
        Ok(())
    })
    .await;
    check(
        "server_tools_emulator_version",
        reply(0, "commit=a1b2c3d\nversion=Polaris-4.2.0\njava=/usr/lib/jvm/java-25-openjdk-amd64\n", ""),
        |s| async move {
            let (commit, version) = server_tools::emulator_version(&*s).await?;
            result(&s, &format!("{}|{}", n(commit), n(version)));
            Ok(())
        },
    )
    .await;
    check("server_tools_emulator_version_none", None, |s| async move {
        let (commit, version) = server_tools::emulator_version(&*s).await?;
        result(&s, &format!("{}|{}", n(commit), n(version)));
        Ok(())
    })
    .await;
    check("server_tools_java_home", reply(0, "/usr/lib/jvm/java-25-openjdk-amd64\n", ""), |s| async move {
        let java = server_tools::java_home(&*s).await?;
        result(&s, &java);
        Ok(())
    })
    .await;
    check("server_tools_java_home_missing", None, |s| async move {
        let java = server_tools::java_home(&*s).await?;
        result(&s, &java);
        Ok(())
    })
    .await;
    check("server_tools_fix_emulator_owner", None, |s| async move { server_tools::fix_emulator_owner(&*s, &log).await }).await;
}

#[tokio::test]
async fn server_tools_debug_sources() {
    let text: String = server_tools::debug_log_sources()
        .iter()
        .map(|s| format!("### {}\n{}\n{}\n{}\n", s.name, s.description, s.follows, s.follow_script()))
        .collect();
    compare("server_tools_debug_sources", &text);
}

#[tokio::test]
async fn plus_import_scenarios() {
    fn line(r: &plus_import::PlusImportResult) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            r.furniture_existing, r.furniture_new, r.crackables_without_prizes, r.pages, r.catalog_items, r.safety_backup
        )
    }
    check("plus_import_furniture_only", reply(0, PLUS_OUTPUT, ""), |s| async move {
        let r = plus_import::import(&*s, "/var/backups/all-in-1/plus-import-1.sql.gz", PlusImportMode::FurnitureOnly, &log).await?;
        result(&s, &line(&r));
        Ok(())
    })
    .await;
    check("plus_import_furniture_and_catalog", None, |s| async move {
        let r = plus_import::import(&*s, "/var/backups/all-in-1/plus-import-1.sql", PlusImportMode::FurnitureAndCatalog, &log).await?;
        result(&s, &line(&r));
        Ok(())
    })
    .await;
    check(
        "plus_import_failed",
        reply(1, "ERROR: x.sql is not a PlusEMU dump: it has no furniture table (Polaris and Arcturus call it items_base).\n", ""),
        |s| async move { plus_import::import(&*s, "/tmp/x.sql", PlusImportMode::FurnitureOnly, &log).await },
    )
    .await;
}

#[tokio::test]
async fn server_status_scenarios() {
    check("server_status_sample", reply(0, STATUS_OUTPUT, ""), |s| async move {
        let x = server_status::sample(&*s).await?;
        result(
            &s,
            &format!(
                "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
                x.cpu_total,
                x.cpu_idle,
                x.cores,
                x.mem_total,
                x.mem_available,
                x.swap_total,
                x.swap_free,
                x.received_bytes,
                x.sent_bytes,
                x.load,
                x.uptime_seconds,
                n(x.players_online),
                b(x.emulator_running)
            ),
        );
        for d in &x.disks {
            result(&s, &format!("disk {}|{}|{}|{}|{}", d.mount, d.device, d.total, d.used, d.available));
        }
        for u in &x.users {
            result(&s, &format!("user {}|{}|{}|{}", u.user, u.terminal, u.since, u.from));
        }
        Ok(())
    })
    .await;
    check(
        "server_status_versions",
        reply(0, "repo0 main|a1b2c3d|2026-10-30 21:15|Fix wired|with a pipe\nrepo1 dev|e4f5a6b|2026-10-29 10:00|Octane update\nrepo3 \njar Polaris-4.2.0\noctane 1.6.3\n", ""),
        |s| async move {
            for v in server_status::versions(&*s).await? {
                result(&s, &format!("{}|{}|{}|{}|{}|{}|{}", v.name, v.directory, n(v.branch), n(v.commit), n(v.date), n(v.subject), n(v.build)));
            }
            Ok(())
        },
    )
    .await;
    // The harness has no settings file, so every clone follows main.
    check(
        "server_status_updates",
        reply(0, "repo0 main|Main|3|9f8e7d6|2026-10-31 08:00|Newest commit\nrepo1 dev||0|\nrepo2 HEAD|main|x|\n", ""),
        |s| async move {
            for u in server_status::updates_with(&*s, &InstallerSettings::default()).await? {
                result(
                    &s,
                    &format!(
                        "{}|{}|{}|{}|{}|{}|{}|{}",
                        u.name,
                        u.directory,
                        n(u.branch.clone()),
                        n(u.target.clone()),
                        u.behind,
                        n(u.newest.clone()),
                        b(u.switches_branch()),
                        n(u.counted())
                    ),
                );
            }
            Ok(())
        },
    )
    .await;
    check(
        "server_status_images",
        reply(0, "image0 true|3.5.3|yes||3.5.4|\nimage2 false|1.6.3||||Docker Hub did not answer\n", ""),
        |s| async move {
            for i in server_status::images(&*s).await? {
                result(
                    &s,
                    &format!(
                        "{}|{}|{}|{}|{}|{}|{}|{}|{}",
                        i.name,
                        i.container,
                        i.image,
                        n(i.version),
                        b(i.running),
                        b(i.newer_on_hub),
                        b(i.not_restarted),
                        n(i.newest_version),
                        n(i.problem)
                    ),
                );
            }
            Ok(())
        },
    )
    .await;
}

#[tokio::test]
async fn emulator_monitor_scenarios() {
    check("emulator_monitor_sample", reply(0, MONITOR_OUTPUT, ""), |s| async move {
        let x = emulator_monitor::sample(&*s).await?;
        result(
            &s,
            &format!(
                "{}|{}|{}|{}|{}|{}|{}|{}",
                x.pid, x.ticks_per_second, x.cores, x.machine_ticks, x.process_ticks, x.rss_bytes, x.max_heap_bytes, x.started_seconds_ago
            ),
        );
        for t in &x.threads {
            result(&s, &format!("thread {}|{}|{}|{}", t.id, t.name, t.state, t.ticks));
        }
        Ok(())
    })
    .await;
    check("emulator_monitor_thread_names", reply(0, "4250 WiredTickCoordinator\n4251 WiredTickShard-0 \nx bad\n", ""), |s| async move {
        let mut names: Vec<(i32, String)> = emulator_monitor::thread_names(&*s, 4242).await?.into_iter().collect();
        names.sort();
        for (id, name) in names {
            result(&s, &format!("{id}|{name}"));
        }
        Ok(())
    })
    .await;
    check("emulator_monitor_wired_log", reply(0, WIRED_OUTPUT, ""), |s| async move {
        let w = emulator_monitor::wired_log(&*s).await?;
        result(&s, &format!("{}|{}", w.slow_tasks, w.dropped_warnings));
        for r in &w.rooms {
            result(
                &s,
                &format!("room {}|{}|{}|{}|{}|{}|{}", r.room_id, n(r.name.clone()), r.count, r.slowest_ms, r.total_ms, r.last_seen, r.shards),
            );
        }
        for line in &w.recent {
            result(&s, &format!("recent {line}"));
        }
        Ok(())
    })
    .await;
}

#[tokio::test]
async fn server_users_scenarios() {
    check(
        "server_users_list",
        reply(
            0,
            "www=emulator,simone\nuser=root:0:/root:/bin/bash\nuser=daemon:1:/usr/sbin:/usr/sbin/nologin\n\
user=www-data:33:/var/www:/usr/sbin/nologin\nuser=emulator:998:/var/lib/emulator:/usr/sbin/nologin\n\
user=zoe:1001:/home/zoe:/bin/bash\nuser=simone:1000:/home/simone:/bin/bash\nuser=nobody:65534:/nonexistent:/usr/sbin/nologin\n",
            "",
        ),
        |s| async move {
            for u in server_users::list(&*s).await? {
                result(
                    &s,
                    &format!("{}|{}|{}|{}|{}|{}|{}", u.name, u.uid, u.home, u.shell, b(u.in_www_data), b(u.is_system()), b(u.can_log_in())),
                );
            }
            Ok(())
        },
    )
    .await;
    check("server_users_list_failed", reply(1, "", " getent: permission denied \n"), |s| async move { server_users::list(&*s).await }).await;
    check("server_users_create", None, |s| async move { server_users::create(&*s, "hotel").await }).await;
    check("server_users_create_root", None, |s| async move { server_users::create(&*s, "root").await }).await;
    check("server_users_create_invalid", None, |s| async move { server_users::create(&*s, "Bad Name").await }).await;
    check("server_users_create_failed", reply(1, "useradd: cannot lock /etc/passwd\n", ""), |s| async move {
        server_users::create(&*s, "hotel").await
    })
    .await;
}

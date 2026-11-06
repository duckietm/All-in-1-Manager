//! A CMS' own NGINX site on the first free port from 85 to 95. (CmsSite.cs)
//!
//! PORTING: the items above the line are used by other modules; keep their names and signatures.

pub fn file(name: &str) -> String {
    format!("/etc/nginx/sites-available/cms-{name}")
}

// ---------------------------------------------------------------------------------------------------------------
//
// A CMS' own NGINX site (AtomCMS, EpicNext or your own CMS) on 127.0.0.1, on the first free port from 85 to 95. Traefik
// sends it the hotel's address except /client, /gamedata and /custom (the hotel site on port 85) and Polaris' paths.

use crate::hotel_site;
use crate::script::{self, fill};
use crate::server::{shell_quote, Log, Result, Server};
use crate::web_access::WebAccessSettings;

pub const FIRST_PORT: u16 = crate::local_ports::FIRST;
pub const LAST_PORT: u16 = crate::local_ports::LAST;

// A static site serves its files; an address without a file is a 404.
const STATIC_LOCATIONS: &str = r#"    location / {
        try_files $uri $uri/ =404;
    }

    location ~ /\.(git|ht|env) {
        deny all;
    }"#;

// A CMS with a server of its own (EpicNext's Next.js): its build's static files and its public assets come straight
// from disk (no trip through Node, cached by browsers), everything else goes to it over kept-alive connections,
// websockets and Server-Sent Events included.
const PROXY_DIRECTIVES: &str = r#"        proxy_pass http://{{upstream}};
        proxy_http_version 1.1;
        proxy_set_header Host              $host;
        proxy_set_header X-Real-IP         $remote_addr;
        # The visitor, as real_ip found it behind Traefik (and Cloudflare). The CMS also reads
        # Cloudflare's headers when present, and anyone can send those: they are
        # cleared, so the address cannot be faked (to dodge limits, or to get another IP banned).
        proxy_set_header X-Forwarded-For   $remote_addr;
        proxy_set_header CF-Connecting-IP  "";
        proxy_set_header CF-Ray            "";
        proxy_set_header CDN-Loop          "";
        proxy_set_header True-Client-IP    "";
        proxy_set_header X-Real-Client-IP  "";
        proxy_set_header X-Forwarded-Proto $http_x_forwarded_proto;
        proxy_set_header X-Forwarded-Host  $host;
        proxy_set_header Upgrade           $http_upgrade;
        proxy_set_header Connection        $cms_{{name}}_connection;
        # Buffered, so a slow visitor does not hold a Node connection; live streams send
        # "X-Accel-Buffering: no" and are passed on as they come.
        proxy_read_timeout 3600s;"#;

// Next.js' build output: file names carry a content hash, so browsers keep them for a year.
const PROXY_LOCATIONS: &str = r#"    client_max_body_size 20m;

    gzip            on;
    gzip_vary       on;
    gzip_comp_level 5;
    gzip_min_length 1024;
    gzip_types      text/plain text/css application/javascript application/json image/svg+xml font/ttf font/otf;

    open_file_cache          max=10000 inactive=60s;
    open_file_cache_valid    30s;
    open_file_cache_errors   on;

    location ^~ /_next/static/ {
        alias {{root}}/.next/static/;
        access_log off;
        add_header Cache-Control "public, max-age=31536000, immutable" always;
        add_header X-Content-Type-Options "nosniff" always;
    }

    # Its public assets, with the cache times of its next.config.ts; anything not on disk goes to the CMS.
    location ^~ /assets/ {
        root {{root}}/public;
        access_log off;
        add_header Cache-Control "public, max-age=31536000, immutable" always;
        add_header X-Content-Type-Options "nosniff" always;
        try_files $uri @cms;
    }
    location ~ ^/(swf|nitro-assets)/ {
        root {{root}}/public;
        access_log off;
        add_header Cache-Control "public, max-age=604800, stale-while-revalidate=2592000" always;
        add_header X-Content-Type-Options "nosniff" always;
        try_files $uri @cms;
    }

    location / {
{{proxyDirectives}}
    }

    location @cms {
{{proxyDirectives}}
    }"#;

const PROXY_HTTP: &str = r#"upstream {{upstream}} {
    server {{server}};
    keepalive 16;
}

# Keep-alive to the CMS, except for a websocket upgrade.
map $http_upgrade $cms_{{name}}_connection {
    default upgrade;
    ""      "";
}
"#;

const CONFIG: &str = r#"# {{name}}: the hotel's website (written by All-in-1 Manager, Addons → CMS). {{what}}.
# Traefik (/opt/traefik) passes {{Url}} here, except /client, /gamedata and /custom (the hotel site on
# 127.0.0.1:{{HotelPort}}) and Polaris' /ws and /api paths.

{{proxyHttp}}server {
    listen 127.0.0.1:@PORT@;
    server_name {{serverName}};{{files}}

    autoindex     off;
    server_tokens off;

    # The visitor's address from X-Forwarded-For, past Traefik (and Cloudflare), as for the hotel site.
    include {{RealIpFile}};
    real_ip_header    X-Forwarded-For;
    real_ip_recursive on;

    add_header X-Content-Type-Options "nosniff" always;
{{hsts}}    add_header Referrer-Policy        "strict-origin";

    # Next to the hotel site's, so Debug → NGINX shows them too.
    access_log {{LogDirectory}}/access.log;
    error_log  {{LogDirectory}}/error.log;

{{locations}}
}"#;

fn config(site: &WebAccessSettings, name: &str, root: &str, php: bool, proxy: Option<&str>) -> String {
    let hsts = if site.https { "    add_header Strict-Transport-Security \"max-age=2592000\" always;\n" } else { "" };
    let upstream = format!("cms_{name}_app");
    let locations = match proxy {
        Some(_) => {
            let directives = fill(PROXY_DIRECTIVES, &[("upstream", &upstream), ("name", name)]);
            fill(&fill(PROXY_LOCATIONS, &[("root", root)]), &[("proxyDirectives", &directives)])
        }
        None if php => hotel_site::php_locations(),
        None => STATIC_LOCATIONS.to_string(),
    };
    let proxy_http = match proxy {
        Some(proxy) => fill(PROXY_HTTP, &[("upstream", &upstream), ("server", &proxy.replace("http://", "")), ("name", name)]),
        None => String::new(),
    };
    let what = match proxy {
        Some(proxy) => format!("Passed to {proxy}"),
        None if php => format!("PHP 8.5 from {root}"),
        None => format!("A static website from {root}"),
    };
    // A blank line after server_name, then root and index.
    let files = match proxy {
        Some(_) => String::new(),
        None => format!("\n\n    root {root};\n    index {};", if php { "index.php index.html" } else { "index.html index.htm" }),
    };
    let text = fill(
        CONFIG,
        &[
            ("Url", &site.url()),
            ("HotelPort", hotel_site::PORT),
            ("serverName", &hotel_site::server_name(site)),
            ("RealIpFile", hotel_site::REAL_IP_FILE),
            ("LogDirectory", hotel_site::LOG_DIRECTORY),
            ("hsts", hsts),
            ("what", &what),
            ("name", name),
        ],
    );
    fill(&text, &[("proxyHttp", &proxy_http), ("files", &files), ("locations", &locations)])
}

const WRITE: &str = r#"[ -d /etc/nginx/sites-available ] || fail "NGINX is not installed with sites-available. Run the NGINX step first."
f={{File}}
{{Functions}}
# The port this CMS had, when it is still free for it; otherwise the first free one.
self_file=$f
pick_port "$(grep -sE '^[[:space:]]*listen[[:space:]]+127\.0\.0\.1:[0-9]+' "$f" | head -1 | grep -oE '[0-9]+;' | tr -d ';')"
install -d -m 755 {{LogDirectory}}
# The proxies the visitor's address is read past: servers set up before this file existed do not have it yet.
{{RealIpScript}}
echo "> $f: listen 127.0.0.1:$port, {{what}}"
cat > "$f" <<'SITE'
{{Config}}
SITE
sed -i "s#@PORT@#$port#" "$f"
enabled=/etc/nginx/sites-enabled/cms-{{name}}
ln -sfn "$f" "$enabled"
nginx -t 2>&1 | sed 's/^/  /'
if ! nginx -t >/dev/null 2>&1; then
  # Never leave NGINX with a configuration it refuses: its next restart, or a reboot, would fail.
  rm -f "$enabled"
  echo "  cms-{{name}} is disabled again, so NGINX keeps a configuration it accepts"
  fail "nginx -t refuses the configuration (see above). NGINX was not reloaded: the hotel keeps running as it was."
fi
# One CMS is the website at a time: the others go off once this one is accepted.
for other in /etc/nginx/sites-enabled/cms-*; do
  [ -e "$other" ] || [ -L "$other" ] || continue
  [ "$other" = "$enabled" ] && continue
  rm -f "$other"
  echo "  $(basename "$other") is no longer the website (disabled; its files and sites-available/$(basename "$other") stay)"
done
# EpicNext runs a server of its own: off with its site.
if [ {{name}} != {{EpicNextSite}} ] && [ -d /run/systemd/system ] && systemctl is-enabled --quiet {{EpicNextService}} 2>/dev/null; then
  systemctl disable --now {{EpicNextService}} >/dev/null 2>&1 || true
  echo "  {{EpicNextService}} stopped and off at boot (its files stay)"
fi
if [ -d /run/systemd/system ]; then systemctl reload nginx || systemctl restart nginx; else nginx -s reload 2>/dev/null || service nginx restart; fi
sleep 1
code=$(curl -s --noproxy '*' -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/" || true)
echo "  http://127.0.0.1:$port/ answers HTTP $code"
[ -n "$code" ] && [ "$code" != 000 ] || fail "NGINX does not answer on 127.0.0.1:$port."
echo "PORT=$port""#;

/// Writes and enables the site, on the port it had or the first free one from 85 to 95, and disables the site of
/// another CMS (its files stay). Prints PORT=. Does not change Traefik. (WriteScript)
pub fn write_script(site: &WebAccessSettings, name: &str, root: &str, php: bool, proxy: Option<&str>) -> String {
    let what = match proxy {
        Some(proxy) => format!("passed to {proxy}"),
        None => format!("root {root} ({})", if php { "PHP" } else { "static" }),
    };
    let text = fill(
        WRITE,
        &[
            ("File", &file(name)),
            ("LogDirectory", hotel_site::LOG_DIRECTORY),
            ("what", &what),
            ("name", name),
            ("EpicNextSite", crate::epicnext::SITE_NAME),
            ("EpicNextService", crate::epicnext::SERVICE),
        ],
    );
    fill(
        &text,
        &[
            ("Functions", &crate::local_ports::functions()),
            ("RealIpScript", &hotel_site::real_ip_script(site)),
            ("Config", &config(site, name, root, php, proxy)),
        ],
    )
}

const REMOVE: &str = r#"off=0
for s in /etc/nginx/sites-enabled/cms-*; do
  [ -e "$s" ] || [ -L "$s" ] || continue
  rm -f "$s"
  off=$((off + 1))
  echo "> $(basename "$s") disabled: NGINX no longer serves it"
done
[ $off -gt 0 ] || echo "  No CMS site was enabled"
{{delete}}
nginx -t 2>&1 | sed 's/^/  /'
nginx -t >/dev/null 2>&1 || fail "nginx -t refuses the configuration (see above)."
if [ -d /run/systemd/system ]; then systemctl reload nginx || systemctl restart nginx; else nginx -s reload 2>/dev/null || service nginx restart; fi
echo "  NGINX reloaded""#;

const DELETE: &str = r#"d={{folder}}
case "$d" in /var/www/?*) ;; *) fail "Refusing to delete $d: not a folder in /var/www." ;; esac
if [ -d "$d" ]; then
  echo "> Deleting $d ($(du -sh "$d" | cut -f1))"
  rm -rf -- "$d"
fi
rm -f {{File}}
echo "  {{File}} deleted""#;

/// Disables every CMS site (the hotel site then is the only one), and with `delete_folder` deletes that CMS' folder
/// and NGINX site file. Reloads NGINX once nginx -t accepts it. (RemoveAsync)
pub async fn remove(server: &dyn Server, site_name: Option<&str>, delete_folder: Option<&str>, log: Log<'_>) -> Result<()> {
    let delete = match (delete_folder, site_name) {
        (Some(folder), Some(name)) => fill(DELETE, &[("File", &file(name)), ("folder", &shell_quote(folder))]),
        _ => String::new(),
    };
    let body = fill(REMOVE, &[("delete", &delete)]);
    script::run(server, &script::with_prelude(&body), log, "Removing the CMS site failed", None).await
}

/// Runs [`write_script`]; returns the port. (ApplyAsync)
pub async fn apply(server: &dyn Server, site: &WebAccessSettings, name: &str, root: &str, php: bool, log: Log<'_>, proxy: Option<&str>) -> Result<i64> {
    let values = script::run_capturing(
        server,
        &script::with_prelude(&write_script(site, name, root, php, proxy)),
        log,
        &format!("Setting up the NGINX site for {name} failed"),
        &["PORT"],
        None,
    )
    .await?;
    match values.int("PORT") {
        port if port > 0 => Ok(port),
        _ => crate::fail!("The NGINX site for {name} has no port."),
    }
}

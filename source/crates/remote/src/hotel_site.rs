//! The hotel's NGINX site: Octane on /, or a CMS with Octane on /client/. (HotelSite.cs)
//!
//! PORTING: the items above the line are used by other modules; keep their names and signatures.

use crate::web_access::WebAccessSettings;

pub const FILE: &str = "/etc/nginx/sites-available/hotel";
pub const PORT: &str = "85";
pub const LOG_DIRECTORY: &str = "/var/log/nginx/hotel";
/// The proxies NGINX takes the visitor's address from: 127.0.0.1 (Traefik), and Cloudflare's ranges behind Cloudflare.
pub const REAL_IP_FILE: &str = "/etc/nginx/hotel-real-ip.conf";
pub const DEFAULT_ROOT: &str = "/usr/share/nginx/html";

/// Written by Polaris, served by NGINX.
pub fn camera() -> String {
    format!("{}/camera", crate::octane::gamedata())
}

/// The script that writes the hotel site with this root (None: keep what the site has). Also disables other sites on
/// 80/443 and checks 127.0.0.1:85. With the CMS on its own port the root is dist/, with a legacy AtomCMS its public/.
/// (WriteScript)
pub fn write_script(site: &WebAccessSettings, root: Option<&str>) -> String {
    let dist = crate::octane::dist();
    let atom = crate::atom_cms::public();
    let root = if site.cms_on_own_port() {
        Some(dist.as_str())
    } else if site.atom_cms {
        Some(atom.as_str())
    } else {
        root
    };
    write_script_for(site, root)
}

/// Writes [`REAL_IP_FILE`]: 127.0.0.1, plus Cloudflare's ranges when the site uses Cloudflare (a failed download keeps
/// the file as it was), and points an older hotel site at it instead of its own 127.0.0.1 line. Does not reload NGINX.
/// (RealIpScript)
pub fn real_ip_script(site: &WebAccessSettings) -> String {
    crate::script::fill(
        REAL_IP,
        &[("cloudflare", if site.cloudflare { "yes" } else { "no" }), ("RealIpFile", REAL_IP_FILE), ("File", FILE)],
    )
}

/// PHP for a CMS: Laravel-style front controller, PHP-FPM 8.5, no .htaccess files served. (PhpLocations)
pub fn php_locations() -> String {
    crate::script::fill(PHP_LOCATIONS, &[("Socket", crate::php::SOCKET)])
}

// ---------------------------------------------------------------------------------------------------------------

use crate::script::fill;

const CORS: &str = r#"        add_header Access-Control-Allow-Origin   $hotel_cors_origin always;
        add_header Access-Control-Allow-Methods  "GET, POST, OPTIONS" always;
        add_header Access-Control-Allow-Headers  "DNT,User-Agent,X-Requested-With,If-Modified-Since,Cache-Control,Content-Type,Range" always;
        add_header Vary                          Origin always;"#;

const PREFLIGHT: &str = r#"        if ($request_method = OPTIONS) {
            add_header Access-Control-Allow-Origin   $hotel_cors_origin always;
            add_header Access-Control-Allow-Methods  "GET, POST, OPTIONS" always;
            add_header Access-Control-Allow-Headers  "DNT,User-Agent,X-Requested-With,If-Modified-Since,Cache-Control,Content-Type,Range" always;
            add_header Access-Control-Max-Age        1728000 always;
            add_header Vary                          Origin always;
            add_header Content-Length                0;
            return 204;
        }"#;

// Octane serves the site root.
const OCTANE_LOCATIONS: &str = r#"    location / {
        try_files $uri $uri/ /index.html;
    }

    location = /index.html {
        add_header Cache-Control "no-cache" always;
    }

    location = /.vite/manifest.json {
        try_files $uri =404;
        default_type application/json;
        add_header Cache-Control "no-store" always;
    }

    # Build output with a hash in the name never changes.
    location ~* ^/(assets|src/assets)/.+-[a-zA-Z0-9_]+\.(js|css|woff2?|ttf|otf|eot|svg|png|jpe?g|gif|webp|avif|map)$ {
        try_files $uri =404;
        expires 1y;
        add_header Cache-Control "public, immutable" always;
    }

    # Octane's runtime configuration: always the current version.
    location /configuration/ {
        try_files $uri =404;
        add_header Cache-Control "no-store" always;
    }"#;

const PHP_LOCATIONS: &str = r#"    client_max_body_size 20m;

    location / {
        try_files $uri $uri/ /index.php?$query_string;
    }

    location ~ \.php$ {
        include snippets/fastcgi-php.conf;
        fastcgi_pass unix:{{Socket}};
        fastcgi_param SCRIPT_FILENAME $realpath_root$fastcgi_script_name;
        fastcgi_hide_header X-Powered-By;
    }

    location ~ /\.ht {
        deny all;
    }"#;

// A CMS serves the site and Octane lives under /client/, which the CMS opens as /client/index.html?sso=...
const CLIENT_LOCATIONS: &str = r#"    location = /client {
        return 301 /client/;
    }

    location /client/ {
        alias {{Dist}}/;
        index index.html;
    }

    location = /client/index.html {
        alias {{Dist}}/index.html;
        add_header Cache-Control "no-cache" always;
    }

    location = /client/.vite/manifest.json {
        alias {{Dist}}/.vite/manifest.json;
        default_type application/json;
        add_header Cache-Control "no-store" always;
    }

    # Vite only puts files with a hash in their name in assets/: they never change.
    location ^~ /client/assets/ {
        alias {{Dist}}/assets/;
        expires 1y;
        add_header Cache-Control "public, immutable" always;
    }

    # Octane's runtime configuration: always the current version.
    location ^~ /client/configuration/ {
        alias {{Dist}}/configuration/;
        add_header Cache-Control "no-store" always;
    }"#;

fn client_locations() -> String {
    fill(CLIENT_LOCATIONS, &[("Dist", &crate::octane::dist())])
}

// An AtomCMS from before the CMS had its own port: this site serves it, with Octane on /client/.
fn legacy_atom_locations() -> String {
    format!("{}\n{}", php_locations(), client_locations())
}

// The CMS has its own site: Traefik only sends /client, /gamedata and /custom here.
fn cms_client_locations() -> String {
    format!("    location / {{\n        return 404;\n    }}\n{}", client_locations())
}

/// Like .NET's Regex.Escape.
pub(crate) fn regex_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' | '*' | '+' | '?' | '|' | '{' | '[' | '(' | ')' | '^' | '$' | '.' | '#' => {
                escaped.push('\\');
                escaped.push(c);
            }
            ' ' => escaped.push_str("\\ "),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\x0c' => escaped.push_str("\\f"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// "_" for an IP address, else the address with and without "www.".
pub(crate) fn server_name(site: &WebAccessSettings) -> String {
    if site.is_ip_address() {
        "_".into()
    } else if let Some(bare) = site.address.strip_prefix("www.") {
        format!("{} {}", site.address, bare)
    } else {
        format!("{} www.{}", site.address, site.address)
    }
}

const CONFIG: &str = r#"# The hotel website (written by All-in-1 Installer; the web access and Octane steps rewrite it).
# Traefik (/opt/traefik) answers on ports 80/443 for {{Url}} (and does the TLS) and passes the
# site to this server on 127.0.0.1:{{Port}}; /ws and Polaris' /api go straight to Polaris on 127.0.0.1:{{PolarisPort}}.
# {{what}}

map $http_origin $hotel_cors_origin {
    default "";
    "{{origin}}" $http_origin;
}

server {
    listen 127.0.0.1:{{Port}};
    server_name {{serverName}};

    root @ROOT@;
    index index.html index.php index.htm;

    autoindex     off;
    server_tokens off;

    # The visitor's address from X-Forwarded-For, read from the right past the trusted proxies: Traefik on
    # 127.0.0.1 and, behind Cloudflare, Cloudflare's ranges (in the real-IP file).
    include {{RealIpFile}};
    real_ip_header    X-Forwarded-For;
    real_ip_recursive on;

    add_header X-Content-Type-Options "nosniff" always;
{{hsts}}    add_header Referrer-Policy        "strict-origin";
    add_header X-XSS-Protection       "1; mode=block";
    add_header Permissions-Policy     "accelerometer=(), autoplay=(self), camera=(), encrypted-media=(), fullscreen=(), geolocation=(), gyroscope=(), magnetometer=(), microphone=(), midi=(), payment=(), picture-in-picture=(), sync-xhr=(), usb=()";

    client_header_buffer_size   1k;
    large_client_header_buffers 4 8k;

    # Text compressed (gamedata JSON such as FurnitureData.json is several MB); .nitro/.hab bundles
    # and images are compressed already and go as they are.
    gzip            on;
    gzip_vary       on;
    gzip_comp_level 5;
    gzip_min_length 1024;
    gzip_types      application/json application/javascript text/css text/plain text/xml application/xml image/svg+xml font/ttf font/otf application/wasm;

    # The client opens hundreds of gamedata files: keep their file handles and metadata at hand.
    open_file_cache          max=10000 inactive=60s;
    open_file_cache_valid    30s;
    open_file_cache_errors   on;

    access_log {{LogDirectory}}/access.log;
    error_log  {{LogDirectory}}/error.log;

{{locations}}
    # The hotel's assets (copied in with the All-in-1 download tool) and the camera photos.
    location /gamedata/ {
        alias {{Gamedata}}/;

{{Preflight}}
{{Cors}}

        try_files $uri $uri/ =404;

        # A nested location does not inherit the preflight above: repeat it.
        location ~* \.(jpg|jpeg|png|gif|ico|nitro|hab)$ {
            if ($request_method = OPTIONS) {
                add_header Access-Control-Allow-Origin   $hotel_cors_origin always;
                add_header Access-Control-Allow-Methods  "GET, POST, OPTIONS" always;
                add_header Access-Control-Allow-Headers  "DNT,User-Agent,X-Requested-With,If-Modified-Since,Cache-Control,Content-Type,Range" always;
                add_header Access-Control-Max-Age        1728000 always;
                add_header Vary                          Origin always;
                add_header Content-Length                0;
                return 204;
            }
            expires 30d;
            add_header Cache-Control               "public, no-transform";
            add_header Access-Control-Allow-Origin $hotel_cors_origin always;
            add_header Vary                        Origin            always;
        }
    }

    location /custom/ {
        alias {{Gamedata}}/custom/;
    }
}"#;

/// The site's NGINX configuration. @ROOT@ is substituted on the server. (Config)
pub fn config(site: &WebAccessSettings) -> String {
    let host = site.address.strip_prefix("www.").unwrap_or(&site.address);
    // /gamedata only allows CORS from the hotel's own pages, with or without www.
    let origin = if site.is_ip_address() {
        format!("~^https?://{}$", regex_escape(&site.address))
    } else {
        format!("~^https?://(www\\.)?{}$", regex_escape(host))
    };
    let hsts = if site.https { "    add_header Strict-Transport-Security \"max-age=2592000\" always;\n" } else { "" };
    let what = if site.cms_on_own_port() {
        format!(
            "{} is the site (its own NGINX site on 127.0.0.1:{}); this one serves Octane on /client/ and the assets.",
            site.cms_name().unwrap_or_default(),
            site.cms_port
        )
    } else if site.atom_cms {
        format!("AtomCMS ({}) is the site, Octane is on /client/.", crate::atom_cms::public())
    } else {
        "Octane is the site.".into()
    };
    let locations = if site.cms_on_own_port() {
        cms_client_locations()
    } else if site.atom_cms {
        legacy_atom_locations()
    } else {
        OCTANE_LOCATIONS.to_string()
    };
    // The inserted parts last, so nothing in them is taken for a placeholder.
    let text = fill(
        CONFIG,
        &[
            ("Url", &site.url()),
            ("Port", PORT),
            ("PolarisPort", crate::web_access::POLARIS_PORT),
            ("origin", &origin),
            ("serverName", &server_name(site)),
            ("RealIpFile", REAL_IP_FILE),
            ("LogDirectory", LOG_DIRECTORY),
            ("Gamedata", &crate::octane::gamedata()),
            ("what", &what),
            ("hsts", hsts),
        ],
    );
    fill(&text, &[("locations", &locations), ("Preflight", PREFLIGHT), ("Cors", CORS)])
}

const REAL_IP: &str = r##"realip={{RealIpFile}}
if [ {{cloudflare}} = yes ]; then
  # A server clock that runs behind makes Cloudflare's certificate "not yet valid": correct it first.
  type fix_clock >/dev/null 2>&1 && fix_clock
  cf=$( (curl -fsS --max-time 20 https://www.cloudflare.com/ips-v4; echo; curl -fsS --max-time 20 https://www.cloudflare.com/ips-v6) \
    | grep -E '^[0-9a-f.:]+/[0-9]+$' ) || true
  if [ "$(echo "$cf" | grep -c .)" -ge 10 ]; then
    { echo "# Proxies NGINX takes the visitor's address from (written by All-in-1 Installer): Traefik and Cloudflare."
      echo "set_real_ip_from 127.0.0.1;"
      echo "$cf" | sed 's/.*/set_real_ip_from &;/'; } > "$realip"
    echo "> $realip: 127.0.0.1 and $(echo "$cf" | grep -c .) Cloudflare ranges (the visitor's address, not Cloudflare's)"
  elif [ -f "$realip" ]; then
    echo "  Could not download Cloudflare's ranges: $realip stays as it was"
  else
    printf 'set_real_ip_from 127.0.0.1;\n' > "$realip"
    echo "  Could not download Cloudflare's ranges: NGINX sees Cloudflare's address until this runs again"
  fi
else
  printf '# Proxies NGINX takes the visitor'"'"'s address from (written by All-in-1 Installer): Traefik.\nset_real_ip_from 127.0.0.1;\n' > "$realip"
  echo "> $realip: 127.0.0.1 (Traefik)"
fi
if [ -f {{File}} ] && ! grep -q 'include {{RealIpFile}}' {{File}}; then
  sed -i -E 's#^([[:space:]]*)set_real_ip_from[[:space:]]+127\.0\.0\.1;#\1include {{RealIpFile}};#' {{File}}
  echo "  {{File}} now takes its proxies from $realip"
fi"##;

const WRITE: &str = r#"[ -d /etc/nginx/sites-available ] || fail "NGINX is not installed with sites-available. Run the NGINX step first."
if [ -f {{File}} ] && ! grep -q 'All-in-1 Installer' {{File}}; then
  cp {{File}} "{{File}}.bak-$(date +%Y%m%d-%H%M%S)"
  echo "  The old {{File}} is saved as {{File}}.bak-<time>"
fi
root='{{root}}'
[ -n "$root" ] || { [ -f {{File}} ] && root=$(awk '$1=="root" {gsub(/;/, "", $2); print $2; exit}' {{File}}); }
[ -n "$root" ] && [ "$root" != "@ROOT@" ] || root={{DefaultRoot}}

install -d -m 755 {{LogDirectory}}
install -d -m 2775 -o www-data -g www-data {{Gamedata}} {{Gamedata}}/custom \
  {{Camera}} {{Camera}}/thumb
# The nginx user must read what the emulator writes there with group www-data and mode 640.
if id nginx >/dev/null 2>&1 && ! id -nG nginx | grep -qw www-data; then
  usermod -aG www-data nginx
  echo "  nginx is now in the www-data group (to read the emulator's camera photos)"
fi

{{RealIpScript}}
echo "> {{File}}: listen 127.0.0.1:{{Port}}, root $root"
cat > {{File}} <<'SITE'
{{Config}}
SITE
sed -i "s#@ROOT@#$root#" {{File}}
ln -sfn {{File}} /etc/nginx/sites-enabled/hotel

# Ports 80 and 443 belong to Traefik, so the default site and any other site must not listen there.
if [ -L /etc/nginx/sites-enabled/default ] || [ -f /etc/nginx/sites-enabled/default ]; then
  rm -f /etc/nginx/sites-enabled/default
  echo "  The default site is disabled (still in sites-available/default)"
fi
for f in /etc/nginx/conf.d/*.conf /etc/nginx/sites-enabled/*; do
  [ -f "$f" ] || continue
  if grep -Eq '^[[:space:]]*listen[[:space:]]+(\[::\]:|0\.0\.0\.0:)?(80|443)([[:space:]]|;)' "$f"; then
    fail "$f listens on port 80 or 443, which Traefik needs. Move that site to 127.0.0.1:{{Port}} (or disable it) and try again."
  fi
done

nginx -t 2>&1 | sed 's/^/  /'
nginx -t >/dev/null 2>&1 || fail "nginx -t refuses the configuration (see above)."
if [ -d /run/systemd/system ]; then systemctl reload nginx || systemctl restart nginx; else nginx -s reload 2>/dev/null || service nginx restart; fi
sleep 1
# With the CMS on its own site this one only has /client (and the assets).
code=$(curl -s --noproxy '*' -o /dev/null -w '%{http_code}' http://127.0.0.1:{{Port}}{{path}} || true)
echo "  http://127.0.0.1:{{Port}}{{path}} answers $code"
case "$code" in 200|302|403{{also}}) ;; *) fail "NGINX does not answer on 127.0.0.1:{{Port}} (HTTP '$code')." ;; esac"#;

fn write_script_for(site: &WebAccessSettings, root: Option<&str>) -> String {
    let own = site.cms_on_own_port();
    let camera = camera();
    let text = fill(
        WRITE,
        &[
            ("File", FILE),
            ("root", root.unwrap_or("")),
            ("DefaultRoot", DEFAULT_ROOT),
            ("LogDirectory", LOG_DIRECTORY),
            ("Gamedata", &crate::octane::gamedata()),
            ("Camera", &camera),
            ("Port", PORT),
            ("path", if own { "/client/" } else { "/" }),
            ("also", if own { "|404" } else { "" }),
        ],
    );
    fill(&text, &[("RealIpScript", &real_ip_script(site)), ("Config", &config(site))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_and_names() {
        assert_eq!(regex_escape("hotel.example.com"), "hotel\\.example\\.com");
        assert_eq!(regex_escape("2001:db8::1"), "2001:db8::1");
        assert_eq!(server_name(&WebAccessSettings::new("192.0.2.10", false, false)), "_");
        assert_eq!(server_name(&WebAccessSettings::new("www.a.com", false, false)), "www.a.com a.com");
        assert_eq!(server_name(&WebAccessSettings::new("a.com", false, false)), "a.com www.a.com");
    }
}

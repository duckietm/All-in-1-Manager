//! Your own CMS: Git access or a ZIP, the root folder and the tests. (CustomCmsInstaller.cs)
//!
//! PORTING: the items above the line are used by other modules; keep their names and signatures.

pub const PARENT: &str = "/var/www";

// ---------------------------------------------------------------------------------------------------------------
//
// Your own CMS as the hotel's website, like AtomCMS: from a Git link or a ZIP, in /var/www/<name>, served by its own
// NGINX site (PHP 8.5 or static) on a free port from 85 to 95, with Octane on /client. Its database settings are yours.

use std::sync::OnceLock;

use aio_core::ServerLayout;
use regex::Regex;

use crate::atom_cms::{self, AtomCmsState};
use crate::cms_site;
use crate::script::{self, fill, ScriptValues};
use crate::server::{shell_quote, Log, Result, Server};
use crate::web_access::WebAccessSettings;

/// What the server has for your own CMS before the step runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomCmsState {
    pub server: AtomCmsState,
    pub current_name: Option<String>,
    pub current_root: Option<String>,
    pub current_php: Option<bool>,
    pub folder_has_content: bool,
}

impl CustomCmsState {
    pub fn site(&self) -> Option<&WebAccessSettings> {
        self.server.site.as_ref()
    }
}

pub const MAX_NAME_LENGTH: usize = 12;

pub fn directory(name: &str) -> String {
    format!("{PARENT}/{name}")
}

fn name_pattern() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    P.get_or_init(|| Regex::new("^[a-z0-9]{1,12}$").unwrap())
}

fn root_pattern() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    P.get_or_init(|| Regex::new("^/([A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)*/?)?$").unwrap())
}

fn git_url_pattern() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    P.get_or_init(|| Regex::new("^https://[A-Za-z0-9.-]+(:[0-9]+)?/[A-Za-z0-9._~/-]+$").unwrap())
}

/// The folder name: lower case letters and digits, at most 12, and not a folder the hotel already uses.
pub fn name_problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("Enter a folder name.".into());
    }
    if !name_pattern().is_match(name) {
        return Some(format!("Use only lower case letters (a-z) and digits, no spaces or other characters, at most {MAX_NAME_LENGTH}."));
    }
    let path = directory(name);
    if matches!(name, "html" | "hotel" | "client" | "gamedata" | "custom")
        || ServerLayout::current().paths().iter().any(|(_, value)| value.trim_end_matches('/').eq_ignore_ascii_case(&path))
    {
        return Some(format!("{path} is used by the hotel already: choose another name."));
    }
    None
}

/// The root inside the CMS folder: "/" or "/public", "/web/dist" ...
pub fn root_problem(root: &str) -> Option<String> {
    if root_pattern().is_match(root) && !root.split('/').any(|part| part == "." || part == "..") {
        None
    } else {
        Some("The root starts with / and has only letters, digits, '.', '_', '-' and '/', for example /public, or just / for the top folder.".into())
    }
}

/// An https Git link, such as https://github.com/owner/cms or https://github.com/owner/cms.git.
pub fn git_url_problem(url: &str) -> Option<String> {
    if git_url_pattern().is_match(url) {
        None
    } else {
        Some("Use the https link of the repository, for example https://github.com/owner/cms.git.".into())
    }
}

/// A folder name from a repository link: "https://github.com/owner/My-CMS.git" gives "mycms".
pub fn name_from_url(url: &str) -> String {
    let mut last = url.trim_end_matches('/').split('/').next_back().unwrap_or("");
    if last.len() >= 4 && last.is_char_boundary(last.len() - 4) && last[last.len() - 4..].eq_ignore_ascii_case(".git") {
        last = &last[..last.len() - 4];
    }
    let name: String = last.chars().flat_map(char::to_lowercase).filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit()).collect();
    if name.len() > MAX_NAME_LENGTH {
        name[..MAX_NAME_LENGTH].to_string()
    } else {
        name
    }
}

/// "/public" or "/".
pub fn normalize_root(root: &str) -> String {
    let r = root.trim();
    if r.chars().count() > 1 {
        r.trim_end_matches('/').to_string()
    } else {
        "/".into()
    }
}

/// With the folder: /var/www/name/public.
pub fn root_path(name: &str, root: &str) -> String {
    let root = normalize_root(root);
    if root == "/" {
        directory(name)
    } else {
        directory(name) + &root
    }
}

const INSPECT: &str = r#"f={{File}}
echo "root=$(awk '$1=="root" {gsub(/;/, "", $2); print $2; exit}' "$f" 2>/dev/null)"
echo "php=$([ -f "$f" ] && { grep -q fastcgi_pass "$f" && echo yes || echo no; })"
echo "content=$([ -n "$(ls -A {{Directory}} 2>/dev/null)" ] && echo yes)""#;

/// `name` is the folder the user entered (when it is valid); otherwise the CMS the hotel has now is inspected.
pub async fn inspect(server: &dyn Server, name: Option<&str>) -> Result<CustomCmsState> {
    let state = atom_cms::inspect(server).await?;
    let current = state.site.as_ref().and_then(|s| s.custom_cms.clone());
    let look = match name {
        Some(n) if !n.is_empty() && name_problem(n).is_none() => Some(n.to_string()),
        _ => current.clone(),
    };
    let mut root = None;
    let mut php = None;
    let mut content = false;
    if let Some(look) = look {
        let folder = directory(&look);
        let script = fill(INSPECT, &[("File", &cms_site::file(&look)), ("Directory", &folder)]);
        let values = ScriptValues::parse(&server.run_as_root(&script).await?.output);
        if let Some(full) = values.get("root").filter(|full| full.starts_with(&folder)) {
            root = Some(if full.len() == folder.len() { "/".to_string() } else { full[folder.len()..].to_string() });
        }
        php = match values.get("php") {
            Some("yes") => Some(true),
            Some("no") => Some(false),
            _ => None,
        };
        content = values.yes("content");
    }
    Ok(CustomCmsState { server: state, current_name: current, current_root: root, current_php: php, folder_has_content: content })
}

const CHECK_GIT_ACCESS: &str = r#"if ! command -v git >/dev/null 2>&1; then
  echo "> Installing git"
  apt_update >/dev/null
  apt-get install -y git >/dev/null
fi
u={{url}}
echo "> git ls-remote $u"
heads=$(GIT_TERMINAL_PROMPT=0 timeout 60 git ls-remote --heads "$u" 2>&1) ||
  fail "The server cannot read $u: it does not exist, or it is private. Make it public, or download it as a ZIP and use the ZIP option."
echo "  $(echo "$heads" | grep -c refs/heads) branches""#;

/// Whether the server can read the repository; a private repository would ask for a password, which hangs.
pub async fn check_git_access(server: &dyn Server, url: &str, log: Log<'_>) -> Result<()> {
    let body = fill(CHECK_GIT_ACCESS, &[("url", &shell_quote(url))]);
    script::run(server, &script::with_prelude(&body), log, "Reading the repository failed", None).await
}

const EXTRACT_ZIP: &str = r#"z={{zip}}
d={{Directory}}
tmp=
trap 'rm -f "$z"; [ -n "$tmp" ] && rm -rf "$tmp"' EXIT
if ! command -v unzip >/dev/null 2>&1; then
  echo "> Installing unzip"
  apt_update >/dev/null
  apt-get install -y unzip >/dev/null
fi
[ -z "$(ls -A "$d" 2>/dev/null)" ] || fail "$d is not empty."
unzip -tq "$z" >/dev/null 2>&1 || fail "$(basename "$z") is not a ZIP file, or it is damaged."
tmp=$(mktemp -d {{Parent}}/.cms-unzip-XXXXXX)
echo "> unzip $(basename "$z") ($(du -h "$z" | cut -f1))"
# unzip refuses paths that leave the folder (../); -o: no questions.
unzip -q -o "$z" -d "$tmp" || fail "unzip stopped (see above)."
src="$tmp"
# One folder at the top and nothing else: that folder is the CMS.
if [ "$(ls -A "$tmp" | wc -l)" = 1 ] && [ -d "$tmp/$(ls -A "$tmp")" ]; then
  src="$tmp/$(ls -A "$tmp")"
  echo "  The ZIP has one folder at the top ($(basename "$src")): using what is in it"
fi
install -d -m 755 "$d"
(shopt -s dotglob; mv "$src"/* "$d"/)
echo "  $(find "$d" -type f | wc -l) files in $d""#;

/// Unpacks an uploaded ZIP into the CMS folder, which must not exist or be empty. A ZIP with one folder at the top (as
/// GitHub's "Download ZIP" makes) gives that folder's content. Removes the upload.
pub async fn extract_zip(server: &dyn Server, remote_zip: &str, name: &str, log: Log<'_>) -> Result<()> {
    let body = fill(EXTRACT_ZIP, &[("zip", &shell_quote(remote_zip)), ("Directory", &directory(name)), ("Parent", PARENT)]);
    script::run(server, &script::with_prelude(&body), log, "Unpacking the ZIP failed", None).await
}

const PREPARE: &str = r#"d={{Directory}}
r={{path}}
if [ ! -d "$r" ]; then
  echo "  The folders at the top of $d:"
  find "$d" -mindepth 1 -maxdepth 1 -type d ! -name '.*' -printf '    /%f\n' | sort | head -20
  fail "The root {{root}} does not exist in $d. Choose the folder with the start page ({{start}}), from the list above."
fi
echo "> Root $r"
found=
for f in {{pages}}; do [ -f "$r/$f" ] && { found=$f; break; }; done
if [ -n "$found" ]; then echo "  start page: $found"; else echo "  WARNING: no {{start}} in $r: the address of the site shows 'not found' until there is one."; fi
if [ {{php}} = no ] && find "$r" -maxdepth 2 -name '*.php' | grep -q .; then
  echo "  WARNING: there are .php files, but the site is set up as static: NGINX sends them as text. Choose PHP if the CMS needs it."
fi
chown -R www-data:www-data "$d"
chmod -R u+rwX,g+rX,o-w "$d"
# Laravel and most PHP CMSes write here.
for w in storage bootstrap/cache cache tmp uploads; do [ -d "$d/$w" ] && chmod -R ug+rwX "$d/$w"; done
echo "  $d belongs to www-data (PHP-FPM and NGINX)""#;

/// Checks that the root exists and has a start page, and gives the folder to www-data (PHP-FPM and NGINX).
pub async fn prepare(server: &dyn Server, name: &str, root: &str, php: bool, log: Log<'_>) -> Result<()> {
    let body = fill(
        PREPARE,
        &[
            ("Directory", &directory(name)),
            ("path", &root_path(name, root)),
            ("root", &normalize_root(root)),
            ("start", if php { "index.php" } else { "index.html" }),
            ("pages", if php { "index.php index.html" } else { "index.html index.htm" }),
            ("php", if php { "yes" } else { "no" }),
        ],
    );
    script::run(server, &script::with_prelude(&body), log, "Checking the CMS folder failed", None).await
}

const TEST: &str = r#"url="{{Scheme}}://{{name}}"
c() { curl -s --noproxy '*' -k --max-time 15 --resolve "{{name}}:{{port}}:127.0.0.1" "$@"; }
echo "> $url/ ({{CmsName}})"
code=$(c -o /dev/null -w '%{http_code}' "$url/" || true)
echo "  HTTP $code"
case "$code" in
  2*|3*) ;;
  000|"") fail "Nothing answers on $url/." ;;
  5*) echo "  The CMS answers with an error: usually its database settings are not filled in yet (see the end of this step)." ;;
  *) echo "  The CMS answers HTTP $code: check the root folder and the start page." ;;
esac
echo "> $url/client/ (Octane)"
c "$url/client/" | grep -q 'configuration/bootstrap.js' || fail "$url/client/ does not serve Octane."
echo "  index.html OK"
echo "> $url/api/health (Polaris)"
echo "  HTTP $(c -o /dev/null -w '%{http_code}' "$url/api/health" || true)""#;

/// Checks through Traefik that the CMS, /client/ and Polaris' /api/health answer. A CMS error page is not a failure.
pub async fn test(server: &dyn Server, site: &WebAccessSettings, log: Log<'_>) -> Result<()> {
    let (name, port) = atom_cms::traefik_target(site);
    let cms_name = site.cms_name().unwrap_or_default();
    let body = fill(TEST, &[("Scheme", site.scheme()), ("name", &name), ("port", port), ("CmsName", &cms_name)]);
    script::run(server, &script::with_prelude(&body), log, "Testing the CMS failed", None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(name_problem(""), Some("Enter a folder name.".into()));
        assert!(name_problem("My CMS").unwrap().starts_with("Use only lower case"));
        assert!(name_problem("abcdefghijklm").is_some());
        assert_eq!(name_problem("hotel"), Some("/var/www/hotel is used by the hotel already: choose another name.".into()));
        assert_eq!(name_problem("mycms"), None);
        assert_eq!(name_from_url("https://github.com/owner/My-CMS.git"), "mycms");
        assert_eq!(name_from_url("https://github.com/owner/Very_Long-Name-Of-CMS.GIT/"), "verylongname");
    }

    #[test]
    fn roots() {
        assert_eq!(root_problem("/"), None);
        assert_eq!(root_problem("/public/"), None);
        assert_eq!(root_problem("/web/dist"), None);
        assert!(root_problem("public").is_some());
        assert!(root_problem("/../etc").is_some());
        assert!(root_problem("/a b").is_some());
        assert_eq!(normalize_root(" /public/ "), "/public");
        assert_eq!(normalize_root(""), "/");
        assert_eq!(normalize_root("/"), "/");
        assert_eq!(root_path("mycms", "/"), "/var/www/mycms");
        assert_eq!(root_path("mycms", "/public/"), "/var/www/mycms/public");
    }

    #[test]
    fn git_urls() {
        assert_eq!(git_url_problem("https://github.com/owner/cms.git"), None);
        assert_eq!(git_url_problem("https://git.example.com:3000/owner/cms"), None);
        assert!(git_url_problem("http://github.com/owner/cms").is_some());
        assert!(git_url_problem("git@github.com:owner/cms.git").is_some());
    }
}

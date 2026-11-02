//! Installs one of Octane's per-language UITexts_<code>.jsonc.example files as ${gamedata.url}/config/UITexts.jsonc,
//! which the renderer loads for the client's interface texts. (UiTextsInstaller.cs)

use std::sync::OnceLock;

use regex::Regex;

use crate::script::{self, fill, ScriptValues};
use crate::server::{Log, Result, Server};

/// `languages`: codes that have a UITexts_<code>.jsonc.example, from the Octane clone or the known list.
/// `current`: the language UITexts.jsonc is a copy of, "custom" if it matches none, or None if there is no file.
/// `from_clone`: whether the examples come from the Octane clone on the server rather than GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiTextsState {
    pub languages: Vec<String>,
    pub current: Option<String>,
    pub from_clone: bool,
}

impl UiTextsState {
    pub fn exists(&self) -> bool {
        self.current.is_some()
    }
}

pub fn target() -> String {
    format!("{}/config/UITexts.jsonc", crate::octane::gamedata())
}

const RAW_URL: &str = "https://raw.githubusercontent.com/duckietm/Octane/main/public/configuration";

/// Octane's languages, used when there is no clone to look in.
pub const KNOWN_LANGUAGES: [&str; 7] = ["ar", "de", "en", "es", "fr", "it", "nl"];

const NAMES: [(&str, &str); 12] = [
    ("ar", "Arabic"),
    ("de", "German (Deutsch)"),
    ("en", "English"),
    ("es", "Spanish (Español)"),
    ("fr", "French (Français)"),
    ("it", "Italian (Italiano)"),
    ("nl", "Dutch (Nederlands)"),
    ("pt", "Portuguese (Português)"),
    ("br", "Brazilian Portuguese (Português)"),
    ("tr", "Turkish (Türkçe)"),
    ("fi", "Finnish (Suomi)"),
    ("pl", "Polish (Polski)"),
];

/// "German (Deutsch) · de", or the code itself for an unknown language.
pub fn name(code: &str) -> String {
    match NAMES.iter().find(|(c, _)| c.eq_ignore_ascii_case(code)) {
        Some((_, name)) => format!("{name} · {code}"),
        None => code.to_string(),
    }
}

fn language_code() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new("^[A-Za-z]{2,3}([_-][A-Za-z]{2})?$").unwrap())
}

/// Whether `code` looks like a language code ("de", "pt_BR").
pub fn is_language_code(code: &str) -> bool {
    language_code().is_match(code)
}

fn example(code: &str) -> String {
    format!("UITexts_{code}.jsonc.example")
}

const INSPECT: &str = r#"dir={{ConfigurationDirectory}}
target={{Target}}
langs=""
for f in "$dir"/UITexts_*.jsonc.example; do
  [ -f "$f" ] || continue
  l=${f##*/UITexts_}; langs="$langs ${l%.jsonc.example}"
done
echo "LANGS=$langs"
if [ -f "$target" ]; then
  current=custom
  for l in $langs; do cmp -s "$target" "$dir/UITexts_$l.jsonc.example" && { current=$l; break; }; done
  echo "CURRENT=$current"
fi"#;

pub async fn inspect(server: &dyn Server) -> Result<UiTextsState> {
    let command = fill(INSPECT, &[("ConfigurationDirectory", &crate::octane::configuration_directory()), ("Target", &target())]);
    let values = ScriptValues::parse(&server.run_as_root(&command).await?.output);
    Ok(parse_state(&values))
}

fn parse_state(values: &ScriptValues) -> UiTextsState {
    let found: Vec<String> = values.or_empty("LANGS").split(' ').filter(|l| !l.is_empty() && is_language_code(l)).map(str::to_string).collect();
    let from_clone = !found.is_empty();
    // Without a clone the file is not compared with GitHub's copies, so it reads as "custom" and is backed up when replaced.
    UiTextsState {
        languages: if from_clone { found } else { KNOWN_LANGUAGES.iter().map(|l| l.to_string()).collect() },
        current: values.string("CURRENT"),
        from_clone,
    }
}

const INSTALL: &str = r#"src={{ConfigurationDirectory}}/{{Example}}
target={{Target}}
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
if [ -f "$src" ]; then
  echo "> $src"
  cp "$src" "$tmp"
else
  echo "> Not in the Octane clone: downloading {{RawUrl}}/{{Example}}"
  download "{{RawUrl}}/{{Example}}" "$tmp" || fail "Downloading {{Example}} failed"
fi
# JSON with comments, so the first character that is not a space or comment must be '{'.
[ -s "$tmp" ] && sed -e 's#//.*##' "$tmp" | tr -d ' \t\r\n\357\273\277' | grep -q '^{' || fail "{{Example}} is not a UITexts file"
echo "  $(grep -c '":' "$tmp") texts"
install -d -m 2775 -o www-data -g www-data "${target%/*}"
# Back up only a custom file, not one of Octane's languages.
own=no
if [ -f "$target" ] && ! cmp -s "$target" "$tmp"; then
  own=yes
  for f in {{ConfigurationDirectory}}/UITexts_*.jsonc.example; do
    [ -f "$f" ] && cmp -s "$target" "$f" && own=no
  done
fi
if [ "$own" = yes ]; then
  backup="$target.bak-$(date +%Y%m%d%H%M%S)"
  cp -p "$target" "$backup"
  echo "  The previous UITexts.jsonc is kept as $backup"
  echo "BACKUP=$backup"
fi
install -m 664 -o www-data -g www-data "$tmp" "$target"
echo "> $target ({{code}})""#;

/// Returns the path of the backup of the previous file, if one was made.
pub async fn install(server: &dyn Server, code: &str, log: Log<'_>) -> Result<Option<String>> {
    if !is_language_code(code) {
        // C#: ArgumentException(..., nameof(code)).
        anyhow::bail!("Not a language code: {code} (Parameter 'code')");
    }
    let body = fill(
        INSTALL,
        &[
            ("ConfigurationDirectory", &crate::octane::configuration_directory()),
            ("Target", &target()),
            ("RawUrl", RAW_URL),
            ("Example", &example(code)),
            ("code", code),
        ],
    );
    let values = script::run_capturing(server, &script::with_prelude(&body), log, "Installing UITexts.jsonc failed", &["BACKUP"], None).await?;
    Ok(values.string("BACKUP"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_names() {
        assert!(is_language_code("de"));
        assert!(is_language_code("pt_BR"));
        assert!(is_language_code("pt-br"));
        assert!(!is_language_code("d"));
        assert!(!is_language_code("de;rm"));
        assert_eq!(name("DE"), "German (Deutsch) · DE");
        assert_eq!(name("xx"), "xx");
    }

    #[test]
    fn state() {
        let s = parse_state(&ScriptValues::parse("LANGS= de en bad;code\nCURRENT=custom"));
        assert_eq!(s.languages, vec!["de", "en"]);
        assert!(s.from_clone && s.exists());
        let s = parse_state(&ScriptValues::parse("LANGS="));
        assert_eq!(s.languages.len(), 7);
        assert!(!s.from_clone && !s.exists());
    }
}

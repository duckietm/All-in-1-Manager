//! A plugin's own translations: `i18n/<lang>.json` in its package (or developer folder), the same convention as the
//! Manager's: a JSON object `{ "English text": "translation" }` with `{name}` placeholders. The English text is the
//! key, and a text without a translation stays English.
//!
//! A language file is untrusted input. [`Texts::parse`] accepts only a flat object of strings, at most
//! [`MAX_TEXTS`] entries, each key and value at most [`MAX_TEXT`] bytes, and a translation must keep every `{name}` of
//! its key. The file size limit (512 KiB) is the package's. Translations are shown like every other plugin text
//! (display-safe), by whoever draws them.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Result};

use crate::manifest::display_safe;

/// The language codes a plugin may ship a file for: the Manager's, without English (English is the key itself).
/// The app checks that this equals its own list.
pub const LANGUAGES: [&str; 8] = ["it", "fr", "de", "nl", "es", "pt", "tr", "fi"];

/// Entries per language file.
pub const MAX_TEXTS: usize = 5_000;
/// Bytes of one key or one translation.
pub const MAX_TEXT: usize = 4 * 1024;
/// Bytes of one language file (the package's limit too).
pub const MAX_FILE: u64 = 512 * 1024;
/// Language files read from a developer folder (a package holds at most 64 files in all).
pub const MAX_FILES: usize = 64;
/// The warning when a folder has more.
pub const TOO_MANY_FILES: &str = "More than 64 language files: the others are ignored";

/// The texts of one language, checked. Cheap to clone (shared).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Texts {
    language: String,
    map: Arc<HashMap<String, String>>,
}

/// The `{name}` placeholders of `text`, in order (the Manager's own rule: from each `{` to the next `}`).
pub fn placeholders(text: &str) -> Vec<&str> {
    text.split('{').skip(1).filter_map(|part| part.split_once('}').map(|(name, _)| name)).collect()
}

impl Texts {
    /// Parses and checks the file `i18n/<language>.json`. The error says what is wrong, naming the file.
    pub fn parse(language: &str, bytes: &[u8]) -> Result<Texts> {
        let file = format!("i18n/{language}.json");
        if bytes.len() as u64 > MAX_FILE {
            bail!("{file} is too large");
        }
        // serde_json refuses nesting deeper than 128 levels, so a deeply nested file is an error, not a crash.
        let Ok(serde_json::Value::Object(object)) = serde_json::from_slice::<serde_json::Value>(bytes) else {
            bail!("{file} is not a JSON object of texts");
        };
        if object.len() > MAX_TEXTS {
            bail!("{file} has more than {MAX_TEXTS} texts");
        }
        let mut map = HashMap::with_capacity(object.len());
        for (key, value) in object {
            let serde_json::Value::String(value) = value else {
                bail!("{file}: the translation of \"{}\" is not a string", display_safe(&key, 80));
            };
            if key.len() > MAX_TEXT || value.len() > MAX_TEXT {
                bail!("{file}: a text is longer than 4 KiB");
            }
            if let Some(lost) = placeholders(&key).into_iter().find(|name| !value.contains(&format!("{{{name}}}"))) {
                bail!("{file}: \"{}\" lost {{{}}}", display_safe(&key, 80), display_safe(lost, 40));
            }
            // An empty translation is no translation: the English text is shown, never a blank.
            if !value.is_empty() {
                map.insert(key, value);
            }
        }
        Ok(Texts { language: language.to_string(), map: Arc::new(map) })
    }

    /// The language code.
    pub fn language(&self) -> &str {
        &self.language
    }

    /// The translation of `english`, if the file has one.
    pub fn get(&self, english: &str) -> Option<&str> {
        self.map.get(english).map(String::as_str)
    }
}

/// `english` in `texts`' language, or `english` itself.
pub fn translate<'a>(english: &'a str, texts: Option<&'a Texts>) -> &'a str {
    texts.and_then(|t| t.get(english)).unwrap_or(english)
}

/// Checks a package's language file: an unknown language is refused, `en` is ignored (`Ok(None)`).
pub(crate) fn check_package_file(language: &str, bytes: &[u8]) -> Result<Option<Texts>> {
    if language == "en" {
        return Ok(None);
    }
    if !LANGUAGES.contains(&language) {
        bail!("Unknown language in the package: {language}");
    }
    Texts::parse(language, bytes).map(Some)
}

/// Reads every `<dir>/i18n/<lang>.json`: the texts by language, and a warning line for each file that is ignored (an
/// unknown language, an unreadable or invalid file). For an installed plugin (its files were checked at install) and
/// for a developer folder (read at load and at each Reload). No folder: nothing, no warning.
pub fn load_dir(dir: &Path) -> (BTreeMap<String, Texts>, Vec<String>) {
    let (mut texts, mut warnings) = (BTreeMap::new(), Vec::new());
    let Ok(read) = std::fs::read_dir(dir.join("i18n")) else { return (texts, warnings) };
    let mut files: Vec<_> = read.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).take(MAX_FILES + 1).collect();
    let too_many = files.len() > MAX_FILES;
    files.truncate(MAX_FILES);
    files.sort();
    for path in files {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(language) = name.strip_suffix(".json") else { continue };
        if language == "en" {
            continue;
        }
        if !LANGUAGES.contains(&language) {
            warnings.push(format!("Unknown language: i18n/{} is ignored", display_safe(&name, 40)));
            continue;
        }
        match read_file(&path).and_then(|bytes| Texts::parse(language, &bytes)) {
            Ok(t) => {
                texts.insert(language.to_string(), t);
            }
            Err(e) => warnings.push(format!("{e:#}; the file is ignored")),
        }
    }
    if too_many {
        warnings.push(TOO_MANY_FILES.to_string());
    }
    (texts, warnings)
}

/// Reads at most [`MAX_FILE`] bytes (one more is an error), through no link.
fn read_file(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let name = format!("i18n/{}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    if !std::fs::symlink_metadata(path).map(|m| m.is_file()).unwrap_or(false) {
        bail!("{name} is not a file");
    }
    let mut out = Vec::new();
    let file = std::fs::File::open(path).map_err(|e| anyhow::anyhow!("{name} cannot be read: {e}"))?;
    file.take(MAX_FILE + 1).read_to_end(&mut out).map_err(|e| anyhow::anyhow!("{name} cannot be read: {e}"))?;
    if out.len() as u64 > MAX_FILE {
        bail!("{name} is too large");
    }
    Ok(out)
}

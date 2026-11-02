//! Window translations. The English text is the key: `t("Run operation")`. Each language is a
//! JSON object `{ "English text": "translation" }` in `assets/i18n/<code>.json`, embedded in the
//! exe; a missing entry falls back to English. Placeholders use `{name}`: `tf("{n} files", &[("n", "3")])`.
//! The step logs stay in English: they are what the server prints, and what gets pasted into a support question.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lang {
    pub code: &'static str,
    pub native_name: &'static str,
    /// The Habbo hotel that speaks it, for the flag next to the name.
    pub hotel_domain: &'static str,
    json: &'static str,
}

/// The languages of the Habbo hotels.
pub const LANGUAGES: &[Lang] = &[
    Lang { code: "en", native_name: "English", hotel_domain: "habbo.com", json: "{}" },
    Lang { code: "it", native_name: "Italiano", hotel_domain: "habbo.it", json: include_str!("../assets/i18n/it.json") },
    Lang { code: "fr", native_name: "Français", hotel_domain: "habbo.fr", json: include_str!("../assets/i18n/fr.json") },
    Lang { code: "de", native_name: "Deutsch", hotel_domain: "habbo.de", json: include_str!("../assets/i18n/de.json") },
    Lang { code: "nl", native_name: "Nederlands", hotel_domain: "habbo.nl", json: include_str!("../assets/i18n/nl.json") },
    Lang { code: "es", native_name: "Español", hotel_domain: "habbo.es", json: include_str!("../assets/i18n/es.json") },
    Lang { code: "pt", native_name: "Português (Brasil)", hotel_domain: "habbo.com.br", json: include_str!("../assets/i18n/pt.json") },
    Lang { code: "tr", native_name: "Türkçe", hotel_domain: "habbo.com.tr", json: include_str!("../assets/i18n/tr.json") },
    Lang { code: "fi", native_name: "Suomi", hotel_domain: "habbo.fi", json: include_str!("../assets/i18n/fi.json") },
];

static CURRENT: AtomicUsize = AtomicUsize::new(0);

fn tables() -> &'static Vec<HashMap<String, &'static str>> {
    static TABLES: OnceLock<Vec<HashMap<String, &'static str>>> = OnceLock::new();
    TABLES.get_or_init(|| {
        LANGUAGES
            .iter()
            .map(|lang| {
                let map: HashMap<String, String> = serde_json::from_str(lang.json).unwrap_or_default();
                // Leaked once per process: the strings live as long as the window.
                map.into_iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k, &*Box::leak(v.into_boxed_str()))).collect()
            })
            .collect()
    })
}

pub fn current() -> Lang {
    LANGUAGES[CURRENT.load(Ordering::Relaxed)]
}

pub fn set(code: &str) {
    if let Some(index) = LANGUAGES.iter().position(|l| l.code.eq_ignore_ascii_case(code)) {
        CURRENT.store(index, Ordering::Relaxed);
    }
}

/// settings.json `Language`, else the system language when it is one of ours, else English.
pub fn init(settings: &aio_core::InstallerSettings) {
    let configured = settings.language.clone().filter(|v| !v.trim().is_empty());
    let code = configured.or_else(|| sys_locale::get_locale().map(|l| l.split(['-', '_']).next().unwrap_or("en").to_lowercase()));
    set(code.as_deref().unwrap_or("en"));
}

/// Marks an English text that is translated later with `t(variable)` (a constant table, a match arm), so
/// source/tools/i18n_keys.py collects it. Does nothing at run time.
pub const fn n(english: &'static str) -> &'static str {
    english
}

/// The translation of an English UI text (or the text itself).
pub fn t(english: &str) -> &str {
    let index = CURRENT.load(Ordering::Relaxed);
    if index == 0 {
        return english;
    }
    tables()[index].get(english).copied().unwrap_or(english)
}

/// `t` with `{name}` placeholders filled in.
pub fn tf(english: &str, values: &[(&str, &str)]) -> String {
    let mut text = t(english).to_string();
    for (name, value) in values {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_file_is_valid_and_keeps_placeholders() {
        // keys.json: every English text of the window (rebuilt when texts change).
        let english_keys: Vec<String> = serde_json::from_str(include_str!("../assets/i18n/keys.json")).unwrap();
        for lang in &LANGUAGES[1..] {
            let map: HashMap<String, String> = serde_json::from_str(lang.json).unwrap_or_else(|e| panic!("{}: {e}", lang.code));
            for key in &english_keys {
                let translated = map.get(key).unwrap_or_else(|| panic!("{}: missing {key:?}", lang.code));
                for part in key.split('{').skip(1) {
                    let name = part.split('}').next().unwrap();
                    assert!(translated.contains(&format!("{{{name}}}")), "{}: {key:?} lost {{{name}}}", lang.code);
                }
            }
        }
    }

    #[test]
    fn fallback_and_placeholders() {
        // The tables directly: set() would switch the language for the other tests running at the same time, which
        // compare English texts.
        let italian = LANGUAGES.iter().position(|l| l.code == "it").unwrap();
        assert_eq!(tables()[italian].get(&format!("{} {}", "no table", "has this")), None);
        assert_eq!(tables()[italian].get("Settings").copied(), Some("Impostazioni"));
        assert_eq!(tf("{count} operations", &[("count", "5")]), "5 operations");
    }
}

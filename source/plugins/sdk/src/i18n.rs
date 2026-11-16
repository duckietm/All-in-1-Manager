//! Translations: show the plugin's texts in the Manager's language.
//!
//! The English text is the key. Write every text of the page as `tr("Click me")`; the Manager looks it up in the
//! plugin's `i18n/<lang>.json` for its current language (`it`, `fr`, `de`, `nl`, `es`, `pt`, `tr` or `fi`) and gives
//! back the translation, or the English text itself when the language is English, there is no file or the file lacks
//! that key. Nothing here needs a permission, and the calls do not count as host I/O for the time budget.
//!
//! A text with values uses `{name}` placeholders, kept in the key and in every translation, and
//! [`tr_with`] fills them in after translating:
//!
//! ```
//! use aio_plugin_sdk::i18n::{tr, tr_with};
//!
//! let title = tr("Settings");
//! let line = tr_with("Greeted {n} times", &[("n", "3")]);
//! assert_eq!((title.as_str(), line.as_str()), ("Settings", "Greeted 3 times"));
//! ```
//!
//! The Manager translates the manifest's `name`, `description`, `page_title` and tile itself, by their English text
//! (no `tr` needed for those). Permission prompts and consent windows stay in English.
//!
//! The Manager draws the page again when the user switches language, so call `tr` in
//! [`render`](crate::Plugin::render) each time rather than keeping translated texts in the plugin's state.
//!
//! Off `wasm32` (a native `cargo test`) there is no Manager: [`tr`] returns the English text and
//! [`host::language`](crate::host::language) returns `"en"`, so a plugin's pages test with English texts.

/// The translation of `english` in the Manager's current language, or `english` itself when there is none.
///
/// Keys and values are at most 4 KiB; a plugin's language file has at most 5,000 entries (see the guide's limits).
pub fn tr(english: &str) -> String {
    translate(english)
}

/// Like [`tr`], then fills the `{name}` placeholders from `values`: `tr_with("Greeted {n} times", &[("n", "3")])`.
///
/// Placeholders are filled in one pass, so a value that itself contains `{x}` is never filled again. A placeholder
/// with no entry in `values` stays as written.
pub fn tr_with(english: &str, values: &[(&str, &str)]) -> String {
    tr_with_using(english, values, translate)
}

#[cfg(target_arch = "wasm32")]
fn translate(english: &str) -> String {
    crate::bindings::aio::plugin::host::translate(english)
}

#[cfg(not(target_arch = "wasm32"))]
fn translate(english: &str) -> String {
    english.to_string()
}

/// [`tr_with`] with the translation step passed in, so that it tests natively.
fn tr_with_using(english: &str, values: &[(&str, &str)], translate: impl Fn(&str) -> String) -> String {
    fill(&translate(english), values)
}

/// `text` with each `{name}` that has an entry in `values` replaced (one pass, left to right). Names are read as the
/// Manager reads them: a placeholder runs from a `{` to the next `}`, so in `"{a {b}"` the name is `b`, and `"{a "`
/// stays as written.
fn fill(text: &str, values: &[(&str, &str)]) -> String {
    let mut parts = text.split('{');
    let mut out = String::with_capacity(text.len());
    out.push_str(parts.next().unwrap_or_default());
    for part in parts {
        match part.split_once('}') {
            Some((name, rest)) => {
                match values.iter().find(|(key, _)| *key == name) {
                    Some((_, value)) => out.push_str(value),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                out.push_str(rest);
            }
            None => {
                out.push('{');
                out.push_str(part);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tr_with_fills_placeholders_after_translating() {
        let italian = |english: &str| match english {
            "{who} greeted {n} times" => "{who} ha salutato {n} volte".to_string(),
            other => other.to_string(),
        };
        assert_eq!(tr_with_using("{who} greeted {n} times", &[("n", "3"), ("who", "Ann")], italian), "Ann ha salutato 3 volte");
        // No translation: the English text, filled.
        assert_eq!(tr_with_using("Greeted {n} times", &[("n", "3")], italian), "Greeted 3 times");
    }

    #[test]
    fn fill_is_one_pass_and_keeps_unknown_placeholders() {
        assert_eq!(fill("{a} and {b}", &[("a", "{b}"), ("b", "x")]), "{b} and x");
        assert_eq!(fill("{missing} {a", &[("a", "1")]), "{missing} {a");
        assert_eq!(fill("no placeholders", &[]), "no placeholders");
        assert_eq!(fill("{n}{n}", &[("n", "é")]), "éé");
        // The Manager's rule: the name runs from the last `{` before the `}`.
        assert_eq!(fill("{a {b}", &[("b", "x")]), "{a x");
        assert_eq!(fill("{a {b}", &[("a {b", "x")]), "{a {b}");
    }

    #[test]
    fn natively_tr_is_english() {
        assert_eq!(tr("Click me"), "Click me");
        assert_eq!(tr_with("Clicked {n} times", &[("n", "2")]), "Clicked 2 times");
    }
}

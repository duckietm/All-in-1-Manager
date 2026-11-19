//! Texts for the user, as an English template plus values, so the plugin can translate them.
//!
//! The core can not depend on the plugin SDK, so it returns `Message { english, values }`: the English text with
//! `{name}` placeholders (the translation key) and the values to put in. [`Display`](std::fmt::Display) renders the
//! English text with the values filled in, which is what the log, the tests and the built-in's wording are; the
//! plugin translates `english` first and fills the values after (`tr_with`).
//!
//! Placeholders are filled in one pass, as the SDK's `tr_with` does: a name runs from a `{` to the next `}`, a name
//! with no value stays as written, and a value that holds `{x}` is never filled again. A value that is itself a
//! message ([`Message::with_message`]) is translated on its own and then put in.
//!
//! Files written into the rip folder (README.txt, the SQL and JSON headers) are not messages: they stay English.

use std::fmt;

/// An English text with `{name}` placeholders and what goes in them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Message {
    /// The template: the translation key. Always a literal at the `Message::new` call (the plugin's drift test
    /// reads those literals).
    pub english: &'static str,
    /// Plain values: names, numbers, paths, texts of the hotel or of the system. Never translated.
    pub values: Vec<(&'static str, String)>,
    /// Values that are messages themselves (translated on their own, then put in).
    pub parts: Vec<(&'static str, Message)>,
}

/// The template of [`Message::verbatim`]: a text that is not ours to translate.
pub const VERBATIM: &str = "{text}";

impl Message {
    pub fn new(english: &'static str) -> Message {
        Message {
            english,
            values: Vec::new(),
            parts: Vec::new(),
        }
    }

    /// A text that stays as it is: a system error, or what the hotel or the user's files said.
    pub fn verbatim(text: impl Into<String>) -> Message {
        Message {
            english: VERBATIM,
            values: vec![("text", text.into())],
            parts: Vec::new(),
        }
    }

    pub fn with(mut self, name: &'static str, value: impl ToString) -> Message {
        self.values.push((name, value.to_string()));
        self
    }

    pub fn with_message(mut self, name: &'static str, part: Message) -> Message {
        self.parts.push((name, part));
        self
    }

    /// The text with every part translated by `translate` (the template itself, then each nested message), the
    /// values filled in. `translate(english)` is the translation of a template, or the template.
    pub fn render(&self, translate: &dyn Fn(&str) -> String) -> String {
        let template = if self.english == VERBATIM {
            VERBATIM.to_string()
        } else {
            translate(self.english)
        };
        let parts: Vec<(&str, String)> = self
            .parts
            .iter()
            .map(|(name, part)| (*name, part.render(translate)))
            .collect();
        let mut values: Vec<(&str, &str)> = self.values.iter().map(|(n, v)| (*n, v.as_str())).collect();
        values.extend(parts.iter().map(|(n, v)| (*n, v.as_str())));
        fill(&template, &values)
    }
}

impl fmt::Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render(&|english| english.to_string()))
    }
}

/// `text` with each `{name}` that has a value replaced, in one pass.
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
    fn messages_render_english_by_default() {
        let m = Message::new("{a} of {b}: {c}")
            .with("a", 3)
            .with("b", "x")
            .with_message("c", Message::new("{n} pages").with("n", 2));
        assert_eq!(m.to_string(), "3 of x: 2 pages");
        assert_eq!(Message::new("plain").to_string(), "plain");
    }

    #[test]
    fn fill_is_one_pass_and_keeps_unknown_placeholders() {
        let m = Message::new("{a} {b} {missing} {a").with("a", "{b}").with("b", "x");
        assert_eq!(m.to_string(), "{b} x {missing} {a");
    }

    #[test]
    fn rendering_translates_the_template_and_the_parts_but_not_values_or_verbatim() {
        let italian = |english: &str| match english {
            "{n} pages" => "{n} pagine".to_string(),
            "Read {what}" => "Letto {what}".to_string(),
            other => other.to_string(),
        };
        let m = Message::new("Read {what}").with_message("what", Message::new("{n} pages").with("n", 2));
        assert_eq!(m.render(&italian), "Letto 2 pagine");
        assert_eq!(Message::verbatim("Read {what}").render(&italian), "Read {what}");
    }
}

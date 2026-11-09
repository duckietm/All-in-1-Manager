//! Translates catalogue page texts with Google Translate, as Octane's page editor does (the emulator's
//! `GoogleTranslateManager`: the free `translate_a/single` endpoint, the source language detected). Page texts are
//! HTML: only the text between the tags is translated, so links, images and formatting stay as they are.

use crate::server::{Result, ServerError};

const ENDPOINT: &str = "https://translate.googleapis.com/translate_a/single";
/// The emulator sends at most this many characters at once; longer text goes in parts.
const MAX_PART: usize = 500;

/// The languages Google's free endpoint takes (the emulator's list): code and name.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("af", "Afrikaans"), ("sq", "Albanian"), ("am", "Amharic"), ("ar", "Arabic"), ("hy", "Armenian"), ("az", "Azerbaijani"), ("eu", "Basque"),
    ("be", "Belarusian"), ("bn", "Bengali"), ("bs", "Bosnian"), ("bg", "Bulgarian"), ("ca", "Catalan"), ("ceb", "Cebuano"), ("ny", "Chichewa"),
    ("zh-CN", "Chinese (Simplified)"), ("zh-TW", "Chinese (Traditional)"), ("co", "Corsican"), ("hr", "Croatian"), ("cs", "Czech"), ("da", "Danish"),
    ("nl", "Dutch"), ("en", "English"), ("eo", "Esperanto"), ("et", "Estonian"), ("tl", "Filipino"), ("fi", "Finnish"), ("fr", "French"), ("fy", "Frisian"),
    ("gl", "Galician"), ("ka", "Georgian"), ("de", "German"), ("el", "Greek"), ("gu", "Gujarati"), ("ht", "Haitian Creole"), ("ha", "Hausa"),
    ("haw", "Hawaiian"), ("iw", "Hebrew"), ("hi", "Hindi"), ("hmn", "Hmong"), ("hu", "Hungarian"), ("is", "Icelandic"), ("ig", "Igbo"), ("id", "Indonesian"),
    ("ga", "Irish"), ("it", "Italian"), ("ja", "Japanese"), ("jw", "Javanese"), ("kn", "Kannada"), ("kk", "Kazakh"), ("km", "Khmer"), ("rw", "Kinyarwanda"),
    ("ko", "Korean"), ("ku", "Kurdish"), ("ky", "Kyrgyz"), ("lo", "Lao"), ("la", "Latin"), ("lv", "Latvian"), ("lt", "Lithuanian"), ("lb", "Luxembourgish"),
    ("mk", "Macedonian"), ("mg", "Malagasy"), ("ms", "Malay"), ("ml", "Malayalam"), ("mt", "Maltese"), ("mi", "Maori"), ("mr", "Marathi"), ("mn", "Mongolian"),
    ("my", "Myanmar"), ("ne", "Nepali"), ("no", "Norwegian"), ("or", "Odia"), ("ps", "Pashto"), ("fa", "Persian"), ("pl", "Polish"), ("pt", "Portuguese"),
    ("pa", "Punjabi"), ("ro", "Romanian"), ("ru", "Russian"), ("sm", "Samoan"), ("gd", "Scots"), ("sr", "Serbian"), ("st", "Sesotho"), ("sn", "Shona"),
    ("sd", "Sindhi"), ("si", "Sinhala"), ("sk", "Slovak"), ("sl", "Slovenian"), ("so", "Somali"), ("es", "Spanish"), ("su", "Sundanese"), ("sw", "Swahili"),
    ("sv", "Swedish"), ("tg", "Tajik"), ("ta", "Tamil"), ("tt", "Tatar"), ("te", "Telugu"), ("th", "Thai"), ("tr", "Turkish"), ("tk", "Turkmen"),
    ("uk", "Ukrainian"), ("ur", "Urdu"), ("ug", "Uyghur"), ("uz", "Uzbek"), ("vi", "Vietnamese"), ("cy", "Welsh"), ("xh", "Xhosa"), ("yi", "Yiddish"),
    ("yo", "Yoruba"), ("zu", "Zulu"),
];

/// The text split into tags (false) and the text between them (true).
pub fn segments(html: &str) -> Vec<(bool, String)> {
    let mut list = Vec::new();
    let mut current = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' if !in_tag => {
                if !current.is_empty() {
                    list.push((true, std::mem::take(&mut current)));
                }
                in_tag = true;
                current.push(c);
            }
            '>' if in_tag => {
                current.push(c);
                list.push((false, std::mem::take(&mut current)));
                in_tag = false;
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        list.push((!in_tag, current));
    }
    list
}

fn decode(text: &str) -> String {
    text.replace("&nbsp;", "\u{a0}").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

fn encode(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('\u{a0}', "&nbsp;")
}

/// Worth sending: it has letters.
fn has_words(text: &str) -> bool {
    text.chars().any(char::is_alphabetic)
}

/// Parts of at most [`MAX_PART`] characters, cut after a sentence or a space where it can.
pub fn parts(text: &str) -> Vec<String> {
    let mut list = Vec::new();
    let mut rest: &str = text;
    while rest.chars().count() > MAX_PART {
        let limit = rest.char_indices().nth(MAX_PART).map(|(i, _)| i).unwrap_or(rest.len());
        let window = &rest[..limit];
        let cut = window.rfind(['.', '!', '?', '\n']).map(|i| i + 1).or_else(|| window.rfind(' ').map(|i| i + 1)).filter(|i| *i > 0).unwrap_or(limit);
        list.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    if !rest.is_empty() {
        list.push(rest.to_string());
    }
    list
}

/// Google's answer: the translated pieces in its first array.
pub fn read_answer(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let pieces = value.get(0)?.as_array()?;
    Some(pieces.iter().filter_map(|p| p.get(0).and_then(|t| t.as_str())).collect())
}

async fn translate_part(client: &reqwest::Client, text: &str, target: &str) -> Result<String> {
    let response = client
        .get(ENDPOINT)
        .query(&[("client", "gtx"), ("sl", "auto"), ("tl", target), ("dt", "t"), ("q", text)])
        .send()
        .await
        .map_err(|e| ServerError(format!("Google Translate could not be reached: {e}")))?;
    if !response.status().is_success() {
        return Err(ServerError(format!("Google Translate answered {}.", response.status())).into());
    }
    let body = response.text().await.map_err(|e| ServerError(e.to_string()))?;
    read_answer(&body).ok_or_else(|| ServerError("Google Translate sent an answer that cannot be read.".into()).into())
}

/// The text (plain or page HTML) in `target` (a code of [`LANGUAGES`]): the tags kept, the text between them translated.
pub async fn translate(text: &str, target: &str) -> Result<String> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build().map_err(|e| ServerError(e.to_string()))?;
    let mut out = String::new();
    for (is_text, segment) in segments(text) {
        if !is_text || !has_words(&segment) {
            out.push_str(&segment);
            continue;
        }
        // The spaces around the words stay where they were.
        let plain = decode(&segment);
        let start = plain.len() - plain.trim_start().len();
        let end = plain.trim_end().len();
        let mut translated = String::new();
        for part in parts(&plain[start..end]) {
            translated.push_str(&translate_part(&client, &part, target).await?);
        }
        out.push_str(&encode(&plain[..start]));
        out.push_str(&encode(&translated));
        out.push_str(&encode(&plain[end..]));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_text_between_tags_is_translated() {
        let parts = segments("<b>Hello</b> world <a href=\"event:x\">here</a>");
        assert_eq!(
            parts,
            vec![
                (false, "<b>".into()),
                (true, "Hello".into()),
                (false, "</b>".into()),
                (true, " world ".into()),
                (false, "<a href=\"event:x\">".into()),
                (true, "here".into()),
                (false, "</a>".into()),
            ]
        );
        assert!(!has_words(" 123 - "));
    }

    #[test]
    fn long_text_goes_in_parts() {
        let text = "One sentence here. ".repeat(60);
        let pieces = parts(&text);
        assert!(pieces.len() > 1);
        assert!(pieces.iter().all(|p| p.chars().count() <= MAX_PART));
        assert_eq!(pieces.concat(), text);
    }

    #[test]
    fn googles_answer_is_read() {
        assert_eq!(read_answer(r#"[[["Hallo ","Hello ",null,null,10],["wereld","world",null,null,10]],null,"en"]"#).as_deref(), Some("Hallo wereld"));
        assert_eq!(read_answer("nonsense"), None);
    }
}

//! The client's texts (ExternalTexts.json in gamedata/config) in the language of one of the official Habbo hotels,
//! ported from the All-in-1 Converter's Download Classes/Flash_Texts.cs: external_flash_texts downloaded and turned
//! into JSON. Texts the hotel has but the official file does not (the emulator writes the names of custom badges
//! there, hotels add their own) stay.

use serde_json::{Map, Value};

use crate::furniture_language::{self, FurniLanguage};
use crate::script::{self, fill};
use crate::server::{shell_quote, Log, Result, Server, ServerError};
use crate::{octane, server_tools};

/// Where the converter downloads the texts (Tools/HabboHotels.cs: externaltexturl, without www).
pub fn url(language: FurniLanguage) -> String {
    format!("https://{}/gamedata/external_flash_texts/1", language.domain)
}

pub fn file() -> String {
    format!("{}/config/ExternalTexts.json", octane::gamedata())
}

/// The texts file (`key=value` lines) as JSON, like ConvertTextsToJsonAsync: blank lines and # comments skipped, split
/// at the first =, both sides trimmed, a later key replacing an earlier one (in the earlier one's place).
pub fn to_json(text: &str) -> Value {
    let mut texts = Map::new();
    for line in text.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let Some(at) = line.find('=').filter(|&at| at > 0) else { continue };
        let key = line[..at].trim();
        if key.is_empty() {
            continue;
        }
        texts.insert(key.to_string(), Value::String(line[at + 1..].trim().to_string()));
    }
    Value::Object(texts)
}

/// Adds the texts of `hotel` that `official` does not have. Returns how many.
pub fn keep_hotel_texts(official: &mut Value, hotel: &Value) -> usize {
    let (Some(official), Some(hotel)) = (official.as_object_mut(), hotel.as_object()) else { return 0 };
    let mut added = 0;
    for (key, value) in hotel {
        if !official.contains_key(key) {
            official.insert(key.clone(), value.clone());
            added += 1;
        }
    }
    added
}

/// Downloaded and converted texts, with the hotel's own added: ready to put in place.
#[derive(Debug, Clone)]
pub struct PreparedTexts {
    pub language: FurniLanguage,
    pub texts: Value,
    pub kept: usize,
}

impl PreparedTexts {
    pub fn count(&self) -> usize {
        self.texts.as_object().map(Map::len).unwrap_or(0)
    }
}

/// Downloads the hotel's texts (on this computer), converts them and adds the server's own texts that they do not
/// have. Nothing on the server changes.
pub async fn prepare(server: &dyn Server, language: FurniLanguage, log: Log<'_>) -> Result<PreparedTexts> {
    let url = url(language);
    log(&format!("> Downloading the texts of {} ({url})", language.domain));
    let text = furniture_language::download(&url).await?;
    let mut texts = to_json(&text);
    let official = texts.as_object().map(Map::len).unwrap_or(0);
    if official == 0 {
        return Err(ServerError(format!("{url} has no texts (key=value lines).")).into());
    }
    log(&format!("  {official} texts"));
    let kept = match current(server).await? {
        Some(hotel) => keep_hotel_texts(&mut texts, &hotel),
        None => 0,
    };
    if kept > 0 {
        log(&format!("  {kept} texts of the hotel that {} does not have are kept (custom badges and the like)", language.domain));
    }
    Ok(PreparedTexts { language, texts, kept })
}

/// The ExternalTexts.json on the server now, or None.
pub async fn current(server: &dyn Server) -> Result<Option<Value>> {
    let result = server.run_as_root(&format!("cat {} 2>/dev/null", shell_quote(&file()))).await?;
    Ok(serde_json::from_str(result.output.trim_start_matches('\u{feff}')).ok())
}

const PUT: &str = r#"json={{JsonFile}}
config={{Gamedata}}/config
trap 'rm -f "$json"' EXIT
[ -s "$json" ] || fail "The uploaded ExternalTexts.json is missing or empty."
echo "> $config/ExternalTexts.json ({{Language}})"
install -d -o www-data -g www-data -m 2775 "$config"
if [ -f "$config/ExternalTexts.json" ]; then
  old="$config/ExternalTexts.json.bak-$(date +%Y%m%d-%H%M%S)"
  cp -p "$config/ExternalTexts.json" "$old"
  echo "  The old one is kept as $old"
  echo "OLD=$old"
fi
# Group www-data may write: the emulator adds the names of custom badges.
install -o www-data -g www-data -m 664 "$json" "$config/ExternalTexts.json""#;

/// Puts the prepared texts in gamedata/config (the old file kept as .bak-<time>). Returns the old file's copy, or
/// None when there was none. Meant for while the emulator is stopped, because it writes to this file too.
pub async fn put(server: &dyn Server, prepared: &PreparedTexts, log: Log<'_>) -> Result<Option<String>> {
    let language = prepared.language;
    let local = std::env::temp_dir().join(format!("all-in-1-texts-{}-{}.json", std::process::id(), chrono::Local::now().format("%Y%m%d-%H%M%S%f")));
    std::fs::write(&local, serde_json::to_string_pretty(&prepared.texts).unwrap_or_default())?;
    let directory = server_tools::backup_directory();
    let remote = format!("{directory}/texts-{}-{}.json", language.code, chrono::Local::now().format("%Y%m%d-%H%M%S"));
    let uploaded = async {
        server.run_as_root(&format!("install -d -m 700 {directory}")).await?;
        server.upload(&local, &remote, &|_| {}).await
    }
    .await;
    let _ = std::fs::remove_file(&local);
    uploaded?;
    let body = fill(
        PUT,
        &[
            ("Gamedata", &shell_quote(&octane::gamedata())),
            ("Language", &format!("{}, {}", language.name, language.domain)),
            ("JsonFile", &shell_quote(&remote)),
        ],
    );
    let values = script::run_capturing(server, &script::with_prelude(&body), log, "Putting ExternalTexts.json in place failed", &["OLD"], None).await?;
    log(&format!("  {} texts", prepared.count()));
    Ok(values.string("OLD"))
}

/// Puts ExternalTexts.json back: the `old` copy, or no file when there was none before `put`.
pub async fn restore(server: &dyn Server, old: Option<&str>, log: Log<'_>) -> Result<()> {
    let target = shell_quote(&file());
    let command = match old {
        Some(old) => format!("cp -p {} {target} && echo '  ExternalTexts.json as it was'", shell_quote(old)),
        None => format!("rm -f {target} && echo '  ExternalTexts.json removed again (there was none)'"),
    };
    let result = server.run_as_root(&command).await?;
    log(result.output.trim_end());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn texts_like_the_converter() {
        let texts = to_json("# comment\n\nbadge_name_ACH_Login1 = Logged in\r\n  # also a comment\nnotitle\n=novalue\nurl=https://a.b/c?d=e\nbadge_name_ACH_Login1=Again\nempty=\n");
        let map = texts.as_object().unwrap();
        assert_eq!(map.len(), 3);
        assert_eq!(map["badge_name_ACH_Login1"], "Again", "a later key replaces an earlier one");
        assert_eq!(map.keys().next().unwrap(), "badge_name_ACH_Login1", "in the earlier one's place");
        assert_eq!(map["url"], "https://a.b/c?d=e", "split at the first =");
        assert_eq!(map["empty"], "");
    }

    #[test]
    fn hotel_texts_stay() {
        let mut official = json!({ "a": "official", "b": "B" });
        let hotel = json!({ "a": "hotel", "badge_name_CUSTOM1": "My badge" });
        assert_eq!(keep_hotel_texts(&mut official, &hotel), 1);
        assert_eq!(official["a"], "official");
        assert_eq!(official["badge_name_CUSTOM1"], "My badge");
        assert_eq!(url(furniture_language::language("nl").unwrap()), "https://habbo.nl/gamedata/external_flash_texts/1");
    }
}

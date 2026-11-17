//! Names derived from a hotel host or a profile name.

/// A file name that is safe on every platform. (`images::safe_file_name` in the built-in crate.)
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').to_string();
    if cleaned.is_empty() {
        "_".into()
    } else {
        cleaned
    }
}

/// `Catalog/<hotel>` name for a game host: game-nl.habbo.com -> habbo.nl, game-us -> habbo.com,
/// game-br -> habbo.com.br, game-tr -> habbo.com.tr, anything else the host itself.
/// (`rip::hotel_name` in the built-in crate.)
pub fn hotel_name(host: &str) -> String {
    let host = host.trim().to_ascii_lowercase();
    let code = host
        .strip_prefix("game-")
        .and_then(|rest| rest.split_once('.'))
        .filter(|(_, domain)| domain.starts_with("habbo."))
        .map(|(code, _)| code);
    let name = match code {
        Some("us") => "habbo.com".to_string(),
        Some("br") => "habbo.com.br".to_string(),
        Some("tr") => "habbo.com.tr".to_string(),
        Some(code) => format!("habbo.{code}"),
        None if host.is_empty() => "hotel".to_string(),
        None => host,
    };
    safe_file_name(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotel_names() {
        assert_eq!(hotel_name("game-nl.habbo.com"), "habbo.nl");
        assert_eq!(hotel_name("game-us.habbo.com"), "habbo.com");
        assert_eq!(hotel_name("game-br.habbo.com"), "habbo.com.br");
        assert_eq!(hotel_name("game-tr.habbo.com"), "habbo.com.tr");
        assert_eq!(hotel_name("127.0.0.1"), "127.0.0.1");
        assert_eq!(hotel_name("my:retro"), "my_retro");
        assert_eq!(hotel_name(""), "hotel");
    }
}

//! The emulator's RCON: one JSON request `{"key": ..., "data": {...}}` sent to 127.0.0.1:`rcon.port` (config.ini,
//! default 3001) on the server, through the SSH session; the emulator answers `{"status": N, "message": "..."}`.
//!
//! The request is built with serde_json (so names, messages and badge codes are escaped as JSON) and handed to the
//! shell as one single-quoted word (so nothing in it is read by the shell).

use serde_json::Value;

use crate::server::{shell_quote, Result, Server, ServerError};

/// The emulator's answer codes (`RCONMessage`).
pub const STATUS_OK: i64 = 0;
pub const STATUS_ERROR: i64 = 1;
pub const USER_NOT_FOUND: i64 = 2;
pub const ROOM_NOT_FOUND: i64 = 3;
pub const SYSTEM_ERROR: i64 = 4;

/// The request for `key` with `data` as its JSON object.
pub fn request(key: &str, data: Value) -> String {
    serde_json::json!({ "key": key, "data": data }).to_string()
}

/// Polaris' `reloadcontent`: loads one kind of content again, as its in-game `:update_*` command does.
pub const RELOAD_CONTENT: &str = "reloadcontent";

/// What the emulator answers for a key it does not know (an emulator from before a command was added).
pub const UNKNOWN_COMMAND: &str = "unknown command";

/// The `reloadcontent` request for `target` (`achievements`, `navigator`, `hotelview`, ...).
pub fn reload_content_request(target: &str) -> String {
    request(RELOAD_CONTENT, serde_json::json!({ "target": target }))
}

/// Whether the emulator does not know the command it was sent.
pub fn is_unknown_command(answer: &Answer) -> bool {
    answer.status == STATUS_ERROR && answer.message == UNKNOWN_COMMAND
}

/// A script that sends `request` to the RCON port of the emulator on 127.0.0.1 and prints the answer.
pub fn script(request: &str) -> String {
    format!(
        r#"port=$(grep -sE '^rcon.port=' {dir}/config.ini | cut -d= -f2 | tr -d '[:space:]') || true
port=${{port:-3001}}
printf '%s' {request} | timeout 20 bash -c "exec 3<>/dev/tcp/127.0.0.1/$port && cat >&3 && cat <&3"
"#,
        dir = crate::emulator::directory(),
        request = shell_quote(request)
    )
}

/// What the emulator answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub status: i64,
    pub message: String,
}

impl Answer {
    pub fn ok(&self) -> bool {
        self.status == STATUS_OK
    }
}

/// The answer in the script's output: the last line that is a JSON object with a status.
pub fn parse(output: &str) -> Option<Answer> {
    let text = output.trim();
    let candidates = std::iter::once(text).chain(text.lines().rev().map(str::trim));
    for candidate in candidates {
        let Some(start) = candidate.find('{') else { continue };
        let Some(end) = candidate.rfind('}') else { continue };
        if end < start {
            continue;
        }
        if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(&candidate[start..=end]) {
            if let Some(status) = object.get("status").and_then(Value::as_i64) {
                let message = object.get("message").and_then(Value::as_str).unwrap_or("").to_string();
                return Some(Answer { status, message });
            }
        }
    }
    None
}

/// Whether the answer says the request worked (`"status":0`).
pub fn succeeded(output: &str) -> bool {
    parse(output).is_some_and(|a| a.ok())
}

/// Sends `request` and returns the emulator's answer; an error when nothing answered (the emulator is not running,
/// or RCON does not listen on 127.0.0.1).
pub async fn send(server: &dyn Server, request: &str) -> Result<Answer> {
    let result = server.run_as_root(&script(request)).await?;
    if let Some(answer) = parse(&result.output) {
        return Ok(answer);
    }
    let detail = if result.output.trim().is_empty() { result.error.trim().to_string() } else { result.output.trim().to_string() };
    Err(ServerError(format!(
        "The emulator did not answer on RCON (is it running, and is RCON on 127.0.0.1?): {}",
        if detail.is_empty() { "no answer".to_string() } else { detail }
    ))
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reload_content_names_its_target() {
        assert_eq!(reload_content_request("petdata"), r#"{"key":"reloadcontent","data":{"target":"petdata"}}"#);
    }

    #[test]
    fn an_older_emulator_does_not_know_the_command() {
        assert!(is_unknown_command(&Answer { status: STATUS_ERROR, message: "unknown command".into() }));
        assert!(!is_unknown_command(&Answer { status: STATUS_ERROR, message: "Unknown target. Use one of: polls".into() }));
        assert!(!is_unknown_command(&Answer { status: STATUS_OK, message: String::new() }));
    }

    #[test]
    fn requests_are_json_and_one_shell_word() {
        let request = request("alertuser", serde_json::json!({ "user_id": 7, "message": "it's \"fine\"\n$(rm -rf /) `x`" }));
        let parsed: Value = serde_json::from_str(&request).unwrap();
        assert_eq!(parsed["key"], "alertuser");
        assert_eq!(parsed["data"]["message"], "it's \"fine\"\n$(rm -rf /) `x`");
        let script = script(&request);
        assert!(script.contains(&shell_quote(&request)));
        assert!(script.contains("/dev/tcp/127.0.0.1/$port"));
        assert!(script.contains("port=${port:-3001}"));
    }

    #[test]
    fn answers_are_read() {
        assert_eq!(parse(r#"{"status":0,"message":""}"#), Some(Answer { status: 0, message: String::new() }));
        assert_eq!(parse("noise\n{ \"status\" : 2, \"message\" : \"user not found\" }\n").map(|a| a.status), Some(USER_NOT_FOUND));
        assert_eq!(parse(r#"{"status":0,"message":"offline"}"#).unwrap().message, "offline");
        assert!(succeeded("{ \"status\" : 0 }"));
        assert!(!succeeded(r#"{"status":1,"message":"invalid request"}"#));
        assert!(!succeeded(""));
        assert!(parse("{not json}").is_none());
    }
}

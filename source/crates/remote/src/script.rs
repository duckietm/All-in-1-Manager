//! Runs install scripts as root with live output. Every install script starts with [`PRELUDE`].

use crate::server::{Log, Result, Server, ServerError};

/// Prepended to every install script (`set -e`, `fail`, `fix_clock`, `apt_update`, `download`, `compose_in`,
/// `ufw_allow`, `load_cloudflare_ranges`). Ends with a newline, so whatever is appended starts on its own line.
pub const PRELUDE: &str = include_str!("prelude.sh");

/// `PRELUDE` followed by `body`, the way the C# installer writes `RemoteScript.Prelude + """..."""`.
pub fn with_prelude(body: &str) -> String {
    let mut script = String::with_capacity(PRELUDE.len() + body.len());
    script.push_str(PRELUDE);
    script.push_str(body);
    script
}

/// Fills `{{name}}` placeholders in a script template. The templates are the C# installer's `$$"""` scripts with
/// their interpolations named, so the text stays the same and bash's own braces need no escaping.
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut text = template.to_string();
    for (name, value) in values {
        text = text.replace(&format!("{{{{{name}}}}}"), value);
    }
    text
}

/// A string literal for MariaDB, with backslash escaping as MariaDB uses by default.
pub fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// Runs a script as root, logging every line indented by two spaces. On a non-zero exit, fails with
/// "`failure`: <the last fail() message, or else the last output line>".
pub async fn run(server: &dyn Server, script: &str, log: Log<'_>, failure: &str, stdin: Option<&str>) -> Result<()> {
    run_with(server, script, &mut |line: &str| log(&format!("  {line}")), failure, stdin).await
}

/// Like [`run`], but `on_line` gets the raw lines (not indented).
pub async fn run_with(server: &dyn Server, script: &str, on_line: &mut (dyn FnMut(&str) + Send), failure: &str, stdin: Option<&str>) -> Result<()> {
    let mut error: Option<String> = None;
    let mut last: Option<String> = None;
    let exit_code = server
        .run_as_root_streaming(
            script,
            &mut |line: &str| {
                if let Some(message) = line.strip_prefix("ERROR: ") {
                    error = Some(message.to_string());
                }
                if !line.trim().is_empty() {
                    last = Some(line.trim().to_string());
                }
                on_line(line);
            },
            stdin,
        )
        .await?;
    if exit_code != 0 {
        let reason = error.or(last).unwrap_or_else(|| format!("exit code {exit_code}"));
        return Err(ServerError(format!("{failure}: {reason}")).into());
    }
    Ok(())
}

/// Like [`run`], but returns the "KEY=value" lines for `keys` instead of logging them.
pub async fn run_capturing(
    server: &dyn Server,
    script: &str,
    log: Log<'_>,
    failure: &str,
    keys: &[&str],
    stdin: Option<&str>,
) -> Result<ScriptValues> {
    let mut captured = Vec::new();
    run_with(
        server,
        script,
        &mut |line: &str| {
            let text = line.trim();
            if keys.iter().any(|key| text.starts_with(&format!("{key}="))) {
                captured.push(text.to_string());
            } else {
                log(&format!("  {line}"));
            }
        },
        failure,
        stdin,
    )
    .await?;
    Ok(ScriptValues::from_lines(captured.iter().map(String::as_str)))
}

/// Runs a short command as root and fails with "`failure`: <first error line>" when it does not succeed. Returns
/// its output.
pub async fn root_output(server: &dyn Server, command: &str, failure: &str) -> Result<String> {
    let result = server.run_as_root(command).await?;
    if !result.succeeded() {
        return Err(ServerError(format!("{failure}: {}", result.error.trim())).into());
    }
    Ok(result.output)
}

/// "key=value" lines printed by a script. Empty values count as missing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScriptValues {
    values: std::collections::HashMap<String, String>,
}

impl ScriptValues {
    pub fn parse(output: &str) -> Self {
        Self::from_lines(output.split('\n'))
    }

    pub fn from_lines<'a>(lines: impl IntoIterator<Item = &'a str>) -> Self {
        let mut values = std::collections::HashMap::new();
        for line in lines {
            if let Some(equals) = line.find('=').filter(|&e| e > 0) {
                values.insert(line[..equals].trim().to_string(), line[equals + 1..].trim().to_string());
            }
        }
        Self { values }
    }

    /// The value, or None when it is missing or empty.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str).filter(|v| !v.is_empty())
    }

    /// The value as an owned string, or None.
    pub fn string(&self, key: &str) -> Option<String> {
        self.get(key).map(str::to_string)
    }

    /// The value, or "" when it is missing.
    pub fn or_empty(&self, key: &str) -> &str {
        self.get(key).unwrap_or("")
    }

    pub fn yes(&self, key: &str) -> bool {
        self.get(key) == Some("yes")
    }

    /// The value as an integer, or 0 (C#'s int.TryParse).
    pub fn int(&self, key: &str) -> i64 {
        self.get(key).and_then(|v| v.parse::<i32>().ok()).map(i64::from).unwrap_or(0)
    }

    /// The value as a 64-bit integer, or 0 (C#'s long.TryParse).
    pub fn long(&self, key: &str) -> i64 {
        self.get(key).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values() {
        let v = ScriptValues::parse("a=1\nb=\n c = yes \nnoequals\n=x\nd=2=3");
        assert_eq!(v.get("a"), Some("1"));
        assert_eq!(v.get("b"), None);
        assert!(v.yes("c"));
        assert_eq!(v.get("d"), Some("2=3"));
        assert_eq!(v.int("a"), 1);
        assert_eq!(v.int("c"), 0);
    }

    #[test]
    fn templates_and_literals() {
        assert_eq!(fill("a {{x}} ${y} {{x}}", &[("x", "1")]), "a 1 ${y} 1");
        assert_eq!(sql_literal("it's a \\ test"), "'it\\'s a \\\\ test'");
        assert!(PRELUDE.starts_with("set -e\n"));
        assert!(PRELUDE.ends_with("}\n"));
    }
}

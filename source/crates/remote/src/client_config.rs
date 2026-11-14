//! Octane's runtime configuration (`<octane>/public/configuration/*.json`): renderer-config, ui-config, client-mode,
//! news and the rest, read and written over SSH. A save goes to `public/configuration` (what the next build copies)
//! and to `dist/configuration` (what the hotel serves, with `Cache-Control: no-store`), so players get it at their next
//! reload without a build. Like the gamedata editor, a save first checks the file was not changed on the server since
//! it was loaded, and keeps the old one as `.bak-<time>`.

use serde_json::Value;

use crate::gamedata_admin::{detect_layout, sha256, write_json, Layout};
use crate::script::{self, fill};
use crate::server::{shell_quote, Log, Result, Server, ServerError};
use crate::{octane, server_tools};

/// A configuration file on the server.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConfigFile {
    /// The file name, e.g. `ui-config.json`.
    pub name: String,
    /// Bytes of the file in public/configuration; None when only its `.example` is there.
    pub size: Option<u64>,
    /// An `.example` of it is there (what this Octane version ships).
    pub example: bool,
    /// The hotel serves it (dist/configuration has it).
    pub served: bool,
}

impl ConfigFile {
    /// JSON with comments: only edited as text, so the comments stay.
    pub fn is_jsonc(&self) -> bool {
        self.name.ends_with(".jsonc")
    }
}

/// A plain file name of the configuration folder: letters, digits, `.`, `_`, `-`, ending in .json or .jsonc.
pub fn valid_name(name: &str) -> bool {
    !name.starts_with('.')
        && (name.ends_with(".json") || name.ends_with(".jsonc"))
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// The file an `.example` becomes: `ui-config.example` -> `ui-config.json`, `radio-stations.jsonc.example` ->
/// `radio-stations.jsonc`. None for examples that are not JSON (`ads-frame.html.example`) or name a language copy of a
/// file the gamedata layout handles (`UITexts_*.jsonc.example`).
pub fn example_target(example: &str) -> Option<String> {
    let base = example.strip_suffix(".example")?;
    if base.starts_with("UITexts") {
        return None;
    }
    let name = if base.ends_with(".json") || base.ends_with(".jsonc") {
        base.to_string()
    } else if base.contains('.') {
        return None;
    } else {
        format!("{base}.json")
    };
    valid_name(&name).then_some(name)
}

/// The example a file is made from: `ui-config.json` -> `ui-config.example`, `x.jsonc` -> `x.jsonc.example`.
pub fn example_of(name: &str) -> String {
    match name.strip_suffix(".json") {
        Some(base) => format!("{base}.example"),
        None => format!("{name}.example"),
    }
}

/// The examples a file may have, the likeliest first: a hotel that keeps `ui-config.jsonc` still has Octane's
/// `ui-config.example`, and the other way round.
pub fn examples_of(name: &str) -> Vec<String> {
    let stem = name.trim_end_matches(".jsonc").trim_end_matches(".json");
    let mut list = vec![example_of(name), format!("{stem}.example"), format!("{stem}.jsonc.example")];
    list.dedup();
    let mut seen = std::collections::HashSet::new();
    list.retain(|e| seen.insert(e.clone()));
    list
}

fn public() -> String {
    octane::configuration_directory()
}

fn served() -> String {
    format!("{}/configuration", octane::dist())
}

/// Reads the listing script's output: `P <name> <size>` for public files, `E <name>` for examples, `D <name>` for
/// served ones.
pub fn parse_listing(output: &str) -> Vec<ConfigFile> {
    fn entry(files: &mut Vec<ConfigFile>, name: &str) -> usize {
        match files.iter().position(|f| f.name == name) {
            Some(i) => i,
            None => {
                files.push(ConfigFile { name: name.to_string(), ..ConfigFile::default() });
                files.len() - 1
            }
        }
    }
    let mut files: Vec<ConfigFile> = Vec::new();
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next(), parts.next()) {
            (Some("P"), Some(name), size) if valid_name(name) => {
                let i = entry(&mut files, name);
                files[i].size = Some(size.and_then(|s| s.parse().ok()).unwrap_or(0));
            }
            (Some("E"), Some(example), _) => {
                if let Some(name) = example_target(example) {
                    let i = entry(&mut files, &name);
                    files[i].example = true;
                }
            }
            (Some("D"), Some(name), _) if valid_name(name) => {
                if let Some(i) = files.iter().position(|f| f.name == name) {
                    files[i].served = true;
                }
            }
            _ => {}
        }
    }
    // A hotel that keeps ui-config.jsonc: Octane's ui-config.example is its example, not a second file to make.
    fn stem(name: &str) -> &str {
        name.trim_end_matches(".jsonc").trim_end_matches(".json")
    }
    let made: Vec<String> = files.iter().filter(|f| f.size.is_some()).map(|f| stem(&f.name).to_string()).collect();
    let examples: Vec<String> = files.iter().filter(|f| f.example).map(|f| stem(&f.name).to_string()).collect();
    files.retain(|f| f.size.is_some() || !made.iter().any(|m| m == stem(&f.name)));
    for file in files.iter_mut() {
        if examples.iter().any(|e| e == stem(&file.name)) {
            file.example = true;
        }
    }
    files.sort_by(|a, b| a.name.cmp(&b.name));
    files
}

const LIST: &str = r#"p={{Public}}
d={{Served}}
[ -d "$p" ] || exit 0
find "$p" -maxdepth 1 -type f \( -name '*.json' -o -name '*.jsonc' \) -printf 'P %f %s\n'
find "$p" -maxdepth 1 -type f -name '*.example' -printf 'E %f\n'
[ -d "$d" ] && find "$d" -maxdepth 1 -type f \( -name '*.json' -o -name '*.jsonc' \) -printf 'D %f\n'
true"#;

/// The configuration files: the ones there, and the ones this Octane version has an example of.
pub async fn list(server: &dyn Server) -> Result<Vec<ConfigFile>> {
    let body = fill(LIST, &[("Public", &shell_quote(&public())), ("Served", &shell_quote(&served()))]);
    let output = script::root_output(server, &body, "Listing Octane's configuration failed").await?;
    Ok(parse_listing(&output))
}

/// A file as loaded: its text, how it was written, its checksum then (empty: not made yet, the text is its example).
#[derive(Clone, Debug)]
pub struct Loaded {
    pub name: String,
    pub text: String,
    pub layout: Layout,
    pub sha256: String,
    /// The example of this Octane version, when there is one (for the settings it adds).
    pub example: Option<String>,
}

impl Loaded {
    pub fn is_new(&self) -> bool {
        self.sha256.is_empty()
    }
}

async fn read(server: &dyn Server, path: &str) -> Result<Option<Vec<u8>>> {
    server.read_file(path, 8 << 20).await
}

/// Loads a file (or, when it is not made yet, its example) and the example beside it.
pub async fn load(server: &dyn Server, name: &str) -> Result<Loaded> {
    if !valid_name(name) {
        return Err(ServerError(format!("{name} is not a configuration file name.")).into());
    }
    let mut example = None;
    for candidate in examples_of(name) {
        if let Some(bytes) = read(server, &format!("{}/{candidate}", public())).await? {
            example = Some(String::from_utf8_lossy(&bytes).into_owned());
            break;
        }
    }
    match read(server, &format!("{}/{name}", public())).await? {
        Some(bytes) => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let layout = if name.ends_with(".jsonc") { jsonc_layout(&text) } else { detect_layout(&text) };
            Ok(Loaded { name: name.into(), layout, sha256: sha256(&bytes), text, example })
        }
        None => match &example {
            Some(text) => Ok(Loaded { name: name.into(), layout: detect_layout(text), sha256: String::new(), text: text.clone(), example }),
            None => Err(ServerError(format!("{name} is not on the server and this Octane version has no example of it.")).into()),
        },
    }
}

/// JSON with comments (`//`, `/* */`) and trailing commas, as plain JSON: for checking a .jsonc file.
pub fn strip_jsonc(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut in_string) = (0, false);
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match (c, chars.get(i + 1)) {
            ('"', _) => {
                in_string = true;
                out.push(c);
                i += 1;
            }
            ('/', Some('/')) => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            (',', _) => {
                // A comma before a closing bracket is dropped.
                let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
                if !matches!(next, Some('}') | Some(']')) {
                    out.push(c);
                }
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// The text as JSON (a .jsonc file's comments and trailing commas allowed), or why it is not.
pub fn parse(name: &str, text: &str) -> std::result::Result<Value, String> {
    let body = text.trim_start_matches('\u{feff}');
    let plain = if name.ends_with(".jsonc") || name.ends_with(".example") { strip_jsonc(body) } else { body.to_string() };
    serde_json::from_str(&plain).map_err(|e| format!("line {}, column {}: {}", e.line(), e.column(), short_error(&e)))
}

fn short_error(e: &serde_json::Error) -> String {
    let text = e.to_string();
    text.split(" at line ").next().unwrap_or(&text).to_string()
}

/// `value` written the way the file was (its indent, BOM and closing newline).
pub fn write(value: &Value, layout: &Layout) -> String {
    write_json(value, layout)
}

/// The comments of a .jsonc file, by the setting they belong to: the lines above a setting, a comment after its value
/// on the same line, and what is before and after the whole file. A path is the keys from the top, joined by "\u{1}".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comments {
    pub head: Vec<String>,
    pub before: std::collections::HashMap<String, Vec<String>>,
    pub after: std::collections::HashMap<String, String>,
    pub tail: Vec<String>,
}

enum Frame {
    Object { key: Option<String> },
    Array,
}

/// Finds the comments of a .jsonc text and the settings they belong to.
pub fn comments(text: &str) -> Comments {
    let chars: Vec<char> = text.trim_start_matches('\u{feff}').chars().collect();
    let mut found = Comments::default();
    let mut stack: Vec<Frame> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    // The setting whose value ended last, and whether a line break came since.
    let mut last: Option<String> = None;
    let mut newline_since = true;
    let mut started = false;
    let mut finished = false;
    let path_of = |stack: &[Frame], key: &str| -> String {
        // The keys of the objects around it (the innermost one is the object this key is in), then the key.
        let parents = &stack[..stack.len().saturating_sub(1)];
        let mut parts: Vec<String> = parents
            .iter()
            .filter_map(|f| match f {
                Frame::Object { key: Some(k) } => Some(k.clone()),
                _ => None,
            })
            .collect();
        parts.push(key.to_string());
        parts.join("\u{1}")
    };
    let current_path = |stack: &[Frame]| -> Option<String> {
        match stack.last() {
            Some(Frame::Object { key: Some(_) }) => {
                let parts: Vec<String> = stack.iter().filter_map(|f| match f {
                    Frame::Object { key: Some(k) } => Some(k.clone()),
                    _ => None,
                }).collect();
                Some(parts.join("\u{1}"))
            }
            _ => None,
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '/' if chars.get(i + 1) == Some(&'/') || chars.get(i + 1) == Some(&'*') => {
                let start = i;
                if chars[i + 1] == '/' {
                    while i < chars.len() && chars[i] != '\n' {
                        i += 1;
                    }
                } else {
                    i += 2;
                    while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                        i += 1;
                    }
                    i = (i + 2).min(chars.len());
                }
                let comment: String = chars[start..i].iter().collect::<String>().trim_end().to_string();
                match (&last, newline_since) {
                    (Some(path), false) if !comment.contains('\n') => {
                        found.after.insert(path.clone(), comment);
                    }
                    _ if !started => found.head.push(comment),
                    _ if finished => found.tail.push(comment),
                    _ => pending.push(comment),
                }
                continue;
            }
            '\n' => newline_since = true,
            '"' => {
                // A string: a key when a ':' follows in an object.
                let start = i + 1;
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                let raw: String = chars[start..i.min(chars.len())].iter().collect();
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                let is_key = chars.get(j) == Some(&':') && matches!(stack.last(), Some(Frame::Object { .. }));
                if is_key {
                    let key = serde_json::from_str::<String>(&format!("\"{raw}\"")).unwrap_or(raw);
                    let path = path_of(&stack, &key);
                    if !pending.is_empty() {
                        found.before.insert(path, std::mem::take(&mut pending));
                    }
                    if let Some(Frame::Object { key: slot }) = stack.last_mut() {
                        *slot = Some(key);
                    }
                } else {
                    last = current_path(&stack);
                    newline_since = false;
                }
            }
            '{' => {
                started = true;
                stack.push(Frame::Object { key: None });
            }
            '[' => {
                started = true;
                stack.push(Frame::Array);
            }
            '}' | ']' => {
                stack.pop();
                // Comments left at the end of an object or list belong to nothing: they go before its end, kept as
                // the comments of its last setting.
                pending.clear();
                last = current_path(&stack);
                newline_since = false;
                if stack.is_empty() {
                    finished = true;
                    last = None;
                }
            }
            ',' => {}
            c if !c.is_whitespace() && !matches!(c, ':') => {
                // A number, true, false or null: the value of the current setting.
                while i + 1 < chars.len() && !matches!(chars[i + 1], ',' | '}' | ']' | '\n' | ' ' | '\t' | '\r' | '/') {
                    i += 1;
                }
                last = current_path(&stack);
                newline_since = false;
            }
            _ => {}
        }
        i += 1;
    }
    found
}

/// `value` written as `layout` says, with the comments put back: above their setting, after its value, and around the
/// whole file. Settings that are new have none; comments of settings that are gone are left out.
pub fn write_with_comments(value: &Value, layout: &Layout, comments: &Comments) -> String {
    let indent = layout.indent.clone().unwrap_or_else(|| "    ".into());
    write_commented(value, layout, &indent, comments)
}

/// How a .jsonc text is laid out: like [`detect_layout`], but the indent read from its first indented line (a comment
/// on the first line hides it from that one).
pub fn jsonc_layout(text: &str) -> Layout {
    let mut layout = detect_layout(text);
    let first = text.lines().map(|l| l.chars().take_while(|c| *c == ' ' || *c == '\t').collect::<String>()).find(|i| !i.is_empty());
    if let Some(indent) = first {
        layout.indent = Some(indent);
    }
    layout
}

fn write_commented(value: &Value, layout: &Layout, indent: &str, comments: &Comments) -> String {
    let indent = indent.to_string();
    let mut out = String::new();
    if layout.bom {
        out.push('\u{feff}');
    }
    for line in &comments.head {
        out.push_str(line);
        out.push('\n');
    }
    write_value(&mut out, value, &mut Vec::new(), 0, &indent, comments);
    if !comments.tail.is_empty() {
        out.push('\n');
        out.push_str(&comments.tail.join("\n"));
    }
    if layout.trailing_newline || layout.indent.is_none() {
        out.push('\n');
    }
    out
}

fn write_value(out: &mut String, value: &Value, path: &mut Vec<String>, depth: usize, indent: &str, comments: &Comments) {
    let pad = |depth: usize| indent.repeat(depth);
    match value {
        Value::Object(map) if !map.is_empty() => {
            out.push_str("{\n");
            let count = map.len();
            for (index, (key, item)) in map.iter().enumerate() {
                path.push(key.clone());
                let joined = path.join("\u{1}");
                for line in comments.before.get(&joined).into_iter().flatten() {
                    // The first line at the setting's indent; the rest of a /* */ comment as it was written.
                    for (number, part) in line.lines().enumerate() {
                        if number == 0 {
                            out.push_str(&pad(depth + 1));
                            out.push_str(part.trim_start());
                        } else {
                            out.push_str(part);
                        }
                        out.push('\n');
                    }
                }
                out.push_str(&pad(depth + 1));
                out.push_str(&serde_json::to_string(key).unwrap_or_default());
                out.push_str(": ");
                write_value(out, item, path, depth + 1, indent, comments);
                if index + 1 < count {
                    out.push(',');
                }
                if let Some(after) = comments.after.get(&joined) {
                    out.push(' ');
                    out.push_str(after);
                }
                out.push('\n');
                path.pop();
            }
            out.push_str(&pad(depth));
            out.push('}');
        }
        Value::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                out.push_str(&pad(depth + 1));
                write_value(out, item, path, depth + 1, indent, comments);
                if index + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad(depth));
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}

/// `current` with every setting of `example` it lacks, in the example's order (objects inside too); its own values and
/// its own extra settings stay. Returns the merged value and the settings added (as dotted paths).
pub fn add_from_example(example: &Value, current: &Value) -> (Value, Vec<String>) {
    let mut added = Vec::new();
    let merged = merge(example, current, "", &mut added);
    (merged, added)
}

fn merge(example: &Value, current: &Value, prefix: &str, added: &mut Vec<String>) -> Value {
    match (example, current) {
        (Value::Object(example), Value::Object(current)) => {
            let mut out = serde_json::Map::new();
            for (key, value) in example {
                let path = format!("{prefix}{key}");
                match current.get(key) {
                    Some(mine) => {
                        out.insert(key.clone(), merge(value, mine, &format!("{path} › "), added));
                    }
                    None => {
                        out.insert(key.clone(), value.clone());
                        added.push(path);
                    }
                }
            }
            for (key, value) in current {
                if !out.contains_key(key) {
                    out.insert(key.clone(), value.clone());
                }
            }
            Value::Object(out)
        }
        (_, mine) => mine.clone(),
    }
}

const SAVE: &str = r#"json={{JsonFile}}
target={{Target}}
served={{Served}}
expected={{Expected}}
trap 'rm -f "$json"' EXIT
[ -s "$json" ] || fail "The uploaded file is missing or empty."
dir=$(dirname "$target")
name=$(basename "$target")
[ -d "$dir" ] || fail "$dir does not exist: run the Octane step first."
if [ -n "$expected" ]; then
  [ -f "$target" ] || fail "$target is gone from the server."
  now=$(sha256sum "$target" | cut -d' ' -f1)
  [ "$now" = "$expected" ] || fail "$target was changed on the server since it was loaded: load it again first, nothing was saved."
  old="$target.bak-$(date +%Y%m%d-%H%M%S)"
  cp -p "$target" "$old"
  echo "OLD=$old"
  owner="$(stat -c %u "$target")"; group="$(stat -c %g "$target")"; mode="$(stat -c %a "$target")"
else
  [ -f "$target" ] && fail "$target was made on the server since it was loaded: load it again first, nothing was saved."
  owner="$(stat -c %u "$dir")"; group="$(stat -c %g "$dir")"; mode=644
fi
tmp="$dir/.$name.new-$$"
install -o "$owner" -g "$group" -m "$mode" "$json" "$tmp"
mv -f "$tmp" "$target"
echo "  $target saved"
# What the hotel serves: the built client's copy, used at the players' next reload.
if [ -d "$served" ]; then
  install -o "$owner" -g "$group" -m "$mode" "$target" "$served/.$name.new-$$"
  mv -f "$served/.$name.new-$$" "$served/$name"
  echo "  $served/$name updated: players get it when they reload the hotel"
  echo "LIVE=yes"
fi
echo "SHA=$(sha256sum "$target" | cut -d' ' -f1)""#;

/// The shell script that puts `uploaded` in place of the file in public/configuration (after checking it is still
/// `expected`; empty: it must not exist yet) and in dist/configuration.
pub fn save_script(name: &str, uploaded: &str, expected: &str) -> String {
    fill(
        SAVE,
        &[
            ("JsonFile", &shell_quote(uploaded)),
            ("Target", &shell_quote(&format!("{}/{name}", public()))),
            ("Served", &shell_quote(&served())),
            ("Expected", &shell_quote(expected)),
        ],
    )
}

/// What a save left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    pub backup: Option<String>,
    pub sha256: String,
    /// The served copy was updated too.
    pub live: bool,
}

/// Writes `text` over the loaded file. The text must be valid JSON (comments allowed in a .jsonc file).
pub async fn save(server: &dyn Server, loaded: &Loaded, text: &str, log: Log<'_>) -> Result<Saved> {
    if let Err(problem) = parse(&loaded.name, text) {
        return Err(ServerError(format!("{} is not valid JSON ({problem}): nothing was saved.", loaded.name)).into());
    }
    let local = std::env::temp_dir().join(format!("all-in-1-config-{}-{}", std::process::id(), chrono::Local::now().format("%Y%m%d-%H%M%S%f")));
    std::fs::write(&local, text.as_bytes())?;
    let directory = server_tools::backup_directory();
    let remote = format!("{directory}/config-{}-{}", loaded.name, chrono::Local::now().format("%Y%m%d-%H%M%S%f"));
    let uploaded = async {
        server.run_as_root(&format!("install -d -m 700 {}", shell_quote(&directory))).await?;
        server.upload(&local, &remote, &|_| {}).await
    }
    .await;
    let _ = std::fs::remove_file(&local);
    uploaded?;
    let body = save_script(&loaded.name, &remote, &loaded.sha256);
    let failure = format!("Saving {} failed", loaded.name);
    let values = script::run_capturing(server, &script::with_prelude(&body), log, &failure, &["OLD", "SHA", "LIVE"], None).await?;
    Ok(Saved { backup: values.string("OLD"), sha256: values.string("SHA").unwrap_or_else(|| sha256(text.as_bytes())), live: values.yes("LIVE") })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_and_examples() {
        for good in ["ui-config.json", "radio-stations.jsonc", "housekeeping-texts-en.json"] {
            assert!(valid_name(good), "{good}");
        }
        for bad in ["../x.json", "a b.json", ".hidden.json", "ui-config.example", "x.js"] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert_eq!(example_target("ui-config.example").as_deref(), Some("ui-config.json"));
        assert_eq!(example_target("radio-stations.jsonc.example").as_deref(), Some("radio-stations.jsonc"));
        assert_eq!(example_target("pets.json.example.json"), None);
        assert_eq!(example_target("ads-frame.html.example"), None);
        assert_eq!(example_target("UITexts_en.jsonc.example"), None);
        assert_eq!(example_of("ui-config.json"), "ui-config.example");
        assert_eq!(example_of("radio-stations.jsonc"), "radio-stations.jsonc.example");
        assert_eq!(examples_of("ui-config.jsonc"), vec!["ui-config.jsonc.example", "ui-config.example"]);
        assert_eq!(examples_of("ui-config.json"), vec!["ui-config.example", "ui-config.jsonc.example"]);
        // A hotel that keeps ui-config.jsonc: one entry, with Octane's example.
        let files = parse_listing("P ui-config.jsonc 9\nE ui-config.example\nD ui-config.jsonc\n");
        assert_eq!(files, vec![ConfigFile { name: "ui-config.jsonc".into(), size: Some(9), example: true, served: true }]);
        let files = parse_listing("P ui-config.json 120\nP news.json 4\nE ui-config.example\nE adsense.example\nE ads-frame.html.example\nD ui-config.json\nP ../evil.json 1\n");
        assert_eq!(
            files,
            vec![
                ConfigFile { name: "adsense.json".into(), size: None, example: true, served: false },
                ConfigFile { name: "news.json".into(), size: Some(4), example: false, served: false },
                ConfigFile { name: "ui-config.json".into(), size: Some(120), example: true, served: true },
            ]
        );
    }

    #[test]
    fn jsonc_is_read_with_its_comments_and_trailing_commas() {
        let text = "{\n  // a comment\n  \"url\": \"https://x/y // not a comment\", /* block */\n  \"list\": [1, 2,],\n}\n";
        assert_eq!(parse("radio.jsonc", text).unwrap(), json!({"url": "https://x/y // not a comment", "list": [1, 2]}));
        assert!(parse("ui-config.json", text).is_err(), "a .json file has no comments");
        let problem = parse("x.json", "{\n \"a\": 1,\n \"b\" 2\n}").unwrap_err();
        assert!(problem.starts_with("line 3"), "{problem}");
    }

    #[test]
    fn a_jsonc_file_keeps_its_comments_when_saved_from_the_form() {
        let text = "// Radio stations of the hotel\n{\n    // The first station\n    \"stations\": [\n        \"https://radio.example/a\",\n    ],\n    \"volume\": 50, // 0 to 100\n    /* The player\n       settings */\n    \"player\": {\n        // Starts on its own\n        \"autoplay\": true,\n        \"url\": \"https://x/y // not a comment\"\n    },\n}\n// end\n";
        let found = comments(text);
        assert_eq!(found.head, vec!["// Radio stations of the hotel"]);
        assert_eq!(found.before.get("stations").unwrap(), &vec!["// The first station".to_string()]);
        assert_eq!(found.after.get("volume").unwrap(), "// 0 to 100");
        assert_eq!(found.before.get("player\u{1}autoplay").unwrap(), &vec!["// Starts on its own".to_string()]);
        assert!(found.before.get("player").unwrap()[0].starts_with("/* The player"));
        assert_eq!(found.tail, vec!["// end"]);
        let mut value = parse("radio.jsonc", text).unwrap();
        value["volume"] = json!(80);
        value["player"]["autoplay"] = json!(false);
        let written = write_with_comments(&value, &jsonc_layout(text), &found);
        assert!(written.contains("\n    \"volume\": 80"), "the file's own indent:\n{written}");
        assert_eq!(parse("radio.jsonc", &written).unwrap(), value, "{written}");
        for kept in ["// Radio stations of the hotel", "// The first station", "\"volume\": 80, // 0 to 100", "// Starts on its own", "/* The player", "settings */", "// end", "https://x/y // not a comment"] {
            assert!(written.contains(kept), "{kept} lost:\n{written}");
        }
        // Without changes it reads back the same.
        let again = write_with_comments(&parse("radio.jsonc", text).unwrap(), &jsonc_layout(text), &found);
        assert_eq!(comments(&again), found, "{again}");
    }

    #[test]
    fn new_settings_of_the_example_are_added_in_its_order() {
        let example = json!({"a": 1, "group": {"x": true, "y": "new"}, "b": 2, "c": 3});
        let current = json!({"c": 30, "a": 10, "group": {"x": false}, "mine": "kept"});
        let (merged, added) = add_from_example(&example, &current);
        assert_eq!(serde_json::to_string(&merged).unwrap(), r#"{"a":10,"group":{"x":false,"y":"new"},"b":2,"c":30,"mine":"kept"}"#);
        assert_eq!(added, vec!["group › y", "b"]);
        let layout = detect_layout("{\n    \"a\": 1\n}\n");
        assert_eq!(write(&json!({"a": 1}), &layout), "{\n    \"a\": 1\n}\n");
    }

    #[test]
    fn the_save_checks_what_it_replaces_and_updates_the_served_copy() {
        let script = save_script("ui-config.json", "/root/b/up", "abc");
        assert!(script.contains("target='/var/www/Octane/public/configuration/ui-config.json'"), "{script}");
        assert!(script.contains("served='/var/www/Octane/dist/configuration'"), "{script}");
        let check = script.find("[ \"$now\" = \"$expected\" ]").unwrap();
        let backup = script.find("cp -p \"$target\" \"$old\"").unwrap();
        let live = script.find("$served/$name").unwrap();
        assert!(check < backup && backup < live);
    }
}

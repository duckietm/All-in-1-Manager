//! Gamedata editor: FurnitureData.json and ExternalTexts.json in gamedata/config, edited in place over SSH.
//!
//! A file is downloaded whole, searched and changed here, and written back the safe way: the server first checks that
//! the file is still the one that was loaded (its SHA-256, so an edit by the emulator or the furni editor in the
//! meantime is not overwritten), copies it to `<file>.bak-<time>`, writes the new one next to it under a temporary
//! name and moves that over the old one (one rename, so nothing ever reads half a file). The JSON keeps the file's
//! own layout (compact or its indent, a BOM, a closing newline) and every field this editor does not know.
//!
//! The emulator watches its furnidata source (`items.furnidata.watch.enabled`, on by default) and sends the changed
//! entries to everyone online (FurnitureDataReload), so a saved FurnitureData.json needs no command. Texts are read
//! by the client when it starts: players see them after reloading the page.
//!
//! UITexts.jsonc and FigureData are not edited here: UITexts.jsonc has comments, which a JSON round trip would drop,
//! and a figuredata entry is a web of set ids, part ids and palettes where a plain field edit easily breaks avatars.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::script::{self, fill};
use crate::server::{shell_quote, Log, Result, Server, ServerError};
use crate::{octane, server_tools};

/// The files this editor changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GamedataFile {
    Furnidata,
    Texts,
}

impl GamedataFile {
    pub fn file_name(self) -> &'static str {
        match self {
            GamedataFile::Furnidata => "FurnitureData.json",
            GamedataFile::Texts => "ExternalTexts.json",
        }
    }

    /// Where it is on the server (Settings → Gamedata layout; gamedata/config as standard).
    pub fn path(self) -> String {
        match self {
            GamedataFile::Furnidata => octane::furnidata(),
            GamedataFile::Texts => octane::gamedata_path("external_texts"),
        }
    }
}

/// How a file was written, so it is written back the same way.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Layout {
    /// None: on one line; otherwise the indent of one level.
    pub indent: Option<String>,
    pub bom: bool,
    pub trailing_newline: bool,
}

/// The layout of `text`: compact when the first value has no line break after it, otherwise the indent of the first
/// indented line.
pub fn detect_layout(text: &str) -> Layout {
    let bom = text.starts_with('\u{feff}');
    let body = text.trim_start_matches('\u{feff}');
    let trailing_newline = body.ends_with('\n');
    let trimmed = body.trim_end();
    let indent = trimmed.lines().nth(1).map(|line| line.chars().take_while(|c| *c == ' ' || *c == '\t').collect::<String>()).map(|i| if i.is_empty() { "  ".to_string() } else { i });
    let indent = if trimmed.contains('\n') { indent } else { None };
    Layout { indent, bom, trailing_newline }
}

/// `value` as text in `layout`.
pub fn write_json(value: &Value, layout: &Layout) -> String {
    let mut out = Vec::new();
    match &layout.indent {
        None => {
            let _ = serde_json::to_writer(&mut out, value);
        }
        Some(indent) => {
            let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
            let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
            let _ = serde::Serialize::serialize(value, &mut serializer);
        }
    }
    let mut text = String::from_utf8(out).unwrap_or_default();
    if layout.bom {
        text.insert(0, '\u{feff}');
    }
    if layout.trailing_newline {
        text.push('\n');
    }
    text
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// A file as loaded: its JSON, how it was written and its checksum then.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub file: GamedataFile,
    pub json: Value,
    pub layout: Layout,
    pub sha256: String,
    pub bytes: usize,
}

/// Parses a downloaded file (a BOM allowed).
pub fn parse(file: GamedataFile, bytes: &[u8]) -> std::result::Result<Loaded, String> {
    let text = String::from_utf8_lossy(bytes);
    let json: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("{} is not valid JSON: {e}", file.file_name()))?;
    if !json.is_object() {
        return Err(format!("{} is not a JSON object.", file.file_name()));
    }
    Ok(Loaded { file, json, layout: detect_layout(&text), sha256: sha256(bytes), bytes: bytes.len() })
}

/// Downloads and parses the file; None when the server has none.
pub async fn load(server: &dyn Server, file: GamedataFile) -> Result<Option<Loaded>> {
    let path = file.path();
    let exists = server.run_as_root(&format!("test -f {}", shell_quote(&path))).await?;
    if !exists.succeeded() {
        return Ok(None);
    }
    let local = local_temp();
    let downloaded = server.download(&path, &local, &|_| {}).await;
    let bytes = downloaded.and_then(|_| Ok(std::fs::read(&local)?));
    let _ = std::fs::remove_file(&local);
    parse(file, &bytes?).map(Some).map_err(|e| ServerError(e).into())
}

fn local_temp() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("all-in-1-gamedata-{}-{}.json", std::process::id(), chrono::Local::now().format("%Y%m%d-%H%M%S%f")))
}

const SAVE: &str = r#"json={{JsonFile}}
target={{Target}}
expected={{Expected}}
trap 'rm -f "$json"' EXIT
[ -s "$json" ] || fail "The uploaded file is missing or empty."
[ -f "$target" ] || fail "$target is gone from the server."
now=$(sha256sum "$target" | cut -d' ' -f1)
[ "$now" = "$expected" ] || fail "$target was changed on the server since it was loaded: load it again first, nothing was saved."
dir=$(dirname "$target")
name=$(basename "$target")
old="$target.bak-$(date +%Y%m%d-%H%M%S)"
cp -p "$target" "$old"
echo "OLD=$old"
tmp="$dir/.$name.new-$$"
install -o "$(stat -c %u "$target")" -g "$(stat -c %g "$target")" -m "$(stat -c %a "$target")" "$json" "$tmp"
mv -f "$tmp" "$target"
echo "SHA=$(sha256sum "$target" | cut -d' ' -f1)"
echo "  $target saved, the old one kept as $old""#;

/// The shell script that puts `uploaded` in place of `target`, after checking it is still `expected` and copying it to
/// `.bak-<time>`. Owner, group and mode of the old file are kept.
pub fn save_script(target: &str, uploaded: &str, expected: &str) -> String {
    fill(SAVE, &[("JsonFile", &shell_quote(uploaded)), ("Target", &shell_quote(target)), ("Expected", &shell_quote(expected))])
}

/// What a save left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    pub backup: String,
    pub sha256: String,
}

/// Writes `json` over the loaded file (see [`save_script`]). Returns the backup and the new checksum.
pub async fn save(server: &dyn Server, loaded: &Loaded, json: &Value, log: Log<'_>) -> Result<Saved> {
    let text = write_json(json, &loaded.layout);
    let local = local_temp();
    std::fs::write(&local, text.as_bytes())?;
    let directory = server_tools::backup_directory();
    let remote = format!("{directory}/gamedata-{}-{}.json", loaded.file.file_name(), chrono::Local::now().format("%Y%m%d-%H%M%S%f"));
    let uploaded = async {
        server.run_as_root(&format!("install -d -m 700 {}", shell_quote(&directory))).await?;
        server.upload(&local, &remote, &|_| {}).await
    }
    .await;
    let _ = std::fs::remove_file(&local);
    uploaded?;
    let body = save_script(&loaded.file.path(), &remote, &loaded.sha256);
    let failure = format!("Saving {} failed", loaded.file.file_name());
    let values = script::run_capturing(server, &script::with_prelude(&body), log, &failure, &["OLD", "SHA"], None).await?;
    Ok(Saved { backup: values.string("OLD").unwrap_or_default(), sha256: values.string("SHA").unwrap_or_else(|| sha256(text.as_bytes())) })
}

// ---------------------------------------------------------------- furnidata

/// Floor or wall furni.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Floor,
    Wall,
}

impl Kind {
    pub fn section(self) -> &'static str {
        match self {
            Kind::Floor => "roomitemtypes",
            Kind::Wall => "wallitemtypes",
        }
    }

    /// items_base.type.
    pub fn item_type(self) -> &'static str {
        match self {
            Kind::Floor => "s",
            Kind::Wall => "i",
        }
    }
}

fn entries(data: &Value, kind: Kind) -> &[Value] {
    data.get(kind.section()).and_then(|s| s.get("furnitype")).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn entries_mut(data: &mut Value, kind: Kind) -> Option<&mut Vec<Value>> {
    data.get_mut(kind.section())?.get_mut("furnitype")?.as_array_mut()
}

/// One furni found by [`search`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub kind: Kind,
    pub index: usize,
    pub id: i64,
    pub classname: String,
    pub name: String,
}

fn text_of(entry: &Value, key: &str) -> String {
    match entry.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn hit(kind: Kind, index: usize, entry: &Value) -> Hit {
    Hit { kind, index, id: entry.get("id").and_then(Value::as_i64).unwrap_or(0), classname: text_of(entry, "classname"), name: text_of(entry, "name") }
}

/// Furni whose id is `query` (a number), or whose classname or name contains it (any case); exact matches first, at
/// most `limit`.
pub fn search(data: &Value, query: &str, limit: usize) -> Vec<Hit> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let id: Option<i64> = query.parse().ok();
    let mut exact = Vec::new();
    let mut partial = Vec::new();
    for kind in [Kind::Floor, Kind::Wall] {
        for (index, entry) in entries(data, kind).iter().enumerate() {
            let h = hit(kind, index, entry);
            let classname = h.classname.to_lowercase();
            if Some(h.id) == id || classname == query {
                exact.push(h);
            } else if classname.contains(&query) || h.name.to_lowercase().contains(&query) {
                partial.push(h);
            }
        }
    }
    exact.extend(partial);
    exact.truncate(limit);
    exact
}

pub fn entry(data: &Value, kind: Kind, index: usize) -> Option<&Map<String, Value>> {
    entries(data, kind).get(index)?.as_object()
}

/// A field shown as text: (name, value as text, what type it keeps).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub text: String,
    pub kind: FieldKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Integer,
    Decimal,
    Bool,
    Null,
}

/// The plain fields of an entry (text, numbers, true/false, null) in the file's order. Objects and lists (partcolors)
/// are left out: they stay as they are.
pub fn plain_fields(entry: &Map<String, Value>) -> Vec<Field> {
    entry
        .iter()
        .filter_map(|(name, value)| {
            let (text, kind) = match value {
                Value::String(s) => (s.clone(), FieldKind::Text),
                Value::Number(n) if n.is_i64() || n.is_u64() => (n.to_string(), FieldKind::Integer),
                Value::Number(n) => (n.to_string(), FieldKind::Decimal),
                Value::Bool(b) => (b.to_string(), FieldKind::Bool),
                Value::Null => (String::new(), FieldKind::Null),
                _ => return None,
            };
            Some(Field { name: name.clone(), text, kind })
        })
        .collect()
}

/// The text of a field back as the type it had. Err: why it does not fit.
fn field_value(field: &Field) -> std::result::Result<Value, String> {
    let text = field.text.trim();
    match field.kind {
        FieldKind::Text => Ok(Value::String(field.text.clone())),
        FieldKind::Integer => text.parse::<i64>().map(Value::from).map_err(|_| format!("{}: \"{text}\" is not a whole number.", field.name)),
        FieldKind::Decimal => text.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number).ok_or_else(|| format!("{}: \"{text}\" is not a number.", field.name)),
        FieldKind::Bool => match text.to_lowercase().as_str() {
            "true" | "1" => Ok(Value::Bool(true)),
            "false" | "0" => Ok(Value::Bool(false)),
            _ => Err(format!("{}: \"{text}\" is not true or false.", field.name)),
        },
        FieldKind::Null if text.is_empty() => Ok(Value::Null),
        FieldKind::Null => Ok(Value::String(field.text.clone())),
    }
}

/// Why a classname cannot be used, or None. Letters, digits and `_ - . *` only, as furni bundles are named.
pub fn classname_problem(classname: &str) -> Option<String> {
    if classname.is_empty() {
        return Some("The classname is empty.".into());
    }
    if classname.len() > 100 {
        return Some("The classname is longer than 100 characters.".into());
    }
    if !classname.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '*')) {
        return Some(format!("The classname {classname} may only have letters, digits and _ - . *"));
    }
    None
}

/// Whether another entry (not `except`) uses the id or classname (classname in any case; floor and wall both).
fn taken(data: &Value, except: Option<(Kind, usize)>, id: i64, classname: &str) -> Option<String> {
    for kind in [Kind::Floor, Kind::Wall] {
        for (index, entry) in entries(data, kind).iter().enumerate() {
            if except == Some((kind, index)) {
                continue;
            }
            let other = hit(kind, index, entry);
            if other.id == id {
                return Some(format!("Id {id} is already used by {}.", other.classname));
            }
            if other.classname.eq_ignore_ascii_case(classname) {
                return Some(format!("The classname {classname} is already used (id {}).", other.id));
            }
        }
    }
    None
}

/// Writes the edited fields into the entry; fields not in `fields` (and objects such as partcolors) stay as they
/// are. Refuses a bad value, a bad classname or an id/classname another furni has.
pub fn apply_fields(data: &mut Value, kind: Kind, index: usize, fields: &[Field]) -> std::result::Result<(), String> {
    let mut values = Vec::with_capacity(fields.len());
    for field in fields {
        values.push((field.name.clone(), field_value(field)?));
    }
    let current = entry(data, kind, index).ok_or("That furni is no longer in the file.")?.clone();
    let mut updated = current;
    for (name, value) in values {
        updated.insert(name, value);
    }
    let id = updated.get("id").and_then(Value::as_i64).ok_or("The id must be a whole number.")?;
    let classname = text_of(&Value::Object(updated.clone()), "classname");
    if let Some(problem) = classname_problem(&classname) {
        return Err(problem);
    }
    if let Some(problem) = taken(data, Some((kind, index)), id, &classname) {
        return Err(problem);
    }
    let list = entries_mut(data, kind).ok_or("The file has no furni list.")?;
    list[index] = Value::Object(updated);
    Ok(())
}

/// The lowest id above every floor and wall furni.
pub fn next_free_id(data: &Value) -> i64 {
    [Kind::Floor, Kind::Wall].iter().flat_map(|k| entries(data, *k)).filter_map(|e| e.get("id").and_then(Value::as_i64)).max().unwrap_or(0) + 1
}

/// Adds a copy of an entry with its own id and classname at the end of the same list; its offer id becomes -1 (no
/// offer yet: Fix offer IDs gives it one). Returns the new entry's index.
pub fn add_copy(data: &mut Value, kind: Kind, index: usize, id: i64, classname: &str) -> std::result::Result<usize, String> {
    if id <= 0 {
        return Err("The id must be above 0.".into());
    }
    if let Some(problem) = classname_problem(classname) {
        return Err(problem);
    }
    if let Some(problem) = taken(data, None, id, classname) {
        return Err(problem);
    }
    let mut copy = entry(data, kind, index).ok_or("That furni is no longer in the file.")?.clone();
    copy.insert("id".into(), Value::from(id));
    copy.insert("classname".into(), Value::String(classname.to_string()));
    if copy.contains_key("offerid") {
        copy.insert("offerid".into(), Value::from(-1));
    }
    let list = entries_mut(data, kind).ok_or("The file has no furni list.")?;
    list.push(Value::Object(copy));
    Ok(list.len() - 1)
}

/// An items_base row of a furni.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemRow {
    pub id: i64,
    pub sprite_id: i64,
    pub item_name: String,
    pub kind: String,
}

const ITEMS: &str = r#"{{DatabaseName}}
mariadb -N "$db" -e 'SELECT 1' >/dev/null 2>&1 || fail "The hotel database $db does not exist or MariaDB does not run."
mariadb -N -B -r "$db" -e "SELECT 'I', id, sprite_id, item_name, type FROM items_base WHERE item_name = '{{Classname}}' OR sprite_id = {{Id}} ORDER BY id LIMIT 50""#;

/// The read-only query for the items_base rows of a classname or sprite id, run as root through the socket. The
/// classname is checked first (see [`classname_problem`]), so it cannot leave the quotes.
pub fn items_script(id: i64, classname: &str) -> std::result::Result<String, String> {
    if let Some(problem) = classname_problem(classname) {
        return Err(problem);
    }
    Ok(fill(ITEMS, &[("DatabaseName", &server_tools::database_name()), ("Classname", classname), ("Id", &id.to_string())]))
}

pub fn parse_items(output: &str) -> Vec<ItemRow> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.trim_end_matches('\r').splitn(5, '\t').collect();
            match fields.as_slice() {
                ["I", id, sprite, name, kind] => Some(ItemRow { id: id.parse().ok()?, sprite_id: sprite.parse().ok()?, item_name: name.to_string(), kind: kind.to_string() }),
                _ => None,
            }
        })
        .collect()
}

/// The items_base rows of a furni (by classname or sprite id).
pub async fn items_for(server: &dyn Server, id: i64, classname: &str) -> Result<Vec<ItemRow>> {
    let body = items_script(id, classname).map_err(ServerError)?;
    let result = server.run_as_root(&script::with_prelude(&body)).await?;
    if !result.succeeded() {
        let reason = result.output.lines().find_map(|l| l.strip_prefix("ERROR: ")).map(str::to_string).unwrap_or_else(|| result.error.trim().to_string());
        return Err(ServerError(format!("Reading items_base failed: {reason}")).into());
    }
    Ok(parse_items(&result.output))
}

/// How an entry and its items_base rows fit: None when they do, otherwise what is off.
pub fn items_problem(kind: Kind, id: i64, classname: &str, rows: &[ItemRow]) -> Option<String> {
    let named: Vec<&ItemRow> = rows.iter().filter(|r| r.item_name.eq_ignore_ascii_case(classname)).collect();
    if named.is_empty() {
        return Some(match rows.iter().find(|r| r.sprite_id == id) {
            Some(other) => format!("Not in items_base by name; sprite id {id} belongs to {} (items_base id {}).", other.item_name, other.id),
            None => "Not in items_base: the hotel cannot place this furni yet.".into(),
        });
    }
    if let Some(row) = named.iter().find(|r| r.sprite_id != id) {
        return Some(format!("items_base {} has sprite id {}, the file says {id}: run Fix items_base.", row.id, row.sprite_id));
    }
    if let Some(row) = named.iter().find(|r| r.kind != kind.item_type()) {
        return Some(format!("items_base {} has type {}, a {} furni needs {}.", row.id, row.kind, if kind == Kind::Floor { "floor" } else { "wall" }, kind.item_type()));
    }
    None
}

// ---------------------------------------------------------------- texts

/// Texts whose key or value contains `query` (any case), in the file's order, at most `limit`; an exact key first.
pub fn search_texts(texts: &Value, query: &str, limit: usize) -> Vec<(String, String)> {
    let Some(map) = texts.as_object() else { return Vec::new() };
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<(String, String)> = Vec::new();
    if let Some((key, value)) = map.iter().find(|(k, _)| k.to_lowercase() == query) {
        found.push((key.clone(), text_of_value(value)));
    }
    for (key, value) in map {
        if found.len() >= limit {
            break;
        }
        let text = text_of_value(value);
        if key.to_lowercase() != query && (key.to_lowercase().contains(&query) || text.to_lowercase().contains(&query)) {
            found.push((key.clone(), text));
        }
    }
    found.truncate(limit);
    found
}

fn text_of_value(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Why a text key cannot be used, or None.
pub fn key_problem(key: &str) -> Option<String> {
    if key.trim().is_empty() {
        return Some("The key is empty.".into());
    }
    if key != key.trim() || key.chars().any(char::is_control) {
        return Some("The key may not start or end with spaces or have line breaks.".into());
    }
    None
}

/// Sets a text: an existing key keeps its place, a new one is added at the end. True when the key is new.
pub fn set_text(texts: &mut Value, key: &str, value: &str) -> std::result::Result<bool, String> {
    if let Some(problem) = key_problem(key) {
        return Err(problem);
    }
    let map = texts.as_object_mut().ok_or("The texts are not a JSON object.")?;
    let new = !map.contains_key(key);
    map.insert(key.to_string(), Value::String(value.to_string()));
    Ok(new)
}

/// Deletes a text, keeping the order of the rest. True when it was there.
pub fn remove_text(texts: &mut Value, key: &str) -> bool {
    texts.as_object_mut().and_then(|m| m.shift_remove(key)).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn furnidata() -> Value {
        json!({
            "roomitemtypes": {"furnitype": [
                {"id": 13, "classname": "shelves_norja", "revision": 61856, "category": "shelf", "defaultdir": 0, "xdim": 1, "ydim": 1,
                 "partcolors": {"color": ["#ffffff", "#F7EBBC"]}, "name": "Beige Bookcase", "description": "For nic naks and books.",
                 "adurl": null, "offerid": 5, "buyout": true, "bc": false, "customparams": "", "furniline": "iced", "rare": false, "x_custom": {"k": 1}},
                {"id": 14, "classname": "chair_norja", "name": "Beige Chair", "offerid": 6, "xdim": 1, "height": 1.5}
            ]},
            "wallitemtypes": {"furnitype": [{"id": 4001, "classname": "poster", "name": "Poster", "offerid": 7}]}
        })
    }

    #[test]
    fn search_by_id_classname_and_name() {
        let data = furnidata();
        let hits = search(&data, "4001", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].kind, hits[0].classname.as_str()), (Kind::Wall, "poster"));
        let hits = search(&data, "NORJA", 10);
        assert_eq!(hits.iter().map(|h| h.id).collect::<Vec<_>>(), vec![13, 14]);
        let hits = search(&data, "chair_norja", 10);
        assert_eq!(hits[0].id, 14, "an exact classname comes first");
        assert_eq!(search(&data, "bookcase", 10)[0].id, 13, "the name counts too");
        assert_eq!(search(&data, "norja", 1).len(), 1);
        assert!(search(&data, "  ", 10).is_empty());
    }

    #[test]
    fn plain_fields_keep_types_and_unknown_fields() {
        let mut data = furnidata();
        let mut fields = plain_fields(entry(&data, Kind::Floor, 0).unwrap());
        assert!(fields.iter().all(|f| f.name != "partcolors" && f.name != "x_custom"), "objects are not shown");
        let set = |fields: &mut Vec<Field>, name: &str, text: &str| fields.iter_mut().find(|f| f.name == name).unwrap().text = text.into();
        set(&mut fields, "name", "Blue Bookcase");
        set(&mut fields, "xdim", "2");
        set(&mut fields, "buyout", "0");
        set(&mut fields, "adurl", "");
        apply_fields(&mut data, Kind::Floor, 0, &fields).unwrap();
        let e = &data["roomitemtypes"]["furnitype"][0];
        assert_eq!(e["name"], "Blue Bookcase");
        assert_eq!(e["xdim"], 2);
        assert_eq!(e["buyout"], false);
        assert_eq!(e["adurl"], Value::Null, "an empty null stays null");
        assert_eq!(e["partcolors"]["color"][1], "#F7EBBC");
        assert_eq!(e["x_custom"]["k"], 1, "unknown fields stay");
        let keys: Vec<&String> = e.as_object().unwrap().keys().collect();
        assert_eq!(keys.first().map(|k| k.as_str()), Some("id"));
        assert_eq!(keys.last().map(|k| k.as_str()), Some("x_custom"), "the order of the fields stays");
    }

    #[test]
    fn bad_edits_are_refused() {
        let mut data = furnidata();
        let fields = plain_fields(entry(&data, Kind::Floor, 1).unwrap());
        let with = |name: &str, text: &str| {
            let mut f = fields.clone();
            f.iter_mut().find(|f| f.name == name).unwrap().text = text.into();
            f
        };
        assert!(apply_fields(&mut data, Kind::Floor, 1, &with("xdim", "two")).unwrap_err().contains("whole number"));
        assert!(apply_fields(&mut data, Kind::Floor, 1, &with("height", "x")).is_err());
        assert!(apply_fields(&mut data, Kind::Floor, 1, &with("id", "13")).unwrap_err().contains("already used"));
        assert!(apply_fields(&mut data, Kind::Floor, 1, &with("id", "4001")).unwrap_err().contains("already used"), "wall ids count");
        assert!(apply_fields(&mut data, Kind::Floor, 1, &with("classname", "Shelves_Norja")).is_err());
        assert!(apply_fields(&mut data, Kind::Floor, 1, &with("classname", "a'b")).unwrap_err().contains("may only"));
        assert_eq!(data, furnidata(), "nothing changed");
        assert!(apply_fields(&mut data, Kind::Floor, 1, &with("height", "2.25")).is_ok());
        assert_eq!(data["roomitemtypes"]["furnitype"][1]["height"], 2.25);
    }

    #[test]
    fn copies_get_their_own_id_and_no_offer() {
        let mut data = furnidata();
        assert_eq!(next_free_id(&data), 4002);
        let index = add_copy(&mut data, Kind::Floor, 0, 4002, "shelves_norja_blue").unwrap();
        assert_eq!(index, 2);
        let copy = &data["roomitemtypes"]["furnitype"][2];
        assert_eq!(copy["id"], 4002);
        assert_eq!(copy["classname"], "shelves_norja_blue");
        assert_eq!(copy["offerid"], -1);
        assert_eq!(copy["partcolors"], data["roomitemtypes"]["furnitype"][0]["partcolors"]);
        assert_eq!(data["roomitemtypes"]["furnitype"][0]["offerid"], 5, "the original stays");
        assert!(add_copy(&mut data, Kind::Floor, 0, 13, "x").is_err());
        assert!(add_copy(&mut data, Kind::Wall, 0, 9000, "POSTER").is_err());
        assert!(add_copy(&mut data, Kind::Wall, 0, 0, "poster2").is_err());
        assert!(add_copy(&mut data, Kind::Wall, 5, 9000, "poster2").is_err());
    }

    #[test]
    fn items_base_checks() {
        let rows = parse_items("I\t1\t13\tshelves_norja\ts\nnoise\nI\t2\t99\tother\ti\r\n");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1], ItemRow { id: 2, sprite_id: 99, item_name: "other".into(), kind: "i".into() });
        assert_eq!(items_problem(Kind::Floor, 13, "shelves_norja", &rows[..1]), None);
        assert!(items_problem(Kind::Floor, 14, "shelves_norja", &rows[..1]).unwrap().contains("Fix items_base"));
        assert!(items_problem(Kind::Wall, 13, "shelves_norja", &rows[..1]).unwrap().contains("type s"));
        assert!(items_problem(Kind::Floor, 99, "new_one", &rows).unwrap().contains("belongs to other"));
        assert!(items_problem(Kind::Floor, 5, "new_one", &[]).unwrap().contains("Not in items_base"));
        let script = items_script(13, "shelves_norja").unwrap();
        assert!(script.contains("item_name = 'shelves_norja' OR sprite_id = 13"));
        assert!(script.contains("db="));
        assert!(items_script(1, "x'; DROP TABLE users; --").is_err(), "a classname cannot leave the quotes");
    }

    #[test]
    fn layout_round_trip() {
        let data = json!({"a": {"b": [1, 2]}, "c": "é"});
        for text in ["{\"a\":{\"b\":[1,2]},\"c\":\"é\"}", "{\n  \"a\": {\n    \"b\": [\n      1,\n      2\n    ]\n  },\n  \"c\": \"é\"\n}\n", "\u{feff}{\n\t\"a\": {\n\t\t\"b\": [\n\t\t\t1,\n\t\t\t2\n\t\t]\n\t},\n\t\"c\": \"é\"\n}", "{\n    \"a\": {\n        \"b\": [\n            1,\n            2\n        ]\n    },\n    \"c\": \"é\"\n}"] {
            let loaded = parse(GamedataFile::Texts, text.as_bytes()).unwrap();
            assert_eq!(loaded.json, data);
            assert_eq!(write_json(&loaded.json, &loaded.layout), text, "written back byte for byte");
        }
        assert!(parse(GamedataFile::Texts, b"[1]").is_err());
        assert!(parse(GamedataFile::Furnidata, b"{").unwrap_err().contains("FurnitureData.json"));
        assert_eq!(sha256(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn texts_edit_keeps_order() {
        let mut texts = json!({"a": "Alpha", "badge_name_X": "My badge", "c": "Gamma"});
        assert_eq!(search_texts(&texts, "BADGE", 10), vec![("badge_name_X".to_string(), "My badge".to_string())]);
        assert_eq!(search_texts(&texts, "a", 10)[0].0, "a", "the exact key first");
        assert_eq!(search_texts(&texts, "gamma", 10)[0].0, "c", "values count");
        assert_eq!(set_text(&mut texts, "a", "A!"), Ok(false));
        assert_eq!(set_text(&mut texts, "new.key", "N"), Ok(true));
        assert!(set_text(&mut texts, " x", "N").is_err());
        assert!(set_text(&mut texts, "", "N").is_err());
        assert!(remove_text(&mut texts, "badge_name_X"));
        assert!(!remove_text(&mut texts, "badge_name_X"));
        assert_eq!(texts.as_object().unwrap().keys().cloned().collect::<Vec<_>>(), vec!["a", "c", "new.key"]);
        assert_eq!(texts["a"], "A!");
    }

    #[test]
    fn paths_and_script_shape() {
        assert!(GamedataFile::Furnidata.path().ends_with("/config/FurnitureData.json"));
        assert!(GamedataFile::Texts.path().ends_with("/config/ExternalTexts.json"));
        let script = save_script("/var/www/gamedata/config/Furni Data.json", "/root/b/up'load.json", "abc");
        assert!(script.contains("target='/var/www/gamedata/config/Furni Data.json'"), "{script}");
        assert!(script.contains(r"json='/root/b/up'\''load.json'"), "{script}");
        let check = script.find("[ \"$now\" = \"$expected\" ]").unwrap();
        let backup = script.find("cp -p \"$target\" \"$old\"").unwrap();
        let replace = script.find("mv -f \"$tmp\" \"$target\"").unwrap();
        assert!(check < backup && backup < replace, "checked, then the backup, then one rename");
        assert!(script.contains("tmp=\"$dir/.$name.new-$$\""), "the temporary file is in the same folder, so mv is a rename");
    }

    /// Runs the save script with bash on a temporary folder (skipped without bash or sha256sum).
    /// The script runs on the Linux server: on Windows, Git's bash gets `C:\...` paths it cannot use, so it is not run
    /// there.
    #[test]
    #[cfg_attr(windows, ignore = "the save script is for the Linux server; Windows paths do not work in it")]
    fn save_script_in_bash() {
        let has = |tool: &str| std::process::Command::new("sh").arg("-c").arg(format!("command -v {tool}")).output().is_ok_and(|o| o.status.success());
        if !has("bash") || !has("sha256sum") {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("FurnitureData.json");
        let upload = dir.path().join("upload.json");
        let run = |expected: &str| {
            std::fs::write(&upload, "{\"new\":1}").unwrap();
            let body = save_script(target.to_str().unwrap(), upload.to_str().unwrap(), expected);
            let script = format!("set -e\nfail() {{ echo \"ERROR: $*\"; exit 1; }}\n{body}");
            std::process::Command::new("bash").arg("-c").arg(script).output().unwrap()
        };
        std::fs::write(&target, "{\"old\":1}").unwrap();
        let refused = run(&sha256(b"something else"));
        assert!(!refused.status.success());
        assert!(String::from_utf8_lossy(&refused.stdout).contains("changed on the server"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{\"old\":1}", "a changed file is not overwritten");
        assert!(!upload.exists(), "the upload is removed either way");

        let saved = run(&sha256(b"{\"old\":1}"));
        let out = String::from_utf8_lossy(&saved.stdout).to_string();
        assert!(saved.status.success(), "{out}{}", String::from_utf8_lossy(&saved.stderr));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{\"new\":1}");
        let old = out.lines().find_map(|l| l.strip_prefix("OLD=")).unwrap();
        assert_eq!(std::fs::read_to_string(old).unwrap(), "{\"old\":1}", "the old file is kept");
        assert!(out.contains(&format!("SHA={}", sha256(b"{\"new\":1}"))));
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        assert_eq!(left.len(), 2, "only the file and its backup: {left:?}");
    }
}

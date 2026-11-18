//! What changed between two rips of the same hotel (the built-in's `diff.rs`): pages and offers
//! added, changed (which columns) and removed, as a report (`catalog_changes.json`) and as a
//! Manager import file with only those changes (`catalog_manager_changes.sql`: INSERT the new,
//! UPDATE the changed columns, and DELETE the removed when asked).
//!
//! The changes file assumes the hotel's catalogue holds the earlier rip with the ripped ids (it
//! was imported with `Replace` or `ReplaceRipped`): it is the "bring my catalogue up to date with
//! the hotel's new releases" file. The Manager's import preview refuses a change whose page or
//! offer is not there, so nothing is written blindly.
//!
//! [`diff`] is the built-in's function, on two whole catalogues in memory: the reference. The
//! job does the same without holding the earlier catalogue: its catalog.json is read in pieces
//! ([`Digest::feed`], an incremental JSON reader) and kept only as the rows the comparison needs
//! (the `Replace` layout of catalog_manager.sql, each row's column values packed in one
//! allocation); [`compare`] then walks the new catalogue's rows once and
//! [`write_changes_sql`] writes the file, the same bytes as [`diff`].
//!
//! Unlike the built-in, the earlier rip is read from its catalog.json (the built-in read its
//! recorded packets: the same catalogue, as catalog.json is written from it), so a rip made
//! without recording can be compared too.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use serde::Serialize;

use crate::export::{comment, item_ids_sql_with, ItemsBase, RippedCatalog};
use crate::furnidata::Furnidata;
use crate::manager::{
    catalog_type, literal_item_ids, manager_rows, offer_columns, page_columns, visit_manager_rows,
    ManagerExport, ManagerMode, ManagerRow, ManagerStats, PageFields,
};
use crate::packets::Offer;
use crate::source::{self, Source, SourceEntry};

/// One page or offer in the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffEntry {
    /// "page" or "offer".
    pub what: &'static str,
    /// NORMAL or BUILDER.
    pub catalog_type: &'static str,
    pub id: i64,
    pub label: String,
    /// For a changed row: the columns that changed, with (before, after).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub changed: BTreeMap<&'static str, (String, String)>,
}

impl DiffEntry {
    fn key(&self) -> (bool, &'static str, i64) {
        (self.what == "page", self.catalog_type, self.id)
    }

    /// `DELETE FROM ... WHERE id = .. AND catalog_type = ..;` ([`ManagerRow::delete`]).
    fn delete(&self) -> String {
        let table = if self.what == "page" {
            "catalog_pages"
        } else {
            "catalog_items"
        };
        format!(
            "DELETE FROM {table} WHERE id = {} AND catalog_type = '{}';",
            self.id, self.catalog_type
        )
    }
}

/// The changes between two rips.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CatalogDiff {
    /// The earlier rip's folder (the built-in wrote its full path; here the [`Source`] path).
    pub since: Option<String>,
    pub since_ripped_at: String,
    pub added: Vec<DiffEntry>,
    pub changed: Vec<DiffEntry>,
    pub removed: Vec<DiffEntry>,
}

impl CatalogDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }

    fn count(list: &[DiffEntry], what: &str) -> usize {
        list.iter().filter(|e| e.what == what).count()
    }

    /// "3 pages and 12 offers added, 4 offers changed, 1 page removed".
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "no changes".into();
        }
        let mut parts = Vec::new();
        for (list, verb) in [
            (&self.added, "added"),
            (&self.changed, "changed"),
            (&self.removed, "removed"),
        ] {
            let pages = Self::count(list, "page");
            let offers = Self::count(list, "offer");
            let mut items = Vec::new();
            if pages > 0 {
                items.push(format!("{pages} page{}", if pages == 1 { "" } else { "s" }));
            }
            if offers > 0 {
                items.push(format!(
                    "{offers} offer{}",
                    if offers == 1 { "" } else { "s" }
                ));
            }
            if !items.is_empty() {
                parts.push(format!("{} {verb}", items.join(" and ")));
            }
        }
        parts.join(", ")
    }
}

fn entry(row: &ManagerRow) -> DiffEntry {
    DiffEntry {
        what: if row.page { "page" } else { "offer" },
        catalog_type: row.kind,
        id: row.id,
        label: row.label.clone(),
        changed: BTreeMap::new(),
    }
}

/// The ManagerExport the comparison uses: the ripped ids (`Replace`), no hotel ids.
fn replace_export<'a>(
    items: Option<&'a ItemsBase>,
    furnidata: Option<&'a Furnidata>,
) -> ManagerExport<'a> {
    ManagerExport {
        items,
        furnidata,
        mode: ManagerMode::Replace,
        existing: None,
    }
}

/// The first lines of catalog_manager_changes.sql.
fn write_head(
    out: &mut dyn fmt::Write,
    report: &CatalogDiff,
    new: &RippedCatalog,
    delete_removed: bool,
) {
    let _ = writeln!(
        out,
        "-- Catalog Studio restricted SQL export.\n-- Changes in the catalogue of {} ({}) since the rip of {}, ripped on {} by {}.\n-- For a hotel whose catalogue holds the earlier rip with the ripped ids. Summary: {}.",
        comment(&new.hotel),
        comment(&new.host),
        comment(&report.since_ripped_at),
        comment(&new.ripped_at),
        comment(&new.generator),
        report.summary(),
    );
    if !delete_removed && !report.removed.is_empty() {
        let _ = writeln!(
            out,
            "-- {} removed pages/offers are kept in the hotel (removing was not asked).",
            report.removed.len()
        );
    }
}

/// The changes from `old` to `new`, and the Manager import file that makes them (the built-in's
/// `diff`, the reference for [`Digest`]). Both are exported with the ripped ids (the `Replace`
/// layout) and the same items_base / furnidata. Only what the new rip read is compared: a page
/// it did not read (another branch, no answer) is neither changed nor removed, nor are its
/// offers; a catalogue it did not rip is left out. (The built-in named
/// `All-in-1 Manager <version>` as the maker in the first line; this names `new.generator`.)
pub fn diff(
    old: &RippedCatalog,
    new: &RippedCatalog,
    export: &ManagerExport,
    delete_removed: bool,
) -> (CatalogDiff, String) {
    let export = ManagerExport {
        mode: ManagerMode::Replace,
        existing: None,
        ..*export
    };
    let (old_rows, _, _) = manager_rows(old, &export);
    let (new_rows, _, _) = manager_rows(new, &export);
    let ripped_now: Vec<&str> = new.modes.iter().map(|m| catalog_type(&m.mode)).collect();
    let old_by_key: BTreeMap<_, &ManagerRow> = old_rows.iter().map(|r| (r.key(), r)).collect();
    let new_by_key: BTreeMap<_, &ManagerRow> = new_rows.iter().map(|r| (r.key(), r)).collect();

    let mut report = CatalogDiff {
        since_ripped_at: old.ripped_at.clone(),
        ..CatalogDiff::default()
    };
    let mut inserts = Vec::new();
    let mut updates = Vec::new();
    // The pages whose content the new rip read: their offers are known completely.
    let read: HashSet<(&str, String)> = new_rows
        .iter()
        .filter(|r| r.page && r.has_content)
        .map(|r| (r.kind, r.id.to_string()))
        .collect();
    let offer_page = |row: &ManagerRow| -> Option<String> {
        row.columns
            .iter()
            .find(|(c, _)| *c == "page_id")
            .map(|(_, v)| v.clone())
    };
    for row in &new_rows {
        match old_by_key.get(&row.key()) {
            // Not read this time: nothing to compare.
            Some(_) if row.page && !row.has_content => {}
            None => {
                inserts.push(row.insert());
                report.added.push(entry(row));
            }
            Some(before) => {
                let mut changed = BTreeMap::new();
                for ((column, after), (_, was)) in row.columns.iter().zip(&before.columns) {
                    if after != was {
                        changed.insert(*column, (was.clone(), after.clone()));
                    }
                }
                if !changed.is_empty() {
                    let columns: Vec<&str> = changed.keys().copied().collect();
                    updates.push(row.update(&columns));
                    report.changed.push(DiffEntry {
                        changed,
                        ..entry(row)
                    });
                }
            }
        }
    }
    let mut deletes = Vec::new();
    // Offers before pages, so no offer is left on a deleted page.
    let mut removed: Vec<&ManagerRow> = old_rows
        .iter()
        .filter(|r| ripped_now.contains(&r.kind) && !new_by_key.contains_key(&r.key()))
        // An offer is gone only from a page that was read this time.
        .filter(|r| r.page || offer_page(r).is_some_and(|p| read.contains(&(r.kind, p))))
        .collect();
    removed.sort_by_key(|r| r.page);
    for row in removed {
        if delete_removed {
            deletes.push(row.delete());
        }
        report.removed.push(entry(row));
    }

    let mut text = String::new();
    write_head(&mut text, &report, new, delete_removed);
    for line in deletes.iter().chain(&inserts).chain(&updates) {
        text.push_str(line);
        text.push('\n');
    }
    (report, text)
}

// ---------------------------------------------------------------- the earlier rip, streamed

/// A row id while its catalogue is read: synthetic page ids and renumbered offer ids are known
/// only at its end (they start above the highest id of the catalogue).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Num {
    Id(i64),
    /// The n-th disabled page of the catalogue.
    Synthetic(u32),
    /// The n-th offer of the catalogue that gets a new id.
    Renumbered(u32),
}

impl Num {
    fn id(self) -> i64 {
        match self {
            Num::Id(id) => id,
            _ => unreachable!("resolved at the end of the catalogue"),
        }
    }
}

/// One row of the earlier rip's catalog_manager.sql (Replace layout).
#[derive(Debug)]
struct OldRow {
    kind: &'static str,
    id: Num,
    /// Pages: the parent's id (not in `data`).
    parent: Num,
    /// The column values (pages: all but parent_id), then the label, each as a LEB128 length
    /// and its bytes.
    data: Box<[u8]>,
}

fn pack(values: impl IntoIterator<Item = impl AsRef<str>>) -> Box<[u8]> {
    let mut out = Vec::new();
    for value in values {
        let bytes = value.as_ref().as_bytes();
        let mut n = bytes.len();
        loop {
            let byte = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
        out.extend_from_slice(bytes);
    }
    out.into_boxed_slice()
}

fn unpack(data: &[u8]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let (mut n, mut shift) = (0usize, 0);
        loop {
            let byte = data[i];
            i += 1;
            n |= ((byte & 0x7f) as usize) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                break;
            }
        }
        out.push(std::str::from_utf8(&data[i..i + n]).unwrap_or_default());
        i += n;
    }
    out
}

impl OldRow {
    /// The column values (pages with parent_id first) and the label.
    fn values(&self, page: bool) -> (Vec<String>, String) {
        let mut values: Vec<String> = Vec::new();
        if page {
            values.push(self.parent.id().to_string());
        }
        let mut packed: Vec<String> = unpack(&self.data).into_iter().map(str::to_string).collect();
        let label = packed.pop().unwrap_or_default();
        values.extend(packed);
        (values, label)
    }
}

/// A token of the incremental JSON reader.
#[derive(Debug, Clone, PartialEq)]
enum Tok {
    BeginObject,
    EndObject,
    BeginArray,
    EndArray,
    Key(String),
    Str(String),
    /// A number, as written.
    Num(String),
    Bool(bool),
    Null,
    /// A whole value, as written ([`Lexer::capture`]).
    Raw(Vec<u8>),
}

/// JSON tokens from bytes that come in pieces: a token cut by the end of the bytes so far waits
/// for the next piece ([`Lexer::feed`]). Commas and colons are only separators (the structure is
/// known from the brackets), so this reads well-formed JSON and does not check it.
#[derive(Debug, Default)]
struct Lexer {
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
    /// Open containers: true for an object.
    stack: Vec<bool>,
    want_key: bool,
}

impl Lexer {
    fn feed(&mut self, data: &[u8], eof: bool) {
        self.buf.drain(..self.pos);
        self.pos = 0;
        self.buf.reserve_exact(data.len());
        self.buf.extend_from_slice(data);
        self.eof |= eof;
    }

    fn skip_separators(&mut self) {
        while let Some(&b) = self.buf.get(self.pos) {
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b',' | b':') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// The end of the string that starts at `start` (its closing quote), if it is all here.
    fn string_end(&self, start: usize) -> Option<usize> {
        let mut i = start + 1;
        while i < self.buf.len() {
            match self.buf[i] {
                b'\\' => i += 2,
                b'"' => return Some(i),
                _ => i += 1,
            }
        }
        None
    }

    fn value_done(&mut self) {
        self.want_key = self.stack.last() == Some(&true);
    }

    fn more(&self, what: &str) -> Result<Option<Tok>, String> {
        if self.eof {
            Err(format!("the file ends in the middle of {what}"))
        } else {
            Ok(None)
        }
    }

    fn string_at(&self, start: usize, end: usize) -> Result<String, String> {
        let raw = &self.buf[start..=end];
        if !raw.contains(&b'\\') {
            return String::from_utf8(raw[1..raw.len() - 1].to_vec())
                .map_err(|e| format!("not UTF-8: {e}"));
        }
        serde_json::from_slice(raw).map_err(|e| e.to_string())
    }

    /// The next token; `Ok(None)`: more bytes are needed (or the end, after the last token).
    fn next(&mut self) -> Result<Option<Tok>, String> {
        self.skip_separators();
        let Some(&b) = self.buf.get(self.pos) else {
            return if self.eof && !self.stack.is_empty() {
                Err("the file ends early".into())
            } else {
                Ok(None)
            };
        };
        let start = self.pos;
        let tok = match b {
            b'{' => {
                self.pos += 1;
                self.stack.push(true);
                self.want_key = true;
                return Ok(Some(Tok::BeginObject));
            }
            b'[' => {
                self.pos += 1;
                self.stack.push(false);
                self.want_key = false;
                return Ok(Some(Tok::BeginArray));
            }
            b'}' | b']' => {
                self.pos += 1;
                let object = self.stack.pop();
                if object != Some(b == b'}') {
                    return Err(format!("unexpected '{}'", b as char));
                }
                self.value_done();
                return Ok(Some(if b == b'}' {
                    Tok::EndObject
                } else {
                    Tok::EndArray
                }));
            }
            b'"' => {
                let Some(end) = self.string_end(start) else {
                    return self.more("a string");
                };
                let text = self.string_at(start, end)?;
                self.pos = end + 1;
                if self.want_key {
                    self.want_key = false;
                    return Ok(Some(Tok::Key(text)));
                }
                Tok::Str(text)
            }
            b't' | b'f' | b'n' => {
                let word: &[u8] = match b {
                    b't' => b"true",
                    b'f' => b"false",
                    _ => b"null",
                };
                if self.buf.len() < start + word.len() {
                    return self.more("a literal");
                }
                if &self.buf[start..start + word.len()] != word {
                    return Err("not JSON".into());
                }
                self.pos = start + word.len();
                match b {
                    b't' => Tok::Bool(true),
                    b'f' => Tok::Bool(false),
                    _ => Tok::Null,
                }
            }
            b'-' | b'0'..=b'9' => {
                let mut i = start;
                while i < self.buf.len()
                    && matches!(self.buf[i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    i += 1;
                }
                if i == self.buf.len() && !self.eof {
                    return Ok(None);
                }
                self.pos = i;
                Tok::Num(String::from_utf8_lossy(&self.buf[start..i]).into_owned())
            }
            other => return Err(format!("unexpected '{}'", other as char)),
        };
        self.value_done();
        Ok(Some(tok))
    }

    /// The next value whole, as written (`Raw`), or the end of the container it would be in.
    fn capture(&mut self) -> Result<Option<Tok>, String> {
        self.skip_separators();
        let Some(&b) = self.buf.get(self.pos) else {
            return self.next();
        };
        if !matches!(b, b'{' | b'[') {
            return self.next().map(|t| {
                t.map(|t| match t {
                    Tok::Str(s) => Tok::Raw(serde_json::to_vec(&s).unwrap_or_default()),
                    Tok::Num(n) => Tok::Raw(n.into_bytes()),
                    other => other,
                })
            });
        }
        let start = self.pos;
        let mut depth = 0usize;
        let mut i = start;
        while i < self.buf.len() {
            match self.buf[i] {
                b'"' => match self.string_end(i) {
                    Some(end) => i = end,
                    None => return self.more("a value"),
                },
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        self.pos = i + 1;
                        self.value_done();
                        return Ok(Some(Tok::Raw(self.buf[start..=i].to_vec())));
                    }
                }
                _ => {}
            }
            i += 1;
        }
        self.more("a value")
    }
}

/// What a key of the object being read means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Skip,
    RippedAt,
    Catalogs,
    ModeName,
    Pages,
    Id,
    Name,
    Localization,
    Icon,
    Visible,
    Layout,
    Images,
    Texts,
    Offers,
    PageStatus,
    Children,
}

/// A node (page) of the tree being read.
#[derive(Debug, Default)]
struct NodeState {
    parent: Option<Num>,
    order: usize,
    page_id: i32,
    name: String,
    localization: String,
    icon: i32,
    visible: bool,
    layout: Option<String>,
    images: Vec<String>,
    texts: Vec<String>,
    /// Set once its row is made: its id, and whether its offers are this catalogue's rows (the
    /// first time the page is in the tree).
    numbered: Option<Num>,
    offers_count: bool,
    position: usize,
}

#[derive(Debug)]
enum Ctx {
    Top,
    Catalogs,
    Mode,
    /// An array of nodes, with their parent's id (None: the root) and how many came.
    Children {
        parent: Option<Num>,
        count: usize,
    },
    Node(Box<NodeState>),
    Offers,
    /// A value being skipped, with its open containers.
    Skip(u32),
}

/// The catalogue being read.
#[derive(Debug, Default)]
struct ModeState {
    kind: &'static str,
    max_page_id: i32,
    max_offer_id: i32,
    synthetic: u32,
    renumbered: u32,
    seen_pages: HashSet<i32>,
    seen_offers: HashSet<i32>,
    first_page: usize,
    first_offer: usize,
}

/// The earlier rip's rows, made from its catalog.json as it streams in.
pub struct Digest {
    lexer: Lexer,
    stack: Vec<Ctx>,
    slot: Option<Slot>,
    mode: Option<ModeState>,
    ripped_at: String,
    pages: Vec<OldRow>,
    offers: Vec<OldRow>,
    done: bool,
    stats: ManagerStats,
}

impl std::fmt::Debug for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Digest")
            .field("pages", &self.pages.len())
            .field("offers", &self.offers.len())
            .finish()
    }
}

/// The rows of the earlier rip, once read ([`Digest::finish`]).
pub struct OldRows {
    pub ripped_at: String,
    pages: Vec<OldRow>,
    offers: Vec<OldRow>,
}

impl OldRows {
    pub fn len(&self) -> usize {
        self.pages.len() + self.offers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The items_base and furnidata the rows are made with (those of the new rip's export).
type Lookup<'a> = (Option<&'a ItemsBase>, Option<&'a Furnidata>);

impl Default for Digest {
    fn default() -> Self {
        Digest::new()
    }
}

impl Digest {
    pub fn new() -> Digest {
        Digest {
            lexer: Lexer::default(),
            stack: Vec::new(),
            slot: None,
            mode: None,
            ripped_at: String::new(),
            pages: Vec::new(),
            offers: Vec::new(),
            done: false,
            stats: ManagerStats::default(),
        }
    }

    /// The next piece of catalog.json (`eof`: there is no more). The rows are made with
    /// `items` and `furnidata`: those of the new rip's export, the same on every call.
    pub fn feed(
        &mut self,
        data: &[u8],
        eof: bool,
        items: Option<&ItemsBase>,
        furnidata: Option<&Furnidata>,
    ) -> Result<(), String> {
        self.lexer.feed(data, eof);
        loop {
            let capture = matches!(self.slot, Some(Slot::Images | Slot::Texts))
                || (self.slot.is_none() && matches!(self.stack.last(), Some(Ctx::Offers)));
            let tok = if capture {
                self.lexer.capture()?
            } else {
                self.lexer.next()?
            };
            let Some(tok) = tok else {
                if eof && !self.done {
                    return Err("the file ends early".into());
                }
                return Ok(());
            };
            self.token(tok, (items, furnidata))?;
        }
    }

    /// The rows, once the whole file was fed.
    pub fn finish(self) -> Result<OldRows, String> {
        if !self.done {
            return Err("the file ends early".into());
        }
        Ok(OldRows {
            ripped_at: self.ripped_at,
            pages: self.pages,
            offers: self.offers,
        })
    }

    fn token(&mut self, tok: Tok, lookup: Lookup) -> Result<(), String> {
        let bad = || Err::<(), String>("not a catalog.json of a rip".to_string());
        if let Some(Ctx::Skip(depth)) = self.stack.last_mut() {
            match tok {
                Tok::BeginObject | Tok::BeginArray => *depth += 1,
                Tok::EndObject | Tok::EndArray => {
                    *depth -= 1;
                    if *depth == 0 {
                        self.stack.pop();
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        if let Tok::Key(key) = &tok {
            self.slot = Some(self.slot_of(key));
            return Ok(());
        }
        let slot = self.slot.take();
        // What holds the value: 0 nothing, 1 the catalogues, 2 a list of pages, 3 offers.
        let top = match self.stack.last() {
            None => 0,
            Some(Ctx::Catalogs) => 1,
            Some(Ctx::Children { .. }) => 2,
            Some(Ctx::Offers) => 3,
            Some(_) => 4,
        };
        match (slot, &tok) {
            (Some(Slot::Skip | Slot::PageStatus), Tok::BeginObject | Tok::BeginArray) => {
                if slot == Some(Slot::PageStatus) {
                    self.node_row()?;
                }
                self.stack.push(Ctx::Skip(1));
            }
            (Some(Slot::Skip), _) => {}
            (Some(Slot::PageStatus), _) => {
                self.node_row()?;
            }
            (Some(Slot::RippedAt), Tok::Str(s)) => self.ripped_at = s.clone(),
            (Some(Slot::Catalogs), Tok::BeginArray) => self.stack.push(Ctx::Catalogs),
            (Some(Slot::ModeName), Tok::Str(s)) => {
                if let Some(mode) = self.mode.as_mut() {
                    mode.kind = catalog_type(s);
                }
            }
            (Some(Slot::Pages), Tok::BeginArray) => self.stack.push(Ctx::Children {
                parent: None,
                count: 0,
            }),
            (Some(Slot::Offers), Tok::BeginArray) => {
                self.node_row()?;
                self.stack.push(Ctx::Offers);
            }
            (Some(Slot::Children), Tok::BeginArray) => {
                let parent = self.node_row()?;
                self.stack.push(Ctx::Children {
                    parent: Some(parent),
                    count: 0,
                });
            }
            (Some(slot), _) => {
                let Some(Ctx::Node(node)) = self.stack.last_mut() else {
                    return bad();
                };
                let num = |t: &Tok| match t {
                    Tok::Num(n) => n.parse::<i32>().map_err(|e| e.to_string()),
                    _ => Err("a number was expected".to_string()),
                };
                let text = |t: &Tok| match t {
                    Tok::Str(s) => Ok(s.clone()),
                    _ => Err("a text was expected".to_string()),
                };
                let list = |t: &Tok| match t {
                    Tok::Raw(raw) => {
                        serde_json::from_slice::<Vec<String>>(raw).map_err(|e| e.to_string())
                    }
                    _ => Err("a list was expected".to_string()),
                };
                match slot {
                    Slot::Id => node.page_id = num(&tok)?,
                    Slot::Name => node.name = text(&tok)?,
                    Slot::Localization => node.localization = text(&tok)?,
                    Slot::Icon => node.icon = num(&tok)?,
                    Slot::Visible => node.visible = tok == Tok::Bool(true),
                    Slot::Layout => node.layout = Some(text(&tok)?),
                    Slot::Images => node.images = list(&tok)?,
                    Slot::Texts => node.texts = list(&tok)?,
                    _ => return bad(),
                }
            }
            (None, Tok::BeginObject) => match top {
                0 => self.stack.push(Ctx::Top),
                1 => {
                    self.mode = Some(ModeState {
                        kind: "NORMAL",
                        first_page: self.pages.len(),
                        first_offer: self.offers.len(),
                        ..ModeState::default()
                    });
                    self.stack.push(Ctx::Mode);
                }
                2 => {
                    let Some(Ctx::Children { parent, count }) = self.stack.last_mut() else {
                        return bad();
                    };
                    *count += 1;
                    let node = NodeState {
                        parent: *parent,
                        order: *count,
                        ..NodeState::default()
                    };
                    self.stack.push(Ctx::Node(Box::new(node)));
                }
                _ => return bad(),
            },
            (None, Tok::Raw(raw)) if top == 3 => {
                let offer: Offer =
                    serde_json::from_slice(raw).map_err(|e| format!("an offer: {e}"))?;
                self.offer(&offer, lookup)?;
            }
            (None, Tok::EndObject) => match self.stack.pop() {
                Some(Ctx::Top) => self.done = true,
                Some(Ctx::Mode) => self.end_mode(),
                Some(Ctx::Node(node)) => {
                    if node.numbered.is_none() {
                        self.stack.push(Ctx::Node(node));
                        self.node_row()?;
                        self.stack.pop();
                    }
                }
                _ => return bad(),
            },
            (None, Tok::EndArray) => match self.stack.pop() {
                Some(Ctx::Catalogs | Ctx::Children { .. } | Ctx::Offers) => {}
                _ => return bad(),
            },
            _ => return bad(),
        }
        Ok(())
    }

    fn slot_of(&self, key: &str) -> Slot {
        match (self.stack.last(), key) {
            (Some(Ctx::Top), "ripped_at") => Slot::RippedAt,
            (Some(Ctx::Top), "catalogs") => Slot::Catalogs,
            (Some(Ctx::Mode), "mode") => Slot::ModeName,
            (Some(Ctx::Mode), "pages") => Slot::Pages,
            (Some(Ctx::Node(_)), key) => match key {
                "id" => Slot::Id,
                "name" => Slot::Name,
                "localization" => Slot::Localization,
                "icon" => Slot::Icon,
                "visible" => Slot::Visible,
                "layout" => Slot::Layout,
                "images" => Slot::Images,
                "texts" => Slot::Texts,
                "offers" => Slot::Offers,
                "page_status" => Slot::PageStatus,
                "children" => Slot::Children,
                _ => Slot::Skip,
            },
            _ => Slot::Skip,
        }
    }

    /// The row of the node being read (once, when its fields are known: before its offers and
    /// children, as [`crate::export::numbered_pages`] numbers the tree depth first). Returns its id.
    fn node_row(&mut self) -> Result<Num, String> {
        let Some(Ctx::Node(node)) = self.stack.last_mut() else {
            return Err("not a catalog.json of a rip".into());
        };
        if let Some(id) = node.numbered {
            return Ok(id);
        }
        let Some(mode) = self.mode.as_mut() else {
            return Err("not a catalog.json of a rip".into());
        };
        mode.max_page_id = mode.max_page_id.max(node.page_id);
        let id = if node.page_id > 0 {
            Num::Id(node.page_id as i64)
        } else {
            mode.synthetic += 1;
            Num::Synthetic(mode.synthetic - 1)
        };
        node.numbered = Some(id);
        // A page id that is in the tree twice: only the first makes rows.
        let first = node.page_id <= 0 || mode.seen_pages.insert(node.page_id);
        node.offers_count = first && node.layout.is_some();
        if first {
            let columns = page_columns(
                &PageFields {
                    parent_id: 0,
                    name: &node.name,
                    localization: &node.localization,
                    icon: node.icon,
                    order: node.order,
                    visible: node.visible,
                    enabled: node.page_id > 0,
                    content: node
                        .layout
                        .as_deref()
                        .map(|layout| (layout, &node.images[..], &node.texts[..])),
                },
                mode.kind,
                &mut self.stats,
            );
            let label = if node.localization.is_empty() {
                &node.name
            } else {
                &node.localization
            };
            let values = columns.iter().skip(1).map(|(_, v)| v.as_str());
            self.pages.push(OldRow {
                kind: mode.kind,
                id,
                parent: node.parent.unwrap_or(Num::Id(-1)),
                data: pack(values.chain(std::iter::once(label.as_str()))),
            });
        }
        // The content is not needed any more.
        node.images = Vec::new();
        node.texts = Vec::new();
        Ok(id)
    }

    fn offer(&mut self, offer: &Offer, (items, furnidata): Lookup) -> Result<(), String> {
        let n = self.stack.len();
        let Some(Ctx::Node(node)) = n.checked_sub(2).and_then(|i| self.stack.get_mut(i)) else {
            return Err("not a catalog.json of a rip".into());
        };
        let Some(mode) = self.mode.as_mut() else {
            return Err("not a catalog.json of a rip".into());
        };
        let position = node.position;
        node.position += 1;
        mode.max_offer_id = mode.max_offer_id.max(offer.offer_id);
        if !node.offers_count || offer.products.is_empty() {
            return Ok(());
        }
        let ids = item_ids_sql_with(offer, items, furnidata);
        if ids.unresolved > 0 {
            return Ok(());
        }
        let page_id = node.numbered.map(Num::id).unwrap_or_default();
        let id = if offer.offer_id > 0 && mode.seen_offers.insert(offer.offer_id) {
            Num::Id(offer.offer_id as i64)
        } else {
            mode.renumbered += 1;
            Num::Renumbered(mode.renumbered - 1)
        };
        let columns = offer_columns(
            offer,
            &literal_item_ids(&ids.sql),
            page_id,
            position,
            offer.offer_id,
            &mut self.stats,
        );
        let values = columns.iter().map(|(_, v)| v.as_str());
        self.offers.push(OldRow {
            kind: mode.kind,
            id,
            parent: Num::Id(0),
            data: pack(values.chain(std::iter::once(offer.localization_id.as_str()))),
        });
        Ok(())
    }

    /// The ids known only at the end of a catalogue.
    fn end_mode(&mut self) {
        let Some(mode) = self.mode.take() else {
            return;
        };
        let first_free = mode.max_page_id.max(0) as i64 + 1;
        let next_offer = mode.max_offer_id.max(0) as i64 + 1;
        let resolve = |n: Num| match n {
            Num::Id(id) => Num::Id(id),
            Num::Synthetic(k) => Num::Id(first_free + k as i64),
            Num::Renumbered(k) => Num::Id(next_offer + k as i64),
        };
        for row in &mut self.pages[mode.first_page..] {
            row.id = resolve(row.id);
            row.parent = resolve(row.parent);
        }
        for row in &mut self.offers[mode.first_offer..] {
            row.id = resolve(row.id);
        }
    }
}

/// What the new rip changed, and how to write catalog_manager_changes.sql from it.
#[derive(Debug, Clone)]
pub struct Changes {
    pub report: CatalogDiff,
    pub delete_removed: bool,
    /// Per row of the new rip (in catalog_manager.sql's order): 1 inserted, 2 updated, 0 neither.
    decisions: Vec<u8>,
}

impl Changes {
    /// `Since the rip of <date> (<folder>): <summary>.`
    pub fn log_line(&self) -> String {
        format!(
            "Since the rip of {} ({}): {}.",
            self.report.since_ripped_at,
            self.report.since.as_deref().unwrap_or_default(),
            self.report.summary()
        )
    }
}

/// The new rip's rows in catalog_manager.sql's order (pages, then offers), one by one.
fn visit_new(catalog: &RippedCatalog, export: &ManagerExport, emit: &mut dyn FnMut(ManagerRow)) {
    visit_manager_rows(catalog, export, true, false, emit);
    visit_manager_rows(catalog, export, false, true, emit);
}

/// Compares the new rip with the earlier one's rows: the report of [`diff`] (with `since`).
pub fn compare(
    old: OldRows,
    since: &str,
    new: &RippedCatalog,
    items: Option<&ItemsBase>,
    furnidata: Option<&Furnidata>,
    delete_removed: bool,
) -> Changes {
    let export = replace_export(items, furnidata);
    let mut by_key: HashMap<(bool, &'static str, i64), (bool, usize)> =
        HashMap::with_capacity(old.len());
    for (page, rows) in [(true, &old.pages), (false, &old.offers)] {
        for (i, row) in rows.iter().enumerate() {
            by_key.insert((page, row.kind, row.id.id()), (page, i));
        }
    }
    let ripped_now: Vec<&str> = new.modes.iter().map(|m| catalog_type(&m.mode)).collect();
    let mut report = CatalogDiff {
        since: Some(since.to_string()),
        since_ripped_at: old.ripped_at.clone(),
        ..CatalogDiff::default()
    };
    let mut decisions = Vec::new();
    let mut new_keys: HashSet<(bool, &'static str, i64)> = HashSet::with_capacity(old.len());
    let mut read: HashSet<(&'static str, i64)> = HashSet::new();
    visit_new(new, &export, &mut |row| {
        new_keys.insert(row.key());
        if row.page && row.has_content {
            read.insert((row.kind, row.id));
        }
        let mut decision = 0;
        match by_key.get(&row.key()) {
            Some(_) if row.page && !row.has_content => {}
            None => {
                decision = 1;
                report.added.push(entry(&row));
            }
            Some(&(page, i)) => {
                let before = if page { &old.pages[i] } else { &old.offers[i] };
                let (values, _) = before.values(page);
                let mut changed = BTreeMap::new();
                for ((column, after), was) in row.columns.iter().zip(values) {
                    if *after != was {
                        changed.insert(*column, (was, after.clone()));
                    }
                }
                if !changed.is_empty() {
                    decision = 2;
                    report.changed.push(DiffEntry {
                        changed,
                        ..entry(&row)
                    });
                }
            }
        }
        decisions.push(decision);
    });
    let mut removed: Vec<DiffEntry> = Vec::new();
    // Offers before pages, so no offer is left on a deleted page.
    for page in [false, true] {
        let rows = if page { &old.pages } else { &old.offers };
        for row in rows {
            let id = row.id.id();
            if !ripped_now.contains(&row.kind) || new_keys.contains(&(page, row.kind, id)) {
                continue;
            }
            let (values, label) = row.values(page);
            // An offer is gone only from a page that was read this time.
            if !page {
                let on_read_page = values
                    .get(1)
                    .and_then(|p| p.parse::<i64>().ok())
                    .is_some_and(|p| read.contains(&(row.kind, p)));
                if !on_read_page {
                    continue;
                }
            }
            removed.push(DiffEntry {
                what: if page { "page" } else { "offer" },
                catalog_type: row.kind,
                id,
                label,
                changed: BTreeMap::new(),
            });
        }
    }
    report.removed = removed;
    Changes {
        report,
        delete_removed,
        decisions,
    }
}

/// catalog_manager_changes.sql into `out`, the bytes [`diff`] makes: the head, the DELETEs (when
/// asked), the INSERTs, the UPDATEs. The new rows are made again (once); the UPDATE lines wait in
/// memory until the INSERTs are written.
pub fn write_changes_sql(
    out: &mut dyn fmt::Write,
    changes: &Changes,
    new: &RippedCatalog,
    items: Option<&ItemsBase>,
    furnidata: Option<&Furnidata>,
) {
    let report = &changes.report;
    write_head(out, report, new, changes.delete_removed);
    if changes.delete_removed {
        for removed in &report.removed {
            let _ = writeln!(out, "{}", removed.delete());
        }
    }
    let export = replace_export(items, furnidata);
    let mut updates = Vec::new();
    let mut changed = report.changed.iter();
    let mut n = 0;
    visit_new(new, &export, &mut |row| {
        match changes.decisions.get(n) {
            Some(1) => {
                let _ = writeln!(out, "{}", row.insert());
            }
            Some(2) => {
                if let Some(entry) = changed.next() {
                    debug_assert_eq!(entry.key(), row.key());
                    let columns: Vec<&str> = entry.changed.keys().copied().collect();
                    updates.push(row.update(&columns));
                }
            }
            _ => {}
        }
        n += 1;
    });
    for line in updates {
        let _ = writeln!(out, "{line}");
    }
}

// ---------------------------------------------------------------- the earlier rip

/// The catalog.json of a rip folder.
pub fn catalog_json_path(folder: &str) -> String {
    source::join(folder, "catalog.json")
}

/// Finds the newest earlier rip of a hotel in the top folder of a [`Source`]: a folder named
/// `<hotel>_...` (other than `skip`) with a catalog.json (the built-in's `previous_rip` looked
/// for `packets/session.json`: the folders it could read). A few calls per [`PreviousRip::step`].
#[derive(Debug)]
pub struct PreviousRip {
    prefix: String,
    skip: Vec<String>,
    listed: Vec<SourceEntry>,
    candidates: Option<Vec<String>>,
}

impl PreviousRip {
    /// `hotel`: [`crate::names::hotel_name`] of the host.
    pub fn new(hotel: &str, skip: Vec<String>) -> PreviousRip {
        PreviousRip {
            prefix: format!("{hotel}_"),
            skip,
            listed: Vec::new(),
            candidates: None,
        }
    }

    /// One call to the source. `Ok(Some(Some(folder)))`: found; `Ok(Some(None))`: there is none.
    pub fn step(&mut self, source: &mut dyn Source) -> Result<Option<Option<String>>, String> {
        let Some(candidates) = self.candidates.as_mut() else {
            let page = source.list("", self.listed.len() as u32)?;
            if !page.is_empty() {
                self.listed.extend(page);
                return Ok(None);
            }
            let mut names: Vec<String> = std::mem::take(&mut self.listed)
                .into_iter()
                .filter(|e| {
                    e.is_folder && e.name.starts_with(&self.prefix) && !self.skip.contains(&e.name)
                })
                .map(|e| e.name)
                .collect();
            names.sort();
            self.candidates = Some(names);
            return Ok(None);
        };
        let Some(folder) = candidates.pop() else {
            return Ok(Some(None));
        };
        let has_catalog = source::list_all(source, &folder)
            .map(|entries| {
                entries
                    .iter()
                    .any(|e| !e.is_folder && e.name == "catalog.json")
            })
            .unwrap_or(false);
        Ok(has_catalog.then_some(Some(folder)))
    }
}

/// The README section about the changes files (the built-in's text).
pub fn readme_section(report: &CatalogDiff, delete_removed: bool) -> String {
    format!(
        "\nCHANGES (catalog_changes.json, catalog_manager_changes.sql)\n  Since the rip of {}: {}.\n  catalog_manager_changes.sql brings a hotel that holds that earlier rip (imported with its ids)\n  up to date: it adds the new pages and offers and updates the changed ones{}.\n",
        report.since_ripped_at,
        report.summary(),
        if delete_removed {
            ", and deletes the removed ones"
        } else {
            "; removed ones are kept"
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries() {
        let e = |what| DiffEntry {
            what,
            catalog_type: "NORMAL",
            id: 1,
            label: String::new(),
            changed: BTreeMap::new(),
        };
        let mut d = CatalogDiff::default();
        assert_eq!(d.summary(), "no changes");
        d.added = vec![e("page"), e("offer"), e("offer")];
        d.removed = vec![e("page")];
        assert_eq!(d.summary(), "1 page and 2 offers added, 1 page removed");
    }

    #[test]
    fn packs_values() {
        let long = "x".repeat(300);
        let data = pack(["a", "", long.as_str(), "é'"]);
        assert_eq!(unpack(&data), ["a", "", long.as_str(), "é'"]);
    }

    #[test]
    fn lexer_waits_for_cut_tokens() {
        // "d" is an escaped e-acute.
        let text = [
            br#"{"a": [1, -2.5e3, "x\"y", true, null, {"b": "c"}], "d": "\u"#.as_slice(),
            b"00e9\"}",
        ]
        .concat();
        let mut all = Vec::new();
        // Every cut of the text gives the same tokens.
        for cut in 0..text.len() {
            let mut lexer = Lexer::default();
            let mut toks = Vec::new();
            for (piece, eof) in [(&text[..cut], false), (&text[cut..], true)] {
                lexer.feed(piece, eof);
                while let Some(t) = lexer.next().unwrap() {
                    toks.push(t);
                }
            }
            if all.is_empty() {
                all = toks.clone();
            }
            assert_eq!(toks, all, "cut at {cut}");
        }
        assert_eq!(all[0], Tok::BeginObject);
        assert_eq!(all[1], Tok::Key("a".into()));
        assert_eq!(all[4], Tok::Num("-2.5e3".into()));
        assert_eq!(all[5], Tok::Str("x\"y".into()));
        assert_eq!(all.last(), Some(&Tok::EndObject));
        assert!(all.contains(&Tok::Str("é".into())));
        let mut lexer = Lexer::default();
        lexer.feed(br#"[{"x": [1, "]"]}, 2]"#, true);
        assert_eq!(lexer.next().unwrap(), Some(Tok::BeginArray));
        assert_eq!(
            lexer.capture().unwrap(),
            Some(Tok::Raw(br#"{"x": [1, "]"]}"#.to_vec()))
        );
        assert_eq!(lexer.capture().unwrap(), Some(Tok::Raw(b"2".to_vec())));
        assert_eq!(lexer.capture().unwrap(), Some(Tok::EndArray));
        let mut lexer = Lexer::default();
        lexer.feed(b"{\"a\": [1", true);
        assert!(std::iter::from_fn(|| lexer.next().transpose()).any(|t| t.is_err()));
    }
}

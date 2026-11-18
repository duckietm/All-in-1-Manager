//! What a rip writes, as bytes: the catalogue assembled from the answers, the data files
//! (catalog.json, the Arcturus SQL, catalog_manager.sql, missing_furni.json,
//! missing_furnidata.json, catalog_changes.json, catalog_manager_changes.sql,
//! catalog_extras.json) and README.txt. The job hands them to its `Sink`; nothing here does I/O.
//! (`Output::write` and `recording::assemble` of the built-in crate; the recorded packets are
//! [`crate::recording`]'s.)

use std::collections::{BTreeMap, HashMap, HashSet};
use std::{fmt, io};

use crate::diff::{self, CatalogDiff, Changes};
use crate::export::{self, ItemsBase, RippedCatalog, RippedMode, SqlFile, SqlStats};
use crate::extras;
use crate::furnidata::Furnidata;
use crate::manager::{self, ExistingCatalogue, ManagerExport, ManagerMode, ManagerStats};
use crate::missing::{self, MissingFurni};
use crate::packets::{CatalogIndex, CatalogPage};
use crate::productdata::Productdata;
use crate::profiles::HotelProfile;
use crate::ItemsBaseRow;

/// items_base rows as the lookup the exporters use (what the built-in did with the rows its
/// `ItemsSource` returned).
pub fn items_base(rows: &[ItemsBaseRow]) -> ItemsBase {
    let mut items = ItemsBase::default();
    for row in rows {
        items.add(
            row.id,
            row.sprite_id,
            &row.item_type,
            &row.item_name,
            row.effect_id,
        );
    }
    items
}

/// The hotel connection a catalogue was ripped from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source<'a> {
    pub host: &'a str,
    pub hotel_version: &'a str,
    pub client_type: &'a str,
    /// `YYYY-mm-dd HH:MM:SS`.
    pub ripped_at: &'a str,
    /// What makes the files ([`RippedCatalog::generator`]).
    pub generator: &'a str,
}

/// The catalogue as the rip writes it: per catalogue in `modes` that has an index, every page
/// of its tree that answered; `failed` lists the pages without an answer.
///
/// The indexes and pages are moved into it (a page no tree lists is dropped): the catalogue is
/// the biggest thing a rip holds, and is not kept twice.
pub fn assemble(
    source: &Source,
    modes: &[String],
    mut indexes: HashMap<String, CatalogIndex>,
    mut pages: HashMap<(String, i32), CatalogPage>,
    failed: &BTreeMap<String, Vec<i32>>,
    complete: bool,
) -> RippedCatalog {
    let mut out = Vec::new();
    for mode in modes {
        let Some(index) = indexes.remove(mode) else {
            continue;
        };
        let mut found = HashMap::new();
        for (node, _, _) in index.walk() {
            if node.page_id > 0 && !found.contains_key(&node.page_id) {
                if let Some(page) = pages.remove(&(mode.clone(), node.page_id)) {
                    found.insert(node.page_id, page);
                }
            }
        }
        out.push(RippedMode {
            mode: mode.clone(),
            index,
            pages: found,
            failed: failed.get(mode).cloned().unwrap_or_default(),
        });
    }
    RippedCatalog {
        hotel: crate::names::hotel_name(source.host),
        host: source.host.to_string(),
        hotel_version: source.hotel_version.to_string(),
        client_type: source.client_type.to_string(),
        ripped_at: source.ripped_at.to_string(),
        generator: source.generator.to_string(),
        complete,
        modes: out,
    }
}

/// What the data files are written from.
#[derive(Debug, Clone, Copy)]
pub struct ExportInput<'a> {
    pub catalog: &'a RippedCatalog,
    pub items: Option<&'a ItemsBase>,
    pub furnidata: Option<&'a Furnidata>,
    pub products: Option<&'a Productdata>,
    pub manager_mode: ManagerMode,
    pub existing: Option<&'a ExistingCatalogue>,
    /// The answers of the extra messages, by [`extras::Extra::key`].
    pub extra_answers: &'a BTreeMap<String, Vec<u8>>,
    /// What changed since an earlier rip (catalog_changes.json and
    /// catalog_manager_changes.sql); [`data_files`] does not write them.
    pub changes: Option<&'a Changes>,
}

/// The data files, in the order the built-in writes them, with what the README and the summary
/// need.
#[derive(Debug, Clone)]
pub struct DataFiles {
    /// (file name, bytes).
    pub files: Vec<(String, Vec<u8>)>,
    pub sql: SqlStats,
    pub manager: ManagerStats,
    pub missing_furni: Vec<MissingFurni>,
    /// Log lines the built-in writes while making them.
    pub log: Vec<String>,
}

fn pretty(value: &impl serde::Serialize) -> Vec<u8> {
    serde_json::to_string_pretty(value)
        .unwrap_or_default()
        .into_bytes()
}

/// catalog.json, catalog_pages.sql, catalog_items.sql (+ the `_bc` pair when the Builders Club
/// was ripped), catalog_manager.sql, missing_furni.json and missing_furnidata.json (when
/// products are missing; the latter only with a furnidata), catalog_extras.json (when an extra
/// answered).
///
/// All at once, through the built-in's JSON trees and lists of rows: the reference for
/// [`DataExport`], which the job uses (one file at a time, the same bytes, a fraction of the
/// memory). Without the changes files (their reference is [`diff::diff`]).
pub fn data_files(input: &ExportInput) -> DataFiles {
    let catalog = input.catalog;
    let mut files = Vec::new();
    let mut log = Vec::new();
    let mut json = export::catalog_json(catalog);
    if let Some(products) = input.products {
        json["product_names"] = product_names(catalog, input.furnidata, products);
    }
    files.push(("catalog.json".to_string(), pretty(&json)));
    let sql = export::arcturus_sql_with(catalog, input.items, input.furnidata);
    files.push(("catalog_pages.sql".into(), sql.pages.into_bytes()));
    files.push(("catalog_items.sql".into(), sql.items.into_bytes()));
    if !sql.pages_bc.is_empty() {
        files.push(("catalog_pages_bc.sql".into(), sql.pages_bc.into_bytes()));
        files.push(("catalog_items_bc.sql".into(), sql.items_bc.into_bytes()));
    }
    // The hotel's ids are only asked for in the modes that keep what is there.
    let existing = if input.manager_mode == ManagerMode::Replace {
        None
    } else {
        input.existing
    };
    let manager = manager::manager_sql_with(
        catalog,
        &ManagerExport {
            items: input.items,
            furnidata: input.furnidata,
            mode: input.manager_mode,
            existing,
        },
    );
    if !manager.stats.note.is_empty() {
        log.push(format!("Manager import file: {}", manager.stats.note));
    }
    files.push(("catalog_manager.sql".into(), manager.text.into_bytes()));

    let missing_furni =
        missing::missing_furni(catalog, input.items, input.furnidata, input.products);
    if !missing_furni.is_empty() {
        log.push(format!(
            "{} products are not in the hotel's items_base: missing_furni.json{}.",
            n0(missing_furni.len()),
            if input.furnidata.is_some() {
                " and missing_furnidata.json (for the furni import)"
            } else {
                ""
            }
        ));
        files.push((
            "missing_furni.json".into(),
            pretty(&serde_json::json!({
                "count": missing_furni.len(),
                "furni": missing_furni,
            })),
        ));
        if let Some(data) = input.furnidata {
            files.push((
                "missing_furnidata.json".into(),
                pretty(&missing::missing_furnidata(&missing_furni, data)),
            ));
        }
    }
    if !input.extra_answers.is_empty() {
        files.push((
            "catalog_extras.json".into(),
            pretty(&extras::extras_json(input.extra_answers)),
        ));
    }
    DataFiles {
        files,
        sql: sql.stats,
        manager: manager.stats,
        missing_furni,
        log,
    }
}

// ---------------------------------------------------------------- one file at a time

/// A data file of the rip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataFile {
    Catalog,
    Sql(SqlFile),
    Manager,
    MissingFurni,
    MissingFurnidata,
    /// catalog_changes.json.
    Changes,
    /// catalog_manager_changes.sql.
    ManagerChanges,
    Extras,
}

impl DataFile {
    pub fn name(self) -> &'static str {
        match self {
            DataFile::Catalog => "catalog.json",
            DataFile::Sql(file) => file.name(),
            DataFile::Manager => "catalog_manager.sql",
            DataFile::MissingFurni => "missing_furni.json",
            DataFile::MissingFurnidata => "missing_furnidata.json",
            DataFile::Changes => "catalog_changes.json",
            DataFile::ManagerChanges => "catalog_manager_changes.sql",
            DataFile::Extras => "catalog_extras.json",
        }
    }
}

/// The data files in the built-in's order (as [`data_files`] lists them).
const LISTED: [DataFile; 11] = [
    DataFile::Catalog,
    DataFile::Sql(SqlFile::Pages),
    DataFile::Sql(SqlFile::Items),
    DataFile::Sql(SqlFile::PagesBc),
    DataFile::Sql(SqlFile::ItemsBc),
    DataFile::Manager,
    DataFile::MissingFurni,
    DataFile::MissingFurnidata,
    DataFile::Changes,
    DataFile::ManagerChanges,
    DataFile::Extras,
];

/// The order [`DataExport`] makes them in: catalog.json, the biggest file, last, once the
/// furnidata entries are no longer needed.
const MADE: [DataFile; 11] = [
    DataFile::Sql(SqlFile::Pages),
    DataFile::Sql(SqlFile::Items),
    DataFile::Sql(SqlFile::PagesBc),
    DataFile::Sql(SqlFile::ItemsBc),
    DataFile::Manager,
    DataFile::MissingFurni,
    DataFile::MissingFurnidata,
    DataFile::Changes,
    DataFile::ManagerChanges,
    DataFile::Extras,
    DataFile::Catalog,
];

/// One data file made by [`DataExport::next_file`].
#[derive(Debug, Clone)]
pub struct MadeFile {
    pub file: DataFile,
    pub bytes: Vec<u8>,
    /// Log lines the built-in writes while making it.
    pub log: Vec<String>,
}

/// One data file streamed by [`DataExport::stream_next_file`].
#[derive(Debug, Clone)]
pub struct StreamedFile {
    pub file: DataFile,
    /// The first error of `append`: the file stopped there (part of it may be written).
    pub result: Result<(), String>,
    /// Log lines the built-in writes while making it.
    pub log: Vec<String>,
}

/// The data files of [`data_files`], made one per call of [`DataExport::stream_next_file`] (or
/// [`DataExport::next_file`]) with the same bytes: each is serialized straight from the
/// catalogue (no JSON tree, no list of rows) and handed on in pieces of [`CHUNK`] bytes, so the
/// plugin never holds a whole file.
#[derive(Debug, Clone, Default)]
pub struct DataExport {
    next: usize,
    products_unresolved: Option<usize>,
    made: Vec<DataFile>,
    /// Filled as the SQL files are made.
    pub sql: SqlStats,
    /// Filled when catalog_manager.sql is made.
    pub manager: ManagerStats,
    /// Filled when missing_furni.json is due (also when it is not written: nothing missing).
    pub missing_furni: Vec<MissingFurni>,
}

/// The size of the pieces a data file is streamed in.
pub const CHUNK: usize = 1024 * 1024;

/// Where a streamed file's pieces go: `append(file name, bytes)`; the first call creates the file.
pub type Append<'a> = &'a mut dyn FnMut(&str, &[u8]) -> Result<(), String>;

/// A data file being streamed: what is written is buffered and handed to `append` in pieces of
/// at most [`CHUNK`] bytes (a single larger write goes as one piece). After the first error
/// nothing more is sent, and the writers report an error so the serializers stop.
struct Chunks<'a, 'b> {
    name: &'static str,
    append: &'a mut Append<'b>,
    buf: Vec<u8>,
    sent: bool,
    error: Option<String>,
}

impl<'a, 'b> Chunks<'a, 'b> {
    fn new(name: &'static str, append: &'a mut Append<'b>) -> Self {
        Self {
            name,
            append,
            buf: Vec::new(),
            sent: false,
            error: None,
        }
    }

    fn send(&mut self, data: &[u8]) -> bool {
        if self.error.is_some() {
            return false;
        }
        self.sent = true;
        if let Err(e) = (self.append)(self.name, data) {
            self.error = Some(e);
            return false;
        }
        true
    }

    fn push(&mut self, data: &[u8]) -> bool {
        if self.error.is_some() {
            return false;
        }
        if self.buf.len() + data.len() > CHUNK && !self.buf.is_empty() {
            let mut buf = std::mem::take(&mut self.buf);
            let ok = self.send(&buf);
            buf.clear();
            self.buf = buf;
            if !ok {
                return false;
            }
        }
        if data.len() >= CHUNK {
            return self.send(data);
        }
        if self.buf.capacity() == 0 {
            self.buf.reserve_exact(CHUNK);
        }
        self.buf.extend_from_slice(data);
        true
    }

    /// Sends the rest (and creates an empty file when nothing was sent).
    fn finish(mut self) -> Result<(), String> {
        if !self.buf.is_empty() || !self.sent {
            let buf = std::mem::take(&mut self.buf);
            self.send(&buf);
        }
        match self.error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// Writes `value` as pretty JSON into `out`. A serializer error (not only a failed append) fails
/// the file: [`Chunks::finish`] returns it, unless an append error came first.
fn write_json<T: serde::Serialize + ?Sized>(out: &mut Chunks, value: &T) {
    if let Err(e) = serde_json::to_writer_pretty(&mut *out, value) {
        if out.error.is_none() {
            out.error = Some(format!("{} could not be written as JSON: {e}", out.name));
        }
    }
}

impl fmt::Write for Chunks<'_, '_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.push(s.as_bytes()) {
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }
}

impl io::Write for Chunks<'_, '_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.push(buf) {
            Ok(buf.len())
        } else {
            Err(io::Error::other("append failed"))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// missing_furni.json.
#[derive(serde::Serialize)]
struct MissingJson<'a> {
    count: usize,
    furni: &'a [MissingFurni],
}

impl DataExport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes the next data file whole (the bytes [`stream_next_file`](Self::stream_next_file)
    /// streams).
    pub fn next_file(&mut self, input: &ExportInput) -> Option<MadeFile> {
        let mut bytes = Vec::new();
        let made = self.stream_next_file(input, &mut |_, data| {
            bytes.extend_from_slice(data);
            Ok(())
        })?;
        Some(MadeFile {
            file: made.file,
            bytes,
            log: made.log,
        })
    }

    /// Streams the next data file (one file at most) to `append`, in pieces of at most
    /// [`CHUNK`] bytes; `None` once every file is made. A file with nothing to say (the `_bc`
    /// pair without a Builders Club catalogue, the missing furni without missing products, the
    /// extras without answers) is skipped.
    pub fn stream_next_file(&mut self, input: &ExportInput, mut append: Append) -> Option<StreamedFile> {
        let catalog = input.catalog;
        while let Some(&file) = MADE.get(self.next) {
            self.next += 1;
            let mut log = Vec::new();
            let mut out = Chunks::new(file.name(), &mut append);
            match file {
                DataFile::Sql(sql) => {
                    let bc = catalog
                        .modes
                        .iter()
                        .any(|m| !m.mode.eq_ignore_ascii_case("NORMAL"));
                    if matches!(sql, SqlFile::PagesBc | SqlFile::ItemsBc) && !bc {
                        continue;
                    }
                    let unresolved = *self.products_unresolved.get_or_insert_with(|| {
                        export::products_unresolved(catalog, input.items, input.furnidata)
                    });
                    export::write_arcturus_file(
                        &mut out,
                        catalog,
                        input.items,
                        input.furnidata,
                        sql,
                        unresolved,
                        &mut self.sql,
                    );
                }
                DataFile::Manager => {
                    let export = ManagerExport {
                        items: input.items,
                        furnidata: input.furnidata,
                        mode: input.manager_mode,
                        existing: if input.manager_mode == ManagerMode::Replace {
                            None
                        } else {
                            input.existing
                        },
                    };
                    self.manager = manager::write_manager_sql(&mut out, catalog, &export);
                    if !self.manager.note.is_empty() {
                        log.push(format!("Manager import file: {}", self.manager.note));
                    }
                }
                DataFile::MissingFurni => {
                    self.missing_furni = missing::missing_furni(
                        catalog,
                        input.items,
                        input.furnidata,
                        input.products,
                    );
                    if self.missing_furni.is_empty() {
                        continue;
                    }
                    log.push(format!(
                        "{} products are not in the hotel's items_base: missing_furni.json{}.",
                        n0(self.missing_furni.len()),
                        if input.furnidata.is_some() {
                            " and missing_furnidata.json (for the furni import)"
                        } else {
                            ""
                        }
                    ));
                    write_json(
                        &mut out,
                        &MissingJson {
                            count: self.missing_furni.len(),
                            furni: &self.missing_furni,
                        },
                    );
                }
                DataFile::MissingFurnidata => {
                    let Some(data) = input.furnidata.filter(|_| !self.missing_furni.is_empty())
                    else {
                        continue;
                    };
                    let wanted = missing::missing_keys(&self.missing_furni);
                    write_json(&mut out, &data.subset_json(&wanted));
                }
                DataFile::Changes => {
                    let Some(changes) = input.changes else {
                        continue;
                    };
                    log.push(changes.log_line());
                    write_json(&mut out, &changes.report);
                }
                DataFile::ManagerChanges => {
                    let Some(changes) = input.changes else {
                        continue;
                    };
                    diff::write_changes_sql(
                        &mut out,
                        changes,
                        catalog,
                        input.items,
                        input.furnidata,
                    );
                }
                DataFile::Extras => {
                    if input.extra_answers.is_empty() {
                        continue;
                    }
                    write_json(&mut out, &extras::extras_json(input.extra_answers));
                }
                DataFile::Catalog => {
                    write_json(
                        &mut out,
                        &export::CatalogJson {
                            catalog,
                            extra: input.products.map(|products| {
                                (
                                    "product_names",
                                    ProductNamesJson {
                                        catalog,
                                        furnidata: input.furnidata,
                                        products,
                                    },
                                )
                            }),
                        },
                    );
                }
            }
            let result = out.finish();
            self.made.push(file);
            return Some(StreamedFile { file, result, log });
        }
        None
    }

    /// Whether a file still to make reads the hotel's items_base (the item SQL files,
    /// catalog_manager.sql, missing_furni.json, catalog_manager_changes.sql).
    pub fn needs_items_base(&self) -> bool {
        MADE[self.next.min(MADE.len())..].iter().any(|f| {
            matches!(
                f,
                DataFile::Sql(_)
                    | DataFile::Manager
                    | DataFile::MissingFurni
                    | DataFile::ManagerChanges
            )
        })
    }

    /// Whether a file still to make needs the furnidata's entries (missing_furnidata.json).
    pub fn needs_furnidata_entries(&self) -> bool {
        MADE[self.next.min(MADE.len())..].contains(&DataFile::MissingFurnidata)
    }

    /// The files made so far, in the built-in's order.
    pub fn files(&self) -> Vec<String> {
        LISTED
            .iter()
            .filter(|f| self.made.contains(f))
            .map(|f| f.name().to_string())
            .collect()
    }
}

/// [`product_names`] as it serializes, straight from the catalogue and the productdata.
pub struct ProductNamesJson<'a> {
    pub catalog: &'a RippedCatalog,
    pub furnidata: Option<&'a Furnidata>,
    pub products: &'a Productdata,
}

#[derive(serde::Serialize)]
struct NameJson<'a> {
    name: &'a str,
    description: &'a str,
}

impl serde::Serialize for ProductNamesJson<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        let mut seen: HashSet<&str> = HashSet::new();
        for mode in &self.catalog.modes {
            for page in mode.pages.values() {
                for offer in &page.offers {
                    let names = offer
                        .products
                        .iter()
                        .filter_map(|p| self.furnidata.and_then(|f| f.classname(p)));
                    for code in std::iter::once(offer.localization_id.as_str()).chain(names) {
                        if let Some((name, description)) = self.products.get(code) {
                            if seen.insert(code) {
                                map.serialize_entry(code, &NameJson { name, description })?;
                            }
                        }
                    }
                }
            }
        }
        map.end()
    }
}

/// `product_names` of catalog.json: the productdata name and description of every offer's
/// localization id and product class name that has one.
pub fn product_names(
    catalog: &RippedCatalog,
    furnidata: Option<&Furnidata>,
    products: &Productdata,
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for mode in &catalog.modes {
        for page in mode.pages.values() {
            for offer in &page.offers {
                let mut codes = vec![offer.localization_id.clone()];
                codes.extend(
                    offer
                        .products
                        .iter()
                        .filter_map(|p| furnidata.and_then(|f| f.classname(p)))
                        .map(str::to_string),
                );
                for code in codes {
                    if let Some((name, description)) = products.get(&code) {
                        out.entry(code).or_insert_with(
                            || serde_json::json!({ "name": name, "description": description }),
                        );
                    }
                }
            }
        }
    }
    serde_json::Value::Object(out)
}

/// What README.txt is written from.
#[derive(Debug, Clone, Copy)]
pub struct ReadmeInput<'a> {
    pub catalog: &'a RippedCatalog,
    pub sql: &'a SqlStats,
    /// The IMAGES line ("12 downloaded, 1 not on the server", "not downloaded (turned off)", ...).
    pub images: &'a str,
    pub database_note: &'a str,
    pub manager: &'a ManagerStats,
    pub with_items_base: bool,
    pub profile: &'a HotelProfile,
    pub missing_furni: usize,
    /// Class names in the furnidata used (None: no furnidata).
    pub furnidata_names: Option<usize>,
    /// What changed since an earlier rip (the CHANGES section), and whether the changes file
    /// deletes the removed pages and offers.
    pub changes: Option<&'a CatalogDiff>,
    pub diff_delete_removed: bool,
    /// Something was written into `packets/` (the PACKETS section).
    pub packets: bool,
    /// The user cancelled the rip: a CANCELLED section says so (not in the built-in).
    pub cancelled: bool,
    /// Windows line ends (the built-in writes them on Windows).
    pub crlf: bool,
}

/// The CANCELLED section (the built-in has none; see [`ReadmeInput::cancelled`]).
pub const CANCELLED_SECTION: &str =
    "\nCANCELLED\n  The rip was cancelled: the files hold what was read until then.\n";

/// The PACKETS section (the built-in's text).
pub const PACKETS_SECTION: &str = "
PACKETS
  packets/ holds the raw catalogue packets: the Manager can go on with a stopped rip or
  write these files again from them without the hotel.
";

/// README.txt.
pub fn readme(input: &ReadmeInput) -> String {
    let mut readme = export::readme(input.catalog, input.sql, input.images, input.database_note);
    readme.push_str(&manager::readme_section(
        input.manager,
        input.with_items_base,
    ));
    readme.push_str(&format!(
        "\nHOTEL PROFILE\n  {} ({})\n",
        input.profile.name,
        input.profile.kind.label()
    ));
    if let Some(report) = input.changes {
        readme.push_str(&diff::readme_section(report, input.diff_delete_removed));
    }
    if input.missing_furni > 0 {
        readme.push_str(&format!(
            "\nMISSING FURNI (missing_furni.json)\n  {} products are not in the hotel's items_base, so their offers are not in catalog_manager.sql.\n  {}\n",
            input.missing_furni,
            if input.furnidata_names.is_some() {
                "missing_furnidata.json lists them as a furnidata: give it to the Manager's furni import first."
            } else {
                "With the source hotel's furnidata they would also be written as a furnidata for the furni import."
            }
        ));
    }
    if let Some(names) = input.furnidata_names {
        readme.push_str(&format!(
            "\nFURNIDATA\n  {names} class names of the source hotel: floor and wall products are matched by\n  class name (items_base.item_name), not by sprite id.\n",
        ));
    }
    if input.packets {
        readme.push_str(PACKETS_SECTION);
    }
    if input.cancelled {
        readme.push_str(CANCELLED_SECTION);
    }
    if input.crlf {
        readme.replace('\n', "\r\n")
    } else {
        readme
    }
}

/// 12345 -> "12,345".
pub fn n0(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_json_error_fails_the_file_like_an_append_error() {
        // A map with non-string keys cannot be JSON: the serializer fails without any I/O error.
        let bad: BTreeMap<(i32, i32), i32> = [((1, 2), 3)].into();
        let mut written = Vec::new();
        let mut sink = |_: &str, data: &[u8]| -> Result<(), String> {
            written.extend_from_slice(data);
            Ok(())
        };
        let mut append: Append = &mut sink;
        let mut out = Chunks::new("catalog.json", &mut append);
        write_json(&mut out, &bad);
        let error = out.finish().unwrap_err();
        assert!(error.contains("JSON"), "{error}");

        // An append error stays the reported error.
        let mut failing = |_: &str, _: &[u8]| -> Result<(), String> { Err("disk full".into()) };
        let mut append: Append = &mut failing;
        let mut out = Chunks::new("catalog.json", &mut append);
        write_json(&mut out, &vec!["x".repeat(2 * CHUNK)]);
        assert_eq!(out.finish(), Err("disk full".to_string()));

        // A good value is written whole.
        let mut sink = |_: &str, data: &[u8]| -> Result<(), String> {
            written.extend_from_slice(data);
            Ok(())
        };
        let mut append: Append = &mut sink;
        let mut out = Chunks::new("catalog.json", &mut append);
        write_json(&mut out, &[1, 2]);
        assert_eq!(out.finish(), Ok(()));
    }

    #[test]
    fn numbers() {
        assert_eq!(n0(0), "0");
        assert_eq!(n0(1234), "1,234");
        assert_eq!(n0(1234567), "1,234,567");
    }

    /// Streams `pieces` through [`Chunks`] whose `append` fails from call `fail_from` on.
    fn stream(pieces: &[&[u8]], fail_from: usize) -> (Vec<Vec<u8>>, Result<(), String>, bool) {
        let mut calls: Vec<Vec<u8>> = Vec::new();
        let mut append = |name: &str, data: &[u8]| {
            assert_eq!(name, "catalog.json");
            calls.push(data.to_vec());
            if calls.len() > fail_from {
                Err("disk full".to_string())
            } else {
                Ok(())
            }
        };
        let mut append: Append = &mut append;
        let mut out = Chunks::new("catalog.json", &mut append);
        let mut all_ok = true;
        for piece in pieces {
            all_ok &= io::Write::write_all(&mut out, piece).is_ok();
        }
        let result = out.finish();
        (calls, result, all_ok)
    }

    #[test]
    fn chunks_hand_on_pieces_of_at_most_a_chunk() {
        let small = vec![b'a'; 300 * 1024];
        let big = vec![b'b'; CHUNK + 5];
        let pieces: Vec<&[u8]> = vec![&small, &small, &small, &small, &big, &small];
        let (calls, result, ok) = stream(&pieces, usize::MAX);
        assert!(ok && result.is_ok());
        let sizes: Vec<usize> = calls.iter().map(Vec::len).collect();
        // Three small pieces fill a chunk; the fourth starts the next; a piece bigger than a
        // chunk goes alone, after what was buffered.
        assert_eq!(sizes, [900 * 1024, 300 * 1024, CHUNK + 5, 300 * 1024]);
        assert_eq!(calls.concat(), pieces.concat());
    }

    #[test]
    fn an_empty_file_is_created_and_an_error_stops_the_stream() {
        let (calls, result, _) = stream(&[], usize::MAX);
        assert_eq!(calls, [Vec::<u8>::new()]);
        assert!(result.is_ok());

        let small = vec![b'a'; 700 * 1024];
        let pieces: Vec<&[u8]> = vec![&small; 6];
        let (calls, result, ok) = stream(&pieces, 1);
        assert_eq!(result, Err("disk full".to_string()));
        assert!(!ok, "the writer reports the error");
        assert_eq!(calls.len(), 2, "nothing is sent after the failed append");
    }

    #[test]
    fn items_base_from_rows() {
        let rows = [ItemsBaseRow {
            id: 10,
            sprite_id: 4001,
            item_type: "s".into(),
            item_name: "rare_dragon".into(),
            effect_id: 0,
        }];
        let items = items_base(&rows);
        assert_eq!(items.by_sprite[&("s".to_string(), 4001)], 10);
    }
}

//! The catalogue in several languages, from rips of several official Habbo hotels.
//!
//! The official hotels share their catalogue: the same page (same id and the same internal
//! name, which no language changes) is in habbo.nl in Dutch, in habbo.it in Italian, and so on.
//! Pairing the pages of a habbo.nl rip with the pages of the other hotels' rips gives, per
//! language, a dictionary "Dutch text -> text in that language": the format of the Manager's
//! page text dictionaries (`assets/catalog/texts/<lang>.json`, read by
//! `aio_remote::catalog_texts`, which translate from Dutch). Habbo's own translations then fill
//! what those dictionaries lack ([`fill_dictionary`]).
//!
//! Only page texts are paired: caption and the page_text columns, in the slots the Arcturus
//! layout classes read them from ([`crate::export::map_page_content`]), so the pairs are the
//! same column of the same page. A page whose internal name differs between two hotels is
//! skipped (counted), so a hotel-only page never pairs with another one.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::export::{map_page_content, RippedCatalog};
use crate::recording;

/// The language of an official hotel, as the Manager's dictionaries name it.
pub fn hotel_language(host_or_hotel: &str) -> Option<&'static str> {
    let name = if host_or_hotel.starts_with("habbo.") {
        host_or_hotel.to_ascii_lowercase()
    } else {
        crate::rip::hotel_name(host_or_hotel)
    };
    Some(match name.as_str() {
        "habbo.nl" => "nl",
        "habbo.com" => "en",
        "habbo.es" => "es",
        "habbo.com.br" => "pt",
        "habbo.de" => "de",
        "habbo.fr" => "fr",
        "habbo.it" => "it",
        "habbo.fi" => "fi",
        "habbo.com.tr" => "tr",
        _ => return None,
    })
}

/// The language the dictionaries translate from (`aio_remote::catalog_texts::SOURCE`).
pub const SOURCE: &str = "nl";

/// The page texts of one rip: (mode, page id) -> (internal name, [(column, text)]).
type PageTexts = BTreeMap<(String, i32), (String, Vec<(&'static str, String)>)>;

fn page_texts(catalog: &RippedCatalog) -> PageTexts {
    let mut out = PageTexts::new();
    for mode in &catalog.modes {
        for (node, _, _) in mode.index.walk() {
            if node.page_id <= 0 {
                continue;
            }
            let mut texts = vec![("caption", node.localization.clone())];
            if let Some(page) = mode.pages.get(&node.page_id) {
                let c = map_page_content(&page.layout, &page.images, &page.texts);
                texts.extend([
                    ("page_text1", c.text1),
                    ("page_text2", c.text2),
                    ("page_text_details", c.text_details),
                    ("page_text_teaser", c.text_teaser),
                ]);
            }
            out.entry((mode.mode.clone(), node.page_id))
                .or_insert((node.name.clone(), texts));
        }
    }
    out
}

/// One language's dictionary from the rips, with how it was made.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LanguageTexts {
    pub language: String,
    /// The rip it came from.
    pub folder: PathBuf,
    /// Dutch text -> text in this language.
    pub dictionary: BTreeMap<String, String>,
    /// Pages found in both hotels and paired.
    pub pages_paired: usize,
    /// Pages with the same id but another internal name: not paired.
    pub pages_mismatched: usize,
    /// Dutch texts that got different translations on different pages: the most frequent kept.
    pub conflicts: usize,
}

/// Every language's dictionary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Translations {
    pub source_folder: PathBuf,
    pub languages: Vec<LanguageTexts>,
    /// Rips that are not of an official hotel (no language known): left out.
    pub skipped: Vec<PathBuf>,
}

/// Pairs a Dutch rip with a rip in `language`.
pub fn pair(source: &RippedCatalog, other: &RippedCatalog, language: &str) -> LanguageTexts {
    let dutch = page_texts(source);
    let translated = page_texts(other);
    let mut counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut result = LanguageTexts {
        language: language.to_string(),
        ..LanguageTexts::default()
    };
    for (key, (name, texts)) in &dutch {
        let Some((other_name, other_texts)) = translated.get(key) else {
            continue;
        };
        if !name.eq_ignore_ascii_case(other_name) {
            result.pages_mismatched += 1;
            continue;
        }
        result.pages_paired += 1;
        let other_by_column: HashMap<&str, &String> =
            other_texts.iter().map(|(c, t)| (*c, t)).collect();
        for (column, text) in texts {
            let Some(other_text) = other_by_column.get(column) else {
                continue;
            };
            if text.trim().is_empty() || other_text.trim().is_empty() {
                continue;
            }
            *counts
                .entry(text.clone())
                .or_default()
                .entry((*other_text).clone())
                .or_default() += 1;
        }
    }
    for (dutch_text, options) in counts {
        if options.len() > 1 {
            result.conflicts += 1;
        }
        // The most frequent translation; on a tie the first in order.
        let best = options
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
            .map(|(t, _)| t.clone())
            .unwrap_or_default();
        result.dictionary.insert(dutch_text, best);
    }
    result
}

/// Dictionaries from rip folders (with recorded packets): one must be habbo.nl (the source);
/// every other official hotel gives its language. When a hotel was ripped more than once, the
/// newest folder (by name) is used.
pub fn build(folders: &[PathBuf]) -> Result<Translations> {
    let mut by_language: BTreeMap<&'static str, (PathBuf, RippedCatalog)> = BTreeMap::new();
    let mut skipped = Vec::new();
    let mut sorted = folders.to_vec();
    sorted.sort();
    for folder in sorted {
        let recording = recording::read(&folder)?;
        let catalog = recording.catalog();
        match hotel_language(&catalog.host) {
            Some(language) => {
                by_language.insert(language, (folder, catalog));
            }
            None => skipped.push(folder),
        }
    }
    let Some((source_folder, source)) = by_language.remove(SOURCE) else {
        bail!("no habbo.nl rip among them: the Manager's dictionaries translate from Dutch, so a rip of habbo.nl is needed");
    };
    if by_language.is_empty() {
        bail!("only habbo.nl was ripped: rip one or more other official hotels (habbo.com, .de, .it, ...) too");
    }
    let languages = by_language
        .into_iter()
        .map(|(language, (folder, catalog))| LanguageTexts {
            folder,
            ..pair(&source, &catalog, language)
        })
        .collect();
    Ok(Translations {
        source_folder,
        languages,
        skipped,
    })
}

/// The newest rip with recorded packets of every official hotel under `root`.
pub fn latest_official_rips(root: &Path) -> Vec<PathBuf> {
    let mut newest: BTreeMap<&'static str, PathBuf> = BTreeMap::new();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join(recording::PACKETS_DIR).join(recording::SESSION_FILE).is_file())
        .collect();
    dirs.sort();
    for dir in dirs {
        let hotel = dir
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.rsplit_once('_').map(|(head, _)| head))
            .and_then(|head| head.rsplit_once('_').map(|(h, _)| h.to_string()));
        if let Some(language) = hotel.as_deref().and_then(hotel_language) {
            newest.insert(language, dir);
        }
    }
    newest.into_values().collect()
}

/// `existing` (a curated dictionary) with the gaps filled from `ripped`: existing translations
/// are kept; returns the merged dictionary and how many texts were added.
pub fn fill_dictionary(
    existing: &BTreeMap<String, String>,
    ripped: &BTreeMap<String, String>,
) -> (BTreeMap<String, String>, usize) {
    let mut merged = existing.clone();
    let mut added = 0;
    for (key, value) in ripped {
        let missing = merged.get(key).is_none_or(|v| v.trim().is_empty());
        if missing {
            merged.insert(key.clone(), value.clone());
            added += 1;
        }
    }
    (merged, added)
}

/// Writes `<dir>/<lang>.json` per language (the Manager's dictionary format: a JSON object,
/// sorted, two-space indent) and `<dir>/translations_report.json`. When `existing_dir` holds the
/// Manager's dictionaries (`texts/<lang>.json`), each file is that dictionary with its gaps
/// filled, and `<lang>.added.json` lists what the rip added. Returns the files written.
pub fn write(
    translations: &Translations,
    dir: &Path,
    existing_dir: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut files = Vec::new();
    let mut report = Vec::new();
    for language in &translations.languages {
        let existing: Option<BTreeMap<String, String>> = existing_dir
            .map(|d| d.join(format!("{}.json", language.language)))
            .filter(|p| p.is_file())
            .map(|p| -> Result<BTreeMap<String, String>> {
                let text = std::fs::read_to_string(&p)?;
                serde_json::from_str(&text).with_context(|| format!("reading {}", p.display()))
            })
            .transpose()?;
        let (dictionary, added) = match &existing {
            Some(existing) => fill_dictionary(existing, &language.dictionary),
            None => (language.dictionary.clone(), language.dictionary.len()),
        };
        let path = dir.join(format!("{}.json", language.language));
        std::fs::write(&path, serde_json::to_string_pretty(&dictionary)? + "\n")?;
        files.push(path);
        if existing.is_some() {
            let new: BTreeMap<&String, &String> = dictionary
                .iter()
                .filter(|(k, _)| existing.as_ref().is_some_and(|e| e.get(*k).is_none_or(|v| v.trim().is_empty())))
                .collect();
            let path = dir.join(format!("{}.added.json", language.language));
            std::fs::write(&path, serde_json::to_string_pretty(&new)? + "\n")?;
            files.push(path);
        }
        report.push(serde_json::json!({
            "language": language.language,
            "rip": language.folder,
            "pages_paired": language.pages_paired,
            "pages_mismatched": language.pages_mismatched,
            "texts": language.dictionary.len(),
            "conflicts": language.conflicts,
            "added_to_existing": existing.as_ref().map(|_| added),
        }));
    }
    let path = dir.join("translations_report.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({
            "source": translations.source_folder,
            "languages": report,
            "skipped": translations.skipped,
        }))?,
    )?;
    files.push(path);
    Ok(files)
}

/// In one call: the newest rip of every official hotel under `output_folder`, paired and
/// written to `out_dir` (see [`write`]; `existing_dir`: the Manager's `texts/` folder to fill).
pub fn dictionaries_from_rips(
    output_folder: &Path,
    out_dir: &Path,
    existing_dir: Option<&Path>,
) -> Result<(Translations, Vec<PathBuf>)> {
    let rips = latest_official_rips(output_folder);
    let translations = build(&rips)?;
    let files = write(&translations, out_dir, existing_dir)?;
    Ok((translations, files))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_of_the_official_hotels() {
        assert_eq!(hotel_language("game-nl.habbo.com"), Some("nl"));
        assert_eq!(hotel_language("game-us.habbo.com"), Some("en"));
        assert_eq!(hotel_language("game-br.habbo.com"), Some("pt"));
        assert_eq!(hotel_language("habbo.com.tr"), Some("tr"));
        assert_eq!(hotel_language("game.retro.net"), None);
    }

    #[test]
    fn filling_keeps_curated_texts() {
        let existing: BTreeMap<String, String> =
            [("Hallo", "Ciao!"), ("Leeg", "")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let ripped: BTreeMap<String, String> = [("Hallo", "Salve"), ("Leeg", "Vuoto"), ("Nieuw", "Nuovo")]
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let (merged, added) = fill_dictionary(&existing, &ripped);
        assert_eq!(added, 2);
        assert_eq!(merged["Hallo"], "Ciao!");
        assert_eq!(merged["Leeg"], "Vuoto");
        assert_eq!(merged["Nieuw"], "Nuovo");
    }
}

//! The source hotel's productdata: the name and description the client shows for a product code
//! (a furni class name, or an offer's localization id). Read in both formats: productdata JSON
//! (`productdata.product[]` with `code`, `name`, `description`) and the older productdata.txt
//! (lines of JSON arrays `[["code","name","description"],...]`).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Productdata {
    /// code in lower case -> (name, description).
    pub products: HashMap<String, (String, String)>,
}

impl Productdata {
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim_start_matches('\u{feff}').trim();
        let mut data = Self::default();
        if text.starts_with('{') {
            let value: Value = serde_json::from_str(text).context("productdata JSON")?;
            let list = value
                .get("productdata")
                .and_then(|p| p.get("product"))
                .and_then(Value::as_array);
            for p in list.into_iter().flatten() {
                let field = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                data.add(&field("code"), field("name"), field("description"));
            }
        } else {
            for line in text.lines().map(str::trim).filter(|l| l.starts_with('[')) {
                let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                for row in rows {
                    let cell = |i: usize| row.get(i).and_then(Value::as_str).unwrap_or("").to_string();
                    data.add(&cell(0), cell(1), cell(2));
                }
            }
        }
        if data.products.is_empty() {
            bail!("the productdata lists no products");
        }
        Ok(data)
    }

    fn add(&mut self, code: &str, name: String, description: String) {
        let code = code.trim().to_lowercase();
        if !code.is_empty() {
            self.products.entry(code).or_insert((name, description));
        }
    }

    /// The name and description of a code (case does not matter).
    pub fn get(&self, code: &str) -> Option<(&str, &str)> {
        self.products
            .get(&code.trim().to_lowercase())
            .map(|(n, d)| (n.as_str(), d.as_str()))
    }

    pub fn len(&self) -> usize {
        self.products.len()
    }

    pub fn is_empty(&self) -> bool {
        self.products.is_empty()
    }
}

/// The productdata of an official Habbo hotel from its game host; `None` for other hosts.
pub fn official_url(host: &str) -> Option<String> {
    let name = crate::rip::hotel_name(host);
    name.starts_with("habbo.")
        .then(|| format!("https://www.{name}/gamedata/productdata_json/1"))
}

/// Loads productdata from an http(s) URL or a local file.
pub async fn load(source: &str) -> Result<Productdata> {
    let source = source.trim();
    let text = if source.starts_with("http://") || source.starts_with("https://") {
        let bytes = crate::http::try_get_bytes(source)
            .await?
            .with_context(|| format!("{source} is not there"))?;
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        let path = Path::new(source.strip_prefix("file://").unwrap_or(source));
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
    };
    Productdata::parse(&text).with_context(|| format!("reading the productdata at {source}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_formats() {
        let json = r#"{"productdata":{"product":[{"code":"Shelves_Norja","name":"Beige Bookcase","description":"For books"}]}}"#;
        let p = Productdata::parse(json).unwrap();
        assert_eq!(p.get("shelves_norja"), Some(("Beige Bookcase", "For books")));
        let txt = "[[\"a\",\"A name\",\"A desc\"],[\"b\",\"B\",\"\"]]\n[[\"c\",\"C\",\"x\"]]\n";
        let p = Productdata::parse(txt).unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(p.get("C"), Some(("C", "x")));
        assert!(Productdata::parse("[]").is_err());
        assert_eq!(
            official_url("game-de.habbo.com").as_deref(),
            Some("https://www.habbo.de/gamedata/productdata_json/1")
        );
    }
}

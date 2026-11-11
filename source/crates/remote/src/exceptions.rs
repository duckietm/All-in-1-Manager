//! The items_base exceptions: furni whose values no file can give (a pommel horse you sit on at height 2, ...).
//! They are kept in assets/items_base_exceptions.json, one entry per item_name with only the columns it sets, and
//! win over what Import Assets and Fix items_base work out. A colour variant ("name*3") uses its base name's entry
//! unless it has one of its own.

use std::sync::OnceLock;

use serde::Deserialize;

/// One furni's fixed values; a column that is missing is worked out as usual.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Exception {
    pub item_name: String,
    /// Why (for whoever reads the file); not used.
    #[serde(default)]
    pub why: String,
    pub interaction_type: Option<String>,
    pub interaction_modes_count: Option<i64>,
    pub width: Option<i64>,
    pub length: Option<i64>,
    pub stack_height: Option<f64>,
    /// "0" or "1".
    pub allow_sit: Option<String>,
    pub allow_lay: Option<String>,
    pub allow_walk: Option<String>,
    pub allow_stack: Option<String>,
    pub multiheight: Option<String>,
}

impl Exception {
    fn flag(value: &Option<String>) -> Option<bool> {
        value.as_deref().map(|v| v.trim() == "1")
    }

    pub fn sit(&self) -> Option<bool> {
        Self::flag(&self.allow_sit)
    }

    pub fn lay(&self) -> Option<bool> {
        Self::flag(&self.allow_lay)
    }

    pub fn walk(&self) -> Option<bool> {
        Self::flag(&self.allow_walk)
    }

    pub fn stack(&self) -> Option<bool> {
        Self::flag(&self.allow_stack)
    }
}

const FILE: &str = include_str!("../assets/items_base_exceptions.json");

/// Every exception, read once.
pub fn all() -> &'static [Exception] {
    static ALL: OnceLock<Vec<Exception>> = OnceLock::new();
    ALL.get_or_init(|| serde_json::from_str(FILE).expect("assets/items_base_exceptions.json is valid (a test checks it)"))
}

/// The exception of an item_name: its own, else its base name's (the part before '*'). Case does not matter.
pub fn find(item_name: &str) -> Option<&'static Exception> {
    let name = item_name.trim().to_lowercase();
    let base = name.split('*').next().unwrap_or("").to_string();
    let list = all();
    list.iter().find(|e| e.item_name.trim().to_lowercase() == name).or_else(|| list.iter().find(|e| e.item_name.trim().to_lowercase() == base))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_is_valid_and_the_pommel_is_in_it() {
        let pommel = find("olympics_c16_pommel").expect("pommel");
        assert_eq!(pommel.interaction_type.as_deref(), Some("multiheight"));
        assert_eq!((pommel.interaction_modes_count, pommel.stack_height), (Some(5), Some(2.0)));
        assert_eq!((pommel.sit(), pommel.stack(), pommel.walk(), pommel.lay()), (Some(true), Some(false), Some(false), None));
        assert_eq!(pommel.multiheight.as_deref(), Some("2.00;2.25;2.50;2.75;3.00"));
        // A colour variant uses its base's entry; other furni have none.
        assert!(find("OLYMPICS_C16_POMMEL*2").is_some());
        assert!(find("chair_norja").is_none());
        // Every entry: a name, flags of "0" / "1", a multiheight count that matches its modes.
        for e in all() {
            assert!(!e.item_name.trim().is_empty());
            for flag in [&e.allow_sit, &e.allow_lay, &e.allow_walk, &e.allow_stack].into_iter().flatten() {
                assert!(matches!(flag.as_str(), "0" | "1"), "{}: {flag}", e.item_name);
            }
            if let (Some(heights), Some(modes)) = (&e.multiheight, e.interaction_modes_count) {
                assert_eq!(crate::furni_import::multiheight_count(heights) as i64, modes, "{}", e.item_name);
            }
            if let Some(h) = e.stack_height {
                assert!(h.abs() <= crate::furni_import::MAX_STACK_HEIGHT, "{}", e.item_name);
            }
        }
    }
}

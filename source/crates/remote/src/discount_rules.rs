//! The catalogue's bulk discount: the `discount.*` emulator settings that say how many of an offer bought at once are
//! free (AIR's bundle discount). The emulator charges with the same sum the client shows; Polaris brings values out of
//! range into range, the manager refuses them instead so what is saved is what the hotel uses.

use crate::server::Result;
use crate::sql_session::{quote, SqlSession, SqlValue};

/// The most of one offer the emulator lets the settings allow at once.
pub const MAX_ITEMS: i64 = 1000;

pub const MAX_ITEMS_KEY: &str = "discount.max.allowed.items";
pub const BATCH_SIZE_KEY: &str = "discount.batch.size";
pub const FREE_PER_BATCH_KEY: &str = "discount.batch.free.items";
pub const BONUS_MINIMUM_KEY: &str = "discount.bonus.min.discounts";
pub const THRESHOLDS_KEY: &str = "discount.additional.thresholds";

/// The settings, in the order of [`Rules`]' fields, with the comment a new row gets.
pub const KEYS: [(&str, &str); 5] = [
    (MAX_ITEMS_KEY, "The most of one offer bought at once."),
    (BATCH_SIZE_KEY, "Number of items required for one discount batch; 0 turns the bulk discount off."),
    (FREE_PER_BATCH_KEY, "Number of free items granted inside one discount batch."),
    (BONUS_MINIMUM_KEY, "Minimum number of discount batches required before the bonus logic applies."),
    (THRESHOLDS_KEY, "Semicolon-separated amounts that each give one more free item."),
];

/// The bulk discount rules.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rules {
    pub max_items: i64,
    pub batch_size: i64,
    pub free_per_batch: i64,
    pub bonus_minimum: i64,
    pub thresholds: Vec<i64>,
}

impl Default for Rules {
    /// The emulator's defaults when a setting is missing.
    fn default() -> Self {
        Rules { max_items: 100, batch_size: 6, free_per_batch: 1, bonus_minimum: 1, thresholds: vec![40, 99] }
    }
}

/// Why typed rules are not saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    /// Not a whole number from 1 to [`MAX_ITEMS`].
    MaxItems,
    /// Not a whole number from 0 to [`MAX_ITEMS`].
    BatchSize,
    /// Not a whole number from 0 to one less than the batch size.
    FreePerBatch,
    /// Not a whole number from 0 to [`MAX_ITEMS`].
    BonusMinimum,
    /// Not whole numbers from 1 to [`MAX_ITEMS`] separated by `;`.
    Thresholds,
}

impl Rules {
    /// The rules as typed, or what is wrong with them.
    pub fn parse(max_items: &str, batch_size: &str, free_per_batch: &str, bonus_minimum: &str, thresholds: &str) -> std::result::Result<Rules, Problem> {
        let number = |text: &str, min: i64, max: i64, problem: Problem| text.trim().parse::<i64>().ok().filter(|n| (min..=max).contains(n)).ok_or(problem);
        let max_items = number(max_items, 1, MAX_ITEMS, Problem::MaxItems)?;
        let batch_size = number(batch_size, 0, MAX_ITEMS, Problem::BatchSize)?;
        let free_per_batch = number(free_per_batch, 0, (batch_size - 1).max(0), Problem::FreePerBatch)?;
        let bonus_minimum = number(bonus_minimum, 0, MAX_ITEMS, Problem::BonusMinimum)?;
        let mut list = Vec::new();
        for part in thresholds.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            list.push(number(part, 1, MAX_ITEMS, Problem::Thresholds)?);
        }
        list.sort_unstable();
        list.dedup();
        Ok(Rules { max_items, batch_size, free_per_batch, bonus_minimum, thresholds: list })
    }

    /// The thresholds as the setting stores them.
    pub fn thresholds_text(&self) -> String {
        self.thresholds.iter().map(i64::to_string).collect::<Vec<_>>().join(";")
    }

    /// The free items among `amount`: Polaris' `BundleDiscountRules.freeItems`, which is AIR's sum.
    pub fn free_items(&self, amount: i64) -> i64 {
        if amount <= 0 || self.batch_size <= 0 {
            return 0;
        }
        let batches = amount / self.batch_size;
        let mut free = batches * self.free_per_batch;
        if batches >= self.bonus_minimum {
            if amount % self.batch_size == self.batch_size - 1 {
                free += 1;
            }
            free += batches - self.bonus_minimum;
        }
        free += self.thresholds.iter().filter(|t| amount >= **t).count() as i64;
        free.min(amount)
    }

    /// The amount paid for.
    pub fn payable(&self, amount: i64) -> i64 {
        (amount - self.free_items(amount)).max(0)
    }

    /// The amounts, up to the most bought at once, where one more item is free: (amount, free, paid for).
    pub fn steps(&self) -> Vec<(i64, i64, i64)> {
        let mut steps = Vec::new();
        let mut last = 0;
        for amount in 1..=self.max_items {
            let free = self.free_items(amount);
            if free != last {
                steps.push((amount, free, amount - free));
                last = free;
            }
        }
        steps
    }

    /// The smallest amount, up to the most bought at once, that costs nothing.
    pub fn first_free_amount(&self) -> Option<i64> {
        (1..=self.max_items).find(|a| self.payable(*a) == 0)
    }

    /// The setting values in the order of [`KEYS`].
    pub fn values(&self) -> [String; 5] {
        [self.max_items.to_string(), self.batch_size.to_string(), self.free_per_batch.to_string(), self.bonus_minimum.to_string(), self.thresholds_text()]
    }
}

pub fn load_sql(database: &str) -> String {
    format!("SELECT `key`, `value` FROM {}.emulator_settings WHERE `key` IN (?, ?, ?, ?, ?)", quote(database))
}

/// Inserts the setting when the database has no row for it (a Polaris database does not), else changes its value.
pub fn save_sql(database: &str) -> String {
    format!("INSERT INTO {}.emulator_settings (`key`, `value`, `comment`) VALUES (?, ?, ?) ON DUPLICATE KEY UPDATE `value` = VALUES(`value`)", quote(database))
}

/// The stored values in the order of [`KEYS`], the default for each missing one, and which are missing.
pub async fn load(session: &SqlSession, database: &str) -> Result<([String; 5], Vec<&'static str>)> {
    let params = KEYS.iter().map(|(k, _)| SqlValue::Text(k.to_string())).collect();
    let rows = session.select(&load_sql(database), params).await?;
    Ok(stored_values(rows.iter().map(|r| (text(r.first()), text(r.get(1))))))
}

fn stored_values(rows: impl Iterator<Item = (String, String)>) -> ([String; 5], Vec<&'static str>) {
    let mut values = Rules::default().values();
    let mut found = [false; 5];
    for (key, value) in rows {
        if let Some(i) = KEYS.iter().position(|(k, _)| *k == key) {
            values[i] = value.trim().to_string();
            found[i] = true;
        }
    }
    let missing = KEYS.iter().zip(found).filter(|(_, f)| !f).map(|((k, _), _)| *k).collect();
    (values, missing)
}

fn text(value: Option<&SqlValue>) -> String {
    match value {
        Some(SqlValue::Text(t)) | Some(SqlValue::Decimal(t)) => t.clone(),
        Some(SqlValue::Int(v)) => v.to_string(),
        Some(SqlValue::UInt(v)) => v.to_string(),
        _ => String::new(),
    }
}

/// Saves all five settings in one transaction.
pub async fn save(session: &SqlSession, database: &str, rules: &Rules) -> Result<u64> {
    let statements: Vec<(String, Vec<SqlValue>)> = KEYS
        .iter()
        .zip(rules.values())
        .map(|((key, comment), value)| (save_sql(database), vec![SqlValue::Text(key.to_string()), SqlValue::Text(value), SqlValue::Text(comment.to_string())]))
        .collect();
    Ok(session.execute(&statements).await?.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_rules_give_airs_free_items() {
        let rules = Rules::default();
        assert_eq!([5, 6, 11, 12, 40].map(|a| rules.free_items(a)), [0, 1, 2, 3, 12]);
        assert_eq!(rules.payable(12), 9);
        assert_eq!(rules.steps().first(), Some(&(6, 1, 5)));
        assert_eq!(rules.first_free_amount(), None);
    }

    #[test]
    fn typed_rules_are_checked_like_the_emulator_ranges() {
        assert_eq!(Rules::parse("100", "6", "1", "1", " 99; 40 ;;40"), Ok(Rules::default()));
        assert_eq!(Rules::parse("0", "6", "1", "1", ""), Err(Problem::MaxItems));
        assert_eq!(Rules::parse("1001", "6", "1", "1", ""), Err(Problem::MaxItems));
        assert_eq!(Rules::parse("100", "-1", "0", "1", ""), Err(Problem::BatchSize));
        assert_eq!(Rules::parse("100", "6", "6", "1", ""), Err(Problem::FreePerBatch));
        assert_eq!(Rules::parse("100", "0", "1", "1", ""), Err(Problem::FreePerBatch));
        assert_eq!(Rules::parse("100", "6", "1", "x", ""), Err(Problem::BonusMinimum));
        assert_eq!(Rules::parse("100", "6", "1", "1", "40;0"), Err(Problem::Thresholds));
        let off = Rules::parse("50", "0", "0", "0", "").unwrap();
        assert!(off.steps().is_empty() && off.free_items(50) == 0);
        assert_eq!(off.thresholds_text(), "");
    }

    #[test]
    fn a_too_generous_rule_is_found() {
        let rules = Rules::parse("100", "2", "1", "0", "").unwrap();
        assert_eq!(rules.first_free_amount(), Some(1), "1 is one short of a batch: the bonus makes it free");
    }

    #[test]
    fn missing_settings_keep_the_defaults() {
        let (values, missing) = stored_values([("discount.batch.size".to_string(), " 8 ".to_string())].into_iter());
        assert_eq!(values, ["100", "8", "1", "1", "40;99"].map(String::from));
        assert_eq!(missing.len(), 4);
        assert!(!missing.contains(&BATCH_SIZE_KEY));
    }

    #[test]
    fn saving_inserts_a_missing_setting() {
        assert_eq!(
            save_sql("hotel"),
            "INSERT INTO `hotel`.emulator_settings (`key`, `value`, `comment`) VALUES (?, ?, ?) ON DUPLICATE KEY UPDATE `value` = VALUES(`value`)"
        );
    }
}

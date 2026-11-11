//! Habbo Club in the hotel database, beside the club gifts of the catalogue: the HC payday settings
//! (`emulator_settings` `subscriptions.hc.payday.*` and the message in `emulator_texts`), the paydays paid
//! (`logs_hc_payday`) and the club gifts members chose (`club_gift_claims`, written by the gameserver since it keeps that
//! record). The rules follow Polaris' SubscriptionClub: the payday pays back a percentage of the credits spent since the
//! last one, plus a streak bonus for the days a member has been HC without a break.

use crate::housekeeping::{int, text};
use crate::rcon;
use crate::server::{Result, Server, ServerError};
use crate::sql_session::{quote, SqlSession, SqlValue};

pub const ENABLED: &str = "subscriptions.hc.payday.enabled";
pub const NEXT_DATE: &str = "subscriptions.hc.payday.next_date";
pub const INTERVAL: &str = "subscriptions.hc.payday.interval";
pub const PERCENTAGE: &str = "subscriptions.hc.payday.percentage";
pub const CURRENCY: &str = "subscriptions.hc.payday.currency";
pub const STREAK: &str = "subscriptions.hc.payday.streak";
pub const RESET_ON_EXPIRE: &str = "subscriptions.hc.payday.creditsspent_reset_on_expire";
/// In `emulator_texts`; `%amount%` is the payout.
pub const MESSAGE: &str = "subscriptions.hc.payday.message";

pub const MAX_MESSAGE: usize = 1000;
const DEFAULT_MESSAGE: &str = "Woohoo HC Payday has arrived! You have received %amount% credits to your purse. Enjoy!";
const DEFAULT_STREAK: &str = "7=5;30=10;60=15;90=20;180=25;365=30";

/// The payday as the emulator reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaydaySettings {
    pub enabled: bool,
    /// `YYYY-MM-DD HH:MM:SS`, in the server's time zone.
    pub next_date: String,
    /// How often: `1 month`, `2 week`, ... (a number and second, minute, hour, day, week, month or year).
    pub interval: String,
    /// The percentage of the credits spent that is paid back.
    pub percentage: i64,
    /// `credits`, `diamonds`, `duckets`, or a points type number.
    pub currency: String,
    /// (days of HC in a row, bonus): the highest reached is added.
    pub streak: Vec<(i64, i64)>,
    /// The credits spent count from the start of a new membership instead of from the last payday.
    pub reset_on_expire: bool,
    pub message: String,
}

impl Default for PaydaySettings {
    fn default() -> Self {
        PaydaySettings {
            enabled: false,
            next_date: String::new(),
            interval: "1 month".into(),
            percentage: 10,
            currency: "credits".into(),
            streak: parse_streak(DEFAULT_STREAK),
            reset_on_expire: false,
            message: DEFAULT_MESSAGE.into(),
        }
    }
}

/// `7=5;30=10` as (days, bonus), sorted by days; parts that are not two numbers are left out, as the emulator skips
/// them.
pub fn parse_streak(value: &str) -> Vec<(i64, i64)> {
    let mut list: Vec<(i64, i64)> = value
        .split(';')
        .filter_map(|part| {
            let (days, bonus) = part.split_once('=')?;
            Some((days.trim().parse::<i32>().ok()? as i64, bonus.trim().parse::<i32>().ok()? as i64))
        })
        .collect();
    list.sort();
    list
}

pub fn streak_text(streak: &[(i64, i64)]) -> String {
    let mut sorted = streak.to_vec();
    sorted.sort();
    sorted.iter().map(|(days, bonus)| format!("{days}={bonus}")).collect::<Vec<_>>().join(";")
}

/// The emulator's true: `1` or `true`.
fn boolean(value: &str) -> bool {
    matches!(value.trim().to_lowercase().as_str(), "1" | "true")
}

/// A date the emulator reads: `YYYY-MM-DD HH:MM:SS`.
pub fn valid_date(value: &str) -> bool {
    chrono::NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M:%S").is_ok()
}

const UNITS: [&str; 7] = ["second", "minute", "hour", "day", "week", "month", "year"];

/// An interval the emulator reads: one or more "<number> <unit>" (`1 month`, `2 week`, `1 month 15 day`); a plural
/// `s` is fine.
pub fn valid_interval(value: &str) -> bool {
    let words: Vec<&str> = value.split_whitespace().collect();
    !words.is_empty()
        && words.len().is_multiple_of(2)
        && words.chunks(2).all(|pair| {
            pair[0].parse::<u32>().is_ok_and(|n| n > 0) && UNITS.iter().any(|u| pair[1].to_lowercase().trim_end_matches('s') == *u)
        })
}

/// The currencies the emulator names; any other is a points type number.
pub const CURRENCIES: [&str; 3] = ["credits", "diamonds", "duckets"];

pub fn valid_currency(value: &str) -> bool {
    let value = value.trim().to_lowercase();
    ["credits", "coins", "credit", "coin", "diamonds", "diamond", "duckets", "ducket", "pixels", "pixel"].contains(&value.as_str())
        || value.parse::<u32>().is_ok()
}

impl PaydaySettings {
    /// What is wrong before it is saved, or None.
    pub fn problem(&self) -> Option<String> {
        if !valid_date(&self.next_date) {
            return Some("The next payday is a date and time like 2026-12-01 18:00:00.".into());
        }
        if !valid_interval(&self.interval) {
            return Some("How often is a number and a unit, like 1 month or 2 week.".into());
        }
        if !(0..=100).contains(&self.percentage) {
            return Some("The percentage is 0 to 100.".into());
        }
        if !valid_currency(&self.currency) {
            return Some("The currency is credits, diamonds, duckets or a points type number.".into());
        }
        if self.streak.iter().any(|(days, bonus)| *days < 1 || *bonus < 0 || *days > 3650 || *bonus > 1_000_000) {
            return Some("A streak bonus needs 1 to 3650 days and a bonus of 0 or more.".into());
        }
        let mut days: Vec<i64> = self.streak.iter().map(|(d, _)| *d).collect();
        days.sort_unstable();
        if days.windows(2).any(|w| w[0] == w[1]) {
            return Some("Each number of days can have one streak bonus.".into());
        }
        if self.message.trim().is_empty() || self.message.chars().count() > MAX_MESSAGE {
            return Some(format!("The message is 1 to {MAX_MESSAGE} characters."));
        }
        None
    }

    /// (key, value) for `emulator_settings`.
    pub fn settings(&self) -> Vec<(&'static str, String)> {
        vec![
            (ENABLED, if self.enabled { "1" } else { "0" }.into()),
            (NEXT_DATE, self.next_date.trim().into()),
            (INTERVAL, self.interval.split_whitespace().collect::<Vec<_>>().join(" ")),
            (PERCENTAGE, self.percentage.to_string()),
            (CURRENCY, self.currency.trim().to_lowercase()),
            (STREAK, streak_text(&self.streak)),
            (RESET_ON_EXPIRE, if self.reset_on_expire { "1" } else { "0" }.into()),
        ]
    }
}

/// The payday settings; a key that is not there keeps its default.
pub async fn load_payday(session: &SqlSession, database: &str) -> Result<PaydaySettings> {
    let db = quote(database);
    let keys = [ENABLED, NEXT_DATE, INTERVAL, PERCENTAGE, CURRENCY, STREAK, RESET_ON_EXPIRE];
    let marks = vec!["?"; keys.len()].join(", ");
    let rows = session.select(&format!("SELECT `key`, `value` FROM {db}.emulator_settings WHERE `key` IN ({marks})"), keys.iter().map(|k| SqlValue::Text(k.to_string())).collect()).await?;
    let mut settings = PaydaySettings::default();
    for row in &rows {
        let value = text(row.get(1));
        match text(row.first()).as_str() {
            ENABLED => settings.enabled = boolean(&value),
            NEXT_DATE => settings.next_date = value.trim().into(),
            INTERVAL => settings.interval = value.trim().into(),
            PERCENTAGE => settings.percentage = value.trim().parse().unwrap_or(10),
            CURRENCY => settings.currency = value.trim().into(),
            STREAK => settings.streak = parse_streak(&value),
            RESET_ON_EXPIRE => settings.reset_on_expire = boolean(&value),
            _ => {}
        }
    }
    let message = session.select(&format!("SELECT `value` FROM {db}.emulator_texts WHERE `key` = ?"), vec![SqlValue::Text(MESSAGE.into())]).await;
    if let Some(row) = message.ok().and_then(|rows| rows.into_iter().next()) {
        settings.message = text(row.first());
    }
    Ok(settings)
}

/// Writes the settings and the message (a key that is not there is added).
pub async fn save_payday(session: &SqlSession, database: &str, settings: &PaydaySettings) -> Result<()> {
    if let Some(problem) = settings.problem() {
        return Err(ServerError(problem).into());
    }
    let db = quote(database);
    let mut statements: Vec<(String, Vec<SqlValue>)> = settings
        .settings()
        .into_iter()
        .map(|(key, value)| {
            (
                format!("INSERT INTO {db}.emulator_settings (`key`, `value`) VALUES (?, ?) ON DUPLICATE KEY UPDATE `value` = VALUES(`value`)"),
                vec![SqlValue::Text(key.into()), SqlValue::Text(value)],
            )
        })
        .collect();
    statements.push((
        format!("INSERT INTO {db}.emulator_texts (`key`, `value`) VALUES (?, ?) ON DUPLICATE KEY UPDATE `value` = VALUES(`value`)"),
        vec![SqlValue::Text(MESSAGE.into()), SqlValue::Text(settings.message.trim().into())],
    ));
    session.execute(&statements).await?;
    Ok(())
}

/// The running emulator reads the settings and texts again (RCON `reloadcontent` config and texts).
pub async fn reload_payday(server: &dyn Server) -> Result<()> {
    for target in ["config", "texts"] {
        let answer = rcon::send(server, &rcon::reload_content_request(target)).await?;
        if !answer.ok() {
            let why = if rcon::is_unknown_command(&answer) { "the emulator does not know reloadcontent yet".to_string() } else { answer.message.clone() };
            return Err(ServerError(format!("Saved, but the emulator did not load it ({target}): {why}. It is used after a restart.")).into());
        }
    }
    Ok(())
}

/// One payday paid to a member (`logs_hc_payday`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Payday {
    pub id: i64,
    pub timestamp: i64,
    pub user_id: i64,
    pub user: String,
    /// Days of HC in a row then.
    pub streak: i64,
    pub spent: i64,
    pub reward_spent: i64,
    pub reward_streak: i64,
    pub payout: i64,
    pub currency: String,
    /// Paid out (an offline member gets it at the next login).
    pub claimed: bool,
}

/// The latest paydays, of one member or of everyone.
pub async fn paydays(session: &SqlSession, database: &str, user: Option<i64>, limit: i64) -> Result<Vec<Payday>> {
    let db = quote(database);
    let filter = if user.is_some() { "WHERE p.user_id = ?" } else { "" };
    let sql = format!(
        "SELECT p.id, p.timestamp, p.user_id, COALESCE(u.username, ''), p.hc_streak, p.total_coins_spent, p.reward_coins_spent, p.reward_streak, \
         p.total_payout, COALESCE(p.currency, ''), p.claimed FROM {db}.logs_hc_payday p LEFT JOIN {db}.users u ON u.id = p.user_id {filter} \
         ORDER BY p.id DESC LIMIT {}",
        limit.clamp(1, 500)
    );
    let params = user.map(|u| vec![SqlValue::Int(u)]).unwrap_or_default();
    Ok(session
        .select(&sql, params)
        .await?
        .iter()
        .map(|r| Payday {
            id: int(r.first()),
            timestamp: int(r.get(1)),
            user_id: int(r.get(2)),
            user: text(r.get(3)),
            streak: int(r.get(4)),
            spent: int(r.get(5)),
            reward_spent: int(r.get(6)),
            reward_streak: int(r.get(7)),
            payout: int(r.get(8)),
            currency: text(r.get(9)),
            claimed: int(r.get(10)) != 0,
        })
        .collect())
}

/// A club gift a member chose (`club_gift_claims`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GiftClaim {
    pub id: i64,
    pub catalog_item_id: i64,
    pub catalog_name: String,
    /// The gift's furni names, when the offer is still there.
    pub furni: String,
    pub claimed_at: i64,
}

pub async fn has_table(session: &SqlSession, database: &str, table: &str) -> Result<bool> {
    let rows = session
        .select("SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?", vec![SqlValue::Text(database.into()), SqlValue::Text(table.into())])
        .await?;
    Ok(rows.first().map(|r| int(r.first())).unwrap_or(0) > 0)
}

/// The club gifts the member chose, newest first; None when the gameserver does not keep that record yet (no
/// `club_gift_claims`).
pub async fn gift_claims(session: &SqlSession, database: &str, user: i64) -> Result<Option<Vec<GiftClaim>>> {
    if !has_table(session, database, "club_gift_claims").await? {
        return Ok(None);
    }
    let db = quote(database);
    let sql = format!(
        "SELECT c.id, c.catalog_item_id, c.catalog_name, COALESCE((SELECT GROUP_CONCAT(COALESCE(NULLIF(b.public_name, ''), b.item_name) SEPARATOR ', ') \
         FROM {db}.items_base b WHERE FIND_IN_SET(b.id, REPLACE(i.item_ids, ';', ','))), ''), c.claimed_at \
         FROM {db}.club_gift_claims c LEFT JOIN {db}.catalog_items i ON i.id = c.catalog_item_id WHERE c.user_id = ? ORDER BY c.claimed_at DESC, c.id DESC LIMIT 200"
    );
    Ok(Some(
        session
            .select(&sql, vec![SqlValue::Int(user)])
            .await?
            .iter()
            .map(|r| GiftClaim { id: int(r.first()), catalog_item_id: int(r.get(1)), catalog_name: text(r.get(2)), furni: text(r.get(3)), claimed_at: int(r.get(4)) })
            .collect(),
    ))
}

/// How many gifts the member took (`users_settings.hc_gifts_claimed`): the count the emulator uses, which also holds
/// gifts from before the record.
pub async fn gifts_taken(session: &SqlSession, database: &str, user: i64) -> Result<i64> {
    let rows = session.select(&format!("SELECT hc_gifts_claimed FROM {}.users_settings WHERE user_id = ?", quote(database)), vec![SqlValue::Int(user)]).await?;
    Ok(rows.first().map(|r| int(r.first())).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaks_read_and_write_as_the_emulator_does() {
        assert_eq!(parse_streak("7=5;30=10; 60 = 15 ;bad;9=x;"), vec![(7, 5), (30, 10), (60, 15)]);
        assert_eq!(parse_streak("30=10;7=5"), vec![(7, 5), (30, 10)]);
        assert_eq!(streak_text(&[(30, 10), (7, 5)]), "7=5;30=10");
        assert_eq!(parse_streak(&streak_text(&PaydaySettings::default().streak)), PaydaySettings::default().streak);
    }

    #[test]
    fn settings_are_checked_before_saving() {
        let good = PaydaySettings { next_date: "2026-12-01 18:00:00".into(), ..PaydaySettings::default() };
        assert_eq!(good.problem(), None);
        for interval in ["1 month", "2 week", "2 weeks", "1 month 15 day", " 1  day "] {
            assert!(valid_interval(interval), "{interval}");
        }
        for interval in ["", "month", "1", "0 day", "1 fortnight", "1 month 2"] {
            assert!(!valid_interval(interval), "{interval}");
        }
        for (field, bad) in [
            ("date", PaydaySettings { next_date: "2026-12-01".into(), ..good.clone() }),
            ("interval", PaydaySettings { interval: "monthly".into(), ..good.clone() }),
            ("percentage", PaydaySettings { percentage: 101, ..good.clone() }),
            ("currency", PaydaySettings { currency: "gold".into(), ..good.clone() }),
            ("streak", PaydaySettings { streak: vec![(0, 5)], ..good.clone() }),
            ("double streak", PaydaySettings { streak: vec![(7, 5), (7, 6)], ..good.clone() }),
            ("message", PaydaySettings { message: " ".into(), ..good.clone() }),
        ] {
            assert!(bad.problem().is_some(), "{field}");
        }
        assert!(valid_currency("Diamonds") && valid_currency("105") && valid_currency("pixels"));
        let written = PaydaySettings { enabled: true, currency: " Diamonds ".into(), interval: "2   week".into(), ..good }.settings();
        assert!(written.contains(&(ENABLED, "1".to_string())));
        assert!(written.contains(&(CURRENCY, "diamonds".to_string())));
        assert!(written.contains(&(INTERVAL, "2 week".to_string())));
        assert!(written.contains(&(STREAK, "7=5;30=10;60=15;90=20;180=25;365=30".to_string())));
    }
}

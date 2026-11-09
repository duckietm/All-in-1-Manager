//! The content manager: hotel tables that are plain content (achievements, daily tasks, quests, polls, the navigator's
//! categories and public rooms, pets, group badge parts, chat bubbles, the calendar and the currency settings), edited
//! row by row in the hotel database through the SSH session, with the SQL Editor's MariaDB login.
//! Each table is described by an `aio_remote::content_admin::TableSpec`; the list, form, checks and statements all
//! come from it. Every tab says how the hotel picks the change up (a hotel command, or a restart of the emulator).

use std::sync::{Arc, Mutex};

use aio_remote::content_admin::{
    self, CurrencyTotal, Field, FieldKind, Row, Setting, TableSpec, ACHIEVEMENT_CATEGORIES, BADGE_PART_TYPES, DAILY_REWARD_TYPES, GOAL_TYPES, PET_SOUND_TYPES,
};
use aio_remote::rcon;
use aio_remote::sql_session::SqlSession;
use aio_remote::Server;
use eframe::egui::{self, RichText};

use super::sql_editor::login::{Login, Prompt};
use super::sql_editor::text;
use crate::gui::dialogs::Choice;
use crate::gui::theme::Palette;
use crate::gui::{widgets, Page as AppPage, Request, Shared, View};
use crate::i18n::{n, t, tf};

const MAX_INT: i64 = i32::MAX as i64;

const fn int(column: &'static str, label: &'static str, min: i64, max: i64, default: &'static str) -> Field {
    Field { column, label, kind: FieldKind::Int { min, max }, default }
}

const fn text_field(column: &'static str, label: &'static str, max: usize, default: &'static str) -> Field {
    Field { column, label, kind: FieldKind::Text { max }, default }
}

const fn flag(column: &'static str, label: &'static str, default: &'static str) -> Field {
    Field { column, label, kind: FieldKind::Flag, default }
}

const ACHIEVEMENTS: TableSpec = TableSpec {
    table: "achievements",
    id: "id",
    key: &[],
    fields: &[
        text_field("name", n("Name"), 64, ""),
        Field { column: "category", label: n("Category"), kind: FieldKind::Choice(ACHIEVEMENT_CATEGORIES), default: "identity" },
        int("level", n("Level"), 1, 1000, "1"),
        int("progress_needed", n("Progress needed"), 1, MAX_INT, "1"),
        int("reward_amount", n("Reward amount"), 0, MAX_INT, "100"),
        int("reward_type", n("Reward currency type"), 0, 1000, "0"),
        int("points", n("Achievement score"), 0, MAX_INT, "10"),
        int("state", n("State"), 0, 4, "1"),
        int("display_method", n("Display method"), 0, 100, "0"),
        text_field("subcategory", n("Subcategory"), 64, ""),
    ],
    order_by: "`name`, `level`",
    parent: None,
};

const DAILY_TASKS: TableSpec = TableSpec {
    table: "daily_tasks",
    id: "id",
    key: &[],
    fields: &[
        text_field("code", n("Code"), 64, ""),
        text_field("goal_type", n("Goal"), 32, "place_item"),
        text_field("goal_data", n("Goal data"), 255, ""),
        int("required_repeats", n("Times"), 1, MAX_INT, "1"),
        flag("is_bonus", n("Bonus task"), "0"),
        Field { column: "reward_type", label: n("Reward"), kind: FieldKind::Choice(DAILY_REWARD_TYPES), default: "duckets" },
        int("reward_amount", n("Reward amount"), 0, MAX_INT, "0"),
        text_field("reward_extra", n("Reward badge or furni"), 255, ""),
        text_field("catalog_name", n("Catalogue page name"), 64, ""),
        text_field("image_version", n("Image version"), 16, "1"),
        int("sort_order", n("Order"), -MAX_INT, MAX_INT, "0"),
        flag("enabled", n("Enabled"), "1"),
    ],
    order_by: "`sort_order`, `id`",
    parent: None,
};

const QUESTS: TableSpec = TableSpec {
    table: "quests",
    id: "id",
    key: &[],
    fields: &[
        text_field("campaign_code", n("Campaign"), 64, ""),
        text_field("code", n("Code"), 64, ""),
        text_field("chain_code", n("Chain"), 64, ""),
        int("sort_order", n("Order"), -MAX_INT, MAX_INT, "0"),
        text_field("goal_type", n("Goal"), 32, "place_item"),
        text_field("goal_data", n("Goal data"), 255, ""),
        int("goal_count", n("Times"), 1, MAX_INT, "1"),
        int("reward_type", n("Reward currency type"), 0, 1000, "0"),
        int("reward_amount", n("Reward amount"), 0, MAX_INT, "0"),
        text_field("reward_badge", n("Reward badge"), 32, ""),
        flag("easy", n("Easy"), "1"),
        flag("daily", n("Daily"), "0"),
        int("seasonal_seconds", n("Seasonal seconds"), 0, MAX_INT, "0"),
        int("wait_seconds", n("Wait seconds"), 0, MAX_INT, "0"),
        text_field("catalog_page_name", n("Catalogue page name"), 64, ""),
        text_field("image_version", n("Image version"), 16, "1"),
        flag("enabled", n("Enabled"), "1"),
    ],
    order_by: "`campaign_code`, `sort_order`, `id`",
    parent: None,
};

const POLLS: TableSpec = TableSpec {
    table: "polls",
    id: "id",
    key: &[],
    fields: &[text_field("title", n("Title"), 255, ""), text_field("thanks_message", n("Thank-you message"), 255, ""), text_field("reward_badge", n("Reward badge"), 10, "")],
    order_by: "`id`",
    parent: None,
};

const QUESTIONS: TableSpec = TableSpec {
    table: "polls_questions",
    id: "id",
    key: &[],
    fields: &[
        int("poll_id", n("Poll"), 1, MAX_INT, "0"),
        int("order", n("Order"), -MAX_INT, MAX_INT, "0"),
        text_field("question", n("Question"), 255, ""),
        int("type", n("Type"), 1, 3, "1"),
        int("min_selections", n("Least answers"), 0, 100, "1"),
        text_field("options", n("Options"), 255, "1:Yes;2:No"),
        int("parent_id", n("Shown after question"), 0, MAX_INT, "0"),
    ],
    order_by: "`order`, `id`",
    parent: Some("poll_id"),
};

const BOOL: &[&str] = &["0", "1"];
/// The calendar's ENUM('1','0') columns, in their own order.
const BOOL_ONE_FIRST: &[&str] = &["1", "0"];

const fn choice(column: &'static str, label: &'static str, options: &'static [&'static str], default: &'static str) -> Field {
    Field { column, label, kind: FieldKind::Choice(options), default }
}

const PUBLIC_CATEGORIES: TableSpec = TableSpec {
    table: "navigator_publiccats",
    id: "id",
    key: &[],
    fields: &[
        text_field("name", n("Name"), 32, ""),
        choice("image", n("Show as pictures"), BOOL, "0"),
        choice("visible", n("Visible"), BOOL, "1"),
        int("order_num", n("Order"), -MAX_INT, MAX_INT, "0"),
    ],
    order_by: "`order_num` DESC, `id`",
    parent: None,
};

const PUBLIC_ROOMS: TableSpec = TableSpec {
    table: "navigator_publics",
    id: "",
    key: &["public_cat_id", "room_id"],
    fields: &[int("public_cat_id", n("Category"), 1, MAX_INT, "0"), int("room_id", n("Room id"), 1, MAX_INT, "0"), choice("visible", n("Visible"), BOOL, "1")],
    order_by: "`room_id`",
    parent: Some("public_cat_id"),
};

const ROOM_CATEGORIES: TableSpec = TableSpec {
    table: "navigator_flatcats",
    id: "id",
    key: &[],
    fields: &[
        text_field("caption", n("Caption"), 100, ""),
        text_field("caption_save", n("Saved caption"), 32, "caption_save"),
        int("min_rank", n("Lowest rank"), 0, 1000, "0"),
        int("max_user_count", n("Most users in a room"), 1, MAX_INT, "100"),
        choice("can_trade", n("Trading"), BOOL, "1"),
        choice("public", n("Official"), BOOL, "0"),
        int("list_type", n("List type"), 0, 1, "0"),
        int("order_num", n("Order"), -MAX_INT, MAX_INT, "0"),
    ],
    order_by: "`order_num`, `id`",
    parent: None,
};

const PETS: TableSpec = TableSpec {
    table: "pet_actions",
    id: "",
    key: &["pet_type"],
    fields: &[
        int("pet_type", n("Pet type"), 0, 1000, "0"),
        text_field("pet_name", n("Name"), 32, ""),
        int("offspring_type", n("Offspring type"), -1, 1000, "-1"),
        text_field("happy_actions", n("Happy actions"), 100, ""),
        text_field("tired_actions", n("Tired actions"), 100, ""),
        text_field("random_actions", n("Random actions"), 100, ""),
        choice("can_swim", n("Can swim"), BOOL_ONE_FIRST, "0"),
    ],
    order_by: "`pet_type`",
    parent: None,
};

const PET_BREEDS: TableSpec = TableSpec {
    table: "pet_breeds",
    id: "",
    key: &["race", "color_one", "color_two", "has_color_one", "has_color_two"],
    fields: &[
        int("race", n("Pet type"), 0, 1000, "0"),
        int("color_one", n("First colour"), 0, MAX_INT, "0"),
        int("color_two", n("Second colour"), 0, MAX_INT, "0"),
        choice("has_color_one", n("Uses the first colour"), BOOL, "1"),
        choice("has_color_two", n("Uses the second colour"), BOOL, "0"),
        choice("club_only", n("Club only"), BOOL, "0"),
    ],
    order_by: "`color_one`, `color_two`",
    parent: Some("race"),
};

const PET_SOUNDS: TableSpec = TableSpec {
    table: "pet_vocals",
    id: "",
    key: &["pet_id", "type", "message"],
    fields: &[int("pet_id", n("Pet type"), 0, 1000, "0"), choice("type", n("When"), PET_SOUND_TYPES, "GENERIC_HAPPY"), text_field("message", n("Message"), 100, "")],
    order_by: "`type`, `message`",
    parent: Some("pet_id"),
};

const PET_COMMANDS: TableSpec = TableSpec {
    table: "pet_commands",
    id: "",
    key: &["pet_id", "command_id"],
    fields: &[int("pet_id", n("Pet type"), 0, 1000, "0"), int("command_id", n("Command"), 0, 1000, "0")],
    order_by: "`command_id`",
    parent: Some("pet_id"),
};

const PET_COMMAND_LIST: TableSpec = TableSpec {
    table: "pet_commands_data",
    id: "",
    key: &["command_id"],
    fields: &[
        int("command_id", n("Command"), 0, 1000, "0"),
        text_field("text", n("Text"), 15, ""),
        int("required_level", n("Level needed"), 0, 100, "1"),
        int("reward_xp", n("Experience given"), 0, MAX_INT, "5"),
        int("cost_happiness", n("Happiness it costs"), 0, 100, "0"),
        int("cost_energy", n("Energy it costs"), 0, 100, "0"),
    ],
    order_by: "`command_id`",
    parent: None,
};

const BADGE_PARTS: TableSpec = TableSpec {
    table: "guilds_elements",
    id: "",
    key: &["id", "type"],
    fields: &[
        choice("type", n("Part"), BADGE_PART_TYPES, "base"),
        int("id", n("Number"), 0, MAX_INT, "1"),
        text_field("firstvalue", n("Image or colour"), 300, ""),
        text_field("secondvalue", n("Second image"), 300, ""),
        choice("enabled", n("Enabled"), BOOL, "1"),
    ],
    order_by: "`type`, `id`",
    parent: None,
};

const CHAT_BUBBLES: TableSpec = TableSpec {
    table: "chat_bubbles",
    id: "",
    key: &["type"],
    fields: &[
        int("type", n("Style number"), 46, MAX_INT, "46"),
        text_field("name", n("Name"), 255, ""),
        text_field("permission", n("Permission needed"), 255, ""),
        flag("overridable", n("Others may override it"), "1"),
        flag("triggers_talking_furniture", n("Talking furni react"), "0"),
        int("catalog_item_id", n("Catalogue item"), 0, MAX_INT, "0"),
    ],
    order_by: "`type`",
    parent: None,
};

const CALENDARS: TableSpec = TableSpec {
    table: "calendar_campaigns",
    id: "id",
    key: &[],
    fields: &[
        text_field("name", n("Name"), 255, ""),
        text_field("image", n("Image"), 255, ""),
        int("start_timestamp", n("Start (Unix time)"), 0, MAX_INT, "0"),
        int("total_days", n("Days"), 1, 366, "30"),
        choice("lock_expired", n("Lock missed days"), BOOL_ONE_FIRST, "1"),
        choice("enabled", n("Enabled"), BOOL_ONE_FIRST, "1"),
    ],
    order_by: "`id`",
    parent: None,
};

const CALENDAR_REWARDS: TableSpec = TableSpec {
    table: "calendar_rewards",
    id: "id",
    key: &[],
    fields: &[
        int("campaign_id", n("Campaign"), 1, MAX_INT, "0"),
        Field { column: "day", label: n("Day"), kind: FieldKind::OptInt { min: 1, max: 366 }, default: "" },
        text_field("product_name", n("Furni (product name)"), 128, ""),
        text_field("custom_image", n("Image"), 128, ""),
        int("credits", n("Credits"), 0, MAX_INT, "0"),
        int("pixels", n("Duckets"), 0, MAX_INT, "0"),
        int("points", n("Points"), 0, MAX_INT, "0"),
        int("points_type", n("Points currency type"), 0, 1000, "0"),
        text_field("badge", n("Badge"), 25, ""),
        int("item_id", n("Item id"), 0, MAX_INT, "0"),
        text_field("subscription_type", n("Subscription"), 128, ""),
        int("subscription_days", n("Subscription days"), 0, MAX_INT, "0"),
    ],
    order_by: "`day`, `id`",
    parent: Some("campaign_id"),
};

/// The tables of the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Achievements,
    DailyTasks,
    Quests,
    Polls,
    Questions,
    PublicCategories,
    PublicRooms,
    RoomCategories,
    Pets,
    PetBreeds,
    PetSounds,
    PetCommands,
    PetCommandList,
    BadgeParts,
    ChatBubbles,
    Calendars,
    CalendarRewards,
}

const KIND_COUNT: usize = 17;

impl Kind {
    fn spec(self) -> &'static TableSpec {
        match self {
            Kind::Achievements => &ACHIEVEMENTS,
            Kind::DailyTasks => &DAILY_TASKS,
            Kind::Quests => &QUESTS,
            Kind::Polls => &POLLS,
            Kind::Questions => &QUESTIONS,
            Kind::PublicCategories => &PUBLIC_CATEGORIES,
            Kind::PublicRooms => &PUBLIC_ROOMS,
            Kind::RoomCategories => &ROOM_CATEGORIES,
            Kind::Pets => &PETS,
            Kind::PetBreeds => &PET_BREEDS,
            Kind::PetSounds => &PET_SOUNDS,
            Kind::PetCommands => &PET_COMMANDS,
            Kind::PetCommandList => &PET_COMMAND_LIST,
            Kind::BadgeParts => &BADGE_PARTS,
            Kind::ChatBubbles => &CHAT_BUBBLES,
            Kind::Calendars => &CALENDARS,
            Kind::CalendarRewards => &CALENDAR_REWARDS,
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    /// The table whose chosen row this one lists the rows of (a poll's questions), or None.
    fn parent(self) -> Option<Kind> {
        match self {
            Kind::Questions => Some(Kind::Polls),
            Kind::PublicRooms => Some(Kind::PublicCategories),
            Kind::PetBreeds | Kind::PetSounds | Kind::PetCommands => Some(Kind::Pets),
            Kind::CalendarRewards => Some(Kind::Calendars),
            _ => None,
        }
    }

    /// The tables listed under a chosen row of this one.
    fn sub_tables(self) -> &'static [Kind] {
        match self {
            Kind::Polls => &[Kind::Questions],
            Kind::PublicCategories => &[Kind::PublicRooms],
            Kind::Pets => &[Kind::PetBreeds, Kind::PetSounds, Kind::PetCommands],
            Kind::Calendars => &[Kind::CalendarRewards],
            _ => &[],
        }
    }

    /// The columns no two rows share (the table's unique key).
    fn unique(self) -> &'static [&'static str] {
        match self {
            Kind::Achievements => &["name", "level"],
            Kind::DailyTasks => &["code"],
            Kind::Quests => &["campaign_code", "code"],
            Kind::Polls | Kind::Questions | Kind::PublicCategories | Kind::RoomCategories | Kind::Calendars | Kind::CalendarRewards => &[],
            Kind::PublicRooms | Kind::PetBreeds | Kind::PetSounds | Kind::PetCommands | Kind::Pets | Kind::PetCommandList | Kind::BadgeParts | Kind::ChatBubbles => {
                self.spec().key
            }
        }
    }

    /// The rows of other tables deleted with a row: `(table, column)`, matched on the row's id (or first key).
    fn children(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Kind::Polls => &[("polls_answers", "poll_id"), ("polls_questions", "poll_id")],
            Kind::Questions => &[("polls_answers", "question_id")],
            Kind::PublicCategories => &[("navigator_publics", "public_cat_id")],
            // pet_vocals of type 0 are said by every pet, so they stay.
            Kind::Pets => &[("pet_breeds", "race"), ("pet_commands", "pet_id")],
            Kind::PetCommandList => &[("pet_commands", "command_id")],
            Kind::ChatBubbles => &[("users_chat_styles", "style_id")],
            Kind::Calendars => &[("calendar_rewards", "campaign_id"), ("calendar_rewards_claimed", "campaign_id")],
            _ => &[],
        }
    }

    /// The column a new row is numbered in (one above the highest), the lowest number, and the column whose
    /// rows have their own numbers (a badge part's type), for a table without AUTO_INCREMENT.
    fn numbered(self) -> Option<(&'static str, i64, Option<&'static str>)> {
        match self {
            Kind::Pets => Some(("pet_type", 0, None)),
            Kind::PetCommandList => Some(("command_id", 0, None)),
            Kind::BadgeParts => Some(("id", 1, Some("type"))),
            Kind::ChatBubbles => Some(("type", 46, None)),
            _ => None,
        }
    }

    /// The name of one row of a table listed under another (the heading of its form).
    fn title(self) -> &'static str {
        match self {
            Kind::Questions => t("Question"),
            Kind::PublicRooms => t("Public room"),
            Kind::PetBreeds => t("Breed"),
            Kind::PetSounds => t("Sound"),
            Kind::PetCommands => t("Command"),
            Kind::CalendarRewards => t("Reward"),
            _ => t("Row"),
        }
    }

    /// The heading of a table listed under another.
    fn heading(self) -> &'static str {
        match self {
            Kind::Questions => t("Questions"),
            Kind::PublicRooms => t("Rooms in this category"),
            Kind::PetBreeds => t("Breeds"),
            Kind::PetSounds => t("Sounds"),
            Kind::PetCommands => t("Commands"),
            Kind::CalendarRewards => t("Rewards"),
            _ => t("Rows"),
        }
    }

    /// How a row is named in the list and the messages.
    fn caption(self, row: &Row) -> String {
        let spec = self.spec();
        let get = |column| row.get(spec, column);
        match self {
            Kind::Achievements => format!("{} {}", get("name"), get("level")),
            Kind::DailyTasks => get("code").to_string(),
            Kind::Quests => format!("{} / {}", get("campaign_code"), get("code")),
            Kind::Polls => get("title").to_string(),
            Kind::Questions => get("question").to_string(),
            Kind::PublicCategories | Kind::Calendars => get("name").to_string(),
            Kind::PublicRooms => tf("Room {id}", &[("id", get("room_id"))]),
            Kind::RoomCategories => get("caption").to_string(),
            Kind::Pets => format!("{} {}", get("pet_type"), get("pet_name")),
            Kind::PetBreeds => format!("{} / {}", get("color_one"), get("color_two")),
            Kind::PetSounds => format!("{}: {}", get("type"), get("message")),
            Kind::PetCommands => tf("Command {id}", &[("id", get("command_id"))]),
            Kind::PetCommandList => format!("{} {}", get("command_id"), get("text")),
            Kind::BadgeParts => format!("{} {}: {}", get("type"), get("id"), get("firstvalue")),
            Kind::ChatBubbles => format!("{} {}", get("type"), get("name")),
            Kind::CalendarRewards => {
                let gift = [get("product_name"), get("badge"), get("subscription_type")].into_iter().find(|v| !v.trim().is_empty()).map(str::to_string);
                let gift = gift.unwrap_or_else(|| tf("{credits} credits", &[("credits", get("credits"))]));
                let day = get("day");
                if day.is_empty() { format!("{} — {gift}", t("Any day")) } else { format!("{} {day} — {gift}", t("Day")) }
            }
        }
    }

    /// The first problem of a row beyond its column kinds, or None. `parent` is the chosen row of the parent table.
    fn problem(self, row: &Row, rows: &[Row], parent: Option<&Row>) -> Option<String> {
        let spec = self.spec();
        let get = |column| row.get(spec, column);
        let needed = |column, text: &str| get(column).trim().is_empty().then(|| text.to_string());
        content_admin::row_problem(spec, row)
            .or_else(|| match self {
                Kind::Achievements => content_admin::achievement_name_problem(get("name")),
                Kind::DailyTasks | Kind::Quests => needed("code", "The code is needed.").or_else(|| content_admin::goal_type_problem(get("goal_type"))),
                Kind::Polls => needed("title", "The poll needs a title."),
                Kind::Questions => needed("question", "The question is needed.")
                    .or_else(|| content_admin::poll_options_problem(get("type").trim().parse().unwrap_or(0), get("options"))),
                Kind::PublicCategories | Kind::Calendars | Kind::ChatBubbles => needed("name", "The name is needed."),
                Kind::RoomCategories => needed("caption", "The caption is needed."),
                Kind::Pets => needed("pet_name", "The name is needed."),
                Kind::PetSounds => needed("message", "The message is needed."),
                Kind::PetCommandList => needed("text", "The text is needed."),
                Kind::BadgeParts => content_admin::badge_part_problem(get("type"), get("firstvalue"), get("secondvalue")),
                Kind::CalendarRewards => {
                    let days = parent.map_or(366, |p| p.get(&CALENDARS, "total_days").trim().parse().unwrap_or(366));
                    content_admin::calendar_day_problem(get("day"), days).or_else(|| {
                        content_admin::calendar_reward_problem(get("product_name"), get("item_id"), &[get("credits"), get("pixels"), get("points")], get("badge"), get("subscription_days"))
                    })
                }
                Kind::PublicRooms | Kind::PetBreeds | Kind::PetCommands => None,
            })
            .or_else(|| if self.unique().is_empty() { None } else { content_admin::duplicate_problem(spec, row, rows, self.unique()) })
    }

    /// What the form says about the table's columns, or None.
    fn help(self) -> Option<&'static str> {
        Some(match self {
            Kind::Questions => t("Type 1: one answer, 2: several answers, 3: a written answer. Options: value:label;value:label (not for type 3)."),
            Kind::Achievements => t("The badge is ACH_ + name + level (ACH_RoomEntry1). State: 0 off, 1 on, 2 archived, 3 off-season, 4 by wired."),
            Kind::PublicCategories => t("The navigator shows the public categories from the highest order down. Show as pictures 1: the rooms are shown as pictures."),
            Kind::PublicRooms => t("The id of a room to show in this category. The emulator skips a room that does not exist."),
            Kind::RoomCategories => t("The caption is the text shown (or a text key). Lowest rank: the rank a player needs to put a room in it. Official 1: an official category, for staff. List type 0: a list, 1: pictures."),
            Kind::Pets => t("Pet type: the number of the pet in the client. Offspring type: the pet a breeding gives (-1: none). Actions: gestures separated by ;."),
            Kind::PetBreeds => t("The colour numbers of a breed the catalogue sells. Club only 1: only club members may buy it."),
            Kind::PetSounds => t("What the pet says. Sounds of pet type 0 are said by every pet."),
            Kind::PetCommands => t("A command the pet learns: the numbers are on the Pet commands tab."),
            Kind::PetCommandList => t("The commands pets can learn. Level needed: the pet's level for the command."),
            Kind::BadgeParts => t("A colour is six hex digits (ffd601); a base or symbol names its image (a symbol in two layers names both). Each part has its own numbers."),
            Kind::ChatBubbles => t("Styles 46 and up are the hotel's own. Permission needed: empty for everyone. Catalogue item: the catalogue offer that sells it (0: not sold)."),
            Kind::Calendars => t("Start: a Unix time in seconds. Lock missed days 1: a day not opened in time stays shut."),
            Kind::CalendarRewards => t("Day empty: the reward is in the random pool of the days that have no reward of their own. Several rewards on one day: one is picked at random."),
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Achievements,
    DailyTasks,
    Quests,
    Polls,
    PublicRooms,
    RoomCategories,
    Pets,
    PetCommands,
    BadgeParts,
    ChatBubbles,
    Calendars,
    Currencies,
}

const TABS: [Tab; 12] = [
    Tab::Achievements,
    Tab::DailyTasks,
    Tab::Quests,
    Tab::Polls,
    Tab::PublicRooms,
    Tab::RoomCategories,
    Tab::Pets,
    Tab::PetCommands,
    Tab::BadgeParts,
    Tab::ChatBubbles,
    Tab::Calendars,
    Tab::Currencies,
];

impl Tab {
    fn kind(self) -> Option<Kind> {
        match self {
            Tab::Achievements => Some(Kind::Achievements),
            Tab::DailyTasks => Some(Kind::DailyTasks),
            Tab::Quests => Some(Kind::Quests),
            Tab::Polls => Some(Kind::Polls),
            Tab::PublicRooms => Some(Kind::PublicCategories),
            Tab::RoomCategories => Some(Kind::RoomCategories),
            Tab::Pets => Some(Kind::Pets),
            Tab::PetCommands => Some(Kind::PetCommandList),
            Tab::BadgeParts => Some(Kind::BadgeParts),
            Tab::ChatBubbles => Some(Kind::ChatBubbles),
            Tab::Calendars => Some(Kind::Calendars),
            Tab::Currencies => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tab::Achievements => t("Achievements"),
            Tab::DailyTasks => t("Daily tasks"),
            Tab::Quests => t("Quests"),
            Tab::Polls => t("Polls"),
            Tab::PublicRooms => t("Public rooms"),
            Tab::RoomCategories => t("Room categories"),
            Tab::Pets => t("Pets"),
            Tab::PetCommands => t("Pet commands"),
            Tab::BadgeParts => t("Group badges"),
            Tab::ChatBubbles => t("Chat bubbles"),
            Tab::Calendars => t("Calendar"),
            Tab::Currencies => t("Currencies"),
        }
    }

    /// The `reloadcontent` target that loads the tab's changes in the running hotel.
    fn reload_target(self) -> &'static str {
        match self {
            Tab::Achievements => "achievements",
            Tab::DailyTasks => "dailytasks",
            Tab::Quests => "quests",
            Tab::Polls => "polls",
            Tab::PublicRooms | Tab::RoomCategories => "navigator",
            Tab::Pets | Tab::PetCommands => "petdata",
            Tab::BadgeParts => "guildparts",
            Tab::ChatBubbles => "chatbubbles",
            Tab::Calendars => "calendar",
            Tab::Currencies => "config",
        }
    }

    /// How the hotel loads the tab's changes without the Reload button (an emulator without `reloadcontent`).
    fn reload_note(self) -> &'static str {
        match self {
            Tab::Achievements => t("The emulator loads achievements when it starts: type :update_achievements in the hotel (or restart the emulator) to load the changes."),
            Tab::DailyTasks | Tab::Quests => t("Press Reload in the hotel to load the changes, or restart the emulator."),
            Tab::Polls => t("Type :update_polls in the hotel (or restart the emulator) to load the changes."),
            Tab::PublicRooms | Tab::RoomCategories => t("Type :update_navigator in the hotel (or restart the emulator) to load the changes."),
            Tab::Pets | Tab::PetCommands => t("Type :update_pet_data in the hotel (or restart the emulator) to load the changes."),
            Tab::BadgeParts => t("Type :update_guildparts in the hotel (or restart the emulator) to load the changes; it reloads the badge images too."),
            Tab::ChatBubbles => t("Type :update_chat_bubbles in the hotel (or restart the emulator) to load the changes."),
            Tab::Calendars => t("Type :update_calendar in the hotel (or restart the emulator) to load the changes."),
            Tab::Currencies => t("Type :update_config in the hotel (or restart the emulator) to load the changes."),
        }
    }
}

#[derive(Default, Clone)]
struct Status {
    text: String,
    error: bool,
}

/// One table: its rows (None until loaded), the row shown as loaded and as edited, and the search.
#[derive(Default)]
struct Table {
    rows: Option<Vec<Row>>,
    row: Option<Row>,
    edit: Option<Row>,
    search: String,
}

#[derive(Default)]
struct State {
    tab: Tab,
    tables: [Table; KIND_COUNT],
    /// The table shown under a chosen row when there are several (a pet's breeds, sounds or commands).
    sub_table: usize,
    /// The currency settings as loaded and as edited (None until loaded), and the totals per currency type.
    settings: Option<Vec<Setting>>,
    setting_edits: Vec<String>,
    totals: Vec<CurrencyTotal>,
    /// Raised when the session closes: an older task changes nothing.
    generation: u64,
    session: Option<Arc<SqlSession>>,
    session_for: Option<usize>,
    opening: bool,
    prompt: Option<Prompt>,
    login_visible: bool,
    database: String,
    subtitle: String,
    busy: bool,
    status: Status,
}

impl State {
    fn close(&mut self, shared: &Shared) {
        self.generation += 1;
        if let Some(session) = self.session.take() {
            shared.spawn(async move {
                if let Ok(session) = Arc::try_unwrap(session) {
                    session.close().await;
                }
            });
        }
        let generation = self.generation;
        *self = State { generation, ..State::default() };
    }

    fn table(&mut self, kind: Kind) -> &mut Table {
        &mut self.tables[kind.index()]
    }

    fn ok(&mut self, text: String) {
        self.status = Status { text, error: false };
    }

    fn error(&mut self, text: String) {
        self.status = Status { text, error: true };
    }
}

pub struct ContentManagerView {
    state: Arc<Mutex<State>>,
}

#[derive(Clone)]
struct Ctl {
    state: Arc<Mutex<State>>,
    shared: Shared,
}

impl Ctl {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    fn with<R>(&self, generation: u64, change: impl FnOnce(&mut State) -> R) -> Option<R> {
        let result = {
            let mut state = self.lock();
            (state.generation == generation).then(|| change(&mut state))
        };
        self.shared.ctx.request_repaint();
        result
    }

    fn session(&self) -> Option<(u64, Arc<SqlSession>, String)> {
        let state = self.lock();
        state.session.clone().map(|s| (state.generation, s, state.database.clone()))
    }

    fn spawn<F: std::future::Future<Output = ()> + Send + 'static>(&self, task: impl FnOnce(Ctl) -> F) {
        self.shared.spawn(task(self.clone()));
    }

    /// Finds the hotel database, logs in to MariaDB like the SQL Editor (the saved root account, or asks), and
    /// loads the tab shown.
    async fn open(self, ask_first: bool) {
        let Some(server) = self.shared.session() else { return };
        let generation = {
            let mut state = self.lock();
            if state.opening {
                return;
            }
            state.opening = true;
            state.generation
        };
        if self.connect(server, ask_first, generation).await {
            let tab = self.lock().tab;
            self.load_tab(tab).await;
        }
        self.with(generation, |s| s.opening = false);
    }

    async fn connect(&self, server: Arc<dyn Server>, ask_first: bool, generation: u64) -> bool {
        if server.is_dry_run() {
            self.with(generation, |s| s.error(t("Dry run: there is no MariaDB to connect to.").into()));
            return false;
        }
        self.with(generation, |s| s.subtitle = t("Looking for the hotel database ...").into());
        let database = match aio_remote::catalog_admin::hotel_database(&*server).await {
            Ok(database) => database,
            Err(error) => {
                self.with(generation, |s| s.error(error.to_string()));
                return false;
            }
        };
        let host = server.login().host.clone();
        let saved = aio_core::config_dir::load_database_credentials(&host);
        let mut login = Login {
            user: saved.as_ref().map(|s| s.root_user.clone()).filter(|u| !u.is_empty()).unwrap_or_else(|| "root".into()),
            password: saved.as_ref().map(|s| s.root_password.clone()).unwrap_or_default(),
            port: saved.as_ref().map(|s| s.port).filter(|p| *p > 0).unwrap_or(3306),
        };
        let mut problem = saved.is_none().then(|| t("No saved MariaDB password for this server on this computer: enter an account.").to_string());
        let mut ask = ask_first || saved.is_none();
        let session = loop {
            if ask {
                let (prompt, answer) = Prompt::new(&host, &login, problem.take());
                if self.with(generation, |s| s.prompt = Some(prompt)).is_none() {
                    return false;
                }
                match answer.await.ok().flatten() {
                    Some(entered) => login = entered,
                    None => {
                        self.with(generation, |s| {
                            s.login_visible = true;
                            s.subtitle = t("Not logged in to MariaDB. Click Log in to MariaDB.").into();
                        });
                        return false;
                    }
                }
            }
            self.with(generation, |s| s.subtitle = tf("Connecting to MariaDB as {user} ...", &[("user", &login.user)]));
            match SqlSession::open(&*server, &login.user, &login.password, login.port).await {
                Ok(session) => break session,
                Err(error) => {
                    let message = error.to_string();
                    problem = Some(if text::is_access_denied(&message) {
                        tf("MariaDB refused {user}: {message}", &[("user", &login.user), ("message", &message)])
                    } else {
                        tf("Could not reach MariaDB on 127.0.0.1:{port} of the server: {message}", &[("port", &login.port.to_string()), ("message", &message)])
                    });
                    ask = true;
                }
            }
        };
        let session = Arc::new(session);
        let subtitle = tf("The content of {database} on {host}, through the SSH tunnel.", &[("database", &database), ("host", &host)]);
        let kept = self.with(generation, |s| {
            s.session = Some(session.clone());
            s.database = database.clone();
            s.login_visible = false;
            s.subtitle = subtitle;
        });
        if kept.is_none() {
            if let Ok(session) = Arc::try_unwrap(session) {
                session.close().await;
            }
            return false;
        }
        true
    }

    async fn load_tab(&self, tab: Tab) {
        match tab.kind() {
            Some(kind) => self.load(kind, None).await,
            None => self.load_currencies().await,
        }
    }

    /// Loads a table's rows (for a table listed under another, those of the parent's chosen row) and shows
    /// `select`, or the row shown before; then the tables listed under the row shown.
    async fn load(&self, kind: Kind, select: Option<Row>) {
        let Some((generation, session, database)) = self.session() else { return };
        let parent = match kind.parent() {
            Some(parent) => match parent_link(&self.lock(), parent) {
                Some(link) => Some(link),
                None => return,
            },
            None => None,
        };
        self.with(generation, |s| s.busy = true);
        let spec = kind.spec();
        let loaded = content_admin::load_rows(&session, &database, spec, parent).await;
        let shown = self.with(generation, |s| {
            s.busy = false;
            match loaded {
                Ok(rows) => {
                    let shown = select.or(s.table(kind).row.clone());
                    let row = shown.and_then(|shown| rows.iter().find(|r| content_admin::same_row(spec, &shown, r)).cloned());
                    if kind.parent().is_none() {
                        s.ok(tf("{count} rows in {table}.", &[("count", &rows.len().to_string()), ("table", spec.table)]));
                    }
                    let table = s.table(kind);
                    table.rows = Some(rows);
                    table.edit = row.clone();
                    table.row = row.clone();
                    row.is_some()
                }
                Err(error) => {
                    s.error(error.to_string());
                    false
                }
            }
        });
        if shown == Some(true) {
            for sub in kind.sub_tables() {
                Box::pin(self.load(*sub, None)).await;
            }
        }
    }

    /// Loads the tab's content again in the running hotel (RCON `reloadcontent`), as its `:update_*` command does.
    async fn reload_hotel(self) {
        let Some(server) = self.shared.session() else { return };
        let (generation, tab) = {
            let mut state = self.lock();
            if state.busy {
                return;
            }
            state.busy = true;
            (state.generation, state.tab)
        };
        let answer = rcon::send(&*server, &rcon::reload_content_request(tab.reload_target())).await;
        self.with(generation, |s| {
            s.busy = false;
            match answer {
                Ok(answer) if answer.ok() => s.ok(tf("{name}: loaded again in the hotel.", &[("name", tab.label())])),
                Ok(answer) if rcon::is_unknown_command(&answer) => {
                    s.error(tf("This emulator is older and cannot reload from here. {reload}", &[("reload", tab.reload_note())]))
                }
                Ok(answer) => {
                    let message = if answer.message.is_empty() { tf("status {status}", &[("status", &answer.status.to_string())]) } else { answer.message };
                    s.error(tf("The emulator refused: {message}", &[("message", &message)]))
                }
                Err(error) => s.error(error.to_string()),
            }
        });
    }

    /// Saves the row shown (adds it when it is new) and loads the table again.
    async fn save(self, kind: Kind) {
        let Some((generation, session, database)) = self.session() else { return };
        let (row, problem) = {
            let s = self.lock();
            let table = &s.tables[kind.index()];
            let Some(row) = table.edit.clone() else { return };
            let problem = kind.problem(&row, table.rows.as_deref().unwrap_or_default(), parent_row(&s, kind));
            (row, problem)
        };
        if let Some(problem) = problem {
            self.with(generation, |s| s.error(problem));
            return;
        }
        self.with(generation, |s| s.busy = true);
        let saved = content_admin::save_row(&session, &database, kind.spec(), &row).await;
        let stored = self.with(generation, |s| {
            s.busy = false;
            match saved {
                Ok(stored) => {
                    s.ok(tf("{name} saved. {reload}", &[("name", &kind.caption(&row)), ("reload", s.tab.reload_note())]));
                    Some(stored)
                }
                Err(error) => {
                    s.error(error.to_string());
                    None
                }
            }
        });
        if let Some(Some(stored)) = stored {
            let status = self.lock().status.clone();
            self.load(kind, Some(stored)).await;
            self.with(generation, |s| {
                if !s.status.error {
                    s.status = status;
                }
            });
        }
    }

    /// Asks first, then deletes the row shown (a poll with its questions and answers).
    async fn delete(self, kind: Kind) {
        let Some((generation, session, database)) = self.session() else { return };
        let spec = kind.spec();
        let Some(row) = self.lock().tables[kind.index()].row.clone().filter(|r| !content_admin::is_new(spec, r)) else { return };
        let name = kind.caption(&row);
        if kind == Kind::RoomCategories {
            match content_admin::count_rows(&session, &database, "rooms", "category", row.id).await {
                Ok(0) => {}
                Ok(rooms) => {
                    let text = tf("{rooms} rooms are in {name}: move them to another category first.", &[("rooms", &rooms.to_string()), ("name", &name)]);
                    self.with(generation, |s| s.error(text));
                    return;
                }
                Err(error) => {
                    self.with(generation, |s| s.error(error.to_string()));
                    return;
                }
            }
        }
        let message = match kind {
            Kind::Polls => tf("The poll {name}, its questions and the answers given are deleted from the database. This cannot be undone.", &[("name", &name)]),
            Kind::Questions => tf("The question {name} and the answers given are deleted from the database. This cannot be undone.", &[("name", &name)]),
            Kind::PublicCategories => tf("The category {name} and its list of public rooms are deleted from the database (the rooms stay). This cannot be undone.", &[("name", &name)]),
            Kind::Pets => tf("The pet {name}, its breeds and its commands are deleted from the database (pets players own stay). This cannot be undone.", &[("name", &name)]),
            Kind::PetCommandList => tf("The command {name} is deleted from the database, and taken from every pet type. This cannot be undone.", &[("name", &name)]),
            Kind::ChatBubbles => tf("The chat bubble {name} is deleted from the database, and taken from the players who bought it. This cannot be undone.", &[("name", &name)]),
            Kind::Calendars => tf("The calendar {name}, its rewards and the record of the days opened are deleted from the database. This cannot be undone.", &[("name", &name)]),
            _ => tf("{name} is deleted from the database. This cannot be undone.", &[("name", &name)]),
        };
        let options = vec![Choice::new(t("Delete"), t("Delete it from the database."), true).primary(), Choice::new(t("Cancel"), t("Keep it."), false)];
        if !self.shared.dialogs.choose(t("Are you sure?"), &tf("Delete {name}?", &[("name", &name)]), &message, false, options).await {
            return;
        }
        self.with(generation, |s| s.busy = true);
        let deleted = content_admin::delete_row(&session, &database, spec, &row, kind.children()).await;
        let done = self.with(generation, |s| {
            s.busy = false;
            match deleted {
                Ok(_) => {
                    let table = s.table(kind);
                    table.row = None;
                    table.edit = None;
                    for sub in kind.sub_tables() {
                        *s.table(*sub) = Table::default();
                    }
                    s.ok(tf("{name} deleted. {reload}", &[("name", &name), ("reload", s.tab.reload_note())]));
                    true
                }
                Err(error) => {
                    s.error(error.to_string());
                    false
                }
            }
        });
        if done == Some(true) {
            let status = self.lock().status.clone();
            self.load(kind, None).await;
            self.with(generation, |s| s.status = status);
        }
    }

    async fn load_currencies(&self) {
        let Some((generation, session, database)) = self.session() else { return };
        self.with(generation, |s| s.busy = true);
        let settings = content_admin::load_currency_settings(&session, &database).await;
        let totals = content_admin::load_currency_totals(&session, &database).await;
        self.with(generation, |s| {
            s.busy = false;
            match (settings, totals) {
                (Ok(settings), Ok(totals)) => {
                    s.setting_edits = settings.iter().map(|x| x.value.clone()).collect();
                    s.settings = Some(settings);
                    s.totals = totals;
                    s.ok(t("Currency settings loaded.").into());
                }
                (Err(error), _) | (_, Err(error)) => s.error(error.to_string()),
            }
        });
    }

    async fn save_setting(self, index: usize) {
        let Some((generation, session, database)) = self.session() else { return };
        let Some((key, value)) = ({
            let s = self.lock();
            s.settings.as_ref().and_then(|list| list.get(index)).map(|x| x.key.clone()).zip(s.setting_edits.get(index).cloned())
        }) else {
            return;
        };
        if let Some(problem) = content_admin::setting_problem(&key, &value) {
            self.with(generation, |s| s.error(problem));
            return;
        }
        self.with(generation, |s| s.busy = true);
        let saved = content_admin::save_setting(&session, &database, &key, &value).await;
        self.with(generation, |s| {
            s.busy = false;
            match saved {
                Ok(_) => {
                    if let Some(setting) = s.settings.as_mut().and_then(|list| list.get_mut(index)) {
                        setting.value = value.trim().to_string();
                    }
                    if let Some(edit) = s.setting_edits.get_mut(index) {
                        *edit = value.trim().to_string();
                    }
                    s.ok(tf("{key} saved. {reload}", &[("key", &key), ("reload", Tab::Currencies.reload_note())]));
                }
                Err(error) => s.error(error.to_string()),
            }
        });
    }
}

fn server_id(server: &Arc<dyn Server>) -> usize {
    Arc::as_ptr(server) as *const () as usize
}

impl ContentManagerView {
    pub fn new(_shared: &Shared) -> Self {
        Self { state: Arc::new(Mutex::new(State::default())) }
    }

    fn ctl(&self, shared: &Shared) -> Ctl {
        Ctl { state: self.state.clone(), shared: shared.clone() }
    }

    fn connect_if_needed(&mut self, shared: &Shared) {
        let Some(server) = shared.session() else { return };
        let id = server_id(&server);
        {
            let mut state = self.state.lock().unwrap();
            if state.session_for == Some(id) {
                return;
            }
            state.close(shared);
            state.session_for = Some(id);
        }
        self.ctl(shared).spawn(|ctl| ctl.open(false));
    }
}

enum Action {
    Login,
    LoadTab(Tab),
    LoadSubTables(Kind),
    Save(Kind),
    Delete(Kind),
    SaveSetting(usize),
    ReloadHotel,
}

fn frame(palette: &Palette) -> egui::Frame {
    widgets::card_frame(palette).inner_margin(10).corner_radius(10)
}

fn small(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).size(12.5)).corner_radius(8).min_size(egui::vec2(0.0, 28.0))
}

const FIELD_HEIGHT: f32 = 26.0;

impl View for ContentManagerView {
    fn show(&mut self, ui: &mut egui::Ui, palette: &Palette, shared: &Shared) {
        self.connect_if_needed(shared);
        let ctx = ui.ctx().clone();
        let mut actions = Vec::new();
        {
            let mut guard = self.state.lock().unwrap();
            let s = &mut *guard;
            if let Some(prompt) = s.prompt.as_mut() {
                if prompt.show(&ctx, palette) {
                    s.prompt = None;
                }
            }
            egui::Frame::new().inner_margin(egui::Margin { left: 0, right: 0, top: 0, bottom: 18 }).show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                header(ui, palette, s, shared, &mut actions);
                ui.add_space(10.0);
                body(ui, palette, s, &mut actions);
            });
        }
        let ctl = self.ctl(shared);
        for action in actions {
            match action {
                Action::Login => ctl.spawn(|c| c.open(true)),
                Action::LoadTab(tab) => ctl.spawn(move |c| async move { c.load_tab(tab).await }),
                Action::LoadSubTables(kind) => ctl.spawn(move |c| async move {
                    for sub in kind.sub_tables() {
                        c.load(*sub, None).await;
                    }
                }),
                Action::Save(kind) => ctl.spawn(move |c| c.save(kind)),
                Action::Delete(kind) => ctl.spawn(move |c| c.delete(kind)),
                Action::SaveSetting(index) => ctl.spawn(move |c| c.save_setting(index)),
                Action::ReloadHotel => ctl.spawn(|c| c.reload_hotel()),
            }
        }
    }

    fn busy(&self) -> bool {
        self.state.lock().unwrap().busy
    }

    fn session_changed(&mut self, shared: &Shared) {
        self.state.lock().unwrap().close(shared);
    }
}

fn header(ui: &mut egui::Ui, palette: &Palette, s: &mut State, shared: &Shared, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(widgets::secondary_button(t("Back"))).clicked() {
                shared.request(Request::Go(AppPage::Dashboard));
            }
            if ui.add_enabled(s.session.is_some() && !s.busy, small(t("Refresh"))).clicked() {
                actions.push(Action::LoadTab(s.tab));
            }
            if s.login_visible && ui.add(widgets::primary_button(t("Log in to MariaDB"))).clicked() {
                actions.push(Action::Login);
            }
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.label(RichText::new(t("Content")).size(13.0).strong());
                ui.add(egui::Label::new(RichText::new(&s.subtitle).size(12.0).color(palette.muted)).wrap());
            });
        });
    });
    if !s.status.text.is_empty() {
        ui.add_space(4.0);
        let color = if s.status.error { palette.error_text } else { palette.muted };
        ui.add(egui::Label::new(RichText::new(&s.status.text).size(12.0).color(color)).wrap());
    }
}

fn body(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        for tab in TABS {
            if ui.selectable_label(s.tab == tab, RichText::new(tab.label()).size(15.0)).clicked() && s.tab != tab {
                s.tab = tab;
                s.sub_table = 0;
                let loaded = match tab.kind() {
                    Some(kind) => s.tables[kind.index()].rows.is_some(),
                    None => s.settings.is_some(),
                };
                if !loaded && s.session.is_some() {
                    actions.push(Action::LoadTab(tab));
                }
            }
        }
    });
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        // RCON reloadcontent: the tab's :update_* command, without being logged in to the hotel.
        if ui.add_enabled(s.session.is_some() && !s.busy, small(t("Reload in the hotel"))).clicked() {
            actions.push(Action::ReloadHotel);
        }
        widgets::muted(ui, palette, s.tab.reload_note(), 12.0);
    });
    ui.add_space(6.0);
    let Some(kind) = s.tab.kind() else {
        egui::CentralPanel::default().frame(frame(palette)).show_inside(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("content-currencies").auto_shrink([false, false]).show(ui, |ui| currencies(ui, palette, s, actions));
        });
        return;
    };
    egui::SidePanel::left(egui::Id::new(("content-list", kind.index())))
        .resizable(true)
        .default_width(300.0)
        .width_range(200.0..=600.0)
        .frame(frame(palette).outer_margin(egui::Margin { left: 0, right: 6, top: 0, bottom: 0 }))
        .show_inside(ui, |ui| {
            if list(ui, palette, s, kind, 0.0) && !kind.sub_tables().is_empty() {
                for sub in kind.sub_tables() {
                    *s.table(*sub) = Table::default();
                }
                actions.push(Action::LoadSubTables(kind));
            }
        });
    egui::CentralPanel::default().frame(frame(palette)).show_inside(ui, |ui| {
        egui::ScrollArea::vertical().id_salt(("content-form", kind.index())).auto_shrink([false, false]).show(ui, |ui| {
            form(ui, palette, s, kind, actions);
            sub_tables(ui, palette, s, kind, actions);
        });
    });
}

/// The rows of a table with a New button and a search; clicking one shows it. True when another row was chosen.
fn list(ui: &mut egui::Ui, palette: &Palette, s: &mut State, kind: Kind, height: f32) -> bool {
    let ready = s.session.is_some() && !s.busy;
    let parent = kind.parent().and_then(|p| parent_link(s, p));
    let spec = kind.spec();
    let table = s.table(kind);
    let mut chosen = false;
    ui.horizontal(|ui| {
        let label = if kind == Kind::Questions { t("New question") } else { t("New") };
        if ui.add_enabled(ready && table.rows.is_some(), small(label)).clicked() {
            let mut row = content_admin::new_row(spec, parent);
            if kind == Kind::Questions {
                let next = table.rows.as_ref().map_or(0, |rows| rows.len()) + 1;
                row.values[1] = next.to_string();
            }
            if let Some((column, floor, group)) = kind.numbered() {
                let next = next_number(spec, table.rows.as_deref().unwrap_or_default(), &row, column, floor, group);
                if let Some(i) = spec.fields.iter().position(|f| f.column == column) {
                    row.values[i] = next.to_string();
                }
            }
            table.row = Some(row.clone());
            table.edit = Some(row);
            chosen = true;
        }
        ui.add(egui::TextEdit::singleline(&mut table.search).hint_text(t("Search")).desired_width(f32::INFINITY));
    });
    ui.add_space(4.0);
    let Some(rows) = table.rows.as_ref() else {
        widgets::muted(ui, palette, t("Not loaded yet."), 12.5);
        return false;
    };
    let query = table.search.trim().to_lowercase();
    let selected = table.row.as_ref().map(|r| r.id);
    let mut select = None;
    let mut scroll = egui::ScrollArea::vertical().id_salt(("content-rows", kind.index())).auto_shrink([false, height > 0.0]);
    if height > 0.0 {
        scroll = scroll.max_height(height);
    }
    scroll.show(ui, |ui| {
        let mut shown = 0;
        for row in rows {
            let caption = kind.caption(row);
            if !query.is_empty() && !caption.to_lowercase().contains(&query) && row.id.to_string() != query {
                continue;
            }
            shown += 1;
            let disabled = row.get(spec, "enabled") == "0" || row.get(spec, "visible") == "0" || (kind == Kind::Achievements && row.get(spec, "state") == "0");
            let color = if disabled { palette.subtle } else { palette.text };
            let caption = if content_admin::is_keyed(spec) { caption } else { format!("{caption}  #{}", row.id) };
            let text = RichText::new(caption).size(12.5).color(color);
            if ui.selectable_label(selected == Some(row.id), text).clicked() && selected != Some(row.id) {
                select = Some(row.clone());
            }
        }
        if shown == 0 {
            widgets::muted(ui, palette, t("Nothing found."), 12.5);
        }
    });
    if let Some(row) = select {
        table.row = Some(row.clone());
        table.edit = Some(row);
        chosen = true;
    }
    chosen
}

/// The form of the row shown: a field per column, Save and Delete.
fn form(ui: &mut egui::Ui, palette: &Palette, s: &mut State, kind: Kind, actions: &mut Vec<Action>) {
    let busy = s.busy || s.session.is_none();
    let spec = kind.spec();
    let parent = parent_row(s, kind).cloned();
    let table = s.table(kind);
    let Some(edit) = table.edit.as_mut() else {
        ui.add_space(20.0);
        let hint = match kind {
            Kind::Questions => t("Choose a question, or add one."),
            _ if kind.parent().is_some() => t("Choose one, or add one with New."),
            _ => t("Choose a row on the left, or add one with New."),
        };
        widgets::muted(ui, palette, hint, 13.0);
        return;
    };
    let is_new = content_admin::is_new(spec, edit);
    let title = if is_new { t("New") } else { t("Edit") };
    let title = if kind.parent().is_some() { format!("{} — {}", kind.title(), title) } else { format!("{} — {}", kind.caption(edit), title) };
    ui.label(widgets::title_text(&title, 17.0));
    ui.add_space(4.0);
    egui::Grid::new(("content-form-grid", kind.index())).num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        for (i, field) in spec.fields.iter().enumerate() {
            if spec.parent == Some(field.column) {
                continue;
            }
            ui.label(RichText::new(t(field.label)).size(12.0).color(palette.muted));
            field_editor(ui, kind, field, &mut edit.values[i]);
            ui.end_row();
        }
    });
    let row = table.edit.clone().unwrap_or_default();
    let changed = is_new || table.edit != table.row;
    let problem = kind.problem(&row, table.rows.as_deref().unwrap_or_default(), parent.as_ref());
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.add_enabled(changed && problem.is_none() && !busy, widgets::primary_button(if is_new { t("Add") } else { t("Save") })).clicked() {
            actions.push(Action::Save(kind));
        }
        if changed && !is_new && ui.add_enabled(!busy, small(t("Undo changes"))).clicked() {
            table.edit = table.row.clone();
        }
        if !is_new && ui.add_enabled(!busy, small(t("Delete"))).clicked() {
            actions.push(Action::Delete(kind));
        }
        if changed {
            ui.label(RichText::new(t("Not saved yet.")).size(12.0).color(palette.warning_text));
        }
    });
    if let Some(problem) = problem {
        ui.add(egui::Label::new(RichText::new(problem).size(12.0).color(palette.error_text)).wrap());
    }
    if let Some(help) = kind.help() {
        widgets::muted(ui, palette, help, 12.0);
    }
}

/// The number a table listed under `parent`'s chosen row is filtered on: that row's id (or first key), when stored.
fn parent_link(s: &State, parent: Kind) -> Option<i64> {
    let spec = parent.spec();
    s.tables[parent.index()].row.as_ref().filter(|r| !content_admin::is_new(spec, r)).map(|r| content_admin::link_value(spec, r))
}

/// The chosen row of the table this one is listed under, or None.
fn parent_row(s: &State, kind: Kind) -> Option<&Row> {
    kind.parent().and_then(|p| s.tables[p.index()].row.as_ref())
}

/// The number of a new row: one above the highest in `column` among the rows with the same `group` value.
fn next_number(spec: &TableSpec, rows: &[Row], row: &Row, column: &str, floor: i64, group: Option<&str>) -> i64 {
    let same = |r: &Row| group.is_none_or(|g| r.get(spec, g) == row.get(spec, g));
    content_admin::next_number(rows.iter().filter(|r| same(r)).filter_map(|r| r.get(spec, column).trim().parse().ok()), floor)
}

fn field_editor(ui: &mut egui::Ui, kind: Kind, field: &Field, value: &mut String) {
    match field.kind {
        FieldKind::Flag => {
            let mut on = value.trim() == "1";
            if ui.checkbox(&mut on, "").changed() {
                *value = if on { "1" } else { "0" }.to_string();
            }
        }
        FieldKind::Choice(options) => {
            egui::ComboBox::from_id_salt(("content-choice", kind.index(), field.column)).selected_text(value.as_str()).width(220.0).show_ui(ui, |ui| {
                for option in options {
                    ui.selectable_value(value, option.to_string(), *option);
                }
            });
        }
        FieldKind::Int { .. } | FieldKind::OptInt { .. } => {
            ui.add_sized([140.0, FIELD_HEIGHT], egui::TextEdit::singleline(value));
        }
        FieldKind::Text { max } if field.column == "goal_type" => {
            ui.horizontal(|ui| {
                ui.add_sized([220.0, FIELD_HEIGHT], egui::TextEdit::singleline(value).char_limit(max));
                egui::ComboBox::from_id_salt(("content-goal", kind.index())).selected_text("").width(28.0).show_ui(ui, |ui| {
                    for goal in GOAL_TYPES {
                        ui.selectable_value(value, goal.to_string(), *goal);
                    }
                });
            });
        }
        FieldKind::Text { max } => {
            let width = (max as f32 * 8.0).clamp(160.0, 460.0);
            ui.add_sized([width, FIELD_HEIGHT], egui::TextEdit::singleline(value).char_limit(max));
        }
    }
}

/// The tables listed under the row shown (a poll's questions, a pet's breeds, sounds and commands), under its form.
fn sub_tables(ui: &mut egui::Ui, palette: &Palette, s: &mut State, kind: Kind, actions: &mut Vec<Action>) {
    let subs = kind.sub_tables();
    if subs.is_empty() || parent_link(s, kind).is_none() {
        return;
    }
    ui.add_space(14.0);
    ui.separator();
    if subs.len() > 1 {
        ui.horizontal(|ui| {
            for (i, sub) in subs.iter().enumerate() {
                if ui.selectable_label(s.sub_table == i, widgets::title_text(sub.heading(), 15.0)).clicked() {
                    s.sub_table = i;
                }
            }
        });
    }
    let sub = subs[s.sub_table.min(subs.len() - 1)];
    if subs.len() == 1 {
        ui.label(widgets::title_text(sub.heading(), 15.0));
    }
    ui.add_space(4.0);
    list(ui, palette, s, sub, 160.0);
    ui.add_space(8.0);
    form(ui, palette, s, sub, actions);
}

fn currencies(ui: &mut egui::Ui, palette: &Palette, s: &mut State, actions: &mut Vec<Action>) {
    let busy = s.busy || s.session.is_none();
    let Some(settings) = s.settings.as_ref() else {
        widgets::muted(ui, palette, t("Not loaded yet."), 12.5);
        return;
    };
    ui.label(widgets::title_text(t("Currency settings"), 17.0));
    widgets::muted(ui, palette, t("The emulator settings that name the currency types: seasonal.types lists them, the seasonal.currency.* settings give a name its type."), 12.0);
    ui.add_space(6.0);
    if settings.is_empty() {
        widgets::muted(ui, palette, t("None of these settings are in emulator_settings."), 12.5);
    }
    egui::Grid::new("content-settings").num_columns(4).spacing([12.0, 6.0]).show(ui, |ui| {
        for (i, setting) in settings.iter().enumerate() {
            ui.label(RichText::new(&setting.key).size(12.5).monospace());
            let Some(edit) = s.setting_edits.get_mut(i) else { continue };
            ui.add_sized([260.0, FIELD_HEIGHT], egui::TextEdit::singleline(edit).char_limit(512));
            let changed = edit.trim() != setting.value;
            let problem = content_admin::setting_problem(&setting.key, edit);
            ui.horizontal(|ui| {
                let button = ui.add_enabled(changed && problem.is_none() && !busy, small(t("Save")));
                if button.clicked() {
                    actions.push(Action::SaveSetting(i));
                }
                if let Some(problem) = &problem {
                    ui.label(RichText::new(problem).size(11.5).color(palette.error_text));
                } else if changed {
                    ui.label(RichText::new(t("Not saved yet.")).size(11.5).color(palette.warning_text));
                }
            });
            ui.add(egui::Label::new(RichText::new(&setting.comment).size(11.5).color(palette.subtle)).truncate());
            ui.end_row();
        }
    });
    ui.add_space(14.0);
    ui.label(widgets::title_text(t("Currencies held by players"), 15.0));
    ui.add_space(4.0);
    if s.totals.is_empty() {
        widgets::muted(ui, palette, t("No player holds a seasonal currency yet."), 12.5);
        return;
    }
    let names = currency_names(settings);
    egui::Grid::new("content-totals").num_columns(4).striped(true).spacing([18.0, 4.0]).show(ui, |ui| {
        for heading in [t("Type"), t("Name"), t("Players"), t("In all")] {
            ui.label(RichText::new(heading).size(12.0).strong());
        }
        ui.end_row();
        for total in &s.totals {
            ui.label(total.kind.to_string());
            ui.label(names.iter().find(|(kind, _)| *kind == total.kind).map_or("", |(_, name)| name.as_str()));
            ui.label(total.players.to_string());
            ui.label(total.amount.to_string());
            ui.end_row();
        }
    });
}

/// The names the settings give currency types: `seasonal.currency.<name> = <type>`.
fn currency_names(settings: &[Setting]) -> Vec<(i64, String)> {
    settings
        .iter()
        .filter_map(|s| {
            let name = s.key.strip_prefix("seasonal.currency.")?;
            (name != "names").then(|| s.value.trim().parse().ok().map(|kind| (kind, name.to_string()))).flatten()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [Kind; KIND_COUNT] = [
        Kind::Achievements,
        Kind::DailyTasks,
        Kind::Quests,
        Kind::Polls,
        Kind::Questions,
        Kind::PublicCategories,
        Kind::PublicRooms,
        Kind::RoomCategories,
        Kind::Pets,
        Kind::PetBreeds,
        Kind::PetSounds,
        Kind::PetCommands,
        Kind::PetCommandList,
        Kind::BadgeParts,
        Kind::ChatBubbles,
        Kind::Calendars,
        Kind::CalendarRewards,
    ];

    #[test]
    fn every_spec_fits_its_table() {
        for kind in KINDS {
            let spec = kind.spec();
            let row = content_admin::new_row(spec, Some(4));
            assert_eq!(row.values.len(), spec.fields.len());
            assert_eq!(kind.index(), KINDS.iter().position(|k| *k == kind).unwrap());
            for column in kind.unique().iter().chain(spec.key).chain(spec.parent.iter()) {
                assert!(spec.fields.iter().any(|f| f.column == *column), "{column} of {}", spec.table);
            }
            assert_eq!(content_admin::is_keyed(spec), spec.id.is_empty(), "{} has an id or a key", spec.table);
            for sub in kind.sub_tables() {
                assert_eq!(sub.parent(), Some(kind));
            }
            if let Some((column, _, group)) = kind.numbered() {
                assert!(spec.key.contains(&column), "{column} of {} is a key", spec.table);
                assert!(group.is_none_or(|g| spec.key.contains(&g)));
            }
            for field in spec.fields {
                if let FieldKind::Choice(options) = field.kind {
                    assert!(options.contains(&field.default), "{} of {}", field.column, spec.table);
                }
            }
        }
        assert_eq!(content_admin::new_row(&QUESTIONS, Some(4)).get(&QUESTIONS, "poll_id"), "4");
        for tab in TABS {
            assert!(!tab.reload_note().is_empty());
            if let Some(kind) = tab.kind() {
                assert_eq!(kind.parent(), None, "a tab shows a top table");
            }
        }
    }

    fn stored(spec: &TableSpec, id: i64, values: &[&str]) -> Row {
        let mut row = Row { id, values: values.iter().map(|v| v.to_string()).collect(), key: Vec::new() };
        if content_admin::is_keyed(spec) {
            row.key = content_admin::key_values(spec, &row);
        }
        row
    }

    #[test]
    fn the_new_tables_are_checked() {
        let base = stored(&BADGE_PARTS, 1, &["base", "1", "base_basic_1.gif", "", "1"]);
        let color = stored(&BADGE_PARTS, 2, &["base_color", "1", "ffd601", "", "1"]);
        let mut part = content_admin::new_row(&BADGE_PARTS, None);
        assert!(Kind::BadgeParts.problem(&part, &[], None).is_some(), "an image is needed");
        part.values[2] = "base_basic_2.gif".into();
        assert!(Kind::BadgeParts.problem(&part, &[base.clone(), color.clone()], None).is_some(), "base 1 is there");
        assert_eq!(next_number(&BADGE_PARTS, &[base.clone(), color.clone()], &part, "id", 1, Some("type")), 2);
        part.values[1] = "2".into();
        assert_eq!(Kind::BadgeParts.problem(&part, &[base.clone(), color.clone()], None), None);
        part.values[0] = "symbol_color".into();
        assert!(Kind::BadgeParts.problem(&part, &[], None).is_some(), "a colour is hex");

        let campaign = stored(&CALENDARS, 3, &["Winter", "", "0", "24", "1", "1"]);
        let mut reward = content_admin::new_row(&CALENDAR_REWARDS, Some(3));
        assert_eq!(reward.get(&CALENDAR_REWARDS, "campaign_id"), "3");
        assert!(Kind::CalendarRewards.problem(&reward, &[], Some(&campaign)).is_some(), "it gives nothing");
        reward.values[4] = "100".into();
        assert_eq!(Kind::CalendarRewards.problem(&reward, &[], Some(&campaign)), None);
        reward.values[1] = "25".into();
        assert!(Kind::CalendarRewards.problem(&reward, &[], Some(&campaign)).is_some(), "day 25 of 24");
        reward.values[1] = "24".into();
        assert_eq!(Kind::CalendarRewards.problem(&reward, &[], Some(&campaign)), None);
        assert_eq!(content_admin::param(&CALENDAR_REWARDS.fields[1], ""), aio_remote::sql_session::SqlValue::Null);

        let mut bubble = content_admin::new_row(&CHAT_BUBBLES, None);
        bubble.values[1] = "GOLD".into();
        assert_eq!(Kind::ChatBubbles.problem(&bubble, &[], None), None);
        bubble.values[0] = "12".into();
        assert!(Kind::ChatBubbles.problem(&bubble, &[], None).is_some(), "the client's own styles are below 46");
        let taken = stored(&CHAT_BUBBLES, 1, &["200", "SHOW_MESSAGE_RED", "", "1", "0", "0"]);
        assert_eq!(next_number(&CHAT_BUBBLES, &[taken.clone()], &bubble, "type", 46, None), 201);
        bubble.values[0] = "200".into();
        assert!(Kind::ChatBubbles.problem(&bubble, &[taken], None).is_some(), "200 is there");

        let dog = stored(&PETS, 1, &["0", "Dog", "29", "", "", "", "0"]);
        assert!(!content_admin::is_new(&PETS, &dog), "pet type 0 is a stored row, not a new one");
        assert_eq!(content_admin::link_value(&PETS, &dog), 0);
        let breed = stored(&PET_BREEDS, 1, &["0", "1", "1", "1", "0", "0"]);
        let mut same = content_admin::new_row(&PET_BREEDS, Some(0));
        same.values = breed.values.clone();
        assert!(Kind::PetBreeds.problem(&same, &[breed], None).is_some(), "the breed is there");
        let mut sound = content_admin::new_row(&PET_SOUNDS, Some(0));
        assert!(Kind::PetSounds.problem(&sound, &[], None).is_some(), "a message is needed");
        sound.values[2] = "Woof!".into();
        assert_eq!(Kind::PetSounds.problem(&sound, &[], None), None);

        let mut room = content_admin::new_row(&PUBLIC_ROOMS, Some(2));
        assert!(Kind::PublicRooms.problem(&room, &[], None).is_some(), "a room id is needed");
        room.values[1] = "41".into();
        assert_eq!(Kind::PublicRooms.problem(&room, &[], None), None);
        let listed = stored(&PUBLIC_ROOMS, 1, &["2", "41", "1"]);
        assert!(Kind::PublicRooms.problem(&room, &[listed], None).is_some(), "room 41 is in the category");
        assert!(Kind::RoomCategories.problem(&content_admin::new_row(&ROOM_CATEGORIES, None), &[], None).is_some(), "a caption is needed");
    }

    #[test]
    fn rows_are_checked_per_table() {
        let mut achievement = content_admin::new_row(&ACHIEVEMENTS, None);
        assert!(Kind::Achievements.problem(&achievement, &[], None).is_some(), "a name is needed");
        achievement.values[0] = "RoomEntry".into();
        assert_eq!(Kind::Achievements.problem(&achievement, &[], None), None);
        let other = Row { id: 9, values: achievement.values.clone(), key: Vec::new() };
        assert!(Kind::Achievements.problem(&achievement, &[other], None).is_some(), "the level is there already");

        let mut task = content_admin::new_row(&DAILY_TASKS, None);
        task.values[0] = "daily_place".into();
        assert_eq!(Kind::DailyTasks.problem(&task, &[], None), None);
        task.values[1] = "fly".into();
        assert!(Kind::DailyTasks.problem(&task, &[], None).is_some());

        let mut question = content_admin::new_row(&QUESTIONS, Some(1));
        question.values[2] = "Do you like the hotel?".into();
        assert_eq!(Kind::Questions.problem(&question, &[], None), None);
        question.values[5] = "Yes".into();
        assert!(Kind::Questions.problem(&question, &[], None).is_some());
    }

    #[test]
    fn currency_names_come_from_the_settings() {
        let setting = |key: &str, value: &str| Setting { key: key.into(), value: value.into(), comment: String::new() };
        let names = currency_names(&[setting("seasonal.currency.ducket", "0"), setting("seasonal.currency.names", "ducket;pixel"), setting("seasonal.types", "0;5")]);
        assert_eq!(names, vec![(0, "ducket".to_string())]);
    }
}

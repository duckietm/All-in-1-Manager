//! The items_base.interaction_type a furni gets from the logicType of its bundle (.nitro or .hab json), for the
//! Polaris emulator: each value is a name its ItemManager registers (`new ItemInteraction("name", Class)`, compared
//! case-insensitively; a name it does not know is InteractionDefault).
//!
//! Only logic types with a Polaris interaction of their own are mapped; the rest stay 'default':
//! - furniture_basic, furniture_multistate (and typos like furniture_muItistate), furniture_soundblock, window, teaser,
//!   static, gift, mysterytrophy, editable_room_link, sound_machine: plain furni for Polaris;
//! - furniture_credit / nft_credit: Polaris redeems them by their CF_ / CFC_ name (CreditFurni), not by interaction;
//! - pet_customization (by the horse_dye* name), crafting_gizmo (crafting_altars by item id), ecotron_box (by name);
//! - es (Freeze: tile, block, exit and four gates share it), score / hockey_score (one counter per team colour):
//!   one logic covers several Polaris interactions, so the owner picks them by hand;
//! - welcome_gift, group_forum_terminal, achievement_resolution and the rest: no Polaris interaction.
//!
//! furniture_present stays 'default' for now (owner's decision pending). Polaris' 'gift' (InteractionGift) is the
//! wrapped present: the gift wrapping settings must name 'gift' rows for wrapped presents to open, but a present furni
//! bought straight from the catalogue (the import sells every new furni on a page) opens to nothing and is deleted.
//! Turning it on is the one commented line in [`LOGIC_TYPES`].

/// (logicType without "furniture_", in lower case; Polaris interaction_type).
///
/// furniture_multiheight is not here: such a furni is 'multiheight' only when the heights of its states are found in
/// its bundle (furni_import::multiheight_heights), else 'default' (Polaris needs the heights).
pub const LOGIC_TYPES: &[(&str, &str)] = &[
    ("recolorable", "recolorable"),
    ("purchasable_clothing", "clothing"),
    ("crackable", "crackable"),
    ("guild_customized", "guild_furni"),
    ("trophy", "trophy"),
    ("dice", "dice"),
    ("badge_display", "badge_display"),
    ("external_image_wallitem", "external_image"),
    ("effectbox", "fx_box"),
    ("fireworks", "fireworks"),
    ("lovelock", "love_lock"),
    ("hween_lovelock", "love_lock"),
    ("hbnxmas24_lovelock", "love_lock"),
    ("hubbe_lovelock_infinitylove", "love_lock"),
    ("stickie", "postit"),
    ("post_it", "postit"),
    ("change_state_when_step_on", "pressureplate"),
    ("pressureplate", "pressureplate"),
    // ("present", "gift"), -- see the module comment: catalogue-bought presents would open empty.
    // Habbo's wheel of fortune: rights-only click, rolls a random state (InteractionColorWheel).
    ("habbowheel", "colorwheel"),
    // highscore_* boards (InteractionWiredHighscore reads the score type from the name, guarded).
    ("high_score", "wf_highscore"),
    // fball_counter, bb_counter, es_counter...: the game clock (InteractionGameTimer).
    ("counter_clock", "game_timer"),
    // hole*, black_hole* (InteractionBlackHole, a gate).
    ("floor_hole", "blackhole"),
    // Balls (bb_puck / es_puck would rather be battlebanzai_puck: set by hand).
    ("pushable", "football"),
    // *_hopper, random_tele* (InteractionHopper).
    ("random_teleport", "hopper"),
    // tile_stackmagic*, stapelfeld_* (InteractionStackHelper).
    ("custom_stack_height", "stack_helper"),
    // info_terminal* (InteractionInformationTerminal: custom value internalLink). tile_fxprovider_nfs shares the
    // logic but has its own Polaris name: set by hand.
    ("internal_link", "information_terminal"),
    ("editable_internal_link", "information_terminal"),
    ("roomdimmer", "dimmer"),
    ("jukebox", "jukebox"),
    ("song_disk", "musicdisc"),
    ("youtube", "youtube"),
    ("vimeo", "vimeo"),
    ("rentable_space", "rentable_space"),
    ("mannequin", "mannequin"),
    ("monsterplant_seed", "monsterplant_seed"),
    ("vote_counter", "vote_counter"),
    ("mysterybox", "mystery_box"),
    ("randomstate", "random_state"),
    ("one_way_door", "onewaygate"),
    ("background_color", "background_toner"),
    // Branded images: ads_background (bg) and ads_mpu_* billboards (bb), both InteractionRoomAds.
    ("bg", "ads_bg"),
    ("bb", "ads_bg"),
    ("area_hide", "conf_area_hide"),
    // wf_storage_furni* / wf_storage_coins* wired chests.
    ("furnichest", "wf_storage_furni1"),
    ("coinschest", "wf_storage_coins1"),
    // Its one bundle is fball_gate, the football outfit gate.
    ("clothing_change", "football_gate"),
];

/// Furni whose Polaris interaction follows their name, not their logicType: (bundle name in lower case, how it is
/// matched, Polaris interaction_type). Checked before [`LOGIC_TYPES`].
pub const NAME_RULES: &[(&str, NameMatch, &str)] = &[
    // tile_fxprovider_nfs, waterfxprovider_nfs: furniture_internal_link, but 'information_terminal' would open the
    // commands page.
    ("fxprovider", NameMatch::Contains, "tile_fxprovider_nfs"),
    // Battle Banzai's puck (es_puck and the football keep 'football' from furniture_pushable).
    ("bb_puck", NameMatch::Prefix, "battlebanzai_puck"),
    ("hween12_costume_hopper", NameMatch::Prefix, "costume_hopper"),
    ("vip_hopper", NameMatch::Prefix, "club_hopper"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameMatch {
    Prefix,
    Contains,
}

/// The Polaris interaction_type a furni's name decides (its bundle name, any case: "bb_puck*3" counts as bb_puck).
pub fn for_name(classname: &str) -> Option<&'static str> {
    let name = classname.split('*').next().unwrap_or("").trim().to_ascii_lowercase();
    NAME_RULES
        .iter()
        .find(|(rule, how, _)| match how {
            NameMatch::Prefix => name.starts_with(rule),
            NameMatch::Contains => name.contains(rule),
        })
        .map(|(_, _, to)| *to)
}

/// The Polaris interaction_type of a bundle's logicType ("furniture_dice" or "dice", any case); None: 'default'.
pub fn for_logic_type(logic_type: &str) -> Option<&'static str> {
    let logic = logic_type.trim().to_ascii_lowercase();
    let logic = logic.strip_prefix("furniture_").unwrap_or(&logic);
    LOGIC_TYPES.iter().find(|(from, _)| *from == logic).map(|(_, to)| *to)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 567 names Polaris' ItemManager registers (polaris/hotel/items/ItemManager.java, 2026-11-25; one
    /// registration, wf_act_give_hotelview_bonus_rare_points, spans two lines).
    const POLARIS: &str = include_str!("../tests/fixtures/polaris_interactions.txt");

    #[test]
    fn every_target_is_a_polaris_interaction() {
        let names: std::collections::HashSet<String> = POLARIS.lines().map(|l| l.trim().to_ascii_lowercase()).filter(|l| !l.is_empty()).collect();
        assert_eq!(names.len(), 567);
        assert!(names.contains("wf_act_give_hotelview_bonus_rare_points"), "the two-line registration");
        for (logic, interaction) in LOGIC_TYPES {
            assert!(names.contains(*interaction), "{logic} -> {interaction} is not registered in Polaris");
        }
        for (name, _, interaction) in NAME_RULES {
            assert!(names.contains(*interaction), "{name} -> {interaction} is not registered in Polaris");
        }
        assert!(names.contains("default") && names.contains("gift"));
    }

    #[test]
    fn each_logic_type_once_in_lower_case() {
        let mut seen = std::collections::HashSet::new();
        for (logic, _) in LOGIC_TYPES {
            assert_eq!(*logic, logic.to_ascii_lowercase(), "{logic}");
            assert!(!logic.starts_with("furniture_"), "{logic}");
            assert!(seen.insert(*logic), "{logic} twice");
        }
    }

    #[test]
    fn names_decide_first() {
        assert_eq!(for_name("tile_fxprovider_nfs"), Some("tile_fxprovider_nfs"));
        assert_eq!(for_name("WaterFxProvider_nfs"), Some("tile_fxprovider_nfs"));
        assert_eq!(for_name("bb_puck"), Some("battlebanzai_puck"));
        assert_eq!(for_name("bb_puck*2"), Some("battlebanzai_puck"));
        assert_eq!(for_name("hween12_costume_hopper"), Some("costume_hopper"));
        assert_eq!(for_name("vip_hopper"), Some("club_hopper"));
        for other in ["es_puck", "fball_ball", "info_terminal", "xbb_puck", "hopper_vip"] {
            assert_eq!(for_name(other), None, "{other}");
        }
    }

    #[test]
    fn logic_types_map() {
        assert_eq!(for_logic_type("furniture_dice"), Some("dice"));
        assert_eq!(for_logic_type("Furniture_Trophy"), Some("trophy"));
        assert_eq!(for_logic_type("stickie"), Some("postit"));
        assert_eq!(for_logic_type("furniture_hween_lovelock"), Some("love_lock"));
        assert_eq!(for_logic_type("hubbe_lovelock_infinitylove"), Some("love_lock"));
        assert_eq!(for_logic_type("crackable"), Some("crackable"));
        assert_eq!(for_logic_type("pressureplate"), Some("pressureplate"));
        assert_eq!(for_logic_type("furniture_habbowheel"), Some("colorwheel"));
        assert_eq!(for_logic_type("furniture_high_score"), Some("wf_highscore"));
        assert_eq!(for_logic_type("furniture_counter_clock"), Some("game_timer"));
        assert_eq!(for_logic_type("furniture_custom_stack_height"), Some("stack_helper"));
        assert_eq!(for_logic_type("FURNITURE_BB"), Some("ads_bg"));
        for other in [
            "furniture_present",
            "furniture_multiheight",
            "furniture_basic",
            "furniture_multistate",
            "furniture_muItistate",
            "furniture_soundblock",
            "furniture_credit",
            "furniture_es",
            "furniture_score",
            "furniture_window",
            "furniture_pet_customization",
            "",
            "dice2",
        ] {
            assert_eq!(for_logic_type(other), None, "{other}");
        }
    }
}

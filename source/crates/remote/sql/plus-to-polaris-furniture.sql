-- Part of the All-in-1 Installer's "Import Plus database" tool (Remote/PlusImport.cs).
-- @S@ is the staging database with the PlusEMU dump, @T@ the hotel's Polaris database; the tool fills them in.
-- Lines starting with a KEY= value in the output are the tool's report.
SET SESSION sql_mode = 'NO_ENGINE_SUBSTITUTION';
SET NAMES utf8mb4 COLLATE utf8mb4_general_ci;
SET SESSION group_concat_max_len = 65535;
USE `@S@`;

-- ---------------------------------------------------------------- interactions
-- What Polaris knows: its registered interactions, plus every type its furniture already uses.
DROP TABLE IF EXISTS _pi_known;
CREATE TABLE _pi_known (name VARCHAR(100) COLLATE utf8mb4_general_ci PRIMARY KEY) ENGINE=InnoDB;
INSERT IGNORE INTO _pi_known VALUES 
  ('ads_bg'), ('antenna'), ('background_toner'), ('badge_display'), ('battlebanzai_counter_blue'), ('battlebanzai_counter_green'),
  ('battlebanzai_counter_red'), ('battlebanzai_counter_yellow'), ('battlebanzai_gate_blue'), ('battlebanzai_gate_green'), ('battlebanzai_gate_red'), ('battlebanzai_gate_yellow'),
  ('battlebanzai_puck'), ('battlebanzai_random_teleport'), ('battlebanzai_sphere'), ('battlebanzai_tile'), ('blackhole'), ('breeding_nest'),
  ('buildarea'), ('bunnyrun_field'), ('bunnyrun_pole'), ('cannon'), ('clothing'), ('club_gate'),
  ('club_hopper'), ('club_teleporttile'), ('colorplate'), ('colorwheel'), ('conf_area_hide'), ('conf_chooser_disable'),
  ('conf_dice_disable'), ('conf_doorkick_disable'), ('conf_handitem_block'), ('conf_hidewired'), ('conf_invis_control'), ('conf_queue_speed'),
  ('conf_wired_disable'), ('costume_hopper'), ('crackable'), ('crackable_master'), ('crackable_monster'), ('crackable_subscription_box'),
  ('default'), ('dice'), ('dimmer'), ('effect_gate'), ('effect_giver'), ('effect_tile'),
  ('effect_tile_group'), ('effect_toggle'), ('effect_vendingmachine'), ('effect_vendingmachine_no_sides'), ('external_image'), ('fireworks'),
  ('football'), ('football_counter_blue'), ('football_counter_green'), ('football_counter_red'), ('football_counter_yellow'), ('football_gate'),
  ('football_goal_blue'), ('football_goal_green'), ('football_goal_red'), ('football_goal_yellow'), ('freeze_block'), ('freeze_counter_blue'),
  ('freeze_counter_green'), ('freeze_counter_red'), ('freeze_counter_yellow'), ('freeze_exit'), ('freeze_gate_blue'), ('freeze_gate_green'),
  ('freeze_gate_red'), ('freeze_gate_yellow'), ('freeze_tile'), ('fx_box'), ('game_timer'), ('game_upcounter'),
  ('gate'), ('gift'), ('guild_furni'), ('guild_gate'), ('gym_equipment'), ('handitem'),
  ('handitem_tile'), ('hopper'), ('icetag_field'), ('icetag_pole'), ('information_terminal'), ('jukebox'),
  ('love_lock'), ('mannequin'), ('monsterplant_seed'), ('multiheight'), ('musicdisc'), ('mutearea'),
  ('mystery_box'), ('mystery_key'), ('nest'), ('obstacle'), ('onewaygate'), ('pet_drink'),
  ('pet_food'), ('pet_toy'), ('pet_trampoline'), ('pet_tree'), ('plants'), ('postit'),
  ('pressureplate'), ('pressureplate_group'), ('puzzle_box'), ('pyramid'), ('random_state'), ('rebug_football'),
  ('recolorable'), ('rentable_space'), ('roller'), ('rollerskate_field'), ('room_invisible_click_tile'), ('room_o_matic'),
  ('snowboard_slope'), ('snowstorm_machine'), ('snowstorm_pile'), ('snowstorm_tree'), ('stack_helper'), ('stack_walk_helper'),
  ('sticky_pole'), ('switch'), ('switch_remote_control'), ('talking_furni'), ('teleport'), ('teleporttile'),
  ('tent'), ('tile_fxprovider_nfs'), ('tile_walkmagic'), ('totem_head'), ('totem_leg'), ('totem_planet'),
  ('trap'), ('trophy'), ('vendingmachine'), ('vendingmachine_no_sides'), ('viking_cotie'), ('vimeo'),
  ('vote_counter'), ('water'), ('water_can'), ('water_item'), ('wf_act_add_tag'), ('wf_act_add_tag_perm'),
  ('wf_act_adjust_clock'), ('wf_act_alert'), ('wf_act_alert_habbo'), ('wf_act_all_users_leave_team'), ('wf_act_allign_furni_stack'), ('wf_act_bot_clothes'),
  ('wf_act_bot_follow_avatar'), ('wf_act_bot_give_handitem'), ('wf_act_bot_give_handitem_or_effect'), ('wf_act_bot_move'), ('wf_act_bot_start_dance'), ('wf_act_bot_stop_dance'),
  ('wf_act_bot_talk'), ('wf_act_bot_talk_custom'), ('wf_act_bot_talk_to_avatar'), ('wf_act_bot_talk_to_avatar_custom'), ('wf_act_bot_teleport'), ('wf_act_call_stacks'),
  ('wf_act_call_stacks_custom'), ('wf_act_cancel_transaction'), ('wf_act_change_opacity'), ('wf_act_change_var_val'), ('wf_act_chase'), ('wf_act_click_conf'),
  ('wf_act_close_dice'), ('wf_act_close_gates'), ('wf_act_cnd_move_furni'), ('wf_act_cnd_move_rotate'), ('wf_act_cnd_toggle_state'), ('wf_act_color_furni'),
  ('wf_act_control_clock'), ('wf_act_dont_chase'), ('wf_act_dont_chase_top'), ('wf_act_double_click'), ('wf_act_execute_for_users'), ('wf_act_execute_stack_custom'),
  ('wf_act_flee'), ('wf_act_forward_user_to_room'), ('wf_act_freeze'), ('wf_act_freeze_habbo'), ('wf_act_furni_to_furni'), ('wf_act_furni_to_user'),
  ('wf_act_give_achievement'), ('wf_act_give_badge'), ('wf_act_give_credits'), ('wf_act_give_currency'), ('wf_act_give_diamonds'), ('wf_act_give_duckets'),
  ('wf_act_give_effect'), ('wf_act_give_enable'), ('wf_act_give_experience'), ('wf_act_give_furni'), ('wf_act_give_handitem'), ('wf_act_give_hotelview_hof_points'),
  ('wf_act_give_look'), ('wf_act_give_or_take_furni'), ('wf_act_give_points_highscore'), ('wf_act_give_points_type'), ('wf_act_give_respect'), ('wf_act_give_reward'),
  ('wf_act_give_score'), ('wf_act_give_score_custom'), ('wf_act_give_score_pp'), ('wf_act_give_score_room'), ('wf_act_give_score_tm'), ('wf_act_give_userbadge'),
  ('wf_act_give_var'), ('wf_act_init_transaction'), ('wf_act_join_team'), ('wf_act_kick_user'), ('wf_act_lay'), ('wf_act_leave_team'),
  ('wf_act_log'), ('wf_act_lower_furni'), ('wf_act_make_fast_walk'), ('wf_act_make_user_say'), ('wf_act_match_to_sshot'), ('wf_act_match_to_sshot_height'),
  ('wf_act_match_to_sshot_height_instant'), ('wf_act_match_to_sshot_new'), ('wf_act_modify_array'), ('wf_act_move_furni_as_group'), ('wf_act_move_furni_from_stack'), ('wf_act_move_furni_to'),
  ('wf_act_move_furni_to_furni'), ('wf_act_move_group_step'), ('wf_act_move_rotate'), ('wf_act_move_rotate_collide'), ('wf_act_move_rotate_diagonal'), ('wf_act_move_rotate_no_under'),
  ('wf_act_move_rotate_user'), ('wf_act_move_to_dir'), ('wf_act_move_user_tiles'), ('wf_act_mute_triggerer'), ('wf_act_neg_call_stack'), ('wf_act_neg_call_stacks'),
  ('wf_act_neg_log'), ('wf_act_neg_send_signal'), ('wf_act_neg_show_message'), ('wf_act_open_gates'), ('wf_act_open_habbo_pages'), ('wf_act_override_height'),
  ('wf_act_pickup_furni'), ('wf_act_place_furni'), ('wf_act_play_youtube_sound'), ('wf_act_plus_match_furni_state'), ('wf_act_progress_achievement'), ('wf_act_progress_reward_track'),
  ('wf_act_quick_bopper'), ('wf_act_raise_furni'), ('wf_act_rel_mov'), ('wf_act_remove_badge'), ('wf_act_remove_furni'), ('wf_act_remove_tag'),
  ('wf_act_remove_var'), ('wf_act_reset_highscores'), ('wf_act_reset_reward_track'), ('wf_act_reset_timers'), ('wf_act_roll_dice'), ('wf_act_roller_speed'),
  ('wf_act_rotate_habbo'), ('wf_act_say_command'), ('wf_act_send_bubble'), ('wf_act_send_dataframe'), ('wf_act_send_message'), ('wf_act_send_signal'),
  ('wf_act_set_altitude'), ('wf_act_set_highlight'), ('wf_act_set_room_ad'), ('wf_act_set_state'), ('wf_act_set_trg_state'), ('wf_act_show_message'),
  ('wf_act_show_message_room'), ('wf_act_sit'), ('wf_act_spawn_furni'), ('wf_act_tele_room'), ('wf_act_teleport_all'), ('wf_act_teleport_blue'),
  ('wf_act_teleport_bots_to_furni'), ('wf_act_teleport_green'), ('wf_act_teleport_red'), ('wf_act_teleport_to'), ('wf_act_teleport_to_room'), ('wf_act_teleport_yellow'),
  ('wf_act_toggle_moodlight'), ('wf_act_toggle_state'), ('wf_act_toggle_state_down'), ('wf_act_toggle_state_trg'), ('wf_act_toggle_to_rnd'), ('wf_act_tp_furni_to_habbo'),
  ('wf_act_unfreeze'), ('wf_act_unfreeze_habbo'), ('wf_act_user_to_furni'), ('wf_act_walk_to_furni'), ('wf_blob'), ('wf_cnd_actor_dir'),
  ('wf_cnd_actor_in_group'), ('wf_cnd_actor_in_team'), ('wf_cnd_atleast_one_user_in_team'), ('wf_cnd_battlebanzai'), ('wf_cnd_bot_is_dancing'), ('wf_cnd_check_array'),
  ('wf_cnd_chest_has_item_type'), ('wf_cnd_chest_has_items'), ('wf_cnd_counter_time_matches'), ('wf_cnd_daily_trg'), ('wf_cnd_date_rng_active'), ('wf_cnd_first_trg'),
  ('wf_cnd_freeze'), ('wf_cnd_furni_in_range'), ('wf_cnd_furni_not_in_range'), ('wf_cnd_furni_opacity_is'), ('wf_cnd_furni_state_pattern'), ('wf_cnd_furnis_hv_avtrs'),
  ('wf_cnd_furnis_hv_avtrs_custom'), ('wf_cnd_habbo_has_at_least_x_items'), ('wf_cnd_habbo_has_credits'), ('wf_cnd_habbo_has_duckets'), ('wf_cnd_habbo_has_rank'), ('wf_cnd_habbo_has_rights'),
  ('wf_cnd_habbo_in_group'), ('wf_cnd_habbo_is_dancing'), ('wf_cnd_habbo_is_female'), ('wf_cnd_habbo_is_male'), ('wf_cnd_habbo_not_owns_furni'), ('wf_cnd_habbo_owns_badge'),
  ('wf_cnd_habbo_owns_furni'), ('wf_cnd_has_altitude'), ('wf_cnd_has_furni_on'), ('wf_cnd_has_handitem'), ('wf_cnd_has_same_height'), ('wf_cnd_has_tag'),
  ('wf_cnd_has_var'), ('wf_cnd_is_state'), ('wf_cnd_match_date'), ('wf_cnd_match_snapshot'), ('wf_cnd_match_snapshot_new'), ('wf_cnd_match_time'),
  ('wf_cnd_motto_contains'), ('wf_cnd_neg_has_var'), ('wf_cnd_not_battlebanzai'), ('wf_cnd_not_battlebz'), ('wf_cnd_not_bot_is_dancing'), ('wf_cnd_not_freeze'),
  ('wf_cnd_not_furni_on'), ('wf_cnd_not_furni_opacity_is'), ('wf_cnd_not_habbo_has_credits'), ('wf_cnd_not_habbo_has_diamonds'), ('wf_cnd_not_habbo_has_duckets'), ('wf_cnd_not_habbo_has_rank'),
  ('wf_cnd_not_habbo_has_rights'), ('wf_cnd_not_habbo_in_group'), ('wf_cnd_not_habbo_is_dancing'), ('wf_cnd_not_habbo_owns_badge'), ('wf_cnd_not_has_handitem'), ('wf_cnd_not_has_same_height'),
  ('wf_cnd_not_has_tag'), ('wf_cnd_not_hv_avtrs'), ('wf_cnd_not_in_group'), ('wf_cnd_not_in_team'), ('wf_cnd_not_match_snap'), ('wf_cnd_not_match_snap_new'),
  ('wf_cnd_not_stuff_is'), ('wf_cnd_not_tgr_furni_hv_avtrs'), ('wf_cnd_not_trg_by_user'), ('wf_cnd_not_trggrer_on'), ('wf_cnd_not_triggerer_match'), ('wf_cnd_not_user_count'),
  ('wf_cnd_not_user_performs_action'), ('wf_cnd_not_wearing_b'), ('wf_cnd_not_wearing_fx'), ('wf_cnd_not_wears_effect'), ('wf_cnd_not_wears_handitem'), ('wf_cnd_not_x_points_leaderboard'),
  ('wf_cnd_slc_quantity'), ('wf_cnd_stuff_is'), ('wf_cnd_team_has_rank'), ('wf_cnd_team_has_score'), ('wf_cnd_tgr_furni_hv_avtrs'), ('wf_cnd_time_less_than'),
  ('wf_cnd_time_more_than'), ('wf_cnd_trg_by_user'), ('wf_cnd_trg_frn_adjacent_state'), ('wf_cnd_trg_state_is'), ('wf_cnd_trggrer_on_frn'), ('wf_cnd_trgr_stuff_matches'),
  ('wf_cnd_triggerer_match'), ('wf_cnd_user_cooldown'), ('wf_cnd_user_count_in'), ('wf_cnd_user_in_range'), ('wf_cnd_user_level'), ('wf_cnd_user_not_in_range'),
  ('wf_cnd_user_on_furni_with_state'), ('wf_cnd_user_performs_action'), ('wf_cnd_valid_moves'), ('wf_cnd_var_age_match'), ('wf_cnd_var_val_match'), ('wf_cnd_wearing_badge'),
  ('wf_cnd_wearing_effect'), ('wf_cnd_wears_effect'), ('wf_cnd_wears_handitem'), ('wf_cnd_x_habbos_on_furni'), ('wf_cnd_x_points_leaderboard'), ('wf_conf_area_hide'),
  ('wf_conf_dice_disable'), ('wf_conf_doorkick_disable'), ('wf_conf_handitem_block'), ('wf_conf_hidewired'), ('wf_conf_invis_control'), ('wf_conf_queue_speed'),
  ('wf_conf_wired_disable'), ('wf_contract_payment'), ('wf_contract_reward'), ('wf_contract_trade'), ('wf_highscore'), ('wf_label'),
  ('wf_link'), ('wf_room_linker'), ('wf_slc_furni_altitude'), ('wf_slc_furni_area'), ('wf_slc_furni_bytype'), ('wf_slc_furni_neighborhood'),
  ('wf_slc_furni_onfurni'), ('wf_slc_furni_picks'), ('wf_slc_furni_signal'), ('wf_slc_furni_with_var'), ('wf_slc_remote'), ('wf_slc_users_area'),
  ('wf_slc_users_byaction'), ('wf_slc_users_byname'), ('wf_slc_users_bytype'), ('wf_slc_users_group'), ('wf_slc_users_handitem'), ('wf_slc_users_neighborhood'),
  ('wf_slc_users_onfurni'), ('wf_slc_users_signal'), ('wf_slc_users_team'), ('wf_slc_users_with_var'), ('wf_storage_coins1'), ('wf_storage_coins2'),
  ('wf_storage_furni1'), ('wf_storage_furni2'), ('wf_storage_furni_starter'), ('wf_trg_anti_afk'), ('wf_trg_at_given_time'), ('wf_trg_at_time_long'),
  ('wf_trg_bot_reached_avtr'), ('wf_trg_bot_reached_stf'), ('wf_trg_click_bot'), ('wf_trg_click_furni'), ('wf_trg_click_tile'), ('wf_trg_click_user'),
  ('wf_trg_clock_counter'), ('wf_trg_cnd_collision'), ('wf_trg_collision'), ('wf_trg_dice_rolled'), ('wf_trg_double_click_furni'), ('wf_trg_enter_room'),
  ('wf_trg_exact_keyword'), ('wf_trg_game_ends'), ('wf_trg_game_starts'), ('wf_trg_game_team_lose'), ('wf_trg_game_team_win'), ('wf_trg_habbo_says_command'),
  ('wf_trg_idles'), ('wf_trg_leave_room'), ('wf_trg_other_collides_user'), ('wf_trg_period_long'), ('wf_trg_period_short'), ('wf_trg_periodically'),
  ('wf_trg_press_keybind'), ('wf_trg_receive_dataframe'), ('wf_trg_receive_message'), ('wf_trg_recv_signal'), ('wf_trg_room_initialized'), ('wf_trg_says_command'),
  ('wf_trg_says_something'), ('wf_trg_score_achieved'), ('wf_trg_starts_dancing'), ('wf_trg_state_changed'), ('wf_trg_stops_dancing'), ('wf_trg_stuff_state'),
  ('wf_trg_transaction_complete'), ('wf_trg_transaction_fail'), ('wf_trg_unidles'), ('wf_trg_user_collides_bot'), ('wf_trg_user_collides_other'), ('wf_trg_user_exits_room'),
  ('wf_trg_user_gets_handitem'), ('wf_trg_user_performs_action'), ('wf_trg_username_as_trigger'), ('wf_trg_var_changed'), ('wf_trg_walks_off_furni'), ('wf_trg_walks_on_furni'),
  ('wf_var_context'), ('wf_var_daily_task'), ('wf_var_echo'), ('wf_var_furni'), ('wf_var_pointer'), ('wf_var_quest'),
  ('wf_var_quest_chain'), ('wf_var_reference'), ('wf_var_room'), ('wf_var_user'), ('wf_xtra_achievement_enabler'), ('wf_xtra_anim_time'),
  ('wf_xtra_array_capture_variable'), ('wf_xtra_condition_change'), ('wf_xtra_custom_contract'), ('wf_xtra_exec_in_order'), ('wf_xtra_execution_limit'), ('wf_xtra_filter_furni'),
  ('wf_xtra_filter_furni_by_var'), ('wf_xtra_filter_user'), ('wf_xtra_filter_users'), ('wf_xtra_filter_users_by_var'), ('wf_xtra_link_speaker'), ('wf_xtra_mov_carry_users'),
  ('wf_xtra_mov_curve'), ('wf_xtra_mov_no_animation'), ('wf_xtra_mov_physics'), ('wf_xtra_one_condition'), ('wf_xtra_or_eval'), ('wf_xtra_random'),
  ('wf_xtra_rotate_to_dir'), ('wf_xtra_scan_chest_furni_by_type'), ('wf_xtra_text_input_variable'), ('wf_xtra_text_output_furni_name'), ('wf_xtra_text_output_global'), ('wf_xtra_text_output_username'),
  ('wf_xtra_text_output_variable'), ('wf_xtra_unseen'), ('wf_xtra_var_fx_boss'), ('wf_xtra_var_fx_health'), ('wf_xtra_var_fx_level'), ('wf_xtra_var_fx_number'),
  ('wf_xtra_var_fx_progress'), ('wf_xtra_var_fx_status'), ('wf_xtra_var_import'), ('wf_xtra_var_leaderboard'), ('wf_xtra_var_lvlup_system'), ('wf_xtra_var_share'),
  ('wf_xtra_var_text_connector'), ('wf_xtra_var_time_util'), ('wf_xtra_var_web_api'), ('wf_xtra_varfx_boss'), ('wf_xtra_varfx_hp'), ('wf_xtra_varfx_levelling'),
  ('wf_xtra_varfx_number'), ('wf_xtra_varfx_prog'), ('wf_xtra_varfx_status'), ('youtube');
INSERT IGNORE INTO _pi_known SELECT DISTINCT LOWER(CONVERT(interaction_type USING utf8mb4) COLLATE utf8mb4_general_ci) FROM `@T@`.items_base;

-- Furniture Polaris drives by classname rather than by the Plus type.
DROP TABLE IF EXISTS _pi_by_name;
CREATE TABLE _pi_by_name (item_name VARCHAR(100) COLLATE utf8mb4_general_ci PRIMARY KEY, interaction VARCHAR(100) COLLATE utf8mb4_general_ci) ENGINE=InnoDB;
INSERT INTO _pi_by_name VALUES 
  ('wf_act_progress_ach', 'wf_act_progress_achievement'), ('wf_act_set_roller_spd', 'wf_act_roller_speed'),
  ('wf_act_set_visibility', 'wf_act_change_opacity'), ('wf_floor_switch1', 'switch'),
  ('wf_floor_switch2', 'switch'), ('wf_game_upcounter1', 'game_upcounter'),
  ('wf_game_upcounter2', 'game_upcounter'), ('wf_ltdproto_act_toggle_state', 'wf_act_toggle_state'),
  ('wf_proto_cnd_trggrer_on_frn', 'wf_cnd_trggrer_on_frn'), ('wf_proto_trg_at_given_time', 'wf_trg_at_given_time'),
  ('wf_trg_room_initializes', 'wf_trg_room_initialized'), ('wf_trg_says_sycommand', 'wf_trg_says_command'),
  ('wf_upcounter1', 'game_upcounter'), ('wf_upcounter2', 'game_upcounter'),
  ('wf_var_dailytask', 'wf_var_daily_task');

-- Plus types that have another name in Polaris (the reverse of plus-migrator's renames).
DROP TABLE IF EXISTS _pi_reverse;
CREATE TABLE _pi_reverse (plus VARCHAR(100) COLLATE utf8mb4_general_ci PRIMARY KEY, polaris VARCHAR(100) COLLATE utf8mb4_general_ci) ENGINE=InnoDB;
INSERT INTO _pi_reverse VALUES 
  ('area_hide', 'conf_area_hide'), ('background', 'ads_bg'), ('badge', 'default'),
  ('ball', 'football'), ('banzaipuck', 'battlebanzai_puck'), ('bb_blue_gate', 'battlebanzai_gate_blue'),
  ('bb_blue_score', 'battlebanzai_counter_blue'), ('bb_green_gate', 'battlebanzai_gate_green'), ('bb_green_score', 'battlebanzai_counter_green'),
  ('bb_patch', 'battlebanzai_tile'), ('bb_pyramid', 'pyramid'), ('bb_red_gate', 'battlebanzai_gate_red'),
  ('bb_red_score', 'battlebanzai_counter_red'), ('bb_teleport', 'battlebanzai_random_teleport'), ('bb_yellow_gate', 'battlebanzai_gate_yellow'),
  ('bb_yellow_score', 'battlebanzai_counter_yellow'), ('blue_goal', 'football_goal_blue'), ('blue_score', 'football_counter_blue'),
  ('bottle', 'dice'), ('camera_picture', 'external_image'), ('chooser_disable', 'conf_chooser_disable'),
  ('crackable_egg', 'crackable'), ('effect_box', 'fx_box'), ('exchange', 'default'),
  ('fbgate', 'football_gate'), ('floor', 'default'), ('freezebluecounter', 'freeze_counter_blue'),
  ('freezebluegate', 'freeze_gate_blue'), ('freezeexit', 'freeze_exit'), ('freezegreencounter', 'freeze_counter_green'),
  ('freezegreengate', 'freeze_gate_green'), ('freezeredcounter', 'freeze_counter_red'), ('freezeredgate', 'freeze_gate_red'),
  ('freezetile', 'freeze_tile'), ('freezetileblock', 'freeze_block'), ('freezetimer', 'game_timer'),
  ('freezeyellowcounter', 'freeze_counter_yellow'), ('freezeyellowgate', 'freeze_gate_yellow'), ('fx_provider', 'tile_fxprovider_nfs'),
  ('gld_gate', 'guild_gate'), ('gld_item', 'guild_furni'), ('green_goal', 'football_goal_green'),
  ('green_score', 'football_counter_green'), ('habbowheel', 'colorwheel'), ('handitem_block', 'conf_handitem_block'),
  ('horse_body_dye', 'default'), ('horse_hair_dye', 'default'), ('horse_hairstyle', 'default'),
  ('horse_saddle_1', 'default'), ('horse_saddle_2', 'default'), ('invis_control', 'conf_invis_control'),
  ('landscape', 'default'), ('lovelock', 'love_lock'), ('pet', 'default'),
  ('pet_breeding_box', 'breeding_nest'), ('pressure_pad', 'pressureplate'), ('purchasable_clothing', 'clothing'),
  ('puzzlebox', 'puzzle_box'), ('queue_speed', 'conf_queue_speed'), ('red_goal', 'football_goal_red'),
  ('red_score', 'football_counter_red'), ('rollerskate', 'rollerskate_field'), ('roombg', 'background_toner'),
  ('scoreboard', 'vote_counter'), ('stacktool', 'stack_helper'), ('television', 'youtube'),
  ('vip_gate', 'club_gate'), ('wallpaper', 'default'), ('wf_floor_switch1', 'switch'),
  ('wf_floor_switch2', 'switch'), ('wf_game_upcounter1', 'game_upcounter'), ('wf_game_upcounter2', 'game_upcounter'),
  ('wf_upcounter1', 'game_upcounter'), ('wf_upcounter2', 'game_upcounter'), ('wired_contract_payment', 'wf_contract_payment'),
  ('wired_contract_reward', 'wf_contract_reward'), ('wired_contract_trade', 'wf_contract_trade'), ('wired_disable', 'conf_wired_disable'),
  ('wired_label', 'wf_label'), ('wired_link', 'wf_link'), ('wired_room_linker', 'wf_room_linker'),
  ('yellow_goal', 'football_goal_yellow'), ('yellow_score', 'football_counter_yellow');

-- ---------------------------------------------------------------- furniture
-- One Plus definition per classname and type (the one with furnidata, else the lowest id).
DROP TABLE IF EXISTS _pi_furni;
CREATE TABLE _pi_furni (
  plus_id INT UNSIGNED PRIMARY KEY,
  name_key VARCHAR(100) COLLATE utf8mb4_general_ci NOT NULL,
  type CHAR(1) NOT NULL,
  canonical INT UNSIGNED NOT NULL,
  polaris_id INT UNSIGNED NULL,
  is_new TINYINT NOT NULL DEFAULT 0,
  interaction VARCHAR(100) COLLATE utf8mb4_general_ci NULL,
  how VARCHAR(20) NULL,
  KEY (name_key, type), KEY (canonical)
) ENGINE=InnoDB;
INSERT INTO _pi_furni (plus_id, name_key, type, canonical)
SELECT f.id, LOWER(TRIM(REGEXP_REPLACE(f.item_name, '[[:cntrl:]]', '') COLLATE utf8mb4_general_ci)), f.type,
       FIRST_VALUE(f.id) OVER (PARTITION BY LOWER(TRIM(REGEXP_REPLACE(f.item_name, '[[:cntrl:]]', '') COLLATE utf8mb4_general_ci)), f.type ORDER BY f.has_furnidata DESC, f.id)
FROM furniture f;

-- Polaris already has it: keep its id and its definition.
DROP TABLE IF EXISTS _pi_existing;
CREATE TABLE _pi_existing (name_key VARCHAR(100) COLLATE utf8mb4_general_ci, type VARCHAR(3), id INT UNSIGNED, PRIMARY KEY (name_key, type)) ENGINE=InnoDB
SELECT LOWER(TRIM(CONVERT(item_name USING utf8mb4) COLLATE utf8mb4_general_ci)) AS name_key, type, MIN(id) AS id FROM `@T@`.items_base GROUP BY 1, 2;
UPDATE _pi_furni p JOIN _pi_existing e ON e.name_key = p.name_key AND e.type = p.type SET p.polaris_id = e.id;

-- New: the next ids after the highest one Polaris has.
SET @base = (SELECT COALESCE(MAX(id), 0) FROM `@T@`.items_base);
DROP TABLE IF EXISTS _pi_new_ids;
CREATE TABLE _pi_new_ids (canonical INT UNSIGNED PRIMARY KEY, polaris_id INT UNSIGNED) ENGINE=InnoDB
SELECT canonical, @base + ROW_NUMBER() OVER (ORDER BY canonical) AS polaris_id
FROM (SELECT DISTINCT canonical FROM _pi_furni WHERE polaris_id IS NULL) c;
UPDATE _pi_furni p JOIN _pi_new_ids n ON n.canonical = p.canonical SET p.polaris_id = n.polaris_id, p.is_new = 1;

-- The interaction of a new definition, first rule that gives a type Polaris knows.
UPDATE _pi_furni p JOIN furniture f ON f.id = p.plus_id
LEFT JOIN _pi_by_name b ON b.item_name = p.name_key
LEFT JOIN _pi_known kb ON kb.name = b.interaction
LEFT JOIN _pi_known kn ON kn.name = p.name_key
  AND (f.interaction_type LIKE 'wired%' OR f.interaction_type LIKE 'wf\_%' OR p.name_key LIKE 'wf\_%' OR p.name_key LIKE 'conf\_%')
LEFT JOIN _pi_reverse r ON r.plus = LOWER(f.interaction_type COLLATE utf8mb4_general_ci)
LEFT JOIN _pi_known kr ON kr.name = r.polaris
LEFT JOIN _pi_known ki ON ki.name = LOWER(f.interaction_type COLLATE utf8mb4_general_ci)
SET p.interaction = COALESCE(kb.name, kn.name, kr.name, ki.name, 'default'),
    p.how = CASE WHEN kb.name IS NOT NULL THEN 'classname' WHEN kn.name IS NOT NULL THEN 'wired' WHEN kr.name IS NOT NULL THEN 'renamed'
                 WHEN ki.name IS NOT NULL THEN 'same' WHEN LOWER(f.interaction_type COLLATE utf8mb4_general_ci) = 'default' THEN 'same' ELSE 'fallback' END
WHERE p.is_new = 1 AND p.plus_id = p.canonical;

INSERT INTO `@T@`.items_base
  (id, sprite_id, public_name, item_name, type, width, length, stack_height, allow_stack, allow_sit, allow_lay, allow_walk,
   allow_gift, allow_trade, allow_recycle, allow_marketplace_sell, allow_inventory_stack, interaction_type, interaction_modes_count,
   vending_ids, multiheight, customparams, effect_id_male, effect_id_female, clothing_on_walk)
SELECT p.polaris_id, f.sprite_id,
       CONVERT(LEFT(IF(f.public_name = '', COALESCE(NULLIF(f.name, ''), f.item_name), f.public_name), 56) USING latin1),
       CONVERT(TRIM(REGEXP_REPLACE(f.item_name, '[[:cntrl:]]', '')) USING latin1), f.type, GREATEST(f.width, 1), GREATEST(f.length, 1), LEAST(f.stack_height, 99.99),
       f.can_stack, f.can_sit, IF(p.interaction = 'bed' OR f.can_lay_on, 1, 0), f.is_walkable,
       f.allow_gift, f.allow_trade, f.allow_recycle, f.allow_marketplace_sell, f.allow_inventory_stack,
       p.interaction, GREATEST(f.interaction_modes_count, 1),
       IF(f.vending_ids IN ('', '0'), '0', REPLACE(f.vending_ids, ';', ',')),
       IF(f.height_adjustable IN ('', '0'), '0', REPLACE(f.height_adjustable, ',', ';')),
       -- Clothing set ids live in catalog_clothing; other parameters stay with the furniture.
       CONVERT(LEFT(IF(p.interaction = 'clothing', '', COALESCE(f.custom_params, '')), 256) USING latin1),
       f.effect_id, f.effect_id, ''
FROM _pi_furni p JOIN furniture f ON f.id = p.plus_id
WHERE p.is_new = 1 AND p.plus_id = p.canonical;

-- Clothing: which clothing sets the furni unlocks. Polaris reads setid as numbers between commas and stops at
-- anything else, so "3373, 3374" becomes "3373,3374".
DROP TABLE IF EXISTS _pi_clothing_existing;
CREATE TABLE _pi_clothing_existing (name_key VARCHAR(100) COLLATE utf8mb4_general_ci PRIMARY KEY) ENGINE=InnoDB
SELECT DISTINCT LOWER(TRIM(CONVERT(name USING utf8mb4) COLLATE utf8mb4_general_ci)) AS name_key FROM `@T@`.catalog_clothing;
INSERT INTO `@T@`.catalog_clothing (name, setid)
SELECT name, setid FROM (
  SELECT CONVERT(TRIM(REGEXP_REPLACE(f.item_name, '[[:cntrl:]]', '')) USING latin1) AS name,
         TRIM(BOTH ',' FROM REGEXP_REPLACE(REGEXP_REPLACE(f.custom_params, '[^0-9,]', ''), ',+', ',')) AS setid
  FROM _pi_furni p JOIN furniture f ON f.id = p.plus_id
  LEFT JOIN _pi_clothing_existing e ON e.name_key = p.name_key
  WHERE p.plus_id = p.canonical AND f.interaction_type = 'purchasable_clothing' AND e.name_key IS NULL
) c
WHERE setid <> '' AND CHAR_LENGTH(setid) <= 75;
-- And rows written like that before (by an earlier import), which keep the emulator from starting.
UPDATE `@T@`.catalog_clothing SET setid = TRIM(BOTH ',' FROM REGEXP_REPLACE(REGEXP_REPLACE(setid, '[^0-9,]', ''), ',+', ','))
WHERE setid REGEXP '[^0-9,]' OR setid LIKE ',%' OR setid LIKE '%,' OR setid LIKE '%,,%';

-- ---------------------------------------------------------------- report
SELECT CONCAT('FURNI_EXISTING=', COUNT(DISTINCT canonical)) FROM _pi_furni WHERE is_new = 0;
SELECT CONCAT('FURNI_NEW=', COUNT(*)) FROM _pi_furni WHERE is_new = 1 AND plus_id = canonical;
SELECT CONCAT('FURNI_HOW=', how, ':', COUNT(*)) FROM _pi_furni WHERE is_new = 1 AND plus_id = canonical GROUP BY how;
SELECT CONCAT('FALLBACK=', f.interaction_type, ':', COUNT(*), ':', GROUP_CONCAT(f.item_name ORDER BY f.id SEPARATOR ',' LIMIT 4))
FROM _pi_furni p JOIN furniture f ON f.id = p.plus_id WHERE p.how = 'fallback' GROUP BY f.interaction_type ORDER BY COUNT(*) DESC;
SELECT CONCAT('CRACKABLE_WITHOUT_PRIZES=', COUNT(*)) FROM _pi_furni p
  WHERE p.is_new = 1 AND p.plus_id = p.canonical AND p.interaction LIKE 'crackable%'
  AND NOT EXISTS (SELECT 1 FROM `@T@`.items_crackable c WHERE c.item_id = p.polaris_id);

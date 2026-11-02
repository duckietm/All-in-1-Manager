-- Part of the All-in-1 Installer's "Import Plus database" tool (Remote/PlusImport.cs).
-- @S@ is the staging database with the PlusEMU dump, @T@ the hotel's Polaris database; the tool fills them in.
-- Lines starting with a KEY= value in the output are the tool's report.
-- Run after plus-to-polaris-furniture.sql, in the same staging database: it uses its _pi_furni map.
SET SESSION sql_mode = 'NO_ENGINE_SUBSTITUTION';
SET NAMES utf8mb4 COLLATE utf8mb4_general_ci;
SET SESSION group_concat_max_len = 65535;
USE `@S@`;

-- ---------------------------------------------------------------- catalog (replaces the normal catalog)
-- Pages: the normal catalog (Builders Club pages stay as Polaris has them), numbered from 1.
DROP TABLE IF EXISTS _pi_pages;
CREATE TABLE _pi_pages (plus_id INT PRIMARY KEY, polaris_id INT NOT NULL, layout VARCHAR(64) COLLATE utf8mb4_general_ci, KEY (polaris_id)) ENGINE=InnoDB
SELECT id AS plus_id, ROW_NUMBER() OVER (ORDER BY id) AS polaris_id,
       CASE layout WHEN 'frontpage4' THEN 'frontpage' WHEN 'club_gifts' THEN 'club_gift' WHEN 'guild_frontpage' THEN 'guilds'
                   WHEN 'guild_custom_furni' THEN 'guild_furni' WHEN 'default_3x3_extrainfo' THEN 'default_3x3' ELSE layout END AS layout
FROM catalog_pages WHERE catalog_mode = 'NORMAL';
-- A layout Polaris does not have becomes the plain 3x3 page.
UPDATE _pi_pages SET layout = 'default_3x3'
WHERE layout NOT IN (SELECT TRIM(BOTH '''' FROM j.v) FROM (
  SELECT SUBSTRING_INDEX(SUBSTRING_INDEX(SUBSTRING(column_type, 6, CHAR_LENGTH(column_type) - 6), ',', n.n), ',', -1) AS v
  FROM information_schema.columns
  JOIN (SELECT seq AS n FROM seq_1_to_100) n
  WHERE table_schema = '@T@' AND table_name = 'catalog_pages' AND column_name = 'page_layout'
    AND n.n <= 1 + CHAR_LENGTH(column_type) - CHAR_LENGTH(REPLACE(column_type, ',', ''))) j);

-- Text and image slots, as Polaris's layouts read them (the reverse of plus-migrator's slots()). Every page's slots
-- in one row first, so nothing is looked up per page.
DROP TABLE IF EXISTS _pi_slots;
CREATE TABLE _pi_slots (plus_id INT PRIMARY KEY, i0 TEXT, i1 TEXT, i2 TEXT, t0 MEDIUMTEXT, t1 MEDIUMTEXT, t2 MEDIUMTEXT, t3 MEDIUMTEXT) ENGINE=InnoDB
SELECT pp.plus_id, img.i0, img.i1, img.i2, txt.t0, txt.t1, txt.t2, txt.t3
FROM _pi_pages pp
LEFT JOIN (SELECT page_id, MAX(IF(slot = 0, image, NULL)) AS i0, MAX(IF(slot = 1, image, NULL)) AS i1, MAX(IF(slot = 2, image, NULL)) AS i2
           FROM catalog_page_images GROUP BY page_id) img ON img.page_id = pp.plus_id
LEFT JOIN (SELECT page_id, MAX(IF(slot = 0, text, NULL)) AS t0, MAX(IF(slot = 1, text, NULL)) AS t1, MAX(IF(slot = 2, text, NULL)) AS t2,
                  MAX(IF(slot = 3, text, NULL)) AS t3
           FROM catalog_page_texts GROUP BY page_id) txt ON txt.page_id = pp.plus_id;

DROP TABLE IF EXISTS _pi_page_text;
CREATE TABLE _pi_page_text (polaris_id INT PRIMARY KEY, headline TEXT, teaser TEXT, special TEXT, text1 MEDIUMTEXT, text2 MEDIUMTEXT, details MEDIUMTEXT, text_teaser MEDIUMTEXT) ENGINE=InnoDB
SELECT pp.polaris_id, s.i0 AS headline, s.i1 AS teaser, s.i2 AS special, s.t0 AS text1,
  CASE WHEN pp.layout IN ('frontpage', 'pets', 'pets2', 'pets3', 'productpage1', 'roomads', 'mad_money') THEN s.t1
       WHEN pp.layout = 'bots' THEN s.t2
       WHEN pp.layout = 'single_bundle' THEN s.t3 END AS text2,
  CASE WHEN pp.layout IN ('pets', 'pets2', 'pets3', 'productpage1') THEN s.t2
       WHEN pp.layout IN ('frontpage', 'roomads', 'mad_money', 'club_buy', 'club_gift', 'info_duckets', 'info_loyalty', 'info_pets') THEN NULL
       ELSE s.t1 END AS details,
  CASE WHEN pp.layout = 'frontpage' THEN s.t2
       WHEN pp.layout IN ('pets', 'pets2', 'pets3', 'productpage1') THEN s.t3
       WHEN pp.layout IN ('bots', 'roomads', 'mad_money', 'soundmachine', 'club_buy', 'club_gift', 'info_duckets', 'info_loyalty') THEN NULL
       ELSE s.t2 END AS text_teaser
FROM _pi_pages pp JOIN _pi_slots s ON s.plus_id = pp.plus_id;

-- Polaris-only: pages with Habbicons (and the pages above them) stay, after the Plus pages.
DROP TABLE IF EXISTS _pi_keep_pages;
CREATE TABLE _pi_keep_pages LIKE `@T@`.catalog_pages;
ALTER TABLE _pi_keep_pages ENGINE=InnoDB;
INSERT INTO _pi_keep_pages SELECT * FROM `@T@`.catalog_pages WHERE id IN (SELECT page_id FROM `@T@`.catalog_items WHERE habbicon_id > 0);
INSERT IGNORE INTO _pi_keep_pages SELECT p.* FROM `@T@`.catalog_pages p WHERE p.id IN (SELECT parent_id FROM _pi_keep_pages);
INSERT IGNORE INTO _pi_keep_pages SELECT p.* FROM `@T@`.catalog_pages p WHERE p.id IN (SELECT parent_id FROM _pi_keep_pages);
INSERT IGNORE INTO _pi_keep_pages SELECT p.* FROM `@T@`.catalog_pages p WHERE p.id IN (SELECT parent_id FROM _pi_keep_pages);
DROP TABLE IF EXISTS _pi_keep_items;
CREATE TABLE _pi_keep_items LIKE `@T@`.catalog_items;
INSERT INTO _pi_keep_items SELECT * FROM `@T@`.catalog_items WHERE page_id IN (SELECT id FROM _pi_keep_pages);

SET @staff_rank = (SELECT COALESCE(MAX(id), 7) FROM `@T@`.permission_ranks);
DELETE FROM `@T@`.catalog_pages;
INSERT INTO `@T@`.catalog_pages
  (id, parent_id, caption_save, caption, page_layout, icon_color, icon_image, min_rank, order_num, visible, enabled, club_only, vip_only,
   page_headline, page_teaser, page_special, page_text1, page_text2, page_text_details, page_text_teaser, room_id, includes, catalog_mode)
SELECT pp.polaris_id, COALESCE(parent.polaris_id, -1), LEFT(COALESCE(c.link, ''), 128), LEFT(c.caption, 128), pp.layout, 1, c.icon,
       IF(c.required_permission IS NULL, 1, @staff_rank), c.position, IF(c.visible, '1', '0'), IF(c.enabled, '1', '0'),
       IF(c.required_club_level > 0, '1', '0'), IF(c.required_club_level > 1, '1', '0'),
       LEFT(COALESCE(t.headline, ''), 1024), LEFT(COALESCE(t.teaser, ''), 64), LEFT(COALESCE(t.special, ''), 2048),
       t.text1, t.text2, t.details, t.text_teaser, 0, '', 'NORMAL'
FROM _pi_pages pp JOIN catalog_pages c ON c.id = pp.plus_id
LEFT JOIN _pi_pages parent ON parent.plus_id = c.parent_id
JOIN _pi_page_text t ON t.polaris_id = pp.polaris_id;

-- Products of an offer as Polaris items: furniture, effects, badges, bots and pets are all items there.
-- The hotel's item names in an indexed table: looking them up in items_base itself would scan it for every product.
DROP TABLE IF EXISTS _pi_target;
CREATE TABLE _pi_target (name_key VARCHAR(100) COLLATE utf8mb4_general_ci, type VARCHAR(3), id INT UNSIGNED, PRIMARY KEY (name_key, type)) ENGINE=InnoDB
SELECT LOWER(TRIM(CONVERT(item_name USING utf8mb4) COLLATE utf8mb4_general_ci)) AS name_key, type, MIN(id) AS id FROM `@T@`.items_base GROUP BY 1, 2;

-- The Polaris item each non-furniture product needs: (kind, key) -> item name and type.
DROP TABLE IF EXISTS _pi_special;
CREATE TABLE _pi_special (kind VARCHAR(10), product_key VARCHAR(100) COLLATE utf8mb4_general_ci, name VARCHAR(100) COLLATE utf8mb4_general_ci,
  type VARCHAR(3), effect INT, item_id INT UNSIGNED NULL, PRIMARY KEY (kind, product_key)) ENGINE=InnoDB
SELECT DISTINCT 'effect' AS kind, CAST(effect_id AS CHAR) COLLATE utf8mb4_general_ci AS product_key,
       CONCAT('avatar_effect', effect_id) COLLATE utf8mb4_general_ci AS name, 'e' AS type, effect_id AS effect
FROM catalog_offer_products WHERE product_type = 'effect'
UNION SELECT DISTINCT 'badge', badge_code COLLATE utf8mb4_general_ci, badge_code COLLATE utf8mb4_general_ci, 'b', 0
FROM catalog_offer_products WHERE product_type = 'badge' AND COALESCE(badge_code, '') <> ''
UNION SELECT DISTINCT 'pet', CAST(pet_type AS CHAR) COLLATE utf8mb4_general_ci, CONCAT('a0 pet', pet_type) COLLATE utf8mb4_general_ci, 's', 0
FROM catalog_offer_products WHERE product_type = 'pet'
UNION SELECT DISTINCT 'bot', CAST(bp.id AS CHAR) COLLATE utf8mb4_general_ci, IF(bp.ai_type = 'bartender', 'bot_bartender', 'bot_generic'), 'r', 0
FROM catalog_offer_products op JOIN catalog_bot_presets bp ON bp.id = op.bot_preset_id WHERE op.product_type = 'bot';
UPDATE _pi_special s JOIN _pi_target t ON t.name_key = LOWER(s.name) AND t.type = s.type SET s.item_id = t.id;

-- Effects, badges and pets Polaris does not have yet get an item first (bots need Polaris' own bot items).
-- Polaris gives the effect of effect_id_male / effect_id_female; pet items carry their own id as sprite, like the ones Polaris ships.
SET @base = (SELECT COALESCE(MAX(id), 0) FROM `@T@`.items_base);
DROP TABLE IF EXISTS _pi_special_new;
CREATE TABLE _pi_special_new (name VARCHAR(100) COLLATE utf8mb4_general_ci, type VARCHAR(3), effect INT, id INT UNSIGNED, PRIMARY KEY (name, type)) ENGINE=InnoDB
SELECT name, type, MAX(effect) AS effect, @base + ROW_NUMBER() OVER (ORDER BY type, name) AS id
FROM _pi_special WHERE item_id IS NULL AND kind <> 'bot' GROUP BY name, type;
INSERT INTO `@T@`.items_base (id, sprite_id, public_name, item_name, type, interaction_type, effect_id_male, effect_id_female)
SELECT id, IF(name LIKE 'a0 pet%', id, 0), CONVERT(LEFT(name, 56) USING latin1), CONVERT(name USING latin1), type, 'default', effect, effect FROM _pi_special_new;
UPDATE _pi_special s JOIN _pi_special_new n ON n.name = s.name AND n.type = s.type SET s.item_id = n.id WHERE s.item_id IS NULL;
-- Pet items that came in as Plus furniture too.
UPDATE `@T@`.items_base SET sprite_id = id WHERE item_name LIKE 'a0 pet%' AND sprite_id = 0;

DROP TABLE IF EXISTS _pi_products;
CREATE TABLE _pi_products (offer_id INT, position INT, item_id INT UNSIGNED NULL, amount INT, param VARCHAR(1024), kind VARCHAR(20), PRIMARY KEY (offer_id, position)) ENGINE=InnoDB
SELECT op.offer_id, op.position, op.amount, op.product_type AS kind,
  CASE op.product_type WHEN 'furni' THEN fm.polaris_id ELSE sp.item_id END AS item_id,
  CASE op.product_type
    -- For an effect Polaris reads extradata as its duration in seconds (empty: one day), as Plus does.
    WHEN 'bot' THEN CONCAT('name:', bp.name, ';motto:', bp.motto, ';figure:', bp.figure, ';gender:', bp.gender)
    -- Wallpaper, floor and landscape patterns come from the catalog name in Polaris.
    WHEN 'furni' THEN IF(f.interaction_type IN ('wallpaper', 'floor', 'landscape'), '', op.extra_param)
    ELSE op.extra_param
  END AS param
FROM catalog_offer_products op
LEFT JOIN _pi_furni fm ON op.product_type = 'furni' AND fm.plus_id = op.furniture_id
LEFT JOIN furniture f ON op.product_type = 'furni' AND f.id = op.furniture_id
LEFT JOIN catalog_bot_presets bp ON op.product_type = 'bot' AND bp.id = op.bot_preset_id
LEFT JOIN _pi_special sp ON sp.kind = op.product_type COLLATE utf8mb4_general_ci AND sp.product_key = CASE op.product_type
    WHEN 'effect' THEN CAST(op.effect_id AS CHAR) WHEN 'badge' THEN op.badge_code WHEN 'pet' THEN CAST(op.pet_type AS CHAR)
    WHEN 'bot' THEN CAST(op.bot_preset_id AS CHAR) END COLLATE utf8mb4_general_ci;

-- Per offer once: its items as Polaris writes them ("id", or "id;id:amount" for a bundle), amount and extradata.
DROP TABLE IF EXISTS _pi_offer_items;
CREATE TABLE _pi_offer_items (offer_id INT PRIMARY KEY, item_ids VARCHAR(1000), amount INT, param VARCHAR(1024), products INT) ENGINE=InnoDB
SELECT x.offer_id,
  GROUP_CONCAT(IF(x.amount > 1 AND n.products > 1, CONCAT(x.item_id, ':', x.amount), x.item_id) ORDER BY x.position SEPARATOR ';') AS item_ids,
  IF(n.products = 1, MAX(x.amount), 1) AS amount,
  SUBSTRING_INDEX(GROUP_CONCAT(NULLIF(x.param, '') ORDER BY x.position SEPARATOR '|~|'), '|~|', 1) AS param,
  n.products
FROM _pi_products x
JOIN (SELECT offer_id, COUNT(*) AS products FROM _pi_products WHERE item_id IS NOT NULL GROUP BY offer_id) n ON n.offer_id = x.offer_id
WHERE x.item_id IS NOT NULL
GROUP BY x.offer_id, n.products;

-- One Polaris catalog item per offer and page it is on (an offer on four pages is four items), numbered from 1.
DROP TABLE IF EXISTS _pi_offers;
CREATE TABLE _pi_offers (catalog_id INT PRIMARY KEY, offer_id INT, page_id INT, position INT, item_ids VARCHAR(1000), amount INT, param VARCHAR(1024),
  KEY (offer_id, page_id)) ENGINE=InnoDB
SELECT ROW_NUMBER() OVER (ORDER BY po.page_id, po.position, po.offer_id) AS catalog_id, po.offer_id, pp.polaris_id AS page_id, po.position,
       oi.item_ids, oi.amount, oi.param
FROM catalog_page_offers po
JOIN _pi_pages pp ON pp.plus_id = po.page_id
JOIN _pi_offer_items oi ON oi.offer_id = po.offer_id;

DELETE FROM `@T@`.catalog_items;
INSERT INTO `@T@`.catalog_items
  (id, item_ids, page_id, catalog_name, cost_credits, cost_points, points_type, amount, limited_stack, limited_sells, order_number,
   offer_id, song_id, extradata, have_offer, club_only)
SELECT o.catalog_id, o.item_ids, o.page_id, LEFT(c.localization_key, 100), c.cost_credits, c.cost_points, c.points_type, GREATEST(COALESCE(o.amount, 1), 1),
       0, 0, o.position, -1, 0, LEFT(COALESCE(o.param, ''), 500), IF(c.bulk_purchase, '1', '0'), IF(c.club_level > 0, '1', '0')
FROM _pi_offers o JOIN catalog_offers c ON c.id = o.offer_id
WHERE c.enabled = 1;

-- The kept Polaris pages and their items, numbered after the Plus ones.
SET @page_base = (SELECT COALESCE(MAX(id), 0) FROM `@T@`.catalog_pages);
SET @item_base = (SELECT COALESCE(MAX(id), 0) FROM `@T@`.catalog_items);
DROP TABLE IF EXISTS _pi_keep_map;
CREATE TABLE _pi_keep_map (old_id INT PRIMARY KEY, new_id INT) ENGINE=InnoDB
SELECT id AS old_id, @page_base + ROW_NUMBER() OVER (ORDER BY id) AS new_id FROM _pi_keep_pages;
UPDATE _pi_keep_items i JOIN _pi_keep_map m ON m.old_id = i.page_id SET i.page_id = m.new_id;
UPDATE _pi_keep_pages p LEFT JOIN _pi_keep_map m ON m.old_id = p.parent_id SET p.parent_id = COALESCE(m.new_id, -1);
UPDATE _pi_keep_pages p JOIN _pi_keep_map m ON m.old_id = p.id SET p.id = m.new_id;
INSERT INTO `@T@`.catalog_pages SELECT * FROM _pi_keep_pages;
DROP TABLE IF EXISTS _pi_keep_item_ids;
CREATE TABLE _pi_keep_item_ids (old_id INT PRIMARY KEY, new_id INT) ENGINE=InnoDB
SELECT id AS old_id, @item_base + ROW_NUMBER() OVER (ORDER BY id) AS new_id FROM _pi_keep_items;
UPDATE _pi_keep_items i JOIN _pi_keep_item_ids m ON m.old_id = i.id SET i.id = m.new_id;
INSERT INTO `@T@`.catalog_items SELECT * FROM _pi_keep_items;

-- The catalog editor numbers new pages and offers after these, and clients reload the catalog.
UPDATE `@T@`.catalog_id_sequences SET next_id = (SELECT COALESCE(MAX(id), 0) + 1 FROM `@T@`.catalog_pages) WHERE entity_type = 'PAGE' AND catalog_type = 'NORMAL';
UPDATE `@T@`.catalog_id_sequences SET next_id = (SELECT COALESCE(MAX(id), 0) + 1 FROM `@T@`.catalog_items) WHERE entity_type = 'OFFER' AND catalog_type = 'NORMAL';
UPDATE `@T@`.catalog_manager_state SET revision = revision + 1;

-- ---------------------------------------------------------------- report
SELECT CONCAT('PAGES=', COUNT(*)) FROM `@T@`.catalog_pages;
SELECT CONCAT('CATALOG_ITEMS=', COUNT(*)) FROM `@T@`.catalog_items;
SELECT CONCAT('BC_PAGES_KEPT=', (SELECT COUNT(*) FROM catalog_pages WHERE catalog_mode <> 'NORMAL'));
SELECT CONCAT('PRODUCTS_SKIPPED=', kind, ':', COUNT(*)) FROM _pi_products WHERE item_id IS NULL GROUP BY kind;
SELECT CONCAT('OFFERS_WITHOUT_ITEMS=', COUNT(*)) FROM catalog_page_offers po JOIN _pi_pages pp ON pp.plus_id = po.page_id
  WHERE NOT EXISTS (SELECT 1 FROM _pi_offers o WHERE o.offer_id = po.offer_id AND o.page_id = pp.polaris_id);
SELECT CONCAT('POLARIS_PAGES_KEPT=', COUNT(*), ':', COALESCE(GROUP_CONCAT(caption SEPARATOR ','), '')) FROM _pi_keep_pages;

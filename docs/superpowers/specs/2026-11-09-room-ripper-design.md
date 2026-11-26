# Room ripper: design

Date: 2026-11-09. Status: draft, to be approved by the owner (SimoLeoLife).

## Intent

The owner runs a Habbo retro (Polaris emulator, Nitro client) and manages it with the All-in-1 Manager. They want to
copy rooms from official Habbo and from other retro hotels into their own hotel: the floor plan, the decoration, every
furni in its place, and the wired, so that a room built elsewhere works in their hotel without rebuilding it by hand.
It is the companion of the Catalogue ripper (copy the catalogue → copy the rooms) and the second step of "a complete
ripper from Habbo and the retros".

The owner's own words: "copy a room from Habbo or a retro and put it in my hotel", "find a way to also read the wired
settings".

## Goals

- Capture one or more rooms while the user walks through them with their own client, through G-Earth.
- Capture the floor plan (heightmap, door, wall height), the decoration (wallpaper, floor, landscape), the room's name
  and description, every floor and wall furni with position, rotation and state (extra data).
- Capture the **wired configuration** wherever the hotel lets the user read it (see "Wired").
- Import a captured room into the owner's hotel as a **new** room, with a preview first and in one transaction,
  recorded so it can be undone. Furni are matched to the hotel's `items_base` by class name.
- Work for official Habbo and for retros (Flash and Nitro clients), with the same hotel profiles as the Catalogue
  ripper.

## Non-goals

- Overwriting or merging into an existing room.
- Copying bots, pets, room users, chat, room rights, groups, or the room's event/promotion.
- Creating missing furni automatically (they are listed; the user adds them with the Catalogue ripper's import or
  Import Assets first).
- A live mirror that keeps the room in sync.

## Anti-goals (a failure even if it "works")

- **Never change anything on the source hotel.** The ripper never sends a save packet (wired save, furni move, room
  settings). The only packets it may send are "open this wired box" (see Wired) and nothing else.
- **No exploits.** Wired configuration is read only where the server sends it to this user (rights, inspection
  setting, staff permission). Anything that relies on a missing permission check or a server bug is out of scope.
- **Never write to the owner's database without a preview and an explicit Apply.**
- Never a half-imported room: one transaction, all or nothing.

## Constraints and facts (verified 2026-11-09)

- **Where the wired configuration lives.** On Arcturus Morningstar (and Polaris, its descendant) the configuration is in
  `items.wired_data` on the server. The server sends it to a client (WiredTriggerData / WiredEffectData /
  WiredConditionData) only from the box's `onClick`, and only when `room.hasRights(habbo)`: owner, room rights, group
  rights, or `acc_anyroomowner` (`InteractionWiredTrigger/Effect/Condition.java`, `Room.java:4318`). Furni extra data of
  a wired box is only "0"/"1".
- **Official Habbo (since May 2024)** has "Wired Creator Tools" with a room setting "who can inspect wired" (everyone /
  rights / group). Whether "inspect" makes the server send the box editor's parameters is not settled (third-party
  sources disagree); it is checked live with G-Earth during the build. PlusEMU's modern fork sends them to anyone the
  inspect setting allows.
- **Opening a box is not side-effect free** on Arcturus: it toggles the box's state for everyone in the room (unless
  `:hidewired`) and queues a database save of the (unchanged) box.
- **Wire layout (Nitro parsers, `roomevents/`)**: shared base = bool stuffTypeSelectionEnabled, int furniLimit,
  int[] stuffIds (selected furni), int stuffTypeId, int id, string stringParam, int[] intParams; trigger adds
  triggerConf + int[] conflictingActions; action adds type + delayInPulses + int[] conflictingTriggers; condition adds
  type. Nitro headers: trigger 383, action 1434, condition 1108, open 1830 (in) / 768 (out); a double click sends
  FURNITURE_MULTISTATE 99.
- **The existing Python tools** (`BETA/scripts/habbo-scraper/cattura-stanza.py`, `genera-sql-stanza.py`) already capture
  rooms passively (RoomReady, FloorHeightmap, ActiveObjects/Objects, Items, FlatProperty, GetGuestRoomResult) and write
  Polaris SQL for `rooms`, `items` and a custom `room_models` row. They are the reference for which packets and columns
  matter (`COLONNE_STANZA`, `COLONNE_ITEMS`, `COLONNE_MODELLI`), not code to call.
- The Manager is a Rust egui app; the Catalogue ripper is the crate `aio-catalog-rip` (G-Earth extension protocol,
  wire format, hotel profiles with header ids, furnidata matching by class name). Plugins are registered in
  `source/crates/app/src/plugins/mod.rs`.

## How it works (decided)

### Capture

- A **passive** G-Earth extension: it listens to the room packets as the user enters rooms and walks around, and
  sends nothing for the room itself. Each room the user enters becomes an entry in a list on the page ("Villa Rosa —
  142 furni, 18 wired (12 read)"), saved as a capture file so it survives a restart.
- The user can capture several rooms in one session, then import the ones they want.

### Wired

1. **Passive, always on:** every wired data packet the client receives (because the user opened a box by hand) is
   stored with its box.
2. **"Read the wired" button** (per captured room, while the user is still in it): opens every wired box of the room
   one at a time by sending the "use/open furni" packet, with a pause between boxes (default 400 ms), and collects the
   answers. If the first box gets no answer within a few seconds, it stops and says the user has no permission to read
   wired in this room. The page warns before starting that the boxes blink for everyone in the room while they are read,
   and suggests `:hidewired` in the user's own rooms. **Decided by the owner:** the blinking is acceptable.
3. **Without permission:** wired boxes are imported empty and listed in the report, with the advice to ask the room's
   owner for rights or for "inspect: everyone" for a moment.

### Import

- Target: the owner's hotel database (Polaris), through the Manager's existing SSH + MariaDB session.
- Creates a **new** room: the user picks the owner (a user of the hotel) and may rename it. If the floor plan is not one
  of the hotel's existing models, a custom `room_models` row is made (as `genera-sql-stanza.py` does).
- Furni are matched to `items_base` by class name (furnidata), as the Catalogue ripper does. Unmatched furni are
  listed; the user can import without them.
- Wired: the selected furni ids inside each box are remapped to the new item ids; the box parameters are converted to
  Polaris' `wired_data` per box type. Box types the Manager does not know how to convert yet are imported empty and
  listed.
- Preview first (rooms/items/models rows that would be created, what is left out and why), then one transaction on
  Apply, recorded in the Manager's history so it can be undone.

### Placement in the app

- A new plugin **"Room ripper"** (id `room-rip`) with its own page, switchable on the Plugins page like the others.
- Built on `aio-catalog-rip`'s G-Earth, wire and profile code (shared, not copied).

## Left to the builder

- Exact crate/module layout (a new crate vs. a module of `aio-catalog-rip`; renaming the shared parts).
- Header ids per profile for the room and wired packets (official: G-Earth packet names; Arcturus/Nitro: verified
  from source), and how profiles gain them without breaking existing profile JSON files.
- Capture file format.
- The per-box-type conversion table to Polaris `wired_data` (which box types first), and how Polaris stores
  `wired_data` (check the emulator source in `DEV/emulatore`).
- Detecting "no permission" (timeout vs. a permissions packet where the hotel sends one).
- Tests: a fake G-Earth end-to-end like the Catalogue ripper's, and import tests against the SQL the import produces.
- Translations of the new texts in every language, README section.

## Open questions (for the owner, from the builder check)

1. **Undo of an import.** Neither existing history can undo a room: `aio_core::history` is a log without undo, and
   the catalogue journal only knows catalogue pages and offers. Options: a new table in the hotel database (an emulator
   migration), extending the shared catalogue journal, or a record kept by the Manager. And what undo means once the
   room was used: refuse when anything changed, delete whatever still has the recorded ids, or also take back furni
   picked up into inventories?
2. **Emulator running or stopped.** Polaris may cache room models and loaded rooms; writing rooms with SQL while the
   room is loaded can leave ghost items or lose the undo. Either "stop the emulator / the room must not be loaded" is a
   precondition with a warning, or import needs an emulator hook (RCON) to load/unload the room.
3. **Valuable furni.** Imported furni can be picked up and traded by the chosen owner: copying a room of rares mints
   rares; LTD serials, group furni, ads pointing at the source hotel and engraved trophies come along with extra data.
   Normal tradeable items, or strip/flag/make non-tradeable?
4. **Room settings.** Copy only layout and decoration and reset access (open, default category), or a faithful copy
   (doorbell/password/invisible, max users, trade mode, chat settings, tags)? Category ids differ between hotels.

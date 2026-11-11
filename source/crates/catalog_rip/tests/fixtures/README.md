# Real catalogue packets

Every folder here is a real rip's `packets/` folder, kept as a regression test: `tests/real_fixtures.rs`
reads each one and checks that every recorded index and page still parses, that the catalogue
assembles, and that `catalog_manager.sql` is accepted by the Manager's own import parser.

To add one after a live rip:

1. Rip the hotel (official or retro) with packet recording on (the default).
2. Copy `<output>/<hotel>_<timestamp>/packets/` here as `tests/fixtures/<hotel>-<client>-<date>/packets/`,
   e.g. `tests/fixtures/habbo.nl-flash-2026-11/packets/`.
3. Run `cargo test -p aio-catalog-rip --test real_fixtures`.

A recording holds only what the hotel sent to the logged-in client (catalogue index and pages):
no account data. Check `session.json` before committing anyway.

If a parser change breaks a recorded page, the test names the folder and the page; a page that
never parsed is listed under "unreadable" in the recording and is skipped (it is what the
`raw page saved` log line kept for working out a new layout).

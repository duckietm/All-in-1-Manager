# Translations

A plugin can show its page in the Manager's language. The English text is the key: you write every text of the page
in English, and a language file maps each English text to its translation. A plugin with no language files, or a text
a file lacks, shows English; the user never sees a blank or a key.

Translations need `"api": "1.1"` in the manifest. They need no permission.

## In the code

```rust
use aio_plugin_sdk::i18n::{tr, tr_with};
use aio_plugin_sdk::Ui;

// The body of `Plugin::render`, with the number of notes passed in.
fn page(ui: &mut Ui, count: usize) {
    ui.heading(tr("Notes"));
    ui.text(tr_with("{n} notes saved", &[("n", &count.to_string())]));
    ui.primary_button("add", tr("Add a note"));
}
```

- `tr(english)` returns the translation in the Manager's current language, or `english` itself.
- `tr_with(english, &[("name", "value")])` translates, then fills the `{name}` placeholders. Values are filled in one
  pass (a value that contains `{x}` is not filled again) and a placeholder with no value stays as written.
- `host::language()` returns the current code (`"en"` for English) if you ever need it. Most plugins do not.

Call `tr` in `render` every time. When the user switches the Manager's language, every open plugin page is rendered
again (the plugin gets no event, and a running long job is not disturbed), so keep English texts, not translated ones,
in your state. A text built outside `render` (a status line, an error returned from `on_event`) is translated where it
is made, in the language of that moment.

Off `wasm32` (a native `cargo test`) `tr` returns the English text, so a plugin's page tests keep working unchanged.

## The language files

Put one file per language in the `i18n` folder next to `manifest.json`: `i18n/it.json`, `i18n/de.json`, ...

```json
{
  "Add a note": "Aggiungi una nota",
  "{n} notes saved": "{n} note salvate"
}
```

- The file is a JSON object whose keys and values are all strings. The key is the English text exactly as passed to
  `tr`.
- The language is the file name, one of the Manager's codes: `it` (Italian), `fr`, `de`, `nl`, `es`, `pt`, `tr`
  (Turkish) and `fi`. English is the key itself: an `en.json` is accepted and ignored.
- Any other code is refused when the package is installed ("Unknown language in the package: xx"). In a developer
  folder it is ignored with a warning line in the plugin log.
- A translation must keep every `{name}` placeholder of its key, or the file is refused (`i18n/it.json: "<key>" lost
  {name}`). It may move them: `"{n} notes saved"` can become `"Salvate {n} note"`.
- An empty translation counts as no translation (English is shown).
- A language you do not translate falls back to English, text by text.

## What the Manager translates by itself

The manifest texts are translated by the Manager, keyed by their English text, so add them to the files and do not
call `tr` for them:

- `name` and `description` (the Plugins page and the plugin's card),
- `page_title` (the sidebar entry and the page's title),
- the tile's `title` and `description`.

The Available tab of the index shows English only: index entries carry no translations.

## What stays in English

The Manager keeps its security texts in English, with the plugin's English name from the manifest (the name the user
consented to): the permission lists, the approval card, the "The plugin <name> asks" title of the host's dialogs, the
"Drawn by the plugin <name>" line and the consent windows (installing, updating, loading a developer folder,
`http:ask`, scripts, database writes). A translation never changes what a permission prompt says. The texts of dialogs
your plugin opens (`ask`, `prompt_text`) are shown as you pass them, so pass them through `tr` yourself.

All texts from language files are shown with the same treatment as every other plugin text: control, direction
changing and invisible characters are replaced.

## Limits

| What | Limit |
|---|---|
| Entries in one language file | 5,000 |
| One key or one value | 4 KiB |
| One language file | 512 KiB |
| Language files in a developer folder | 64 (the others are ignored, with a warning) |

A package holds at most 64 files in all (see [Limits](limits.md#packages-and-manifests)). A file over a limit, not a
JSON object, nested too deeply, or with a non-string value is refused at install.

## Trying it in developer mode

1. Load the plugin folder in [Developer mode](getting-started.md#developer-mode-run-from-the-folder) and open its page.
2. Switch the Manager's language: the page is drawn again in the new language.
3. Edit `i18n/it.json`, then click **Reload** on the plugin's card: the folder's language files are read again, so the
   new text shows at once (no build is needed when only a language file changed).

Files that are ignored (an unknown code, a file that does not parse) are listed as warning lines in the plugin log.

## Checking

`aio-plugin check <folder or .aiop>` refuses a package whose language files the Manager would refuse, and prints one
line per language with its number of texts:

```text
languages:
  - de: 12 texts
  - it: 14 texts
```

A package with no language files prints `languages:   none (English only)`. The check does not tell you which `tr`
texts a file lacks; a native test that reads `src/lib.rs` and the file can (the Catalogue ripper's plugin crate has such a test).

`aio-plugin new` writes a starting `i18n/it.json` with two entries for the texts of its page.

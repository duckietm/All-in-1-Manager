# {{name}}

An [All-in-1 Manager](https://github.com/duckietm/All-in-1-Manager) plugin (id `{{id}}`), written in Rust on
`aio-plugin-sdk` and built as a WebAssembly component.

## Build and try it

```sh
rustup target add wasm32-wasip2   # once
aio-plugin build                  # cargo build --release --target wasm32-wasip2
aio-plugin dev                    # how to load this folder in the Manager's developer mode
```

## Package it

```sh
aio-plugin pack                   # builds, then writes dist/{{id}}.aiop
aio-plugin check dist/{{id}}.aiop # what the Manager will show: permissions, size, sha256
```

`i18n/it.json` is an example translation: the plugin's texts go through `tr("English text")` and each language file
maps the English text to its translation (`aio-plugin check` lists the languages and refuses a broken file). The
Manager's codes are `it fr de nl es pt tr fi`; see `docs/plugins/translations.md`.

`manifest.json` describes the plugin: change `name`, `author`, `description` and `version` before publishing, and
list in `permissions` only what the plugin needs. See the plugin guide in the All-in-1 Manager repository
(`docs/plugins/`).

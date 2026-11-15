# Publishing

There are two ways to get a plugin to users:

- **A `.aiop` file** you hand out yourself. Users install it with **Install from file …** on the Plugins page. It
  always shows a red **Not verified** badge, and the user decides whether to trust you.
- **The official plugin index**: a reviewed list that the Manager reads from the GitHub repository
  `all-in-1-plugins/index`. The index's CI builds your plugin from your source, a maintainer reviews it, and the
  signed index offers it on the **Available** tab with a green **Verified** badge. This page is about that path.

> **The index is not live yet.** It works only after its owner has created the organization and repository, made the
> signing key, and shipped a Manager release with the public key (`OFFICIAL_INDEX_KEYS` in
> `source/crates/app/src/plugins/index.rs`). Until then the Available tab says "The official plugin index is not set
> up yet", and plugins can only be installed from a file. The rules below are those of the index repository's
> README; check it for the current version before you submit.

## Before you submit

Your plugin's source must be in a **public GitHub repository**: a Rust crate built for `wasm32-wasip2`, with its
`manifest.json` next to its `Cargo.toml`. A project made by `aio-plugin new` has that layout.

The index's CI builds the plugin **sealed**: it downloads what `Cargo.lock` names first (`cargo fetch --locked`), then
builds `--locked --offline` in a sandbox with no network and a read-only filesystem except the build folder and
`/tmp`. So:

- **Commit `Cargo.lock`**, next to the workspace's `Cargo.toml` (for a project from `aio-plugin new`, next to its own
  `Cargo.toml`). A missing or stale lock file fails the build. The lock file also pins the SDK's git dependency to
  one commit.
- **Nothing may download during the build**: no `build.rs` that fetches files, no tool that needs the network.
- **No symlinks and no submodules** anywhere in the repository, not only in the plugin's folder.
- **No Git LFS** and no other filter: every file must check out exactly as committed. A `.gitattributes` end-of-line
  or encoding rule that changes a file is refused. In particular, `manifest.json` must check out byte-identical to the
  committed file, because the publish job compares the package's manifest with it.
- **Your build settings are not used**: CI sets the toolchain (the Rust version in the index repository's
  `tools/RUST_TOOLCHAIN`) and the rustflags. `rustflags` from your `.cargo/config.toml` and your
  `rust-toolchain.toml` are ignored.
- The build may write only inside its own folder (the crate's `target/`, Cargo's home) and `/tmp`.

Check locally that the plugin builds the way CI does:

```sh
aio-plugin pack --locked --offline
aio-plugin check dist/<id>.aiop
```

Then set the final `name`, `author`, `description` and `version` in `manifest.json`, commit, and push. Note the full
commit hash (`git rev-parse HEAD`).

## The pull request

Open a pull request on the index repository that adds **exactly one file**, `plugins/<id>/plugin.json`, where `<id>`
is the `id` in your `manifest.json`:

```json
{
  "repo": "https://github.com/<owner>/<name>",
  "commit": "<the full 40-character commit hash>",
  "path": "<folder of the crate inside the repository, for example . or plugins/mine>",
  "sha256": "<the package hash the check prints>"
}
```

- `repo` must be on github.com.
- `commit` is the full lowercase 40-hex hash, not a branch or a tag. It must be on a branch or tag of `repo`: a commit
  that only exists in a fork is refused.
- `path` is the folder that holds `Cargo.toml` and `manifest.json`.
- One plugin per pull request: the check refuses a pull request that changes more than one `plugins/<id>/plugin.json`,
  or anything else under `plugins/`.

In the pull request description, **justify every permission**: what the plugin does with it. Ask for the least you
need; wide permissions (root over SSH, private database columns) get the closest look.

### The sha256 pin

The `sha256` is the SHA-256 of the package CI builds from your commit. You cannot know it in advance: CI builds in a
fixed path with its own flags and toolchain, so the hash `aio-plugin check` prints on your machine usually differs.

1. Open the pull request with `sha256` empty or missing.
2. CI builds your plugin (and only yours) from that commit and posts the manifest's permissions, the package size
   and its SHA-256 as a comment on the pull request (on a pull request from a fork, in the job summary only). Check
   that they are what you meant.
3. The check fails while `sha256` is missing or different, and prints the value to put there. Put it in
   `plugin.json` yourself, or leave it to the maintainer.
4. A maintainer reviews the source **at that commit**, makes sure `sha256` is the hash the check printed (the check
   re-runs and must pass), and merges.

That hash is the pin. After the merge, the publish workflow rebuilds the plugin and signs it into the index only if
the package's SHA-256 is exactly the pinned one. The package is then a release asset named `<id>-<version>`, and the
Manager's Available tab lists it. An install checks the size and SHA-256 against the signed index, and that the
package's manifest (id, version, author, API version, permissions) equals the index entry.

## Review rules

- The source must build reproducibly from the pinned commit through the index's build script. A plugin that does not
  build in CI is not reviewed.
- Maintainers read the code at the pinned commit, including everything that runs at build time: `build.rs`, proc
  macros, and every change to `Cargo.lock` (each new or updated dependency is code that runs during the build).
- No network access beyond the hosts declared in the manifest.
- Every permission must be justified in the pull request.
- Maintainers can reject or remove a plugin, or revoke a version, without giving a reason.

## Updates

Open a pull request that changes `commit` and `sha256` in your `plugins/<id>/plugin.json`, and **bump `version`** in
`manifest.json`. A released version never changes: the same version with different bytes is refused. The sha256 pin
works as for the first submission.

For the Manager, an entry is an update of an installed plugin only when it has **the same id, the same author** and a
higher version (compared as numbers), and is not revoked. So keep `author` exactly as it was: a package with the same
id and another author is treated as a different plugin, and it does not get the old one's private data.

Updates are never automatic. The user sees **Update available (x.y.z)** on the plugin's card and clicks it; if the new
version asks for more permissions, the consent window opens again with them marked "new in this version". The private
folder is kept across updates.

Moving the plugin to another repository is an ownership change: every Manager would offer the new repository's code as
an update. The check warns about it, and maintainers confirm it with the current author first.

## Removal and revocation

- To remove a plugin from the index, open a pull request that deletes its `plugins/<id>/` folder. Installed copies
  are not switched off by that: only a revocation does that.
- To pull a bad version from users' machines, a maintainer adds it to `revoked.json` in the index repository:

  ```json
  [{ "id": "<id>", "versions": ["1.0.0"], "reason": "..." }]
  ```

  (`["*"]` revokes every version.) At the next index refresh, every Manager switches the revoked version off, shows
  "Revoked by the official index: it is switched off" with the reason, and does not let the user switch it on again
  while it stays revoked. A revoked version cannot be installed. If you find a serious problem in a released version,
  ask the maintainers to revoke it, and publish a fixed version.

## Trying an index locally

With **Developer mode** on, Settings has **Index URL (testing)** and **Test keys**: the Manager can read a test index
signed with your own key instead of the official one. The index repository's `tools/local-test.md` shows the whole
round trip (a throw-away key, packing a plugin, building and signing an index with the `aio-index` tool, serving it on
`127.0.0.1`). Plugins installed from a test index are never shown as Verified.

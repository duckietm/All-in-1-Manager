#!/usr/bin/env python3
"""Builds an All-in-1 Manager plugin and packs it into an .aiop file.

    python source/tools/pack_plugin.py <plugin-dir> [--features <f>] [--out <file>] [--locked] [--offline]

The plugin is a Rust crate (with a manifest.json next to its Cargo.toml) built for
wasm32-wasip2, which already produces a WebAssembly component. The .aiop is a zip with
manifest.json, plugin.wasm (the built wasm, renamed) and, when present, icon.png and i18n/.
Default output: dist/<id>.aiop in the current folder. Prints the output path.

--locked builds with exactly the versions of the workspace's Cargo.lock (it must exist);
--offline never touches the network. The zip is deterministic: the same files always give
the same bytes (fixed order, time 1980-01-01 00:00:00, mode 0644, deflate level 9).
"""
import argparse
import json
import re
import subprocess
import sys
import zipfile
from pathlib import Path

TARGET = "wasm32-wasip2"
MISSING = f"the {TARGET} target is missing: rustup target add {TARGET}"
# Every zip entry gets this time, mode and compression, so packing is reproducible.
FIXED_TIME = (1980, 1, 1, 0, 0, 0)
FILE_MODE = 0o100644
COMPRESS_LEVEL = 9


def fail(message):
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


def crate_name(plugin_dir):
    """The package name from Cargo.toml (the wasm file is named after it, dashes as underscores)."""
    text = (plugin_dir / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r'^\s*name\s*=\s*"([^"]+)"', text, re.M)
    if not m:
        fail("no package name in Cargo.toml")
    return m.group(1).replace("-", "_")


def workspace_lock(plugin_dir):
    """The Cargo.lock of the workspace the plugin builds in (next to the workspace's Cargo.toml)."""
    found = subprocess.run(["cargo", "locate-project", "--workspace", "--message-format", "plain"], cwd=plugin_dir, capture_output=True, encoding="utf-8", errors="replace")
    if found.returncode != 0:
        fail("cargo locate-project failed:\n" + found.stderr)
    return Path(found.stdout.strip()).parent / "Cargo.lock"


def entry_order(name):
    """manifest.json, plugin.wasm, icon.png, then i18n/*.json by name."""
    first = {"manifest.json": 0, "plugin.wasm": 1, "icon.png": 2}
    return (first.get(name, 3), name)


def write_package(out, files):
    """Writes the .aiop: `files` maps each entry name to its bytes. Same files, same bytes."""
    with zipfile.ZipFile(out, "w") as z:
        for name in sorted(files, key=entry_order):
            info = zipfile.ZipInfo(name, FIXED_TIME)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.create_system = 3  # Unix, so the mode below means the same everywhere
            info.external_attr = FILE_MODE << 16
            z.writestr(info, files[name], compresslevel=COMPRESS_LEVEL)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("plugin_dir")
    ap.add_argument("--features", help="Cargo features to build with")
    ap.add_argument("--out", help="output file (default dist/<id>.aiop)")
    ap.add_argument("--locked", action="store_true", help="build with the workspace's Cargo.lock as it is (required)")
    ap.add_argument("--offline", action="store_true", help="build without the network (cargo --offline)")
    args = ap.parse_args()

    plugin_dir = Path(args.plugin_dir).resolve()
    if not (plugin_dir / "Cargo.toml").is_file() or not (plugin_dir / "manifest.json").is_file():
        fail(f"{plugin_dir} needs a Cargo.toml and a manifest.json")
    manifest = json.loads((plugin_dir / "manifest.json").read_text(encoding="utf-8"))
    plugin_id = manifest.get("id")
    if not plugin_id:
        fail("manifest.json has no id")

    try:
        rustup = subprocess.run(["rustup", "target", "list", "--installed"], capture_output=True, encoding="utf-8", errors="replace")
        if rustup.returncode == 0 and TARGET not in rustup.stdout.split():
            fail(MISSING)
    except FileNotFoundError:
        pass  # no rustup: let cargo report what is missing

    cargo_flags = []
    if args.locked:
        lock = workspace_lock(plugin_dir)
        if not lock.is_file():
            fail(f"--locked needs a Cargo.lock next to the workspace's Cargo.toml: {lock} is missing")
        cargo_flags.append("--locked")
    if args.offline:
        cargo_flags.append("--offline")

    # A feature build gets its own target dir so the variants do not overwrite each other.
    cmd = ["cargo", "build", "--release", "--target", TARGET, "--quiet"] + cargo_flags
    target_dir = None
    if args.features:
        target_dir = plugin_dir / "target" / ("features-" + re.sub(r"[^A-Za-z0-9_+-]", "_", args.features))
        cmd += ["--features", args.features, "--target-dir", str(target_dir)]
    result = subprocess.run(cmd, cwd=plugin_dir, capture_output=True, encoding="utf-8", errors="replace")
    if result.returncode != 0:
        output = result.stdout + result.stderr
        if TARGET in output and "may not be installed" in output:
            fail(MISSING)
        print(output, file=sys.stderr)
        fail("the cargo build failed")

    if target_dir is None:
        # The plugin may be a member of a workspace, which owns the target dir.
        meta = subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"] + cargo_flags, cwd=plugin_dir, capture_output=True, encoding="utf-8", errors="replace")
        if meta.returncode != 0:
            fail("cargo metadata failed:\n" + meta.stderr)
        target_dir = Path(json.loads(meta.stdout)["target_directory"])
    wasm = target_dir / TARGET / "release" / f"{crate_name(plugin_dir)}.wasm"
    if not wasm.is_file():
        fail(f"the build did not produce {wasm} (is crate-type cdylib?)")

    out = Path(args.out) if args.out else Path("dist") / f"{plugin_id}.aiop"
    out.parent.mkdir(parents=True, exist_ok=True)
    files = {"manifest.json": (plugin_dir / "manifest.json").read_bytes(), "plugin.wasm": wasm.read_bytes()}
    if (plugin_dir / "icon.png").is_file():
        files["icon.png"] = (plugin_dir / "icon.png").read_bytes()
    i18n = plugin_dir / "i18n"
    if i18n.is_dir():
        for f in i18n.glob("*.json"):
            files[f"i18n/{f.name}"] = f.read_bytes()
    write_package(out, files)
    print(out.resolve())


if __name__ == "__main__":
    main()

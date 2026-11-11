#!/usr/bin/env python3
"""Builds an All-in-1 Manager plugin and packs it into an .aiop file.

    python source/tools/pack_plugin.py <plugin-dir> [--features <f>] [--out <file>]

The plugin is a Rust crate (with a manifest.json next to its Cargo.toml) built for
wasm32-wasip2, which already produces a WebAssembly component. The .aiop is a zip with
manifest.json, plugin.wasm (the built wasm, renamed) and, when present, icon.png and i18n/.
Default output: dist/<id>.aiop in the current folder. Prints the output path.
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


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("plugin_dir")
    ap.add_argument("--features", help="Cargo features to build with")
    ap.add_argument("--out", help="output file (default dist/<id>.aiop)")
    args = ap.parse_args()

    plugin_dir = Path(args.plugin_dir).resolve()
    if not (plugin_dir / "Cargo.toml").is_file() or not (plugin_dir / "manifest.json").is_file():
        fail(f"{plugin_dir} needs a Cargo.toml and a manifest.json")
    manifest = json.loads((plugin_dir / "manifest.json").read_text(encoding="utf-8"))
    plugin_id = manifest.get("id")
    if not plugin_id:
        fail("manifest.json has no id")

    try:
        installed = subprocess.run(["rustup", "target", "list", "--installed"], capture_output=True, text=True).stdout
        if TARGET not in installed.split():
            fail(MISSING)
    except FileNotFoundError:
        pass  # no rustup: let cargo report what is missing

    # A feature build gets its own target dir so the variants do not overwrite each other.
    cmd = ["cargo", "build", "--release", "--target", TARGET, "--quiet"]
    target_dir = None
    if args.features:
        target_dir = plugin_dir / "target" / f"features-{args.features.replace(',', '+')}"
        cmd += ["--features", args.features, "--target-dir", str(target_dir)]
    result = subprocess.run(cmd, cwd=plugin_dir, capture_output=True, text=True)
    if result.returncode != 0:
        output = result.stdout + result.stderr
        if TARGET in output and "may not be installed" in output:
            fail(MISSING)
        print(output, file=sys.stderr)
        fail("the cargo build failed")

    if target_dir is None:
        # The plugin may be a member of a workspace, which owns the target dir.
        meta = subprocess.run(["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=plugin_dir, capture_output=True, text=True)
        if meta.returncode != 0:
            fail("cargo metadata failed:\n" + meta.stderr)
        target_dir = Path(json.loads(meta.stdout)["target_directory"])
    wasm = target_dir / TARGET / "release" / f"{crate_name(plugin_dir)}.wasm"
    if not wasm.is_file():
        fail(f"the build did not produce {wasm} (is crate-type cdylib?)")

    out = Path(args.out) if args.out else Path("dist") / f"{plugin_id}.aiop"
    out.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
        z.write(plugin_dir / "manifest.json", "manifest.json")
        z.write(wasm, "plugin.wasm")
        if (plugin_dir / "icon.png").is_file():
            z.write(plugin_dir / "icon.png", "icon.png")
        i18n = plugin_dir / "i18n"
        if i18n.is_dir():
            for f in sorted(i18n.glob("*.json")):
                z.write(f, f"i18n/{f.name}")
    print(out.resolve())


if __name__ == "__main__":
    main()

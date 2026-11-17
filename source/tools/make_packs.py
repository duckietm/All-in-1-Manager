"""The data packs of the Plugins page (source/crates/app/src/plugins/packs.rs), for a release or a local build.

    python source/tools/make_packs.py dist/            # plugin-<id>.zip and plugin-<id>.zip.sha256, to publish
    python source/tools/make_packs.py --install DIR    # unpacked into DIR/plugins/<id>/, next to the program

client-assets   = source/Assets (unpacked into a folder)
default-catalog = source/crates/app/assets/catalog/Full_Catalog.zip (kept as it is)
"""

import hashlib
import os
import shutil
import sys
import zipfile

SOURCE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSETS = os.path.join(SOURCE, "Assets")
CATALOG = os.path.join(SOURCE, "crates", "app", "assets", "catalog", "Full_Catalog.zip")


def zip_folder(folder, target):
    # Images are compressed already: stored, like build.rs did.
    with zipfile.ZipFile(target, "w", zipfile.ZIP_STORED) as z:
        for root, dirs, files in os.walk(folder):
            dirs.sort()
            for name in sorted(files):
                path = os.path.join(root, name)
                z.write(path, os.path.relpath(path, folder).replace(os.sep, "/"))


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def publish(out):
    os.makedirs(out, exist_ok=True)
    made = []
    target = os.path.join(out, "plugin-client-assets.zip")
    zip_folder(ASSETS, target)
    made.append(target)
    target = os.path.join(out, "plugin-default-catalog.zip")
    shutil.copyfile(CATALOG, target)
    made.append(target)
    for path in made:
        with open(path + ".sha256", "w", newline="\n") as f:
            f.write(f"{sha256(path)}  {os.path.basename(path)}\n")
        print(f"{path}  ({os.path.getsize(path) // 1024} KB)")


def install(program_dir):
    """Puts the packs in plugins/ next to the program, where it finds them, so a local build needs no download."""
    plugins = os.path.join(program_dir, "plugins")
    assets = os.path.join(plugins, "client-assets")
    shutil.rmtree(assets, ignore_errors=True)
    shutil.copytree(ASSETS, assets)
    catalog = os.path.join(plugins, "default-catalog")
    os.makedirs(catalog, exist_ok=True)
    shutil.copyfile(CATALOG, os.path.join(catalog, "Full_Catalog.zip"))
    print(f"Packs: {plugins}")


def main():
    args = sys.argv[1:]
    if len(args) == 2 and args[0] == "--install":
        install(args[1])
    elif len(args) == 1 and not args[0].startswith("-"):
        publish(args[0])
    else:
        print(__doc__.strip())
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())

"""Builds the installer and puts the program in public/, ready to run:

    python source/tools/build.py dev        # debug build      -> public/dev/
    python source/tools/build.py release    # release build    -> public/release/ (with Assets/ next to the program)
    python source/tools/build.py dev --run  # and start it

The program keeps its configuration (SSH keys, passwords, history) in configuration/ next to itself, so public/dev and
public/release each have their own; git ignores both folders.
"""

import os
import shutil
import subprocess
import sys

SOURCE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ROOT = os.path.dirname(SOURCE)
EXE = "all-in-1-installer" + (".exe" if os.name == "nt" else "")


def main():
    args = sys.argv[1:]
    if not args or args[0] not in ("dev", "release"):
        print(__doc__.strip())
        return 2
    kind = args[0]
    command = ["cargo", "build", "-p", "aio-app"] + (["--release"] if kind == "release" else [])
    print("> " + " ".join(command))
    if subprocess.call(command, cwd=ROOT) != 0:
        return 1

    built = os.path.join(ROOT, "target", "release" if kind == "release" else "debug", EXE)
    into = os.path.join(ROOT, "public", kind)
    os.makedirs(into, exist_ok=True)
    target = os.path.join(into, EXE)
    try:
        shutil.copy2(built, target)
    except PermissionError:
        print(f"{target} is in use: close the installer and run this again.")
        return 1
    if kind == "release":
        # The client images are in the program too; the folder next to it is what Deploy Assets reads first.
        assets = os.path.join(into, "Assets")
        shutil.rmtree(assets, ignore_errors=True)
        shutil.copytree(os.path.join(SOURCE, "Assets"), assets)
    print(f"Ready: {target}")

    if "--run" in args:
        subprocess.Popen([target], cwd=into)
    return 0


if __name__ == "__main__":
    sys.exit(main())

"""Builds the installer and puts the program in public/, ready to run:

    python source/tools/build.py dev        # debug build      -> public/dev/
    python source/tools/build.py release    # release build    -> public/release/ (with plugins/ next to the program)
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
EXE = "all-in-1-manager" + (".exe" if os.name == "nt" else "")


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
        # The data packs of the Plugins page, next to the program, so it needs no download.
        sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
        import make_packs

        make_packs.install(into)
    shortcut(into, kind)
    print(f"Ready: {target}")

    if "--run" in args:
        subprocess.Popen([target], cwd=into)
    return 0


def shortcut(into, kind):
    """A shortcut next to build.cmd to the program, started in its own folder so it keeps its own configuration/.

    Windows: "All-in-1 Manager.lnk"; elsewhere a symbolic link ./all-in-1-manager, and on Linux also a launcher
    "All-in-1 Manager.desktop" to double-click. "(dev)" / "-dev" for the debug build. A failure only warns."""
    title = "All-in-1 Manager" + (" (dev)" if kind == "dev" else "")
    target = os.path.join(into, EXE)
    try:
        if os.name == "nt":
            script = (
                "$ErrorActionPreference='Stop'; $s=(New-Object -ComObject WScript.Shell).CreateShortcut($env:AIO_LNK); "
                "$s.TargetPath=$env:AIO_EXE; $s.WorkingDirectory=$env:AIO_DIR; $s.IconLocation=$env:AIO_EXE+',0'; "
                "$s.Description='All-in-1 Manager'; $s.Save()"
            )
            env = dict(os.environ, AIO_LNK=os.path.join(ROOT, title + ".lnk"), AIO_EXE=target, AIO_DIR=into)
            subprocess.run(["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", script], env=env, check=True, capture_output=True)
            return
        link = os.path.join(ROOT, "all-in-1-manager" + ("-dev" if kind == "dev" else ""))
        if os.path.lexists(link):
            os.remove(link)
        os.symlink(os.path.relpath(target, ROOT), link)
        os.chmod(target, 0o755)
        if sys.platform.startswith("linux"):
            launcher = os.path.join(ROOT, title + ".desktop")
            with open(launcher, "w", encoding="utf-8") as f:
                lines = [
                    "[Desktop Entry]",
                    "Type=Application",
                    "Name=" + title,
                    "Comment=Hotel server setup",
                    'Exec="' + target + '"',
                    "Path=" + into,
                    "Terminal=false",
                    "Categories=Utility;Development;",
                ]
                icon = os.path.join(SOURCE, "crates", "app", "assets", "icon.png")
                if os.path.exists(icon):
                    lines.append("Icon=" + icon)
                f.write("\n".join(lines) + "\n")
            os.chmod(launcher, 0o755)
    except (OSError, subprocess.CalledProcessError) as e:
        print(f"Note: the shortcut to {target} could not be made ({e}).")


if __name__ == "__main__":
    sys.exit(main())

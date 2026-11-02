"""The program's version, kept the same in Cargo.toml ([workspace.package]), Cargo.lock (the four aio-* crates) and
the Windows version information (source/crates/app/resources/app.rc).

    python source/tools/version.py              # print the version
    python source/tools/version.py --next       # raise the last number (2.0.3 -> 2.0.4) and print the new version
    python source/tools/version.py --set 2.1.0  # set a version of your own

The release workflow runs --next by itself when main gets new commits after a release.
"""

import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CARGO = os.path.join(ROOT, "Cargo.toml")
LOCK = os.path.join(ROOT, "Cargo.lock")
RC = os.path.join(ROOT, "source", "crates", "app", "resources", "app.rc")
CRATES = ("aio-app", "aio-core", "aio-remote", "aio-terminal")
WORKSPACE_VERSION = re.compile(r'(\[workspace\.package\][^\[]*?\nversion = ")([^"]+)(")', re.S)


def read(path):
    with open(path, encoding="utf-8", newline="") as f:
        return f.read()


def write(path, text):
    with open(path, "w", encoding="utf-8", newline="") as f:
        f.write(text)


def current():
    return WORKSPACE_VERSION.search(read(CARGO)).group(2)


def set_version(new):
    major, minor, patch = (int(x) for x in new.split("."))
    write(CARGO, WORKSPACE_VERSION.sub(lambda m: m.group(1) + new + m.group(3), read(CARGO), count=1))
    lock = read(LOCK)
    for crate in CRATES:
        lock = re.sub(r'(name = "%s"\nversion = ")[^"]+(")' % re.escape(crate), lambda m: m.group(1) + new + m.group(2), lock)
    write(LOCK, lock)
    rc = read(RC)
    rc = re.sub(r"((?:FILE|PRODUCT)VERSION\s+)\d+,\d+,\d+,\d+", lambda m: f"{m.group(1)}{major},{minor},{patch},0", rc)
    rc = re.sub(r'(VALUE "(?:File|Product)Version",\s+")[^"]+(")', lambda m: m.group(1) + new + m.group(2), rc)
    write(RC, rc)


def main():
    args = sys.argv[1:]
    if not args:
        print(current())
    elif args[0] == "--next":
        major, minor, patch = (int(x) for x in current().split("."))
        set_version(f"{major}.{minor}.{patch + 1}")
        print(current())
    elif args[0] == "--set" and len(args) == 2 and re.fullmatch(r"\d+\.\d+\.\d+", args[1]):
        set_version(args[1])
        print(current())
    else:
        print(__doc__.strip())
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())

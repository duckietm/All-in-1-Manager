"""Collects every English window text (the first argument of t("...") and tf("...")) in crates/app/src into
crates/app/assets/i18n/keys.json, and lists per language the keys its JSON file is missing.

    python source/tools/i18n_keys.py            # write keys.json, report what is missing
    python source/tools/i18n_keys.py --missing  # also write <lang>.missing.json with the untranslated keys
"""

import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SOURCE = os.path.join(ROOT, "crates", "app", "src")
I18N = os.path.join(ROOT, "crates", "app", "assets", "i18n")
LANGUAGES = ["it", "fr", "de", "nl", "es", "pt", "tr", "fi"]

# t("..."), tf("...", ...) or n("...") (a text translated later through a variable): a Rust string literal with \" and
# \\ escapes; also raw strings.
CALL = re.compile(r'\b(?:t|tf|n)\(\s*(?:"((?:[^"\\]|\\.)*)"|r#?"(.*?)"#?)', re.S)


def unescape(text):
    out = []
    i = 0
    while i < len(text):
        c = text[i]
        if c == "\\" and i + 1 < len(text):
            n = text[i + 1]
            if n == "n":
                out.append("\n")
            elif n == "t":
                out.append("\t")
            elif n in "\"\\'":
                out.append(n)
            elif n == "\n":
                # A line continuation: skip the newline and the indentation after it.
                i += 2
                while i < len(text) and text[i] in " \t":
                    i += 1
                continue
            elif n == "u":
                end = text.index("}", i)
                out.append(chr(int(text[i + 3:end], 16)))
                i = end + 1
                continue
            else:
                out.append(n)
            i += 2
        else:
            out.append(c)
            i += 1
    return "".join(out)


def collect():
    keys = set()
    for folder, _, files in os.walk(SOURCE):
        for name in files:
            if not name.endswith(".rs"):
                continue
            with open(os.path.join(folder, name), encoding="utf-8") as f:
                text = f.read()
            for match in CALL.finditer(text):
                value = unescape(match.group(1)) if match.group(1) is not None else match.group(2)
                if value.strip():
                    keys.add(value)
    return sorted(keys)


def main():
    keys = collect()
    with open(os.path.join(I18N, "keys.json"), "w", encoding="utf-8", newline="\n") as f:
        json.dump(keys, f, ensure_ascii=False, indent=2)
        f.write("\n")
    print(f"{len(keys)} texts")
    for lang in LANGUAGES:
        path = os.path.join(I18N, f"{lang}.json")
        table = {}
        if os.path.exists(path):
            with open(path, encoding="utf-8") as f:
                table = json.load(f)
        missing = [k for k in keys if not table.get(k)]
        print(f"{lang}: {len(missing)} missing")
        if "--missing" in sys.argv:
            with open(os.path.join(I18N, f"{lang}.missing.json"), "w", encoding="utf-8", newline="\n") as f:
                json.dump({k: "" for k in missing}, f, ensure_ascii=False, indent=2)
                f.write("\n")


if __name__ == "__main__":
    main()

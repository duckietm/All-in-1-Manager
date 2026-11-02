"""The catalogue's page texts in other languages: collects the Dutch texts of catalog_pages and catalog_pages_bc from the
packed catalogue (crates/app/assets/catalog/Full_Catalog.zip, Catalog_Export.sql) into
crates/app/assets/catalog/texts/keys.json, and per language what its dictionary (texts/<lang>.json, Dutch text ->
translation) is missing.

    python source/tools/catalog_texts.py            # write keys.json, report what is missing per language
    python source/tools/catalog_texts.py --missing  # also write texts/<lang>.missing.json with the untranslated texts
    python source/tools/catalog_texts.py --merge    # put the filled-in texts/<lang>.missing.json into texts/<lang>.json

After a new catalogue export: replace Full_Catalog.zip, run --missing, translate the values in the .missing.json files
(keep HTML tags, line breaks and %placeholders% as they are), run --merge and commit. A text without a translation
stays Dutch in the hotel.
"""

import json
import os
import sys
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CATALOG = os.path.join(ROOT, "crates", "app", "assets", "catalog")
TEXTS = os.path.join(CATALOG, "texts")
LANGUAGES = ["en", "es", "pt", "de", "fr", "it", "fi", "tr"]
TABLES = ["catalog_pages", "catalog_pages_bc"]
COLUMNS = ["caption", "page_text1", "page_text2", "page_text_details", "page_text_teaser"]

ESCAPES = {"0": "\0", "b": "\b", "n": "\n", "r": "\r", "t": "\t", "Z": "\x1a"}


def statements(sql, table):
    """The INSERT statements of one table: (columns, rows as lists of values)."""
    head = f"INSERT INTO `{table}` ("
    at = 0
    while True:
        start = sql.find(head, at)
        if start < 0:
            return
        end_columns = sql.index(")", start + len(head))
        columns = [c.strip().strip("`") for c in sql[start + len(head):end_columns].split(",")]
        values_at = sql.index("VALUES", end_columns) + len("VALUES")
        rows, at = parse_rows(sql, values_at)
        yield columns, rows


def parse_rows(sql, i):
    """The (...),(...); tuples from position i: values as str (quoted), or the raw word (numbers, NULL)."""
    rows, row, value, quoted = [], None, None, False
    n = len(sql)
    while i < n:
        c = sql[i]
        if row is None:
            if c == "(":
                row, value, quoted = [], [], False
            elif c == ";":
                return rows, i + 1
            i += 1
            continue
        if c == "'":
            # A quoted string, with \\ escapes and '' for a quote.
            i += 1
            text = []
            while True:
                c = sql[i]
                if c == "\\":
                    text.append(ESCAPES.get(sql[i + 1], sql[i + 1]))
                    i += 2
                elif c == "'" and i + 1 < n and sql[i + 1] == "'":
                    text.append("'")
                    i += 2
                elif c == "'":
                    i += 1
                    break
                else:
                    text.append(c)
                    i += 1
            value, quoted = "".join(text), True
            continue
        if c in ",)":
            row.append(value if quoted else ("".join(value).strip() if isinstance(value, list) else value))
            value, quoted = [], False
            if c == ")":
                rows.append(row)
                row = None
            i += 1
            continue
        if isinstance(value, list):
            value.append(c)
        i += 1
    return rows, i


def read_sql():
    with zipfile.ZipFile(os.path.join(CATALOG, "Full_Catalog.zip")) as archive:
        return archive.read("Catalog_Export.sql").decode("utf-8")


def collect(sql):
    texts = set()
    for table in TABLES:
        for columns, rows in statements(sql, table):
            wanted = [columns.index(c) for c in COLUMNS if c in columns]
            for row in rows:
                for index in wanted:
                    value = row[index]
                    if isinstance(value, str) and value.strip() and value != "NULL":
                        texts.add(value)
    return sorted(texts)


def load(path):
    if not os.path.exists(path):
        return {}
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def save(path, data):
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=2)
        f.write("\n")


def main():
    os.makedirs(TEXTS, exist_ok=True)
    keys = collect(read_sql())
    save(os.path.join(TEXTS, "keys.json"), keys)
    print(f"{len(keys)} texts in the catalogue")
    for lang in LANGUAGES:
        path = os.path.join(TEXTS, f"{lang}.json")
        dictionary = load(path)
        missing_path = os.path.join(TEXTS, f"{lang}.missing.json")
        if "--merge" in sys.argv and os.path.exists(missing_path):
            filled = {k: v for k, v in load(missing_path).items() if isinstance(v, str) and v.strip()}
            dictionary.update(filled)
            save(path, dict(sorted(dictionary.items())))
            os.remove(missing_path)
            print(f"{lang}: {len(filled)} merged")
        missing = [k for k in keys if not dictionary.get(k)]
        unused = [k for k in dictionary if k not in set(keys)]
        print(f"{lang}: {len(missing)} missing" + (f", {len(unused)} no longer in the catalogue" if unused else ""))
        if "--missing" in sys.argv and missing:
            save(missing_path, {k: "" for k in missing})
    return 0


if __name__ == "__main__":
    sys.exit(main())

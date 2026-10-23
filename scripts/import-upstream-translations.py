#!/usr/bin/env python3
"""Carry upstream's translations into the Apple String Catalog.

Upstream (Android) ships 19 locales of human translations in
`app/src/main/res/values-*/strings.xml`. Our UI strings are mostly the same
English — the Apple app is a port — so wherever one of our catalog keys is
*exactly* an upstream English string, the upstream translation is carried over.
Nothing is machine-translated, and nothing is matched loosely: a near match is a
different sentence, and a wrong translation is worse than an English one.

Usage (from the repo root, after a build has extracted keys into the catalog):

    python3 scripts/import-upstream-translations.py <upstream-checkout>

Keys with format arguments are matched only when the argument shapes agree
(Android `%1$s`/`%s`/`%d` ↔ Apple `%@`/`%lld`), and are rewritten to the Apple form.
"""

import json
import os
import re
import sys
import xml.etree.ElementTree as ET

CATALOG = "AppleApp/Resources/Localization/Localizable.xcstrings"

# Android resource qualifier → Apple locale identifier.
LOCALES = {
    "de": "de", "es": "es", "fr": "fr", "he": "he", "hi": "hi", "id": "id",
    "it": "it", "ja": "ja", "pl": "pl", "pt": "pt-BR", "ru": "ru", "th": "th",
    "tr": "tr", "vi": "vi", "zh": "zh-Hans", "zh-rTW": "zh-Hant",
    "zh-rHK": "zh-HK", "b+zh+Hant": "zh-Hant",
}

ANDROID_ARG = re.compile(r"%(?:(\d+)\$)?([sd])")


def android_text(element):
    """The string as a user would read it: Android escapes undone."""
    text = "".join(element.itertext())
    text = text.replace("\\'", "'").replace('\\"', '"').replace("\\n", "\n").replace("\\@", "@")
    text = text.replace("\\?", "?")
    if len(text) >= 2 and text[0] == '"' and text[-1] == '"':
        text = text[1:-1]
    return text


def load(path):
    try:
        root = ET.parse(path).getroot()
    except (ET.ParseError, FileNotFoundError):
        return {}
    out = {}
    for item in root.findall("string"):
        if item.get("translatable") == "false":
            continue
        out[item.get("name")] = android_text(item)
    return out


def to_apple(text):
    """`%1$s` → `%1$@`, `%d` → `%lld`; the shape Swift interpolation extracts as."""
    def swap(match):
        index, kind = match.group(1), match.group(2)
        spec = "@" if kind == "s" else "lld"
        return f"%{index}${spec}" if index else f"%{spec}"
    return ANDROID_ARG.sub(swap, text)


def shape(text):
    return sorted(re.findall(r"%(?:\d+\$)?(@|lld|d|s)", text))


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    res = os.path.join(sys.argv[1], "app/src/main/res")
    english = load(os.path.join(res, "values/strings.xml"))
    # English text (Apple form) → resource name. First wins on duplicates.
    by_text = {}
    for name, text in english.items():
        by_text.setdefault(to_apple(text), name)

    with open(CATALOG, encoding="utf-8") as handle:
        catalog = json.load(handle)
    strings = catalog.setdefault("strings", {})

    carried = {locale: 0 for locale in set(LOCALES.values())}
    for qualifier, locale in LOCALES.items():
        translated = load(os.path.join(res, f"values-{qualifier}/strings.xml"))
        if not translated:
            continue
        for key, entry in strings.items():
            name = by_text.get(key)
            if name is None or name not in translated:
                continue
            value = to_apple(translated[name])
            if shape(value) != shape(key) or value == key:
                continue
            localizations = entry.setdefault("localizations", {})
            if locale in localizations:
                continue
            localizations[locale] = {"stringUnit": {"state": "translated", "value": value}}
            carried[locale] += 1

    with open(CATALOG, "w", encoding="utf-8") as handle:
        json.dump(catalog, handle, ensure_ascii=False, indent=2, sort_keys=True)
        handle.write("\n")
    total = len(strings)
    for locale, count in sorted(carried.items()):
        print(f"{locale:8} {count:5} of {total} keys")


if __name__ == "__main__":
    main()

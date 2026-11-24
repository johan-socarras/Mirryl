# -*- coding: utf-8 -*-
"""Generates eula-installer.txt from EULA.txt.

The NSIS installer (tauri.conf.json -> bundle.windows.nsis.licenseFile) shows
the EULA in a control that only draws ASCII, so this file is EULA.txt
transliterated and in CRLF, nothing more. Run it from src-tauri whenever
EULA.txt changes:

    python gen-eula-installer.py

It aborts if any non-ASCII character is left untranslated, instead of leaving
a stray symbol on the acceptance screen.
"""
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
SRC = HERE / "EULA.txt"
DST = HERE / "eula-installer.txt"

MAP = {
    "—": "-",    # em dash
    "–": "-",    # en dash
    "→": "->",   # arrow
    "’": "'", "‘": "'",
    "“": '"', "”": '"',
    "…": "...",
    " ": " ",
}

text = SRC.read_bytes().decode("utf-8")
if "\r" in text:
    sys.exit("EULA.txt contains CR: it must be LF (it is the source, not the output)")
for k, v in MAP.items():
    text = text.replace(k, v)
rest = sorted({c for c in text if ord(c) > 127})
if rest:
    sys.exit(f"non-ASCII characters left untranslated: {rest!r}")
DST.write_bytes(text.replace("\n", "\r\n").encode("ascii"))
print(f"OK {DST.name}: {text.count(chr(10))} lines, ASCII, CRLF")

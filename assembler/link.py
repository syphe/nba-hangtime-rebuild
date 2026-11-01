#!/usr/bin/env python3
"""Link experimental TMS34010 JSON objects into a flat ROM image."""

import argparse
import base64
import json
import struct
from pathlib import Path

ROM_BASE = 0xFF800000
ROM_LENGTH = 0x7F6F00
FILL = 0xFF


def section_name(name: str) -> str:
    lowered = name.lower()
    if lowered == "text" or lowered == ".text":
        return "text"
    if lowered == "data" or lowered == ".data":
        return "data"
    return lowered


def load_objects(paths: list[Path]):
    objects = []
    for path in paths:
        value = json.loads(path.read_text())
        if value.get("format") != "tms34010-object":
            raise ValueError(f"{path}: unsupported object format")
        objects.append((path, value))
    return objects


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("objects", nargs="+", type=Path)
    parser.add_argument("--output", type=Path, default=Path("build/BB.bin"))
    args = parser.parse_args()

    objects = load_objects(args.objects)
    text_cursor = ROM_BASE
    symbols = {}
    placements = []

    text_size = sum(
        len(base64.b64decode(section["data"]))
        for _, obj in objects
        for section in obj.get("sections", [])
        if section_name(section["name"]) == "text"
    )
    data_cursor = ROM_BASE + text_size

    for path, obj in objects:
        section_addresses = {}
        for section in obj.get("sections", []):
            name = section_name(section["name"])
            data = base64.b64decode(section["data"])
            if name == "text":
                section_addresses[name] = text_cursor
                text_cursor += len(data)
            elif name == "data":
                section_addresses[name] = data_cursor
                data_cursor += len(data)
            else:
                section_addresses[name] = 0

        placements.append((path, obj, section_addresses))
        for symbol in obj.get("symbols", []):
            section = section_name(symbol.get("section", "")) if symbol.get("section") else ""
            if section in section_addresses:
                symbols[symbol["name"].lower()] = section_addresses[section] + symbol.get("section_offset", 0)

    rom = bytearray([FILL]) * ROM_LENGTH

    # Copy section payloads into the flat ROM image.
    for path, obj, addresses in placements:
        for section in obj.get("sections", []):
            name = section_name(section["name"])
            if name not in ("text", "data"):
                continue
            data = base64.b64decode(section["data"])
            address = addresses[name]
            if address == 0:
                continue
            start = address - ROM_BASE
            if start < 0 or start + len(data) > len(rom):
                raise ValueError(f"{path}: section {name} is outside the ROM image")
            rom[start:start + len(data)] = data

    unresolved_fixups = []
    # Apply the short relative fixups emitted in object format.
    for path, obj, addresses in placements:
        section_addresses = addresses
        for fixup in obj.get("fixups", []):
            section = section_name(fixup["section"])
            if section not in ("text", "data") or section not in section_addresses:
                continue
            if fixup["symbol"].lower() not in symbols:
                unresolved_fixups.append((path, fixup["symbol"]))
                continue
            source = section_addresses[section] + fixup["offset"]
            target = symbols[fixup["symbol"].lower()]
            relative = (target - (source + 2)) // 2
            if relative < -128 or relative > 127:
                print(f"Warning: {path}: long fixup {fixup['symbol']} ({relative})")
                continue
            start = source - ROM_BASE
            if start < 0 or start + 2 > len(rom):
                raise ValueError(f"{path}: fixup is outside the ROM image")
            rom[start + 1] = relative & 0xFF

    if unresolved_fixups:
        unique = sorted({f"{path.name}: {symbol}" for path, symbol in unresolved_fixups})
        raise ValueError("Unresolved relocations:\n  " + "\n  ".join(unique))

    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(rom)
    print(f"Wrote {output} ({len(rom)} bytes)")


if __name__ == "__main__":
    main()

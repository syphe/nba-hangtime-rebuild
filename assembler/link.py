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
    unresolved_definitions = []
    placements = []

    # The linker places text first, followed by data, in .cmd module order.
    for path, obj in objects:
        for section in obj.get("sections", []):
            if section_name(section["name"]) == "text":
                text_cursor += len(base64.b64decode(section["data"]))

    data_cursor = text_cursor
    for path, obj in objects:
        section_addresses = {}
        text_address = 0
        data_address = 0
        for section in obj.get("sections", []):
            name = section_name(section["name"])
            data = base64.b64decode(section["data"])
            if name == "text":
                address = section_addresses.get("text", 0)
                if not address:
                    address = ROM_BASE
                    for earlier_path, earlier_obj, earlier_addresses in placements:
                        if earlier_addresses.get("text"):
                            earlier_section = next(
                                (item for item in earlier_obj["sections"] if section_name(item["name"]) == "text"),
                                None,
                            )
                            if earlier_section:
                                address += len(base64.b64decode(earlier_section["data"]))
                text_address = address
            elif name == "data":
                address = data_cursor
                data_address = address
                data_cursor += len(data)
            else:
                address = 0
            section_addresses[name] = address

        placements.append((path, obj, section_addresses))
        for symbol in obj.get("symbols", []):
            section = section_name(symbol.get("section", "")) if symbol.get("section") else ""
            if section == "text":
                symbols[symbol["name"].lower()] = text_address + symbol.get("section_offset", 0)
            elif section == "data":
                symbols[symbol["name"].lower()] = data_address + symbol.get("section_offset", 0)
            elif symbol.get("address", 0) != 0:
                unresolved_definitions.append((path, symbol["name"]))

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
            if section not in section_addresses:
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

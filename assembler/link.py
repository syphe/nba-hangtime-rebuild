#!/usr/bin/env python3
"""Link experimental TMS34010 JSON objects into a flat ROM image."""

import argparse
import base64
import json
import ast
import re
import struct
from pathlib import Path

ROM_BASE = 0xFF800000
ROM_LENGTH = 0x7F6F00
FILL = 0xFF


def order_from_cmd(objects, cmd_path: Path):
    """Order supplied object files as listed in the legacy linker command file."""
    by_name = {path.name.upper(): (path, obj) for path, obj in objects}
    listed_names = []
    for line in cmd_path.read_text(errors="replace").splitlines():
        line = re.sub(r"/\*.*?\*/", "", line).strip()
        match = re.match(r"([^\s]+\.OBJ)\b", line, re.IGNORECASE)
        if match:
            listed_names.append(match.group(1).upper())

    ordered = []
    used = set()
    for name in listed_names:
        if name in by_name and name not in used:
            ordered.append(by_name[name])
            used.add(name)
    # Keep objects absent from the legacy command file so new modules still link,
    # after all explicitly ordered legacy inputs.
    ordered.extend(item for item in objects if item[0].name.upper() not in used)
    return ordered


def section_name(name: str) -> str:
    lowered = name.lower()
    if lowered == "text" or lowered == ".text":
        return "text"
    if lowered == "data" or lowered == ".data":
        return "data"
    return lowered


def resolve_expression(expression: str, symbols: dict[str, int]):
    text = expression.strip().replace("$", "0x")
    exact_symbol = symbols.get(text.lower())
    if exact_symbol is not None:
        return exact_symbol

    def legacy_hex(match):
        token = match.group(0)
        return str(int(token[:-1], 16))

    text = re.sub(r"(?<![A-Za-z0-9_])[0-9A-Fa-f]+H", legacy_hex, text)

    symbol_names = sorted(symbols, key=len, reverse=True)
    if symbol_names:
        symbol_pattern = re.compile(
            r"(?<![A-Za-z0-9_])(" + "|".join(re.escape(name) for name in symbol_names) + r")(?=$|[^A-Za-z0-9_])",
            re.IGNORECASE,
        )
        text = symbol_pattern.sub(lambda match: str(symbols[match.group(1).lower()]), text)
    text = text.replace("|", " | ")

    try:
        tree = ast.parse(text, mode="eval")
    except SyntaxError as error:
        raise ValueError(f"unsupported relocation expression {expression!r}") from error

    def evaluate(node):
        if isinstance(node, ast.Expression):
            return evaluate(node.body)
        if isinstance(node, ast.Constant) and isinstance(node.value, (int, float)):
            return int(node.value)
        if isinstance(node, ast.BinOp):
            left, right = evaluate(node.left), evaluate(node.right)
            if isinstance(node.op, ast.Add): return left + right
            if isinstance(node.op, ast.Sub): return left - right
            if isinstance(node.op, ast.Mult): return left * right
            if isinstance(node.op, ast.Div): return int(left / right)
            if isinstance(node.op, ast.FloorDiv): return left // right
            if isinstance(node.op, ast.BitOr): return left | right
            if isinstance(node.op, ast.BitAnd): return left & right
            if isinstance(node.op, ast.LShift): return left << right
            if isinstance(node.op, ast.RShift): return left >> right
        raise ValueError(f"unsupported relocation expression {expression!r}")

    return evaluate(tree)


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
    parser.add_argument("--cmd-file", type=Path)
    args = parser.parse_args()

    objects = load_objects(args.objects)
    if args.cmd_file:
        objects = order_from_cmd(objects, args.cmd_file)
    text_cursor = ROM_BASE
    symbols = {}
    symbol_kinds = {}
    placements = []

    text_size = sum(
        len(base64.b64decode(section["data"]))
        for _, obj in objects
        for section in obj.get("sections", [])
        if section_name(section["name"]) == "text"
    )
    data_cursor = ROM_BASE + text_size
    total_rom_size = text_size + sum(
        len(base64.b64decode(section["data"]))
        for _, obj in objects
        for section in obj.get("sections", [])
        if section_name(section["name"]) == "data"
    )
    if total_rom_size > ROM_LENGTH:
        raise ValueError(
            f"text/data sections require 0x{total_rom_size:X} bytes, "
            f"exceeding the BB.CMD ROM region (0x{ROM_LENGTH:X})"
        )

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
            name = symbol["name"].lower()
            address = None
            if section in section_addresses:
                address = section_addresses[section] + symbol.get("section_offset", 0)
            elif symbol.get("section") == "" and symbol.get("kind") == "symbol":
                address = symbol.get("address", 0)
            if address is None:
                continue

            previous_kind = symbol_kinds.get(name)
            if previous_kind == "absolute" and section in ("text", "data"):
                continue
            if previous_kind is None or section in ("text", "data"):
                symbols[name] = address
                symbol_kinds[name] = "absolute" if section == "" else "section"
            elif section == "" and previous_kind == "section":
                symbols[name] = address
                symbol_kinds[name] = "absolute"

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
    implicit_local_labels = {"c", "e", "z"}
    short_relative_count = 0
    # Apply the short relative fixups emitted in object format.
    for path, obj, addresses in placements:
        section_addresses = addresses
        for fixup in obj.get("fixups", []):
            section = section_name(fixup["section"])
            if section not in ("text", "data") or section not in section_addresses:
                continue
            try:
                target = resolve_expression(fixup["symbol"], symbols)
            except ValueError:
                if fixup["symbol"].lower() not in implicit_local_labels:
                    unresolved_fixups.append((path, fixup["symbol"]))
                continue
            source = section_addresses[section] + fixup["offset"]
            if fixup.get("kind") in ("absolute16", "absolute32"):
                width = 2 if fixup["kind"] == "absolute16" else 4
                start = source - ROM_BASE
                if start < 0 or start + width > len(rom):
                    raise ValueError(f"{path}: absolute fixup is outside the ROM image")
                if width == 2:
                    rom[start:start + 2] = struct.pack("<H", target & 0xFFFF)
                else:
                    if target < 0 or target > 0xFFFFFFFF:
                        raise ValueError(
                            f"{path}: absolute fixup {fixup['symbol']!r} resolved to out-of-range address 0x{target:X}"
                        )
                    rom[start:start + 4] = struct.pack("<I", target)
                continue
            relative = (target - (source + 2)) // 2
            if relative < -128 or relative > 127:
                raise ValueError(
                    f"{path}: relative relocation {fixup['symbol']!r} at 0x{source:X} "
                    f"is out of range ({relative} words; expected -128..127)"
                )
            short_relative_count += 1
            start = source - ROM_BASE
            if start < 0 or start + 2 > len(rom):
                raise ValueError(f"{path}: fixup is outside the ROM image")
            rom[start + 1] = relative & 0xFF

    if unresolved_fixups:
        unique = sorted({f"{path.name}: {symbol}" for path, symbol in unresolved_fixups})
        raise ValueError("Unresolved relocations:\n  " + "\n  ".join(unique))

    print(f"Relocation classification: {short_relative_count} short relative")

    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(rom)
    print(f"Wrote {output} ({len(rom)} bytes)")


if __name__ == "__main__":
    main()

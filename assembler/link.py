#!/usr/bin/env python3
"""Link experimental TMS34010 JSON objects into a flat ROM image."""

import argparse
import base64
import json
import ast
import re
import struct
from pathlib import Path

ROM_BASE = 0xFF800000  # TMS34010 bit address, as used by BB.CMD
ROM_LENGTH_BITS = 0x7F6F00
ROM_LENGTH_BYTES = ROM_LENGTH_BITS // 8
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


def rom_region(cmd_path: Path | None):
    if not cmd_path:
        return ROM_BASE, ROM_LENGTH_BITS
    text = cmd_path.read_text(errors="replace")
    match = re.search(
        r"\bROM\s*:\s*org\s*=\s*(0x[0-9A-Fa-f]+|[0-9A-Fa-f]+)\s*,\s*len\s*=\s*(0x[0-9A-Fa-f]+|[0-9A-Fa-f]+)",
        text,
        re.IGNORECASE,
    )
    if not match:
        raise ValueError(f"{cmd_path}: could not find the ROM memory region")
    return tuple(int(value, 0) if value.lower().startswith("0x") else int(value, 16) for value in match.groups())


def write_tms34010_coff(path: Path, text: bytes, data: bytes, rom_base: int, entry: int):
    """Write a linked TMS34010 COFF image for the legacy bigsrec converter."""
    section_specs = [
        ("VECTORS", 0xFFFFFC00, b"", 0x40),
        ("IMAGES", 0x02000000, b"", 0x0),
        ("shit", 0xFFFF6F00, b"", 0x0),
        ("SHIT", 0xFFFF6F00, b"", 0x0),
        ("STACK", 0x01000000, b"", 0x40),
        ("PRCBSS", 0x01002000, b"", 0x40),
        ("OBJBSS", 0x01002040, b"", 0x40),
        (".BSS", 0x010020A0, b"", 0x0),
        (".bss", 0x010020A0, b"", 0x80),
        (".text", rom_base, text, 0x920),
        (".TEXT", rom_base, b"", 0x0),
        (".data", rom_base + len(text) * 8, data, 0x40),
        (".DATA", rom_base, b"", 0x0),
        ("img_tbl", rom_base, b"", 0x0),
    ]
    section_sizes = {
        "VECTORS": 0x400, "STACK": 0x2000, "PRCBSS": 0x40,
        "OBJBSS": 0x60, ".bss": 0x13E1F0,
    }
    header_size = 20 + 28 + len(section_specs) * 40
    text_pointer = header_size
    data_pointer = text_pointer + len(text)
    out = bytearray(struct.pack("<HHIIIHH", 0x0090, len(section_specs), 0, 0, 0, 28, 0x011F))
    out.extend(struct.pack("<HHIIIIII", 0x0108, 0x0262, len(text) * 8, len(data) * 8, 0, entry, rom_base, rom_base + len(text) * 8))
    for name, address, payload, flags in section_specs:
        pointer = text_pointer if name == ".text" and payload else data_pointer if name == ".data" and payload else 0
        out.extend(struct.pack(
            "<8sIIIIIIHHI",
            name.encode("ascii")[:8].ljust(8, b"\0"), address, address,
            section_sizes.get(name, len(payload) * 8), pointer, 0, 0, 0, 0, flags,
        ))
    out.extend(text)
    out.extend(data)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(out)


def section_name(name: str) -> str:
    lowered = name.lower()
    if lowered == "text" or lowered == ".text":
        return "text"
    if lowered == "data" or lowered == ".data":
        return "data"
    return lowered


def compare_layout(objects, map_path: Path):
    map_text = map_path.read_text(errors="replace")
    totals = {}
    inputs = {}
    for line in map_text.splitlines():
        fields = line.split()
        if len(fields) >= 4 and fields[0] in (".text", ".data", ".bss") and fields[1].isdigit():
            totals[fields[0]] = int(fields[3], 16)
        input_row = re.match(
            r"^\s+[0-9a-f]{8}\s+([0-9a-f]{8})\s+([^\s]+\.OBJ)\s+\(([^)]+)\)", line, re.I
        )
        if input_row:
            inputs[(input_row.group(2).upper(), input_row.group(3).lower())] = int(input_row.group(1), 16)

    actual = {}
    for path, obj in objects:
        for section in obj.get("sections", []):
            name = section_name(section["name"])
            if name in ("text", "data"):
                actual[(path.name.upper(), f".{name}")] = len(base64.b64decode(section["data"])) * 8
    for name in (".text", ".data"):
        expected = totals.get(name, 0)
        observed = sum(size for (obj_name, section), size in actual.items() if section == name)
        print(f"MAP {name}: legacy=0x{expected:X} bits, objects=0x{observed:X} bits, delta={observed - expected:+d}")
        mismatches = [
            (obj_name, size, actual.get((obj_name, name)))
            for (obj_name, section), size in inputs.items() if section == name
            if actual.get((obj_name, name)) != size
        ]
        print(f"  per-object size mismatches: {len(mismatches)} / {sum(1 for _, section in inputs if section == name)}")
        for obj_name, expected_size, observed_size in mismatches[:12]:
            print(f"  {obj_name}: map=0x{expected_size:X}, object={None if observed_size is None else f'0x{observed_size:X}'}")
    bss_objects = sum(1 for _, obj in objects if any(section_name(s["name"]) == "bss" for s in obj.get("sections", [])))
    print(f"MAP .bss: legacy=0x{totals.get('.bss', 0):X} bits; object files with BSS payload metadata={bss_objects}/{len(objects)} (allocation sizes remain unrepresented)")


def compare_coff(path: Path, legacy_path: Path):
    def sections(file_path):
        data = file_path.read_bytes()
        count = struct.unpack_from("<H", data, 2)[0]
        return {
            data[48 + index * 40:56 + index * 40].split(b"\0", 1)[0].decode("ascii", errors="replace"):
            (struct.unpack_from("<I", data, 56 + index * 40)[0], struct.unpack_from("<I", data, 64 + index * 40)[0])
            for index in range(count)
        }
    expected, actual = sections(legacy_path), sections(path)
    mismatches = [(name, expected.get(name), actual.get(name)) for name in expected if expected.get(name) != actual.get(name)]
    print(f"COFF sections: generated={len(actual)}, legacy={len(expected)}, name/address/size mismatches={len(mismatches)}")
    for name, wanted, got in mismatches:
        print(f"  {name}: legacy={wanted}, generated={got}")


def resolve_expression(expression: str, symbols: dict[str, int], signed_constants: set[str] | None = None):
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
        def symbol_value(match):
            name = match.group(1).lower()
            value = symbols[name]
            if signed_constants and name in signed_constants and value >= 0x80000000:
                value -= 0x100000000
            return str(value)
        text = symbol_pattern.sub(symbol_value, text)
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
        if isinstance(node, ast.UnaryOp):
            value = evaluate(node.operand)
            if isinstance(node.op, ast.UAdd): return value
            if isinstance(node.op, ast.USub): return -value
            if isinstance(node.op, ast.Invert): return ~value
        if isinstance(node, ast.BinOp):
            left, right = evaluate(node.left), evaluate(node.right)
            if isinstance(node.op, ast.Add): return left + right
            if isinstance(node.op, ast.Sub): return left - right
            if isinstance(node.op, ast.Mult): return left * right
            if isinstance(node.op, ast.Div): return int(left / right)
            if isinstance(node.op, ast.FloorDiv): return left // right
            if isinstance(node.op, ast.BitOr): return left | right
            if isinstance(node.op, ast.BitAnd): return left & right
            if isinstance(node.op, ast.BitXor): return left ^ right
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
    parser.add_argument("--coff-output", type=Path)
    parser.add_argument("--allow-unresolved", action="store_true", help="write provisional images with unresolved/out-of-range fixups left unchanged")
    parser.add_argument("--legacy-map", type=Path, help="compare object section sizes with a legacy linker map")
    parser.add_argument("--legacy-coff", type=Path, help="compare generated COFF section headers with a legacy BB.OUT")
    args = parser.parse_args()

    objects = load_objects(args.objects)
    if args.cmd_file:
        objects = order_from_cmd(objects, args.cmd_file)
    if args.legacy_map:
        compare_layout(objects, args.legacy_map)
    rom_base, rom_length_bits = rom_region(args.cmd_file)
    if rom_base % 8 or rom_length_bits % 8:
        raise ValueError("ROM base and length must be byte aligned")
    rom_length_bytes = rom_length_bits // 8
    text_cursor = rom_base
    symbols = {}
    symbol_kinds = {}
    placements = []

    text_size = sum(
        len(base64.b64decode(section["data"]))
        for _, obj in objects
        for section in obj.get("sections", [])
        if section_name(section["name"]) == "text"
    )
    data_cursor = rom_base + text_size * 8
    total_rom_size = text_size + sum(
        len(base64.b64decode(section["data"]))
        for _, obj in objects
        for section in obj.get("sections", [])
        if section_name(section["name"]) == "data"
    )
    if total_rom_size * 8 > rom_length_bits:
        raise ValueError(
            f"text/data sections require 0x{total_rom_size * 8:X} bits, "
            f"exceeding the BB.CMD ROM region (0x{rom_length_bits:X} bits)"
        )

    for path, obj in objects:
        section_addresses = {}
        for section in obj.get("sections", []):
            name = section_name(section["name"])
            data = base64.b64decode(section["data"])
            if name == "text":
                section_addresses[name] = text_cursor
                text_cursor += len(data) * 8
            elif name == "data":
                section_addresses[name] = data_cursor
                data_cursor += len(data) * 8
            else:
                if data:
                    raise ValueError(f"{path}: initialized section {section['name']!r} has no BB.CMD placement")
                section_addresses[name] = 0

        placements.append((path, obj, section_addresses))
        for symbol in obj.get("symbols", []):
            section = section_name(symbol.get("section", "")) if symbol.get("section") else ""
            name = symbol["name"].lower()
            address = None
            if section in section_addresses:
                address = section_addresses[section] + symbol.get("section_offset", 0) * 8
            elif symbol.get("section") == "" and symbol.get("kind") == "symbol":
                address = symbol.get("address", 0)
            if address is None:
                continue

            previous_kind = symbol_kinds.get(name)
            if section == "" and address == 0 and previous_kind == "section":
                continue
            if previous_kind == "absolute" and section in ("text", "data"):
                continue
            if previous_kind == "external" and section in ("text", "data"):
                symbols[name] = address
                symbol_kinds[name] = "section"
                continue
            if previous_kind is None or section in ("text", "data"):
                symbols[name] = address
                symbol_kinds[name] = (
                    "external" if section == "" and address == 0
                    else "absolute" if section == ""
                    else "section"
                )
            elif section == "" and previous_kind == "section":
                symbols[name] = address
                symbol_kinds[name] = "absolute"

    rom = bytearray([FILL]) * rom_length_bytes

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
            if (address - rom_base) % 8:
                raise ValueError(f"{path}: section {name} is not byte aligned")
            start = (address - rom_base) // 8
            if start < 0 or start + len(data) > len(rom):
                raise ValueError(f"{path}: section {name} is outside the ROM image")
            rom[start:start + len(data)] = data

    unresolved_fixups = []
    implicit_local_labels = {"c", "e", "z"}
    short_relative_count = 0
    # Apply the short relative fixups emitted in object format.
    for path, obj, addresses in placements:
        section_addresses = addresses
        object_symbols = dict(symbols)
        signed_constants = {
            symbol["name"].lower()
            for symbol in obj.get("symbols", [])
            if not symbol.get("section")
            and symbol.get("kind") == "symbol"
            and symbol.get("address", 0) >= 0x80000000
        }
        for symbol in obj.get("symbols", []):
            section_name_value = section_name(symbol.get("section", "")) if symbol.get("section") else ""
            if section_name_value in section_addresses:
                value = section_addresses[section_name_value] + symbol.get("section_offset", 0) * 8
            elif not symbol.get("section") and symbol.get("kind") == "symbol":
                value = symbol.get("address", 0)
                # .ref/.global declarations are represented as zero-valued,
                # sectionless symbols. Keep an already-defined global target
                # instead of letting this object's unresolved placeholder hide it.
                if value == 0 and symbol["name"].lower() in symbols:
                    continue
            else:
                continue
            object_symbols[symbol["name"].lower()] = value
        for fixup in obj.get("fixups", []):
            section = section_name(fixup["section"])
            if section not in ("text", "data") or section not in section_addresses:
                continue
            try:
                target = resolve_expression(fixup["symbol"], object_symbols, signed_constants)
            except ZeroDivisionError as error:
                if args.allow_unresolved:
                    unresolved_fixups.append((path, fixup["symbol"]))
                    continue
                raise ValueError(
                    f"{path}: division by zero in relocation expression {fixup['symbol']!r}"
                ) from error
            except ValueError:
                if fixup["symbol"].lower() not in implicit_local_labels or args.allow_unresolved:
                    unresolved_fixups.append((path, fixup["symbol"]))
                continue
            source = section_addresses[section] + fixup["offset"] * 8
            if fixup.get("kind") in ("absolute16", "absolute32"):
                width = 2 if fixup["kind"] == "absolute16" else 4
                start = (source - rom_base) // 8
                if start < 0 or start + width > len(rom):
                    raise ValueError(f"{path}: absolute fixup is outside the ROM image")
                if width == 2:
                    rom[start:start + 2] = struct.pack("<H", target & 0xFFFF)
                else:
                    if target < -0x80000000 or target > 0xFFFFFFFF:
                        if args.allow_unresolved:
                            unresolved_fixups.append((path, fixup["symbol"]))
                            continue
                        raise ValueError(
                            f"{path}: absolute fixup {fixup['symbol']!r} resolved to out-of-range address 0x{target:X}"
                        )
                    rom[start:start + 4] = struct.pack("<I", target & 0xFFFFFFFF)
                continue
            if fixup.get("kind") == "tms34010-relative16":
                relative = (target - (source + 16)) // 16
                if relative < -32768 or relative > 32767:
                    if args.allow_unresolved:
                        unresolved_fixups.append((path, fixup["symbol"]))
                        continue
                    raise ValueError(
                        f"{path}: long relative relocation {fixup['symbol']!r} at 0x{source:X} "
                        f"is out of range ({relative} words; expected -32768..32767)"
                    )
                start = (source - rom_base) // 8
                if start < 0 or start + 2 > len(rom):
                    raise ValueError(f"{path}: long relative fixup is outside the ROM image")
                rom[start:start + 2] = struct.pack("<h", relative)
                continue
            relative = (target - (source + 16)) // 16
            if relative < -128 or relative > 127:
                if args.allow_unresolved:
                    unresolved_fixups.append((path, fixup["symbol"]))
                    continue
                raise ValueError(
                    f"{path}: relative relocation {fixup['symbol']!r} at 0x{source:X} "
                    f"is out of range ({relative} words; expected -128..127)"
                )
            short_relative_count += 1
            start = (source - rom_base) // 8
            if start < 0 or start + 2 > len(rom):
                raise ValueError(f"{path}: fixup is outside the ROM image")
            rom[start + 1] = relative & 0xFF

    if unresolved_fixups:
        unique = sorted({f"{path.name}: {symbol}" for path, symbol in unresolved_fixups})
        if not args.allow_unresolved:
            raise ValueError("Unresolved relocations:\n  " + "\n  ".join(unique))
        print(f"PROVISIONAL: left {len(unique)} unresolved/out-of-range relocation sites unchanged")
        for item in unique[:25]:
            print(f"  {item}")
        if len(unique) > 25:
            print(f"  ... and {len(unique) - 25} more")

    print(f"Relocation classification: {short_relative_count} short relative")

    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_bytes(rom)
    print(f"Wrote {output} ({len(rom)} bytes)")
    if args.coff_output:
        linked_text = b"".join(
            base64.b64decode(section["data"])
            for _, obj in objects
            for section in obj.get("sections", [])
            if section_name(section["name"]) == "text"
        )
        linked_data = b"".join(
            base64.b64decode(section["data"])
            for _, obj in objects
            for section in obj.get("sections", [])
            if section_name(section["name"]) == "data"
        )
        write_tms34010_coff(
            args.coff_output.resolve(), linked_text, linked_data, rom_base, symbols.get("init_prog", 0)
        )
        print(f"Wrote linked TMS34010 COFF {args.coff_output.resolve()}")
        if args.legacy_coff:
            compare_coff(args.coff_output.resolve(), args.legacy_coff.resolve())


if __name__ == "__main__":
    main()

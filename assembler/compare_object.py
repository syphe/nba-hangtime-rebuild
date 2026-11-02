#!/usr/bin/env python3
"""Compare a native JSON object with a preserved TI COFF v0 object."""

import argparse
import base64
import json
import struct
from pathlib import Path


def compare(native_path: Path, legacy_path: Path):
    native = json.loads(native_path.read_text())
    if native.get("format") != "tms34010-object":
        raise ValueError(f"{native_path}: expected a native JSON object")
    legacy = legacy_path.read_bytes()
    magic, count, _, _, _, optional_size, _ = struct.unpack_from("<HHIIIHH", legacy)
    if magic != 0x90:
        raise ValueError(f"{legacy_path}: expected TI COFF v0 (0x90), got {magic:#x}")
    native_sections = {section["name"]: section for section in native["sections"]}
    result = []
    for index in range(count):
        fields = struct.unpack_from("<8sIIIIIIHHH", legacy, 20 + optional_size + 40 * index)
        raw_name, _, _, size_bits, pointer, reloc_pointer, _, reloc_count, _, flags = fields
        name = raw_name.rstrip(b"\0").decode("ascii")
        if name not in (".text", ".data"):
            continue
        if size_bits % 8:
            raise ValueError(f"{legacy_path}: {name} has a non-byte-aligned size")
        size = size_bits // 8
        expected = legacy[pointer:pointer + size]
        if len(expected) != size:
            raise ValueError(f"{legacy_path}: truncated {name} payload")
        section = native_sections.get(name)
        actual = base64.b64decode(section["data"]) if section else b""
        masked = set()
        legacy_offsets = []
        unsupported_types = set()
        for entry in range(reloc_count):
            address_bits, _, kind = struct.unpack_from("<IIH", legacy, reloc_pointer + 10 * entry)
            if kind != 0x11:  # TI R_RELLONG: relocated 32-bit field
                unsupported_types.add(kind)
                continue
            if address_bits % 8:
                raise ValueError(f"{legacy_path}: unaligned {name} relocation")
            offset = address_bits // 8
            legacy_offsets.append(offset)
            masked.update(range(offset, offset + 4))
        fixups = [fixup for fixup in native["fixups"] if fixup["section"] == name]
        native_offsets = [fixup["offset"] for fixup in fixups]
        differences = [offset for offset, (a, b) in enumerate(zip(actual, expected))
                       if offset not in masked and a != b]
        supported = not unsupported_types
        result.append({
            "section": name,
            "legacy_bytes": size,
            "native_bytes": len(actual),
            "size_delta_bytes": len(actual) - size,
            "legacy_relocations": reloc_count,
            "native_fixups": len(fixups),
            "unsupported_legacy_relocation_types": sorted(unsupported_types),
            "relocation_sites_match": supported and sorted(native_offsets) == sorted(legacy_offsets)
                and all(fixup["kind"] == "absolute32" for fixup in fixups),
            # These byte comparisons are meaningful only when the sections have
            # corresponding layouts. They do not compare linked target values.
            "non_relocated_common_byte_differences": len(differences) if supported else None,
            "first_non_relocated_difference": differences[0] if differences and supported else None,
            "size_and_non_relocated_bytes_match": supported and len(actual) == size and not differences,
        })
    return {"native": str(native_path), "legacy": str(legacy_path), "sections": result}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("native", type=Path)
    parser.add_argument("legacy", type=Path)
    parser.add_argument("--output", type=Path, help="also save the JSON diagnostic report")
    args = parser.parse_args()
    report = json.dumps(compare(args.native, args.legacy), indent=2)
    if args.output:
        args.output.write_text(report + "\n")
    print(report)


if __name__ == "__main__":
    main()

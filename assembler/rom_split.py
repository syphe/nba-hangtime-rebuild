#!/usr/bin/env python3
"""Split a byte-addressed TMS34010 ROM image into its even/odd program ROMs."""

import argparse
from pathlib import Path

ROM_BYTES = 0x100000
CHIP_BYTES = ROM_BYTES // 2
FILL = 0xFF


def split_program_rom(image: bytes):
    if len(image) > ROM_BYTES and any(byte != FILL for byte in image[ROM_BYTES:]):
        raise ValueError("initialized program data extends past the two 512 KiB ROMs")
    image = image[:ROM_BYTES].ljust(ROM_BYTES, bytes([FILL]))
    return image[0::2], image[1::2]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("image", type=Path)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--reference", type=Path)
    args = parser.parse_args()

    even, odd = split_program_rom(args.image.read_bytes())
    args.output_dir.mkdir(parents=True, exist_ok=True)
    even_path = args.output_dir / "l1.03_maximum_hangtime_u54_l_version.u54"
    odd_path = args.output_dir / "l1.03_maximum_hangtime_u63_l_version.u63"
    even_path.write_bytes(even)
    odd_path.write_bytes(odd)
    interleaved = bytes(value for pair in zip(even, odd) for value in pair)
    combined_path = args.output_dir / "ROM_MINE.bin"
    combined_path.write_bytes(interleaved)
    print(f"Wrote {even_path.name} and {odd_path.name} ({CHIP_BYTES} bytes each)")
    print(f"Wrote {combined_path} ({len(interleaved)} bytes)")

    if args.reference:
        reference = args.reference.read_bytes()
        if reference == interleaved:
            print(f"Byte comparison passed: {args.reference}")
        else:
            common = min(len(reference), len(interleaved))
            first = next((i for i in range(common) if reference[i] != interleaved[i]), common)
            differences = sum(a != b for a, b in zip(reference, interleaved)) + abs(len(reference) - len(interleaved))
            print(
                f"Byte comparison differs: {differences} bytes; first difference at 0x{first:X}; "
                f"reference={len(reference)} bytes, native={len(interleaved)} bytes"
            )


if __name__ == "__main__":
    main()

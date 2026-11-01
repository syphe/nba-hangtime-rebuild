# TMS34010 Rust Assembler - Project Handoff

## 1. Project Overview

A modern, native Rust assembler designed for the TMS34010 arcade graphics processor. The goal is to replace legacy DOSBox-based assembly pipelines with a fast, robust, modular tool capable of generating interleaved binary ROM images for physical arcade hardware and MAME.

---

## 2. Architecture & Design

- **2-Pass Assembly Pipeline**:
  - **Pass 1**: Parsing, symbol resolution, and building the symbol table (`.equ` and label locations).
  - **Pass 2**: Machine code generation and multi-word instruction encoding (little-endian output).
- **Parsing Layer**: Powered by `nom` combinators with a robust sanitization layer for handling comments and whitespace.
- **ROM Interleaving**: Built-in binary splitting utility to generate N-way Even/Odd ROM chip files.

---

## 3. Current Feature Set & Progress

### Instruction Set & Operands

- **Control Flow / Branching**: `NOP`, `JNE`, `CALL`, and target labels.
- **Arithmetic & Data Movement**: `MOVE`, `ADD`, `SUB`.
- **Shift Operations**: `SLL` (Shift Left Logical) and `SRL` (Shift Right Logical) supporting both immediate shift counts and register-based shift counts.
- **Data Directives**: `.WORD` supporting both hex (`$BEEF`) and decimal (`1337`) formats.
- **Number Parsing**: Robust handling of hexadecimal (`$`, `0x`) and decimal formats.

### Modularity & Build Pipeline

- **Recursive File Inclusion**: Full support for `.include "filename.asm"` directives to modularize code across separate files (e.g., separating hardware equates from game logic).
- **CLI & Output**: Automated file loading, verbose logging, and multi-chip ROM splitting.

---

## 4. Codebase Structure

- `src/main.rs`: CLI entry point, recursive file preprocessor (`load_source_file`), and pipeline execution orchestration.
- `src/parser.rs`: `nom`-based combinators for instructions, operands (immediates, registers, indirect), and directives.
- `src/encoder.rs`: Pass 2 machine code encoder handling bitfield packing, multi-word instructions, and little-endian serialization.

---

## 5. Current Status

- The Rust assembler and `build.ps1` object assembly sweep are complete for the Makefile module list.
- `build.ps1` currently produces the expected collection of 51 raw `.OBJ` binaries, but it does not yet link them or produce `BB.OUT`.
- Testing both Python entry points confirmed that the legacy DOSBox build scripts complete:
  - `build.py` creates the two 512 KiB program ROM halves and `ROM_MINE.bin`.
  - `buildgfx.py` creates `misc.bin` and the four graphics ROM images, and modifies the generated palette source.
- The legacy `BB.OUT` and `BB.MAP` are preserved in `BACKUP/XCODE101L` and are being used as reference outputs for linker development.
- Object generation now has a linker-development mode (`--object-format` / `build.ps1 -ObjectFormat`) that emits JSON objects with embedded machine code, per-section payloads, symbols, branch/call fixups including section-local offsets. A regenerated clean object sweep produced all 51 JSON objects.
- The first native experimental linker (`link.py`) now places text/data sections, applies short control-flow fixups, rejects unresolved relocations, and is invoked by `build.ps1 -ObjectFormat` in Makefile module order.
- `MMTM` label-on-instruction support was added so inline labels such as `ADJ_BOX MMTM ...` enter the symbol table.
- Hash-prefixed local labels (`#name`) are now recognized as inline definitions and stripped to their symbol name in object metadata. Rebuilding ATTRACT confirms `agn` and `nxta` are emitted as symbols.
- Final unresolved-symbol cleanup is complete: `ADD` inline-label recognition and A/B register operand filtering were added, and macro expansion now assigns unique names to legacy `?` local labels so macro bodies such as `BADCHK` do not emit false `bad` relocations.
- A clean rebuild of all 51 objects now completes and the experimental linker writes `build/BB.bin`; no unresolved symbols remain in the final seven-name set.
- Relocation classification is implemented in `link.py`: a clean link applies short relative relocations and reports out-of-range branches separately. `CALLA` targets now emit `absolute32` fixups (4,139 across the object set) and the linker writes them as 32-bit operands.
- Section allocation directives such as `.BSS name,size` are no longer mistaken for section switches, correcting `.text` symbol metadata and eliminating cross-module false addresses.
- The earlier standalone COFF-extraction experiment was removed because its section-offset assumptions were not reliable; legacy `BB.OUT` remains a reference only.

## 6. Next Steps

The next logical implementation step is relocation correctness in the modern program linker:

1. Extend PC-relative branch support for out-of-range relocations; first replace the current encoder clamp with explicit known-branch expansion and add equivalent linker fixups for the expanded instruction sequence.
2. Add explicit relocation kinds for address fixups emitted by `MOVI` and `.WORD` operands, then apply them in `link.py` (many may share the `absolute32` encoding).
3. Reconcile the experimental flat layout with `SRC/BB.CMD` and legacy section addresses/sizes.
4. Generate `BB.OUT` in a format compatible with the legacy ROM conversion tools.
5. Add native ROM conversion/splitting to `assembler/build.ps1` and compare output with the DOSBox reference artifacts byte-for-byte.
6. Keep graphics replacement and GSP changes separate until the program-ROM linker passes byte comparison.

Graphics replacement remains separate and must not be included in the linker implementation commit.

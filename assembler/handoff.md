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
- Legacy `.set` directives and `.set`/`.equ` numeric constants are now recognized without treating inline instruction operands as constants; legacy `400h` values are supported.
- The Python linker now recognizes compound relocation expressions and preserves absolute constants separately from section symbols. Remaining expression work is to resolve symbolic `.equ` chains (for example `TM_X+189` and `ADJ_ULY-20`) rather than recording them as zero.
- Recent full-object validation reached linker expression diagnosis successfully; final clean rebuild/link validation remains in progress after expression-symbol fixes.
- Rust pass one now retains `.equ`/`.set` source expressions and reevaluates them after labels are collected, allowing forward symbol references and equate chains to be represented in the symbol table.
- The encoder now promotes supported unconditional, equal/not-equal, less-than, and greater-than relative branches to the TMS34010 two-word, signed 16-bit `JRcc` form when the short displacement is out of range. Pass one repeats label/equate layout until branch-size changes settle. Object files mark expanded branch operands as `tms34010-relative16`, and `link.py` resolves and range-checks those fixups. Relative calls still reject short-range overflow; remaining conditional mnemonics need condition-code preservation in the parser, and targets beyond the long relative range need `JAcc` support.
- Object generation emits `absolute16` fixups for `.WORD` expressions containing symbols; symbolic `MOVI` operands use the existing `absolute32` fixup path. The linker applies both widths.
- The experimental linker can now take `--cmd-file SRC/BB.CMD`; `build.ps1 -ObjectFormat` passes that file so legacy-listed modules are placed in command-file order, with additional modules appended afterward. It also rejects text/data payloads larger than the ROM region defined by `BB.CMD` (origin `0xFF800000`, length `0x7F6F00`). The legacy map confirms separate packed `.text` then `.data` ranges; byte-for-byte layout reconciliation has not yet been completed.
- The parser now treats the legacy `JSRP target` pseudo-instruction as `CALLR target`, including both opcode-detection paths used for labels. Object generation therefore records its relocation as a signed 16-bit relative call instead of the unsupported short-relative form that previously stopped linking at `FRANIMQ`.
- `link.py` now gives each object's own local symbols precedence over same-named symbols from other modules during relocation expression evaluation, and accepts negative absolute 32-bit expressions as two's-complement values.
- The toolchain now includes a TI COFF `BB.OUT` writer and a native program-ROM split/combine utility. `build.ps1 -ObjectFormat` invokes both after linking and compares the combined ROM with `BACKUP/XCODE101L/ROM/ROM_MINE.bin`. These outputs have not yet been validated by a completed end-to-end run in this session.
- A rebuild after the `JSRP` parser change started recompiling the module set but did not complete within the command window. Subsequent `cargo build` attempts were blocked because Windows returned “Access is denied” while replacing `target/debug/tms34010-asm.exe`; no linker or ROM comparison result was produced from that run.

## 6. Next Steps

The next implementation steps are:

1. Complete a clean full-object build and link; verify `JSRP` relocations and fix any newly exposed relocation or expression failures.
2. Validate linker section addresses and sizes against all entries in `SRC/BB.CMD` and `BACKUP/XCODE101L/BB.MAP`, including initialized vectors and uninitialized memory sections. The current object format does not yet carry enough allocation metadata to claim BSS/vector correctness.
3. Validate generated `BB.OUT` using the legacy ROM conversion tools or a known-good COFF reader, then compare the native combined ROM with the DOSBox reference byte-for-byte and investigate mismatches.
4. Keep graphics replacement and GSP changes separate until the program-ROM linker passes byte comparison.

Graphics replacement remains separate and must not be included in the linker implementation commit.

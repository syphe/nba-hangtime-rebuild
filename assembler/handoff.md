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
- `JSRP` is a source macro, not a relative call. It expands through `JAUC`, but the assembler's former synthetic `JAUC` macro incorrectly reduced that absolute jump to a short relative `JMP`. Removed that synthetic macro and added `JAUC target` parsing as an unconditional absolute jump, so `JSRP FRANIMQ` now emits a six-byte absolute jump and an `absolute32` relocation.
- Branch relaxation now measures instruction sizes using the encoder's emitted byte lengths, keeping label offsets in sync with object section offsets. This promotes `PLYR2`'s `JRUC do_scroll` to a signed 16-bit relative branch.
- Cross-object calls and branches now reserve a six-byte absolute transfer in object files, avoiding link-time range failures when an external target is too far for relative encoding. `PLYR2`'s calls to `seekdir_xyxy128` are emitted as `absolute32` fixups.
- Numeric `.asg` aliases are recorded with their value instead of zero. A clean build now confirms `UNZIP.OBJ` contains `IMGBPP=8` and the `RAMBUFSIZ/IMGBPP/4` expression no longer divides by zero. Linker expression evaluation reports division-by-zero relocations with the object and source expression.
- Inline label recognition handles the legacy `nop dsj ...` / `line1 line 1` pattern in `AUDIT` and `DIAG`; regenerated objects now contain the `line1` label and its branch is promoted to `relative16`.
- `link.py` now gives each object's own local symbols precedence over same-named symbols from other modules during relocation expression evaluation, and accepts negative absolute 32-bit expressions as two's-complement values.
- The toolchain now includes a TI COFF `BB.OUT` writer and a native program-ROM split/combine utility. `build.ps1 -ObjectFormat` invokes both after linking and compares the combined ROM with `BACKUP/XCODE101L/ROM/ROM_MINE.bin`. These outputs have not yet been validated by a completed end-to-end run in this session.
- Linker symbol collection no longer lets a zero-valued, sectionless external placeholder overwrite a real definition from another object; the same guard applies when assembling each object's local relocation context. This was required because the `SUCIDE` external placeholder in `BB.OBJ` had hidden its definition in `MPROC.OBJ`.
- The clean build and link pass the `FRANIMQ`, `do_scroll`, cross-object `seekdir_xyxy128`, `SCORE` division expression, `UNZIP` alias, and `AUDIT`/`DIAG` local-label issues. The latest link reaches unresolved-relocation reporting across many objects. Remaining issues include expression operators such as unary negative values (now supported by the linker) and comma-packed `.WORD` lists being misclassified as a single symbolic expression, plus existing unresolved references. COFF and ROM comparison still await a successful link.
- Added diagnostic `--allow-unresolved`, `--legacy-map`, and `--legacy-coff` modes to `link.py`. The provisional mode leaves unresolved or out-of-range fixups unchanged, writes explicitly named provisional ROM/COFF outputs, compares per-object payload sizes with `BB.MAP`, and compares generated COFF section headers with legacy `BB.OUT`.
- The provisional layout report shows object `.text` totals are 0x1819D0 bits versus 0x3B39C0 in the legacy map (short by 0x231FF0 bits / 287,742 bytes). `.data` totals are 0x80670 bits versus 0x3C50E0 (short by 0x344A70 bits / 428,366 bytes). All 45 mapped `.text` object contributions and all 31 `.data` contributions differ; `IMGTBL.OBJ` has no `.data` payload in the current object set. Object files still provide no BSS allocation sizes; legacy `.bss` is 0x13E1F0 bits.
- Provisional `BB.OUT` has all 14 expected COFF sections, with 12 section names/addresses/sizes matching legacy `BB.OUT`. `.text` and `.data` header addresses match, but their sizes are smaller by the same payload deficits above. The provisional combined ROM is 1,048,576 bytes and differs from the reference at 923,301 bytes (first difference at byte 0); this comparison is diagnostic only because the provisional image has 17,492 unresolved/out-of-range relocation sites left unchanged.
- Diagnostics for handoff steps 2 and 3 are complete: they quantify the section payload mismatch and show that the COFF container layout is mostly correct while its program sections are incomplete. Final section/BSS/vector validation and final COFF/ROM validation remain blocked on correct assembly payloads and a fully resolved link.

## 6. Next Steps

The next implementation steps are:

1. Correct `.WORD` fixup generation and data parsing so numeric comma-separated tables are emitted with their full payload sizes; then resolve genuine external references and rerun the linker.
2. Add BSS/vector allocation metadata to objects and compare all initialized and uninitialized section ranges with `BB.CMD` and `BB.MAP`.
3. After a clean link, validate final `BB.OUT` with the legacy ROM conversion tools and compare the native combined ROM byte-for-byte. Provisional comparison currently shows a 923,301-byte mismatch and must not be treated as final validation.
4. Keep graphics replacement and GSP changes separate until the program-ROM linker passes byte comparison.

Graphics replacement remains separate and must not be included in the linker implementation commit.

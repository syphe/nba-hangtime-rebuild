use std::fs;
use std::path::Path;

use crate::encoder;
use crate::parser::ParsedInstruction;
use crate::SymbolTable;

/// Re-read the generated ROM image and verify that its contents match Pass 2 encoding.
pub fn verify_rom<P: AsRef<Path>>(
    bin_path: P,
    ast: &[ParsedInstruction],
    symbols: &SymbolTable,
) -> Result<(), String> {
    let bytes =
        fs::read(bin_path).map_err(|error| format!("failed to read output binary: {}", error))?;

    println!(
        "\n[Verifier] Starting round-trip verification of output binary ({} bytes)...",
        bytes.len()
    );

    let mut pc = 0usize;
    for (line_idx, instr) in ast.iter().enumerate() {
        let encoded = encoder::encode_instruction(instr, pc as u32, symbols).map_err(|error| {
            format!(
                "failed to encode instruction #{} {:?}: {:?}",
                line_idx, instr, error
            )
        })?;

        let end = pc
            .checked_add(encoded.len())
            .ok_or_else(|| format!("program counter overflow at instruction #{}", line_idx))?;
        if end > bytes.len() {
            return Err(format!(
                "out of bounds at instruction #{}: {:?} (PC: {:04X}, needed {} bytes, available {})",
                line_idx,
                instr,
                pc,
                encoded.len(),
                bytes.len().saturating_sub(pc)
            ));
        }
        if bytes[pc..end] != encoded {
            return Err(format!(
                "encoded output differs at PC {:04X} for instruction {:?}",
                pc, instr
            ));
        }

        pc = end;
    }

    if pc != bytes.len() {
        return Err(format!(
            "output length differs from AST encoding: encoded {} bytes, file contains {} bytes",
            pc,
            bytes.len()
        ));
    }

    println!("[Verifier] Verification passed successfully! Output bytes match Pass 2 encoding.");
    Ok(())
}

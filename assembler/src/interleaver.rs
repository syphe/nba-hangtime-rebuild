use std::fs;
use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum InterleaveError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Splits a raw binary buffer into N-way interleaved ROM chunks.
/// For a 16-bit arcade bus, `ways = 2` creates Even (ROM 0) and Odd (ROM 1) splits.
pub fn split_binary(data: &[u8], ways: usize) -> Vec<Vec<u8>> {
    let mut chunks = vec![Vec::new(); ways];

    for (i, &byte) in data.iter().enumerate() {
        let target_bank = i % ways;
        chunks[target_bank].push(byte);
    }

    chunks
}

/// Helper function to write split ROM chunks to disk with custom suffixes
pub fn write_split_roms(base_path: &Path, data: &[u8], ways: usize) -> Result<(), InterleaveError> {
    let chunks = split_binary(data, ways);
    let stem = base_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("rom");
    let parent = base_path.parent().unwrap_or(Path::new("."));

    for (bank_idx, chunk) in chunks.iter().enumerate() {
        // Name files like output_even.bin / output_odd.bin (or bank0.bin, bank1.bin)
        let suffix = match ways {
            2 if bank_idx == 0 => "even",
            2 => "odd",
            _ => &format!("bank{}", bank_idx),
        };

        let file_name = format!("{}_{}.bin", stem, suffix);
        let out_path = parent.join(file_name);

        fs::write(&out_path, chunk)?;
        println!(
            "Generated ROM chip file: {:?} ({} bytes)",
            out_path,
            chunk.len()
        );
    }

    Ok(())
}

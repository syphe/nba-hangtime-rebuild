use crate::parser::{ParsedInstruction, Operand, Register};
use crate::SymbolTable;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum EncodeError {
    #[error("Unsupported instruction or addressing mode")]
    UnsupportedInstruction,
    #[error("Invalid directive argument: {0}")]
    InvalidDirectiveArg(String),
    #[error("Undefined label referenced: {0}")]
    UndefinedLabel(String),
    #[error("Undefined symbol referenced: {0}")]
    UndefinedSymbol(String),
    #[error("Invalid operand for instruction")]
    InvalidOperand,
    #[error("Branch offset {offset} words out of 9-bit signed range (-256..255) for label '{label}' at {addr:04X}")]
    BranchOutOfRange { label: String, addr: u32, offset: i32 },
}

pub fn encode_register(reg: &Register) -> u8 {
    match reg {
        Register::A(num) => *num,
        Register::B(num) => 16 + *num,
    }
}

/// Encode a single instruction given its current memory address and the symbol table
pub fn encode_instruction(
    instr: &ParsedInstruction,
    current_addr: u32,
    symbols: &SymbolTable,
) -> Result<Vec<u8>, EncodeError> {
    let mut bytes = Vec::new();

    match instr {
        ParsedInstruction::Nop => {
            bytes.extend_from_slice(&0x0000u16.to_le_bytes());
        }
        ParsedInstruction::Equ { name, value } => {
            // Equates are handled in pass one / symbol table, 
            // but if reached here during encode, no code bytes generated.
        }
        ParsedInstruction::Label(_label) => {
            // Labels generate no bytes
        }
        ParsedInstruction::Usect { .. } => {
            // .usect allocates uninitialized RAM space; no bytes emitted into ROM
        }
        ParsedInstruction::Directive { name, args } => {
            if name.eq_ignore_ascii_case(".WORD") {
                for arg in args {
                    let val: Result<i32, EncodeError> = if let Some(hex_str) = arg.strip_prefix('$') {
                        u16::from_str_radix(hex_str, 16)
                            .map(|v| v as i32)
                            .map_err(|_| EncodeError::InvalidDirectiveArg(arg.clone()))
                    } else if let Some(hex_str) = arg.strip_prefix("0x") {
                        u16::from_str_radix(hex_str, 16)
                            .map(|v| v as i32)
                            .map_err(|_| EncodeError::InvalidDirectiveArg(arg.clone()))
                    } else if let Ok(n) = arg.parse::<i32>() {
                        Ok(n)
                    } else {
                        // Try resolving as a symbol equate or label
                        symbols.get(arg)
                            .copied()
                            .map(|v| v as i32)
                            .ok_or_else(|| EncodeError::UndefinedSymbol(arg.clone()))
                    };

                    match val {
                        Ok(v) => bytes.extend_from_slice(&(v as u16).to_le_bytes()),
                        Err(e) => return Err(e),
                    }
                }
            }
        }
        ParsedInstruction::Jr { label } 
        | ParsedInstruction::Call { label } 
        | ParsedInstruction::Jeq { label } 
        | ParsedInstruction::Jne { label } => {
            let target_addr = symbols.get(label)
                .ok_or_else(|| EncodeError::UndefinedLabel(label.clone()))?;

            let pc_next = current_addr + 2;
            let offset_bytes = (*target_addr as i32) - (pc_next as i32);
            let word_offset = (offset_bytes / 2) as i16;

            // The offset field is 8 bits wide, so the signed range is -128..127
            // words. Out-of-range offsets must be rejected, not silently truncated.
            if word_offset < -128 || word_offset > 127 {
                return Err(EncodeError::BranchOutOfRange {
                    label: label.clone(),
                    addr: current_addr,
                    offset: word_offset as i32,
                });
            }

            let mut opcode: u16 = match instr {
                ParsedInstruction::Jr { .. } => 0x6F00,
                ParsedInstruction::Call { .. } => 0xEF00,
                ParsedInstruction::Jeq { .. } => 0x6700,
                ParsedInstruction::Jne { .. } => 0x6800,
                _ => unreachable!(),
            };

            opcode |= (word_offset as u16) & 0x00FF;
            bytes.extend_from_slice(&opcode.to_le_bytes());
        }
        ParsedInstruction::Move { src, dst } => {
            match (src, dst) {
                // Existing: Immediate / Symbol -> Reg (#val, #symbol)
                (src_op, Operand::Reg(d_reg)) if matches!(src_op, Operand::Immediate(_) | Operand::Symbol(_)) => {
                    let val = resolve_immediate(src_op, symbols)?;
                    let mut opcode: u16 = 0x4400; 
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= d_val & 0x0F;
                    
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(val as u16).to_le_bytes());
                }

                // Existing: Register -> Register (Rn, Rm)
                (Operand::Reg(s_reg), Operand::Reg(d_reg)) => {
                    let mut opcode: u16 = 0x2000;
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 4;
                    opcode |= d_val & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }

                // NEW: Simple Register Indirect (*Rn -> Rm)
                (Operand::Indirect(s_reg), Operand::Reg(d_reg)) => {
                    let mut opcode: u16 = 0x2100; // Base opcode for indirect load
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 4;
                    opcode |= d_val & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }

                // NEW: Indirect Post-Increment (*Rn+ -> Rm)
                (Operand::IndirectPostInc(s_reg), Operand::Reg(d_reg)) => {
                    let mut opcode: u16 = 0x2200; // Base opcode for post-increment load
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 4;
                    opcode |= d_val & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }

                // NEW: Indirect Pre-Decrement (-*Rn -> Rm)
                (Operand::IndirectPreDec(s_reg), Operand::Reg(d_reg)) => {
                    let mut opcode: u16 = 0x2300; // Base opcode for pre-decrement load
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 4;
                    opcode |= d_val & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }

                // NEW: Direct Memory / Absolute (@label -> Rm)
                (Operand::DirectMemory(label), Operand::Reg(d_reg)) => {
                    // Resolve the label's address from your symbol table
                    let val = symbols.get(label).copied().ok_or_else(|| {
                        EncodeError::UndefinedSymbol(label.clone())
                    })?;

                    let mut opcode: u16 = 0x4500; // Base opcode for direct memory load
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= d_val & 0x0F;

                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(val as u16).to_le_bytes());
                }

                _ => return Err(EncodeError::UnsupportedInstruction),
            }
        }
        ParsedInstruction::Add { src, dst } 
        | ParsedInstruction::Sub { src, dst }
        | ParsedInstruction::And { src, dst }
        | ParsedInstruction::Or { src, dst }
        | ParsedInstruction::Xor { src, dst } => {
            let instr_type = match instr {
                ParsedInstruction::Add { .. } => 0,
                ParsedInstruction::Sub { .. } => 1,
                ParsedInstruction::And { .. } => 2,
                ParsedInstruction::Or { .. } => 3,
                ParsedInstruction::Xor { .. } => 4,
                _ => unreachable!(),
            };

            match (src, dst) {
                (src_op, Operand::Reg(d_reg)) if matches!(src_op, Operand::Immediate(_) | Operand::Symbol(_)) => {
                    let val = resolve_immediate(src_op, symbols)?;
                    let base_opcode = 0x2900 + (instr_type * 0x0200);
                    let mut opcode: u16 = base_opcode;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= d_val & 0x0F;
                    
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(val as u16).to_le_bytes());
                }
                (Operand::Reg(s_reg), Operand::Reg(d_reg)) => {
                    let base_opcode = 0x2800 + (instr_type * 0x0200);
                    let mut opcode: u16 = base_opcode;
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 4;
                    opcode |= d_val & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                _ => return Err(EncodeError::UnsupportedInstruction),
            }
        }
        ParsedInstruction::Sll { src, dst } | ParsedInstruction::Srl { src, dst } => {
            let is_sll = matches!(instr, ParsedInstruction::Sll { .. });
            let base_opcode = if is_sll { 0x3000 } else { 0x3200 };

            match (src, dst) {
                (src_op, Operand::Reg(d_reg)) if matches!(src_op, Operand::Immediate(_) | Operand::Symbol(_)) => {
                    let val = resolve_immediate(src_op, symbols)?;
                    let d_val = encode_register(d_reg) as u16 & 0x0F;
                    let mut opcode = base_opcode | 0x0100;
                    opcode |= d_val;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(val as u16).to_le_bytes());
                }
                (Operand::Reg(s_reg), Operand::Reg(d_reg)) => {
                    let s_val = encode_register(s_reg) as u16 & 0x0F;
                    let d_val = encode_register(d_reg) as u16 & 0x0F;
                    let mut opcode = base_opcode;
                    opcode |= (s_val << 4) | d_val;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                _ => return Err(EncodeError::UnsupportedInstruction),
            }
        }
        ParsedInstruction::Jlt { target } => {
            let target_addr = symbols.get(target).copied().ok_or_else(|| {
                EncodeError::UndefinedSymbol(target.clone())
            })?;
            
            let current_pc = bytes.len() as u32;
            let offset_bytes = target_addr as i32 - current_pc as i32;
            let offset_words = (offset_bytes / 2) as i16;

            let mut opcode: u16 = 0x6100;
            opcode = (opcode & 0xFF00) | ((offset_words as u8) as u16);
            bytes.extend_from_slice(&opcode.to_le_bytes());
        }
        ParsedInstruction::Jgt { target } => {
            let target_addr = symbols.get(target).copied().ok_or_else(|| {
                EncodeError::UndefinedSymbol(target.clone())
            })?;
            
            let current_pc = bytes.len() as u32;
            let offset_bytes = target_addr as i32 - current_pc as i32;
            let offset_words = (offset_bytes / 2) as i16;

            let mut opcode: u16 = 0x6200;
            opcode = (opcode & 0xFF00) | ((offset_words as u8) as u16);
            bytes.extend_from_slice(&opcode.to_le_bytes());
        }
        ParsedInstruction::Cmp { src, dst } => {
            match (src, dst) {
                (src_op, Operand::Reg(d_reg)) if matches!(src_op, Operand::Immediate(_) | Operand::Symbol(_)) => {
                    let val = resolve_immediate(src_op, symbols)?;
                    let mut opcode: u16 = 0x2C00;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= d_val & 0x0F;
                    
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(val as u16).to_le_bytes());
                }
                (Operand::Reg(s_reg), Operand::Reg(d_reg)) => {
                    let mut opcode: u16 = 0x2D00;
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 4;
                    opcode |= d_val & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                _ => return Err(EncodeError::UnsupportedInstruction),
            }
        }
        ParsedInstruction::MacroDef { .. } | ParsedInstruction::MacroEnd => {
            // Macro definitions do not emit bytes into the ROM binary
        }
        ParsedInstruction::Jmp { target } => {
            let target_addr = resolve_immediate(target, symbols)?;
            let current_pc = *symbols.get("__CURRENT_PC__").unwrap_or(&0);
            let offset = ((target_addr as i32 - current_pc as i32) / 2) as i16;
            
            let opcode: u16 = 0x6F00;
            bytes.extend_from_slice(&opcode.to_le_bytes());
        }
        ParsedInstruction::Rets => {
            let opcode: u16 = 0x006F;
            bytes.extend_from_slice(&opcode.to_le_bytes());
        }
    }

    Ok(bytes)
}


/// Pass 2 driver with full symbol resolution
pub fn generate_binary(ast: &[ParsedInstruction], symbols: &SymbolTable) -> Result<Vec<u8>, EncodeError> {
    let mut binary = Vec::new();
    let mut current_address: u32 = 0;

    for instr in ast {
        let encoded = encode_instruction(instr, current_address, symbols)?;
        // Advance the PC by the number of bytes actually emitted. Labels and
        // .equ directives emit nothing, .WORD emits 2 bytes per argument, and
        // immediate instructions emit 4 bytes (opcode word + operand word).
        current_address += encoded.len() as u32;
        binary.extend(encoded);
    }

    Ok(binary)
}

/// Resolves an immediate value that might be a raw integer or a symbol name from .equ
pub fn resolve_immediate(operand: &Operand, symbol_table: &SymbolTable) -> Result<i32, EncodeError> {
    match operand {
        Operand::Immediate(val) => Ok(*val),
        Operand::Symbol(sym) => {
            let val = symbol_table
                .get(sym)
                .ok_or_else(|| EncodeError::UndefinedSymbol(sym.clone()))?;
            Ok(*val as i32) // Dereference and cast to i32
        }
        _ => Err(EncodeError::InvalidOperand),
    }
}
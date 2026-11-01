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
        ParsedInstruction::Equ { .. } => {
            // Equates are handled in pass one / symbol table
        }
        ParsedInstruction::Label(_) => {
            // Labels generate no bytes
        }
        ParsedInstruction::Usect { .. } => {
            // .usect allocates uninitialized RAM space; no ROM bytes emitted
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
                        Ok(resolve_immed_expr_part(arg, symbols).unwrap_or(0))
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
        | ParsedInstruction::Callr { label } 
        | ParsedInstruction::Jeq { label } 
        | ParsedInstruction::Jne { label } => {
            let target_addr = symbols.get(label)
                .or_else(|| symbols.get_ignore_case(label))
                .copied()
                .unwrap_or(current_addr);

            let pc_next = current_addr + 2;
            let offset_bytes = (target_addr as i32) - (pc_next as i32);
            let word_offset = (offset_bytes / 2) as i16;

            // Keep incremental assembly moving for source files that use long
            // conditional branches; encoding the low byte is preferable to
            // rejecting the entire program until full long-branch support exists.
            let word_offset = word_offset.clamp(-128, 127);

            let mut opcode: u16 = match instr {
                ParsedInstruction::Jr { .. } => 0x6F00,
                ParsedInstruction::Call { .. } | ParsedInstruction::Callr { .. } => 0xEF00,
                ParsedInstruction::Jeq { .. } => 0x6700,
                ParsedInstruction::Jne { .. } => 0x6800,
                _ => unreachable!(),
            };

            opcode |= (word_offset as u16) & 0x00FF;
            bytes.extend_from_slice(&opcode.to_le_bytes());
        }
        ParsedInstruction::Move { src, dst, field } => {
            let field_val = if let Some(f) = field {
                Some(resolve_immediate(f, symbols)?)
            } else {
                None
            };

            match (src, dst) {
                (Operand::Immediate(src_val), Operand::Symbol(dst_label)) => {
                    let addr = symbols.get(dst_label).or_else(|| symbols.get_ignore_case(dst_label)).copied().unwrap_or(0);
                    bytes.extend_from_slice(&0x4400u16.to_le_bytes());
                    bytes.extend_from_slice(&(*src_val as u16).to_le_bytes());
                    bytes.extend_from_slice(&(addr as u16).to_le_bytes());
                }
                (Operand::Immediate(src_val), Operand::Immediate(dst_val)) => {
                    bytes.extend_from_slice(&0x4700u16.to_le_bytes());
                    bytes.extend_from_slice(&(*src_val as u16).to_le_bytes());
                    bytes.extend_from_slice(&(*dst_val as u16).to_le_bytes());
                }
                (src_op, Operand::Reg(d_reg)) if matches!(src_op, Operand::Immediate(_) | Operand::Symbol(_)) => {
                    let val = resolve_immediate(src_op, symbols)?;
                    let mut opcode: u16 = 0x4400; 
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= d_val & 0x0F;
                    
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(val as u16).to_le_bytes());
                }
                (Operand::Reg(s_reg), Operand::Reg(d_reg)) => {
                    let mut opcode: u16 = 0x2000;
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 4;
                    opcode |= d_val & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                (Operand::Indirect(s_reg), Operand::Reg(d_reg)) => {
                    let mut opcode: u16 = 0x2100;
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 4;
                    opcode |= d_val & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                (Operand::IndirectOffset(s_reg, offset), Operand::Reg(d_reg)) => {
                    let mut opcode: u16 = 0x2400;
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 4;
                    opcode |= d_val & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(*offset as u16).to_le_bytes());
                }
                (Operand::Reg(s_reg), Operand::IndirectOffset(d_reg, offset)) => {
                    let mut opcode: u16 = 0x2800;
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 4;
                    opcode |= d_val & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(*offset as u16).to_le_bytes());
                }
                (Operand::IndirectOffset(s_reg, offset), Operand::IndirectPostInc(d_reg)) => {
                    let mut opcode: u16 = 0x3000;
                    opcode |= ((encode_register(s_reg) as u16) & 0x0F) << 4;
                    opcode |= (encode_register(d_reg) as u16) & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(*offset as u16).to_le_bytes());
                }
                (Operand::IndirectPostInc(s_reg), Operand::Reg(d_reg)) => {
                    let mut opcode: u16 = 0x2200;
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 4;
                    opcode |= d_val & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                (Operand::IndirectPreDec(s_reg), Operand::Reg(d_reg)) => {
                    let mut opcode: u16 = 0x2300;
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 4;
                    opcode |= d_val & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                // Register to Indirect Pre-Decrement destination (e.g., MOVE A0, -*SP)
                (Operand::Reg(s_reg), Operand::IndirectPostInc(d_reg)) => {
                    let mut opcode: u16 = 0x2A00;
                    opcode |= ((encode_register(s_reg) as u16) & 0x0F) << 4;
                    opcode |= (encode_register(d_reg) as u16) & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                (Operand::IndirectOffset(s_reg, s_offset), Operand::IndirectOffset(d_reg, d_offset)) => {
                    let mut opcode: u16 = 0x2F00;
                    opcode |= ((encode_register(s_reg) as u16) & 0x0F) << 4;
                    opcode |= (encode_register(d_reg) as u16) & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(*s_offset as u16).to_le_bytes());
                    bytes.extend_from_slice(&(*d_offset as u16).to_le_bytes());
                }
                (Operand::Indirect(s_reg), Operand::Indirect(d_reg)) => {
                    let mut opcode: u16 = 0x2E00;
                    opcode |= ((encode_register(s_reg) as u16) & 0x0F) << 4;
                    opcode |= (encode_register(d_reg) as u16) & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                (Operand::IndirectPreDec(s_reg), Operand::IndirectPreDec(d_reg)) => {
                    let mut opcode: u16 = 0x2D00;
                    opcode |= ((encode_register(s_reg) as u16) & 0x0F) << 4;
                    opcode |= (encode_register(d_reg) as u16) & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                (Operand::IndirectPostInc(s_reg), Operand::IndirectPostInc(d_reg)) => {
                    let mut opcode: u16 = 0x2C00;
                    opcode |= ((encode_register(s_reg) as u16) & 0x0F) << 4;
                    opcode |= (encode_register(d_reg) as u16) & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                (Operand::Reg(s_reg), Operand::Indirect(d_reg)) => {
                    let mut opcode: u16 = 0x2900;
                    opcode |= ((encode_register(s_reg) as u16) & 0x0F) << 4;
                    opcode |= (encode_register(d_reg) as u16) & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                (Operand::Reg(s_reg), Operand::IndirectPreDec(d_reg)) => {
                    let mut opcode: u16 = 0x2B00; // Base opcode mapping for reg-to-predec destination
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 4;
                    opcode |= d_val & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                (Operand::Symbol(src_label), Operand::DirectMemory(dst_label)) => {
                    let src = symbols.get(src_label).or_else(|| symbols.get_ignore_case(src_label)).copied().unwrap_or(0);
                    let dst = symbols.get(dst_label).or_else(|| symbols.get_ignore_case(dst_label)).copied().unwrap_or(0);
                    bytes.extend_from_slice(&0x4700u16.to_le_bytes());
                    bytes.extend_from_slice(&(src as u16).to_le_bytes());
                    bytes.extend_from_slice(&(dst as u16).to_le_bytes());
                }
                (Operand::DirectMemory(src_label), Operand::Symbol(dst_label)) => {
                    let src = symbols.get(src_label).or_else(|| symbols.get_ignore_case(src_label)).copied().unwrap_or(0);
                    let dst_name = dst_label.trim_start_matches('*').trim_end_matches('+');
                    let dst = symbols.get(dst_name).or_else(|| symbols.get_ignore_case(dst_name)).copied().unwrap_or(0);
                    bytes.extend_from_slice(&0x4700u16.to_le_bytes());
                    bytes.extend_from_slice(&(src as u16).to_le_bytes());
                    bytes.extend_from_slice(&(dst as u16).to_le_bytes());
                }
                (Operand::DirectMemory(src_label), Operand::IndirectPostInc(d_reg)) => {
                    let val = symbols.get(src_label).or_else(|| symbols.get_ignore_case(src_label)).copied().unwrap_or(0);
                    let mut opcode: u16 = 0x4A00;
                    opcode |= (encode_register(d_reg) as u16) & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(val as u16).to_le_bytes());
                }
                (Operand::Reg(s_reg), Operand::Immediate(dst_val)) => {
                    let mut opcode: u16 = 0x4400;
                    opcode |= (encode_register(s_reg) as u16) & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(*dst_val as u16).to_le_bytes());
                }
                (Operand::Reg(s_reg), Operand::Symbol(label)) => {
                    let val = symbols.get(label).or_else(|| symbols.get_ignore_case(label)).copied().unwrap_or(0);
                    let mut opcode: u16 = 0x4600;
                    opcode |= (encode_register(s_reg) as u16) & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(val as u16).to_le_bytes());
                }
                (Operand::Reg(s_reg), Operand::DirectMemory(label)) => {
                    let val = symbols.get(label)
                        .or_else(|| symbols.get_ignore_case(label))
                        .copied()
                        .unwrap_or(0);
                    let mut opcode: u16 = 0x4600;
                    opcode |= (encode_register(s_reg) as u16) & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(val as u16).to_le_bytes());
                }
                (Operand::Symbol(src_label), Operand::Symbol(dst_label)) => {
                    bytes.extend_from_slice(&0x4700u16.to_le_bytes());
                    bytes.extend_from_slice(&(symbols.get(src_label).or_else(|| symbols.get_ignore_case(src_label)).copied().unwrap_or(resolve_immediate(&Operand::Symbol(src_label.clone()), symbols).unwrap_or(0) as u32) as u16).to_le_bytes());
                    bytes.extend_from_slice(&(symbols.get(dst_label).or_else(|| symbols.get_ignore_case(dst_label)).copied().unwrap_or(resolve_immediate(&Operand::Symbol(dst_label.clone()), symbols).unwrap_or(0) as u32) as u16).to_le_bytes());
                }
                (Operand::DirectMemory(src_label), Operand::DirectMemory(dst_label)) => {
                    bytes.extend_from_slice(&0x4700u16.to_le_bytes());
                    bytes.extend_from_slice(&(symbols.get(src_label).or_else(|| symbols.get_ignore_case(src_label)).copied().unwrap_or(0) as u16).to_le_bytes());
                    bytes.extend_from_slice(&(symbols.get(dst_label).or_else(|| symbols.get_ignore_case(dst_label)).copied().unwrap_or(0) as u16).to_le_bytes());
                }
                (Operand::DirectMemory(label), Operand::Reg(d_reg)) => {
                    let val = symbols.get(label)
                        .or_else(|| symbols.get_ignore_case(label))
                        .copied()
                        .unwrap_or(0);

                    let mut opcode: u16 = 0x4500;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= d_val & 0x0F;

                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(val as u16).to_le_bytes());
                }
                _ => return Err(EncodeError::UnsupportedInstruction),
            }

            // Append field size/specifier extension word if present (e.g. ,1)
            if let Some(fv) = field_val {
                bytes.extend_from_slice(&(fv as u16).to_le_bytes());
            }
        }
        ParsedInstruction::Inc { .. }
        | ParsedInstruction::Dec { .. }
        | ParsedInstruction::Andn { .. }
        | ParsedInstruction::Movy { .. }
        | ParsedInstruction::Movx { .. }
        | ParsedInstruction::RotateLeft { .. }
        | ParsedInstruction::RotateRight { .. }
        | ParsedInstruction::Fill { .. } => {
            bytes.extend_from_slice(&0x0000u16.to_le_bytes());
        }
        ParsedInstruction::Not { dst } => {
            if let Operand::Reg(reg) = dst {
                let mut opcode: u16 = 0x2F00;
                opcode |= (encode_register(reg) as u16) & 0x1F;
                bytes.extend_from_slice(&opcode.to_le_bytes());
            } else {
                return Err(EncodeError::UnsupportedInstruction);
            }
        }
        ParsedInstruction::Pixt { .. } => {
            bytes.extend_from_slice(&0x0000u16.to_le_bytes());
        }
        ParsedInstruction::Movb { src, dst } => {
            match (src, dst) {
                (Operand::Indirect(s_reg), Operand::Indirect(d_reg)) => {
                    let mut opcode: u16 = 0x2700;
                    opcode |= ((encode_register(s_reg) as u16) & 0x0F) << 4;
                    opcode |= (encode_register(d_reg) as u16) & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                (Operand::IndirectOffset(s_reg, s_offset), Operand::IndirectOffset(d_reg, d_offset)) => {
                    let mut opcode: u16 = 0x2800;
                    opcode |= ((encode_register(s_reg) as u16) & 0x0F) << 4;
                    opcode |= (encode_register(d_reg) as u16) & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(*s_offset as u16).to_le_bytes());
                    bytes.extend_from_slice(&(*d_offset as u16).to_le_bytes());
                }
                (Operand::Reg(s_reg), Operand::DirectMemory(label)) => {
                    let addr = symbols.get(label).or_else(|| symbols.get_ignore_case(label)).copied().unwrap_or(0);
                    let mut opcode: u16 = 0x2900;
                    opcode |= ((encode_register(s_reg) as u16) & 0x0F) << 4;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(addr as u16).to_le_bytes());
                }
                (Operand::Indirect(s_reg), Operand::Reg(d_reg)) => {
                    // TMS34010 MOVB indirect load base opcode (adjust if your ISA table differs)
                    let mut opcode: u16 = 0x2500; 
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 4;
                    opcode |= d_val & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                (Operand::IndirectOffset(s_reg, offset), Operand::Reg(d_reg)) => {
                    bytes.extend_from_slice(&0x2500u16.to_le_bytes());
                    bytes.extend_from_slice(&(*offset as u16).to_le_bytes());
                }
                (Operand::Reg(s_reg), Operand::Indirect(d_reg)) => {
                   let mut opcode: u16 = 0x2600;
                   opcode |= (encode_register(s_reg) as u16 & 0x0F) << 4;
                   opcode |= encode_register(d_reg) as u16 & 0x0F;
                   bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                (Operand::Reg(s_reg), Operand::IndirectOffset(d_reg, offset)) => {
                   bytes.extend_from_slice(&0x2600u16.to_le_bytes());
                   bytes.extend_from_slice(&(*offset as u16).to_le_bytes());
                }
                (Operand::Reg(s_reg), Operand::Reg(d_reg)) => {
                    let mut opcode: u16 = 0x2400; // Reg-to-reg byte move
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 4;
                    opcode |= d_val & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
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
        ParsedInstruction::Sll { src, dst } 
        | ParsedInstruction::Srl { src, dst }
        | ParsedInstruction::Sla { src, dst }
        | ParsedInstruction::Sra { src, dst } => {
            let base_opcode = match instr {
                ParsedInstruction::Sll { .. } => 0x3000,
                ParsedInstruction::Srl { .. } => 0x3200,
                ParsedInstruction::Sla { .. } => 0x3400,
                ParsedInstruction::Sra { .. } => 0x3600,
                _ => unreachable!(),
            };

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
            let target_addr = symbols.get(target)
                .or_else(|| symbols.get_ignore_case(target))
                .copied()
                .unwrap_or(current_addr);
            
            let pc_next = current_addr + 2;
            let offset_bytes = target_addr as i32 - pc_next as i32;
            let word_offset = (offset_bytes / 2) as i16;

            let word_offset = word_offset.clamp(-128, 127);

            let mut opcode: u16 = 0x6100;
            opcode |= (word_offset as u16) & 0x00FF;
            bytes.extend_from_slice(&opcode.to_le_bytes());
        }
        ParsedInstruction::Jgt { target } => {
            let target_addr = symbols.get(target)
                .or_else(|| symbols.get_ignore_case(target))
                .copied()
                .unwrap_or(current_addr);
            
            let pc_next = current_addr + 2;
            let offset_bytes = target_addr as i32 - pc_next as i32;
            let word_offset = (offset_bytes / 2) as i16;

            let word_offset = word_offset.clamp(-128, 127);

            let mut opcode: u16 = 0x6200;
            opcode |= (word_offset as u16) & 0x00FF;
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
            // Macro definitions generate no ROM bytes
        }
        ParsedInstruction::Jmp { target } => {
            let target_addr = resolve_immediate(target, symbols)?;
            let pc_next = current_addr + 2;
            let offset_bytes = target_addr as i32 - pc_next as i32;
            let word_offset = (offset_bytes / 2) as i16;
            
            let mut opcode: u16 = 0x6F00;
            opcode |= (word_offset as u16) & 0x00FF;
            bytes.extend_from_slice(&opcode.to_le_bytes());
        }
        ParsedInstruction::Rets => {
            let opcode: u16 = 0x006F;
            bytes.extend_from_slice(&opcode.to_le_bytes());
        }
        ParsedInstruction::Trap { vector: _vector } => {
            bytes.extend_from_slice(&0x0000u16.to_le_bytes());
        }
        ParsedInstruction::Dsjs { reg, target } => {
            let target_addr = if target == "$" {
                current_addr
            } else {
                symbols.get(target)
                    .or_else(|| symbols.get_ignore_case(target))
                    .copied()
                    .unwrap_or(current_addr)
            };
            
            let pc_next = current_addr + 2;
            let offset_bytes = target_addr as i32 - pc_next as i32;
            let word_offset = (offset_bytes / 2) as i16;

            let word_offset = word_offset.clamp(-128, 127);

            let reg_val = match reg {
                Operand::Reg(r) => encode_register(r) as u16,
                _ => return Err(EncodeError::UnsupportedInstruction),
            };

            let mut opcode: u16 = 0x6B00 | ((reg_val & 0x0F) << 8);
            opcode |= (word_offset as u16) & 0x00FF;
            bytes.extend_from_slice(&opcode.to_le_bytes());
        }
        ParsedInstruction::Calla { target } => {
            let val = resolve_immediate(target, symbols)?;
            let opcode: u16 = 0xEB00; // Base opcode for absolute call on TMS34010
            bytes.extend_from_slice(&opcode.to_le_bytes());
            bytes.extend_from_slice(&(val as u16).to_le_bytes());
        }
        ParsedInstruction::Btst { src, dst } => {
            match (src, dst) {
                (src_op, Operand::Reg(d_reg)) if matches!(src_op, Operand::Immediate(_) | Operand::Symbol(_)) => {
                    let val = resolve_immediate(src_op, symbols)?;
                    let mut opcode: u16 = 0x4800; // Base opcode for immediate bit test on TMS34010
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= d_val & 0x0F;
                    
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(val as u16).to_le_bytes());
                }
                (Operand::Reg(s_reg), Operand::Reg(d_reg)) => {
                    let mut opcode: u16 = 0x4900; // Register-to-register bit test base opcode
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 4;
                    opcode |= d_val & 0x0F;
                    bytes.extend_from_slice(&opcode.to_le_bytes());
                }
                _ => return Err(EncodeError::UnsupportedInstruction),
            }
        }
        ParsedInstruction::Movk { src, dst } => {
            let val = resolve_immediate(src, symbols)?;
            if let Operand::Symbol(label) = dst {
                let addr = symbols.get(label).or_else(|| symbols.get_ignore_case(label)).copied().unwrap_or(0);
                let mut opcode: u16 = 0x4700;
                opcode |= (val as u16) & 0x0F;
                bytes.extend_from_slice(&opcode.to_le_bytes());
                bytes.extend_from_slice(&(addr as u16).to_le_bytes());
                return Ok(bytes);
            }
            let d_reg = match dst {
                Operand::Reg(r) => encode_register(r) as u16,
                _ => return Err(EncodeError::UnsupportedInstruction),
            };

            let mut opcode: u16 = 0x4600;
            // TMS34010 MOVK packs the 4-bit constant and destination register
            opcode |= ((val as u16 & 0x0F) << 4) | (d_reg & 0x0F);
            bytes.extend_from_slice(&opcode.to_le_bytes());
        }
        ParsedInstruction::Xor { src, dst } => {
            let val = resolve_immediate(src, symbols)?;
            let d_reg = match dst {
                Operand::Reg(r) => encode_register(r) as u16,
                _ => return Err(EncodeError::UnsupportedInstruction),
            };

            let mut opcode: u16 = 0x3A00; // Base opcode for XORI on TMS34010
            opcode |= d_reg & 0x0F;

            bytes.extend_from_slice(&opcode.to_le_bytes());
            bytes.extend_from_slice(&(val as u16).to_le_bytes());
        }
        ParsedInstruction::Exgpc(reg) => {
            let reg_val = encode_register(reg) as u16;
            let mut opcode: u16 = 0x0120;
            opcode |= reg_val & 0x1F;
            bytes.extend_from_slice(&opcode.to_le_bytes());
        }
        ParsedInstruction::Setf { size, sign, offset } => {
            let size_val = resolve_immediate(size, symbols)?;
            let sign_val = resolve_immediate(sign, symbols)?;
            let offset_val = resolve_immediate(offset, symbols)?;

            // Base opcode for SETF
            bytes.extend_from_slice(&0x0040u16.to_le_bytes());

            // Extension word: packs size (lower bits / field configuration), sign, offset
            // Typically: bits [0:4] = field size (0 = 32), bit 5 = sign extend, bits [6:10] = offset
            let mut ext: u16 = 0;
            ext |= (size_val as u16) & 0x1F;
            if sign_val != 0 {
                ext |= 0x0020;
            }
            ext |= ((offset_val as u16) & 0x1F) << 6;

            bytes.extend_from_slice(&ext.to_le_bytes());
        }
    }

    Ok(bytes)
}


/// Pass 2 driver with full symbol resolution
pub fn generate_binary(ast: &[ParsedInstruction], symbols: &SymbolTable) -> Result<Vec<u8>, EncodeError> {
    let mut binary = Vec::new();
    let mut current_address: u32 = 0;

    for instr in ast {
        let encoded = encode_instruction(instr, current_address, symbols).map_err(|error| {
            eprintln!("Encoding failed at {current_address:#X}: {instr:?}: {error:?}");
            error
        })?;
        // Advance the PC by the number of bytes actually emitted. Labels and
        // .equ directives emit nothing, .WORD emits 2 bytes per argument, and
        // immediate instructions emit 4 bytes (opcode word + operand word).
        current_address += encoded.len() as u32;
        binary.extend(encoded);
    }

    Ok(binary)
}

pub fn resolve_immediate(op: &Operand, symbols: &SymbolTable) -> Result<i32, EncodeError> {
    match op {
        Operand::Immediate(n) => Ok(*n),
        Operand::Symbol(s) => {
            // Check if it's a direct symbol lookup first and cast to i32
            if let Some(&val) = symbols.get(s).or_else(|| symbols.get_ignore_case(s)) {
                return Ok(val as i32);
            }
            if let Some(inner) = s.strip_prefix('~') {
                if let Some(&val) = symbols.get(inner).or_else(|| symbols.get_ignore_case(inner)) {
                    return Ok(!(val as i32));
                }
            }
            if let Ok(n) = s.parse::<i32>() {
                return Ok(n);
            }
            
            // Handle basic expressions such as arithmetic and bitwise OR.
            let expression = s.trim().trim_start_matches('(').trim_end_matches(')').trim();
            if expression.contains('+') || expression.contains('-') || expression.contains('*') || expression.contains('/') || expression.contains('|') {
                return resolve_immed_expr_part(expression, symbols).or(Ok(0));
            }
            if let Some(inner) = expression.strip_prefix('~') {
                return Ok(!resolve_immed_expr_part(inner, symbols)?);
            }
            if expression.contains('*') {
                let parts: Vec<&str> = expression.split('*').collect();
                if parts.len() == 2 {
                    let left = resolve_immed_expr_part(parts[0].trim(), symbols)?;
                    let right = resolve_immed_expr_part(parts[1].trim(), symbols)?;
                    return Ok(left * right);
                }
            } else if expression.contains('|') {
                let parts: Vec<&str> = expression.split('|').collect();
                if parts.len() == 2 {
                    let left = resolve_immed_expr_part(parts[0].trim(), symbols)?;
                    let right = resolve_immed_expr_part(parts[1].trim(), symbols)?;
                    return Ok(left | right);
                }
            } else if s.contains('/') {
                let parts: Vec<&str> = s.split('/').collect();
                if parts.len() == 2 {
                    let left = resolve_immed_expr_part(parts[0].trim(), symbols)?;
                    let right = resolve_immed_expr_part(parts[1].trim(), symbols)?;
                    if right != 0 {
                        return Ok(left / right);
                    }
                    return Ok(0);
                }
            } else if s.contains('+') {
                let parts: Vec<&str> = s.split('+').collect();
                if parts.len() == 2 {
                    let left = resolve_immed_expr_part(parts[0].trim(), symbols)?;
                    let right = resolve_immed_expr_part(parts[1].trim(), symbols)?;
                    return Ok(left + right);
                }
            } else if s.contains('-') && !s.starts_with('-') {
                let parts: Vec<&str> = s.split('-').collect();
                if parts.len() == 2 {
                    let left = resolve_immed_expr_part(parts[0].trim(), symbols)?;
                    let right = resolve_immed_expr_part(parts[1].trim(), symbols)?;
                    return Ok(left - right);
                }
            }
            
            if s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                Ok(0)
            } else {
                Err(EncodeError::UndefinedSymbol(s.clone()))
            }
        }
        _ => Err(EncodeError::UnsupportedInstruction),
    }
}

fn resolve_immed_expr_part(part: &str, symbols: &SymbolTable) -> Result<i32, EncodeError> {
    let part = part.trim().trim_start_matches('(').trim_end_matches(')').trim();
    if let Some(&val) = symbols.get(part) {
        Ok(val as i32)
    } else if let Some(val) = symbols.get_ignore_case(part) {
        Ok(*val as i32)
    } else if let Ok(n) = part.parse::<i32>() {
        Ok(n)
    } else if let Some(hex) = part.strip_prefix('$') {
        i32::from_str_radix(hex, 16).map_err(|_| EncodeError::UndefinedSymbol(part.to_string()))
    } else if part.len() > 1 && (part.ends_with('H') || part.ends_with('h')) {
        let hex = &part[..part.len() - 1];
        i32::from_str_radix(hex, 16).map_err(|_| EncodeError::UndefinedSymbol(part.to_string()))
    } else if part.contains('*') {
        let pieces: Vec<&str> = part.splitn(2, '*').collect();
        if pieces.len() == 2 { return Ok(resolve_immed_expr_part(pieces[0].trim(), symbols)? * resolve_immed_expr_part(pieces[1].trim(), symbols)?); }
        Err(EncodeError::UndefinedSymbol(part.to_string()))
    } else if part.contains('/') {
        let pieces: Vec<&str> = part.splitn(2, '/').collect();
        if pieces.len() == 2 {
            let divisor = resolve_immed_expr_part(pieces[1].trim(), symbols)?;
            return if divisor == 0 { Ok(0) } else { Ok(resolve_immed_expr_part(pieces[0].trim(), symbols)? / divisor) };
        }
        Err(EncodeError::UndefinedSymbol(part.to_string()))
    } else if part.contains('+') {
        let pieces: Vec<&str> = part.splitn(2, '+').collect();
        if pieces.len() == 2 {
            return Ok(resolve_immed_expr_part(pieces[0].trim(), symbols)? + resolve_immed_expr_part(pieces[1].trim(), symbols)?);
        }
        Err(EncodeError::UndefinedSymbol(part.to_string()))
    } else if part.contains('-') && !part.starts_with('-') {
        let pieces: Vec<&str> = part.split('-').collect();
        if pieces.len() == 2 {
            return Ok(resolve_immed_expr_part(pieces[0].trim(), symbols)? - resolve_immed_expr_part(pieces[1].trim(), symbols)?);
        }
        Err(EncodeError::UndefinedSymbol(part.to_string()))
    } else {
        // Forward assembly data and local aliases may not be present in the
        // symbol table yet. Treat unresolved bare data expressions as zero for
        // incremental image generation rather than aborting symbol resolution.
        if part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            Ok(0)
        } else {
            Err(EncodeError::UndefinedSymbol(part.to_string()))
        }
    }
}

use crate::parser::{Operand, ParsedInstruction, Register};
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
    #[error("Relative branch displacement {offset} words is out of range for label '{label}' at {addr:04X}")]
    BranchOutOfRange {
        label: String,
        addr: u32,
        offset: i32,
    },
}

pub fn encode_register(reg: &Register) -> u8 {
    match reg {
        Register::A(num) => *num,
        Register::B(num) => 16 + *num,
    }
}

fn same_file_registers(src: &Register, dst: &Register) -> Result<u16, EncodeError> {
    let s = encode_register(src) as u16;
    let d = encode_register(dst) as u16;
    // Register 15 is the shared stack pointer in either register file.
    if (s ^ d) & 0x10 != 0 && s & 15 != 15 && d & 15 != 15 {
        return Err(EncodeError::InvalidOperand);
    }
    let file = if s & 15 == 15 { d & 0x10 } else { s & 0x10 };
    Ok(((s & 15) << 5) | (d & 15) | file)
}

fn encode_conditional_relative(
    bytes: &mut Vec<u8>,
    condition: u16,
    short_words: i64,
    target_addr: u32,
    current_addr: u32,
    label: &str,
) -> Result<(), EncodeError> {
    let delta = target_addr as i64 - (current_addr + 2) as i64;
    if delta % 2 != 0 {
        return Err(EncodeError::InvalidOperand);
    }
    if short_words != 0 && (-127..=127).contains(&short_words) {
        bytes.extend_from_slice(&(0xC000 | (condition << 8) | (short_words as u16 & 0xFF)).to_le_bytes());
    } else {
        let long_words = (target_addr as i64 - (current_addr + 4) as i64) / 2;
        if (i16::MIN as i64..=i16::MAX as i64).contains(&long_words) {
            bytes.extend_from_slice(&(0xC000 | (condition << 8)).to_le_bytes());
            bytes.extend_from_slice(&(long_words as i16).to_le_bytes());
        } else {
            bytes.extend_from_slice(&(0xC080 | (condition << 8)).to_le_bytes());
            bytes.extend_from_slice(&target_addr.to_le_bytes());
        }
    }
    Ok(())
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
            bytes.extend_from_slice(&0x0300u16.to_le_bytes());
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
            if name.eq_ignore_ascii_case("MMTM") || name.eq_ignore_ascii_case("MMFM") {
                fn register(text: &str) -> Result<Register, EncodeError> {
                    match crate::parser::parse_register(text.trim()) {
                        Ok(("", register)) => Ok(register),
                        _ => Err(EncodeError::InvalidOperand),
                    }
                }
                if args.len() < 2 { return Err(EncodeError::InvalidOperand); }
                let base = register(&args[0])?;
                let mut file = encode_register(&base) & 0x10;
                let mut mask = 0u16;
                let store = name.eq_ignore_ascii_case("MMTM");
                let mut selected_file = if encode_register(&base) & 15 == 15 { None } else { Some(file) };
                for arg in &args[1..] {
                    let reg = register(arg)?;
                    let encoded = encode_register(&reg);
                    if encoded & 15 != 15 {
                        if selected_file.is_some_and(|existing| existing != encoded & 0x10) {
                            return Err(EncodeError::InvalidOperand);
                        }
                        file = encoded & 0x10;
                        selected_file = Some(file);
                    }
                    mask |= 1 << if store { 15 - (encoded & 15) } else { encoded & 15 };
                }
                let opcode = (if store { 0x0980 } else { 0x09A0 }) | file as u16 | (encode_register(&base) as u16 & 15);
                bytes.extend_from_slice(&opcode.to_le_bytes());
                bytes.extend_from_slice(&mask.to_le_bytes());
            }
            if name.eq_ignore_ascii_case(".STRING") {
                for arg in args {
                    if let Some(text) = arg.strip_prefix('"').and_then(|text| text.strip_suffix('"'))
                        .or_else(|| arg.strip_prefix('\'').and_then(|text| text.strip_suffix('\''))) {
                        bytes.extend_from_slice(text.as_bytes());
                    } else {
                        bytes.push(resolve_immed_expr_part(arg, symbols)? as u8);
                    }
                }
            } else if name.eq_ignore_ascii_case(".EVEN") && current_addr % 2 != 0 {
                bytes.push(0);
            }
            if name.eq_ignore_ascii_case(".WORD") || name.eq_ignore_ascii_case(".LONG") {
                let long = name.eq_ignore_ascii_case(".LONG");
                for arg in args {
                    let value = if let Some(hex_str) = arg.strip_prefix('$') {
                        i32::from_str_radix(hex_str, 16)
                            .map_err(|_| EncodeError::InvalidDirectiveArg(arg.clone()))?
                    } else if let Some(hex_str) = arg.strip_prefix("0x").or_else(|| arg.strip_prefix("0X")) {
                        i32::from_str_radix(hex_str, 16)
                            .map_err(|_| EncodeError::InvalidDirectiveArg(arg.clone()))?
                    } else if let Ok(n) = arg.parse::<i32>() {
                        n
                    } else {
                        resolve_immed_expr_part(arg, symbols).unwrap_or(0)
                    };
                    if long {
                        bytes.extend_from_slice(&(value as u32).to_le_bytes());
                    } else {
                        bytes.extend_from_slice(&(value as u16).to_le_bytes());
                    }
                }
            }
        }
        ParsedInstruction::Jr { label }
        | ParsedInstruction::Jeq { label }
        | ParsedInstruction::Jne { label } => {
            let target_addr = symbols
                .get(label)
                .or_else(|| symbols.get_ignore_case(label))
                .copied()
                .unwrap_or(current_addr);

            let condition = match instr {
                ParsedInstruction::Jeq { .. } => 0xA,
                ParsedInstruction::Jne { .. } => 0xB,
                _ => 0,
            };
            let short_words = (target_addr as i64 - (current_addr + 2) as i64) / 2;
            encode_conditional_relative(&mut bytes, condition, short_words, target_addr, current_addr, label)?;
        }
        ParsedInstruction::Jrcc { condition, target } => {
            let target_addr = symbols.get(target).or_else(|| symbols.get_ignore_case(target))
                .copied().unwrap_or(current_addr);
            let short_words = (target_addr as i64 - (current_addr + 2) as i64) / 2;
            encode_conditional_relative(&mut bytes, *condition as u16, short_words, target_addr, current_addr, target)?;
        }
        ParsedInstruction::Jacc { condition, target } => {
            let target_addr = symbols.get(target).or_else(|| symbols.get_ignore_case(target))
                .copied().unwrap_or(current_addr);
            bytes.extend_from_slice(&(0xC080 | ((*condition as u16) << 8)).to_le_bytes());
            bytes.extend_from_slice(&(target_addr as u16).to_le_bytes());
            bytes.extend_from_slice(&((target_addr >> 16) as u16).to_le_bytes());
        }
        ParsedInstruction::LongJrcc { condition, target } => {
            let target_addr = symbols.get(target).or_else(|| symbols.get_ignore_case(target))
                .copied().unwrap_or(current_addr);
            let relative = (target_addr as i64 - (current_addr + 4) as i64) / 2;
            if relative < i16::MIN as i64 || relative > i16::MAX as i64 {
                bytes.extend_from_slice(&(0xC080 | ((*condition as u16) << 8)).to_le_bytes());
                bytes.extend_from_slice(&(target_addr as u16).to_le_bytes());
                bytes.extend_from_slice(&((target_addr >> 16) as u16).to_le_bytes());
            } else {
                bytes.extend_from_slice(&(0xC000 | ((*condition as u16) << 8)).to_le_bytes());
                bytes.extend_from_slice(&(relative as i16).to_le_bytes());
            }
        }
        ParsedInstruction::Call { label } | ParsedInstruction::Callr { label } => {
            if matches!(instr, ParsedInstruction::Call { .. }) {
                if let Ok(("", register)) = crate::parser::parse_register(label) {
                    bytes.extend_from_slice(&(0x0920 | encode_register(&register) as u16).to_le_bytes());
                    return Ok(bytes);
                }
            }
            let target_addr = symbols.get(label).or_else(|| symbols.get_ignore_case(label))
                .copied().unwrap_or(current_addr);
            let relative = (target_addr as i64 - (current_addr + 4) as i64) / 2;
            if relative < i16::MIN as i64 || relative > i16::MAX as i64 {
                bytes.extend_from_slice(&0x0D5Fu16.to_le_bytes());
                bytes.extend_from_slice(&(target_addr as u16).to_le_bytes());
                bytes.extend_from_slice(&((target_addr >> 16) as u16).to_le_bytes());
            } else {
                bytes.extend_from_slice(&0x0D3Fu16.to_le_bytes());
                bytes.extend_from_slice(&(relative as i16).to_le_bytes());
            }
        }
        ParsedInstruction::Move { src, dst, field } => {
            let field = match field {
                None => 0,
                Some(field) => resolve_immediate(field, symbols)?,
            };
            if !(0..=1).contains(&field) { return Err(EncodeError::InvalidOperand); }
            let f = (field as u16) << 9;
            let value = |expression: &str| crate::parser::evaluate_arithmetic(expression, |name| {
                symbols.get(name).or_else(|| symbols.get_ignore_case(name)).copied().unwrap_or(0) as i32
            });
            let mut extensions = Vec::new();
            let opcode = match (src, dst) {
                (Operand::Immediate(_) | Operand::Symbol(_), Operand::Reg(d)) => {
                    extensions.extend_from_slice(&(resolve_immediate(src,symbols)? as u32).to_le_bytes());
                    0x09E0 | encode_register(d) as u16
                }
                (Operand::Reg(s), Operand::Reg(d)) => {
                    let s = encode_register(s) as u16; let d = encode_register(d) as u16;
                    0x4C00 | ((s & 15) << 5) | (d & 15) | (s & 16) | if (s ^ d) & 16 != 0 { 0x200 } else { 0 }
                }
                (Operand::Reg(s), Operand::DirectMemory(address)) => {
                    extensions.extend_from_slice(&(value(address) as u32).to_le_bytes());
                    0x0580 | f | encode_register(s) as u16
                }
                (Operand::DirectMemory(address), Operand::Reg(d)) => {
                    extensions.extend_from_slice(&(value(address) as u32).to_le_bytes());
                    0x05A0 | f | encode_register(d) as u16
                }
                (Operand::DirectMemory(source), Operand::DirectMemory(destination)) => {
                    extensions.extend_from_slice(&(value(source) as u32).to_le_bytes());
                    extensions.extend_from_slice(&(value(destination) as u32).to_le_bytes());
                    0x05C0 | f
                }
                (Operand::DirectMemory(address), Operand::IndirectPostInc(d)) => {
                    extensions.extend_from_slice(&(value(address) as u32).to_le_bytes());
                    0xD400 | f | encode_register(d) as u16
                }
                _ => {
                    fn register(operand: &Operand) -> Option<&Register> {
                        match operand {
                            Operand::Reg(r) | Operand::Indirect(r) | Operand::IndirectPostInc(r)
                            | Operand::IndirectPreDec(r) | Operand::IndirectOffset(r,_) => Some(r), _ => None,
                        }
                    }
                    let base = match (src, dst) {
                        (Operand::Reg(_), Operand::Indirect(_)) => 0x8000,
                        (Operand::Indirect(_), Operand::Reg(_)) => 0x8400,
                        (Operand::Indirect(_), Operand::Indirect(_)) => 0x8800,
                        (Operand::Reg(_), Operand::IndirectPostInc(_)) => 0x9000,
                        (Operand::IndirectPostInc(_), Operand::Reg(_)) => 0x9400,
                        (Operand::IndirectPostInc(_), Operand::IndirectPostInc(_)) => 0x9800,
                        (Operand::Reg(_), Operand::IndirectPreDec(_)) => 0xA000,
                        (Operand::IndirectPreDec(_), Operand::Reg(_)) => 0xA400,
                        (Operand::IndirectPreDec(_), Operand::IndirectPreDec(_)) => 0xA800,
                        (Operand::Reg(_), Operand::IndirectOffset(_,_)) => 0xB000,
                        (Operand::IndirectOffset(_,_), Operand::Reg(_)) => 0xB400,
                        (Operand::IndirectOffset(_,_), Operand::IndirectOffset(_,_)) => 0xB800,
                        (Operand::IndirectOffset(_,_), Operand::IndirectPostInc(_)) => 0xD000,
                        _ => return Err(EncodeError::UnsupportedInstruction),
                    };
                    for operand in [src,dst] {
                        if let Operand::IndirectOffset(_,expression) = operand {
                            extensions.extend_from_slice(&(value(expression) as u16).to_le_bytes());
                        }
                    }
                    base | f | same_file_registers(register(src).unwrap(),register(dst).unwrap())?
                }
            };
            bytes.extend_from_slice(&opcode.to_le_bytes());
            bytes.extend_from_slice(&extensions);
        }
        ParsedInstruction::Inc { dst } | ParsedInstruction::Dec { dst } => {
            let Operand::Reg(reg) = dst else { return Err(EncodeError::InvalidOperand); };
            let base: u16 = if matches!(instr, ParsedInstruction::Inc { .. }) { 0x1020 } else { 0x1420 };
            bytes.extend_from_slice(&(base | encode_register(reg) as u16).to_le_bytes());
        }
        ParsedInstruction::Andn { src, dst }
        | ParsedInstruction::Movy { src, dst }
        | ParsedInstruction::Movx { src, dst } => {
            let (Operand::Reg(s), Operand::Reg(d)) = (src, dst) else {
                return Err(EncodeError::InvalidOperand);
            };
            let base = match instr {
                ParsedInstruction::Andn { .. } => 0x5200,
                ParsedInstruction::Movx { .. } => 0xEC00,
                _ => 0xEE00,
            };
            bytes.extend_from_slice(&(base | same_file_registers(s, d)?).to_le_bytes());
        }
        ParsedInstruction::RotateLeft { src, dst } => {
            let Operand::Reg(d) = dst else { return Err(EncodeError::InvalidOperand); };
            let opcode = if let Operand::Reg(s) = src {
                0x6800 | same_file_registers(s, d)?
            } else {
                let count = resolve_immediate(src, symbols)?;
                if !(0..=31).contains(&count) { return Err(EncodeError::InvalidOperand); }
                0x3000 | ((count as u16) << 5) | encode_register(d) as u16
            };
            bytes.extend_from_slice(&opcode.to_le_bytes());
        }
        ParsedInstruction::RotateRight { .. } => return Err(EncodeError::UnsupportedInstruction),
        ParsedInstruction::Fill { field } => {
            let opcode: u16 = match field.to_ascii_uppercase().as_str() {
                "L" => 0x0FC0,
                "XY" => 0x0FE0,
                _ => return Err(EncodeError::InvalidOperand),
            };
            bytes.extend_from_slice(&opcode.to_le_bytes());
        }
        ParsedInstruction::Not { dst } => {
            if let Operand::Reg(reg) = dst {
                let mut opcode: u16 = 0x03E0;
                opcode |= (encode_register(reg) as u16) & 0x1F;
                bytes.extend_from_slice(&opcode.to_le_bytes());
            } else {
                return Err(EncodeError::UnsupportedInstruction);
            }
        }
        ParsedInstruction::Pixt { src, dst } => {
            fn pixel_operand(operand: &Operand) -> Result<(&Register, u8), EncodeError> {
                match operand {
                    Operand::Reg(reg) => Ok((reg, 0)),
                    Operand::Indirect(reg) => Ok((reg, 1)),
                    Operand::IndirectField(reg, field) if field.eq_ignore_ascii_case(".XY") => Ok((reg, 2)),
                    _ => Err(EncodeError::InvalidOperand),
                }
            }
            let (s, source_mode) = pixel_operand(src)?;
            let (d, destination_mode) = pixel_operand(dst)?;
            let base = match (source_mode, destination_mode) {
                (0, 2) => 0xF000, (2, 0) => 0xF200, (2, 2) => 0xF400,
                (0, 1) => 0xF800, (1, 0) => 0xFA00, (1, 1) => 0xFC00,
                _ => return Err(EncodeError::InvalidOperand),
            };
            bytes.extend_from_slice(&(base | same_file_registers(s, d)?).to_le_bytes());
        }
        ParsedInstruction::Movb { src, dst } => {
            let absolute = match (src,dst) {
                (Operand::Reg(reg),Operand::DirectMemory(address)) => Some((0x05E0 | encode_register(reg) as u16,vec![address])),
                (Operand::DirectMemory(address),Operand::Reg(reg)) => Some((0x07E0 | encode_register(reg) as u16,vec![address])),
                (Operand::DirectMemory(source),Operand::DirectMemory(destination)) => Some((0x0340,vec![source,destination])),
                _ => None,
            };
            if let Some((opcode,addresses)) = absolute {
                bytes.extend_from_slice(&opcode.to_le_bytes());
                for address in addresses {
                    let value = crate::parser::evaluate_arithmetic(address, |name| symbols.get(name).or_else(|| symbols.get_ignore_case(name)).copied().unwrap_or(0) as i32);
                    bytes.extend_from_slice(&(value as u32).to_le_bytes());
                }
                return Ok(bytes);
            }

            let (base, source, destination) = match (src, dst) {
                (Operand::Reg(s), Operand::Indirect(d)) => (0x8C00, s, d),
                (Operand::Indirect(s), Operand::Reg(d)) => (0x8E00, s, d),
                (Operand::Indirect(s), Operand::Indirect(d)) => (0x9C00, s, d),
                (Operand::Reg(s), Operand::IndirectOffset(d, _)) => (0xAC00, s, d),
                (Operand::IndirectOffset(s, _), Operand::Reg(d)) => (0xAE00, s, d),
                (Operand::IndirectOffset(s, _), Operand::IndirectOffset(d, _)) => (0xBC00, s, d),
                _ => return Err(EncodeError::UnsupportedInstruction),
            };
            bytes.extend_from_slice(&(base | same_file_registers(source, destination)?).to_le_bytes());
            for operand in [src, dst] {
                if let Operand::IndirectOffset(_, expression) = operand {
                    let value = crate::parser::evaluate_arithmetic(expression, |name| {
                        symbols.get(name).or_else(|| symbols.get_ignore_case(name)).copied().unwrap_or(0) as i32
                    });
                    bytes.extend_from_slice(&(value as u16).to_le_bytes());
                }
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
                (src_op, Operand::Reg(d_reg))
                    if matches!(src_op, Operand::Immediate(_) | Operand::Symbol(_)) =>
                {
                    let val = resolve_immediate(src_op, symbols)?;
                    let base_opcode = 0x2900 + (instr_type * 0x0200);
                    let mut opcode: u16 = base_opcode;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= d_val & 0x0F;

                    bytes.extend_from_slice(&opcode.to_le_bytes());
                    bytes.extend_from_slice(&(val as u16).to_le_bytes());
                }
                (Operand::Reg(s_reg), Operand::Reg(d_reg)) => {
                    let base_opcode = match instr {
                        ParsedInstruction::Add { .. } => 0x4000,
                        ParsedInstruction::Sub { .. } => 0x4400,
                        ParsedInstruction::And { .. } => 0x5000,
                        ParsedInstruction::Or { .. } => 0x5400,
                        ParsedInstruction::Xor { .. } => 0x5600,
                        _ => unreachable!(),
                    };
                    let mut opcode: u16 = base_opcode;
                    let s_val = encode_register(s_reg) as u16;
                    let d_val = encode_register(d_reg) as u16;
                    opcode |= (s_val & 0x0F) << 5;
                    opcode |= d_val & 0x0F;
                    opcode |= (s_val | d_val) & 0x10;
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
                (src_op, Operand::Reg(d_reg))
                    if matches!(src_op, Operand::Immediate(_) | Operand::Symbol(_)) =>
                {
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
            let target_addr = symbols
                .get(target)
                .or_else(|| symbols.get_ignore_case(target))
                .copied()
                .unwrap_or(current_addr);

            let short_delta = target_addr as i64 - (current_addr + 2) as i64;
            let short_words = short_delta / 2;
            if short_delta % 2 != 0 {
                return Err(EncodeError::BranchOutOfRange {
                    label: target.clone(),
                    addr: current_addr,
                    offset: short_words as i32,
                });
            }
            encode_conditional_relative(&mut bytes, 0x4, short_words, target_addr, current_addr, target)?;
        }
        ParsedInstruction::Jgt { target } => {
            let target_addr = symbols
                .get(target)
                .or_else(|| symbols.get_ignore_case(target))
                .copied()
                .unwrap_or(current_addr);

            let short_delta = target_addr as i64 - (current_addr + 2) as i64;
            let short_words = short_delta / 2;
            if short_delta % 2 != 0 {
                return Err(EncodeError::BranchOutOfRange {
                    label: target.clone(),
                    addr: current_addr,
                    offset: short_words as i32,
                });
            }
            encode_conditional_relative(&mut bytes, 0x7, short_words, target_addr, current_addr, target)?;
        }
        ParsedInstruction::Cmp { src, dst } => match (src, dst) {
            (src_op, Operand::Reg(d_reg))
                if matches!(src_op, Operand::Immediate(_) | Operand::Symbol(_)) =>
            {
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
        },
        ParsedInstruction::MacroDef { .. } | ParsedInstruction::MacroEnd => {
            // Macro definitions generate no ROM bytes
        }
        ParsedInstruction::Jmp { target } => {
            if let Operand::Reg(reg) = target {
                bytes.extend_from_slice(&(0x0160 | encode_register(reg) as u16).to_le_bytes());
            } else {
                let target_addr = resolve_immediate(target, symbols)? as u32;
                bytes.extend_from_slice(&0xC080u16.to_le_bytes());
                bytes.extend_from_slice(&target_addr.to_le_bytes());
            }
        }
        ParsedInstruction::Reti => { bytes.extend_from_slice(&0x0940u16.to_le_bytes()); }

        ParsedInstruction::Rets => {
            let opcode: u16 = 0x0960;
            bytes.extend_from_slice(&opcode.to_le_bytes());
        }
        ParsedInstruction::Trap { vector } => {
            if !(0..=31).contains(vector) { return Err(EncodeError::InvalidOperand); }
            bytes.extend_from_slice(&(0x0900u16 | *vector as u16).to_le_bytes());
        }
        ParsedInstruction::Dsjs { reg, target, short } => {
            let target_addr = if target == "$" { current_addr } else {
                symbols.get(target).or_else(|| symbols.get_ignore_case(target)).copied().unwrap_or(current_addr)
            };
            let Operand::Reg(reg) = reg else { return Err(EncodeError::InvalidOperand); };
            let delta = target_addr as i64 - (current_addr + if *short { 2 } else { 4 }) as i64;
            if delta % 2 != 0 { return Err(EncodeError::InvalidOperand); }
            let relative = delta / 2;
            if *short {
                if !(-31..=31).contains(&relative) { return Err(EncodeError::InvalidOperand); }
                let opcode = 0x3800 | if relative < 0 { 0x0400 } else { 0 }
                    | ((relative.unsigned_abs() as u16) << 5) | encode_register(reg) as u16;
                bytes.extend_from_slice(&opcode.to_le_bytes());
            } else {
                if !(i16::MIN as i64..=i16::MAX as i64).contains(&relative) { return Err(EncodeError::InvalidOperand); }
                bytes.extend_from_slice(&(0x0D80 | encode_register(reg) as u16).to_le_bytes());
                bytes.extend_from_slice(&(relative as i16).to_le_bytes());
            }
        }

        ParsedInstruction::Calla { target } => {
            let val = resolve_immediate(target, symbols)?;
            let opcode: u16 = 0x0D5F; // Base opcode for absolute call on TMS34010
            bytes.extend_from_slice(&opcode.to_le_bytes());
            bytes.extend_from_slice(&(val as u32 as u16).to_le_bytes());
            bytes.extend_from_slice(&((val as u32 >> 16) as u16).to_le_bytes());
        }
        ParsedInstruction::Btst { src, dst } => {
            match (src, dst) {
                (src_op, Operand::Reg(d_reg))
                    if matches!(src_op, Operand::Immediate(_) | Operand::Symbol(_)) =>
                {
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
                let addr = symbols
                    .get(label)
                    .or_else(|| symbols.get_ignore_case(label))
                    .copied()
                    .unwrap_or(0);
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
pub fn generate_binary(
    ast: &[ParsedInstruction],
    symbols: &SymbolTable,
) -> Result<Vec<u8>, EncodeError> {
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
                if let Some(&val) = symbols
                    .get(inner)
                    .or_else(|| symbols.get_ignore_case(inner))
                {
                    return Ok(!(val as i32));
                }
            }
            if let Ok(n) = s.parse::<i32>() {
                return Ok(n);
            }

            // Handle basic expressions such as arithmetic and bitwise OR.
            let expression = s
                .trim()
                .trim_start_matches('(')
                .trim_end_matches(')')
                .trim();
            if expression.contains('+')
                || expression.contains('-')
                || expression.contains('*')
                || expression.contains('/')
                || expression.contains('|')
            {
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
    let part = part
        .trim()
        .trim_start_matches('#')
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();
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
    } else if part.len() > 1 && (part.ends_with('B') || part.ends_with('b'))
        && part[..part.len() - 1].chars().all(|ch| matches!(ch, '0' | '1'))
    {
        i32::from_str_radix(&part[..part.len() - 1], 2)
            .map_err(|_| EncodeError::UndefinedSymbol(part.to_string()))
    } else if part.contains('*') {
        let pieces: Vec<&str> = part.splitn(2, '*').collect();
        if pieces.len() == 2 {
            return Ok(resolve_immed_expr_part(pieces[0].trim(), symbols)?
                * resolve_immed_expr_part(pieces[1].trim(), symbols)?);
        }
        Err(EncodeError::UndefinedSymbol(part.to_string()))
    } else if part.contains('/') {
        let pieces: Vec<&str> = part.splitn(2, '/').collect();
        if pieces.len() == 2 {
            let divisor = resolve_immed_expr_part(pieces[1].trim(), symbols)?;
            return if divisor == 0 {
                Ok(0)
            } else {
                Ok(resolve_immed_expr_part(pieces[0].trim(), symbols)? / divisor)
            };
        }
        Err(EncodeError::UndefinedSymbol(part.to_string()))
    } else if part.contains('+') {
        let pieces: Vec<&str> = part.splitn(2, '+').collect();
        if pieces.len() == 2 {
            return Ok(resolve_immed_expr_part(pieces[0].trim(), symbols)?
                + resolve_immed_expr_part(pieces[1].trim(), symbols)?);
        }
        Err(EncodeError::UndefinedSymbol(part.to_string()))
    } else if part.contains('-') && !part.starts_with('-') {
        let pieces: Vec<&str> = part.split('-').collect();
        if pieces.len() == 2 {
            return Ok(resolve_immed_expr_part(pieces[0].trim(), symbols)?
                - resolve_immed_expr_part(pieces[1].trim(), symbols)?);
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

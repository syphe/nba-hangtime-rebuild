use crate::encoder::{encode_instruction, EncodeError};
use crate::parser::{Operand, ParsedInstruction, Register};
use crate::SymbolTable;

fn word(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn fmt_operand(op: &Operand) -> String {
    match op {
        Operand::Reg(r) | Operand::Indirect(r) => {
            let s = match r {
                Register::A(n) => format!("A{}", n),
                Register::B(n) => format!("B{}", n),
            };
            if matches!(op, Operand::Indirect(_)) {
                format!("*{}", s)
            } else {
                s
            }
        }
        Operand::Immediate(v) => format!("#${:04X}", *v as u16),
        Operand::Symbol(s) => format!("#{}", s),
        Operand::DirectMemory(s) => format!("${}", s),
        Operand::IndirectPostInc(r) => {
            let s = match r {
                Register::A(n) => format!("A{}", n),
                Register::B(n) => format!("B{}", n),
            };
            format!("*{}+", s)
        }
        Operand::IndirectPreDec(r) => {
            let s = match r {
                Register::A(n) => format!("A{}", n),
                Register::B(n) => format!("B{}", n),
            };
            format!("*-{}", s)
        }
        Operand::IndirectOffset(reg, offset) => {
            let s = match reg {
                Register::A(n) => format!("A{}", n),
                Register::B(n) => format!("B{}", n),
            };
            format!("*{}({})", s, offset)
        }
        Operand::IndirectField(reg, field) => {
            let s = match reg {
                Register::A(n) => format!("A{}", n),
                Register::B(n) => format!("B{}", n),
            };
            format!("*{}{}", s, field)
        }
    }
}

fn describe(instr: &ParsedInstruction) -> String {
    match instr {
        ParsedInstruction::Nop => "NOP".to_string(),
        ParsedInstruction::Label(l) => format!("{}:", l),
        ParsedInstruction::Equ { name, value } => format!(".equ {}, {}", name, value),
        ParsedInstruction::Directive { name, args } => format!("{} {}", name, args.join(", ")),
        ParsedInstruction::Move { src, dst, field } => {
            if let Some(f) = field {
                format!("MOVE {}, {}, {}", fmt_operand(src), fmt_operand(dst), fmt_operand(f))
            } else {
                format!("MOVE {}, {}", fmt_operand(src), fmt_operand(dst))
            }
        }
        ParsedInstruction::Add { src, dst } => format!("ADD {}, {}", fmt_operand(src), fmt_operand(dst)),
        ParsedInstruction::Sub { src, dst } => format!("SUB {}, {}", fmt_operand(src), fmt_operand(dst)),
        ParsedInstruction::Sll { src, dst } => format!("SLL {}, {}", fmt_operand(src), fmt_operand(dst)),
        ParsedInstruction::Srl { src, dst } => format!("SRL {}, {}", fmt_operand(src), fmt_operand(dst)),
        ParsedInstruction::Sla { src, dst } => format!("SLA {}, {}", fmt_operand(src), fmt_operand(dst)),
        ParsedInstruction::Sra { src, dst } => format!("SRA {}, {}", fmt_operand(src), fmt_operand(dst)),
        ParsedInstruction::Jr { label } => format!("JR {}", label),
        ParsedInstruction::Call { label } => format!("CALL {}", label),
        ParsedInstruction::Jeq { label } => format!("JEQ {}", label),
        ParsedInstruction::Jne { label } => format!("JNE {}", label),
        ParsedInstruction::Cmp { src, dst } => format!("CMP {}, {}", fmt_operand(src), fmt_operand(dst)),
        ParsedInstruction::Jlt { target } => format!("JLT {}", target),
        ParsedInstruction::Jgt { target } => format!("JGT {}", target),
        ParsedInstruction::MacroDef { name, params } => format!(".macro {} {}", name, params.join(", ")),
        ParsedInstruction::MacroEnd => ".endm".to_string(),
        ParsedInstruction::And { src, dst } => format!("AND {}, {}", fmt_operand(src), fmt_operand(dst)),
        ParsedInstruction::Or { src, dst } => format!("OR {}, {}", fmt_operand(src), fmt_operand(dst)),
        ParsedInstruction::Xor { src, dst } => format!("XOR {}, {}", fmt_operand(src), fmt_operand(dst)),
        ParsedInstruction::Jmp { target } => format!("JMP {}", fmt_operand(target)),
        ParsedInstruction::Rets => "RETS".to_string(),
        ParsedInstruction::Usect { name, section, args } => format!(".usect {} {} {}", name, section, args.join(", ")),
        ParsedInstruction::Trap { vector } => format!("TRAP {}", vector),
        ParsedInstruction::Not { dst } => format!("NOT {}", fmt_operand(dst)),
        ParsedInstruction::Movb { src, dst } => format!("MOVB {}, {}", fmt_operand(src), fmt_operand(dst)),
        ParsedInstruction::Pixt { src, dst } => format!("PIXT {}, {}", fmt_operand(src), fmt_operand(dst)),
        ParsedInstruction::Dsjs { reg, target } => format!("DSJS {}, {}", fmt_operand(reg), target),
        ParsedInstruction::Calla { target } => format!("CALLA {}", fmt_operand(target)),
        ParsedInstruction::Btst { src, dst } => format!("BTST {}, {}", fmt_operand(src), fmt_operand(dst)),
        ParsedInstruction::Callr { label } => format!("CALLR {}", label),
        ParsedInstruction::Movk { src, dst } => format!("MOVK {}, {}", fmt_operand(src), fmt_operand(dst)),
        ParsedInstruction::Xor { src, dst } => format!("XORI {}, {}", fmt_operand(src), fmt_operand(dst)),
        ParsedInstruction::Exgpc(reg) => {
            let reg_str = match reg {
                Register::A(n) => format!("A{}", n),
                Register::B(n) => format!("B{}", n),
            };
            format!("EXGPC {}", reg_str)
        }
        ParsedInstruction::Setf { size, sign, offset } => format!(
            "SETF {}, {}, {}", 
            fmt_operand(size), 
            fmt_operand(sign), 
            fmt_operand(offset)
        ),
    }
}

/// Word-level hex listing of the generated binary, in lockstep with the AST.
pub fn dump_listing(ast: &[ParsedInstruction], binary: &[u8], symbols: &SymbolTable) -> Result<String, EncodeError> {
    let mut out = String::new();
    let mut addr: u32 = 0;

    out.push_str("  ADDR   BYTES          WORDS          SOURCE\n");
    out.push_str("  ----   -----          -----          ------\n");

    for instr in ast {
        let encoded = encode_instruction(instr, addr, symbols)?;
        let slice = &binary[addr as usize..addr as usize + encoded.len()];

        let bytes: Vec<String> = slice.iter().map(|b| format!("{:02X}", b)).collect();
        let words: Vec<String> = slice.chunks(2).map(|w| format!("{:04X}", word(w))).collect();

        out.push_str(&format!(
            "  {:04X}   {:<15}  {:<15}  {}\n",
            addr,
            bytes.join(" "),
            words.join(" "),
            describe(instr)
        ));

        addr += encoded.len() as u32;
    }

    out.push_str(&format!("\n  Total: {} bytes\n", binary.len()));
    Ok(out)
}

/// Re-decodes the emitted binary and checks it against the symbol table.
pub fn verify(ast: &[ParsedInstruction], binary: &[u8], symbols: &SymbolTable) -> Result<Vec<(bool, String)>, EncodeError> {
    let mut results: Vec<(bool, String)> = Vec::new();
    let mut addr: u32 = 0;

    results.push((
        binary.len() % 2 == 0,
        format!("binary length {} bytes is word-aligned", binary.len()),
    ));

    for instr in ast {
        let encoded = encode_instruction(instr, addr, symbols)?;
        let slice = &binary[addr as usize..addr as usize + encoded.len()];

        match instr {
            ParsedInstruction::Jr { label }
            | ParsedInstruction::Call { label }
            | ParsedInstruction::Jeq { label }
            | ParsedInstruction::Jne { label } => {
                let w = word(slice);
                let offset = (w & 0x00FF) as i8;
                let target = (addr as i32 + 2) + (offset as i32) * 2;
                let expected = *symbols
                    .get(label)
                    .ok_or_else(|| EncodeError::UndefinedLabel(label.clone()))?;
                results.push((
                    target == expected as i32,
                    format!(
                        "{} @ {:04X}: offset {:>3} ({} words) -> target {:04X}, symbol {:04X}",
                        describe(instr),
                        addr,
                        offset,
                        offset,
                        target,
                        expected
                    ),
                ));
            }
            ParsedInstruction::Move { src, dst, field } => {
            if matches!(src, Operand::Immediate(_) | Operand::Symbol(_)) {
                let expected = crate::encoder::resolve_immediate(src, symbols)?;
                // If a field specifier extension word is present, shift the immediate operand word offset
                let offset = if field.is_some() { 4 } else { 2 };
                let decoded = word(&slice[offset..offset + 2]) as i32;
                results.push((
                    decoded == expected,
                    format!(
                        "{} @ {:04X}: operand word {:04X} = {}, expected {}",
                        describe(instr),
                        addr,
                        word(&slice[offset..offset + 2]),
                        decoded,
                        expected
                    ),
                ));
            }
            let _ = (dst, field);
        }
        ParsedInstruction::Add { src, dst }
        | ParsedInstruction::Sub { src, dst }
        | ParsedInstruction::Sll { src, dst }
        | ParsedInstruction::Srl { src, dst } => {
            if matches!(src, Operand::Immediate(_) | Operand::Symbol(_)) {
                let expected = crate::encoder::resolve_immediate(src, symbols)?;
                let decoded = word(&slice[2..4]) as i32;
                results.push((
                    decoded == expected,
                    format!(
                        "{} @ {:04X}: operand word {:04X} = {}, expected {}",
                        describe(instr),
                        addr,
                        word(&slice[2..4]),
                        decoded,
                        expected
                    ),
                ));
            }
            let _ = dst;
        }
        ParsedInstruction::Directive { name, args } => {
                if name.eq_ignore_ascii_case(".WORD") {
                    for (i, arg) in args.iter().enumerate() {
                        let w = word(&slice[i * 2..i * 2 + 2]);
                        let expected = if let Some(hex) = arg.strip_prefix('$') {
                            u16::from_str_radix(hex, 16).ok()
                        } else if let Some(hex) = arg.strip_prefix("0x") {
                            u16::from_str_radix(hex, 16).ok()
                        } else if let Some(sym) = symbols.get(arg) {
                            Some(*sym as u16)
                        } else {
                            arg.parse::<i32>().ok().map(|v| v as u16)
                        };
                        results.push((
                            expected == Some(w),
                            format!(
                                ".WORD {} @ {:04X}: decoded {:04X}, expected {:?}",
                                arg,
                                addr + (i * 2) as u32,
                                w,
                                expected
                            ),
                        ));
                    }
                }
            }
            _ => {}
        }

        addr += encoded.len() as u32;
    }

    results.push((
        addr as usize == binary.len(),
        format!("pass-1 address {:04X} matches binary length {}", addr, binary.len()),
    ));

    Ok(results)
}

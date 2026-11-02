//! Audited TMS34010 scalar instructions. Opcode bases and reserved bits follow
//! MAME's 34010tbl.hxx; immediate complements follow 34010ops.hxx.
use crate::encoder::{encode_register, resolve_immediate, same_file_registers, EncodeError};
use crate::parser::{self, Operand, ParsedInstruction};
use crate::SymbolTable;

#[derive(Clone, Debug, PartialEq)]
pub enum Format {
    Unary(u16),
    Pair(u16),
    Fixed(u16),
    Field(u16),
    Constant(u16),
    Immediate {
        short: Option<u16>,
        long: u16,
        complement: bool,
    },
    Alu {
        register: u16,
        short: Option<u16>,
        long: u16,
        complement: bool,
    },
    Setf,
    Exgf,
    Line,
    Pixblt,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NativeInstruction {
    pub format: Format,
    pub operands: Vec<Operand>,
    pub width: Option<u8>,
}

pub fn format(name: &str) -> Option<Format> {
    use Format::*;
    Some(match name.to_ascii_uppercase().as_str() {
        "GETPC" => Unary(0x0140),
        "GETST" => Unary(0x0180),
        "PUTST" => Unary(0x01a0),
        "ABS" => Unary(0x0380),
        "NEG" => Unary(0x03a0),
        "NEGB" => Unary(0x03c0),
        "REV" => Unary(0x0020),
        "ADDC" => Pair(0x4200),
        "SUBB" => Pair(0x4600),
        "DIVS" => Pair(0x5800),
        "DIVU" => Pair(0x5a00),
        "MPYS" => Pair(0x5c00),
        "MPYU" => Pair(0x5e00),
        "LMO" => Pair(0x6a00),
        "MODS" => Pair(0x6c00),
        "MODU" => Pair(0x6e00),
        "ADDXY" => Pair(0xe000),
        "SUBXY" => Pair(0xe200),
        "CMPXY" => Pair(0xe400),
        "CPW" => Pair(0xe600),
        "CVXYL" => Pair(0xe800),
        "DRAV" => Pair(0xf600),
        "CLRC" => Fixed(0x0320),
        "SETC" => Fixed(0x0de0),
        "DINT" => Fixed(0x0360),
        "EINT" => Fixed(0x0d60),
        "PUSHST" => Fixed(0x01e0),
        "POPST" => Fixed(0x01c0),
        "SEXT" => Field(0x0500),
        "ZEXT" => Field(0x0520),
        "ADDK" => Constant(0x1000),
        "SUBK" => Constant(0x1400),
        "MOVK" => Constant(0x1800),
        "ADDI" => Immediate {
            short: Some(0x0b00),
            long: 0x0b20,
            complement: false,
        },
        "SUBI" => Immediate {
            short: Some(0x0be0),
            long: 0x0d00,
            complement: true,
        },
        "CMPI" => Immediate {
            short: Some(0x0b40),
            long: 0x0b60,
            complement: true,
        },
        "ANDI" => Immediate {
            short: None,
            long: 0x0b80,
            complement: true,
        },
        "ANDNI" => Immediate {
            short: None,
            long: 0x0b80,
            complement: false,
        },
        "ORI" => Immediate {
            short: None,
            long: 0x0ba0,
            complement: false,
        },
        "XORI" => Immediate {
            short: None,
            long: 0x0bc0,
            complement: false,
        },
        "ADD" => Alu {
            register: 0x4000,
            short: Some(0x0b00),
            long: 0x0b20,
            complement: false,
        },
        "SUB" => Alu {
            register: 0x4400,
            short: Some(0x0be0),
            long: 0x0d00,
            complement: true,
        },
        "CMP" => Alu {
            register: 0x4800,
            short: Some(0x0b40),
            long: 0x0b60,
            complement: true,
        },
        "AND" => Alu {
            register: 0x5000,
            short: None,
            long: 0x0b80,
            complement: true,
        },
        "OR" => Alu {
            register: 0x5400,
            short: None,
            long: 0x0ba0,
            complement: false,
        },
        "XOR" => Alu {
            register: 0x5600,
            short: None,
            long: 0x0bc0,
            complement: false,
        },
        "SETF" => Setf,
        "EXGF" => Exgf,
        "LINE" => Line,
        "PIXBLT" => Pixblt,
        _ => return None,
    })
}

pub fn parse(input: &str) -> nom::IResult<&str, ParsedInstruction> {
    let (name, tail) = input.split_once(char::is_whitespace).unwrap_or((input, ""));
    let invalid = || nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Tag));
    let format = format(name).ok_or_else(invalid)?;
    let mut args = parser::split_asm_operands(tail);
    let width =
        if matches!(format, Format::Immediate { .. } | Format::Alu { .. }) && args.len() == 3 {
            Some(match args.pop().unwrap().to_ascii_uppercase().as_str() {
                "W" => 2,
                "L" => 4,
                _ => return Err(invalid()),
            })
        } else {
            None
        };
    let valid_count = match format {
        Format::Unary(_) => args.len() == 1,
        Format::Fixed(_) => args.is_empty(),
        Format::Field(_) => (1..=2).contains(&args.len()),
        Format::Setf => (2..=3).contains(&args.len()),
        Format::Line => args.len() == 1,
        _ => args.len() == 2,
    };
    if !valid_count {
        return Err(invalid());
    }
    let operands = if matches!(format, Format::Pixblt) {
        args.into_iter()
            .map(|s| Operand::Symbol(s.to_ascii_uppercase()))
            .collect()
    } else {
        let mut result = Vec::new();
        for arg in args {
            let (rest, op) = parser::parse_operand(&arg).map_err(|_| invalid())?;
            if !rest.trim().is_empty() {
                return Err(invalid());
            }
            result.push(op);
        }
        result
    };
    Ok((
        "",
        ParsedInstruction::Native(NativeInstruction {
            format,
            operands,
            width,
        }),
    ))
}

impl NativeInstruction {
    /// Relocate the actual encoded immediate, including one's complement.
    pub fn immediate_fixup(&self) -> Option<(String, u32)> {
        let complement = match self.format {
            Format::Immediate { complement, .. } => complement,
            Format::Alu { complement, .. }
                if !matches!(self.operands.first(), Some(Operand::Reg(_))) =>
            {
                complement
            }
            _ => return None,
        };
        let Operand::Symbol(expression) = self.operands.first()? else {
            return None;
        };
        Some((
            if complement {
                format!("(~({expression}))&0FFFFFFFFh")
            } else {
                expression.clone()
            },
            self.width.unwrap_or(4) as u32,
        ))
    }

    pub fn encode(&self, symbols: &SymbolTable) -> Result<Vec<u8>, EncodeError> {
        use Format::*;
        let invalid = EncodeError::InvalidOperand;
        let reg = |index| match self.operands.get(index) {
            Some(Operand::Reg(r)) => Ok(r),
            _ => Err(EncodeError::InvalidOperand),
        };
        let val = |index| {
            self.operands
                .get(index)
                .ok_or(EncodeError::InvalidOperand)
                .and_then(|v| resolve_immediate(v, symbols))
        };
        let field = |index| -> Result<u16, EncodeError> {
            let n = if self.operands.len() > index {
                val(index)?
            } else {
                0
            };
            if !(0..=1).contains(&n) {
                return Err(EncodeError::InvalidOperand);
            }
            Ok((n as u16) << 9)
        };
        let mut extra = Vec::new();
        let word = match self.format {
            Unary(base) => base | encode_register(reg(0)?) as u16,
            Pair(base) => base | same_file_registers(reg(0)?, reg(1)?)?,
            Fixed(base) => base,
            Field(base) => base | field(1)? | encode_register(reg(0)?) as u16,
            Constant(base) => {
                let n = val(0)?;
                // Unknown/forward equates get a stable two-byte layout; final
                // pass two still enforces the architectural constant range.
                if !(1..=32).contains(&n) {
                    return Err(invalid);
                }
                base | ((n as u16 & 31) << 5) | encode_register(reg(1)?) as u16
            }
            Alu { register, .. } if matches!(self.operands[0], Operand::Reg(_)) => {
                if self.width.is_some() {
                    return Err(invalid);
                }
                register | same_file_registers(reg(0)?, reg(1)?)?
            }
            Immediate {
                short,
                long,
                complement,
            }
            | Alu {
                short,
                long,
                complement,
                ..
            } => {
                let n = val(0)?;
                if self.width == Some(2) && !(i16::MIN as i32..=i16::MAX as i32).contains(&n) {
                    return Err(invalid);
                }
                let n = if complement { !n } else { n };
                let base = if self.width == Some(2) {
                    extra.extend_from_slice(&(n as u16).to_le_bytes());
                    short.ok_or(EncodeError::InvalidOperand)?
                } else {
                    extra.extend_from_slice(&(n as u32).to_le_bytes());
                    long
                };
                base | encode_register(reg(1)?) as u16
            }
            Setf => {
                let size = val(0)?;
                let sign = val(1)?;
                if !(1..=32).contains(&size) || !(0..=1).contains(&sign) {
                    return Err(invalid);
                }
                0x0540 | field(2)? | ((sign as u16) << 5) | (size as u16 & 31)
            }
            Exgf => 0xd500 | field(1)? | encode_register(reg(0)?) as u16,
            Line => match val(0)? {
                0 => 0xdf1a,
                1 => 0xdf9a,
                _ => return Err(invalid),
            },
            Pixblt => {
                let (Operand::Symbol(a), Operand::Symbol(b)) =
                    (&self.operands[0], &self.operands[1])
                else {
                    return Err(invalid);
                };
                match (a.as_str(), b.as_str()) {
                    ("L", "L") => 0x0f00,
                    ("L", "XY") => 0x0f20,
                    ("XY", "L") => 0x0f40,
                    ("XY", "XY") => 0x0f60,
                    ("B", "L") => 0x0f80,
                    ("B", "XY") => 0x0fa0,
                    _ => return Err(invalid),
                }
            }
        };
        let mut result = word.to_le_bytes().to_vec();
        result.extend(extra);
        Ok(result)
    }
}

use nom::{
    branch::alt,
    bytes::complete::{tag_no_case, take_while1, take_while, tag},
    character::complete::{alphanumeric1, char, multispace0, multispace1, satisfy, u32 as parse_u32, space0, space1},
    combinator::{map, opt},
    multi::separated_list1,
    sequence::{delimited, preceded, separated_pair},
    IResult,
};

use either::Either;

use nom::sequence::tuple;

/// TMS34010 Register banks
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Register {
    A(u8), // A0 - A15
    B(u8), // B0 - B15
}

/// Instruction Operands
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Reg(Register),
    Immediate(i32),
    Indirect(Register), // e.g., *A0
    Symbol(String),
    IndirectPostInc(Register),
    IndirectPreDec(Register),
    DirectMemory(String), 
    IndirectOffset(Register, i32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParsedInstruction {
    Nop,
    Move { src: Operand, dst: Operand },
    Add { src: Operand, dst: Operand },
    Sub { src: Operand, dst: Operand },
    Label(String),
    Directive { name: String, args: Vec<String> },
    Sll { src: Operand, dst: Operand }, // Shift Left Logical
    Srl { src: Operand, dst: Operand }, // Shift Right Logical
    Jr { label: String },
    Call { label: String },
    Jeq { label: String },
    Jne { label: String },
    Equ { name: String, value: i32 },
    Cmp { src: Operand, dst: Operand },
    Jlt { target: String },
    Jgt { target: String },
    MacroDef { name: String, params: Vec<String> },
    MacroEnd, // for .endm
    And { src: Operand, dst: Operand },
    Or  { src: Operand, dst: Operand },
    Xor { src: Operand, dst: Operand },
    Jmp { target: Operand },
    Rets,
    Usect { 
        name: String, 
        section: String, 
        args: Vec<String> 
    },
}

#[derive(Clone, Debug)]
pub struct MacroTemplate {
    pub name: String,
    pub params: Vec<String>,
    pub body: Vec<String>,
}

// --- PARSERS ---

/// Parse a register like A0-A15 or B0-B15 (case-insensitive)
pub fn parse_register(input: &str) -> IResult<&str, Register> {
    let (input, bank) = satisfy(|c| c == 'a' || c == 'A' || c == 'b' || c == 'B')(input)?;
    let (input, num) = parse_u32(input)?;

    if num > 15 {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Verify,
        )));
    }

    let reg = match bank.to_ascii_lowercase() {
        'a' => Register::A(num as u8),
        'b' => Register::B(num as u8),
        _ => unreachable!(),
    };

    Ok((input, reg))
}

pub fn parse_number(input: &str) -> IResult<&str, i32> {
    alt((
        // Hex starting with $
        map(
            preceded(char('$'), take_while1(|c: char| c.is_ascii_hexdigit())),
            |s| i32::from_str_radix(s, 16).unwrap_or(0),
        ),
        // Hex starting with 0x
        map(
            preceded(tag_no_case("0x"), take_while1(|c: char| c.is_ascii_hexdigit())),
            |s| i32::from_str_radix(s, 16).unwrap_or(0),
        ),
        // Decimal number with explicit &str type annotation
        map(
            take_while1(|c: char| c.is_ascii_digit() || c == '-'),
            |s: &str| s.parse::<i32>().unwrap_or(0),
        ),
    ))(input)
}

/// Parse an immediate value prefixed with '#' (e.g., "#1" or "#$0100")
pub fn parse_immediate(input: &str) -> IResult<&str, Operand> {
    let (input, _) = char('#')(input)?;
    let (input, val) = parse_number(input)?;
    Ok((input, Operand::Immediate(val)))
}

// pub fn parse_operand(input: &str) -> IResult<&str, Operand> {
//     alt((
//         // Parse register (e.g., A0)
//         map(parse_register, Operand::Reg),
//         // Parse immediate with symbol support (e.g., #SCREEN_WIDTH or #384)
//         map(
//             preceded(
//                 char('#'),
//                 alt((
//                     // Try parsing as a number first
//                     map(parse_number, |n| Either::Left(n)),
//                     // Otherwise parse as an alphanumeric symbol identifier
//                     map(take_while1(|c: char| c.is_alphanumeric() || c == '_'), |s: &str| Either::Right(s.to_string())),
//                 ))
//             ),
//             |val| match val {
//                 Either::Left(n) => Operand::Immediate(n),
//                 Either::Right(s) => Operand::Symbol(s),
//             }
//         ),
//     ))(input)
// }

pub fn parse_operand(input: &str) -> IResult<&str, Operand> {
    alt((
        // 1. Direct Register: A0
        map(parse_register, Operand::Reg),
        
        // 2. Immediate / Symbol: #384 or #SCREEN_WIDTH / #$0100
        map(
            preceded(char('#'), take_while1(|c: char| c.is_alphanumeric() || c == '_' || c == '$' || c == '-')),
            |token: &str| {
                if let Ok(n) = token.parse::<i32>() {
                    Operand::Immediate(n)
                } else if let Some(hex) = token.strip_prefix('$') {
                    Operand::Immediate(i32::from_str_radix(hex, 16).unwrap_or(0))
                } else {
                    Operand::Symbol(token.to_string())
                }
            }
        ),

        // 3. Direct Memory / Absolute: @label
        map(
            preceded(char('@'), take_while1(|c: char| c.is_alphanumeric() || c == '_')),
            |s: &str| Operand::DirectMemory(s.to_string())
        ),

        // 4. Indirect Post-Increment: *A0+
        map(
            tuple((char('*'), parse_register, char('+'))),
            |(_, reg, _)| Operand::IndirectPostInc(reg)
        ),

        // 5. Indirect Pre-Decrement: -*A0
        map(
            tuple((char('-'), char('*'), parse_register)),
            |(_, _, reg)| Operand::IndirectPreDec(reg)
        ),

        // 6. Indirect with Offset: *A0(16) -> Reference it here!
        parse_indirect_offset,

        // 7. Simple Register Indirect: *A0
        map(
            preceded(char('*'), parse_register),
            Operand::Indirect
        ),
    ))(input)
}

/// Parse the NOP instruction
pub fn parse_nop(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("NOP")(input)?;
    Ok((input, ParsedInstruction::Nop))
}

/// Parse the MOVE instruction (e.g., "MOVE A0, A1")
pub fn parse_move(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("MOVE")(input)?;
    let (input, _) = multispace1(input)?;
    
    // Parse source and destination separated by a comma and optional whitespace
    let (input, (src, dst)) = separated_pair(
        parse_operand,
        delimited(multispace0, char(','), multispace0),
        parse_operand,
    )(input)?;

    Ok((input, ParsedInstruction::Move { src, dst }))
}

/// Helper to clean comments and whitespace from a line before parsing
// pub fn sanitize_line(input: &str) -> &str {
//     // Split by ';' and take the first part (the code)
//     let code_part = input.split(';').next().unwrap_or(input);
//     code_part.trim()
// }

pub fn sanitize_line(input: &str) -> String {
    input
        .chars()
        // Filter out Ctrl+Z (EOF) and other non-printable control characters (except tab/newline/cr)
        .filter(|&c| c != '\u{001A}' && (c >= ' ' || c == '\t' || c == '\n' || c == '\r'))
        .collect::<String>()
        // Also strip comments starting with ';' or '*'
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

pub fn parse_line(input: &str) -> IResult<&str, ParsedInstruction> {
    // No local String allocation here; parse directly against the input slice
    alt((
        alt((
            parse_nop,
            parse_move,
            parse_add,
            parse_sub,
            parse_jr,
            parse_call,
            parse_jeq,
            parse_jne,
            parse_directive,
            parse_label,
            parse_shift,
            parse_equ,
        )),
        alt((
            parse_cmp,
            parse_jgt,
            parse_jlt,
            parse_set,
            parse_macro_def,
            parse_macro_end,
            parse_and,
            parse_or,
            parse_xor,
            parse_jmp,
            parse_rets,
            // parse_usect,
            parse_addi,
        )),
    ))(input)
}

/// Parse a valid assembly label identifier (alphanumeric + underscores)
pub fn parse_identifier(input: &str) -> IResult<&str, &str> {
    take_while1(|c: char| c.is_ascii_alphanumeric() || c == '_')(input)
}

/// Parse a JR (Jump Relative) instruction (e.g., "JR START_LOOP")
pub fn parse_jr(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("JR")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, label) = parse_identifier(input)?; 
    Ok((input, ParsedInstruction::Jr { label: label.to_string() }))
}

/// Parse a CALL instruction (e.g., "CALL SUBROUTINE")
pub fn parse_call(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("CALL")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, label) = parse_identifier(input)?;
    Ok((input, ParsedInstruction::Call { label: label.to_string() }))
}

/// Parse ADD instruction (e.g., "ADD A0, A1")
pub fn parse_add(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("ADD")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, (src, dst)) = separated_pair(
        parse_operand,
        delimited(multispace0, char(','), multispace0),
        parse_operand,
    )(input)?;
    Ok((input, ParsedInstruction::Add { src, dst }))
}

/// Parse SUB instruction (e.g., "SUB A2, A3")
pub fn parse_sub(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("SUB")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, (src, dst)) = separated_pair(
        parse_operand,
        delimited(multispace0, char(','), multispace0),
        parse_operand,
    )(input)?;
    Ok((input, ParsedInstruction::Sub { src, dst }))
}

/// Parse JEQ (Jump if Equal) instruction
pub fn parse_jeq(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("JEQ")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, label) = parse_identifier(input)?;
    Ok((input, ParsedInstruction::Jeq { label: label.to_string() }))
}

/// Parse JNE (Jump if Not Equal) instruction
pub fn parse_jne(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("JNE")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, label) = parse_identifier(input)?;
    Ok((input, ParsedInstruction::Jne { label: label.to_string() }))
}

/// Parse a label definition ending with a colon (e.g., "INIT:")
pub fn parse_label(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, label_name) = parse_identifier(input)?;
    let (input, _) = char(':')(input)?;
    Ok((input, ParsedInstruction::Label(label_name.to_string())))
}

/// Parse a directive like `.WORD 1234` or `.WORD $BEEF`
pub fn parse_directive(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, name) = alt((tag_no_case(".WORD"), tag_no_case(".BYTE")))(input)?;
    let (input, _) = multispace1(input)?;
    
    // Parse comma-separated arguments following the directive
    let (input, args) = separated_list1(
        delimited(multispace0, char(','), multispace0),
        take_while1(|c: char| !c.is_whitespace() && c != ';')
    )(input)?;

    Ok((
        input,
        ParsedInstruction::Directive {
            name: name.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        },
    ))
}

pub fn parse_shift(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, mnemonic) = alt((tag_no_case("SLL"), tag_no_case("SRL")))(input)?;
    let (input, _) = multispace1(input)?;
    let (input, src) = parse_operand(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?; // Use parse_operand here instead of parse_register

    let is_sll = mnemonic.eq_ignore_ascii_case("SLL");
    Ok((
        input,
        if is_sll {
            ParsedInstruction::Sll { src, dst }
        } else {
            ParsedInstruction::Srl { src, dst }
        },
    ))
}

pub fn parse_equ(input: &str) -> IResult<&str, ParsedInstruction> {
    map(
        tuple((
            // 1. Symbol name
            take_while1(|c: char| c.is_alphanumeric() || c == '_'),
            space1, 
            // 2. Directive: .equ or equ
            alt((
                tag_no_case(".equ"),
                tag_no_case("equ"),
            )),
            space1,
            // 3. Value: Consume everything to the end of the line (excluding comments)
            take_while1(|c: char| c != ';' && c != '\r' && c != '\n'),
        )),
        |(name, _, _, _, val_str)| {
            let value = parse_number_literal(val_str);
            ParsedInstruction::Equ {
                name: name.to_string(),
                value,
            }
        },
    )(input)
}

// Enhanced numeric & coordinate literal parser
fn parse_number_literal(s: &str) -> i32 {
    let s = s.trim();
    
    // Handle coordinate pairs like [0, -32]
    if s.starts_with('[') && s.ends_with(']') {
        let inner = &s[1..s.len() - 1];
        let parts: Vec<&str> = inner.split(',').map(|p| p.trim()).collect();
        if parts.len() == 2 {
            let y = parse_number_literal(parts[0]);
            let x = parse_number_literal(parts[1]);
            // Pack 16-bit Y (upper) and 16-bit X (lower) into a 32-bit value
            return ((y & 0xFFFF) << 16) | (x & 0xFFFF);
        }
    }

    if let Some(hex_str) = s.strip_prefix('$') {
        i32::from_str_radix(hex_str, 16).unwrap_or(0)
    } else if let Some(hex_str) = s.strip_prefix("0x") {
        i32::from_str_radix(hex_str, 16).unwrap_or(0)
    } else {
        s.parse::<i32>().unwrap_or(0)
    }
}

// Parse CMP instruction: CMP src, dst
pub fn parse_cmp(input: &str) -> IResult<&str, ParsedInstruction> {
    map(
        tuple((
            tag_no_case("CMP"),
            space1,
            parse_operand,
            char(','),
            space0,
            parse_operand,
        )),
        |(_, _, src, _, _, dst)| ParsedInstruction::Cmp { src, dst },
    )(input)
}

// Parse JLT instruction: JLT target
pub fn parse_jlt(input: &str) -> IResult<&str, ParsedInstruction> {
    map(
        preceded(
            tuple((tag_no_case("JLT"), space1)),
            take_while1(|c: char| c.is_alphanumeric() || c == '_'),
        ),
        |target: &str| ParsedInstruction::Jlt {
            target: target.to_string(),
        },
    )(input)
}

// Parse JGT instruction: JGT target
pub fn parse_jgt(input: &str) -> IResult<&str, ParsedInstruction> {
    map(
        preceded(
            tuple((tag_no_case("JGT"), space1)),
            take_while1(|c: char| c.is_alphanumeric() || c == '_'),
        ),
        |target: &str| ParsedInstruction::Jgt {
            target: target.to_string(),
        },
    )(input)
}

pub fn parse_set(input: &str) -> IResult<&str, ParsedInstruction> {
    map(
        tuple((
            take_while1(|c: char| c.is_alphanumeric() || c == '_'),
            preceded(space0, tag_no_case(".set")),
            preceded(
                space1,
                take_while1(|c: char| c.is_alphanumeric() || c == '_' || c == '$' || c == '-'),
            ),
        )),
        |(name, _, val_str): (&str, &str, &str)| {
            let value = if let Ok(n) = val_str.parse::<i32>() {
                n
            } else if let Some(hex) = val_str.strip_prefix('$') {
                i32::from_str_radix(hex, 16).unwrap_or(0)
            } else {
                0
            };
            ParsedInstruction::Equ {
                name: name.to_string(),
                value,
            }
        },
    )(input)
}

fn preceding_spaces_and_args(input: &str) -> IResult<&str, Vec<String>> {
    let (input, _) = space1(input)?;
    if input.trim().is_empty() {
        return Ok((input, Vec::new()));
    }
    
    // Split remaining line by commas and trim
    let params: Vec<String> = input
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
        
    Ok(("", params))
}

pub fn parse_macro_def(input: &str) -> IResult<&str, ParsedInstruction> {
    map(
        tuple((
            // 1. Macro name
            take_while1(|c: char| c.is_alphanumeric() || c == '_'),
            // 2. Optional trailing colon (e.g., "jauc:")
            opt(tag(":")),
            space1,
            // 3. Directive: .macro or macro
            alt((tag_no_case(".macro"), tag_no_case("macro"))),
            // 4. Optional parameters
            opt(preceding_spaces_and_args),
        )),
        |(name, _, _, _, params_opt)| {
            let params = params_opt.unwrap_or_default();
            ParsedInstruction::MacroDef {
                name: name.to_string(),
                params,
            }
        },
    )(input)
}

pub fn parse_macro_end(input: &str) -> IResult<&str, ParsedInstruction> {
    map(tag_no_case(".endm"), |_| ParsedInstruction::MacroEnd)(input)
}

pub fn parse_and(input: &str) -> IResult<&str, ParsedInstruction> {
    map(
        tuple((
            tag_no_case("and"),
            space1,
            parse_operand,
            space0,
            tag(","),
            space0,
            parse_operand,
        )),
        |(_, _, src, _, _, _, dst)| ParsedInstruction::And { src, dst },
    )(input)
}

pub fn parse_or(input: &str) -> IResult<&str, ParsedInstruction> {
    map(
        tuple((
            tag_no_case("or"),
            space1,
            parse_operand,
            space0,
            tag(","),
            space0,
            parse_operand,
        )),
        |(_, _, src, _, _, _, dst)| ParsedInstruction::Or { src, dst },
    )(input)
}

pub fn parse_xor(input: &str) -> IResult<&str, ParsedInstruction> {
    map(
        tuple((
            tag_no_case("xor"),
            space1,
            parse_operand,
            space0,
            tag(","),
            space0,
            parse_operand,
        )),
        |(_, _, src, _, _, _, dst)| ParsedInstruction::Xor { src, dst },
    )(input)
}

pub fn parse_jmp(input: &str) -> IResult<&str, ParsedInstruction> {
    map(
        tuple((
            tag_no_case("jmp"),
            space1,
            alt((
                parse_operand,
                // Fallback to parse bare symbol/identifier names like RENDER_FRAME
                map(
                    nom::bytes::complete::take_while1(|c: char| c.is_alphanumeric() || c == '_'),
                    |s: &str| Operand::Symbol(s.to_string()),
                ),
            )),
        )),
        |(_, _, target)| ParsedInstruction::Jmp { target },
    )(input)
}

pub fn parse_rets(input: &str) -> IResult<&str, ParsedInstruction> {
    map(
        tag_no_case("rets"),
        |_| ParsedInstruction::Rets,
    )(input)
}

// pub fn parse_usect(input: &str) -> IResult<&str, ParsedInstruction> {
//     map(
//         tuple((
//             // 1. Symbol name
//             take_while1(|c: char| c.is_alphanumeric() || c == '_'),
//             space1,
//             alt((tag_no_case(".usect"), tag_no_case("usect"))),
//             space1,
//             // 2. Section name in quotes e.g. "STACK"
//             take_while1(|c: char| c != ',' && !c.is_whitespace() && c != ';'),
//             take_while(|c: char| c.is_whitespace() || c == ','),
//             // 3. Size and optional alignment arguments (rest of line excluding comments)
//             take_while1(|c: char| c != ';' && c != '\r' && c != '\n'),
//         )),
//         |(name, _, _, _, section, _, args_str)| {
//             ParsedInstruction::Usect {
//                 name: name.to_string(),
//                 section: section.to_string(),
//                 args: args_str.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
//             }
//         },
//     )(input)
// }

// Parses *reg(offset), e.g. *a0(16)
fn parse_indirect_offset(input: &str) -> IResult<&str, Operand> {
    let (input, _) = tag("*")(input)?;
    let (input, reg) = parse_register(input)?; // Use your project's register parser
    let (input, _) = tag("(")(input)?;
    let (input, offset_str) = nom::bytes::complete::take_till(|c| c == ')')(input)?;
    let (input, _) = tag(")")(input)?;
    
    let offset = offset_str.trim().parse::<i32>().unwrap_or(0);
    Ok((input, Operand::IndirectOffset(reg, offset)))
}

/// Parse ADDI instruction (e.g., "ADDI 96, A0" -> translates to Add with Immediate)
pub fn parse_addi(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("ADDI")(input)?;
    let (input, _) = multispace1(input)?;
    
    // Parse the value/symbol as an immediate (without requiring a leading #)
    let (input, val) = parse_number(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?;

    Ok((input, ParsedInstruction::Add { 
        src: Operand::Immediate(val), 
        dst 
    }))
}
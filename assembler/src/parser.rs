use nom::{
    branch::alt,
    bytes::complete::{tag, tag_no_case, take_while, take_while1},
    character::complete::{
        alphanumeric1, char, multispace0, multispace1, satisfy, space0, space1, u32 as parse_u32,
    },
    combinator::{map, opt},
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
    IndirectField(Register, String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParsedInstruction {
    Nop,
    Inc {
        dst: Operand,
    },
    Dec {
        dst: Operand,
    },
    Movy {
        src: Operand,
        dst: Operand,
    },
    Movx {
        src: Operand,
        dst: Operand,
    },
    RotateLeft {
        src: Operand,
        dst: Operand,
    },
    RotateRight {
        src: Operand,
        dst: Operand,
    },
    Move {
        src: Operand,
        dst: Operand,
        field: Option<Operand>,
    },
    Movb {
        src: Operand,
        dst: Operand,
    },
    Add {
        src: Operand,
        dst: Operand,
    },
    Sub {
        src: Operand,
        dst: Operand,
    },
    Label(String),
    Directive {
        name: String,
        args: Vec<String>,
    },
    Sll {
        src: Operand,
        dst: Operand,
    }, // Shift Left Logical
    Srl {
        src: Operand,
        dst: Operand,
    }, // Shift Right Logical
    Sla {
        src: Operand,
        dst: Operand,
    },
    Sra {
        src: Operand,
        dst: Operand,
    },
    Jr {
        label: String,
    },
    Jrcc {
        condition: u8,
        target: String,
    },
    Jacc {
        condition: u8,
        target: String,
    },
    LongJrcc {
        condition: u8,
        target: String,
    },
    Call {
        label: String,
    },
    Jeq {
        label: String,
    },
    Jne {
        label: String,
    },
    Equ {
        name: String,
        value: i32,
    },
    Cmp {
        src: Operand,
        dst: Operand,
    },
    Jlt {
        target: String,
    },
    Jgt {
        target: String,
    },
    MacroDef {
        name: String,
        params: Vec<String>,
    },
    MacroEnd, // for .endm
    And {
        src: Operand,
        dst: Operand,
    },
    Andn {
        src: Operand,
        dst: Operand,
    },
    Or {
        src: Operand,
        dst: Operand,
    },
    Xor {
        src: Operand,
        dst: Operand,
    },
    Jmp {
        target: Operand,
    },
    Rets,
    Usect {
        name: String,
        section: String,
        args: Vec<String>,
    },
    Trap {
        vector: i32,
    },
    Dsjs {
        reg: Operand,
        target: String,
    },
    Calla {
        target: Operand,
    },
    Btst {
        src: Operand,
        dst: Operand,
    },
    Callr {
        label: String,
    },
    Movk {
        src: Operand,
        dst: Operand,
    },
    Exgpc(Register),
    Setf {
        size: Operand,
        sign: Operand,
        offset: Operand,
    },
    Not {
        dst: Operand,
    },
    Fill {
        field: String,
    },
    Pixt {
        src: Operand,
        dst: Operand,
    },
}

#[derive(Clone, Debug)]
pub struct MacroTemplate {
    pub name: String,
    pub params: Vec<String>,
    pub body: Vec<String>,
}

// --- PARSERS ---

pub fn parse_register(input: &str) -> IResult<&str, Register> {
    let (input, reg_str) = take_while1(|c: char| c.is_alphanumeric())(input)?;
    let upper = reg_str.to_uppercase();

    let reg = match upper.as_str() {
        "SP" => Register::A(15),
        _ if upper.starts_with('A') => {
            if let Ok(n) = upper[1..].parse::<u8>() {
                if n <= 15 {
                    Register::A(n)
                } else {
                    return Err(nom::Err::Error(nom::error::Error::new(
                        input,
                        nom::error::ErrorKind::Tag,
                    )));
                }
            } else {
                return Err(nom::Err::Error(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Tag,
                )));
            }
        }
        _ if upper.starts_with('B') => {
            if let Ok(n) = upper[1..].parse::<u8>() {
                if n <= 15 {
                    Register::B(n)
                } else {
                    return Err(nom::Err::Error(nom::error::Error::new(
                        input,
                        nom::error::ErrorKind::Tag,
                    )));
                }
            } else {
                return Err(nom::Err::Error(nom::error::Error::new(
                    input,
                    nom::error::ErrorKind::Tag,
                )));
            }
        }
        _ => {
            return Err(nom::Err::Error(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Tag,
            )))
        }
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
            preceded(
                tag_no_case("0x"),
                take_while1(|c: char| c.is_ascii_hexdigit()),
            ),
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

pub fn parse_operand(input: &str) -> IResult<&str, Operand> {
    alt((
        // 1. Direct Register: A0
        map(parse_register, Operand::Reg),
        // Indirect operands must precede the general symbol parser because '*' is
        // also permitted in immediate expressions.
        map(
            tuple((char('-'), char('*'), parse_register)),
            |(_, _, reg)| Operand::IndirectPreDec(reg),
        ),
        map(
            tuple((char('*'), char('-'), parse_register)),
            |(_, _, reg)| Operand::IndirectPreDec(reg),
        ),
        map(
            tuple((char('*'), parse_register, char('+'))),
            |(_, reg, _)| Operand::IndirectPostInc(reg),
        ),
        parse_indirect_offset,
        map(preceded(char('*'), parse_register), Operand::Indirect),
        // 2. Comprehensive Immediate / Symbol: #384, #510*01000H, #STACKL/32-32, etc.
        map(
            preceded(
                opt(char('#')),
                take_while1(|c: char| {
                    c.is_alphanumeric()
                        || c == '_'
                        || c == '$'
                        || c == '-'
                        || c == '/'
                        || c == '*'
                        || c == '('
                        || c == ')'
                        || c == '|'
                        || c == '&'
                        || c == '+'
                        || c == '~'
                        || c == ' '
                        || c == '\t'
                        || c == 'h'
                        || c == 'H'
                }),
            ),
            |token: &str| {
                let token_trimmed = token.trim();
                if let Ok(n) = token_trimmed.parse::<i32>() {
                    Operand::Immediate(n)
                } else if let Some(hex) = token_trimmed.strip_prefix('$') {
                    Operand::Immediate(i32::from_str_radix(hex, 16).unwrap_or(0))
                } else if let Some(hex) = token_trimmed
                    .strip_prefix("0x")
                    .or_else(|| token_trimmed.strip_prefix("0X"))
                {
                    Operand::Immediate(i32::from_str_radix(hex, 16).unwrap_or(0))
                } else if let Some(hex) = token_trimmed
                    .strip_suffix('h')
                    .or_else(|| token_trimmed.strip_suffix('H'))
                {
                    Operand::Immediate(i32::from_str_radix(hex, 16).unwrap_or(0))
                } else {
                    Operand::Symbol(token_trimmed.to_string())
                }
            },
        ),
        // 3. Direct Memory / Absolute: @label
        map(
            preceded(
                char('@'),
                take_while1(|c: char| c.is_alphanumeric() || c == '_'),
            ),
            |s: &str| Operand::DirectMemory(s.to_string()),
        ),
        // 4. Indirect Pre-Decrement: -*A0 (must be before simple indirect)
        map(
            tuple((char('-'), char('*'), parse_register)),
            |(_, _, reg)| Operand::IndirectPreDec(reg),
        ),
        // 5. Indirect Post-Increment: *A0+
        map(
            tuple((char('*'), parse_register, char('+'))),
            |(_, reg, _)| Operand::IndirectPostInc(reg),
        ),
        // 6. Indirect with Offset: *A0(16)
        parse_indirect_offset,
        // 7. Simple Register Indirect: *A0
        map(preceded(char('*'), parse_register), Operand::Indirect),
    ))(input)
}

pub fn parse_inc_dec(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, mnemonic) = alt((tag_no_case("INC"), tag_no_case("DEC")))(input)?;
    let (input, _) = multispace1(input)?;
    let (input, dst) = parse_operand(input)?;
    if mnemonic.eq_ignore_ascii_case("INC") {
        Ok((input, ParsedInstruction::Inc { dst }))
    } else {
        Ok((input, ParsedInstruction::Dec { dst }))
    }
}

/// Parse the NOP instruction
pub fn parse_nop(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("NOP")(input)?;
    Ok((input, ParsedInstruction::Nop))
}

/// Parse MOVE instruction with optional field specifier (e.g., "MOVE A0, -*SP, 1")
pub fn parse_move(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("MOVE")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, src) = alt((parse_operand, parse_legacy_immediate))(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?;

    // Optional third argument for field size/width (e.g., ",1")
    let (input, field) = opt(tuple((
        delimited(multispace0, char(','), multispace0),
        parse_legacy_immediate,
    )))(input)?;

    let field_op = field.map(|(_, f)| f);

    Ok((
        input,
        ParsedInstruction::Move {
            src,
            dst,
            field: field_op,
        },
    ))
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
            parse_inc_dec,
            parse_move,
            parse_add,
            parse_sub,
            parse_jr,
            parse_relative_conditional,
            parse_call,
            parse_jauc,
            parse_jahs,
            parse_jeq,
            parse_jne,
            parse_directive,
            parse_label,
            parse_shift,
            parse_equ,
            parse_movb,
            parse_andi,
            parse_ori,
            parse_dsjs,
        )),
        alt((
            parse_cmp,
            parse_jgt,
            parse_jlt,
            parse_set,
            parse_macro_def,
            parse_macro_end,
            parse_and,
            parse_andn,
            parse_or,
            parse_xor,
            parse_jmp,
            parse_rets,
            // parse_usect,
            parse_addi,
            parse_addk,
            parse_subk,
            parse_cmpi,
            parse_clr,
            parse_calli,
            parse_movi,
            parse_preprocessor_line,
            parse_trap,
        )),
        alt((
            parse_calla,
            parse_btst,
            parse_callr,
            parse_movk,
            parse_xori,
            parse_exgpc,
            parse_setf,
            parse_not,
            parse_coordinate_move,
            parse_rotate,
            parse_mmfm,
            parse_fill,
            parse_pixt,
        )),
        parse_unknown_instruction,
    ))(input)
}

/// Parse a valid assembly label identifier (alphanumeric + underscores)
pub fn parse_identifier(input: &str) -> IResult<&str, &str> {
    take_while1(|c: char| c.is_ascii_alphanumeric() || c == '_')(input)
}

/// Accept legacy TMS34010 instructions that do not yet need specialized AST handling.
pub fn parse_unknown_instruction(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, name) = parse_identifier(input)?;
    let (input, _) = space0(input)?;
    let args = input.trim();
    Ok((
        "",
        ParsedInstruction::Directive {
            name: name.to_uppercase(),
            args: args
                .split(',')
                .map(|arg| arg.trim().to_string())
                .filter(|arg| !arg.is_empty())
                .collect(),
        },
    ))
}

/// Parse a JR (Jump Relative) instruction (e.g., "JR START_LOOP")
/// Parse JR or JRUC (Jump Relative / Unconditional) instruction (e.g., "JRUC _ac_loop")
pub fn parse_jr(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = alt((tag_no_case("JRUC"), tag_no_case("JR")))(input)?;
    let (input, _) = multispace1(input)?;
    let (input, label) = parse_identifier(input)?;
    Ok((
        input,
        ParsedInstruction::Jr {
            label: label.to_string(),
        },
    ))
}
pub fn parse_relative_conditional(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, mnemonic) = parse_identifier(input)?;
    if !mnemonic.to_ascii_uppercase().starts_with("JR") {
        return Err(nom::Err::Error(nom::error::Error::new(
            input,
            nom::error::ErrorKind::Tag,
        )));
    }
    let (input, _) = multispace1(input)?;
    let (input, label) = parse_identifier(input)?;
    let condition = match mnemonic.to_ascii_uppercase().as_str() {
        "JRUC" | "JR" => 0x0,
        "JRP" => 0x1,
        "JRLS" => 0x2,
        "JRHI" => 0x3,
        "JRLT" | "JLT" => 0x4,
        "JRGE" => 0x5,
        "JRLE" => 0x6,
        "JRGT" | "JGT" => 0x7,
        "JRLO" | "JRC" | "JRB" => 0x8,
        "JRHS" | "JRNC" | "JRNB" => 0x9,
        "JREQ" | "JRZ" | "JEQ" => 0xA,
        "JRNE" | "JRNZ" | "JNE" => 0xB,
        "JRV" | "JAV" => 0xC,
        "JRNV" | "JANV" => 0xD,
        "JRN" | "JAN" => 0xE,
        "JRNN" | "JANN" => 0xF,
        _ => return Err(nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Tag))),
    };
    Ok((input, ParsedInstruction::Jrcc { condition, target: label.to_string() }))
}

/// Parse a CALL instruction (e.g., "CALL SUBROUTINE")
pub fn parse_call(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("CALL")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, label) = parse_identifier(input)?;
    Ok((
        input,
        ParsedInstruction::Call {
            label: label.to_string(),
        },
    ))
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

/// Parse absolute conditional JAHS (Jump if all halfwords high sign) instruction.
pub fn parse_jahs(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("JAHS")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, label) = parse_identifier(input)?;
    Ok((
        input,
        ParsedInstruction::Jacc { condition: 0x9, target: label.to_string() },
    ))
}

/// Parse JEQ (Jump if Equal) instruction
pub fn parse_jeq(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("JEQ")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, label) = parse_identifier(input)?;
    Ok((
        input,
        ParsedInstruction::Jeq {
            label: label.to_string(),
        },
    ))
}

/// Parse JNE (Jump if Not Equal) instruction
pub fn parse_jne(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("JNE")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, label) = parse_identifier(input)?;
    Ok((
        input,
        ParsedInstruction::Jne {
            label: label.to_string(),
        },
    ))
}

/// Parse a label definition ending with a colon (e.g., "INIT:")
pub fn parse_label(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, label_name) = parse_identifier(input)?;
    let (input, _) = char(':')(input)?;
    Ok((input, ParsedInstruction::Label(label_name.to_string())))
}

/// Parse a directive like `.WORD 1234` or `.WORD $BEEF`
pub fn parse_directive(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, name) = alt((tag_no_case(".WORD"), tag_no_case(".LONG"), tag_no_case(".BYTE")))(input)?;
    let (input, _) = multispace1(input)?;
    let (input, args_text) = take_while1(|c: char| c != '\r' && c != '\n' && c != ';')(input)?;

    Ok((
        input,
        ParsedInstruction::Directive {
            name: name.to_string(),
            args: split_asm_operands(args_text),
        },
    ))
}

/// Split an assembler operand list on top-level commas, preserving commas inside
/// parentheses, brackets, and quoted strings.
pub fn split_asm_operands(input: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut parens = 0usize;
    let mut brackets = 0usize;
    let mut quote = None;
    for (index, ch) in input.char_indices() {
        if let Some(active_quote) = quote {
            if ch == active_quote { quote = None; }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => parens += 1,
            ')' => parens = parens.saturating_sub(1),
            '[' => brackets += 1,
            ']' => brackets = brackets.saturating_sub(1),
            ',' if parens == 0 && brackets == 0 => {
                let operand = input[start..index].trim();
                if !operand.is_empty() { result.push(operand.to_string()); }
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    let operand = input[start..].trim();
    if !operand.is_empty() { result.push(operand.to_string()); }
    result
}

/// Parse shift instructions (e.g., "SLL 5, A0" or "SLL #5, A0")
pub fn parse_shift(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, mnemonic) = alt((
        tag_no_case("SLL"),
        tag_no_case("SRL"),
        tag_no_case("SLA"),
        tag_no_case("SRA"),
    ))(input)?;

    let (input, _) = multispace1(input)?;

    let (input, count) = alt((parse_operand, map(parse_number, Operand::Immediate)))(input)?;

    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?;

    let instruction = match mnemonic.to_uppercase().as_str() {
        "SLL" => ParsedInstruction::Sll { src: count, dst },
        "SRL" => ParsedInstruction::Srl { src: count, dst },
        "SLA" => ParsedInstruction::Sla { src: count, dst },
        "SRA" => ParsedInstruction::Sra { src: count, dst },
        _ => {
            return Err(nom::Err::Error(nom::error::Error::new(
                input,
                nom::error::ErrorKind::Tag,
            )))
        }
    };

    Ok((input, instruction))
}

pub fn parse_equ(input: &str) -> IResult<&str, ParsedInstruction> {
    map(
        tuple((
            // 1. Symbol name
            take_while1(|c: char| c.is_alphanumeric() || c == '_' || c == '#'),
            space1,
            // 2. Directive: .equ or equ
            alt((tag_no_case(".equ"), tag_no_case("equ"))),
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
pub fn parse_expression_value(directive: &str, symbols: &std::collections::HashMap<String, u32>) -> i32 {
    let upper = directive.to_ascii_uppercase();
    let marker = upper.split_whitespace().scan(0usize, |offset, token| {
        let start = upper[*offset..].find(token).map(|relative| *offset + relative)?;
        *offset = start + token.len();
        Some((token, start, *offset))
    }).find_map(|(token, start, end)| {
        matches!(token, ".EQU" | "EQU" | ".SET" | "SET")
            .then_some((upper[..start].trim().is_empty(), end))
    });
    let expression = marker
        .map(|(directive_first, end)| {
            let remainder = upper[end..].trim();
            if directive_first {
                remainder.split_once(char::is_whitespace).map(|(_, expression)| expression.trim()).unwrap_or("")
            } else {
                remainder
            }
        })
        .unwrap_or("");

    fn value_expr(input: &str, symbols: &std::collections::HashMap<String, u32>, position: &mut usize) -> i32 {
        fn primary(input: &str, symbols: &std::collections::HashMap<String, u32>, position: &mut usize) -> i32 {
            while input.as_bytes().get(*position).is_some_and(|c| c.is_ascii_whitespace()) { *position += 1; }
            if input.as_bytes().get(*position) == Some(&b'(') { *position += 1; let v = value_expr(input, symbols, position); *position += 1; return v; }
            let start = *position;
            while input.as_bytes().get(*position).is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'$' | b'#')) { *position += 1; }
            let token = &input[start..*position];
            if token.is_empty() { return 0; }
            if let Some(hex) = token.strip_prefix('$') {
                i32::from_str_radix(hex, 16).unwrap_or(0)
            } else if let Some(hex) = token.strip_prefix("0x") {
                i32::from_str_radix(hex, 16).unwrap_or(0)
            } else if token.len() > 1 && token.ends_with(|c: char| c == 'H' || c == 'h') {
                i32::from_str_radix(&token[..token.len() - 1], 16).unwrap_or(0)
            } else if let Ok(number) = token.parse::<i32>() {
                number
            } else {
                symbols.get(&token.to_ascii_uppercase()).copied().unwrap_or(0) as i32
            }
        }
        let mut value = primary(input, symbols, position);
        loop {
            while input.as_bytes().get(*position).is_some_and(|c| c.is_ascii_whitespace()) { *position += 1; }
            let op = input.as_bytes().get(*position).copied();
            match op {
                Some(b'*') => {
                    *position += 1;
                    value = value.wrapping_mul(primary(input, symbols, position));
                }
                Some(b'/') => {
                    *position += 1;
                    value = value.wrapping_div(primary(input, symbols, position).max(1));
                }
                Some(b'+') | Some(b'-') => {
                    let op = input.as_bytes()[*position] as char;
                    *position += 1;
                    let rhs = primary(input, symbols, position);
                    value = if op == '+' { value.wrapping_add(rhs) } else { value.wrapping_sub(rhs) };
                    while input.as_bytes().get(*position).is_some_and(|c| c.is_ascii_whitespace()) {
                        *position += 1;
                    }
                    continue;
                }
                _ => break,
            }
        }
        value
    }

    let mut position = 0;
    value_expr(expression, symbols, &mut position)
}

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

/// Parse JLT or JRLT instruction (e.g., "JRLT map_ok")
pub fn parse_jlt(input: &str) -> IResult<&str, ParsedInstruction> {
    map(
        preceded(
            tuple((alt((tag_no_case("JRLT"), tag_no_case("JLT"))), space1)),
            take_while1(|c: char| c.is_alphanumeric() || c == '_'),
        ),
        |target: &str| ParsedInstruction::Jlt {
            target: target.to_string(),
        },
    )(input)
}

/// Parse JGT or JRGT instruction (e.g., "JRGT _invalid" or "JGT _invalid")
pub fn parse_jgt(input: &str) -> IResult<&str, ParsedInstruction> {
    map(
        preceded(
            tuple((alt((tag_no_case("JRGT"), tag_no_case("JGT"))), space1)),
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
                take_while1(|c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '-' | '+' | '*' | '/' | '(' | ')' | '[' | ']' | ',' | '|')),
            ),
        )),
        |(name, _, val_str): (&str, &str, &str)| {
            let value = if let Ok(n) = val_str.parse::<i32>() {
                n
            } else if let Some(hex) = val_str.strip_prefix('$') {
                i32::from_str_radix(hex, 16).unwrap_or(0)
            } else if val_str.to_ascii_uppercase().ends_with('H') {
                i32::from_str_radix(&val_str[..val_str.len() - 1], 16).unwrap_or(0)
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

pub fn parse_andn(input: &str) -> IResult<&str, ParsedInstruction> {
    map(
        tuple((
            tag_no_case("ANDN"),
            space1,
            parse_operand,
            space0,
            tag(","),
            space0,
            parse_operand,
        )),
        |(_, _, src, _, _, _, dst)| ParsedInstruction::Andn { src, dst },
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
    map(tag_no_case("rets"), |_| ParsedInstruction::Rets)(input)
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

/// Parse ADDI instruction (supports legacy syntax like "ADDI AddressTable, A0" or standard "#val, A0")
pub fn parse_addi(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("ADDI")(input)?;
    let (input, _) = multispace1(input)?;

    // Parse source operand: handles optional '#' followed by symbol/expression/number, or bare symbols/numbers
    let (input, src) = alt((
        parse_operand, // Handles #val or #symbol via standard operand parser
        map(
            take_while1(|c: char| {
                c.is_alphanumeric()
                    || c == '_'
                    || c == '$'
                    || c == '-'
                    || c == '('
                    || c == ')'
                    || c == '|'
                    || c == '&'
                    || c == '+'
            }),
            |token: &str| {
                let trimmed = token.trim();
                if let Ok(n) = trimmed.parse::<i32>() {
                    Operand::Immediate(n)
                } else if let Some(hex) = trimmed.strip_prefix('$') {
                    Operand::Immediate(i32::from_str_radix(hex, 16).unwrap_or(0))
                } else {
                    Operand::Symbol(trimmed.to_string())
                }
            },
        ),
    ))(input)?;

    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?;

    Ok((input, ParsedInstruction::Add { src, dst }))
}

/// Parse SUBK instruction (e.g., "SUBK 1, A0" -> translates to Sub with Immediate).
pub fn parse_subk(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("SUBK")(input)?;
    let (input, _) = multispace1(input)?;

    let (input, val) = parse_number(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?;

    Ok((
        input,
        ParsedInstruction::Sub {
            src: Operand::Immediate(val),
            dst,
        },
    ))
}

/// Parse ADDK instruction (e.g., "ADDK 1, A0" -> translates to Add with Immediate)
pub fn parse_addk(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("ADDK")(input)?;
    let (input, _) = multispace1(input)?;

    // Parse the quick constant value
    let (input, val) = parse_number(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?;

    Ok((
        input,
        ParsedInstruction::Add {
            src: Operand::Immediate(val),
            dst,
        },
    ))
}

/// Parse CMPI instruction (e.g., "CMPI 5, A0" -> translates to Cmp with Immediate source)
pub fn parse_cmpi(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("CMPI")(input)?;
    let (input, _) = multispace1(input)?;

    // Parse immediate number value
    let (input, src) = alt((
        parse_legacy_immediate,
        map(parse_number, Operand::Immediate),
    ))(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?;

    Ok((input, ParsedInstruction::Cmp { src, dst }))
}

/// Parse CLR instruction (e.g., "CLR A0" -> translates to MOVE #0, A0)
pub fn parse_clr(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("CLR")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, dst) = parse_operand(input)?;

    Ok((
        input,
        ParsedInstruction::Move {
            src: Operand::Immediate(0),
            dst,
            field: None,
        },
    ))
}

/// Parse MOVI instruction (e.g., "MOVI STACKL/32-32, A2")
pub fn parse_movi(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("MOVI")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, src) = parse_legacy_immediate(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?;
    let (input, field) = opt(tuple((
        delimited(multispace0, char(','), multispace0),
        parse_legacy_immediate,
    )))(input)?;

    Ok((
        input,
        ParsedInstruction::Move {
            src,
            dst,
            field: field.map(|(_, field)| field),
        },
    ))
}

/// Parse CALLI instruction (e.g., "CALLI map_ok" -> translates to Call with immediate/symbol target)
pub fn parse_calli(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("CALLI")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, label) = parse_identifier(input)?;
    Ok((
        input,
        ParsedInstruction::Call {
            label: label.to_string(),
        },
    ))
}

/// Parse lines starting with '#' (comments or preprocessor directives) as Nop
pub fn parse_preprocessor_line(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = char('#')(input)?;
    let (input, _) = take_while(|c: char| c != '\n' && c != '\r')(input)?;
    Ok((input, ParsedInstruction::Nop))
}

/// Parse TRAP instruction (e.g., "TRAP 3")
pub fn parse_trap(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("TRAP")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, vector) = parse_number(input)?;
    Ok((input, ParsedInstruction::Trap { vector }))
}

pub fn parse_mmfm(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("MMFM")(input)?;
    let (input, _) = take_while(|c: char| c != '\n' && c != '\r')(input)?;
    Ok((
        input,
        ParsedInstruction::Directive {
            name: "MMFM".to_string(),
            args: Vec::new(),
        },
    ))
}

pub fn parse_coordinate_move(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, mnemonic) = alt((tag_no_case("MOVY"), tag_no_case("MOVX")))(input)?;
    let (input, _) = multispace1(input)?;
    let (input, src) = parse_operand(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?;
    let instruction = if mnemonic.eq_ignore_ascii_case("MOVY") {
        ParsedInstruction::Movy { src, dst }
    } else {
        ParsedInstruction::Movx { src, dst }
    };
    Ok((input, instruction))
}

pub fn parse_rotate(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, mnemonic) = alt((tag_no_case("RL"), tag_no_case("RR")))(input)?;
    let (input, _) = multispace1(input)?;
    let (input, src) = parse_operand(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?;
    let instruction = if mnemonic.eq_ignore_ascii_case("RL") {
        ParsedInstruction::RotateLeft { src, dst }
    } else {
        ParsedInstruction::RotateRight { src, dst }
    };
    Ok((input, instruction))
}

/// Parse FILL instruction and its optional field selector.
pub fn parse_fill(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("FILL")(input)?;
    let (input, field) = opt(preceded(multispace1, parse_identifier))(input)?;
    Ok((
        input,
        ParsedInstruction::Fill {
            field: field.unwrap_or("").to_string(),
        },
    ))
}

/// Parse NOT instruction (one operand), which complements the destination register.
pub fn parse_not(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("NOT")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, dst) = parse_operand(input)?;
    Ok((input, ParsedInstruction::Not { dst }))
}

/// Parse PIXT instruction, including pixel-field address modifiers such as *A0.XY.
pub fn parse_pixt(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("PIXT")(input)?;
    let (input, _) = multispace1(input)?;
    let (remaining, operands) = nom::bytes::complete::take_while(|_| true)(input)?;
    let (src_text, dst_text) = operands.split_once(',').ok_or_else(|| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Tag))
    })?;

    fn parse_pixt_operand(text: &str) -> Option<Operand> {
        let text = text.trim();
        if let Some((base, field)) = text.strip_prefix('*').and_then(|s| s.split_once('.')) {
            let (register_remaining, reg) = parse_register(base).ok()?;
            if register_remaining.trim().is_empty() {
                return Some(Operand::IndirectField(reg, format!(".{}", field.trim())));
            }
        }
        let (remaining, operand) = parse_operand(text).ok()?;
        remaining.trim().is_empty().then_some(operand)
    }

    let src = parse_pixt_operand(src_text).ok_or_else(|| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Tag))
    })?;
    let dst = parse_pixt_operand(dst_text).ok_or_else(|| {
        nom::Err::Error(nom::error::Error::new(input, nom::error::ErrorKind::Tag))
    })?;

    Ok((remaining, ParsedInstruction::Pixt { src, dst }))
}

/// Parse MOVB instruction (e.g., "MOVB *A1, A4")
pub fn parse_movb(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("MOVB")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, src) = parse_operand(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?;
    Ok((input, ParsedInstruction::Movb { src, dst }))
}

pub fn parse_legacy_immediate(input: &str) -> IResult<&str, Operand> {
    let (input, _) = opt(char('#'))(input)?;

    if input.starts_with('[') {
        if let Some(end) = input.find(']') {
            let coordinate = &input[..=end];
            return Ok((
                &input[end + 1..],
                Operand::Immediate(parse_number_literal(coordinate)),
            ));
        }
    }

    let (input, token) = take_while1(|c: char| {
        c.is_alphanumeric()
            || c == '_'
            || c == '$'
            || c == '-'
            || c == '/'
            || c == '*'
            || c == '('
            || c == ')'
            || c == '|'
            || c == '&'
            || c == '+'
            || c == '~'
            || c == ' '
            || c == '\t'
            || c == 'h'
            || c == 'H'
    })(input)?;

    let token_trimmed = token.trim();
    let operand = if let Ok(n) = token_trimmed.parse::<i32>() {
        Operand::Immediate(n)
    } else if let Some(hex) = token_trimmed.strip_prefix('$') {
        Operand::Immediate(i32::from_str_radix(hex, 16).unwrap_or(0))
    } else if let Some(hex) = token_trimmed
        .strip_prefix("0x")
        .or_else(|| token_trimmed.strip_prefix("0X"))
    {
        Operand::Immediate(i32::from_str_radix(hex, 16).unwrap_or(0))
    } else if let Some(hex) = token_trimmed
        .strip_suffix('h')
        .or_else(|| token_trimmed.strip_suffix('H'))
    {
        Operand::Immediate(i32::from_str_radix(hex, 16).unwrap_or(0))
    } else {
        Operand::Symbol(token_trimmed.to_string())
    };

    Ok((input, operand))
}

/// Parse ORI instruction.
pub fn parse_ori(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("ORI")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, src) = parse_legacy_immediate(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?;
    Ok((input, ParsedInstruction::Or { src, dst }))
}

/// Parse ANDI instruction (e.g., "ANDI 0ffh, A4")
pub fn parse_andi(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("ANDI")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, src) = parse_legacy_immediate(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?;

    Ok((input, ParsedInstruction::And { src, dst }))
}

/// Parse DSJS / DSJ instruction (e.g., "DSJS A2, SFC_iloop" or "DSJS A0, $")
pub fn parse_dsjs(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = alt((tag_no_case("DSJS"), tag_no_case("DSJ")))(input)?;
    let (input, _) = multispace1(input)?;
    let (input, reg) = parse_operand(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    // Allow alphanumeric characters, underscores, $, and .
    let (input, target) =
        take_while1(|c: char| c.is_alphanumeric() || c == '_' || c == '$' || c == '.')(input)?;

    Ok((
        input,
        ParsedInstruction::Dsjs {
            reg,
            target: target.to_string(),
        },
    ))
}

/// Parse CALLA instruction (e.g., "CALLA InitAddresses" or absolute expressions)
pub fn parse_calla(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("CALLA")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, target) = parse_legacy_immediate(input)?;

    Ok((input, ParsedInstruction::Calla { target }))
}

/// Parse BTST instruction (e.g., "BTST 12, A0" or "#12, A0")
pub fn parse_btst(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("BTST")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, src) = parse_legacy_immediate(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?;

    Ok((input, ParsedInstruction::Btst { src, dst }))
}

/// Parse CALLR instruction (e.g., "CALLR io_init")
pub fn parse_callr(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = alt((tag_no_case("CALLR"), tag_no_case("JSRP")))(input)?;
    let (input, _) = multispace1(input)?;
    let (input, label) = take_while1(|c: char| c.is_alphanumeric() || c == '_' || c == '.')(input)?;

    Ok((
        input,
        ParsedInstruction::Callr {
            label: label.to_string(),
        },
    ))
}

/// Parse JAUC (jump absolute, unconditional) as an absolute condition-code jump.
pub fn parse_jauc(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("JAUC")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, target) = parse_identifier(input)?;
    Ok((input, ParsedInstruction::Jacc { condition: 0, target: target.to_string() }))
}

/// Parse MOVK instruction (e.g., "MOVK INAMODE, A0")
pub fn parse_movk(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("MOVK")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, src) = parse_legacy_immediate(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?;

    Ok((input, ParsedInstruction::Movk { src, dst }))
}

/// Parse XORI instruction (e.g., "XORI 02000h, A0")
pub fn parse_xori(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("XORI")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, src) = parse_legacy_immediate(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, dst) = parse_operand(input)?;

    Ok((input, ParsedInstruction::Xor { src, dst }))
}

/// Parse EXGPC instruction (e.g., "EXGPC B6")
pub fn parse_exgpc(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("EXGPC")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, reg) = parse_register(input)?;

    Ok((input, ParsedInstruction::Exgpc(reg)))
}

/// Parse SETF instruction (e.g., "SETF 16, 1, 0")
pub fn parse_setf(input: &str) -> IResult<&str, ParsedInstruction> {
    let (input, _) = tag_no_case("SETF")(input)?;
    let (input, _) = multispace1(input)?;
    let (input, size) = parse_legacy_immediate(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, sign) = parse_legacy_immediate(input)?;
    let (input, _) = delimited(multispace0, char(','), multispace0)(input)?;
    let (input, offset) = parse_legacy_immediate(input)?;

    Ok((input, ParsedInstruction::Setf { size, sign, offset }))
}

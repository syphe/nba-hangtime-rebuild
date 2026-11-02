use clap::Parser;
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use thiserror::Error;

use std::fs;

mod dump;
mod encoder;
mod interleaver;
mod parser;
mod verify;
use parser::{MacroTemplate, Operand, ParsedInstruction};

fn substitute_macro_parameters(
    line: &str,
    params: &[String],
    args: &[String],
) -> String {
    let mut output = String::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        if ch == ':' {
            if let Some((end, _)) = line[start + 1..].char_indices().find(|(_, c)| *c == ':') {
                let name_end = start + 1 + end;
                let name = &line[start + 1..name_end];
                if let Some(index) = params.iter().position(|param| param == name) {
                    if let Some(arg) = args.get(index) {
                        output.push_str(arg);
                        while chars.peek().is_some_and(|(position, _)| *position <= name_end) {
                            chars.next();
                        }
                        continue;
                    }
                }
            }
        }
        if ch.is_ascii_alphanumeric() || ch == '_' {
            let mut end = start + ch.len_utf8();
            while let Some((position, next)) = chars.peek().copied() {
                if next.is_ascii_alphanumeric() || next == '_' {
                    end = position + next.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            let token = &line[start..end];
            if let Some(index) = params.iter().position(|param| param == token) {
                if let Some(arg) = args.get(index) {
                    output.push_str(arg);
                    continue;
                }
            }
            output.push_str(token);
        } else {
            output.push(ch);
        }
    }
    output
}

#[derive(Error, Debug)]
pub enum AsmError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parse error on line {line}: {message}")]
    Parse { line: usize, message: String },

    #[error("Undefined symbol '{0}'")]
    UndefinedSymbol(String),

    #[error("Unknown opcode '{0}' at line {1}")]
    UnknownOpcode(String, usize),
}

/// Command line arguments parser
#[derive(Parser, Debug)]
#[command(author, version, about = "TMS34010 Native Assembler for Arcade ROM Modernization", long_about = None)]
struct Args {
    /// Path to input assembly file (.asm / .s)
    #[arg(short, long)]
    input: PathBuf,

    /// Path to output binary object file (.bin / .obj)
    #[arg(short, long, default_value = "output.bin")]
    output: PathBuf,

    /// Number of ways to interleave the ROM output (e.g. 2 for even/odd, 4 for quad banks)
    #[arg(short = 'w', long, default_value_t = 2)]
    interleave_ways: usize,

    /// Write a relocatable JSON object containing symbols and code (for linker development).
    #[arg(long)]
    object_format: bool,

    /// Verbose output showing passes and symbol table
    #[arg(short, long)]
    verbose: bool,

    /// Print a word-level hex listing of the generated binary and verify
    /// branch offsets, immediate operands, and .WORD values against the symbol table
    #[arg(long)]
    dump: bool,
}

/// Symbol Table for tracking labels and memory addresses
#[derive(Debug, Default)]
pub struct SymbolTable {
    symbols: HashMap<String, u32>,
}

#[derive(Serialize)]
struct ObjectSymbol {
    name: String,
    address: u32,
    section: String,
    section_offset: u32,
    kind: String,
}

#[derive(Serialize)]
struct ObjectFixup {
    offset: u32,
    object_offset: u32,
    section: String,
    symbol: String,
    kind: String,
}

fn is_symbolic_expression(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty()
        || value.starts_with('$')
        || value.starts_with("0x")
        || value.starts_with("0X")
        || value.parse::<i64>().is_ok()
    {
        return false;
    }
    if let Some(hex) = value.strip_suffix('h').or_else(|| value.strip_suffix('H')) {
        if !hex.is_empty() && hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return false;
        }
    }
    if let Some(binary) = value.strip_suffix('b').or_else(|| value.strip_suffix('B')) {
        if !binary.is_empty() && binary.chars().all(|ch| matches!(ch, '0' | '1')) {
            return false;
        }
    }
    value.chars().any(|ch| ch.is_ascii_alphabetic() || ch == '_' || ch == '#')
}

fn expression_references_symbol(value: &str, names: &HashSet<String>) -> bool {
    let mut token = String::new();
    for ch in value.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '#' {
            token.push(ch.to_ascii_uppercase());
        } else {
            if names.contains(&token) {
                return true;
            }
            token.clear();
        }
    }
    false
}

#[derive(Serialize, Clone)]
struct ObjectSection {
    name: String,
    data: String,
}

#[derive(Serialize)]
struct ObjectFile {
    format: &'static str,
    version: u32,
    source: String,
    text_address: u32,
    text: String,
    sections: Vec<ObjectSection>,
    bss_size_bits: u32,
    symbols: Vec<ObjectSymbol>,
    fixups: Vec<ObjectFixup>,
}

impl ObjectFile {
    fn from_assembler(
        source: &Path,
        assembler: &Assembler,
        ast: &[Instruction],
        binary: &[u8],
    ) -> Self {
        use base64::{engine::general_purpose::STANDARD, Engine as _};

        let mut symbol_sections = HashMap::<String, String>::new();
        let mut symbol_section_offsets = HashMap::<String, u32>::new();
        let mut label_section = ".text".to_string();
        let mut section_offsets = HashMap::<String, u32>::new();
        let mut bss_size_bits = 0u32;
        let mut bss_symbols = Vec::new();
        for instruction in ast {
            if let Instruction::Directive { name, args } = instruction {
                let directive = name.trim_start_matches('.').to_ascii_lowercase();
                if directive == "bss" && args.len() >= 2 {
                    let symbol = args[0].trim().to_string();
                    let size_bits = assembler.eval_arithmetic(&args[1]).max(0) as u32;
                    symbol_sections.insert(symbol.clone(), ".bss".to_string());
                    symbol_section_offsets.insert(symbol, bss_size_bits);
                    bss_symbols.push((args[0].trim().to_string(), bss_size_bits));
                    bss_size_bits = bss_size_bits.saturating_add(size_bits);
                    continue;
                }
                if directive == "sect" {
                    if let Some(section) = args.first() {
                        label_section = section.trim_matches(|c| c == '"' || c == '\'').to_string();
                    }
                } else if matches!(directive.as_str(), "text" | "data" | "bss")
                    && args.is_empty()
                {
                    label_section = format!(".{directive}");
                }
            }
            if let Instruction::Label(name) = instruction {
                symbol_sections.insert(name.clone(), label_section.clone());
                symbol_section_offsets.insert(
                    name.clone(),
                    section_offsets.get(&label_section).copied().unwrap_or(0),
                );
            }
            if !matches!(instruction, Instruction::Directive { name, .. } if {
                let directive = name.trim_start_matches('.').to_ascii_lowercase();
                directive == "sect" || matches!(directive.as_str(), "text" | "data" | "bss")
            }) {
                let section_pc = section_offsets.get(&label_section).copied().unwrap_or(0);
                let external_transfer = external_absolute_transfer(instruction, ast).is_some();
                let len = if external_transfer {
                    6
                } else {
                    encoder::encode_instruction(instruction, section_pc, &assembler.symbol_table)
                        .map(|bytes| bytes.len() as u32)
                        .unwrap_or(2)
                };
                *section_offsets.entry(label_section.clone()).or_default() += len;
            }
        }

        let mut symbols: Vec<ObjectSymbol> = assembler
            .symbol_table
            .symbols
            .iter()
            .map(|(name, address)| ObjectSymbol {
                name: name.clone(),
                address: *address,
                section: symbol_sections.get(name).cloned().unwrap_or_default(),
                section_offset: symbol_section_offsets.get(name).copied().unwrap_or(0),
                kind: "symbol".to_string(),
            })
            .collect();
        for (name, section_offset) in bss_symbols {
            if !symbols.iter().any(|symbol| symbol.name.eq_ignore_ascii_case(&name)) {
                symbols.push(ObjectSymbol {
                    name,
                    address: 0,
                    section: ".bss".to_string(),
                    section_offset,
                    kind: "symbol".to_string(),
                });
            }
        }
        symbols.sort_by(|a, b| a.name.cmp(&b.name));

        let mut fixups = Vec::new();
        let mut section_offsets = HashMap::<String, u32>::new();
        let mut section_data: Vec<(String, Vec<u8>)> = Vec::new();
        let mut current_section = ".text".to_string();
        let mut offset = 0u32;
        for instruction in ast {
            if let Instruction::Directive { name, args } = instruction {
                let directive = name.trim_start_matches('.').to_ascii_lowercase();
                let section_name = if directive == "sect" {
                    args.first()
                        .map(|value| value.trim_matches(|c| c == '"' || c == '\'').to_string())
                } else if matches!(directive.as_str(), "text" | "data" | "bss") && args.is_empty() {
                    Some(format!(".{directive}"))
                } else {
                    None
                };
                if let Some(section_name) = section_name {
                    current_section = section_name;
                }
            }

            let section_pc = section_offsets.get(&current_section).copied().unwrap_or(0);
            let external_transfer = external_absolute_transfer(instruction, ast);
            let encoded = if let Some((opcode, _)) = external_transfer {
                let mut bytes = opcode.to_le_bytes().to_vec();
                bytes.extend_from_slice(&[0, 0, 0, 0]);
                bytes
            } else {
                encoder::encode_instruction(instruction, section_pc, &assembler.symbol_table)
                    .unwrap_or_default()
            };

            let symbol = match instruction {
                Instruction::Jr { label }
                | Instruction::Call { label }
                | Instruction::Callr { label }
                | Instruction::Jeq { label }
                | Instruction::Jne { label } => Some(label.as_str()),
                Instruction::Jlt { target } | Instruction::Jgt { target } => Some(target.as_str()),
                Instruction::Jrcc { target, .. } => Some(target.as_str()),
                Instruction::Jacc { target, .. } | Instruction::LongJrcc { target, .. } => Some(target.as_str()),
                Instruction::Jmp { target } => match target {
                    Operand::Symbol(name) => Some(name.as_str()),
                    _ => None,
                },
                Instruction::Calla { target } => match target {
                    Operand::Symbol(name) => Some(name.as_str()),
                    _ => None,
                },
                Instruction::Move { src: Operand::Symbol(name), .. } => Some(name.as_str()),
                _ => None,
            };
            if let Some(symbol) = symbol {
                let upper = symbol.to_ascii_uppercase();
                let is_register = (upper.starts_with('A') || upper.starts_with('B'))
                    && upper[1..].chars().all(|c| c.is_ascii_digit())
                    || upper == "SP"
                    || upper == "FP";
                if !is_register {
                    let is_rel_branch = matches!(
                        instruction,
                        Instruction::Jr { .. }
                            | Instruction::Jeq { .. }
                            | Instruction::Jne { .. }
                            | Instruction::Jlt { .. }
                            | Instruction::Jgt { .. }
                            | Instruction::Jrcc { .. }
                            | Instruction::LongJrcc { .. }
                    );
                    let kind = match &instruction {
                        Instruction::Calla { .. } | Instruction::Move { .. } => "absolute32",
                        Instruction::Jacc { .. } => "absolute32",
                        _ if matches!(instruction, Instruction::Call { .. } | Instruction::Callr { .. }) && encoded.len() == 6 => "absolute32",
                        _ if is_rel_branch && encoded.len() == 6 => "absolute32",
                        Instruction::Call { .. } | Instruction::Callr { .. } => "tms34010-relative16",
                        _ if is_rel_branch && encoded.len() == 4 => "tms34010-relative16",
                        _ => "tms34010-relative",
                    };
                    let field_offset = if kind == "absolute32" || kind == "tms34010-relative16" { 2 } else { 0 };
                    fixups.push(ObjectFixup {
                        offset: section_offsets.get(&current_section).copied().unwrap_or(0)
                            + field_offset,
                        object_offset: offset + field_offset,
                        section: current_section.clone(),
                        symbol: symbol.to_string(),
                        kind: kind.to_string(),
                    });
                }
            }
            if let Instruction::Directive { name, args } = &instruction {
                if name.eq_ignore_ascii_case(".WORD") || name.eq_ignore_ascii_case(".LONG") {
                    let long = name.eq_ignore_ascii_case(".LONG");
                    let width = if long { 4 } else { 2 };
                    for (index, arg) in args.iter().enumerate() {
                        if is_symbolic_expression(arg) {
                            fixups.push(ObjectFixup {
                                offset: section_offsets.get(&current_section).copied().unwrap_or(0)
                                    + index as u32 * width,
                                object_offset: offset + index as u32 * width,
                                section: current_section.clone(),
                                symbol: arg.trim().to_string(),
                                kind: if long { "absolute32" } else { "absolute16" }.to_string(),
                            });
                        }
                    }
                }
            }
            let encoded_len = encoded.len() as u32;
            if !encoded.is_empty() {
                let section = section_data
                    .iter_mut()
                    .find(|(name, _)| name == &current_section);
                if let Some((_, data)) = section {
                    data.extend_from_slice(&encoded);
                } else {
                    section_data.push((current_section.clone(), encoded.clone()));
                }
                *section_offsets.entry(current_section.clone()).or_default() += encoded_len;
            }
            offset += encoded_len;
        }

        let sections = section_data
            .into_iter()
            .map(|(name, data)| ObjectSection {
                name,
                data: STANDARD.encode(data),
            })
            .collect();

        Self {
            format: "tms34010-object",
            version: 1,
            source: source.to_string_lossy().into_owned(),
            text_address: 0,
            text: STANDARD.encode(binary),
            sections,
            bss_size_bits,
            symbols,
            fixups,
        }
    }
}

impl SymbolTable {
    pub fn insert(&mut self, name: String, addr: u32) {
        self.symbols.insert(name, addr);
    }

    pub fn get(&self, name: &str) -> Option<&u32> {
        self.symbols.get(name)
    }

    pub fn get_ignore_case(&self, name: &str) -> Option<&u32> {
        self.symbols
            .iter()
            .find(|(symbol, _)| symbol.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    }
}

pub type Instruction = parser::ParsedInstruction;

fn layout_instruction_size(instruction: &Instruction, address: u32, symbols: &SymbolTable) -> u32 {
    match instruction {
        Instruction::Label(_) | Instruction::Equ { .. } | Instruction::Usect { .. }
        | Instruction::MacroDef { .. } | Instruction::MacroEnd => 0,
        Instruction::Directive { name, .. } if name.eq_ignore_ascii_case("MMTM") || name.eq_ignore_ascii_case("MMFM") => 4,
        _ => encoder::encode_instruction(instruction, address, symbols)
            .map(|bytes| bytes.len() as u32)
            .unwrap_or(0),
    }
}

fn external_absolute_transfer<'a>(instruction: &'a Instruction, ast: &[Instruction]) -> Option<(u16, &'a str)> {
    let (opcode, target) = match instruction {
        Instruction::Call { label } | Instruction::Callr { label } => (0xEB00, label.as_str()),
        Instruction::Jr { label } => (0xC080, label.as_str()),
        Instruction::Jeq { label } => (0xCA80, label.as_str()),
        Instruction::Jne { label } => (0xCB80, label.as_str()),
        Instruction::Jlt { target } => (0xC480, target.as_str()),
        Instruction::Jgt { target } => (0xC780, target.as_str()),
        Instruction::Jrcc { condition, target } => (0xC080 | ((*condition as u16) << 8), target.as_str()),
        Instruction::LongJrcc { condition, target } => (0xC080 | ((*condition as u16) << 8), target.as_str()),
        _ => return None,
    };
    let local = ast.iter().any(|candidate| matches!(candidate, Instruction::Label(name) if name.eq_ignore_ascii_case(target)));
    (!local).then_some((opcode, target))
}

pub struct Assembler {
    pub symbol_table: SymbolTable,
    pub macro_table: HashMap<String, MacroTemplate>,
    current_address: u32,
    assignment_aliases: HashMap<String, String>,
}

#[cfg(test)]
mod loop_expression_tests {
    use super::*;

    #[test]
    fn audited_opcodes_match_tms34010_reference_words() {
        // Golden words checked against MAME's 34010dsm.cpp and PAL.OBJ.
        for (source, words) in [
            ("nop", vec![0x0300]),
            ("movi 12345678h,a3", vec![0x09e3, 0x5678, 0x1234]),
            ("movi 12345678h,b3", vec![0x09f3, 0x5678, 0x1234]),
            ("clr a1", vec![0x5621]),
            ("clr b2", vec![0x5652]),
            ("move a2,a3", vec![0x4c43]),
            ("move b2,b3", vec![0x4c53]),
            ("move a2,b3", vec![0x4e43]),
            ("move b2,a3", vec![0x4e53]),
            ("add b2,b3", vec![0x4053]),
            ("sub a2,a3", vec![0x4443]),
            ("and a2,a3", vec![0x5043]),
            ("or b2,b3", vec![0x5453]),
            ("xor b2,b3", vec![0x5653]),
        ] {
            let mut assembler = Assembler::new();
            let ast = assembler.pass_one(&[source]).unwrap();
            let expected: Vec<u8> = words.into_iter().flat_map(u16::to_le_bytes).collect();
            assert_eq!(assembler.pass_two(&ast).unwrap(), expected, "{source}");
        }
        let mut assembler = Assembler::new();
        let ast = assembler.pass_one(&["OFFSET .set b4", "clr OFFSET", "COLOR0 .set b8", "clr COLOR0"]).unwrap();
        assert_eq!(assembler.pass_two(&ast).unwrap(), vec![0x94, 0x56, 0x18, 0x57]);
    }

    #[test]
    fn strings_preserve_punctuation_control_bytes_and_word_alignment() {
        let mut assembler = Assembler::new();
        let source = ".string \"A,B;C\",1,-1,0 ; comment\n.string 'Z'\n.even\nafter .word 1234h\n.even";
        let ast = assembler.pass_one(&source.lines().collect::<Vec<_>>()).unwrap();
        assert_eq!(assembler.symbol_table.get("after"), Some(&10));
        assert_eq!(assembler.pass_two(&ast).unwrap(), b"A,B;C\x01\xff\0Z\0\x34\x12");
    }

    #[test]
    fn arithmetic_obeys_grouping_precedence_and_signed_division() {
        let assembler = Assembler::new();
        for (expression, expected) in [
            ("(100-20)/16", 5), ("20-6-4", 10), ("2+3*4", 14),
            ("1024/(8192/32)", 4), ("-20/-4", 5), ("0xffffffff+2", 1),
        ] {
            assert_eq!(assembler.eval_arithmetic(expression), expected, "{expression}");
        }
    }

    #[test]
    fn labeled_macro_loops_use_call_time_counts_and_freeze_each_iteration() {
        let mut assembler = Assembler::new();
        let source = "GEN .macro unused\n.loop COUNT\n.word N\n.eval N+1,N\n.endloop\n.endm\n.asg 3/8,FRACTION\n.eval 16*FRACTION,COUNT\n.eval 1,N\ntable GEN 0";
        let ast = assembler.pass_one(&source.lines().collect::<Vec<_>>()).unwrap();
        assert_eq!(assembler.symbol_table.get("table"), Some(&0));
        assert_eq!(assembler.pass_two(&ast).unwrap(), vec![1,0,2,0,3,0,4,0,5,0,6,0]);
    }
}

#[derive(Clone, Copy)]
struct ConditionalFrame {
    parent_active: bool,
    branch_taken: bool,
}

impl Assembler {
    fn eval_condition(&self, expression: &str) -> i32 {
        let expression = expression.split(';').next().unwrap_or(expression).trim();
        if let Some((left, right)) = expression.split_once("&&") {
            return (self.eval_condition(left) != 0 && self.eval_condition(right) != 0) as i32;
        }
        if let Some((left, right)) = expression.split_once("||") {
            return (self.eval_condition(left) != 0 || self.eval_condition(right) != 0) as i32;
        }
        for op in [">=", "<=", "==", "!=", ">", "<", "="] {
            if let Some((left, right)) = expression.split_once(op) {
                let a = self.eval_arithmetic(left.trim());
                let b = self.eval_arithmetic(right.trim());
                return match op {
                    ">=" => a >= b,
                    "<=" => a <= b,
                    "==" | "=" => a == b,
                    "!=" => a != b,
                    ">" => a > b,
                    "<" => a < b,
                    _ => false,
                } as i32;
            }
        }
        self.eval_arithmetic(expression)
    }

    fn eval_arithmetic(&self, expression: &str) -> i32 {
        let mut expanded = expression.to_string();
        let (names, values): (Vec<_>, Vec<_>) = self.assignment_aliases.iter().map(|(name, value)| (name.clone(), value.clone())).unzip();
        for _ in 0..16 {
            let next = substitute_macro_parameters(&expanded, &names, &values);
            if next == expanded { break; }
            expanded = next;
        }
        parser::evaluate_arithmetic(&expanded, |name| {
            self.symbol_table.get(name).or_else(|| self.symbol_table.get_ignore_case(name)).copied().unwrap_or(0) as i32
        })
    }

    pub fn new() -> Self {
        Self {
            symbol_table: SymbolTable::default(),
            macro_table: HashMap::new(),
            current_address: 0,
            assignment_aliases: HashMap::new(),
        }
    }

    /// Pass 1: Parse AST via nom, collect labels, and assign memory locations
    pub fn pass_one(&mut self, lines: &[&str]) -> Result<Vec<Instruction>, AsmError> {
        let mut ast = Vec::new();
        // Keep the source expressions as well as the parser's provisional numeric
        // value.  Equates can refer to labels and equates declared later in the file.
        let mut pending_equates: Vec<(String, String)> = Vec::new();
        let mut current_macro: Option<MacroTemplate> = None;
        let mut macro_expansion_count = 0usize;
        let mut volatile_symbols = HashSet::<String>::new();
        let mut register_aliases = HashMap::<String, String>::new();

        let mut line_queue: VecDeque<String> = lines.iter().map(|s| s.to_string()).collect();
        let mut processed_lines = 0usize;
        let mut conditionals: Vec<ConditionalFrame> = Vec::new();

        while let Some(raw_line) = line_queue.pop_front() {
            processed_lines += 1;

            let clean_line = parser::sanitize_line(&raw_line);
            let mut trimmed_owned = clean_line.trim().to_string();
            if let Some(colon_pos) = trimmed_owned.find(':') {
                let rest = trimmed_owned[colon_pos + 1..].trim_start();
                if rest.to_uppercase().starts_with("EQU") {
                    trimmed_owned.replace_range(colon_pos..colon_pos + 1, " ");
                }
            }
            let mut trimmed = trimmed_owned.as_str();
            let upper_trimmed = trimmed.to_uppercase();
            let line_num = processed_lines; // approximate

            // 2. If we are currently recording a macro definition body
            if let Some(ref mut macro_def) = current_macro {
                if trimmed.eq_ignore_ascii_case(".endm") {
                    self.macro_table
                        .insert(macro_def.name.to_uppercase(), macro_def.clone());
                    current_macro = None;
                } else {
                    macro_def.body.push(raw_line);
                }
                continue;
            }

            let conditional_directive = upper_trimmed.split_whitespace().next().unwrap_or("");
            if conditional_directive == ".IF"
                || conditional_directive == ".ELSE"
                || conditional_directive == ".ENDIF"
            {
                let parent_active = conditionals.iter().all(|frame| frame.branch_taken);
                if conditional_directive == ".IF" {
                    let expression = trimmed.splitn(2, char::is_whitespace).nth(1).unwrap_or("0");
                    let active = parent_active && self.eval_condition(expression) != 0;
                    conditionals.push(ConditionalFrame {
                        parent_active,
                        branch_taken: active,
                    });
                } else if conditional_directive == ".ELSE" {
                    if let Some(frame) = conditionals.last_mut() {
                        frame.branch_taken = frame.parent_active && !frame.branch_taken;
                    }
                } else {
                    conditionals.pop();
                }
                continue;
            }

            if !conditionals.iter().all(|frame| frame.branch_taken) {
                continue;
            }

            // Expand TI .loop blocks before parsing so table-generating macros
            // emit each iteration with the current .eval values.
            if conditional_directive == ".LOOP" {
                let count_expr = trimmed.splitn(2, char::is_whitespace).nth(1).unwrap_or("0");
                let count = self.eval_arithmetic(count_expr);
                if !(0..=8192).contains(&count) {
                    return Err(AsmError::Parse {
                        line: line_num,
                        message: format!(".loop count {count} is outside 0..=8192"),
                    });
                }
                let mut depth = 1usize;
                let mut body = Vec::new();
                let mut found_end = false;
                while let Some(candidate_line) = line_queue.pop_front() {
                    let candidate = parser::sanitize_line(&candidate_line);
                    let directive = candidate.trim().split_whitespace().next().unwrap_or("");
                    if directive.eq_ignore_ascii_case(".loop") {
                        depth += 1;
                    } else if directive.eq_ignore_ascii_case(".endloop") {
                        depth -= 1;
                        if depth == 0 {
                            found_end = true;
                            break;
                        }
                    }
                    body.push(candidate_line);
                }
                if !found_end {
                    return Err(AsmError::Parse {
                        line: line_num,
                        message: "missing .endloop".to_string(),
                    });
                }
                for _ in 0..count {
                    for body_line in body.iter().rev() {
                        line_queue.push_front(body_line.clone());
                    }
                }
                continue;
            }
            if conditional_directive.eq_ignore_ascii_case(".ENDLOOP") {
                return Err(AsmError::Parse {
                    line: line_num,
                    message: "unexpected .endloop without .loop".to_string(),
                });
            }

            // 1a. Check for Single-Line Macros
            if upper_trimmed.contains(".MACRO") && upper_trimmed.contains(".ENDM") {
                if let Some(macro_idx) = upper_trimmed.find(".MACRO") {
                    let name = trimmed[..macro_idx].trim().to_uppercase();
                    let remainder = &trimmed[macro_idx + 6..];
                    if let Some(endm_idx) = remainder.to_uppercase().find(".ENDM") {
                        let params_and_body = remainder[..endm_idx].trim().to_string();
                        let template = MacroTemplate {
                            name: name.clone(),
                            params: Vec::new(),
                            body: vec![params_and_body],
                        };
                        println!("[DEBUG] Registered single-line macro: {}", name);
                        self.macro_table.insert(name, template);
                        continue;
                    }
                }
            }

            // 1b. Check for Multi-Line Macro Definition (.macro) FIRST
            if let Ok((_, ParsedInstruction::MacroDef { name, params })) =
                parser::parse_line(trimmed)
            {
                if let Some(prev_macro) = current_macro.take() {
                    println!("[DEBUG] Auto-closing unended macro: {}", prev_macro.name);
                    self.macro_table
                        .insert(prev_macro.name.to_uppercase(), prev_macro);
                }
                current_macro = Some(MacroTemplate {
                    name: name.to_uppercase(),
                    params,
                    body: Vec::new(),
                });
                continue;
            }

            // 3. Skip empty lines and comments
            if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('*') {
                continue;
            }

            // 4. Check for .equ Equates FIRST (before labels)
            let constant_parts: Vec<&str> = upper_trimmed.split_whitespace().collect();
            let is_set_constant = constant_parts.len() >= 3
                && constant_parts[1].eq_ignore_ascii_case("SET") ||
                constant_parts.len() >= 3 && constant_parts[1].eq_ignore_ascii_case(".SET") ||
                constant_parts.len() >= 2 && constant_parts.first().is_some_and(|word| word.eq_ignore_ascii_case(".SET"));
            let is_equ_constant = upper_trimmed.contains(".EQU")
                || (constant_parts.len() >= 3 && constant_parts[1].eq_ignore_ascii_case("EQU"));
            if is_set_constant || is_equ_constant
            {
                let parsed = if is_set_constant
                {
                    parser::parse_set(trimmed).map(|(_, instruction)| instruction)
                } else {
                    parser::parse_equ(trimmed).map(|(_, instruction)| instruction)
                };
                match parsed {
                    Ok(Instruction::Equ { name, value }) => {
                        let mut value = value;
                        let words: Vec<&str> = trimmed.split_whitespace().collect();
                        if words.len() >= 3 {
                            let expression = words[2..].join(" ");
                            let register = register_aliases.get(expression.trim()).cloned().or_else(|| {
                                parser::parse_register(expression.trim()).ok()
                                    .filter(|(rest, _)| rest.is_empty())
                                    .map(|_| expression.trim().to_string())
                            });
                            if let Some(register) = register {
                                register_aliases.insert(name, register);
                                continue;
                            }
                            register_aliases.remove(&name);
                            value = self.eval_arithmetic(&expression);
                            pending_equates.push((
                                name.clone(),
                                expression.split(';').next().unwrap_or("").trim().to_string(),
                            ));
                        }
                        self.symbol_table.insert(name.clone(), value as u32);
                        ast.push(Instruction::Equ { name, value });
                        continue;
                    }
                    _ => {
                        return Err(AsmError::UnknownOpcode(
                            format!("Invalid constant directive syntax: {}", trimmed),
                            line_num,
                        ));
                    }
                }
            }

            // 5. Check for MacroEnd without active macro
            if trimmed.eq_ignore_ascii_case(".endm") {
                return Err(AsmError::UnknownOpcode(
                    format!("Unexpected .endm without active macro"),
                    line_num,
                ));
            }

            // 6. Check if this line is a Macro Invocation
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if !parts.is_empty() {
                let direct = self
                    .macro_table
                    .get(&parts[0].to_uppercase())
                    .cloned()
                    .map(|template| (template, 0usize, None));
                let labeled = if parts.len() > 1 {
                    self.macro_table
                        .get(&parts[1].to_uppercase())
                        .cloned()
                        .map(|template| (template, trimmed.find(parts[1]).unwrap_or(parts[0].len()), Some(parts[0])))
                } else {
                    None
                };
                let macro_call = direct.or(labeled).filter(|(template, start, _)| {
                    template.params.is_empty() || !trimmed[start + parts[if *start == 0 { 0 } else { 1 }].len()..].trim().is_empty()
                });
                if let Some((macro_template, macro_start, label)) = macro_call {
                    let macro_name_len = if macro_start == 0 { parts[0].len() } else { parts[1].len() };
                    let arg_str = trimmed[macro_start + macro_name_len..].trim();
                    let args = parser::split_asm_operands(arg_str);

                    let mut expanded_lines = Vec::new();
                    if let Some(label) = label {
                        expanded_lines.push(label.to_string());
                    }
                    let mut local_labels: HashMap<String, String> = HashMap::new();
                    for body_line in &macro_template.body {
                        let mut expanded = substitute_macro_parameters(
                            body_line,
                            &macro_template.params,
                            &args,
                        );

                        let mut question_index = expanded.find('?');
                        while let Some(index) = question_index {
                            let prefix_start = expanded[..index]
                                .char_indices()
                                .rev()
                                .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
                                .map(|(pos, c)| pos + c.len_utf8())
                                .unwrap_or(0);
                            let local = expanded[prefix_start..index].to_string();
                            if !local.is_empty() && !local_labels.contains_key(&local) {
                                local_labels.insert(
                                    local.clone(),
                                    format!("{}_macro{}", local, macro_expansion_count),
                                );
                            }
                            question_index = expanded[index + 1..].find('?').map(|pos| index + 1 + pos);
                        }
                        for (local, unique) in &local_labels {
                            expanded = expanded.replace(&format!("{}?", local), unique);
                        }
                        expanded_lines.push(expanded);
                    }
                    macro_expansion_count += 1;
                    for expanded_line in expanded_lines.into_iter().rev() {
                        line_queue.push_front(expanded_line);
                    }
                    continue;
                }
            }

            // 7. Check for Labels
            let mut is_label = false;
            let mut label_name_to_insert = String::new();

            if let Some(stripped) = trimmed.strip_prefix('#') {
                let parts: Vec<&str> = stripped.split_whitespace().collect();
                if let Some(first_word) = parts.first().copied() {
                    let remainder = stripped[first_word.len()..].trim_start();
                    let is_equ = remainder.to_ascii_lowercase().starts_with(".equ")
                        || remainder.to_ascii_lowercase().starts_with("equ ");
                    if !is_equ {
                        label_name_to_insert = first_word.trim_end_matches(':').to_string();
                        is_label = true;
                        trimmed = remainder;
                    }
                }
            } else if let Some(stripped) = trimmed.strip_suffix(':') {
                label_name_to_insert = stripped.trim().to_string();
                is_label = true;
            } else {
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if !parts.is_empty() {
                    let first_word = parts[0];
                    let upper_first = first_word.to_uppercase();

                    let is_opcode = matches!(
                        upper_first.as_str(),
                        "MOVE"
                            | "MOVB"
                            | "INC"
                            | "DEC"
                            | "MOVK"
                            | "MOVX"
                            | "MOVY"
                            | "RL"
                            | "RR"
                            | "ADDI"
                            | "ADDK"
                            | "SUB"
                            | "SUBK"
                            | "CMP"
                            | "CMPI"
                            | "JMP"
                            | "NOP"
                            | "RETS"
                            | "SLL"
                            | "SRL"
                            | "SLA"
                            | "SRA"
                            | "AND"
                            | "ANDN"
                            | "ANDI"
                            | "OR"
                            | "ORI"
                            | "XOR"
                            | "XORI"
                            | "MOVI"
                            | "JR"
                            | "JRUC"
                            | "JREQ"
                            | "JRNE"
                            | "JRNZ"
                            | "JRZ"
                            | "JRC"
                            | "JRNC"
                            | "JRLT"
                            | "JRGT"
                            | "JRGE"
                            | "JRHS"
                            | "JRLO"
                            | "JRZ"
                            | "JRN"
                            | "JRNZ"
                            | "JEQ"
                            | "JNE"
                            | "JLT"
                            | "JGT"
                            | "CALL"
                            | "CALLA"
                            | "CALLR"
                            | "JSRP"
                            | "JAUC"
                            | "CALLI"
                            | "BTST"
                            | "CLR"
                            | "PIXT"
                            | "FILL"
                            | "NOT"
                            | "DSJ"
                            | "DSJS"
                            | "SETF"
                            | "MMFM"
                            | "EXGPC"
                            | "TRAP"
                            | "LINE"
                    );
                    let is_directive = first_word.starts_with('.');
                    let is_macro = self.macro_table.get(&upper_first).is_some_and(|template| {
                        template.params.is_empty() || parts.len() > 1
                    });

                    if upper_first == "NOP"
                        && parts.get(1).is_some_and(|word| matches!(word.to_ascii_uppercase().as_str(), "DSJ" | "DSJS"))
                    {
                        label_name_to_insert = first_word.to_string();
                        is_label = true;
                        trimmed = trimmed[first_word.len()..].trim();
                    } else if !is_opcode
                        && !is_directive
                        && !is_macro
                        && first_word
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
                    {
                        if parts.len() == 1 {
                            label_name_to_insert = first_word.to_string();
                            is_label = true;
                            trimmed = &trimmed[first_word.len()..].trim();
                        } else if first_word.starts_with('$') {
                            label_name_to_insert = first_word.to_string();
                            is_label = true;
                            trimmed = &trimmed[first_word.len()..].trim();
                        } else {
                            let second_word = parts[1];
                            let upper_second = second_word.to_uppercase();
                            let second_is_opcode = matches!(
                                upper_second.as_str(),
                                "MOVE"
                                    | "MOVB"
                                    | "INC"
                                    | "DEC"
                                    | "MOVK"
                                    | "MOVX"
                                    | "MOVY"
                                    | "RL"
                                    | "RR"
                                    | "ADDI"
                                    | "ADDK"
                                    | "ADD"
                                    | "ADDXY"
                                    | "SUB"
                                    | "SUBXY"
                                    | "SUBI"
                                    | "SUBK"
                                    | "CMP"
                                    | "CMPI"
                                    | "JMP"
                                    | "NOP"
                                    | "RETS"
                                    | "SLL"
                                    | "SRL"
                                    | "SLA"
                                    | "SRA"
                                    | "AND"
                                    | "ANDN"
                                    | "ANDI"
                                    | "OR"
                                    | "ORI"
                                    | "XOR"
                                    | "XORI"
                                    | "MOVI"
                                    | "JR"
                                    | "JRUC"
                                    | "JREQ"
                                    | "JRNE"
                                    | "JRNZ"
                                    | "JRZ"
                                    | "JRC"
                                    | "JRNC"
                                    | "JRLT"
                                    | "JRGT"
                                    | "JRGE"
                                    | "JRHS"
                                    | "JRLO"
                                    | "JRNV"
                                    | "JRZ"
                                    | "JRN"
                                    | "JRNZ"
                                    | "JEQ"
                                    | "JNE"
                                    | "JLT"
                                    | "JGT"
                                    | "CALL"
                                    | "CALLA"
                                    | "CALLR"
                                    | "JSRP"
                                    | "JAUC"
                                    | "CALLI"
                                    | "BTST"
                                    | "CLR"
                                    | "PIXT"
                                    | "FILL"
                                    | "NOT"
                                    | "DSJ"
                                    | "DSJS"
                                    | "SETF"
                                    | "SETC"
                                    | "DINT"
                                    | "EINT"
                                    | "MMFM"
                                    | "MMTM"
                                    | "EXGPC"
                                    | "TRAP"
                                    | "LINE"
                            );
                            if second_word.starts_with('.')
                                || second_is_opcode
                                || self.macro_table.contains_key(&upper_second)
                            {
                                label_name_to_insert = first_word.to_string();
                                is_label = true;
                                trimmed = trimmed[first_word.len()..].trim();
                            }
                        }
                    }
                }
            }

            if is_label {
                self.symbol_table
                    .insert(label_name_to_insert.clone(), self.current_address);
                ast.push(Instruction::Label(label_name_to_insert));
                if trimmed.is_empty() {
                    continue;
                }
            }

            // Assignment directives may omit the leading dot in some source
            // variants and can be spaced around their comma.
            let upper_assignment = trimmed.to_uppercase();
            if upper_assignment.starts_with(".ASG") || upper_assignment.starts_with("ASG ") {
                let assignment = trimmed
                    .split_whitespace()
                    .skip(1)
                    .collect::<Vec<_>>()
                    .join(" ");
                let parts: Vec<&str> = assignment.split(',').collect();
                if parts.len() >= 2 {
                    let alias = parts[1].trim();
                    if !alias.is_empty() {
                        let value = self.eval_arithmetic(parts[0].trim()) as u32;
                        self.symbol_table.insert(alias.to_string(), value);
                        self.assignment_aliases.insert(alias.to_string(), parts[0].trim().to_string());
                    }
                }
                ast.push(Instruction::Directive {
                    name: ".asg".to_string(),
                    args: parts.iter().map(|s| s.trim().to_string()).collect(),
                });
                continue;
            }

            // 8. Check for Directives (e.g. ".WORD")
            if trimmed.starts_with('.') {
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                let name = parts[0].to_string();
                let args_text = trimmed[name.len()..].trim();
                let mut args = parser::split_asm_operands(args_text);
                if name.eq_ignore_ascii_case(".WORD") || name.eq_ignore_ascii_case(".LONG") {
                    for arg in &mut args {
                        *arg = arg.trim_start_matches('#').trim().to_string();
                        if !arg.starts_with('[')
                            && expression_references_symbol(arg, &volatile_symbols)
                        {
                            *arg = self.eval_arithmetic(arg).to_string();
                        }
                    }
                }

                if name.eq_ignore_ascii_case(".WORD") {
                    self.current_address += (args.len() * 2) as u32;
                } else if name.eq_ignore_ascii_case(".LONG") {
                    self.current_address += (args.len() * 4) as u32;
                } else if name.eq_ignore_ascii_case(".STRING") || name.eq_ignore_ascii_case(".EVEN") {
                    let directive = Instruction::Directive { name: name.clone(), args: args.clone() };
                    self.current_address += encoder::encode_instruction(&directive, self.current_address, &self.symbol_table)
                        .map_err(|error| AsmError::Parse { line: line_num, message: error.to_string() })?.len() as u32;
                }
                if name.eq_ignore_ascii_case(".asg") || name.eq_ignore_ascii_case(".eval") {
                    let assignment = if args.len() >= 2 {
                        Some((args[0].as_str(), args[1].as_str()))
                    } else {
                        args.first().and_then(|arg| {
                            arg.split_once(',')
                                .map(|(expression, alias)| (expression, alias))
                        })
                    };
                    if let Some((expression, alias)) = assignment {
                        let alias = alias.trim();
                        if !alias.is_empty() {
                            let value = self.eval_arithmetic(expression.trim()) as u32;
                            if name.eq_ignore_ascii_case(".asg") {
                                self.assignment_aliases.insert(alias.to_string(), expression.trim().to_string());
                            } else {
                                self.assignment_aliases.remove(alias);
                            }
                            self.symbol_table.insert(alias.to_string(), value);
                            if name.eq_ignore_ascii_case(".eval") {
                                volatile_symbols.insert(alias.to_ascii_uppercase());
                            }
                        }
                    }
                }
                if name.eq_ignore_ascii_case(".globl")
                    || name.eq_ignore_ascii_case(".global")
                    || name.eq_ignore_ascii_case(".ref")
                {
                    for arg in args.iter().flat_map(|arg| arg.split(',')) {
                        let symbol = arg.trim();
                        if !symbol.is_empty() {
                            if self.symbol_table.get(symbol).is_none() {
                                self.symbol_table.insert(symbol.to_string(), 0);
                            }
                        }
                    }
                }

                ast.push(Instruction::Directive { name, args });
                continue;
            }

            // Check for .usect directive
            if trimmed.to_uppercase().contains(".USECT") {
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if !parts.is_empty() {
                    let name = parts[0].to_string();
                    self.symbol_table.insert(name.clone(), self.current_address);

                    let args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
                    ast.push(Instruction::Directive {
                        name: ".usect".to_string(),
                        args,
                    });
                    continue;
                }
            }

            // Check for mmtm / mmfm stack instructions
            if upper_trimmed.starts_with("MMTM") || upper_trimmed.starts_with("MMFM") {
                let parts: Vec<&str> = trimmed.splitn(2, char::is_whitespace).collect();
                if !parts.is_empty() {
                    let name = parts[0].to_uppercase();
                    let args: Vec<String> = if parts.len() > 1 {
                        parts[1].split(',').map(|s| s.trim().to_string()).collect()
                    } else {
                        Vec::new()
                    };

                    self.current_address += 4;

                    ast.push(Instruction::Directive { name, args });
                    continue;
                }
            }

            // 9. Parse Instructions with move/movi/jrz/jrn translation and size suffix stripping
            let mut parser_trimmed = trimmed.to_string();
            if let Some((mnemonic, operands)) = trimmed.split_once(char::is_whitespace) {
                let (names, registers): (Vec<_>, Vec<_>) = register_aliases.iter()
                    .map(|(name, register)| (name.clone(), register.clone())).unzip();
                parser_trimmed = format!("{} {}", mnemonic, substitute_macro_parameters(operands, &names, &registers));
            }
            let upper_pt = parser_trimmed.to_uppercase();
            if (upper_pt.starts_with("MOVE")
                || upper_pt.starts_with("MOVI")
                || upper_pt.starts_with("ADD")
                || upper_pt.starts_with("SUB"))
                && (parser_trimmed.ends_with(",L")
                    || parser_trimmed.ends_with(",W")
                    || parser_trimmed.ends_with(",l")
                    || parser_trimmed.ends_with(",w"))
            {
                parser_trimmed = parser_trimmed[..parser_trimmed.len() - 2]
                    .trim()
                    .to_string();
            }

            let upper_pt_2 = parser_trimmed.to_uppercase();
            if upper_pt_2.starts_with("MOVI") {
                let remainder = parser_trimmed[4..].trim().trim_start_matches('#').trim();
                parser_trimmed = format!("move\t#{}", remainder);
            }

            match parser::parse_line(&parser_trimmed) {
                Ok((_, instruction)) => {
                    match &instruction {
                        Instruction::Move { src, dst, field } => {
                            let mut extra_words = 0;
                            if matches!(src, Operand::Immediate(_) | Operand::Symbol(_)) {
                                extra_words += 1;
                            }
                            if matches!(
                                src,
                                Operand::DirectMemory(_) | Operand::IndirectOffset { .. }
                            ) {
                                extra_words += 1;
                            }
                            if matches!(
                                dst,
                                Operand::DirectMemory(_) | Operand::IndirectOffset { .. }
                            ) {
                                extra_words += 1;
                            }
                            if field.is_some() {
                                extra_words += 1;
                            }
                            self.current_address += 2 + (extra_words * 2);
                        }
                        Instruction::Add { src, dst }
                        | Instruction::Sub { src, dst }
                        | Instruction::Sll { src, dst }
                        | Instruction::Srl { src, dst }
                        | Instruction::And { src, dst }
                        | Instruction::Or { src, dst }
                        | Instruction::Xor { src, dst } => {
                            let mut extra_words = 0;
                            if matches!(src, Operand::Immediate(_) | Operand::Symbol(_)) {
                                extra_words += 1;
                            }
                            if matches!(
                                src,
                                Operand::DirectMemory(_) | Operand::IndirectOffset { .. }
                            ) {
                                extra_words += 1;
                            }
                            if matches!(
                                dst,
                                Operand::DirectMemory(_) | Operand::IndirectOffset { .. }
                            ) {
                                extra_words += 1;
                            }
                            self.current_address += 2 + (extra_words * 2);
                        }
                        Instruction::Cmp { src, .. } => {
                            let is_immediate =
                                matches!(src, Operand::Immediate(_) | Operand::Symbol(_));
                            if is_immediate {
                                self.current_address += 4;
                            } else {
                                self.current_address += 2;
                            }
                        }
                        Instruction::Setf { .. } => {
                            self.current_address += 4; // Base opcode + 16-bit extension word
                        }
                        Instruction::Jlt { .. }
                        | Instruction::Jgt { .. }
                        | Instruction::Jmp { .. } => {
                            self.current_address += 2;
                        }
                        Instruction::Rets => {
                            self.current_address += 2;
                        }
                        _ => {
                            self.current_address += 2;
                        }
                    }

                    ast.push(instruction);
                }
                Err(e) => {
                    return Err(AsmError::UnknownOpcode(
                        format!("{} (Parse error: {:?})", trimmed, e),
                        line_num,
                    ));
                }
            }
        }

        // Re-layout after labels and equates are known. Branches that promote to
        // the two-word form move later labels, so repeat until addresses settle.
        let mut latest_equates = HashMap::<String, String>::new();
        for (name, expression) in pending_equates.drain(..) {
            latest_equates.insert(name, expression);
        }
        let mut pending_equates: Vec<(String, String)> = latest_equates.into_iter().collect();
        pending_equates.sort_by(|left, right| left.0.cmp(&right.0));
        let layout_names: Vec<String> = ast.iter().filter_map(|instruction| {
            if let Instruction::Label(name) = instruction { Some(name.clone()) } else { None }
        }).collect();
        let branch_count = ast.iter().filter(|instruction| matches!(
            instruction,
            Instruction::Jr { .. } | Instruction::Jrcc { .. } | Instruction::Jeq { .. }
                | Instruction::Jne { .. } | Instruction::Jlt { .. } | Instruction::Jgt { .. }
        )).count();
        let max_layout_passes = branch_count + 3;
        let mut layout_converged = false;
        for _ in 0..max_layout_passes {
            let previous_symbols: Vec<Option<u32>> = layout_names.iter().map(|name| {
                self.symbol_table.get(name).copied()
            }).collect();
            let mut address = 0u32;
            for instruction in &mut ast {
                match instruction {
                    Instruction::Label(name) => {
                        self.symbol_table.insert(name.clone(), address);
                    }
                    Instruction::Equ { .. } => {}
                    Instruction::Directive { name, args } if name.eq_ignore_ascii_case(".WORD") => {
                        address += (args.len() * 2) as u32;
                    }
                    Instruction::Directive { name, args } if name.eq_ignore_ascii_case(".LONG") => {
                        address += (args.len() * 4) as u32;
                    }
                    _ => {
                        let size = layout_instruction_size(instruction, address, &self.symbol_table);
                        if size > 2 {
                            let long_branch = match instruction {
                                Instruction::Jr { label } => Some((0, label.clone())),
                                Instruction::Jeq { label } => Some((0xA, label.clone())),
                                Instruction::Jne { label } => Some((0xB, label.clone())),
                                Instruction::Jlt { target } => Some((0x4, target.clone())),
                                Instruction::Jgt { target } => Some((0x7, target.clone())),
                                Instruction::Jrcc { condition, target } => Some((*condition, target.clone())),
                                _ => None,
                            };
                            if let Some((condition, target)) = long_branch {
                                *instruction = if size == 6 {
                                    Instruction::Jacc { condition, target }
                                } else {
                                    Instruction::LongJrcc { condition, target }
                                };
                            }
                        }
                        address += size;
                    }
                }
            }

            // Resolve forward and chained equates using the latest label layout.
            let mut symbols: HashMap<String, u32> = self.symbol_table.symbols.iter()
                    .map(|(key, value)| (key.to_ascii_uppercase(), *value)).collect();
            // Legacy equate chains are shallow; cap malformed or cyclic chains
            // so branch relaxation does not multiply their cost across every pass.
            let max_equate_passes = pending_equates.len().min(16);
            for _ in 0..max_equate_passes {
                let values: Vec<(String, u32)> = pending_equates.iter().map(|(name, expr)| {
                    (name.clone(), parser::parse_expression_value(&format!(".equ {} {}", name, expr), &symbols) as u32)
                }).collect();
                let changed = values.iter().any(|(name, value)| self.symbol_table.get(name) != Some(value));
                for (name, value) in values {
                    symbols.insert(name.to_ascii_uppercase(), value);
                    self.symbol_table.insert(name, value);
                }
                if !changed { break; }
            }
            let current_symbols: Vec<Option<u32>> = layout_names.iter().map(|name| {
                self.symbol_table.get(name).copied()
            }).collect();
            if current_symbols == previous_symbols {
                layout_converged = true;
                break;
            }
        }
        if !layout_converged && branch_count > 0 {
            eprintln!("Warning: branch layout did not converge within {max_layout_passes} passes");
        }
        // Finish deeper equate chains once the final label addresses are known.
        for _ in 0..256 {
            let mut symbols: HashMap<String, u32> = self.symbol_table.symbols.iter()
                .map(|(key, value)| (key.to_ascii_uppercase(), *value)).collect();
            let values: Vec<(String, u32)> = pending_equates.iter().map(|(name, expr)| {
                (name.clone(), parser::parse_expression_value(&format!(".equ {} {}", name, expr), &symbols) as u32)
            }).collect();
            let changed = values.iter().any(|(name, value)| self.symbol_table.get(name) != Some(value));
            for (name, value) in values {
                symbols.insert(name.to_ascii_uppercase(), value);
                self.symbol_table.insert(name, value);
            }
            if !changed { break; }
        }
        Ok(ast)
    }

    pub fn pass_two(&self, ast: &[Instruction]) -> Result<Vec<u8>, AsmError> {
        encoder::generate_binary(ast, &self.symbol_table).map_err(|e| AsmError::Parse {
            line: 0,
            message: format!("Encoding/Symbol error: {:?}", e),
        })
    }
}

/// Recursively loads a source file and expands any .include directives
pub fn load_source_file(path: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    let content = String::from_utf8_lossy(&bytes);
    let mut resolved_lines = Vec::new();
    let base_dir = path.parent().unwrap_or(Path::new("."));

    for raw_line in content.lines() {
        let trimmed = raw_line.trim();

        // Check if the line is an include directive (e.g., .include "hardware.asm")
        if trimmed.to_lowercase().starts_with(".include") {
            if let Some(filename) = extract_quoted_filename(trimmed) {
                let include_path = base_dir.join(filename);
                println!("[DEBUG Include] Loading file: {:?}", include_path);

                // Recursively load the included file's lines
                let nested_lines = load_source_file(&include_path)?;
                resolved_lines.extend(nested_lines);
            } else {
                return Err(format!("Invalid .include syntax: {}", raw_line).into());
            }
        } else {
            resolved_lines.push(raw_line.to_string());
        }
    }

    Ok(resolved_lines)
}

/// Helper to extract text inside double quotes (e.g., "hardware.asm")
fn extract_quoted_filename(line: &str) -> Option<String> {
    if let (Some(start), Some(end)) = (line.find('"'), line.rfind('"')) {
        if start < end {
            return Some(line[start + 1..end].to_string());
        }
    }
    let (_, filename) = line.split_once(char::is_whitespace)?;
    let filename = filename.trim().split_whitespace().next()?.trim_matches('"');
    (!filename.is_empty()).then(|| filename.to_string())
}

fn main() -> Result<(), AsmError> {
    let args = Args::parse();

    println!("Starting TMS34010 Assembly process...");
    println!("Input file: {:?}", args.input);

    let input_path = Path::new(&args.input);

    let lines = load_source_file(input_path).map_err(|e| AsmError::Parse {
        line: 0,
        message: format!("File include error: {}", e),
    })?;

    let mut assembler = Assembler::new();

    // Pass 1: Parse & build symbol map
    println!("Executing Pass 1 (Parsing & Symbol Resolution)...");

    // Convert Vec<String> into a vector of &str references
    let line_refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();

    // Now pass the &str slice to pass_one
    let ast = assembler.pass_one(&line_refs)?;

    if args.verbose {
        println!("Symbol Table: {:?}", assembler.symbol_table);
    }

    // Pass 2: Code generation
    println!("Executing Pass 2 (Machine Code Generation)...");
    let binary_output = assembler.pass_two(&ast)?;

    if args.object_format {
        let object = ObjectFile::from_assembler(input_path, &assembler, &ast, &binary_output);
        let json = serde_json::to_vec_pretty(&object).map_err(|error| AsmError::Parse {
            line: 0,
            message: format!("Failed to serialize object file: {error}"),
        })?;
        std::fs::write(&args.output, json)?;
        println!(
            "Successfully emitted relocatable object {} bytes -> {:?}",
            binary_output.len(),
            args.output
        );
    } else {
        std::fs::write(&args.output, &binary_output)?;
        println!(
            "Successfully assembled {} bytes -> {:?}",
            binary_output.len(),
            args.output
        );
    }

    if !args.object_format {
        verify::verify_rom(&args.output, &ast, &assembler.symbol_table)
            .map_err(|message| AsmError::Parse { line: 0, message })?;
    }

    // Conditionally apply ROM interleaver based on CLI argument
    if !args.object_format && args.interleave_ways > 1 {
        println!(
            "Splitting binary for arcade hardware ({}-way interleave)...",
            args.interleave_ways
        );
        interleaver::write_split_roms(&args.output, &binary_output, args.interleave_ways).map_err(
            |e| AsmError::Parse {
                line: 0,
                message: format!("Interleave error: {:?}", e),
            },
        )?;
    } else {
        println!("Interleaving skipped (1-way mode).");
    }

    // Word-level hex dump + little-endian verification against the symbol table
    if args.dump {
        let listing =
            dump::dump_listing(&ast, &binary_output, &assembler.symbol_table).map_err(|e| {
                AsmError::Parse {
                    line: 0,
                    message: format!("Encode error: {:?}", e),
                }
            })?;
        print!("{}", listing);

        let checks = dump::verify(&ast, &binary_output, &assembler.symbol_table).map_err(|e| {
            AsmError::Parse {
                line: 0,
                message: format!("Encode error: {:?}", e),
            }
        })?;

        for (ok, msg) in &checks {
            println!("  [{}] {}", if *ok { "PASS" } else { "FAIL" }, msg);
        }
        let failed = checks.iter().filter(|(ok, _)| !ok).count();
        println!("\n  {} checks, {} failed", checks.len(), failed);
        if failed > 0 {
            return Err(AsmError::Parse {
                line: 0,
                message: format!("{} verification check(s) failed", failed),
            });
        }
    }

    Ok(())
}

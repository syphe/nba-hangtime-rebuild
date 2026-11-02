use clap::Parser;
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use thiserror::Error;

use std::fs;

mod dump;
mod encoder;
mod interleaver;
mod isa;
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
                let name = name.strip_suffix(".S").unwrap_or(name);
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
                    // Legacy string parameters also occur without colons, e.g. P1.S.
                    if line[end..].starts_with(".S") && line[end + 2..].chars().next()
                        .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '.') {
                        while chars.peek().is_some_and(|(position, _)| *position < end + 2) {
                            chars.next();
                        }
                    }
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
    /// Reject instruction mnemonics that currently emit no code.
    #[arg(long)]
    deny_omitted_instructions: bool,

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
    let symbolic=std::cell::Cell::new(false);
    let _=parser::evaluate_arithmetic_checked(value, |_| {symbolic.set(true);0});
    symbolic.get()
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
    omitted_instructions: Vec<OmittedInstruction>,
}

#[derive(Serialize, Debug, PartialEq)]
struct OmittedInstruction {
    mnemonic: String,
    count: usize,
    examples: Vec<String>,
}

fn omitted_instructions(ast: &[Instruction]) -> Vec<OmittedInstruction> {
    let mut counts = std::collections::BTreeMap::<String, (usize, Vec<String>)>::new();
    for instruction in ast {
        if let Instruction::Directive { name, args } = instruction {
            if !name.starts_with('.') && !matches!(name.to_ascii_uppercase().as_str(), "MMTM" | "MMFM") {
                let (count, examples) = counts.entry(name.to_ascii_uppercase()).or_default();
                *count += 1;
                let example = args.join(", ");
                if examples.len() < 3 && !examples.contains(&example) { examples.push(example); }
            }
        }
    }
    counts.into_iter().map(|(mnemonic, (count, examples))| OmittedInstruction { mnemonic, count, examples }).collect()
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
                Instruction::Dsjs { target, .. } if target != "$" => Some(target.as_str()),
                Instruction::Dsjcc { target, .. } => Some(target.as_str()),
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
                        Instruction::Jacc { .. } | Instruction::Jmp { .. } => "absolute32",
                        Instruction::Dsjs { short: true, .. } => "tms34010-dsjs5",
                        Instruction::Dsjs { short: false, .. } | Instruction::Dsjcc { .. } => "tms34010-relative16",
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
            if let Instruction::Native(native) = instruction {
                if let Some((symbol,width)) = native.immediate_fixup() {
                    fixups.push(ObjectFixup {offset:section_pc+2,object_offset:offset+2,
                        section:current_section.clone(),symbol,kind:if width==2 {"absolute16"} else {"absolute32"}.into()});
                }
            }
            if let Instruction::Move { src, dst, .. } | Instruction::Movb { src, dst } = instruction {
                let mut extension_offset = 2u32;
                for operand in [src,dst] {
                    let expression = match operand {
                        Operand::DirectMemory(expression) => Some((expression,4u32)),
                        Operand::IndirectOffset(_,expression) => Some((expression,2u32)),
                        _ => None,
                    };
                    if let Some((expression,width)) = expression {
                        if is_symbolic_expression(expression) {
                            fixups.push(ObjectFixup {
                                offset: section_pc + extension_offset,
                                object_offset: offset + extension_offset,
                                section: current_section.clone(),
                                symbol: expression.clone(),
                                kind: if width == 4 { "absolute32" } else { "absolute16" }.to_string(),
                            });
                        }
                        extension_offset += width;
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
            omitted_instructions: omitted_instructions(ast),
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
        Instruction::Native(native) => if matches!(native.format,isa::Format::Immediate {..})
            || matches!(native.format,isa::Format::Alu {..}) && !matches!(native.operands.first(),Some(Operand::Reg(_))) {
                2+native.width.unwrap_or(4) as u32
            } else {2},
        Instruction::Dsjcc {..} => 4,
        Instruction::Directive { name, .. } if name.eq_ignore_ascii_case("MMTM") || name.eq_ignore_ascii_case("MMFM") => 4,
        _ => encoder::encode_instruction(instruction, address, symbols)
            .map(|bytes| bytes.len() as u32)
            .unwrap_or(0),
    }
}

fn external_absolute_transfer<'a>(instruction: &'a Instruction, ast: &[Instruction]) -> Option<(u16, &'a str)> {
    let (opcode, target) = match instruction {
        Instruction::Call { label } | Instruction::Callr { label } => (0x0D5F, label.as_str()),
        Instruction::Jr { label } => (0xC080, label.as_str()),
        Instruction::Jmp { target: Operand::Symbol(label) } => (0xC080, label.as_str()),
        Instruction::Jeq { label } => (0xCA80, label.as_str()),
        Instruction::Jne { label } => (0xCB80, label.as_str()),
        Instruction::Jlt { target } => (0xC480, target.as_str()),
        Instruction::Jgt { target } => (0xC780, target.as_str()),
        Instruction::Jrcc { condition, target } => (0xC080 | ((*condition as u16) << 8), target.as_str()),
        Instruction::LongJrcc { condition, target } => (0xC080 | ((*condition as u16) << 8), target.as_str()),
        _ => return None,
    };
    if matches!(instruction, Instruction::Call { .. }) && parser::parse_register(target).is_ok_and(|(rest, _)| rest.is_empty()) {
        return None;
    }
    let local = ast.iter().any(|candidate| matches!(candidate, Instruction::Label(name) if name.eq_ignore_ascii_case(target)));
    (!local).then_some((opcode, target))
}

pub struct Assembler {
    pub symbol_table: SymbolTable,
    pub macro_table: HashMap<String, MacroTemplate>,
    current_address: u32,
    assignment_aliases: HashMap<String, String>,
    object_format: bool,
    register_aliases: HashMap<String, String>,
}

#[cfg(test)]
mod loop_expression_tests {
    use super::*;

    #[test]
    fn memory_moves_match_reference_words_and_preserve_expressions() {
        for (source, words) in [
            ("move a2,*a3", vec![0x8043u16]), ("move *b2,b3,L", vec![0x8653]),
            ("move *a2,*a3,1", vec![0x8a43]), ("move a2,*a3+,1", vec![0x9243]),
            ("move *b2+,b3", vec![0x9453]), ("move *a2+,*a3+", vec![0x9843]),
            ("move a2,-*a3,L", vec![0xa243]), ("move -*a2,a3", vec![0xa443]),
            ("move -*a2,-*a3,1", vec![0xaa43]),
            ("move a2,*a3(16)", vec![0xb043,16]), ("move *a2(-16),a3,L", vec![0xb643,0xfff0]),
            ("move *a2(16),*a3(32),1", vec![0xba43,16,32]),
            ("move *a2(16),*a3+,1", vec![0xd243,16]),
            ("move a2,@12345678h", vec![0x0582,0x5678,0x1234]),
            ("move @12345678h,b3,L", vec![0x07b3,0x5678,0x1234]),
            ("move @12345678h,@22334455h,1", vec![0x07c0,0x5678,0x1234,0x4455,0x2233]),
            ("move @12345678h,*b3+,L", vec![0xd613,0x5678,0x1234]),
            ("movb @12345678h,b3", vec![0x07f3,0x5678,0x1234]),
            ("movb b3,@12345678h", vec![0x05f3,0x5678,0x1234]),
            ("movb @12345678h,@22334455h", vec![0x0340,0x5678,0x1234,0x4455,0x2233]),
        ] {
            let mut assembler = Assembler::new();
            let ast = assembler.pass_one(&[source]).unwrap();
            let expected = words.into_iter().flat_map(u16::to_le_bytes).collect::<Vec<_>>();
            assert_eq!(assembler.pass_two(&ast).unwrap(),expected,"{source}");
            assert!(omitted_instructions(&ast).is_empty());
        }
        let mut assembler = Assembler::new();
        let source = ".ref player\nmove @player+FIELD,a0,L\nmove a0,*a1((FIELD+16)/2),1\nmove @player,@player+32,1\nFIELD .equ 32";
        let ast = assembler.pass_one(&source.lines().collect::<Vec<_>>()).unwrap();
        let binary = assembler.pass_two(&ast).unwrap();
        assert_eq!(binary[8..10],[24,0]);
        let object = ObjectFile::from_assembler(Path::new("test.asm"),&assembler,&ast,&binary);
        assert_eq!(object.fixups.iter().map(|f| (f.kind.as_str(),f.offset,f.symbol.as_str())).collect::<Vec<_>>(),
                   vec![("absolute32",2,"player+FIELD"),("absolute16",8,"(FIELD+16)/2"),("absolute32",12,"player"),("absolute32",16,"player+32")]);
        assert!(parser::parse_move("move a0,a1 unexpected").is_err());
        // CALLR's low bits are required by the execution table, despite being
        // ignored by the disassembler's broad opcode mask. Check a real object.
        let legacy = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../BACKUP/XCODE101L/PAL.OBJ")).unwrap();
        let text_start = u32::from_le_bytes(legacy[40..44].try_into().unwrap()) as usize;
        assert_eq!(u16::from_le_bytes(legacy[text_start+36..text_start+38].try_into().unwrap()),0x0d3f);
    }

    #[test]
    fn byte_moves_match_execution_table_and_reject_invalid_forms() {
        // 34010tbl.hxx: MOVB register/indirect/displacement dispatch families.
        for (source, words) in [
            ("movb a2,*a3", vec![0x8c43u16]), ("movb *b2,b3", vec![0x8e53]),
            ("movb *a2,*a3", vec![0x9c43]), ("movb b2,*b3(16)", vec![0xac53,16]),
            ("movb *a2(-16),a3", vec![0xae43,0xfff0]),
            ("movb *b2(16),*b3(32)", vec![0xbc53,16,32]),
            ("movb sp,*b3", vec![0x8df3]), ("movb *a2,sp", vec![0x8e4f]),
        ] {
            let mut assembler = Assembler::new();
            let ast = assembler.pass_one(&[source]).unwrap();
            assert!(omitted_instructions(&ast).is_empty(), "{source}");
            let expected = words.into_iter().flat_map(u16::to_le_bytes).collect::<Vec<_>>();
            assert_eq!(assembler.pass_two(&ast).unwrap(), expected, "{source}");
        }
        for source in ["movb a2,b3", "movb a2,a3", "movb a2,*b3", "movb *a2(16),*b3(32)",
                       "movb *a2+,a3", "movb a2,-*a3"] {
            let mut assembler = Assembler::new();
            let ast = assembler.pass_one(&[source]).unwrap();
            assert!(assembler.pass_two(&ast).is_err(), "{source}");
        }
        for source in ["movb a2,*a3 junk", "movb a2,*a3,1", "movb *a2(16),a3+4"] {
            assert!(parser::parse_movb(source).is_err(), "{source}");
        }
        // Independently check real UTIL words at the bit addresses in UTIL.LST.
        let legacy = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../BACKUP/XCODE101L/UTIL.OBJ")).unwrap();
        let text_start = u32::from_le_bytes(legacy[40..44].try_into().unwrap()) as usize;
        for (source, bit_address) in [("movb a9,*a1",0x1ce0), ("movb *a8,a1",0x2300), ("movb *a8,a14",0x2bd0)] {
            let mut assembler = Assembler::new();
            let ast = assembler.pass_one(&[source]).unwrap();
            let offset = text_start + bit_address / 8;
            assert_eq!(assembler.pass_two(&ast).unwrap(), legacy[offset..offset+2], "{source}");
        }
        let mut assembler = Assembler::new();
        let source = "movb *b2(FIELD+16),*b3(FIELD-16)\nFIELD .equ 32";
        let ast = assembler.pass_one(&source.lines().collect::<Vec<_>>()).unwrap();
        let binary = assembler.pass_two(&ast).unwrap();
        assert_eq!(binary, [0x53,0xbc,48,0,16,0]);
        let object = ObjectFile::from_assembler(Path::new("test.asm"),&assembler,&ast,&binary);
        assert_eq!(object.fixups.iter().map(|f| (f.kind.as_str(), f.offset, f.symbol.as_str())).collect::<Vec<_>>(),
                   [("absolute16",2,"FIELD+16"),("absolute16",4,"FIELD-16")]);
    }

    #[test]
    fn production_test_and_swap_macros_consume_string_parameter_suffixes() {
        let params = vec!["R1".into()];
        let args = vec!["a2".into()];
        assert_eq!(substitute_macro_parameters(":R1.S: R1.S R1_SAVE :OTHER.S:", &params, &args),
                   "a2 a2 R1_SAVE :OTHER.S:");
        assert_eq!(substitute_macro_parameters("R1.SAVED", &params, &args), "a2.SAVED");
        let header = load_source_file(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../SRC/MACROS.HDR")).unwrap().join("\n");
        let source = format!("{header}\nTEST a2\nSWAP b2,b3");
        let mut assembler = Assembler::new();
        let ast = assembler.pass_one(&source.lines().collect::<Vec<_>>()).unwrap();
        assert!(omitted_instructions(&ast).is_empty());
        assert_eq!(assembler.pass_two(&ast).unwrap(),
                   [0x4c42u16,0x5653,0x5672,0x5653].into_iter().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
    }

    #[test]
    fn hash_targets_emit_branches_calls_and_keep_local_symbol_names() {
        let source = "outside\nnop\njrle #local\ncallr #local\n#local rets\njrne #local";
        let mut assembler = Assembler::new();
        let ast = assembler.pass_one(&source.lines().collect::<Vec<_>>()).unwrap();
        assert!(omitted_instructions(&ast).is_empty());
        assert_eq!(assembler.symbol_table.get("#local"), Some(&8));
        assert_eq!(assembler.symbol_table.get("local"), Some(&8));
        let binary = assembler.pass_two(&ast).unwrap();
        assert_eq!(binary, vec![0,3,2,0xc6,0x3f,0x0d,0,0,0x60,9,0xfe,0xcb]);
        let object = ObjectFile::from_assembler(Path::new("test.asm"), &assembler, &ast, &binary);
        assert_eq!(object.fixups.iter().map(|fixup| (fixup.symbol.as_str(),fixup.kind.as_str(),fixup.offset)).collect::<Vec<_>>(),
                   vec![("#local","tms34010-relative",2),("#local","tms34010-relative16",6),("#local","tms34010-relative",10)]);
        for (source, expected) in [("jage #local",5), ("jaz #local",10), ("janz #local",11), ("jac #local",8)] {
            assert!(matches!(parser::parse_line(source).unwrap().1, Instruction::Jacc { condition, target } if condition == expected && target == "#local"));
        }
    }

    #[test]
    fn hash_locals_do_not_shadow_global_names_or_guess_between_duplicates() {
        let mut assembler = Assembler::new();
        let source = "dest\nnop\njr #dest\n#dest\nrets\njr dest";
        let ast = assembler.pass_one(&source.lines().collect::<Vec<_>>()).unwrap();
        assert_eq!(assembler.symbol_table.get("dest"), Some(&0));
        assert_eq!(assembler.symbol_table.get("#dest"), Some(&6));
        assert!(assembler.pass_two(&ast).is_ok());
        let mut assembler = Assembler::new();
        assert!(assembler.pass_one(&["#loop", "nop", "#loop", "jr #loop"]).unwrap_err().to_string().contains("ambiguous repeated local target"));
        let mut assembler = Assembler::new();
        let ast = assembler.pass_one(&["jr #ordinary", "nop", "ordinary", "rets"]).unwrap();
        assert!(matches!(&ast[0], Instruction::Jr { label } if label == "ordinary"));
        assert!(parser::parse_relative_conditional("jrle #local+4").is_err());
        let mut assembler = Assembler::new();
        let ast = assembler.pass_one(&["entry jrle #done", "other jaz #done", "#done", "rets"]).unwrap();
        assert!(omitted_instructions(&ast).is_empty());
        assert!(assembler.symbol_table.get("entry").is_some());
        assert!(assembler.symbol_table.get("other").is_some());
        let mut assembler = Assembler::new();
        let ast = assembler.pass_one(&["dest .equ 42", "jr #dest", "#dest", "rets"]).unwrap();
        let binary = assembler.pass_two(&ast).unwrap();
        let object = ObjectFile::from_assembler(Path::new("test.asm"), &assembler, &ast, &binary);
        assert_eq!(assembler.symbol_table.get("dest"),Some(&42));
        assert_eq!(object.symbols.iter().find(|symbol| symbol.name == "dest").unwrap().section, "");
        let mut assembler = Assembler::new();
        let ast = assembler.pass_one(&["target .set b4", "callr #target", "#target", "rets"]).unwrap();
        assert!(matches!(&ast[0], Instruction::Callr { label } if label == "#target"));
    }

    #[test]
    fn elseif_chooses_only_first_match_and_respects_inactive_parents() {
        let source = ".if 0\n.word 99\n.elseif 1\n.word 1\n.elseif 1\n.word 99\n.else\n.word 99\n.endif\n.if 0\n.if 0\n.elseif 1\n.word 99\n.else\n.word 99\n.endif\n.endif\n.if 0\n.elseif 0\n.else\n.word 2\n.endif";
        let mut assembler = Assembler::new();
        let ast = assembler.pass_one(&source.lines().collect::<Vec<_>>()).unwrap();
        assert_eq!(assembler.pass_two(&ast).unwrap(), vec![1,0,2,0]);
        for source in [".else", ".elseif 1", ".endif", ".if 1", ".if 1\n.else\n.else\n.endif", ".if 1\n.else\n.elseif 1\n.endif"] {
            assert!(Assembler::new().pass_one(&source.lines().collect::<Vec<_>>()).is_err(), "{source}");
        }
    }

    #[test]
    fn production_stack_macros_select_all_registers() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../SRC/MACROS.HDR");
        let header = load_source_file(&path).unwrap().join("\n");
        for (count, store_mask, load_mask) in [(3,0xe000u16,7u16), (5,0xf800,0x1f), (10,0xffc0,0x3ff)] {
            let registers = (0..count).map(|index| format!("a{index}")).collect::<Vec<_>>().join(",");
            let source = format!("{header}\nPUSH {registers}\nPULL {registers}");
            let mut assembler = Assembler::new();
            let ast = assembler.pass_one(&source.lines().collect::<Vec<_>>()).unwrap();
            let expected = [0x098fu16,store_mask,0x09af,load_mask].into_iter().flat_map(u16::to_le_bytes).collect::<Vec<_>>();
            assert_eq!(assembler.pass_two(&ast).unwrap(), expected, "{count} registers");
        }
        for (push, pull, base) in [("PUSH","PULL",15), ("PUSHP","PULLP",12)] {
            for count in 1..=2 {
                let registers = (0..count).map(|index| format!("a{index}")).collect::<Vec<_>>().join(",");
                let source = format!("{header}\n{push} {registers}\n{pull} {registers}");
                let mut assembler = Assembler::new();
                let ast = assembler.pass_one(&source.lines().collect::<Vec<_>>()).unwrap();
                let moved = ast.iter().filter_map(|instruction| match instruction {
                    Instruction::Move { src: Operand::Reg(reg), dst: Operand::IndirectPreDec(stack), .. } => Some((reg,stack)),
                    Instruction::Move { src: Operand::IndirectPostInc(stack), dst: Operand::Reg(reg), .. } => Some((reg,stack)),
                    _ => None,
                }).map(|(reg,stack)| { assert_eq!(encoder::encode_register(stack),base); encoder::encode_register(reg) }).collect::<Vec<_>>();
                let expected: Vec<u8> = (0..count).chain((0..count).rev()).collect();
                assert_eq!(moved,expected,"{push} {count} registers");
            }
        }
        let mut assembler = Assembler::new();
        let source = "alias .set b4\n.if $isreg(alias)\n.word 1\n.endif\n.if $isreg(missing)\n.word 99\n.endif";
        let ast = assembler.pass_one(&source.lines().collect::<Vec<_>>()).unwrap();
        assert_eq!(assembler.pass_two(&ast).unwrap(), vec![1,0]);
    }

    #[test]
    fn omitted_instruction_diagnostic_counts_mnemonics_but_not_metadata() {
        let mut assembler = Assembler::new();
        let ast = assembler.pass_one(&["UNSUPPORTED a0,1", "unsupported a1,1", ".title sample", "nop", "mmtm sp,a0"]).unwrap();
        assert_eq!(omitted_instructions(&ast), vec![OmittedInstruction { mnemonic: "UNSUPPORTED".into(), count: 2, examples: vec!["a0, 1".into(),"a1, 1".into()] }]);
    }

    #[test]
    fn branch_and_call_words_match_reference_and_reserved_displacements_expand() {
        for condition in 0..16 {
            let mut symbols = SymbolTable::default();
            symbols.insert("target".into(), 4);
            let instruction = Instruction::Jrcc { condition, target: "target".into() };
            assert_eq!(encoder::encode_instruction(&instruction, 0, &symbols).unwrap(),
                       (0xc001u16 | ((condition as u16) << 8)).to_le_bytes());
        }
        for (source, pc, target, words) in [
            ("jr target", 254, 2, vec![0xc081u16]),
            ("jr target", 254, 0, vec![0xc000, 0xff7f]),
            ("jr target", 0, 2, vec![0xc000, 0xffff]),
            ("jr target", 0, 258, vec![0xc000, 0x007f]),
            ("jr target", 0, 0x20000, vec![0xc080, 0, 2]),
            ("callr target", 0, 4, vec![0x0d3f, 0]),
            ("callr target", 0, 0x20000, vec![0x0d5f, 0, 2]),
            ("dsj b3,target", 0, 0, vec![0x0d93, 0xfffe]),
            ("dsjs b3,target", 0, 0, vec![0x3c33]),
        ] {
            let mut symbols = SymbolTable::default();
            symbols.insert("target".into(), target);
            let (_, instruction) = parser::parse_line(source).unwrap();
            assert_eq!(encoder::encode_instruction(&instruction, pc, &symbols).unwrap(),
                       words.into_iter().flat_map(u16::to_le_bytes).collect::<Vec<_>>(), "{source}");
        }
        let mut assembler = Assembler::new();
        let ast = assembler.pass_one(&["jreq next", "next", "reti"]).unwrap();
        assert_eq!(assembler.pass_two(&ast).unwrap(), vec![0, 0xca, 0, 0, 0x40, 9]);
        let object = ObjectFile::from_assembler(Path::new("test.asm"), &assembler, &ast, &assembler.pass_two(&ast).unwrap());
        assert_eq!(object.fixups[0].kind, "tms34010-relative16");
        assert_eq!(object.fixups[0].offset, 2);
        for (source, expected) in [("call a2", 0x0922u16), ("call b2", 0x0932),
                                   ("jmp a2", 0x0162), ("jump b2", 0x0172), ("reti", 0x0940)] {
            let mut assembler = Assembler::new();
            let ast = assembler.pass_one(&[source]).unwrap();
            assert_eq!(assembler.pass_two(&ast).unwrap(), expected.to_le_bytes(), "{source}");
            assert!(external_absolute_transfer(&ast[0], &ast).is_none());
        }
    }

    #[test]
    fn graphics_and_rotate_opcodes_match_reference_and_reject_invalid_modes() {
        for (source, word) in [
            ("andn a2,a3", 0x5243u16), ("andn b2,b3", 0x5253),
            ("movx a2,a3", 0xec43), ("movy b2,b3", 0xee53),
            ("rl 16,a4", 0x3204), ("rl 0,b4", 0x3014), ("rl 31,b4", 0x33f4),
            ("rl a2,a3", 0x6843), ("rl b2,b3", 0x6853),
            ("andn sp,b3", 0x53f3), ("andn b15,a3", 0x53e3),
            ("fill L", 0x0fc0), ("fill XY", 0x0fe0),
            ("pixt a2,*a3.XY", 0xf043), ("pixt *a2.XY,a3", 0xf243),
            ("pixt *a2.XY,*a3.XY", 0xf443), ("pixt b2,*b3", 0xf853),
            ("pixt *b2,b3", 0xfa53), ("pixt *b2,*b3", 0xfc53),
        ] {
            let mut assembler = Assembler::new();
            let ast = assembler.pass_one(&[source]).unwrap();
            assert_eq!(assembler.pass_two(&ast).unwrap(), word.to_le_bytes(), "{source}");
        }
        for source in ["andn a2,b3", "movx a2,b3", "rl 32,a3", "rl -1,a3", "rr 1,a3",
                       "fill invalid", "pixt a2,a3", "pixt *a2.XY,*a3", "pixt a2,*b3"] {
            let mut assembler = Assembler::new();
            let ast = assembler.pass_one(&[source]).unwrap();
            assert!(assembler.pass_two(&ast).is_err(), "{source}");
        }
    }

    #[test]
    fn audited_opcodes_match_tms34010_reference_words() {
        // Golden words checked against MAME's 34010dsm.cpp and PAL.OBJ.
        for (source, words) in [
            ("nop", vec![0x0300]),
            ("inc a2", vec![0x1022]),
            ("inc b2", vec![0x1032]),
            ("dec a2", vec![0x1422]),
            ("dec b2", vec![0x1432]),
            ("not a2", vec![0x03e2]),
            ("not b2", vec![0x03f2]),
            ("trap 0", vec![0x0900]),
            ("trap 31", vec![0x091f]),
            ("rets", vec![0x0960]),
            ("mmtm sp,a0,a2", vec![0x098f, 0xa000]),
            ("mmfm sp,a0,a2", vec![0x09af, 0x0005]),
            ("mmtm sp,b0,b3", vec![0x099f, 0x9000]),
            ("mmfm sp,b0,b3", vec![0x09bf, 0x0009]),
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
        for source in ["trap -1", "trap 32"] {
            let mut assembler = Assembler::new();
            let ast = assembler.pass_one(&[source]).unwrap();
            assert!(assembler.pass_two(&ast).is_err(), "{source}");
        }
        let mut assembler = Assembler::new();
        assembler.object_format = true;
        let source = ".ref outside\nstart\n".to_string() + &"call outside\n".repeat(11) + "dsjs a2,start\nrets";
        let ast = assembler.pass_one(&source.lines().collect::<Vec<_>>()).unwrap();
        let binary = assembler.pass_two(&ast).unwrap();
        assert_eq!(binary.len(), 72);
        assert_eq!(&binary[66..70], &[0x82, 0x0d, 0xdd, 0xff]);
        let object = ObjectFile::from_assembler(Path::new("test.asm"), &assembler, &ast, &binary);
        assert_eq!(object.fixups[0].kind, "absolute32");
        assert_eq!(object.fixups[11].kind, "tms34010-relative16");
        assert_eq!(object.fixups[11].offset, 68);
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
    any_taken: bool,
    else_seen: bool,
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

    fn freeze_expression(&self, expression: &str, volatile: &HashSet<String>) -> String {
        fn rewrite(text: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
            let mut result = String::new();
            let mut chars = text.char_indices().peekable();
            while let Some((start,c)) = chars.next() {
                if c == '\'' {
                    result.push(c);
                    for (_,quoted) in chars.by_ref() {result.push(quoted);if quoted=='\'' {break;}}
                } else if c.is_ascii_alphanumeric() || matches!(c,'_'|'#'|'$') {
                    let mut end=start+c.len_utf8();
                    while let Some((position,c))=chars.peek().copied() {
                        if c.is_ascii_alphanumeric() || matches!(c,'_'|'#'|'$'|'.') {end=position+c.len_utf8();chars.next();} else {break;}
                    }
                    let token=&text[start..end];result.push_str(&lookup(token).unwrap_or_else(||token.to_string()));
                } else {result.push(c);}
            }
            result
        }
        let mut result=expression.to_string();
        for _ in 0..16 {
            let next=rewrite(&result,|name|self.assignment_aliases.get(name).cloned());
            if next==result {break;} result=next;
        }
        rewrite(&result,|name| {
            volatile.contains(&name.to_ascii_uppercase()).then(||format!("({})",self.symbol_table.get(name)
                .or_else(||self.symbol_table.get_ignore_case(name)).copied().unwrap_or(0) as i32))
        })
    }

    fn eval_arithmetic(&self, expression: &str) -> i32 {
        let mut expanded = expression.to_string();
        let (names, values): (Vec<_>, Vec<_>) = self.assignment_aliases.iter().map(|(name, value)| (name.clone(), value.clone())).unzip();
        for _ in 0..16 {
            let next = substitute_macro_parameters(&expanded, &names, &values);
            if next == expanded { break; }
            expanded = next;
        }
        while let Some(start) = expanded.to_ascii_lowercase().find("$isreg(") {
            let argument_start = start + "$isreg(".len();
            let Some(end) = expanded[argument_start..].find(')').map(|offset| argument_start + offset) else { break; };
            let argument = expanded[argument_start..end].trim();
            let register = self.register_aliases.get(argument).map(String::as_str).unwrap_or(argument);
            let valid = parser::parse_register(register).is_ok_and(|(rest, _)| rest.trim().is_empty());
            expanded.replace_range(start..=end, if valid { "1" } else { "0" });
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
            object_format: false,
            register_aliases: HashMap::new(),
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
            if trimmed.starts_with("#*") { continue; }
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
                || conditional_directive == ".ELSEIF"
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
                        any_taken: active,
                        else_seen: false,
                    });
                } else if conditional_directive == ".ELSE" || conditional_directive == ".ELSEIF" {
                    let Some(frame) = conditionals.last_mut() else {
                        return Err(AsmError::Parse { line: line_num, message: format!("unexpected {conditional_directive} without .if") });
                    };
                    if frame.else_seen {
                        return Err(AsmError::Parse { line: line_num, message: format!("unexpected {conditional_directive} after .else") });
                    }
                    let eligible = frame.parent_active && !frame.any_taken;
                    frame.branch_taken = eligible && (conditional_directive == ".ELSE" || {
                        let expression = trimmed.splitn(2, char::is_whitespace).nth(1).unwrap_or("0");
                        self.eval_condition(expression) != 0
                    });
                    frame.any_taken |= frame.branch_taken;
                    frame.else_seen = conditional_directive == ".ELSE";
                } else {
                    if conditionals.pop().is_none() {
                        return Err(AsmError::Parse { line: line_num, message: "unexpected .endif without .if".to_string() });
                    }
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
                            let expression = self.freeze_expression(&words[2..].join(" "),&volatile_symbols);
                            let expression = if expression.contains('$') {
                                let pc_name=format!("__equate_pc_{}",ast.len());
                                let rewritten=expression.split('$').enumerate().map(|(i,part)| {
                                    if i==0 {part.to_string()} else if part.chars().next().is_some_and(|c|c.is_ascii_hexdigit()) {
                                        format!("${part}")
                                    } else {format!("{pc_name}{part}")}
                                }).collect::<String>();
                                if rewritten!=expression {
                                    self.symbol_table.insert(pc_name.clone(),self.current_address);
                                    ast.push(Instruction::Label(pc_name));
                                }
                                rewritten
                            } else {expression};
                            let register = self.register_aliases.get(expression.trim()).cloned().or_else(|| {
                                parser::parse_register(expression.trim()).ok()
                                    .filter(|(rest, _)| rest.is_empty())
                                    .map(|_| expression.trim().to_string())
                            });
                            if let Some(register) = register {
                                self.register_aliases.insert(name, register);
                                continue;
                            }
                            self.register_aliases.remove(&name);
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
                let labeled = if parts.len() > 1 && !parser::is_mnemonic(parts[0]) {
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
                        label_name_to_insert = format!("#{}", first_word.trim_end_matches(':'));
                        is_label = true;
                        trimmed = remainder;
                    }
                }
            } else if let Some(stripped) = trimmed.strip_suffix(':') {
                label_name_to_insert = stripped.trim().to_string();
                is_label = true;
                trimmed = "";
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
                            | "JUMP"
                            | "NOP"
                            | "RETS"
                            | "RETI"
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
                    let is_opcode = is_opcode || isa::format(first_word).is_some() || parser::branch_condition(first_word).is_some() || matches!(upper_first.as_str(),"DSJEQ"|"DSJNE");
                    let is_directive = first_word.starts_with('.');
                    let is_macro = self.macro_table.get(&upper_first).is_some_and(|template| {
                        template.params.is_empty() || parts.len() > 1
                    });

                    if (upper_first == "NOP"
                        && parts.get(1).is_some_and(|word| matches!(word.to_ascii_uppercase().as_str(), "DSJ" | "DSJS")))
                        || (upper_first=="ADD" && parts.get(1).is_some_and(|word|word.eq_ignore_ascii_case("ADDI")))
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
                            | "JUMP"
                                    | "NOP"
                                    | "RETS"
                            | "RETI"
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
                            let second_is_opcode = second_is_opcode || isa::format(second_word).is_some() || parser::branch_condition(second_word).is_some() || matches!(upper_second.as_str(),"DSJEQ"|"DSJNE");
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
                let parts = parser::split_asm_operands(&assignment);
                if parts.len() >= 2 {
                    let alias = parts[1].trim();
                    if !alias.is_empty() {
                        let expression = self.freeze_expression(parts[0].trim(),&volatile_symbols);
                        let value = self.eval_arithmetic(&expression) as u32;
                        self.symbol_table.insert(alias.to_string(), value);
                        self.assignment_aliases.insert(alias.to_string(),expression);
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
                        *arg = self.freeze_expression(arg,&volatile_symbols);
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
                let (names, registers): (Vec<_>, Vec<_>) = self.register_aliases.iter()
                    .map(|(name, register)| (name.clone(), register.clone())).unzip();
                let explicit_local = operands.trim_start().starts_with('#') &&
                    (parser::branch_condition(mnemonic).is_some() || matches!(mnemonic.to_ascii_uppercase().as_str(), "CALL" | "CALLR" | "CALLI"));
                parser_trimmed = format!("{} {}", mnemonic, if explicit_local { operands.to_string() } else { substitute_macro_parameters(operands, &names, &registers) });
            }
            let upper_pt = parser_trimmed.to_uppercase();
            if isa::format(upper_pt.split_whitespace().next().unwrap_or("")).is_none() && (upper_pt.starts_with("MOVI")
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
                Ok((_, mut instruction)) => {
                    let freeze = |operand: &mut Operand| match operand {
                        Operand::Symbol(text)|Operand::DirectMemory(text)|Operand::IndirectOffset(_,text) => {
                            *text=self.freeze_expression(text,&volatile_symbols);
                        }, _ => {},
                    };
                    match &mut instruction {
                        Instruction::Native(native) if !matches!(native.format,isa::Format::Pixblt) => {
                            for operand in &mut native.operands {freeze(operand);}
                        }
                        Instruction::Move {src,dst,field} => {freeze(src);freeze(dst);if let Some(field)=field {freeze(field);}}
                        Instruction::Movb {src,dst} => {freeze(src);freeze(dst);}
                        _ => {},
                    }
                    let target=match &mut instruction {
                        Instruction::Jr {label}|Instruction::Jeq {label}|Instruction::Jne {label} => Some(label),
                        Instruction::Jrcc {target,..}|Instruction::Jacc {target,..} => Some(target),_=>None,
                    };
                    if let Some(target)=target.filter(|t|t.as_str()=="$") {
                        *target=format!("__current_pc_{}",ast.len());
                        ast.push(Instruction::Label(target.clone()));
                    }
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
        if !conditionals.is_empty() {
            return Err(AsmError::Parse { line: processed_lines, message: "missing .endif".to_string() });
        }
        // Preserve explicit hash-local references separately from ordinary
        // symbols, while retaining legacy bare references to unique locals.
        let mut hash_counts = HashMap::<String, usize>::new();
        let mut normal_labels = HashSet::new();
        for instruction in &ast {
            if let Instruction::Label(name) = instruction {
                if name.starts_with('#') { *hash_counts.entry(name.clone()).or_default() += 1; }
                else { normal_labels.insert(name.clone()); }
            }
            if let Instruction::Equ { name, .. } = instruction {
                normal_labels.insert(name.clone());
            }
            if let Instruction::Directive { name, args } = instruction {
                if name.eq_ignore_ascii_case(".bss") && args.len() >= 2 { normal_labels.insert(args[0].clone()); }
            }
        }
        for instruction in &mut ast {
            let target = match instruction {
                Instruction::Jr { label } | Instruction::Jeq { label } | Instruction::Jne { label }
                | Instruction::Call { label } | Instruction::Callr { label } => Some(label),
                Instruction::Jrcc { target, .. } | Instruction::Jacc { target, .. } | Instruction::LongJrcc { target, .. }
                | Instruction::Jlt { target } | Instruction::Jgt { target } | Instruction::Dsjs { target, .. } | Instruction::Dsjcc { target, .. } => Some(target),
                _ => None,
            };
            if let Some(target) = target.filter(|name| name.starts_with('#')) {
                match hash_counts.get(target) {
                    Some(1) => {},
                    Some(_) => return Err(AsmError::Parse { line: 0, message: format!("ambiguous repeated local target {target}; use a unique label") }),
                    None if !normal_labels.contains(target) => *target = target.trim_start_matches('#').to_string(),
                    None => {},
                }
            }
        }
        let mut with_aliases = Vec::with_capacity(ast.len() + hash_counts.len());
        for instruction in ast {
            if let Instruction::Label(name) = &instruction {
                if let Some(bare) = name.strip_prefix('#') {
                    if !normal_labels.contains(bare) {
                        self.symbol_table.insert(bare.to_string(), self.symbol_table.get(name).copied().unwrap_or(0));
                        with_aliases.push(Instruction::Label(bare.to_string()));
                    }
                }
            }
            with_aliases.push(instruction);
        }
        let mut ast = with_aliases;
        // the two-word form move later labels, so repeat until addresses settle.
        // Reserve external transfers before layout as well as during object
        // serialization; otherwise nearby DSJS/branches see incorrect distances.
        if self.object_format {
            let transfers: Vec<_> = ast.iter().map(|instruction| {
                external_absolute_transfer(instruction, &ast).map(|(opcode, target)| (opcode, target.to_string()))
            }).collect();
            for (instruction, transfer) in ast.iter_mut().zip(transfers) {
                if let Some((opcode, target)) = transfer {
                    *instruction = if opcode == 0x0D5F {
                        Instruction::Calla { target: Operand::Symbol(target) }
                    } else {
                        Instruction::Jacc { condition: ((opcode >> 8) & 15) as u8, target }
                    };
                }
            }
        }
        let mut latest_equates = HashMap::<String, String>::new();
        for (name, expression) in pending_equates.drain(..) {
            if volatile_symbols.contains(&name.to_ascii_uppercase()) {continue;}
            latest_equates.insert(name, expression);
        }
        let mut pending_equates: Vec<(String, String)> = latest_equates.into_iter().collect();
        pending_equates.sort_by(|left, right| left.0.cmp(&right.0));
        let layout_names: Vec<String> = ast.iter().filter_map(|instruction| {
            if let Instruction::Label(name) = instruction { Some(name.clone()) } else { None }
        }).collect();
        let layout_set:HashSet<_>=layout_names.iter().cloned().collect();
        let branch_count = ast.iter().filter(|instruction| matches!(
            instruction,
            Instruction::Jr { .. } | Instruction::Jrcc { .. } | Instruction::Jeq { .. }
                | Instruction::Jne { .. } | Instruction::Jlt { .. } | Instruction::Jgt { .. } | Instruction::Dsjs { .. }
        )).count();
        let max_layout_passes = branch_count + 3;
        let mut layout_converged = false;
        for _ in 0..max_layout_passes {
            let previous_symbols: Vec<Option<u32>> = layout_names.iter().map(|name| {
                self.symbol_table.get(name).copied()
            }).collect();
            let mut address = 0u32;
            for instruction in &mut ast {
                if let Instruction::Dsjs { target, short, .. } = instruction {
                    if *short && target != "$" {
                        if let Some(target_address) = self.symbol_table.get(target).or_else(|| self.symbol_table.get_ignore_case(target)) {
                            let delta = *target_address as i64 - (address + 2) as i64;
                            if !(-31..=31).contains(&(delta / 2)) { *short = false; }
                        }
                    }
                }
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
                    .map(|(key, value)| (key.to_ascii_uppercase(), if layout_set.contains(key) {value.wrapping_mul(8)} else {*value})).collect();
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
                .map(|(key, value)| (key.to_ascii_uppercase(), if layout_set.contains(key) {value.wrapping_mul(8)} else {*value})).collect();
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
    assembler.object_format = args.object_format;
    println!("Executing Pass 1 (Parsing & Symbol Resolution)...");

    // Convert Vec<String> into a vector of &str references
    let line_refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();

    // Now pass the &str slice to pass_one
    let ast = assembler.pass_one(&line_refs)?;
    let omissions = omitted_instructions(&ast);
    if !omissions.is_empty() {
        let summary = omissions.iter().map(|item| format!("{} ({})", item.mnemonic, item.count)).collect::<Vec<_>>().join(", ");
        if args.deny_omitted_instructions {
            return Err(AsmError::Parse { line: 0, message: format!("instructions emit no code: {summary}") });
        }
        eprintln!("WARNING: instructions emit no code: {summary}");
    }

    if args.verbose {
        for omission in &omissions {
            eprintln!("Omitted {}: {}",omission.mnemonic,omission.examples.join(" | "));
        }
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

#[cfg(test)]
mod isa_tests;

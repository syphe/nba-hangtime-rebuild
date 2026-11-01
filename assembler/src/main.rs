use clap::Parser;
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use thiserror::Error;

use std::fs;

mod dump;
mod encoder;
mod interleaver;
mod parser;
use parser::{Operand, ParsedInstruction, MacroTemplate};

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

impl SymbolTable {
    pub fn insert(&mut self, name: String, addr: u32) {
        self.symbols.insert(name, addr);
    }

    pub fn get(&self, name: &str) -> Option<&u32> {
        self.symbols.get(name)
    }

    pub fn get_ignore_case(&self, name: &str) -> Option<&u32> {
        self.symbols.iter()
            .find(|(symbol, _)| symbol.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    }
}

pub type Instruction = parser::ParsedInstruction;

pub struct Assembler {
    pub symbol_table: SymbolTable,
    pub macro_table: HashMap<String, MacroTemplate>,
    current_address: u32,
}

impl Assembler {
    pub fn new() -> Self {
        Self {
            symbol_table: SymbolTable::default(),
            macro_table: HashMap::new(),
            current_address: 0,
        }
    }

    /// Pass 1: Parse AST via nom, collect labels, and assign memory locations
    pub fn pass_one(&mut self, lines: &[&str]) -> Result<Vec<Instruction>, AsmError> {
        let mut ast = Vec::new();
        let mut current_macro: Option<MacroTemplate> = None;

        let mut line_queue: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
        let mut idx = 0;

        self.macro_table.entry("JAUC".to_string()).or_insert(MacroTemplate {
            name: "JAUC".to_string(),
            params: vec!["P1".to_string()],
            body: vec!["jmp\t:P1:".to_string()],
        });

        while idx < line_queue.len() {
            let raw_line = line_queue[idx].clone();
            idx += 1;

            let clean_line = parser::sanitize_line(&raw_line);
            let mut trimmed = clean_line.trim();
            let upper_trimmed = trimmed.to_uppercase();

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
            if let Ok((_, ParsedInstruction::MacroDef { name, params })) = parser::parse_line(trimmed) {
                if let Some(prev_macro) = current_macro.take() {
                    println!("[DEBUG] Auto-closing unended macro: {}", prev_macro.name);
                    self.macro_table.insert(prev_macro.name.to_uppercase(), prev_macro);
                }
                current_macro = Some(MacroTemplate {
                    name: name.to_uppercase(),
                    params,
                    body: Vec::new(),
                });
                continue;
            }

            // 2. If we are currently recording a macro definition body
            if let Some(ref mut macro_def) = current_macro {
                if trimmed.eq_ignore_ascii_case(".endm") {
                    self.macro_table.insert(macro_def.name.to_uppercase(), macro_def.clone());
                    current_macro = None;
                } else {
                    macro_def.body.push(raw_line);
                }
                continue;
            }

            // 3. Skip empty lines and comments
            if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('*') {
                continue;
            }

            let line_num = idx; // approximate

            // 4. Check for .equ Equates FIRST (before labels)
            if upper_trimmed.contains(".EQU") {
                match parser::parse_equ(trimmed) {
                    Ok((_, Instruction::Equ { name, value })) => {
                        self.symbol_table.insert(name.clone(), value as u32);
                        ast.push(Instruction::Equ { name, value });
                        continue;
                    }
                    _ => {
                        return Err(AsmError::UnknownOpcode(
                            format!("Invalid .equ directive syntax: {}", trimmed),
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
                let potential_macro_name = parts[0].to_uppercase();
                if let Some(macro_template) = self.macro_table.get(&potential_macro_name).cloned() {
                    let arg_str = trimmed[parts[0].len()..].trim();
                    let args: Vec<String> = arg_str
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();

                    let mut expanded_lines = Vec::new();
                    for body_line in &macro_template.body {
                        let mut expanded = body_line.clone();
                        for (p_idx, param_name) in macro_template.params.iter().enumerate() {
                            if let Some(arg_val) = args.get(p_idx) {
                                expanded = expanded.replace(&format!(":{}:", param_name), arg_val);
                                expanded = expanded.replace(param_name, arg_val);
                            }
                        }
                        expanded_lines.push(expanded);
                    }

                    line_queue.splice(idx..idx, expanded_lines);
                    continue;
                }
            }

            // 7. Check for Labels
            let mut is_label = false;
            let mut label_name_to_insert = String::new();

            if let Some(stripped) = trimmed.strip_suffix(':') {
                label_name_to_insert = stripped.trim().to_string();
                is_label = true;
            } else {
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if !parts.is_empty() {
                    let first_word = parts[0];
                    let upper_first = first_word.to_uppercase();
                    
                    let is_opcode = matches!(upper_first.as_str(),
                        "MOVE" | "MOVB" | "INC" | "DEC" | "MOVK" | "MOVX" | "MOVY" | "RL" | "RR" | "ADDI" | "ADDK" | "SUB" | "SUBK" |
                        "CMP" | "CMPI" | "JMP" | "NOP" | "RETS" | "SLL" | "SRL" | "SLA" | "SRA" |
                        "AND" | "ANDN" | "ANDI" | "OR" | "ORI" | "XOR" | "XORI" | "MOVI" | "JR" | "JRUC" | "JREQ" | "JRNE" | "JRNZ" | "JRZ" | "JRC" | "JRNC" | "JRLT" | "JRGT" | "JRHS" | "JRLO" |
                        "JRZ" | "JRN" | "JRNZ" | "JEQ" | "JNE" | "JLT" | "JGT" | "CALL" | "CALLA" |
                        "CALLR" | "CALLI" | "BTST" | "CLR" | "PIXT" | "FILL" | "NOT" | "DSJ" | "DSJS" | "SETF" | "MMFM" | "EXGPC" | "TRAP");
                    let is_directive = first_word.starts_with('.');
                    let is_macro = self.macro_table.contains_key(&upper_first);

                    if !is_opcode && !is_directive && !is_macro && first_word.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        if parts.len() == 1 {
                            label_name_to_insert = first_word.to_string();
                            is_label = true;
                            trimmed = &trimmed[first_word.len()..].trim();
                        } else {
                            let second_word = parts[1];
                            let upper_second = second_word.to_uppercase();
                            let second_is_opcode = matches!(upper_second.as_str(),
                                "MOVE" | "MOVB" | "INC" | "DEC" | "MOVK" | "MOVX" | "MOVY" | "RL" | "RR" | "ADDI" | "ADDK" | "SUB" | "SUBK" |
                                "CMP" | "CMPI" | "JMP" | "NOP" | "RETS" | "SLL" | "SRL" | "SLA" | "SRA" |
                                "AND" | "ANDN" | "ANDI" | "OR" | "ORI" | "XOR" | "XORI" | "MOVI" | "JR" | "JRUC" | "JREQ" | "JRNE" | "JRNZ" | "JRZ" | "JRC" | "JRNC" | "JRLT" | "JRGT" | "JRHS" | "JRLO" |
                                "JRZ" | "JRN" | "JRNZ" | "JEQ" | "JNE" | "JLT" | "JGT" | "CALL" | "CALLA" |
                                "CALLR" | "CALLI" | "BTST" | "CLR" | "PIXT" | "FILL" | "NOT" | "DSJ" | "DSJS" | "SETF" | "MMFM" | "EXGPC" | "TRAP");
                            if second_word.starts_with('.') || second_is_opcode || self.macro_table.contains_key(&upper_second) {
                                label_name_to_insert = first_word.to_string();
                                is_label = true;
                                trimmed = trimmed[first_word.len()..].trim();
                            }
                        }
                    }
                }
            }

            if is_label {
                self.symbol_table.insert(label_name_to_insert.clone(), self.current_address);
                ast.push(Instruction::Label(label_name_to_insert));
                if trimmed.is_empty() {
                    continue;
                }
            }

            // Assignment directives may omit the leading dot in some source
            // variants and can be spaced around their comma.
            let upper_assignment = trimmed.to_uppercase();
            if upper_assignment.starts_with(".ASG") || upper_assignment.starts_with("ASG ") {
                let assignment = trimmed.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
                let parts: Vec<&str> = assignment.split(',').collect();
                if parts.len() >= 2 {
                    let alias = parts[1].trim();
                    if !alias.is_empty() {
                        self.symbol_table.insert(alias.to_string(), 0);
                    }
                }
                ast.push(Instruction::Directive { name: parts.first().unwrap_or(&".ASG").to_string(), args: parts.iter().skip(1).map(|s| s.trim().to_string()).collect() });
                continue;
            }

            // 8. Check for Directives (e.g. ".WORD")
            if trimmed.starts_with('.') {
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                let name = parts[0].to_string();
                let args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
                
                if name.eq_ignore_ascii_case(".WORD") {
                    self.current_address += (args.len() * 2) as u32;
                }
                if name.eq_ignore_ascii_case(".asg") || name.eq_ignore_ascii_case(".eval") {
                    if let Some(arg) = args.first() {
                        let parts: Vec<&str> = arg.split(',').collect();
                        if parts.len() >= 2 {
                            let alias = parts[1].trim();
                            if !alias.is_empty() {
                                if self.symbol_table.get(alias).is_none() {
                                    self.symbol_table.insert(alias.to_string(), 0);
                                }
                            }
                        }
                    }
                }
                if name.eq_ignore_ascii_case(".globl") || name.eq_ignore_ascii_case(".global") || name.eq_ignore_ascii_case(".ref") {
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
                        args 
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

                    ast.push(Instruction::Directive { 
                        name, 
                        args 
                    });
                    continue;
                }
            }

            // 9. Parse Instructions with move/movi/jrz/jrn translation and size suffix stripping
            let mut parser_trimmed = trimmed.to_string();
            let upper_pt = parser_trimmed.to_uppercase();
            if (upper_pt.starts_with("MOVE") || upper_pt.starts_with("MOVI") || upper_pt.starts_with("ADD") || upper_pt.starts_with("SUB")) 
                && (parser_trimmed.ends_with(",L") || parser_trimmed.ends_with(",W") || parser_trimmed.ends_with(",l") || parser_trimmed.ends_with(",w")) 
            {
                parser_trimmed = parser_trimmed[..parser_trimmed.len() - 2].trim().to_string();
            }

            let upper_pt_2 = parser_trimmed.to_uppercase();
            if upper_pt_2.starts_with("MOVI") {
                let remainder = parser_trimmed[4..].trim().trim_start_matches('#').trim();
                parser_trimmed = format!("move\t#{}", remainder);
            } else if upper_pt_2.starts_with("JRZ") {
                let remainder = parser_trimmed[3..].trim();
                parser_trimmed = format!("jeq\t{}", remainder);
            } else if upper_pt_2.starts_with("JRN") {
                let remainder = parser_trimmed[3..].trim();
                parser_trimmed = format!("jlt\t{}", remainder);
            }

            match parser::parse_line(&parser_trimmed) {
                Ok((_, instruction)) => {
                    match &instruction {
                        Instruction::Move { src, dst, field } => {
                            let mut extra_words = 0;
                            if matches!(src, Operand::Immediate(_) | Operand::Symbol(_)) {
                                extra_words += 1;
                            }
                            if matches!(src, Operand::DirectMemory(_) | Operand::IndirectOffset { .. }) {
                                extra_words += 1;
                            }
                            if matches!(dst, Operand::DirectMemory(_) | Operand::IndirectOffset { .. }) {
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
                            if matches!(src, Operand::DirectMemory(_) | Operand::IndirectOffset { .. }) {
                                extra_words += 1;
                            }
                            if matches!(dst, Operand::DirectMemory(_) | Operand::IndirectOffset { .. }) {
                                extra_words += 1;
                            }
                            self.current_address += 2 + (extra_words * 2);
                        }
                        Instruction::Cmp { src, .. } => {
                            let is_immediate = matches!(src, Operand::Immediate(_) | Operand::Symbol(_));
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
                        line_num
                    ));
                }
            }
        }

        Ok(ast)
    }

    pub fn pass_two(&self, ast: &[Instruction]) -> Result<Vec<u8>, AsmError> {
        encoder::generate_binary(ast, &self.symbol_table)
            .map_err(|e| AsmError::Parse { 
                line: 0, 
                message: format!("Encoding/Symbol error: {:?}", e) 
            })
    }
}

/// Recursively loads a source file and expands any .include directives
pub fn load_source_file(path: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let content = fs::read_to_string(path)?;
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
    let start = line.find('"')?;
    let end = line.rfind('"')?;
    if start < end {
        Some(line[start + 1..end].to_string())
    } else {
        None
    }
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

    // Write primary binary output to disk
    std::fs::write(&args.output, &binary_output)?;
    println!(
        "Successfully assembled {} bytes -> {:?}",
        binary_output.len(),
        args.output
    );

    // Conditionally apply ROM interleaver based on CLI argument
    if args.interleave_ways > 1 {
        println!("Splitting binary for arcade hardware ({}-way interleave)...", args.interleave_ways);
        interleaver::write_split_roms(&args.output, &binary_output, args.interleave_ways)
            .map_err(|e| AsmError::Parse { line: 0, message: format!("Interleave error: {:?}", e) })?;
    } else {
        println!("Interleaving skipped (1-way mode).");
    }

    // Word-level hex dump + little-endian verification against the symbol table
    if args.dump {
        let listing = dump::dump_listing(&ast, &binary_output, &assembler.symbol_table)
            .map_err(|e| AsmError::Parse { line: 0, message: format!("Encode error: {:?}", e) })?;
        print!("{}", listing);

        let checks = dump::verify(&ast, &binary_output, &assembler.symbol_table)
            .map_err(|e| AsmError::Parse { line: 0, message: format!("Encode error: {:?}", e) })?;

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

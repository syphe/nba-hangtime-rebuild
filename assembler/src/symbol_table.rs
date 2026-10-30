use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct SymbolTable {
    pub symbols: HashMap<String, u32>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            symbols: HashMap::new(),
        }
    }

    /// Insert a symbol (label address or .equ constant value)
    pub fn insert(&mut self, name: String, value: u32) -> Result<(), String> {
        // Optional: you can check for duplicate definitions here if desired
        self.symbols.insert(name, value);
        Ok(())
    }

    /// Look up a symbol value
    pub fn get(&self, name: &str) -> Option<&u32> {
        self.symbols.get(name)
    }
}
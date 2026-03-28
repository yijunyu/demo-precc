//! Cross-file dependency analysis module
//!
//! This module provides cross-file optimization by analyzing dependencies
//! across multiple source files in a project. It enables:
//! - Global type header generation (shared declarations)
//! - Smart incremental rebuilds (only rebuild affected PUs)
//! - Parallel build graph optimization
//! - Dependency manifest output for build systems

use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, BufWriter};
use std::path::Path;
use std::time::Instant;

/// Extract C identifiers from code (simple tokenizer)
/// Returns a set of unique identifiers found in the code
fn extract_c_identifiers(code: &str) -> FxHashSet<String> {
    let mut identifiers = FxHashSet::default();
    let mut current_ident = String::new();
    let mut in_string = false;
    let mut in_char = false;
    let mut in_comment = false;
    let mut in_line_comment = false;
    let mut prev_char = '\0';

    for ch in code.chars() {
        // Handle comments and strings
        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
            }
            prev_char = ch;
            continue;
        }
        if in_comment {
            if prev_char == '*' && ch == '/' {
                in_comment = false;
            }
            prev_char = ch;
            continue;
        }
        if prev_char == '/' && ch == '/' {
            in_line_comment = true;
            prev_char = ch;
            continue;
        }
        if prev_char == '/' && ch == '*' {
            in_comment = true;
            prev_char = ch;
            continue;
        }

        // Handle string/char literals
        if ch == '"' && prev_char != '\\' && !in_char {
            in_string = !in_string;
            prev_char = ch;
            continue;
        }
        if ch == '\'' && prev_char != '\\' && !in_string {
            in_char = !in_char;
            prev_char = ch;
            continue;
        }
        if in_string || in_char {
            prev_char = ch;
            continue;
        }

        // Extract identifiers
        if ch.is_alphanumeric() || ch == '_' {
            current_ident.push(ch);
        } else {
            if !current_ident.is_empty() && !current_ident.chars().next().unwrap().is_numeric() {
                identifiers.insert(current_ident.clone());
            }
            current_ident.clear();
        }
        prev_char = ch;
    }

    // Don't forget the last identifier
    if !current_ident.is_empty() && !current_ident.chars().next().unwrap().is_numeric() {
        identifiers.insert(current_ident);
    }

    identifiers
}

/// Remove preprocessor directives (lines starting with #) from code
fn clean_preprocessor_directives(code: &str) -> String {
    code.lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with('#')
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Topological sort of types based on their dependencies
/// Returns types in dependency order (types with no deps first)
fn topological_sort_types(deps: &FxHashMap<String, FxHashSet<String>>) -> Vec<String> {
    let mut result = Vec::new();
    let mut visited: FxHashSet<String> = FxHashSet::default();
    let mut temp_visited: FxHashSet<String> = FxHashSet::default();

    fn visit(
        node: &str,
        deps: &FxHashMap<String, FxHashSet<String>>,
        visited: &mut FxHashSet<String>,
        temp_visited: &mut FxHashSet<String>,
        result: &mut Vec<String>,
    ) {
        if visited.contains(node) {
            return;
        }
        if temp_visited.contains(node) {
            return; // Cycle detected, skip
        }

        temp_visited.insert(node.to_string());

        // Visit dependencies first
        if let Some(node_deps) = deps.get(node) {
            for dep in node_deps {
                visit(dep, deps, visited, temp_visited, result);
            }
        }

        temp_visited.remove(node);
        visited.insert(node.to_string());
        result.push(node.to_string());
    }

    // Sort keys for deterministic output
    let mut keys: Vec<_> = deps.keys().collect();
    keys.sort();

    for key in keys {
        visit(key, deps, &mut visited, &mut temp_visited, &mut result);
    }

    result
}

/// Represents a symbol (function, variable, type) across the project
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    /// Symbol name
    pub name: String,
    /// Symbol type (function, variable, typedef, struct, enum, union)
    pub kind: SymbolKind,
    /// Source file where this symbol is defined
    pub defined_in: String,
    /// Full code/declaration of the symbol
    pub code: String,
    /// Is this symbol static (file-local)?
    pub is_static: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    Variable,
    Typedef,
    Struct,
    Enum,
    Union,
    Prototype,
    ExternVar,
}

impl SymbolKind {
    pub fn from_key_prefix(key: &str) -> Option<Self> {
        match key.as_bytes().first()? {
            b'f' if key.starts_with("function:") => Some(SymbolKind::Function),
            b'v' if key.starts_with("variable:") => Some(SymbolKind::Variable),
            b't' if key.starts_with("typedef:") => Some(SymbolKind::Typedef),
            b's' if key.starts_with("struct:") => Some(SymbolKind::Struct),
            b'e' if key.starts_with("enum:") => Some(SymbolKind::Enum),
            b'u' if key.starts_with("union:") => Some(SymbolKind::Union),
            b'p' if key.starts_with("prototype:") => Some(SymbolKind::Prototype),
            b'e' if key.starts_with("extern_var:") => Some(SymbolKind::ExternVar),
            _ => None,
        }
    }
}

/// Cross-file dependency information for a project
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossFileDeps {
    /// All symbols in the project, indexed by qualified name (kind:name:file)
    pub symbols: FxHashMap<String, Symbol>,

    /// Cross-file call graph: caller_file -> function -> set of (callee_file, function)
    /// Only includes cross-file calls (same-file calls are handled by per-file deps)
    pub cross_file_calls: FxHashMap<String, FxHashMap<String, FxHashSet<(String, String)>>>,

    /// Type definitions: type_name -> defining_file
    /// For types used across multiple files
    pub type_definitions: FxHashMap<String, String>,

    /// Types used by multiple files (candidates for common header)
    pub common_types: FxHashSet<String>,

    /// Files in dependency order (topologically sorted)
    pub build_order: Vec<String>,

    /// For each file, which other files it depends on
    pub file_dependencies: FxHashMap<String, FxHashSet<String>>,

    /// Reverse dependencies: for each file, which files depend on it
    pub reverse_dependencies: FxHashMap<String, FxHashSet<String>>,

    /// Statistics
    pub stats: CrossFileStats,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CrossFileStats {
    pub total_files: usize,
    pub total_symbols: usize,
    pub total_functions: usize,
    pub total_types: usize,
    pub cross_file_calls: usize,
    pub common_type_count: usize,
    pub analysis_time_ms: u64,
}

/// Per-file analysis result that can be merged into CrossFileDeps
#[derive(Debug, Clone)]
pub struct FileAnalysis {
    /// Source filename
    pub filename: String,
    /// Symbols defined in this file
    pub defined_symbols: FxHashMap<String, Symbol>,
    /// Symbols referenced but not defined in this file
    pub external_refs: FxHashSet<String>,
    /// Direct dependencies (function calls, type uses)
    pub dependencies: FxHashMap<String, FxHashSet<String>>,
}

impl CrossFileDeps {
    /// Create a new empty CrossFileDeps
    pub fn new() -> Self {
        Self {
            symbols: FxHashMap::default(),
            cross_file_calls: FxHashMap::default(),
            type_definitions: FxHashMap::default(),
            common_types: FxHashSet::default(),
            build_order: Vec::new(),
            file_dependencies: FxHashMap::default(),
            reverse_dependencies: FxHashMap::default(),
            stats: CrossFileStats::default(),
        }
    }

    /// Analyze multiple files and build cross-file dependency graph
    pub fn analyze_files(
        file_analyses: Vec<FileAnalysis>,
    ) -> Self {
        let start = Instant::now();
        let mut deps = Self::new();

        deps.stats.total_files = file_analyses.len();

        // Phase 1: Collect all symbols and type definitions
        for analysis in &file_analyses {
            for (key, symbol) in &analysis.defined_symbols {
                deps.symbols.insert(key.clone(), symbol.clone());

                // Track type definitions
                match symbol.kind {
                    SymbolKind::Typedef | SymbolKind::Struct |
                    SymbolKind::Enum | SymbolKind::Union => {
                        deps.type_definitions.insert(symbol.name.clone(), analysis.filename.clone());
                        deps.stats.total_types += 1;
                    }
                    SymbolKind::Function | SymbolKind::Prototype => {
                        deps.stats.total_functions += 1;
                    }
                    _ => {}
                }
                deps.stats.total_symbols += 1;
            }
        }

        // Phase 2: Build cross-file call graph by scanning function code
        // First, build a map of function name -> defining file (only actual definitions, not prototypes)
        let mut func_definitions: FxHashMap<String, String> = FxHashMap::default();
        for (_key, symbol) in &deps.symbols {
            if symbol.kind == SymbolKind::Function && !symbol.is_static {
                func_definitions.insert(symbol.name.clone(), symbol.defined_in.clone());
            }
        }

        // Debug output
        if std::env::var("DEBUG_CROSSFILE").is_ok() {
            eprintln!("DEBUG: Found {} non-static function definitions", func_definitions.len());
            for (name, file) in func_definitions.iter().take(5) {
                eprintln!("  {} -> {}", name, file);
            }
        }

        // Now scan each function's code for calls to functions defined in other files
        for (_key, symbol) in &deps.symbols {
            if symbol.kind != SymbolKind::Function {
                continue;
            }
            let caller_file = &symbol.defined_in;
            let caller_name = &symbol.name;

            // Extract identifiers from the function code and check if they're function calls
            let identifiers = extract_c_identifiers(&symbol.code);

            for ident in identifiers {
                // Check if this identifier is a function defined in another file
                if let Some(callee_file) = func_definitions.get(&ident) {
                    if callee_file != caller_file {
                        // Cross-file call detected!
                        deps.cross_file_calls
                            .entry(caller_file.clone())
                            .or_default()
                            .entry(caller_name.clone())
                            .or_default()
                            .insert((callee_file.clone(), ident.clone()));

                        // Track file-level dependencies
                        deps.file_dependencies
                            .entry(caller_file.clone())
                            .or_default()
                            .insert(callee_file.clone());

                        // Track reverse dependencies
                        deps.reverse_dependencies
                            .entry(callee_file.clone())
                            .or_default()
                            .insert(caller_file.clone());

                        deps.stats.cross_file_calls += 1;
                    }
                }
            }
        }

        // Phase 3: Identify common types (defined in or used by 2+ files)
        // Track which files define each type (to detect duplicate definitions from shared headers)
        let mut type_defined_in_files: FxHashMap<String, FxHashSet<String>> = FxHashMap::default();
        // Track which files use each type (from function/variable code)
        let mut type_usage_count: FxHashMap<String, FxHashSet<String>> = FxHashMap::default();

        // Count type definitions per file (types from shared headers appear in multiple files)
        for (_key, symbol) in &deps.symbols {
            match symbol.kind {
                SymbolKind::Typedef | SymbolKind::Struct |
                SymbolKind::Enum | SymbolKind::Union => {
                    type_defined_in_files
                        .entry(symbol.name.clone())
                        .or_default()
                        .insert(symbol.defined_in.clone());
                }
                _ => {}
            }
        }

        // Count type usage in function/variable code
        for (_key, symbol) in &deps.symbols {
            if !matches!(symbol.kind, SymbolKind::Function | SymbolKind::Variable |
                         SymbolKind::Prototype | SymbolKind::ExternVar) {
                continue;
            }
            let identifiers = extract_c_identifiers(&symbol.code);
            for ident in identifiers {
                // Check if this identifier is a type
                if type_defined_in_files.contains_key(&ident) {
                    type_usage_count
                        .entry(ident)
                        .or_default()
                        .insert(symbol.defined_in.clone());
                }
            }
        }

        // Types defined in 2+ files OR used by 2+ files are common types
        for (type_name, files) in &type_defined_in_files {
            if files.len() >= 2 {
                deps.common_types.insert(type_name.clone());
            }
        }
        for (type_name, files) in &type_usage_count {
            if files.len() >= 2 {
                deps.common_types.insert(type_name.clone());
            }
        }
        deps.stats.common_type_count = deps.common_types.len();

        // Phase 4: Compute topological build order
        deps.build_order = deps.compute_build_order();

        deps.stats.analysis_time_ms = start.elapsed().as_millis() as u64;
        deps
    }

    /// Compute topological sort of files based on dependencies
    fn compute_build_order(&self) -> Vec<String> {
        let mut in_degree: FxHashMap<String, usize> = FxHashMap::default();
        let mut all_files: FxHashSet<String> = FxHashSet::default();

        // Collect all files
        for (file, deps) in &self.file_dependencies {
            all_files.insert(file.clone());
            for dep in deps {
                all_files.insert(dep.clone());
            }
        }

        // Initialize in-degrees
        for file in &all_files {
            in_degree.insert(file.clone(), 0);
        }

        // Count incoming edges
        for (_, deps) in &self.file_dependencies {
            for dep in deps {
                *in_degree.get_mut(dep).unwrap() += 1;
            }
        }

        // Kahn's algorithm
        let mut queue: Vec<String> = in_degree.iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(f, _)| f.clone())
            .collect();
        queue.sort(); // Deterministic ordering

        let mut result = Vec::new();
        while let Some(file) = queue.pop() {
            result.push(file.clone());

            if let Some(deps) = self.file_dependencies.get(&file) {
                for dep in deps {
                    let deg = in_degree.get_mut(dep).unwrap();
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push(dep.clone());
                        queue.sort();
                    }
                }
            }
        }

        // Reverse to get dependency order (dependencies first)
        result.reverse();
        result
    }

    /// Get files that need recompilation when a given file changes
    pub fn get_affected_files(&self, changed_file: &str) -> FxHashSet<String> {
        let mut affected = FxHashSet::default();
        let mut queue = vec![changed_file.to_string()];

        while let Some(file) = queue.pop() {
            if affected.contains(&file) {
                continue;
            }
            affected.insert(file.clone());

            // Add all files that depend on this file
            if let Some(dependents) = self.reverse_dependencies.get(&file) {
                for dep in dependents {
                    if !affected.contains(dep) {
                        queue.push(dep.clone());
                    }
                }
            }
        }

        affected
    }

    /// Generate common header content with shared type declarations
    pub fn generate_common_header(&self) -> String {
        let mut header = String::new();
        header.push_str("/* Auto-generated common header for shared type declarations */\n");
        header.push_str("#ifndef _PRECC_COMMON_H\n");
        header.push_str("#define _PRECC_COMMON_H\n\n");

        // Build dependency graph for types
        let mut type_deps: FxHashMap<String, FxHashSet<String>> = FxHashMap::default();
        let mut type_code: FxHashMap<String, String> = FxHashMap::default();
        let mut type_file: FxHashMap<String, String> = FxHashMap::default();

        for type_name in &self.common_types {
            if let Some(defining_file) = self.type_definitions.get(type_name) {
                for (_key, symbol) in &self.symbols {
                    if symbol.name == *type_name && symbol.defined_in == *defining_file {
                        // Clean code: remove preprocessor directives
                        let clean_code = clean_preprocessor_directives(&symbol.code);
                        type_code.insert(type_name.clone(), clean_code.clone());
                        type_file.insert(type_name.clone(), defining_file.clone());

                        // Find type dependencies in the code
                        let idents = extract_c_identifiers(&clean_code);
                        let deps: FxHashSet<String> = idents.into_iter()
                            .filter(|id| self.common_types.contains(id) && id != type_name)
                            .collect();
                        type_deps.insert(type_name.clone(), deps);
                        break;
                    }
                }
            }
        }

        // Topological sort of types by dependencies
        let sorted_types = topological_sort_types(&type_deps);

        for type_name in sorted_types {
            if let Some(code) = type_code.get(&type_name) {
                if let Some(file) = type_file.get(&type_name) {
                    header.push_str(&format!("/* From {} */\n", file));
                }
                header.push_str(code.trim());
                header.push_str("\n\n");
            }
        }

        header.push_str("#endif /* _PRECC_COMMON_H */\n");
        header
    }

    /// Export dependency graph as JSON
    pub fn to_json(&self) -> io::Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    /// Export dependency graph as JSON to a file
    pub fn write_json(&self, path: &Path) -> io::Result<()> {
        let file = File::create(path)?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, self)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    /// Load dependency graph from JSON file
    pub fn from_json_file(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        serde_json::from_reader(file)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
    }

    /// Print summary statistics
    pub fn print_summary(&self) {
        eprintln!("=== Cross-File Dependency Analysis ===");
        eprintln!("  Files analyzed:       {}", self.stats.total_files);
        eprintln!("  Total symbols:        {}", self.stats.total_symbols);
        eprintln!("  Functions:            {}", self.stats.total_functions);
        eprintln!("  Types:                {}", self.stats.total_types);
        eprintln!("  Cross-file calls:     {}", self.stats.cross_file_calls);
        eprintln!("  Common types:         {}", self.stats.common_type_count);
        eprintln!("  Analysis time:        {}ms", self.stats.analysis_time_ms);

        if !self.build_order.is_empty() {
            eprintln!("\n  Build order ({} files):", self.build_order.len());
            for (i, file) in self.build_order.iter().take(10).enumerate() {
                eprintln!("    {}. {}", i + 1, file);
            }
            if self.build_order.len() > 10 {
                eprintln!("    ... and {} more", self.build_order.len() - 10);
            }
        }
    }
}

impl Default for CrossFileDeps {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for FileAnalysis from precc's internal structures
pub struct FileAnalysisBuilder {
    filename: String,
    defined_symbols: FxHashMap<String, Symbol>,
    external_refs: FxHashSet<String>,
    dependencies: FxHashMap<String, FxHashSet<String>>,
}

impl FileAnalysisBuilder {
    pub fn new(filename: &str) -> Self {
        Self {
            filename: filename.to_string(),
            defined_symbols: FxHashMap::default(),
            external_refs: FxHashSet::default(),
            dependencies: FxHashMap::default(),
        }
    }

    /// Add a symbol from a PU key and its code
    pub fn add_symbol(&mut self, key: &str, code: &str) {
        let parts: Vec<&str> = key.splitn(3, ':').collect();
        if parts.len() < 2 {
            return;
        }

        let kind = match parts[0] {
            "function" => SymbolKind::Function,
            "variable" => SymbolKind::Variable,
            "typedef" => SymbolKind::Typedef,
            "struct" => SymbolKind::Struct,
            "enum" => SymbolKind::Enum,
            "union" => SymbolKind::Union,
            "prototype" => SymbolKind::Prototype,
            "extern_var" => SymbolKind::ExternVar,
            _ => return,
        };

        let name = parts[1].to_string();
        let is_static = code.contains("static ");

        self.defined_symbols.insert(key.to_string(), Symbol {
            name: name.clone(),
            kind,
            defined_in: self.filename.clone(),
            code: code.to_string(),
            is_static,
        });
    }

    /// Add a dependency (function in this file calls another function)
    pub fn add_dependency(&mut self, caller: &str, callee: &str) {
        self.dependencies
            .entry(caller.to_string())
            .or_default()
            .insert(callee.to_string());
    }

    /// Add an external reference (symbol used but not defined in this file)
    pub fn add_external_ref(&mut self, name: &str) {
        self.external_refs.insert(name.to_string());
    }

    /// Build the FileAnalysis
    pub fn build(self) -> FileAnalysis {
        FileAnalysis {
            filename: self.filename,
            defined_symbols: self.defined_symbols,
            external_refs: self.external_refs,
            dependencies: self.dependencies,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cross_file_deps_basic() {
        // Create mock file analyses
        let mut builder1 = FileAnalysisBuilder::new("file1.c");
        builder1.add_symbol("function:foo:file1.c", "void foo() {}");
        builder1.add_symbol("typedef:MyType:file1.c", "typedef int MyType;");
        builder1.add_dependency("foo", "bar"); // calls bar in file2
        let analysis1 = builder1.build();

        let mut builder2 = FileAnalysisBuilder::new("file2.c");
        builder2.add_symbol("function:bar:file2.c", "void bar() {}");
        builder2.add_external_ref("MyType"); // uses MyType from file1
        let analysis2 = builder2.build();

        let deps = CrossFileDeps::analyze_files(vec![analysis1, analysis2]);

        assert_eq!(deps.stats.total_files, 2);
        assert_eq!(deps.stats.total_functions, 2);
        assert_eq!(deps.stats.total_types, 1);
    }

    #[test]
    fn test_affected_files() {
        let mut deps = CrossFileDeps::new();

        // file1 <- file2 <- file3
        deps.file_dependencies.insert("file2.c".to_string(),
            [("file1.c".to_string())].into_iter().collect());
        deps.file_dependencies.insert("file3.c".to_string(),
            [("file2.c".to_string())].into_iter().collect());

        deps.reverse_dependencies.insert("file1.c".to_string(),
            [("file2.c".to_string())].into_iter().collect());
        deps.reverse_dependencies.insert("file2.c".to_string(),
            [("file3.c".to_string())].into_iter().collect());

        let affected = deps.get_affected_files("file1.c");
        assert!(affected.contains("file1.c"));
        assert!(affected.contains("file2.c"));
        assert!(affected.contains("file3.c"));
    }
}

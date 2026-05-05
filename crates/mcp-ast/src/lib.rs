use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub line: usize,
    pub visibility: Visibility,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SymbolKind {
    Function,
    Class,
    Method,
    Interface,
    Struct,
    Enum,
    Constant,
    Variable,
    Import,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Visibility {
    Public,
    Private,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportInfo {
    pub module: String,
    pub items: Vec<String>,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstIndex {
    pub file_path: String,
    pub language: String,
    pub symbols: Vec<Symbol>,
    pub imports: Vec<ImportInfo>,
    pub line_count: usize,
}

pub fn detect_language(file_path: &str) -> &str {
    match file_path.rsplit('.').next().unwrap_or("") {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" => "javascript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "sql" => "sql",
        _ => "unknown",
    }
}

pub fn index_source(file_path: &str, source: &str) -> AstIndex {
    let language = detect_language(file_path);
    let lines: Vec<&str> = source.lines().collect();
    let line_count = lines.len();

    let symbols = match language {
        "typescript" | "javascript" => extract_ts_symbols(&lines),
        "rust" => extract_rust_symbols(&lines),
        "python" => extract_python_symbols(&lines),
        _ => extract_generic_symbols(&lines),
    };

    let imports = match language {
        "typescript" | "javascript" => extract_ts_imports(&lines),
        "rust" => extract_rust_imports(&lines),
        "python" => extract_python_imports(&lines),
        _ => vec![],
    };

    AstIndex {
        file_path: file_path.to_string(),
        language: language.to_string(),
        symbols,
        imports,
        line_count,
    }
}

fn extract_ts_symbols(lines: &[&str]) -> Vec<Symbol> {
    let fn_re = Regex::new(r"(?:export\s+)?(?:async\s+)?function\s+(\w+)").unwrap();
    let class_re = Regex::new(r"(?:export\s+)?class\s+(\w+)").unwrap();
    let iface_re = Regex::new(r"(?:export\s+)?interface\s+(\w+)").unwrap();
    let const_re = Regex::new(r"(?:export\s+)?const\s+(\w+)\s*[=:]").unwrap();
    let arrow_re = Regex::new(r"(?:export\s+)?const\s+(\w+)\s*=\s*(?:async\s+)?\(").unwrap();

    let mut symbols = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let is_export = trimmed.starts_with("export");
        let vis = if is_export {
            Visibility::Public
        } else {
            Visibility::Private
        };

        if let Some(cap) = arrow_re.captures(trimmed) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Function,
                line: i + 1,
                visibility: vis,
            });
        } else if let Some(cap) = fn_re.captures(trimmed) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Function,
                line: i + 1,
                visibility: vis,
            });
        } else if let Some(cap) = class_re.captures(trimmed) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Class,
                line: i + 1,
                visibility: vis,
            });
        } else if let Some(cap) = iface_re.captures(trimmed) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Interface,
                line: i + 1,
                visibility: vis,
            });
        } else if let Some(cap) = const_re.captures(trimmed) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Constant,
                line: i + 1,
                visibility: vis,
            });
        }
    }
    symbols
}

fn extract_rust_symbols(lines: &[&str]) -> Vec<Symbol> {
    let fn_re = Regex::new(r"(?:pub(?:\(crate\))?\s+)?(?:async\s+)?fn\s+(\w+)").unwrap();
    let struct_re = Regex::new(r"(?:pub\s+)?struct\s+(\w+)").unwrap();
    let enum_re = Regex::new(r"(?:pub\s+)?enum\s+(\w+)").unwrap();
    let impl_re = Regex::new(r"impl\s+(\w+)").unwrap();

    let mut symbols = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let vis = if trimmed.starts_with("pub") {
            Visibility::Public
        } else {
            Visibility::Private
        };

        if let Some(cap) = fn_re.captures(trimmed) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Function,
                line: i + 1,
                visibility: vis,
            });
        } else if let Some(cap) = struct_re.captures(trimmed) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Struct,
                line: i + 1,
                visibility: vis,
            });
        } else if let Some(cap) = enum_re.captures(trimmed) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Enum,
                line: i + 1,
                visibility: vis,
            });
        } else if let Some(cap) = impl_re.captures(trimmed) {
            if !trimmed.starts_with("//") {
                symbols.push(Symbol {
                    name: cap[1].to_string(),
                    kind: SymbolKind::Class,
                    line: i + 1,
                    visibility: Visibility::Unknown,
                });
            }
        }
    }
    symbols
}

fn extract_python_symbols(lines: &[&str]) -> Vec<Symbol> {
    let fn_re = Regex::new(r"^(\s*)(?:async\s+)?def\s+(\w+)").unwrap();
    let class_re = Regex::new(r"^class\s+(\w+)").unwrap();

    let mut symbols = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(cap) = class_re.captures(line) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Class,
                line: i + 1,
                visibility: Visibility::Public,
            });
        } else if let Some(cap) = fn_re.captures(line) {
            let indent = cap[1].len();
            let kind = if indent > 0 {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            };
            let vis = if cap[2].starts_with('_') {
                Visibility::Private
            } else {
                Visibility::Public
            };
            symbols.push(Symbol {
                name: cap[2].to_string(),
                kind,
                line: i + 1,
                visibility: vis,
            });
        }
    }
    symbols
}

fn extract_generic_symbols(lines: &[&str]) -> Vec<Symbol> {
    let fn_re = Regex::new(r"(?:function|fn|def|func)\s+(\w+)").unwrap();
    let mut symbols = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(cap) = fn_re.captures(line) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Function,
                line: i + 1,
                visibility: Visibility::Unknown,
            });
        }
    }
    symbols
}

fn extract_ts_imports(lines: &[&str]) -> Vec<ImportInfo> {
    let re = Regex::new(r#"import\s+(?:\{([^}]+)\}|(\w+))\s+from\s+['"]([^'"]+)['"]"#).unwrap();
    let mut imports = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(cap) = re.captures(line) {
            let items = if let Some(named) = cap.get(1) {
                named
                    .as_str()
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .collect()
            } else if let Some(default) = cap.get(2) {
                vec![default.as_str().to_string()]
            } else {
                vec![]
            };
            let module = cap[3].to_string();
            imports.push(ImportInfo {
                module,
                items,
                line: i + 1,
            });
        }
    }
    imports
}

fn extract_rust_imports(lines: &[&str]) -> Vec<ImportInfo> {
    let re = Regex::new(r"use\s+([\w:]+)(?:::\{([^}]+)\})?").unwrap();
    let mut imports = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(cap) = re.captures(line.trim()) {
            let module = cap[1].to_string();
            let items = cap
                .get(2)
                .map(|m| {
                    m.as_str()
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .collect()
                })
                .unwrap_or_default();
            imports.push(ImportInfo {
                module,
                items,
                line: i + 1,
            });
        }
    }
    imports
}

fn extract_python_imports(lines: &[&str]) -> Vec<ImportInfo> {
    let from_re = Regex::new(r"from\s+([\w.]+)\s+import\s+(.+)").unwrap();
    let import_re = Regex::new(r"^import\s+([\w.]+)").unwrap();
    let mut imports = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if let Some(cap) = from_re.captures(trimmed) {
            let items: Vec<String> = cap[2].split(',').map(|s| s.trim().to_string()).collect();
            imports.push(ImportInfo {
                module: cap[1].to_string(),
                items,
                line: i + 1,
            });
        } else if let Some(cap) = import_re.captures(trimmed) {
            imports.push(ImportInfo {
                module: cap[1].to_string(),
                items: vec![],
                line: i + 1,
            });
        }
    }
    imports
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_typescript_parsing() {
        let source = r#"
import { Router } from 'express';
export async function handleAuth(token: string): Promise<User> {
    return user;
}
export class AuthService {
}
const SECRET = "abc";
"#;
        let index = index_source("auth.ts", source);
        assert_eq!(index.language, "typescript");
        assert!(index.symbols.iter().any(|s| s.name == "handleAuth"));
        assert!(index.symbols.iter().any(|s| s.name == "AuthService"));
        assert!(index.imports.iter().any(|i| i.module == "express"));
    }

    #[test]
    fn test_rust_parsing() {
        let source = r#"
use serde::{Serialize, Deserialize};
pub struct Config { pub name: String }
pub async fn init() -> Result<()> { Ok(()) }
"#;
        let index = index_source("lib.rs", source);
        assert_eq!(index.language, "rust");
        assert!(index
            .symbols
            .iter()
            .any(|s| s.name == "Config" && s.kind == SymbolKind::Struct));
        assert!(index
            .symbols
            .iter()
            .any(|s| s.name == "init" && s.kind == SymbolKind::Function));
    }

    #[test]
    fn test_python_parsing() {
        let source = r#"
from flask import Flask
class UserService:
    def get_user(self, id):
        pass
    def _internal(self):
        pass
def main():
    pass
"#;
        let index = index_source("service.py", source);
        assert_eq!(index.language, "python");
        assert!(index
            .symbols
            .iter()
            .any(|s| s.name == "UserService" && s.kind == SymbolKind::Class));
        assert!(index
            .symbols
            .iter()
            .any(|s| s.name == "get_user" && s.kind == SymbolKind::Method));
        assert!(index
            .symbols
            .iter()
            .any(|s| s.name == "_internal" && s.visibility == Visibility::Private));
    }
}

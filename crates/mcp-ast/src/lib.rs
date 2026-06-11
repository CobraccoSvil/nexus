// safety: tutte le `Regex::new("...").unwrap()` in questo modulo sono
// pattern literal hardcoded, compilati UNA volta in static `LazyLock<Regex>`
// (C3, docs/tech-debt-rust.md): `index_source` e' chiamata in loop per-file
// dagli scan di progetto e ricompilare le regex ad ogni file era il collo di
// bottiglia del fallback non-tree-sitter.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

mod ts_parser;

// --- Regex compilate una sola volta (pattern literal; safety: literal valido) ---

// TS/JS: funzioni, classi, interfacce, costanti, arrow function.
static RE_TS_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:export\s+)?(?:async\s+)?function\s+(\w+)").unwrap());
static RE_TS_CLASS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:export\s+)?class\s+(\w+)").unwrap());
static RE_TS_IFACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:export\s+)?interface\s+(\w+)").unwrap());
static RE_TS_CONST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:export\s+)?const\s+(\w+)\s*[=:]").unwrap());
static RE_TS_ARROW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:export\s+)?const\s+(\w+)\s*=\s*(?:async\s+)?\(").unwrap());

// Rust: fn, struct, enum, impl.
static RE_RS_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:pub(?:\(crate\))?\s+)?(?:async\s+)?fn\s+(\w+)").unwrap());
static RE_RS_STRUCT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:pub\s+)?struct\s+(\w+)").unwrap());
static RE_RS_ENUM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:pub\s+)?enum\s+(\w+)").unwrap());
static RE_RS_IMPL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"impl\s+(\w+)").unwrap());

// Python: def (con indent catturato) e class.
static RE_PY_DEF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\s*)(?:async\s+)?def\s+(\w+)").unwrap());
static RE_PY_CLASS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^class\s+(\w+)").unwrap());

// Estrattore generico language-agnostic (Go/Java/C++/C#/Ruby/PHP/...).
static RE_GEN_FN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:function|fn|def|func|sub|fun)\s+(\w+)").unwrap());
// Tipi: class/struct/interface/enum/trait/type/record/protocol/object.
// Include `type Nome` (Go: `type X struct`, TS/Swift type alias) oltre alle
// keyword che precedono direttamente il nome del tipo.
static RE_GEN_TYPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:class|struct|interface|enum|trait|record|protocol|object|type)\s+(\w+)")
        .unwrap()
});
// Metodi a graffe stile C/Java/Go: `tipo Nome(...)` o `func (r R) Nome(...)`.
static RE_GEN_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:[\w<>:\*\&\[\] ]+\s+)?(\w+)\s*\([^;]*\)\s*\{?\s*$").unwrap()
});
static RE_GEN_CONST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:const|val|let|final|static)\s+(\w+)").unwrap());
static RE_GEN_PUBLIC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:pub|public|export)\b").unwrap());
static RE_GEN_PRIVATE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:private|priv|internal)\b").unwrap());

// Import per-linguaggio.
static RE_TS_IMPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"import\s+(?:\{([^}]+)\}|(\w+))\s+from\s+['"]([^'"]+)['"]"#).unwrap()
});
static RE_RS_USE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"use\s+([\w:]+)(?:::\{([^}]+)\})?").unwrap());
static RE_PY_FROM_IMPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"from\s+([\w.]+)\s+import\s+(.+)").unwrap());
static RE_PY_IMPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^import\s+([\w.]+)").unwrap());

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

/// Chiamata di funzione/metodo rilevata (call-graph). Popolata dal parser
/// tree-sitter (AST); il fallback regex la lascia vuota.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallInfo {
    pub caller: Option<String>, // funzione/metodo chiamante, se determinabile
    pub callee: String,         // nome chiamato
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstIndex {
    pub file_path: String,
    pub language: String,
    pub symbols: Vec<Symbol>,
    pub imports: Vec<ImportInfo>,
    #[serde(default)]
    pub calls: Vec<CallInfo>,
    pub line_count: usize,
    /// true se l'indice e' stato prodotto da tree-sitter (AST preciso),
    /// false se dal fallback regex.
    #[serde(default)]
    pub precise: bool,
}

/// Rileva il linguaggio dall'estensione. Language-agnostic: i linguaggi senza
/// parser dedicato ritornano comunque il loro nome reale (non "unknown") cosi'
/// il generatore di documentazione AI sa quale linguaggio sta documentando e il
/// fallback `extract_generic_symbols` estrae comunque i simboli principali.
/// "unknown" e' riservato ai file senza estensione riconoscibile.
pub fn detect_language(file_path: &str) -> &'static str {
    let ext = file_path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "rs" => "rust",
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" | "pyi" => "python",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "scala" | "sc" => "scala",
        "rb" => "ruby",
        "php" => "php",
        "cs" => "csharp",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "m" | "mm" => "objc",
        "lua" => "lua",
        "sh" | "bash" | "zsh" => "shell",
        "pl" | "pm" => "perl",
        "r" => "r",
        "dart" => "dart",
        "ex" | "exs" => "elixir",
        "erl" => "erlang",
        "hs" => "haskell",
        "clj" | "cljs" => "clojure",
        "sql" => "sql",
        "vue" => "vue",
        "svelte" => "svelte",
        "" => "unknown",
        other => {
            // Estensione non in elenco ma presente: la usiamo come etichetta
            // linguaggio (best-effort) invece di scartare il file.
            match other {
                _ if other.len() <= 8 => "code",
                _ => "unknown",
            }
        }
    }
}

pub fn index_source(file_path: &str, source: &str) -> AstIndex {
    let language = detect_language(file_path);
    let lines: Vec<&str> = source.lines().collect();

    // Tentativo AST preciso (tree-sitter) per i linguaggi con grammatica:
    // simboli + call-graph. Gli import restano dal parser regex per-linguaggio
    // (gia' affidabile e mirato agli import intra-progetto). Se la grammatica
    // non c'e' o il parse fallisce, ricade sul parser regex/generic sotto.
    if let Some(mut idx) = ts_parser::index_with_treesitter(file_path, language, source) {
        idx.imports = match language {
            "typescript" | "javascript" => extract_ts_imports(&lines),
            "rust" => extract_rust_imports(&lines),
            "python" => extract_python_imports(&lines),
            _ => Vec::new(),
        };
        return idx;
    }

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
        calls: Vec::new(),
        line_count,
        precise: false,
    }
}

fn extract_ts_symbols(lines: &[&str]) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let is_export = trimmed.starts_with("export");
        let vis = if is_export {
            Visibility::Public
        } else {
            Visibility::Private
        };

        if let Some(cap) = RE_TS_ARROW.captures(trimmed) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Function,
                line: i + 1,
                visibility: vis,
            });
        } else if let Some(cap) = RE_TS_FN.captures(trimmed) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Function,
                line: i + 1,
                visibility: vis,
            });
        } else if let Some(cap) = RE_TS_CLASS.captures(trimmed) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Class,
                line: i + 1,
                visibility: vis,
            });
        } else if let Some(cap) = RE_TS_IFACE.captures(trimmed) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Interface,
                line: i + 1,
                visibility: vis,
            });
        } else if let Some(cap) = RE_TS_CONST.captures(trimmed) {
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
    let mut symbols = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let vis = if trimmed.starts_with("pub") {
            Visibility::Public
        } else {
            Visibility::Private
        };

        if let Some(cap) = RE_RS_FN.captures(trimmed) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Function,
                line: i + 1,
                visibility: vis,
            });
        } else if let Some(cap) = RE_RS_STRUCT.captures(trimmed) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Struct,
                line: i + 1,
                visibility: vis,
            });
        } else if let Some(cap) = RE_RS_ENUM.captures(trimmed) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Enum,
                line: i + 1,
                visibility: vis,
            });
        } else if let Some(cap) = RE_RS_IMPL.captures(trimmed) {
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
    let mut symbols = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(cap) = RE_PY_CLASS.captures(line) {
            symbols.push(Symbol {
                name: cap[1].to_string(),
                kind: SymbolKind::Class,
                line: i + 1,
                visibility: Visibility::Public,
            });
        } else if let Some(cap) = RE_PY_DEF.captures(line) {
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

/// Estrattore generico language-agnostic per i linguaggi senza parser dedicato
/// (Go, Java, C/C++, C#, Ruby, PHP, Kotlin, Swift, ...). Cattura i costrutti
/// piu' comuni con euristiche che funzionano nella maggioranza dei linguaggi a
/// graffe e a indentazione. Best-effort: la documentazione AI completa il resto.
fn extract_generic_symbols(lines: &[&str]) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    let mut seen: std::collections::HashSet<(String, usize)> = std::collections::HashSet::new();
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("//") || line.starts_with('#') || line.starts_with('*') {
            continue;
        }
        let vis = if RE_GEN_PUBLIC.is_match(line) {
            Visibility::Public
        } else if RE_GEN_PRIVATE.is_match(line) {
            Visibility::Private
        } else {
            Visibility::Unknown
        };

        let mut push = |name: &str, kind: SymbolKind, syms: &mut Vec<Symbol>| {
            if name.is_empty() {
                return;
            }
            if seen.insert((name.to_string(), i + 1)) {
                syms.push(Symbol { name: name.to_string(), kind, line: i + 1, visibility: vis.clone() });
            }
        };

        if let Some(cap) = RE_GEN_TYPE.captures(line) {
            push(&cap[1], SymbolKind::Class, &mut symbols);
        }
        if let Some(cap) = RE_GEN_FN.captures(line) {
            push(&cap[1], SymbolKind::Function, &mut symbols);
        } else if let Some(cap) = RE_GEN_CONST.captures(line) {
            push(&cap[1], SymbolKind::Constant, &mut symbols);
        } else if let Some(cap) = RE_GEN_METHOD.captures(line) {
            // Evita falsi positivi su keyword di controllo (if/for/while/switch).
            let name = &cap[1];
            if !matches!(name, "if" | "for" | "while" | "switch" | "catch" | "return" | "match") {
                push(name, SymbolKind::Method, &mut symbols);
            }
        }
    }
    symbols
}

fn extract_ts_imports(lines: &[&str]) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(cap) = RE_TS_IMPORT.captures(line) {
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
    let mut imports = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some(cap) = RE_RS_USE.captures(line.trim()) {
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
    let mut imports = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if let Some(cap) = RE_PY_FROM_IMPORT.captures(trimmed) {
            let items: Vec<String> = cap[2].split(',').map(|s| s.trim().to_string()).collect();
            imports.push(ImportInfo {
                module: cap[1].to_string(),
                items,
                line: i + 1,
            });
        } else if let Some(cap) = RE_PY_IMPORT.captures(trimmed) {
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
    fn test_treesitter_call_graph() {
        // tree-sitter: simboli precisi (precise=true) + call-graph con caller.
        let src = "fn helper() {}\nfn run() {\n    helper();\n    helper();\n}\n";
        let idx = index_source("x.rs", src);
        assert!(idx.precise, "rust deve usare tree-sitter");
        assert!(idx.symbols.iter().any(|s| s.name == "helper"));
        assert!(idx.symbols.iter().any(|s| s.name == "run"));
        assert!(
            idx.calls
                .iter()
                .any(|c| c.callee == "helper" && c.caller.as_deref() == Some("run")),
            "il call-graph deve collegare run -> helper"
        );
    }

    #[test]
    fn test_generic_languages_have_symbols() {
        // Go: func + type struct via estrattore generico.
        let go = index_source(
            "main.go",
            "package main\nfunc Handler(w http.ResponseWriter) {}\ntype Server struct {}\n",
        );
        assert_eq!(go.language, "go");
        assert!(go.symbols.iter().any(|s| s.name == "Handler"));
        assert!(go.symbols.iter().any(|s| s.name == "Server"));

        // Java: class + method.
        let java = index_source(
            "App.java",
            "public class App {\n  public void run() {}\n}\n",
        );
        assert_eq!(java.language, "java");
        assert!(java.symbols.iter().any(|s| s.name == "App"));

        // C++: class + funzione.
        let cpp = index_source("x.cpp", "class Widget {};\nint compute(int a) { return a; }\n");
        assert_eq!(cpp.language, "cpp");
        assert!(cpp.symbols.iter().any(|s| s.name == "Widget"));

        // Linguaggio senza estensione nota: ritorna etichetta, mai vuoto/crash.
        let ruby = index_source("x.rb", "class Foo\n  def bar\n  end\nend\n");
        assert_eq!(ruby.language, "ruby");
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

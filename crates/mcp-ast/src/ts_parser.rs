//! W1-bis code-wiki: parser AST via tree-sitter.
//!
//! Estrae simboli (con riga e visibilita') e call-graph precisi per i linguaggi
//! con grammatica disponibile. Ritorna `None` per i linguaggi senza grammatica:
//! il chiamante (`index_source`) ricade sul parser regex/generic (ibrido
//! language-agnostic). I node-kind sono abbastanza distintivi tra grammatiche da
//! permettere un walk unico invece di query per-linguaggio.

use tree_sitter::{Node, Parser};

use crate::{AstIndex, CallInfo, Symbol, SymbolKind, Visibility};

fn language_for(lang: &str) -> Option<tree_sitter::Language> {
    let l = match lang {
        "rust" => tree_sitter_rust::language(),
        "python" => tree_sitter_python::language(),
        "javascript" => tree_sitter_javascript::language(),
        "typescript" => tree_sitter_typescript::language_typescript(),
        "go" => tree_sitter_go::language(),
        // java/c/cpp/altri: nessuna grammatica qui -> fallback regex/generic.
        _ => return None,
    };
    Some(l)
}

fn symbol_kind_for(node_kind: &str) -> Option<SymbolKind> {
    match node_kind {
        "function_item" | "function_definition" | "function_declaration" => {
            Some(SymbolKind::Function)
        }
        "method_definition" | "method_declaration" => Some(SymbolKind::Method),
        "struct_item" => Some(SymbolKind::Struct),
        "enum_item" | "enum_declaration" | "enum_specifier" => Some(SymbolKind::Enum),
        "trait_item" | "interface_declaration" => Some(SymbolKind::Interface),
        "class_definition" | "class_declaration" => Some(SymbolKind::Class),
        "type_declaration" | "type_spec" | "type_item" | "type_alias_declaration" => {
            Some(SymbolKind::Class)
        }
        _ => None,
    }
}

fn is_call_kind(node_kind: &str) -> bool {
    matches!(node_kind, "call_expression" | "call" | "method_invocation")
}

fn node_name(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src).ok())
        .map(|s| s.to_string())
}

fn call_callee(node: Node, src: &[u8]) -> Option<String> {
    // call_expression -> field "function"; method_invocation (java) -> "name".
    let target = node
        .child_by_field_name("function")
        .or_else(|| node.child_by_field_name("name"))?;
    let text = target.utf8_text(src).ok()?;
    // Ultimo segmento dopo '.' o ':' (callee "semplice", senza ricevitore).
    let last = text.rsplit(['.', ':']).next().unwrap_or(text).trim();
    if last.is_empty() || !last.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false)
    {
        None
    } else {
        Some(last.to_string())
    }
}

fn detect_visibility(node: Node, src: &[u8]) -> Visibility {
    if let Ok(text) = node.utf8_text(src) {
        let head = text.lines().next().unwrap_or("");
        if head.contains("pub") || head.contains("public") || head.contains("export") {
            return Visibility::Public;
        }
        if head.contains("private") {
            return Visibility::Private;
        }
    }
    Visibility::Unknown
}

fn is_type_kind(sk: &SymbolKind) -> bool {
    matches!(
        sk,
        SymbolKind::Class | SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Interface
    )
}

fn walk(
    node: Node,
    src: &[u8],
    symbols: &mut Vec<Symbol>,
    calls: &mut Vec<CallInfo>,
    callers: &mut Vec<String>,
    in_type: bool,
) {
    let kind = node.kind();
    let mut pushed_caller = false;
    let mut child_in_type = in_type;

    if let Some(mut sk) = symbol_kind_for(kind) {
        // Una funzione dichiarata dentro un tipo (classe/struct) e' un metodo:
        // copre Python/JS dove i metodi usano lo stesso node-kind delle funzioni.
        if in_type && matches!(sk, SymbolKind::Function) {
            sk = SymbolKind::Method;
        }
        if is_type_kind(&sk) {
            child_in_type = true;
        }
        if let Some(name) = node_name(node, src) {
            let line = node.start_position().row + 1;
            let mut visibility = detect_visibility(node, src);
            // Convenzione underscore (Python/JS): nome che inizia con '_' e'
            // privato se non c'e' un marcatore esplicito di visibilita'.
            if matches!(visibility, Visibility::Unknown) && name.starts_with('_') {
                visibility = Visibility::Private;
            }
            let is_callable = matches!(sk, SymbolKind::Function | SymbolKind::Method);
            symbols.push(Symbol {
                name: name.clone(),
                kind: sk,
                line,
                visibility,
            });
            if is_callable {
                callers.push(name);
                pushed_caller = true;
            }
        }
    } else if is_call_kind(kind) {
        if let Some(callee) = call_callee(node, src) {
            calls.push(CallInfo {
                caller: callers.last().cloned(),
                callee,
                line: node.start_position().row + 1,
            });
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, src, symbols, calls, callers, child_in_type);
    }

    if pushed_caller {
        callers.pop();
    }
}

/// Indicizza il sorgente con tree-sitter. `None` se il linguaggio non ha
/// grammatica o il parse fallisce (il chiamante usa il fallback regex).
pub fn index_with_treesitter(file_path: &str, language: &str, source: &str) -> Option<AstIndex> {
    let lang = language_for(language)?;
    let mut parser = Parser::new();
    parser.set_language(&lang).ok()?;
    let tree = parser.parse(source, None)?;
    let src = source.as_bytes();

    let mut symbols: Vec<Symbol> = Vec::new();
    let mut calls: Vec<CallInfo> = Vec::new();
    let mut callers: Vec<String> = Vec::new();
    walk(tree.root_node(), src, &mut symbols, &mut calls, &mut callers, false);

    Some(AstIndex {
        file_path: file_path.to_string(),
        language: language.to_string(),
        symbols,
        imports: Vec::new(),
        calls,
        line_count: source.lines().count(),
        precise: true,
    })
}

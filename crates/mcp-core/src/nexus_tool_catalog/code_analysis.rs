//! Registrazione handler dominio: code_analysis
//!
//! Generato dal refactor di `nexus_tool_catalog.rs` (god-file split).
//! Nessun cambiamento di comportamento: spostamento puro delle
//! chiamate `register_with_handler` raggruppate per dominio.

use super::{NexusToolCatalog, NexusToolCategory, NexusToolSpec};
use std::sync::Arc;

pub(super) fn register(c: &NexusToolCatalog) {
    use crate::nexus_tools::{
        ast_parse::AstParseTool, ast_query::AstQueryTool, ca_attr_count::CaAttrCountTool,
        ca_complexity_estimate::CaComplexityEstimateTool, ca_derive_count::CaDeriveCountTool,
        ca_doc_comment_count::CaDocCommentCountTool, ca_enum_count::CaEnumCountTool,
        ca_fn_count::CaFnCountTool, ca_generic_count::CaGenericCountTool,
        ca_if_let_count::CaIfLetCountTool, ca_impl_count::CaImplCountTool,
        ca_inline_comment_count::CaInlineCommentCountTool, ca_lifetime_count::CaLifetimeCountTool,
        ca_macro_count::CaMacroCountTool, ca_match_count::CaMatchCountTool,
        ca_mod_count::CaModCountTool, ca_pub_fn_count::CaPubFnCountTool,
        ca_struct_count::CaStructCountTool, ca_todo_fixme_count::CaTodoFixmeCountTool,
        ca_trait_count::CaTraitCountTool, ca_use_count::CaUseCountTool,
        ca_while_let_count::CaWhileLetCountTool, cargo_check::CargoCheckTool,
        cargo_check_all_features::CargoCheckAllFeaturesTool,
        cargo_check_release::CargoCheckReleaseTool, cargo_edition_detect::CargoEditionDetectTool,
        cargo_fmt_check::CargoFmtCheckTool, cargo_metadata::CargoMetadataTool,
        cargo_msrv_detect::CargoMsrvDetectTool, clippy_lint::ClippyLintTool,
        count_loc::CountLocTool, find_pubapi::FindPubApiTool, find_todos::FindTodosTool,
        find_unsafe::FindUnsafeTool, format_code::FormatCodeTool, lint_run::LintRunTool,
        rustc_explain::RustcExplainTool, rustc_version::RustcVersionTool,
    };

    // CodeAnalysis
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_check",
            NexusToolCategory::CodeAnalysis,
            "Run `cargo check --message-format=json` and parse errors/warnings",
        ),
        Arc::new(CargoCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_metadata",
            NexusToolCategory::CodeAnalysis,
            "Run `cargo metadata --format-version=1` and return workspace graph",
        ),
        Arc::new(CargoMetadataTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "rustc_version",
            NexusToolCategory::CodeAnalysis,
            "Run `rustc --version --verbose` and parse toolchain info",
        ),
        Arc::new(RustcVersionTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "rustc_explain",
            NexusToolCategory::CodeAnalysis,
            "Run `rustc --explain Exxxx` for a given error code",
        ),
        Arc::new(RustcExplainTool),
    );

    // CodeQuality
    c.register_with_handler(
        NexusToolSpec::new(
            "clippy_lint",
            NexusToolCategory::CodeQuality,
            "Run `cargo clippy --message-format=json` and parse lints",
        ),
        Arc::new(ClippyLintTool),
    );

    // CodeQuality (Fase 9C)
    c.register_with_handler(
        NexusToolSpec::new(
            "format_code",
            NexusToolCategory::CodeQuality,
            "Run `cargo fmt [--check]` and list files changed",
        ),
        Arc::new(FormatCodeTool),
    );

    // CodeAnalysis (Fase 9D)
    c.register_with_handler(
        NexusToolSpec::new(
            "ast_parse",
            NexusToolCategory::CodeAnalysis,
            "Parse source into AST via mcp-ast (Rust/TS/JS/Python/Go/Java)",
        ),
        Arc::new(AstParseTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ast_query",
            NexusToolCategory::CodeAnalysis,
            "Query AST symbols by kind/name_pattern/visibility",
        ),
        Arc::new(AstQueryTool),
    );

    // CodeQuality (Fase 9D)
    c.register_with_handler(
        NexusToolSpec::new(
            "lint_run",
            NexusToolCategory::CodeQuality,
            "Multi-language linter dispatcher (clippy / eslint / ruff / flake8)",
        ),
        Arc::new(LintRunTool),
    );

    // Code Analysis extras (Fase 9P, 20 new)
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_struct_count",
            NexusToolCategory::CodeAnalysis,
            "Count struct declarations",
        ),
        Arc::new(CaStructCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_enum_count",
            NexusToolCategory::CodeAnalysis,
            "Count enum declarations",
        ),
        Arc::new(CaEnumCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_trait_count",
            NexusToolCategory::CodeAnalysis,
            "Count trait declarations",
        ),
        Arc::new(CaTraitCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_impl_count",
            NexusToolCategory::CodeAnalysis,
            "Count impl blocks",
        ),
        Arc::new(CaImplCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_fn_count",
            NexusToolCategory::CodeAnalysis,
            "Count fn declarations (sync/async/const/unsafe)",
        ),
        Arc::new(CaFnCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_pub_fn_count",
            NexusToolCategory::CodeAnalysis,
            "Count public function declarations",
        ),
        Arc::new(CaPubFnCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_macro_count",
            NexusToolCategory::CodeAnalysis,
            "Count macro definitions (rules + proc)",
        ),
        Arc::new(CaMacroCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_use_count",
            NexusToolCategory::CodeAnalysis,
            "Count `use` statements",
        ),
        Arc::new(CaUseCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_mod_count",
            NexusToolCategory::CodeAnalysis,
            "Count module declarations",
        ),
        Arc::new(CaModCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_lifetime_count",
            NexusToolCategory::CodeAnalysis,
            "Count lifetime annotations",
        ),
        Arc::new(CaLifetimeCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_generic_count",
            NexusToolCategory::CodeAnalysis,
            "Count generic param usage",
        ),
        Arc::new(CaGenericCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_derive_count",
            NexusToolCategory::CodeAnalysis,
            "Count derive macros",
        ),
        Arc::new(CaDeriveCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_attr_count",
            NexusToolCategory::CodeAnalysis,
            "Count common attribute macros",
        ),
        Arc::new(CaAttrCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_doc_comment_count",
            NexusToolCategory::CodeAnalysis,
            "Count doc comments (///, //!)",
        ),
        Arc::new(CaDocCommentCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_inline_comment_count",
            NexusToolCategory::CodeAnalysis,
            "Count inline comments",
        ),
        Arc::new(CaInlineCommentCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_todo_fixme_count",
            NexusToolCategory::CodeAnalysis,
            "Count TODO/FIXME/XXX/HACK markers",
        ),
        Arc::new(CaTodoFixmeCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_match_count",
            NexusToolCategory::CodeAnalysis,
            "Count match expressions",
        ),
        Arc::new(CaMatchCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_if_let_count",
            NexusToolCategory::CodeAnalysis,
            "Count if let / let Some / let Ok",
        ),
        Arc::new(CaIfLetCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_while_let_count",
            NexusToolCategory::CodeAnalysis,
            "Count while let / for / loop",
        ),
        Arc::new(CaWhileLetCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "ca_complexity_estimate",
            NexusToolCategory::CodeAnalysis,
            "Heuristic cyclomatic complexity estimate",
        ),
        Arc::new(CaComplexityEstimateTool),
    );

    // Fase 9F: CodeAnalysis / Quality batch (3)
    c.register_with_handler(
        NexusToolSpec::new(
            "count_loc",
            NexusToolCategory::CodeAnalysis,
            "Count lines of code per language (self-contained, no tokei)",
        ),
        Arc::new(CountLocTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "find_todos",
            NexusToolCategory::CodeAnalysis,
            "Find TODO/FIXME/HACK/XXX markers in source files",
        ),
        Arc::new(FindTodosTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_fmt_check",
            NexusToolCategory::CodeQuality,
            "Run `cargo fmt --check` and report files needing reformat",
        ),
        Arc::new(CargoFmtCheckTool),
    );

    // Fase 9G: CodeAnalysis batch (2)
    c.register_with_handler(
        NexusToolSpec::new(
            "find_unsafe",
            NexusToolCategory::CodeAnalysis,
            "Find `unsafe` blocks/fn/impl in Rust source files",
        ),
        Arc::new(FindUnsafeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "find_pubapi",
            NexusToolCategory::CodeAnalysis,
            "Count `pub` items per Rust file (top-files surface area)",
        ),
        Arc::new(FindPubApiTool),
    );

    // CodeAnalysis (Fase 9H)
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_check_release",
            NexusToolCategory::CodeAnalysis,
            "`cargo check --release` with warning/error counts",
        ),
        Arc::new(CargoCheckReleaseTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_check_all_features",
            NexusToolCategory::CodeAnalysis,
            "`cargo check --all-features` with warning/error counts",
        ),
        Arc::new(CargoCheckAllFeaturesTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_msrv_detect",
            NexusToolCategory::CodeAnalysis,
            "Walk workspace manifests to find `rust-version` (MSRV)",
        ),
        Arc::new(CargoMsrvDetectTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_edition_detect",
            NexusToolCategory::CodeAnalysis,
            "Walk workspace manifests grouping crates by `edition`",
        ),
        Arc::new(CargoEditionDetectTool),
    );
}

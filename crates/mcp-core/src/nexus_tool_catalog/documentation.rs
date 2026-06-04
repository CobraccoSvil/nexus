//! Registrazione handler dominio: documentation
//!
//! Generato dal refactor di `nexus_tool_catalog.rs` (god-file split).
//! Nessun cambiamento di comportamento: spostamento puro delle
//! chiamate `register_with_handler` raggruppate per dominio.

use super::{NexusToolCatalog, NexusToolCategory, NexusToolSpec};
use std::sync::Arc;

pub(super) fn register(c: &NexusToolCatalog) {
    use crate::nexus_tools::{
        cargo_doc_check::CargoDocCheckTool, doc_api_list::DocApiListTool,
        doc_changelog_check::DocChangelogCheckTool, doc_codeblocks_count::DocCodeblocksCountTool,
        doc_codeblocks_extract::DocCodeblocksExtractTool,
        doc_codeowners_check::DocCodeownersCheckTool,
        doc_contributing_check::DocContributingCheckTool, doc_examples_list::DocExamplesListTool,
        doc_frontmatter_parse::DocFrontmatterParseTool, doc_generate::DocGenerateTool,
        doc_heading_depth::DocHeadingDepthTool, doc_image_list::DocImageListTool,
        doc_license_detect::DocLicenseDetectTool, doc_link_check_local::DocLinkCheckLocalTool,
        doc_links_extract::DocLinksExtractTool, doc_md_lint::DocMdLintTool,
        doc_orphan_md::DocOrphanMdTool, doc_readme_check::DocReadmeCheckTool,
        doc_security_md_check::DocSecurityMdCheckTool, doc_size_report::DocSizeReportTool,
        doc_toc_extract::DocTocExtractTool, doc_word_count::DocWordCountTool,
    };

    // Documentation (Fase 9D)
    c.register_with_handler(
        NexusToolSpec::new(
            "api_docs",
            NexusToolCategory::Documentation,
            "Generate project API docs (cargo doc / npm docs / sphinx)",
        ),
        Arc::new(DocGenerateTool),
    );

    // Documentation extras (Fase 9L, 20 new)
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_readme_check",
            NexusToolCategory::Documentation,
            "Check README.md presence and minimal sections",
        ),
        Arc::new(DocReadmeCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_changelog_check",
            NexusToolCategory::Documentation,
            "Check CHANGELOG.md presence and release count",
        ),
        Arc::new(DocChangelogCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_license_detect",
            NexusToolCategory::Documentation,
            "Detect LICENSE file and license type",
        ),
        Arc::new(DocLicenseDetectTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_codeowners_check",
            NexusToolCategory::Documentation,
            "Check CODEOWNERS file presence",
        ),
        Arc::new(DocCodeownersCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_contributing_check",
            NexusToolCategory::Documentation,
            "Check CONTRIBUTING.md presence",
        ),
        Arc::new(DocContributingCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_security_md_check",
            NexusToolCategory::Documentation,
            "Check SECURITY.md presence with contact/disclosure",
        ),
        Arc::new(DocSecurityMdCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_toc_extract",
            NexusToolCategory::Documentation,
            "Extract markdown headings (table of contents)",
        ),
        Arc::new(DocTocExtractTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_links_extract",
            NexusToolCategory::Documentation,
            "Extract markdown links from a file",
        ),
        Arc::new(DocLinksExtractTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_word_count",
            NexusToolCategory::Documentation,
            "Count words/lines/chars in a markdown file",
        ),
        Arc::new(DocWordCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_link_check_local",
            NexusToolCategory::Documentation,
            "Check that local links in a .md exist on disk",
        ),
        Arc::new(DocLinkCheckLocalTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_image_list",
            NexusToolCategory::Documentation,
            "List images referenced from a .md",
        ),
        Arc::new(DocImageListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_frontmatter_parse",
            NexusToolCategory::Documentation,
            "Parse YAML frontmatter from a .md",
        ),
        Arc::new(DocFrontmatterParseTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_md_lint",
            NexusToolCategory::Documentation,
            "Basic markdown lint (long lines, trailing spaces, tabs)",
        ),
        Arc::new(DocMdLintTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_orphan_md",
            NexusToolCategory::Documentation,
            "Markdown files not referenced from README.md",
        ),
        Arc::new(DocOrphanMdTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_size_report",
            NexusToolCategory::Documentation,
            "Total .md file count and bytes in project",
        ),
        Arc::new(DocSizeReportTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_heading_depth",
            NexusToolCategory::Documentation,
            "Max heading depth and per-level distribution",
        ),
        Arc::new(DocHeadingDepthTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_codeblocks_extract",
            NexusToolCategory::Documentation,
            "Extract fenced code blocks with language",
        ),
        Arc::new(DocCodeblocksExtractTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_codeblocks_count",
            NexusToolCategory::Documentation,
            "Count fenced code blocks per language",
        ),
        Arc::new(DocCodeblocksCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_api_list",
            NexusToolCategory::Documentation,
            "List .md files under docs/api",
        ),
        Arc::new(DocApiListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "doc_examples_list",
            NexusToolCategory::Documentation,
            "List entries under examples/",
        ),
        Arc::new(DocExamplesListTool),
    );

    // Documentation (Fase 9H)
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_doc_check",
            NexusToolCategory::Documentation,
            "`cargo doc --no-deps --quiet` with warning/error counts",
        ),
        Arc::new(CargoDocCheckTool),
    );
}

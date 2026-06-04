//! Registrazione handler dominio: testing
//!
//! Generato dal refactor di `nexus_tool_catalog.rs` (god-file split).
//! Nessun cambiamento di comportamento: spostamento puro delle
//! chiamate `register_with_handler` raggruppate per dominio.

use super::{NexusToolCatalog, NexusToolCategory, NexusToolSpec};
use std::sync::Arc;

pub(super) fn register(c: &NexusToolCatalog) {
    use crate::nexus_tools::{
        cargo_test::CargoTestTool, cargo_test_doc::CargoTestDocTool,
        cargo_test_lib::CargoTestLibTool, coverage_report::CoverageReportTool,
        test_assert_count::TestAssertCountTool, test_bench_count::TestBenchCountTool,
        test_count_files::TestCountFilesTool, test_coverage::TestCoverageTool,
        test_coverage_summary::TestCoverageSummaryTool, test_doc_count::TestDocCountTool,
        test_failed_log::TestFailedLogTool, test_fixtures_list::TestFixturesListTool,
        test_generate::TestGenerateTool, test_ignored_count::TestIgnoredCountTool,
        test_mock_count::TestMockCountTool, test_module_count::TestModuleCountTool,
        test_playwright::TestPlaywrightTool, test_proptest_count::TestProptestCountTool,
        test_quickcheck_count::TestQuickcheckCountTool,
        test_run_integration::TestRunIntegrationTool, test_run_quiet::TestRunQuietTool,
        test_run_unit::TestRunUnitTool, test_run_workspace::TestRunWorkspaceTool,
        test_should_panic_count::TestShouldPanicCountTool,
        test_snapshots_list::TestSnapshotsListTool, test_stale_snapshots::TestStaleSnapshotsTool,
        test_workflow_files::TestWorkflowFilesTool,
    };

    // Testing
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_test",
            NexusToolCategory::Testing,
            "Run `cargo test --no-fail-fast` and parse pass/fail counts",
        ),
        Arc::new(CargoTestTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_coverage",
            NexusToolCategory::Testing,
            "Run `cargo llvm-cov --json --summary-only` and summarize coverage",
        ),
        Arc::new(TestCoverageTool),
    );

    // Testing (Fase 9D)
    c.register_with_handler(
        NexusToolSpec::new(
            "test_generate",
            NexusToolCategory::Testing,
            "Scaffold unit tests from function signatures (mcp-ast based)",
        ),
        Arc::new(TestGenerateTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "coverage_report",
            NexusToolCategory::Testing,
            "Multi-stack coverage dispatcher (cargo llvm-cov / npm coverage)",
        ),
        Arc::new(CoverageReportTool),
    );

    // Testing extras (Fase 9N, 20 new)
    c.register_with_handler(
        NexusToolSpec::new(
            "test_run_unit",
            NexusToolCategory::Testing,
            "Run `cargo test --lib --quiet`",
        ),
        Arc::new(TestRunUnitTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_run_integration",
            NexusToolCategory::Testing,
            "Run `cargo test --tests --quiet`",
        ),
        Arc::new(TestRunIntegrationTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_run_quiet",
            NexusToolCategory::Testing,
            "Run `cargo test --quiet` with optional filter",
        ),
        Arc::new(TestRunQuietTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_run_workspace",
            NexusToolCategory::Testing,
            "Run `cargo test --workspace --quiet`",
        ),
        Arc::new(TestRunWorkspaceTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_count_files",
            NexusToolCategory::Testing,
            "Count *_test.rs and tests/*.rs files",
        ),
        Arc::new(TestCountFilesTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_ignored_count",
            NexusToolCategory::Testing,
            "Count `#[ignore]` attributes in source",
        ),
        Arc::new(TestIgnoredCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_should_panic_count",
            NexusToolCategory::Testing,
            "Count `#[should_panic` attributes",
        ),
        Arc::new(TestShouldPanicCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_module_count",
            NexusToolCategory::Testing,
            "Count test modules (`mod tests`, `#[cfg(test)]`)",
        ),
        Arc::new(TestModuleCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_assert_count",
            NexusToolCategory::Testing,
            "Count assert!/assert_eq!/assert_ne!/debug_assert",
        ),
        Arc::new(TestAssertCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_proptest_count",
            NexusToolCategory::Testing,
            "Count proptest!/prop_assert/use proptest",
        ),
        Arc::new(TestProptestCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_quickcheck_count",
            NexusToolCategory::Testing,
            "Count #[quickcheck]/quickcheck! usages",
        ),
        Arc::new(TestQuickcheckCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_mock_count",
            NexusToolCategory::Testing,
            "Count mockall/wiremock/MockServer usages",
        ),
        Arc::new(TestMockCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_bench_count",
            NexusToolCategory::Testing,
            "Count #[bench]/criterion_group!/criterion_main!",
        ),
        Arc::new(TestBenchCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_doc_count",
            NexusToolCategory::Testing,
            "Count doctest fences in /// comments",
        ),
        Arc::new(TestDocCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_fixtures_list",
            NexusToolCategory::Testing,
            "List entries under tests/fixtures/",
        ),
        Arc::new(TestFixturesListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_snapshots_list",
            NexusToolCategory::Testing,
            "Walk for `.snap` files (insta snapshots)",
        ),
        Arc::new(TestSnapshotsListTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_stale_snapshots",
            NexusToolCategory::Testing,
            "Walk for `.snap.new` files (unaccepted snapshots)",
        ),
        Arc::new(TestStaleSnapshotsTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_coverage_summary",
            NexusToolCategory::Testing,
            "Check for cobertura.xml/lcov.info/tarpaulin reports",
        ),
        Arc::new(TestCoverageSummaryTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_failed_log",
            NexusToolCategory::Testing,
            "Run `cargo test --no-run --quiet` and parse compile errors",
        ),
        Arc::new(TestFailedLogTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_workflow_files",
            NexusToolCategory::Testing,
            "List .github/workflows/*.yml with test mentions",
        ),
        Arc::new(TestWorkflowFilesTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "test_playwright",
            NexusToolCategory::Testing,
            "Run Playwright e2e test suite (`npx playwright test`) with pass/fail counts",
        ),
        Arc::new(TestPlaywrightTool),
    );

    // Testing (Fase 9H)
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_test_doc",
            NexusToolCategory::Testing,
            "`cargo test --doc` with passed/failed counts",
        ),
        Arc::new(CargoTestDocTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_test_lib",
            NexusToolCategory::Testing,
            "`cargo test --lib` with passed/failed counts",
        ),
        Arc::new(CargoTestLibTool),
    );
}

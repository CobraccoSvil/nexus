//! Registrazione handler dominio: performance
//!
//! Generato dal refactor di `nexus_tool_catalog.rs` (god-file split).
//! Nessun cambiamento di comportamento: spostamento puro delle
//! chiamate `register_with_handler` raggruppate per dominio.

use super::{NexusToolCatalog, NexusToolCategory, NexusToolSpec};
use std::sync::Arc;

pub(super) fn register(c: &NexusToolCatalog) {
    use crate::nexus_tools::{
        bench_run::BenchRunTool, cargo_bench::CargoBenchTool,
        cargo_size_estimate::CargoSizeEstimateTool, perf_arc_mutex::PerfArcMutexTool,
        perf_async_funcs::PerfAsyncFuncsTool, perf_binary_size::PerfBinarySizeTool,
        perf_box_count::PerfBoxCountTool, perf_cargo_bloat::PerfCargoBloatTool,
        perf_cargo_build_time::PerfCargoBuildTimeTool, perf_clone_count::PerfCloneCountTool,
        perf_codegen_units::PerfCodegenUnitsTool, perf_compile_units::PerfCompileUnitsTool,
        perf_dep_count::PerfDepCountTool, perf_largest_files::PerfLargestFilesTool,
        perf_loc_per_crate::PerfLocPerCrateTool, perf_lto_check::PerfLtoCheckTool,
        perf_optimization_check::PerfOptimizationCheckTool, perf_panic_count::PerfPanicCountTool,
        perf_string_alloc::PerfStringAllocTool, perf_target_dir_size::PerfTargetDirSizeTool,
        perf_test_count::PerfTestCountTool, perf_unsafe_blocks::PerfUnsafeBlocksTool,
        perf_unused_deps::PerfUnusedDepsTool, profile_run::ProfileRunTool,
    };

    // Performance
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_bench",
            NexusToolCategory::Performance,
            "Run `cargo bench` and count benchmark entries",
        ),
        Arc::new(CargoBenchTool),
    );

    // Performance (Fase 9D)
    c.register_with_handler(
        NexusToolSpec::new(
            "profile_run",
            NexusToolCategory::Performance,
            "Wall-clock profiling with N runs and mean/min/max/p95 stats",
        ),
        Arc::new(ProfileRunTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "bench_run",
            NexusToolCategory::Performance,
            "Benchmark dispatcher (cargo bench / npm run bench)",
        ),
        Arc::new(BenchRunTool),
    );

    // Performance extras (Fase 9M, 20 new)
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_cargo_build_time",
            NexusToolCategory::Performance,
            "Run `cargo build --timings` and report duration",
        ),
        Arc::new(PerfCargoBuildTimeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_binary_size",
            NexusToolCategory::Performance,
            "Sizes of binaries in target/release",
        ),
        Arc::new(PerfBinarySizeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_cargo_bloat",
            NexusToolCategory::Performance,
            "`cargo bloat --release --crates -n 20`",
        ),
        Arc::new(PerfCargoBloatTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_target_dir_size",
            NexusToolCategory::Performance,
            "Total size of target/ directory",
        ),
        Arc::new(PerfTargetDirSizeTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_largest_files",
            NexusToolCategory::Performance,
            "Top N .rs files by byte size",
        ),
        Arc::new(PerfLargestFilesTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_loc_per_crate",
            NexusToolCategory::Performance,
            "Lines of Rust code per workspace crate",
        ),
        Arc::new(PerfLocPerCrateTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_unused_deps",
            NexusToolCategory::Performance,
            "Heuristic: deps in Cargo.toml not referenced in src/",
        ),
        Arc::new(PerfUnusedDepsTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_test_count",
            NexusToolCategory::Performance,
            "Count #[test] / #[tokio::test] attributes",
        ),
        Arc::new(PerfTestCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_async_funcs",
            NexusToolCategory::Performance,
            "Count `async fn` and `.await` usages",
        ),
        Arc::new(PerfAsyncFuncsTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_unsafe_blocks",
            NexusToolCategory::Performance,
            "Count `unsafe {`, `unsafe fn`, `unsafe impl`",
        ),
        Arc::new(PerfUnsafeBlocksTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_panic_count",
            NexusToolCategory::Performance,
            "Count `panic!`/`unwrap()`/`expect(`/`todo!`/`unimplemented!`",
        ),
        Arc::new(PerfPanicCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_clone_count",
            NexusToolCategory::Performance,
            "Count `.clone()` and `.to_owned()`",
        ),
        Arc::new(PerfCloneCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_string_alloc",
            NexusToolCategory::Performance,
            "Count common String allocation patterns",
        ),
        Arc::new(PerfStringAllocTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_box_count",
            NexusToolCategory::Performance,
            "Count `Box<dyn` and `Box::new`",
        ),
        Arc::new(PerfBoxCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_arc_mutex",
            NexusToolCategory::Performance,
            "Count Arc<Mutex/RwLock> patterns",
        ),
        Arc::new(PerfArcMutexTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_dep_count",
            NexusToolCategory::Performance,
            "Count deps/dev-deps/build-deps in Cargo.toml",
        ),
        Arc::new(PerfDepCountTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_compile_units",
            NexusToolCategory::Performance,
            "Workspace package count via `cargo metadata`",
        ),
        Arc::new(PerfCompileUnitsTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_optimization_check",
            NexusToolCategory::Performance,
            "Inspect [profile.release] optimization keys",
        ),
        Arc::new(PerfOptimizationCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_lto_check",
            NexusToolCategory::Performance,
            "Check LTO setting in [profile.release]",
        ),
        Arc::new(PerfLtoCheckTool),
    );
    c.register_with_handler(
        NexusToolSpec::new(
            "perf_codegen_units",
            NexusToolCategory::Performance,
            "Check codegen-units in [profile.release]",
        ),
        Arc::new(PerfCodegenUnitsTool),
    );

    // Performance (Fase 9H)
    c.register_with_handler(
        NexusToolSpec::new(
            "cargo_size_estimate",
            NexusToolCategory::Performance,
            "Sum sizes of binaries in target/release/ (per-bin and total)",
        ),
        Arc::new(CargoSizeEstimateTool),
    );
}

// Guscio del binario: costruisce il runtime e delega alla lib.
//
// Tutto il resto di mcp-core vive in `src/lib.rs`. Fino al 2026-08-05 questo
// crate era un BINARIO PURO — nessun `lib.rs`, nessun `[lib]` nel Cargo.toml —
// e 215.421 righe, meta' del workspace, stavano in una singola unita' di
// compilazione che nessun altro crate poteva usare. Due conseguenze:
//
//   1. I 10 test in `tests/` non testavano il codice. Nessuno fa
//      `use mcp_core::...`: interrogano il servizio via HTTP, perche' un bin
//      non e' linkabile e non c'era altro modo. Meta' del workspace non aveva
//      alcun test unitario esterno — la premessa della regola O non era
//      soddisfatta, e non per distrazione: era impossibile.
//   2. I punti unici che vivono qui dentro (27, secondo il catalogo di
//      CLAUDE.md) erano irraggiungibili da admin-service, plugin-service,
//      doc-service e dai worker — e la regola L chiede che i call site
//      DELEGHINO al punto unico. Non potendo, duplicavano: e' la storia delle
//      wave di de-duplicazione (nexus-mcp-client, admin_dto, fs_browse sono
//      nati cosi').
//
// NON risolve la doppia compilazione, e vale la pena scriverlo perche' e'
// l'errore che avevo fatto: MISURATO con `cargo test -p mcp-core --no-run -v`,
// il codice pesante viene compilato DUE volte anche cosi' — una come `rlib`,
// una col test harness (`--test`), perche' `cfg(test)` cambia il codice e la
// rlib non e' riusabile. Prima erano due passate su `main.rs`, ora sono due
// passate su `lib.rs`: le uniche invocazioni diventate banali sono quelle su
// questo guscio.
//
// Qui resta solo cio' che appartiene davvero all'eseguibile: la costruzione
// del runtime.

fn main() -> anyhow::Result<()> {
    // Builder esplicito al posto di #[tokio::main]: serve `thread_stack_size`.
    // Tre crash STATUS_STACK_OVERFLOW (0xc00000fd, faulting __chkstk) in un
    // giorno sui tokio-rt-worker durante run agentici con payload JSON grossi:
    // lo stack default dei worker (2 MB) e' tarato su frame da build RELEASE,
    // ma lo stack dev gira in DEBUG dove i frame Rust sono 10-20x piu' grandi
    // — l'equivalente release di 2 MB debug e' ~200 KB. Gli 8 MB riallineano
    // il margine debug a quello che release ha gia'; una ricorsione INFINITA
    // esploderebbe comunque (piu' tardi), quindi nessun bug viene mascherato.
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(32)
        .thread_stack_size(8 * 1024 * 1024)
        .enable_all()
        .build()?
        .block_on(mcp_core::run())
}

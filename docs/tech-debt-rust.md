# Tech debt — Rust

Backlog delle violazioni CLAUDE.md §F nei crate Rust (`unwrap`/`expect` fuori
test). Aggiornato durante Fase 3 del branch `chore/backlog-closure` (2026-05-19).

## Strumenti di scansione

NOTA (migrazione zero-Python): gli script di scansione Python
(`scripts/unwrap-perfile-v2.py`, `scripts/classify-unwrap.py`) sono stati RIMOSSI.
Il conteggio resta riproducibile con un grep diretto sui crate, ad esempio:

```bash
# Conteggio grezzo unwrap/expect fuori dai blocchi di test:
rg -n --type rust '\.(unwrap|expect)\(' crates/ | rg -v '#\[cfg\(test\)\]'
```

## Baseline post-Fase-3 step 1+2 (2026-05-19)

  Totale PROD:  128 unwrap + 23 expect = 151
  Totale TEST:  316 unwrap + 24 expect = 340  (legittime)

  Classificazione PROD:
    REGEX literal hardcoded:   93   (ammessi §F, vedi sotto)
    PARSE static literal:        1
    OTHER:                      57   (di cui 15 fix reali applicati, 17 idiomatici §F,
                                       5 falsi positivi del classifier, 20 residui minori)

## Cluster Regex literal — annotati come safety §F

CLAUDE.md §F ammette esplicitamente `Regex::new("...").unwrap()` su pattern
literal. I 6 file con cluster denso hanno commento `// safety:` in testa che
ricorda la regola e segnala il refactor opportuno (`LazyLock<Regex>`):

- `crates/mcp-quality/src/lib.rs`            — 29 — FATTO (C3, static LazyLock)
- `crates/mcp-ast/src/lib.rs`                — 16 — FATTO (C3, static LazyLock)
- `crates/mcp-core/src/project_workspace/scan_ports.rs` — 14
- `crates/mcp-learning/src/lib.rs`            — 8
- `crates/mcp-core/src/nexus_tools/secret_scan.rs` — 7
- `crates/mcp-core/src/nexus_tools/sast_scan.rs`   — 6
- `crates/mcp-core/src/project_workspace/sync_ports.rs` — 2 (annotato in Fase 3 step 2)

## Idiomatici §F — non da fixare

Eccezioni ammesse esplicitamente da §F. Documentati qui invece che con annotazione
inline per non disperdere il rumore:

### Env bootstrap (fail-fast all'avvio del servizio)
- `crates/admin-service/src/main.rs:63` — `env::var("DATABASE_URL").expect(...)`
- `crates/billing-service/src/main.rs:54` — id.
- `crates/chat-service/src/main.rs:69` — id.
- `crates/doc-service/src/main.rs:43` — id.
- `crates/plugin-service/src/main.rs:42` — id.
- `crates/mcp-core/src/main.rs:637, 674` — `TOOL_RUNNER_ADDR`, `AGENT_ROUTER_ADDR`.
- `crates/mcp-core/src/main.rs:2214` — `SIGTERM` handler registration.

### Tokenizer init (cl100k_base)
- `crates/mcp-token/src/lib.rs:44, 84` — load tokenizer condizionante: se fallisce, il modulo non puo' lavorare.

### Lock poisoned (semantica: invariante rotta = panic)
- `crates/nexus-orchestrator/src/prompt_registry.rs:35, 57` — `read/write().expect("poisoned")`.

### Conversione statica SHA256[..16] → [u8; 16]
- `crates/mcp-core/src/projects/indexing.rs:284, 502, 672` — `try_into().expect("sha256>=16")`.
- `crates/mcp-core/src/nexus_builtin/mcp_runtime.rs:141` — `digest[..16].try_into().unwrap()`.

### Parse di literal compile-time
- `crates/mcp-core/src/agent_tools/testing.rs:62` — `"127.0.0.1:0".parse()`.
- `crates/mcp-core/src/nexus_builtin/catalog.rs:13` — `Uuid::parse_str(NEXUS_BUILTIN_SERVER_ID_STR).expect(...)`.

### reqwest client builder (bootstrap critico — vedi nexus-http/lib.rs:110)
- `crates/mcp-core/src/nexus_builtin/docs.rs:131` — `Client::builder()...build().unwrap()`.
- `crates/mcp-core/src/nexus_gateway.rs:77` — `.expect("reqwest client")`.

### Time validi (mod 24h)
- `crates/mcp-core/src/chat_learning.rs:1200` — `and_hms_milli_opt(hour, minute, 0, 0).expect("valid time")` su valori da loop validati.

## Fix reali applicati (Fase 3 step 2)

15 unwrap reali convertiti a `?`, `let-else`, pattern match, `ok_or_else`, `unwrap_or_*`:

- `mcp_client.rs` (mcp-core + plugin-service): `child.stdin/stdout/stderr.take()` → `ok_or_else(McpError::Protocol)`
- `agent_processes.rs:93` — `project_root.as_ref().unwrap()` → `ok_or_else(String)`
- `main.rs:211` — `pid.unwrap()` → let-else + continue
- `prompt_admin.rs:36` — `is_some()` + `.unwrap()` → if-let
- `openapi_validate.rs:75` — guardia + unwrap → match
- `orchestrator.rs:2386` → `unwrap_or_default`
- `profiles.rs:404` → `map + unwrap_or_else`
- `prompt_templates.rs:868` → `Value::Array` pattern
- `mcp-db/lib.rs:34` → fallback ParserError difensivo
- `ruvector/core.rs:278` → let-else
- `git_blame.rs:102` → let-else dopo peek
- `profile_run.rs:32` → indice esplicito post is_empty guard
- `auth.rs:307` → `unwrap_or_else` con Response default
- `nexus-http/lib.rs:66,71` → if-let
- `routing_config.rs` — `parse_str` magic fallback rimosso → propaga errore (CLAUDE.md §G, commit Fase 1).

## Falsi positivi del classifier (string content)

- `mcp-quality/lib.rs:422,426,427` — `.unwrap()` come stringa in messaggi/code-smell.
- `nexus_tools/perf_panic_count.rs:12` — lista pattern da scansionare.
- `nexus_tools/sec_unwrap_count.rs:12` — lista pattern.
- `nexus_tools/sec_audit_summary.rs:15` — pattern audit.

## Residui minori (~20, da affrontare in commit successivi)

Non considerati high-priority: principalmente in tool/utility che fanno chain di
operazioni dove un crash non corrompe stato persistente. Esempi:
- `crates/mcp-core/src/nexus_tools/{fs_*,extract_function,rename_symbol}.rs` — fs/path ops.
- `crates/mcp-comments/src/lib.rs` — 5 unwrap.
- `crates/mcp-core/src/projects/indexing.rs` — 4 unwrap fs + path (non gia' citate sopra).

Per ognuno la strategia consigliata e' la stessa applicata in step 2 (let-else,
pattern match, error propagation). Aprire un commit dedicato per crate.

## Refactor opportuno (non violazione)

Migrare i `Regex::new("...").unwrap()` annotati a `std::sync::LazyLock<Regex>`
per evitare ricompilazione ad ogni chiamata. Miglioramento di performance, non
fix di sicurezza — gestire come tech-debt separato.

## Gate ratchet qualita codice (quality-scan)

Gate automatico anti-god-file/god-function sul codice Rust del workspace,
analogo a jscpd (`.dup-baseline.json`) e audit-settings. Punto unico (regola L):
la logica di scansione vive in `mcp_quality::scan` (`crates/mcp-quality/src/scan.rs`);
il sottocomando `xtask quality-scan` la consuma; `scripts/quality-scan.sh` e' il
wrapper; `scripts/verify.sh` lo esegue come fase finale (saltabile con
`VERIFY_SKIP_RUST=1`).

Metriche sottoposte a gate (file di test esclusi) — possono solo SCENDERE
rispetto a `scripts/quality-baseline.json`:

- `total`            — findings totali (tutte le categorie)
- `long_functions`   — funzioni > 50 righe
- `complexity_high`  — funzioni con complessita ciclomatica > 20
- `security`         — findings di categoria security

Baseline iniziale (2026-06-27, post-porting zero-Python, 845 file non-test):

  findings totali   : 10300
  funzioni >50 righe:   876
  complessita >20   :    43
  security          :   133

Uso:

```bash
bash scripts/quality-scan.sh --gate     # default: exit!=0 su regressione
bash scripts/quality-scan.sh --update   # riallinea la baseline al ribasso dopo un refactor
```

Nota di attribuzione: il porting LangGraph->Rust (`nexus-agent-graph` +
`nexus-gateway`) contribuisce solo ~15% dei findings, ~11% delle funzioni
lunghe e ~9% della complessita estrema. Gli hotspot peggiori (complessita 104
in `chat_messages/agent_run.rs`, 84 in `project_db_routes/connection.rs`, 63 in
`project_workspace/wizard.rs`) sono codice `mcp-core` preesistente al porting.

### C3 — cluster HOT completati (2026-06-11)

I cluster con compilazione per-file durante gli scan di progetto sono stati
convertiti a `static LazyLock<Regex>` a livello di modulo:

- `crates/mcp-quality/src/lib.rs` — 25 static (28 occorrenze, 3 pattern condivisi
  consolidati in `RE_FN_DEF_CAPTURE`); `analyze_source` e' chiamata in loop
  per-file da `projects/quality.rs`.
- `crates/mcp-quality/src/injection.rs` — gia' a `LazyLock` (ADR 0021), nessuna
  modifica necessaria.
- `crates/mcp-ast/src/lib.rs` — 21 static; `index_source` per-file (fallback
  non-tree-sitter).
- `crates/mcp-core/src/agent_tools/port_scanner.rs` — gia' a `once_cell::Lazy`
  (PORT_REGEXES, RANGE_REGEX, DEFAULT_PORT_REGEX, ENV_FALLBACK_PORT_REGEXES),
  nessuna modifica necessaria. Nota: ENV_FALLBACK_PORT_REGEXES usa `format!`
  ma su costanti, dentro lo static — compilazione comunque una-tantum.

Misura empirica: test `bench_analyze_source_200_iterazioni` in mcp-quality
(200 iterazioni di `analyze_source` su sorgente sintetico ~110 righe, build
dev): 200 iterazioni in ~816 ms, ~4,08 ms per iterazione, senza alcuna
ricompilazione regex per-chiamata. Lanciare con:
`cargo test -p mcp-quality --message-format=short -- --nocapture bench_analyze_source`

Cluster residui (compilazione NON per-file o bassa frequenza): `scan_ports.rs`,
`mcp-learning/src/lib.rs`, `secret_scan.rs`, `sast_scan.rs`, `sync_ports.rs`.

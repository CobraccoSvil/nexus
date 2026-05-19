# Tech debt — Rust

Backlog delle violazioni CLAUDE.md §F nei crate Rust (`unwrap`/`expect` fuori
test). Aggiornato durante Fase 3 del branch `chore/backlog-closure` (2026-05-19).

## Strumenti di scansione

```bash
# Conteggio robusto PROD vs TEST (esclude correttamente blocchi #[cfg(test)]):
python3 scripts/unwrap-perfile-v2.py all

# Classificazione semantica (regex literal / Mutex / Option real / ecc.):
python3 scripts/classify-unwrap.py
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

- `crates/mcp-quality/src/lib.rs`            — 29
- `crates/mcp-ast/src/lib.rs`                — 16
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

---
id: adr-0026-punto-unico-de-duplicazione
kind: adr
title: "ADR 0026 - Punto unico di controllo: de-duplicazione e meccanismo di centralizzazione"
slug: 0026-punto-unico-de-duplicazione
tags:
  - adr
  - single-source-of-truth
  - de-duplicazione
  - regola-L
  - tooling
  - enforcement
auto_generated: false
nexus_meta_version: 1
---

# ADR 0026 - Punto unico di controllo: de-duplicazione e meccanismo di centralizzazione

## Stato

In corso. Operativizza la regola L del `CLAUDE.md` con tooling di enforcement e
un catalogo dei punti unici. Implementazione a wave (vedi sezione "Roadmap").

## Contesto

La regola L ("ogni decisione/logica ha UN solo punto di controllo; i call site
delegano, non re-implementano") era un principio senza enforcement automatico.
Un'analisi sistematica del monorepo ha trovato violazioni in tutti e tre i layer,
con divergenza silenziosa gia' in atto. Esempi verificati:

- `parse_user_id` definito 2 volte (`crates/nexus-types/src/lib.rs` + copia
  `pub(crate)` in `crates/mcp-core/src/projects/mod.rs`), ~56 call site con import incoerenti.
- `get_setting` con 5 varianti a semantica errori incompatibile (alcune ingoiano
  l'errore e ritornano `None`, altre lo propagano) -> bug subdoli.
- `TemplateCache` duplicata identica tra `mcp-core` e `admin-service`, due cache TTL
  non coordinate sullo stesso DB.
- Python: pattern "cache 60s" copiato in 4 punti; `psycopg2.connect()` con
  connection-string copiata in ~31 file; estrazione JSON-da-markdown in 3+ nodi.
- Frontend: componenti condivisi esistenti ma quasi inutilizzati (1 uso su ~20 pagine).
- Cross-language: 4 implementazioni Python del chunking divergenti dall'unica Rust.

Senza una misura e un gate, il debito puo' solo crescere a ogni PR.

## Decisione

1. **Meccanismo di centralizzazione per-caso** (non libero), allineato a
   "composition over inheritance".
2. **Catalogo dei punti unici** come riferimento autoritativo (sotto).
3. **Enforcement automatico e duraturo**: misura jscpd + gate "ratchet" + guard
   testuale, innestati in pre-commit e CI.

### Meccanismo di centralizzazione (criterio per-caso)

L'ereditarieta' di classi si usa SOLO per relazioni "is-a" reali e poco profonde,
mai per riusare codice. Vincolo pratico: Rust non ha ereditarieta' di classi
(si usano `trait`/generics/composizione); in React ereditare componenti e' anti-pattern.

| Natura della logica duplicata | Meccanismo corretto | Esempio |
|---|---|---|
| Stateless (calcolo puro, IO singolo) | funzione in un modulo | `get_setting`, `parse_user_id`, `extract_json_block`, `chunk_text`, `formatDate` |
| Stato + comportamento | classe/struct incapsulata + generics | `TtlCache<K,V>` (Rust), `db_pool` (Python) |
| Varianti polimorfiche su contratto comune | `trait` (Rust) / ABC-Protocol (Python) + composizione | `provider_health`, `capability`; provider su `brain/providers/base.py` |
| UI | composizione (componenti + custom hooks) | `AdminPageHeader`, `useListData`, `AdminModal` |

Anti-pattern vietati: incapsulare una funzione stateless in una classe con
sottoclassi ("regno dei sostantivi"); gerarchie di ereditarieta' profonde per
condividere codice (fragile base class, accoppiamento al genitore).

### Catalogo dei punti unici

| Concern | Modulo/funzione autoritativa | Stato |
|---|---|---|
| Gate disponibilita' provider | gate unico (ADR 0020) | esistente |
| SQL-injection detector | detector unificato (ADR 0021) | esistente |
| Capability modello (vision/tool/thinking) | vista `v_model_capabilities` (mig 0318) + classificatore `model_catalog_sync.rs::{classify_capabilities, infer_capabilities_from_name}` (ADR 0024) | esistente |
| Routing/default/purpose model | `routing_matrix.rs` + tabelle mig 0101/0102 | esistente |
| Selezione catalog: eleggibilita' + pesi scoring | `orchestrator/model_selection.rs` (`EligibilityFilter`, `select_models_tierchain`, `default_scoring_weights`) + riga sentinella mig 0379 (ADR 0030) | esistente |
| Identita' utente/progetto | `crates/nexus-types/src/lib.rs` | Wave 1 |
| Cache TTL (Rust) | crate `nexus-cache` (`TtlCache<K,V>`) | Wave 2 |
| Lettura settings (Rust) | `nexus-auth::settings` (`get_setting`, `get_setting_nonempty`, bool/int) | Wave 3 |
| Health/cooldown provider | `mcp-core/src/provider_health.rs` | Wave 5 |
| Pool DB (Python) | `brain/utils/db_pool.py` | Wave 6 |
| Cache TTL (Python) | `brain/utils/ttl_cache.py` | Wave 6 |
| Estrazione JSON da markdown (Python) | `brain/utils/json_extract.py` | Wave 6 |
| Intent canonici (Python) | `brain/router/intents.py` | Wave 6 |
| Fetch HTTP frontend | `apps/web-ide/lib/api/_shared.ts` (`fetchJson`) | esistente |
| Formatter frontend | `apps/web-ide/lib/format.ts` | Wave 7 |
| Chunking testo | `crates/mcp-core/src/rag/chunker.rs` (riferimento) + `brain/utils/text_chunk.py` (paritetico, golden test) | esistente |
| Classificazione errore provider (testo) | `crates/mcp-core/src/provider_error_classifier.rs` (paritetico a `brain/providers/error_handler.py`, golden test) | esistente |
| Path-safety workspace (resolve_workspace_target, path_within) | `crates/nexus-types/src/workspace_paths.rs` (adapter HTTP in `mcp-core::projects`) | esistente |
| Esecuzione comandi git (run_git_command, GitCommandOptions) | `crates/nexus-types/src/git_exec.rs` (re-export in `mcp-core::projects`) | esistente |
| Reindex vettoriale post-mutazione (contratto tool agente) | trait `FileReindexer` in `nexus-agent-tools::context_core` (impl `NeuralFileReindexer` in mcp-core, delega a `reindex_single_file`) | esistente |
| Estrazione JSON da output LLM (Rust, paritetico ADR 0032) | `crates/nexus-types/src/llm_json.rs` (re-export in mcp-core via `crate::llm_json`) | esistente |
| Estensioni file di codice (CODE_EXTENSIONS) | `crates/nexus-types/src/code_files.rs` (re-export in `mcp-core::projects`) | esistente |
| Servizi AI del wiki (embed/completion/purpose) | trait `WikiAiServices` in `nexus-wiki::deps` (impl `AppStateWikiAi` in `mcp-core::wiki`, delega a NeuralCoreClient + internal_routing) | esistente |
| Identita' e dedup servizi di progetto (label generiche, similarity con split trattini, stop duplicati pre-spawn) | `mcp-core/src/agent_processes.rs` (`is_generic_service_label`, `similar_service_labels`, `stop_similar_running_services`); delegano: tool `run_service`, wizard install Windows, start/restart pannello, launch run config; visibilita' voci pannello Windows in `project_workspace/services.rs::visible_windows_services` | esistente |
| Secret scanner su stringa in-memory (scan, redazione totale, redazione context-preserving) | `nexus-tool-kit/src/secret_text_scanner.rs` (`SecretScanner::{scan, redact, redact_secrets_preserving_context}`); re-export gateway in `nexus-gateway::redaction::secret_scanner`; delegano: pipeline DLP gateway, `mcp-core::agent_processes::redact_secrets_for_persistence` (flush + lettura output processi, `terminal_ws`) | esistente |

### Enforcement

- `jscpd.json` + `scripts/dup-report.sh`: misura cross-linguaggio. Gate "ratchet":
  il numero di cloni puo' solo SCENDERE rispetto a `.dup-baseline.json`. La baseline
  si riallinea al ribasso (`--update-baseline`) dopo ogni wave che riduce il debito,
  mai al rialzo.
- `scripts/check-single-source.sh`: guard testuale che blocca una nuova definizione
  di un punto unico fuori dal suo modulo. I check si attivano per wave.
- `clippy.toml`: punto di config Rust (la dup vera la copre jscpd; clippy non ha
  copy-paste detection in stable).
- `docs/tech-debt-dup.md`: metrica e baseline.
- Innesto: `lefthook.yml` (pre-commit veloce) e `.github/workflows/verify.yml` (gate completo).

## Procedura "prima di scrivere logica che decide"

1. Cerca il concern nel catalogo. Se esiste, **delega** al punto unico.
2. Se e' un concern trasversale nuovo, crea PRIMA il punto unico col meccanismo
   corretto (tabella sopra) e aggiungilo al catalogo.
3. Mai copiare-e-adattare una funzione esistente.

## Definition of Done anti-duplicazione

Un PR che tocca un concern del catalogo deve: passare `scripts/dup-report.sh` senza
aumento di cloni; passare `scripts/check-single-source.sh`; se introduce un nuovo
punto unico, registrarlo in questo ADR e attivare il relativo check.

## Conseguenze

- Positive: divergenza silenziosa impedita strutturalmente; un solo posto da
  modificare per concern; debito misurabile e monotono decrescente.
- Costo: convergenza dei call site a blocchi (il punto unico convive col vecchio
  finche' tutti migrano); falsi positivi jscpd gestiti via `ignore` in `jscpd.json`.
- Cambi di comportamento osservabile (es. semantica errori di `get_setting`) dietro
  flag in `settings` (regola G), con test che cattura la regressione (regola H).

## Roadmap

Wave 0 (questo ADR + tooling) -> 1 (`parse_user_id`) -> 2 (`nexus-cache`) ->
7 (frontend) -> 3 (`get_setting`) -> 4 (capability) -> 6 (Python utils) ->
5 (health/seeding) -> 8 (cross-language). Dettaglio operativo nel piano di campagna.

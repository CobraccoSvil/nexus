---
id: 0021-sql-injection-detector-unificato
kind: adr
title: "SQL injection detector unificato — un solo percorso, zero falsi positivi su DDL"
slug: 0021-sql-injection-detector-unificato
tags:
  - architecture
  - security
  - scanner
  - quality
  - false-positive
  - unification
auto_generated: false
created_at: 2026-06-04T20:00:00Z
updated_at: 2026-07-02T00:00:00Z
nexus_meta_version: 1
---

# ADR 0021 — SQL injection detector unificato

> **Status**: implementato (verificato 2026-07-02)
> **Aggiornamento 2026-07-02 (as-built)**: `crates/mcp-quality/src/injection.rs` e' il punto unico del detector; `mcp-db` delega; `nexus-tool-kit/sec_sql_injection_check` delega.
> **Decisori**: team Nexus
> **Principio cardine**: stesso dell'ADR 0017 — un sistema unico, non duplicato. Un solo detector di SQL injection, condiviso tra scanner del pannello "Ottimizzazione" e tool MCP on-demand. La injection si cerca dove davvero esiste (codice applicativo che costruisce query), non nei file `.sql` statici.
> **Trigger**: incident del 04/06/2026 su Beauty-Book. Il pannello Ottimizzazione mostra 6 falsi positivi "Potential SQL injection (high)" su `backend/migrations/001_init_schema.sql`, un file DDL puro. Causa: un hash bcrypt `$2a$10$...` nella riga di seed viene scambiato per interpolazione di variabile.

## Contesto: due percorsi divergenti

Nexus ha **due implementazioni separate** della stessa idea (rilevare SQL injection), nate in momenti diversi e mai unificate:

| | Percorso A — scanner pannello | Percorso B — tool MCP agente |
|---|---|---|
| Codice | `crates/mcp-db/src/lib.rs::check_injection_patterns` | `crates/mcp-core/src/nexus_tools/sec_sql_injection_check.rs` |
| Invocato da | `projects/quality.rs:384` (scan automatico file `.sql`), `quality.rs:872`, `agent_tools/quality_tools.rs:26` | agente AI on-demand (`nexus_sec_sql_injection_check`) |
| Input | testo SQL grezzo di file `.sql` | file di codice (Rust) |
| Strategia | regex `\$\{?\w+\}?`, `+`, `format!` sul testo SQL | substring `format!("SELECT`, `format!("INSERT`, ecc. |
| Esito | **buggato** (6 falsi positivi sull'incident) | **corretto** |

### Perche' esistono entrambi

- **Percorso A** (`mcp-db`) e' nato per analizzare la **qualita' semantica dello SQL** nei file `.sql` (N+1, `SELECT *`, `WHERE` mancante, `DISTINCT` inutile) usando un vero parser (`parse_sql_multidialect`). Ci e' stato poi aggiunto un check injection con regex testuali — il pezzo difettoso.
- **Percorso B** (`nexus_tools`) e' un tool on-demand che cerca injection nel posto concettualmente corretto: il **codice applicativo** che costruisce query via `format!`/concatenazione.

### Errore concettuale di fondo

La SQL injection e' un difetto del **codice che COSTRUISCE la query**, non del file `.sql` statico. Un `CREATE TABLE` o un `INSERT` con valori letterali non puo' avere injection: non c'e' input utente concatenato. **Il Percorso A cerca injection nel posto sbagliato.**

## I due bug concreti del Percorso A

### Bug 1 — Check eseguito N volte sull'intero file (`mcp-db/src/lib.rs:94-103`)

```rust
for stmt in &statements {                          // N statement nel file
    ...
    all_findings.extend(check_injection_patterns(sql));  // <-- passa SEMPRE `sql` intero
}
```

`check_injection_patterns` riceve il file completo `sql`, non il singolo `stmt`. Con 6 statement (5 `CREATE TABLE` + 1 `INSERT`) il check gira 6 volte sull'intero file → **6 finding identici**. Spiega esattamente i 6 duplicati del pannello.

### Bug 2 — Regex matcha SQL legittimo (`mcp-db/src/lib.rs:284`)

```rust
let interp_re = Regex::new(r#"\$\{?\w+\}?"#).unwrap();
```

Pensata per template literal JS (`${var}`) e shell (`$var`). Applicata a SQL matcha tre pattern **tutti sicuri**:

1. **Hash bcrypt** `$2a$10$dummyhashfortestingonly` (riga 59 del file incident) → 3 match
2. **Parametri posizionali PostgreSQL** `$1`, `$2` — che sono **il modo corretto e sicuro** di parametrizzare. Ironia: lo scanner segnala come injection proprio la difesa contro l'injection.
3. **Dollar-quoting** `$tag$...$tag$` (PL/pgSQL) — gia' parzialmente gestito per `$$...$$` ma non per tag nominati.

## Decisione

1. **Rimuovere `check_injection_patterns` dal Percorso A.** I file `.sql` statici non possono avere injection. `mcp-db` resta per cio' che fa bene (N+1, SELECT *, WHERE mancante, DB-first) via parser.
2. **Creare un detector unico** `injection_detector` riutilizzabile che cerca interpolazione/concatenazione nel **codice applicativo** (Rust, Python, TypeScript/JS) che costruisce query SQL.
3. **Integrare il detector in `mcp-quality::analyze_source`** (che gia' gira sui file `.rs`/`.py`/`.ts` nello scanner del pannello) come nuovo analyzer `check_sql_injection_in_code`. Cosi' il pannello Ottimizzazione segnala injection dove serve davvero.
4. **Il tool MCP `sec_sql_injection_check` usa lo stesso detector.** Zero duplicazione.

```
        ┌──────────────────────────────────────┐
        │   injection_detector (unico)         │
        │   - detect(lang, source) -> findings │
        │   - keyword SQL + interpolazione +   │
        │     input non-costante               │
        │   - whitelist: query parametrizzate  │
        │     ($1, ?, :name), sqlx::query!     │
        └──────────────┬───────────────────────┘
                       │
        ┌──────────────┴──────────────┐
        │                             │
┌───────▼─────────┐          ┌────────▼──────────┐
│ mcp-quality     │          │ nexus_tools       │
│ analyze_source  │          │ sec_sql_injection │
│ (scanner pannel)│          │ (tool MCP agente) │
│ su .rs/.py/.ts  │          │ on-demand         │
└─────────────────┘          └───────────────────┘

mcp-db::analyze_query  -->  NON fa piu' injection check
(solo qualita' semantica SQL via parser)
```

## Il detector unificato

### Dove vive

Nuovo modulo `crates/mcp-quality/src/injection.rs` (mcp-quality e' gia' dipendenza sia dello scanner sia raggiungibile dai tool). In alternativa, se serve indipendenza, un micro-crate `crates/sql-injection-detector/`. Preferenza: dentro `mcp-quality` per minimizzare le dipendenze nuove.

### API

```rust
pub struct InjectionFinding {
    pub line: usize,
    pub severity: String,   // "high" se input chiaramente esterno, "medium" se incerto
    pub snippet: String,
    pub detail: String,
}

/// Rileva costruzione non sicura di query SQL nel codice applicativo.
pub fn detect_sql_injection(file_path: &str, source: &str) -> Vec<InjectionFinding>;
```

### Logica di detection (per linguaggio)

Il detector cerca il pattern: **costruzione di una stringa SQL** (keyword SQL presente) + **interpolazione/concatenazione di un valore non-costante**.

**Rust**:
- `format!("...SELECT|INSERT|UPDATE|DELETE|WHERE...{}...", var)` con almeno un `{}`/`{var}` e una keyword SQL → high
- `"...SELECT...".to_string() + &var` o `query.push_str(&var)` → high
- **Whitelist (NON segnalare)**: `sqlx::query!`, `sqlx::query_as!` (macro compile-checked), `.bind(...)`, query con placeholder `$1`/`$2`

**Python**:
- f-string con keyword SQL e `{var}`: `f"SELECT ... {user_input}"` → high
- `"SELECT ..." % var`, `"SELECT ..." + var`, `.format(...)` con keyword SQL → high
- **Whitelist**: `cursor.execute(sql, params)` con tuple/dict params, `?`/`:name`/`%s` placeholder

**TypeScript/JS**:
- template literal con keyword SQL e `${var}`: `` `SELECT ... ${userInput}` `` → high
- concatenazione `"SELECT ..." + var` → high
- **Whitelist**: query parametrizzate (`$1`, `?`), ORM builder (Prisma, Drizzle, Knex `.where()`), tagged template `sql\`...\`` di librerie safe (postgres.js)

### Riduzione falsi positivi (cuore dell'ADR)

1. **Mai sui file `.sql`**: il detector gira solo su file di codice. `mcp-db` non fa piu' injection check.
2. **Keyword SQL obbligatoria**: serve almeno una tra `SELECT|INSERT|UPDATE|DELETE|FROM|WHERE|JOIN|VALUES` nella stringa, altrimenti non e' una query.
3. **Interpolazione di valore non-costante**: `format!("SELECT 1")` senza placeholder non e' injection. Serve `{var}`/`${var}`/`+ var`/`% var`.
4. **Whitelist parametrizzazione**: `$1..$n`, `?`, `:name`, `%s`, `.bind()`, macro `sqlx::query!` → sicuri, mai segnalati.
5. **Severity graduata**: `high` se la variabile interpolata ha nome che suggerisce input esterno (`user`, `input`, `param`, `req`, `body`, `query`, `arg`); `medium` altrimenti.

## Modifiche al codice

### A. `crates/mcp-db/src/lib.rs`

- **Rimuovere** `check_injection_patterns` (funzione + chiamata nel loop riga 101 + le 3 regex).
- `analyze_query` continua a fare: `check_select_star`, `check_missing_where`, `check_n_plus_one_hints`, `check_performance_issues`, `check_db_first_principles`.
- Aggiornare i test che si aspettavano injection finding sui file `.sql` (rimuoverli o convertirli).

### B. `crates/mcp-quality/src/injection.rs` (nuovo)

- `detect_sql_injection(file_path, source)` con la logica sopra.
- Test unit estesi: i casi che PRIMA erano falsi positivi devono ora dare zero finding:
  - file `.sql` con hash bcrypt → mai chiamato (il detector non gira sui .sql)
  - `sqlx::query!("SELECT ... WHERE id = $1", id)` → 0 finding (parametrizzato)
  - `format!("SELECT * FROM users WHERE name = '{}'", user_input)` → 1 finding high

### C. `crates/mcp-quality/src/lib.rs`

- Aggiungere `findings.extend(injection::detect_sql_injection(file_path, source).into_iter().map(...))` in `analyze_source` (dopo gli altri analyzer, riga ~140).
- Mappare `InjectionFinding` su `QualityFinding` con `category="security"`.

### D. `crates/mcp-core/src/nexus_tools/sec_sql_injection_check.rs`

- Sostituire `scan_substrings` con un loop sui file di codice del progetto che chiama `mcp_quality::injection::detect_sql_injection`.
- Output retro-compatibile (mantiene `interpolated_total`, `parameterized_total`, `warning`) ma derivato dal detector unico.

### E. `crates/mcp-core/src/projects/quality.rs`

- Il ramo `if file_path.ends_with(".sql")` continua a usare `mcp_db::analyze_query` (ora senza injection check).
- Il ramo `else` usa `mcp_quality::analyze_source` (ora CON injection check) — nessuna modifica al call site, il detector e' dentro `analyze_source`.

## Migrazione DB

Nessuna. E' un fix di codice puro. Opzionale: una migrazione `0314_injection_detector_settings.sql` con:

```
agent.scanner.sql_injection_enabled = true
agent.scanner.sql_injection_min_severity = medium
```

per consentire all'admin di disattivare/tarare il detector (regola G). Default: enabled, medium.

## Effetto sull'incident Beauty-Book

| Finding attuale | Dopo ADR 0021 |
|---|---|
| 6x "Potential SQL injection (high)" su `001_init_schema.sql` | **0** (detector non gira sui `.sql`; l'hash bcrypt non e' piu' scansionato) |
| 1x complessita' (BookingPage) | invariato (reale) |
| 14x manutenibilita' | invariato (da valutare separatamente, ma non piu' inquinati dai 6 FP security) |

Il pannello passa da 21 a ~15 problemi, con la categoria "security" che scende da 6 (tutti falsi) a 0 falsi. Se in futuro un progetto avra' codice Rust/Python/TS con `format!("SELECT...{}", user)`, QUELLO verra' correttamente segnalato.

## Metriche di Done

- ✅ `check_injection_patterns` rimosso da `mcp-db`
- ✅ `detect_sql_injection` in `mcp-quality` con >=10 test (5 veri positivi su .rs/.py/.ts, 5 veri negativi inclusi parametrizzati)
- ✅ `analyze_source` integra il detector
- ✅ Tool MCP `sec_sql_injection_check` usa il detector unico
- ✅ Scan di Beauty-Book: 0 finding security su file `.sql`
- ✅ Scan di Nexus stesso (codebase Rust con sqlx ovunque): 0 falsi positivi su `sqlx::query!`/`.bind()`/`$1`
- ✅ Un test E2E ricostruisce il file incident (`001_init_schema.sql` con hash bcrypt) → 0 security finding
- ✅ `cargo check --workspace` + `cargo clippy --workspace -D warnings` + `pnpm verify` verdi

## Rischi e mitigazioni

| Rischio | Mitigazione |
|---|---|
| Falsi negativi nuovi (injection reale non rilevata perche' detector troppo conservativo) | Keyword SQL + interpolazione e' il pattern minimo di ogni injection reale; whitelist solo su parametrizzazione provata |
| Codice che costruisce SQL dinamico legittimo (es. query builder custom) segnalato | Severity `medium` quando la variabile non sembra input esterno; admin puo' tarare via settings |
| Detector lento su file grandi | Line-based scan O(n), come gli altri analyzer di mcp-quality |
| sqlx con stringa non-macro (`sqlx::query(&format!(...))`) | Questo E' un vero rischio injection: il detector lo segnala correttamente (format! con keyword SQL), e fa bene |

## Cosa NON facciamo

- ❌ **Mantenere injection check sui file `.sql`**. E' concettualmente sbagliato. Rimosso, non "tarato".
- ❌ **Taint analysis completa** (tracciare il flusso dell'input dal request handler alla query). Eccesso: il pattern "keyword SQL + interpolazione non-parametrizzata" copre il 95% dei casi reali con O(n).
- ❌ **Tre detector per tre linguaggi**. Un solo `detect_sql_injection` con branch interni per linguaggio, API unica.
- ❌ **Bloccare il commit su finding injection**. Resta segnalazione nel pannello, non gate (per ora).

## Riferimenti

- [[0017-knowledge-graph-parita]] ADR 0017 (stesso principio: un sistema unico)
- Incident Beauty-Book 04/06/2026 — 6 FP su `backend/migrations/001_init_schema.sql`
- `crates/mcp-db/src/lib.rs:280-303` (codice da rimuovere)
- `crates/mcp-quality/src/lib.rs:110` (`analyze_source` da estendere)
- `crates/mcp-core/src/nexus_tools/sec_sql_injection_check.rs` (da convergere)

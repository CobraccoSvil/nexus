---
id: 0019-file-picking-robusto-verify-chain
kind: adr
title: "Robustezza file picking + verify chain coder agent"
slug: 0019-file-picking-robusto-verify-chain
tags:
  - architecture
  - agent
  - coder
  - filesystem
  - verify
  - error-fix
auto_generated: false
created_at: 2026-06-04T19:00:00Z
updated_at: 2026-06-04T19:00:00Z
nexus_meta_version: 1
---

# ADR 0019 — Robustezza file picking + verify chain coder agent

> **Status**: proposto
> **Decisori**: team Nexus
> **Correlato a**: [[0017-segnali-strutturali-vs-euristiche-testuali]] (ADR 0018)
> **Trigger**: incident M44 del 04/06/2026 sul progetto Beauty-Book — 3 modelli in escalation (DeepSeek v4-pro, Gemini 2.5 Pro, DeepSeek di nuovo) hanno fallito un fix di complessità ciclomatica creando componenti in `figma_export/` (directory export Figma, NON build graph). Test "0/0 passati" reali; refactor orfano; nessuna verifica eseguita.

## Contesto: anatomia del fallimento

### Cosa è successo

1. Errore segnalato a Nexus: `BookingPage.tsx` complessità 25 (> 10) + `playwright_test 0/0 test passati`.
2. Nel repo Beauty-Book esistono **due file omonimi**:
   - `src/app/pages/BookingPage.tsx` (1006 righe, monolitico, nel build graph — `tsconfig.json` ha `include: ["src"]`)
   - `figma_export/src/app/pages/BookingPage.tsx` (export Figma → React, NON nel build graph)
3. DeepSeek ha aperto il secondo. Ha estratto hook `useBookingInitialization`, creato 5 sotto-componenti in `figma_export/src/app/components/booking/`. Ha riscritto il file orfano.
4. Gemini 2.5 Pro ha continuato sulla stessa traccia: altri 2 componenti, altra riscrittura. Sempre in `figma_export/`.
5. DeepSeek terzo turno ha dichiarato: *"non dispongo di un tool `run_playwright_tests`"* e si è fermato senza verifica.
6. Nexus ha emesso il messaggio "Modello non risponde con azione dopo 3 tentativi" e ha chiuso il run.

### Verifica esterna (Claude Code, 04/06 sera)

In ~5 minuti:

```bash
find . -name BookingPage.tsx        # 2 risultati: src/app/ + figma_export/
cat tsconfig.json | grep include    # ["src"] — figma_export fuori
npx tsc --noEmit | grep BookingPage  # 8 errori type-only nel file VERO
```

Gli 8 errori (unused imports, null-check mancante) erano la vera causa del `0/0`: il preflight `tsc` del tool `run_playwright_tests` falliva e ritornava `0 test eseguiti`. Fix: 5 Edit chirurgici, 1 file modificato → 22 test eseguiti (12 pass, 10 fail per spec divergenti dalla UI, fuori scope).

### 4 root cause distinte

| # | Root cause | Componente Nexus colpevole |
|---|---|---|
| RC1 | **File picking cieco**: ha scelto un file plausibile per nome senza verificare che sia nel build graph (chi lo importa?) | `agent_tools/filesystem.rs` `write_file`/`edit_file` |
| RC2 | **Convenzione directory non-produttive non riconosciuta** (`figma_export/`, `design/`, `archive/`) | Sistema prompt coder + assenza policy DB |
| RC3 | **Verify loop rotto**: il modello ha dichiarato "no tool" e si è arreso. Mai chiamato `tsc`, `pnpm build`, `pnpm lint` come fallback | System prompt coder + workflow M44 |
| RC4 | **Tool discovery fragile**: `run_playwright_tests` esiste in `NexusToolCatalog` (351 tool) ma il modello non l'ha trovato/usato — possibili cause: filtro routing matrix, allucinazione provider, naming mismatch | `prompt_templates.rs` + tool catalog injection |

E una **RC5** secondaria sul workflow:

| RC5 | **Escalation cieca**: dopo 3 fail Nexus si limita a chiudere il run con "riformula". Non spawna un sub-agent diagnostico che indaghi *perché* i 3 hanno fallito | `nexus-orchestrator` escalation logic |

## Decisione

Implementare un **preflight robusto** per ogni operazione di scrittura su file di codice + una **verify chain** automatica + una **directory policy** DB-driven + una **diagnostica di escalation** post-3-fail.

I 5 layer:

```
┌──────────────────────────────────────────────────────────────────────┐
│  L1 — File picking preflight (build graph awareness)                 │
│  Prima di write_file/edit_file su .ts/.tsx/.js/.jsx/.rs/.py/.go:     │
│   - grep_imports_of(path) -> set di file che importano questo path   │
│   - se 0 import AND path non in entrypoint registry -> WARN + chiedi │
│     conferma esplicita all'agente (richiesta_conferma_orfano=true)   │
│   - se policy del progetto e' deny per quel pattern -> errore        │
├──────────────────────────────────────────────────────────────────────┤
│  L2 — Directory policy DB-driven                                     │
│  Tabella nexus_project_directory_policies:                           │
│   (project_id, path_glob, kind, behavior, note)                      │
│   - kind in {production, design, mockup, archive, legacy, generated} │
│   - behavior in {write_allowed, warn, deny}                          │
│  Default seeding per ogni nuovo progetto (mig 0306):                 │
│   - figma_export/**, design/**, mockups/** -> kind=design, warn     │
│   - archive/**, legacy/**, .backup/** -> kind=archive, warn         │
│   - generated/**, *.generated.*, node_modules/** -> kind=generated, │
│     deny (default node_modules e' gia' bloccato)                    │
├──────────────────────────────────────────────────────────────────────┤
│  L3 — Verify chain con escalation                                    │
│  Tool nexus_verify_change(scope?) che esegue in ordine:              │
│   1. typecheck del linguaggio principale (tsc/cargo check/mypy)      │
│   2. build (next build/cargo build/pnpm build)                       │
│   3. lint (eslint/clippy/ruff)                                       │
│   4. test runner specifico (playwright/cargo test/pytest)            │
│   Esce al primo errore con report strutturato.                       │
│  Tool e direttiva sostituiscono il "non ho tool" come escape route.  │
├──────────────────────────────────────────────────────────────────────┤
│  L4 — Tool catalog discovery hint                                    │
│  Per ogni agent run a intent debug/code/test, injettare nel system   │
│  prompt una <tool_discovery> con regex-hints:                        │
│   - "verifica" -> nexus_verify_change, *_test, *_check               │
│   - "esegui test" -> *playwright*, *cypress*, *pytest*, *cargo_test* │
│  Cosi' il modello che vuole eseguire test trova il tool anche se     │
│  ha nome leggermente diverso dal previsto.                           │
├──────────────────────────────────────────────────────────────────────┤
│  L5 — Diagnostica escalation (post 3 fail)                           │
│  Quando il workflow rileva 3+ tentativi senza azione verificata:     │
│   - spawn un sub-agent "diagnostico" con system prompt dedicato      │
│   - input: meta-step + file_paths toccati + ultimo error code        │
│   - missione: dire all'utente la causa semantica, non il sintomo     │
│  Output mostrato all'utente al posto di "riformula richiesta".       │
└──────────────────────────────────────────────────────────────────────┘
```

## Implementazione dettagliata

### L1 — File picking preflight

**File**: `crates/mcp-core/src/agent_tools/filesystem.rs` (o equivalente — cerca dove sono `write_file`/`edit_file`/`str_replace_editor`).

```rust
pub async fn preflight_file_in_build_graph(
    project_root: &Path,
    file_path: &Path,
) -> Result<BuildGraphStatus> {
    // Extension non-codice: salta il check
    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if !matches!(ext, "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "rs" | "py" | "go" | "rb") {
        return Ok(BuildGraphStatus::NotCodeFile);
    }
    // Entrypoint conosciuti per linguaggio (estendibile via DB)
    let entrypoints = entrypoints_for_project(project_root).await?;
    if entrypoints.iter().any(|e| e == file_path) {
        return Ok(BuildGraphStatus::Entrypoint);
    }
    // Grep ricorsivo per import del file
    let rel = file_path.strip_prefix(project_root).unwrap_or(file_path);
    let stem = file_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let importers = grep_importers(project_root, stem, rel).await?;
    if importers.is_empty() {
        return Ok(BuildGraphStatus::Orphan { stem: stem.into() });
    }
    Ok(BuildGraphStatus::Imported { by: importers })
}

pub enum BuildGraphStatus {
    NotCodeFile,
    Entrypoint,
    Imported { by: Vec<PathBuf> },
    Orphan { stem: String },
}
```

Nel wrap di `write_file`/`edit_file`:

```rust
if let BuildGraphStatus::Orphan { stem } = preflight_file_in_build_graph(root, path).await? {
    // Check policy
    let policy = lookup_directory_policy(project_id, path).await?;
    if policy.behavior == "deny" {
        return Err(format!("file {} in directory '{}' (kind={}, behavior=deny): write rifiutato", path.display(), policy.path_glob, policy.kind));
    }
    // warn o write_allowed: ritorna warning nel result
    tool_result.warnings.push(format!(
        "ATTENZIONE: il file {} non e' importato da nessun altro file del progetto (orfano). \
         Verifica che sia davvero il file giusto da modificare. \
         Suggerimento: grep -r 'from .*{}' --include='*.{{ts,tsx}}' src/ per confermare.",
        path.display(), stem
    ));
}
```

Il warning DEVE essere visibile nel `tool_result` perche' il modello lo legga al turno successivo (regola D — fuori chat, prompt-only contract).

### L2 — Directory policy DB-driven

**Migrazione `db/migrations/0306_project_directory_policies.sql`**:

```sql
CREATE TABLE nexus_project_directory_policies (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id  UUID REFERENCES projects(id) ON DELETE CASCADE,
    -- NULL = policy globale (vale per tutti i progetti)
    path_glob   TEXT NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN (
        'production','design','mockup','archive','legacy','generated','test'
    )),
    behavior    TEXT NOT NULL CHECK (behavior IN ('write_allowed','warn','deny')),
    note        TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_pdp_project ON nexus_project_directory_policies (project_id);
CREATE UNIQUE INDEX uq_pdp_glob ON nexus_project_directory_policies
  (COALESCE(project_id::text,''), path_glob);

-- Policy globali di default (project_id NULL)
INSERT INTO nexus_project_directory_policies (project_id, path_glob, kind, behavior, note) VALUES
  (NULL, 'figma_export/**',  'design',    'warn', 'Export Figma -> React, raramente production'),
  (NULL, 'design/**',        'design',    'warn', 'Sorgenti design, raramente production'),
  (NULL, 'mockups/**',       'mockup',    'warn', 'Mockup UI, mai production'),
  (NULL, 'archive/**',       'archive',   'warn', 'Archivio storico'),
  (NULL, 'legacy/**',        'legacy',    'warn', 'Codice legacy'),
  (NULL, '.backup/**',       'archive',   'deny', 'Backup, mai modificare'),
  (NULL, 'node_modules/**',  'generated', 'deny', 'Generato da package manager'),
  (NULL, 'target/**',        'generated', 'deny', 'Build output Rust'),
  (NULL, 'dist/**',          'generated', 'deny', 'Build output JS'),
  (NULL, 'build/**',         'generated', 'deny', 'Build output generico'),
  (NULL, '.next/**',         'generated', 'deny', 'Build output Next.js'),
  (NULL, '*.generated.*',    'generated', 'deny', 'File generati');
```

**API runtime**:

```rust
pub async fn lookup_directory_policy(
    db: &PgPool,
    project_id: Uuid,
    file_path: &Path,
) -> Result<Option<DirectoryPolicy>> {
    // Match per glob: prima project-specifico, poi globale
    // glob crate gia' in workspace
    let mut policies = sqlx::query_as::<_, DirectoryPolicy>(
        "SELECT * FROM nexus_project_directory_policies
         WHERE project_id = $1 OR project_id IS NULL
         ORDER BY (project_id IS NOT NULL) DESC, length(path_glob) DESC"
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    for p in policies {
        if glob_match(&p.path_glob, file_path) {
            return Ok(Some(p));
        }
    }
    Ok(None)
}
```

Cache 60s (regola G — DB-driven, no hardcode).

### L3 — Verify chain

**Nuovo tool MCP `nexus_verify_change`**:

```rust
pub async fn tool_nexus_verify_change(
    state: &AppState,
    project_id: Uuid,
    scope: VerifyScope, // Quick | Full | Typecheck | Lint | Test
) -> Result<VerifyReport> {
    let project = load_project(state, project_id).await?;
    let lang = detect_primary_language(&project.repository_root_path).await?;
    let mut report = VerifyReport::default();
    // Catena di check, esce al primo errore
    let steps: Vec<(&str, Box<dyn Fn() -> _ >)> = match lang {
        Language::Rust => vec![
            ("cargo check",  Box::new(|| run_cmd("cargo check --message-format=short"))),
            ("cargo clippy", Box::new(|| run_cmd("cargo clippy --all-targets -- -D warnings"))),
            ("cargo test",   Box::new(|| run_cmd("cargo test --no-fail-fast"))),
        ],
        Language::TypeScript | Language::JavaScript => {
            let pkg = read_package_json(&project.repository_root_path).await?;
            let mut v = vec![("tsc", Box::new(|| run_cmd("npx tsc --noEmit")))];
            if pkg.scripts.contains_key("lint") {
                v.push(("lint", Box::new(|| run_cmd("pnpm lint"))));
            }
            if pkg.scripts.contains_key("build") {
                v.push(("build", Box::new(|| run_cmd("pnpm build"))));
            }
            if pkg.devDependencies.contains_key("@playwright/test") {
                v.push(("playwright", Box::new(|| run_playwright_via_nexus(project_id))));
            }
            v
        },
        Language::Python => vec![
            ("mypy", Box::new(|| run_cmd("mypy ."))),
            ("ruff", Box::new(|| run_cmd("ruff check ."))),
            ("pytest", Box::new(|| run_cmd("pytest -x"))),
        ],
        _ => vec![],
    };
    for (name, run) in steps {
        let t0 = Instant::now();
        let res = run().await;
        report.steps.push(VerifyStep {
            name: name.into(),
            status: if res.is_ok() { "ok" } else { "fail" },
            elapsed_ms: t0.elapsed().as_millis() as u64,
            stdout_tail: res.as_ref().map(|r| tail(&r.stdout, 1000)).unwrap_or_default(),
            stderr_tail: res.as_ref().err().map(|e| tail(&e.to_string(), 2000)).unwrap_or_default(),
        });
        if res.is_err() { break; }
    }
    Ok(report)
}
```

Esposto a MCP tool catalog con descrizione esplicita per agenti:

```
nexus_verify_change: esegue la catena di verifica del progetto attivo
(typecheck -> lint -> build -> test). USA QUESTO STRUMENTO ogni volta
che hai applicato una modifica di codice e devi confermare che funziona.
NON dichiarare "non ho tool per verificare" se questo strumento e' disponibile.
```

### L4 — Tool discovery hint nel system prompt

Aggiungere a `agent.coder.base` (migrazione `0307_coder_tool_discovery_hint.sql` con UPSERT idempotente):

```xml
<tool_discovery>
Quando hai bisogno di:
- ESEGUIRE TEST -> cerca nel tool catalog regex: (run|exec).*test, playwright,
  cypress, pytest, cargo_test, jest. Sicuro non c'e' niente che faccia al caso tuo?
  Allora USA nexus_verify_change scope='test' che orchestra il runner del progetto.
- VERIFICARE COMPILAZIONE -> tsc, cargo_check, build, mypy. Oppure nexus_verify_change
  scope='typecheck'.
- VERIFICARE LINT -> eslint, clippy, ruff. Oppure nexus_verify_change scope='lint'.
- VERIFICARE TUTTO PRIMA DI DICHIARARE OK -> nexus_verify_change scope='full'.

NON dichiarare "non dispongo di tool per X" se non hai fatto una ricerca regex
nel tool catalog. Se davvero manca, dichiaralo solo dopo aver provato
nexus_verify_change come fallback.
</tool_discovery>

<file_picking_policy>
Prima di scrivere o modificare un file di codice (.ts, .tsx, .js, .jsx, .rs,
.py, .go) ESEGUI sempre questo controllo:

1. grep ricorsivo per import di questo file dal codice del progetto:
   grep -r "from .*<stem>" src/ packages/ apps/ --include='*.ts'
   (adattabile per il linguaggio)
2. Se 0 risultati E il file non e' un entrypoint conosciuto (App.tsx, main.rs,
   __init__.py, index.ts, ecc.) -> il file e' ORFANO. Non modificarlo finche'
   non hai certezza che sia davvero quello giusto.
3. Verifica i tsconfig/Cargo.toml/setup.py per capire quale directory e' nel
   build graph. Esempio: tsconfig.json con `include: ["src"]` esclude tutto
   quello che e' fuori da src/.

Nexus emette WARNING automatici via tool_result quando rileva un file orfano.
LEGGILI. Se appare un warning di file orfano, NON proseguire la modifica:
ricerca prima il file corretto.
</file_picking_policy>
```

### L5 — Diagnostica escalation post-3-fail

**File**: `crates/nexus-orchestrator/src/escalation.rs` (o equivalente — cerca dove vive la logica di escalation).

Quando il workflow rileva: 3+ tool call consecutive senza che la verifica sia mai stata eseguita (nessun `nexus_verify_change`/`*_test`/`*_check` chiamato con esito ok), spawn diagnostic agent:

```rust
pub async fn spawn_diagnostic_agent_on_escalation(
    state: &AppState,
    run_id: Uuid,
    meta_steps: &[MetaStep],
    files_touched: &[PathBuf],
) -> Result<DiagnosticReport> {
    let prompt = render_template(state, "agent.diagnostic.escalation", json!({
        "meta_steps_summary": summarize(meta_steps),
        "files_touched": files_touched,
        "build_graph_status": check_files_in_build_graph(files_touched).await?,
        "verify_attempts": count_verify_attempts(meta_steps),
    })).await?;
    let result = invoke_purpose_model(state, "diagnostic_escalation", &prompt).await?;
    parse_diagnostic_report(&result)
}
```

Prompt template `agent.diagnostic.escalation` (XML strutturato, regola D):

```xml
<role>
Sei un diagnostico di processo agentico. Analizzi una sequenza di tool call
fallite e identifichi la causa semantica del fallimento.
</role>
<contesto>
Un run agentico ha fatto N (>= 3) tool call senza riuscire a verificare il
proprio output. Hai accesso a: meta-step summary, file toccati, esito grep
build-graph dei file, conteggio tentativi di verify.
</contesto>
<analisi>
1. I file toccati sono nel build graph? Se no, l'agente ha lavorato in posti
   orfani.
2. L'agente ha chiamato un tool di verify? Se no, ha applicato modifiche
   alla cieca.
3. I tool falliti hanno un pattern (es. tutti file_write su stessa directory)?
   Suggerisce confusione di file picking.
</analisi>
<output_format>
JSON SOLO:
{
  "root_cause": "<una frase>",
  "evidence": [<lista breve>],
  "user_message": "<spiegazione 2-4 frasi al utente, italiano>",
  "next_action_suggested": "<azione concreta>"
}
</output_format>
```

Output mostrato all'utente al posto di "riformula richiesta", con un tag chiaro `[diagnostica automatica Nexus]`.

## Migrazioni DB

| Mig | Cosa |
|---|---|
| `0306_project_directory_policies.sql` | Tabella + 12 policy globali default |
| `0307_coder_tool_discovery_hint.sql` | UPSERT `nexus_prompt_templates['agent.coder.base']` aggiungendo blocchi `<tool_discovery>` e `<file_picking_policy>` |
| `0308_diagnostic_escalation_prompt.sql` | UPSERT `nexus_prompt_templates['agent.diagnostic.escalation']` + `nexus_purpose_model['diagnostic_escalation'] = google/gemini-2.5-flash-lite` |
| `0309_verify_chain_settings.sql` | Settings agent.verify.* (timeout per step, max parallel, cache 60s) |

## File backend da creare/modificare

| File | Tipo modifica |
|---|---|
| `crates/mcp-core/src/agent_tools/filesystem.rs` | Aggiungere preflight `preflight_file_in_build_graph` + `lookup_directory_policy` nei wrap di write/edit |
| `crates/mcp-core/src/agent_tools/verify.rs` (nuovo) | Tool `nexus_verify_change` con catena tsc/cargo check/clippy/build/test |
| `crates/mcp-core/src/directory_policy.rs` (nuovo) | Cache 60s + lookup glob |
| `crates/nexus-orchestrator/src/escalation.rs` | Hook post-3-fail con spawn diagnostic agent |
| Tool catalog injection (in `prompt_templates.rs` o equivalente) | Aggiungere descrizione esplicita di `nexus_verify_change` |

## Metriche di Done

- ✅ Mig 0306-0309 applicate, settings + policy popolate
- ✅ `preflight_file_in_build_graph` emette warning su file orfano (test E2E con file in `figma_export/`)
- ✅ `lookup_directory_policy` blocca write in `node_modules/` (deny) e avvisa in `figma_export/` (warn)
- ✅ `nexus_verify_change` esegue catena su progetto TS (tsc → lint → build → playwright) in <2min
- ✅ Diagnostic escalation prodice JSON parseable in <30s sui 3 fail
- ✅ Test E2E ricostruisce lo scenario M44 originale (BookingPage in figma_export/): la nuova pipeline emette warning + l'agente sceglie il file giusto
- ✅ `cargo check --workspace` + `pnpm verify` verdi
- ✅ ADR documentato in vault meta + presente in DB `wiki_docs`

## Rischi e mitigazioni

| Rischio | Mitigazione |
|---|---|
| Grep `import` ricorsivo lento su monorepo grandi | Limita ricerca a `src/`, `apps/`, `packages/`, `crates/` (whitelist directory di codice); usa ripgrep (`rg`) non `grep`; cache risultati per 30s per (file, project_id) |
| Glob match falso positivo (es. `figma_export.config.ts` matchato da `figma_export/**`) | Usa la lib `glob` di Rust con sintassi standard, non substring match |
| Policy globale troppo aggressiva (es. blocca lavoro legittimo in `archive/`) | Tutte le policy default sono `warn`, mai `deny` per directory ambigue. `deny` solo per generated/build output. Admin override via UI |
| `nexus_verify_change` su monorepo grande timeout | Settings `agent.verify.timeout_per_step_secs` (default 120s); fail-fast su primo step rosso |
| Diagnostic agent costa LLM | `gemini-2.5-flash-lite` (~$0.0005 per diagnostica), un solo spawn per run, cap diurno settings |
| Agenti pre-esistenti ignorano `<tool_discovery>` nel prompt | Aggiungerlo al `system.nexus_base` (mig 0307) cosi' eredita tutti i system prompt |

## Cosa NON facciamo (regola H)

- ❌ **Negare write su `figma_export/` di default**. È una directory legittima per chi lavora con design import. Warning sì, deny no.
- ❌ **Forzare grep import su file non-code** (`.md`, `.json`, `.yaml`). Solo file di codice.
- ❌ **Implementare un static analyzer completo** (call graph, dead code detection). È fuori scope: il preflight è solo "qualcuno importa questo file?".
- ❌ **Riscrivere il tool catalog injection** per renderlo dinamico. La `<tool_discovery>` statica nel system prompt è sufficiente.
- ❌ **Cambiare il workflow M44** dal lato UI. Solo logica backend.

## Riferimenti

- Incident M44 Beauty-Book 04/06/2026 (run_id da identificare in `agent_runs`)
- `crates/mcp-core/src/agent_tools/` (modulo target principale)
- `nexus_prompt_templates` schema (mig 0086)
- `nexus_purpose_model` (mig 0102)
- [[0017-knowledge-graph-parita]] ADR 0017 (precedente, regola del file picking applicabile anche al meta-vault)

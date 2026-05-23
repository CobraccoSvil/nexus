// ═══════════════════════════════════════════════════════════════════════════
// meta_docs/generators/concepts.rs — Genera note concettuali per il vault
//
// Note di alto livello che spiegano funzionalmente e architetturalmente
// cosa fa Nexus. Auto-citano altre note del vault via wikilink `[[slug]]`
// per popolare il grafo.
//
// Le note sono semi-statiche (non auto-rigenerate ad ogni commit ma solo
// quando il file sorgente del generator cambia). Curabili manualmente:
// se l'utente edita un file generato, il watcher segna `auto_generated=false`
// e il generator non lo sovrascrive piu'.
// ═══════════════════════════════════════════════════════════════════════════

use super::{GeneratedDoc, MetaDocContext, MetaDocGenerator};
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;

pub struct ConceptsGenerator;

#[async_trait]
impl MetaDocGenerator for ConceptsGenerator {
    fn name(&self) -> &'static str {
        "concepts"
    }

    fn relevant_for(&self, _files: &[String]) -> bool {
        true
    }

    async fn generate(&self, _ctx: &MetaDocContext<'_>) -> Result<Vec<GeneratedDoc>> {
        let now = Utc::now();
        let mut docs = Vec::new();

        for c in concept_specs() {
            docs.push(GeneratedDoc {
                kind: c.kind.to_string(),
                title: c.title.to_string(),
                slug: c.slug.to_string(),
                body_md: c.body.to_string(),
                tags: c.tags.iter().map(|s| s.to_string()).collect(),
                source_files: vec!["crates/mcp-core/src/meta_docs/generators/concepts.rs".to_string()],
                source_commit: None,
                vault_file_path: format!("{}/{}.md", c.folder, c.slug),
                now,
            });
        }

        Ok(docs)
    }
}

struct ConceptSpec {
    kind: &'static str,
    folder: &'static str,
    slug: &'static str,
    title: &'static str,
    tags: &'static [&'static str],
    body: &'static str,
}

#[allow(clippy::too_many_lines)]
fn concept_specs() -> Vec<ConceptSpec> {
    vec![
        ConceptSpec {
            kind: "other",
            folder: "concepts",
            slug: "nexus-funzionale",
            title: "Cosa fa Nexus (funzionale)",
            tags: &["concept", "funzionale", "overview"],
            body: r#"# Cosa fa Nexus (vista funzionale)

Nexus e' una **piattaforma AI orchestrator multi-progetto** che aiuta sviluppatori e team a:

## Capacita' principali

- **Gestire molteplici progetti software** in un unico hub, con isolamento totale (codice, sessioni chat, credenziali, knowledge base).
- **Chattare con AI multi-provider** (OpenAI, Anthropic, Google, DeepSeek, Mistral) usando una matrice di routing che sceglie il modello migliore per ogni intento.
- **Eseguire agenti autonomi** (Coder, Tester, Reviewer, Architect, SecurityAuditor, ecc.) che leggono/modificano il codice del progetto via MCP tools.
- **Memorizzare la conoscenza del progetto** in una Knowledge Base auto-aggiornata, navigabile come vault Obsidian (vedi [[knowledge-base-funzionamento]]).
- **Documentare automaticamente il proprio codice** (meta-vault Nexus) con architettura, ADR, API, schema DB, changelog, decisioni estratte da chat.
- **Apprendere dagli outcomes** via Q-learning + feedback workers che migliorano routing e prompt nel tempo.

## Stakeholder

- **Utenti finali**: sviluppatori che vogliono un IDE web con AI integrata multi-progetto.
- **Team leader**: vogliono telemetria/billing/governance su uso AI.
- **Admin/DevOps**: gestiscono provider, policy, deploy.

## Casi d'uso tipici

1. **Onboarding rapido**: importa un repo Git, Nexus indicizza il codice e prepara KB.
2. **Implementazione feature**: chat porta avanti task multi-step con agenti (vedi [[change-drafter]]).
3. **Code review automatica**: SecurityAuditor + Reviewer analizzano PR.
4. **Doc auto-generata**: meta-vault (vedi [[meta-vault-architettura]]) si aggiorna ad ogni commit.

Vedi anche: [[nexus-architetturale]], [[knowledge-base-funzionamento]], [[multi-provider-routing]].
"#,
        },
        ConceptSpec {
            kind: "other",
            folder: "concepts",
            slug: "nexus-architetturale",
            title: "Architettura di Nexus (vista architetturale)",
            tags: &["concept", "architettura", "overview"],
            body: r#"# Architettura di Nexus

Sistema tri-layer fortemente disaccoppiato. Vedi [[overview]] per il diagramma a blocchi.

## Layer

### 1. Frontend (Next.js)

- **web-ide** (`apps/web-ide`): UI principale (chat, file editor, terminal, knowledge panel).
- **admin** (in `apps/web-ide/app/admin`): pannello amministrativo (settings, billing, orchestrator, meta-docs).
- **landing** (`apps/landing`): sito vetrina pubblico.

Vedi [[frontend-nextjs]].

### 2. Backend orchestrazione (Rust)

- **mcp-core** (`crates/mcp-core`): cuore HTTP/SSE, agent loop, MCP tools (350+).
- **nexus-orchestrator** (`crates/nexus-orchestrator`): scheduler + 14 worker (Q-learning, anomaly, profiling, ecc.).
- **microservizi** (`crates/mcp-ast`, `mcp-quality`, `mcp-comments`, ecc.): tool dedicati gRPC.

Vedi [[crates-rust]].

### 3. Brain (Python + FastAPI)

- **LangGraph**: state machine per conversazioni agente.
- **Provider abstraction**: gateway unificato verso tutti i provider AI.
- **Embedding service**: `sentence-transformers/all-MiniLM-L6-v2` (384 dim).

Vedi [[brain-python]].

## Decisioni fondanti

- [[adr-0001-provider-abstraction-layer]] - Provider abstraction multi-LLM.
- [[adr-0005-meta-docs-vault]] - Meta-vault Obsidian-compatible.
- [[routing-matrix]] - Nessun modello AI hardcoded.
- [[isolamento-progetti]] - Ogni progetto e' un mondo a se'.

## Persistenza

- **PostgreSQL** (porta 5433 dev, 5432 prod): tutte le tabelle stato.
- **Qdrant** (porta 6333): collection vettoriali per RAG e semantica.
- **Redis** (porta 6379): cache + pub/sub.

Vedi [[postgres-tables]], [[qdrant-collections]].
"#,
        },
        ConceptSpec {
            kind: "other",
            folder: "concepts",
            slug: "knowledge-base-funzionamento",
            title: "Knowledge Base per-progetto",
            tags: &["concept", "kb", "knowledge", "obsidian"],
            body: r#"# Knowledge Base per-progetto

Ogni progetto registrato in Nexus ha una **Knowledge Base auto-aggiornata** che cattura:

- **Note funzionali** create manualmente (Feature, Requirement, Decisione, Dominio, User Story, Architettura)
- **Note auto** create da ogni messaggio chat dell'utente (intent classificato da LLM)
- **Tag** aggregati da contenuti
- **Link automatici** tra note simili (via embedding Qdrant + soglia di similarita')

## Sincronizzazione vault Obsidian

Ogni progetto ha una cartella `.nexus/knowledge/` sincronizzata bidirezionalmente:
- **DB -> filesystem**: ogni nota viene scritta come file `.md` Obsidian-compatible
- **filesystem -> DB**: un file watcher rileva modifiche manuali (es. da Obsidian) e aggiorna DB

Vedi [[adr-0003-knowledge-base-obsidian-compat]].

## Struttura tabelle

- `project_knowledge_notes` - le note
- `project_knowledge_links` - relazioni tra note
- `project_knowledge_tags` - tag aggregati

Vedi [[postgres-tables]] per dettagli schema.

## Differenza con meta-vault

Il **meta-vault Nexus** (`docs/.nexus-vault/`) documenta NEXUS STESSO (architettura, ADR, runbook).
La **KB per-progetto** documenta UN SINGOLO PROGETTO gestito da Nexus.

Vedi [[meta-vault-architettura]].
"#,
        },
        ConceptSpec {
            kind: "other",
            folder: "concepts",
            slug: "meta-vault-architettura",
            title: "Meta-vault Nexus (questa stessa doc)",
            tags: &["concept", "meta-vault", "obsidian", "documentation"],
            body: r#"# Meta-vault Nexus

La documentazione del **progetto Nexus stesso** vive in `docs/.nexus-vault/` come vault Obsidian.

## Cosa contiene

- **architecture/** - mappa di crate Rust, moduli Python, app frontend
- **adr/** - decisioni di design (ADR)
- **api/** - endpoint REST, MCP tools, settings keys
- **schema/** - tabelle Postgres, migrazioni, Qdrant
- **runbook/** - deploy, troubleshooting, monitoring
- **changelog/** - entry auto da commit significativi
- **decisions/** - decisioni estratte da chat utente
- **concepts/** - note concettuali (questa nota stessa)

## Pipeline auto-update

1. Sviluppatore fa `git commit`
2. Hook lefthook `post-commit` chiama `POST /api/meta-docs/ingest-commit`
3. mcp-core dispatcia 6 generator in parallelo:
   - SchemaGenerator
   - ArchitectureGenerator
   - ApiGenerator
   - ChangelogGenerator (LLM significance)
   - DecisionExtractor (LLM su chat_messages)
   - ConceptsGenerator (questo)
4. Ogni generator produce 1+ note `.md`
5. Hash-based loop detection: skip se il contenuto non e' cambiato
6. File watcher bidirezionale per modifiche manuali in Obsidian

Vedi [[adr-0005-meta-docs-vault]] per design rationale.

## Tabelle correlate

- `nexus_meta_docs` - le note del meta-vault
- `nexus_meta_doc_links` - relazioni (auto da wikilink + semantic via embedding)
- `nexus_meta_doc_changes` - commit processati

Vedi [[postgres-tables]].
"#,
        },
        ConceptSpec {
            kind: "other",
            folder: "concepts",
            slug: "multi-provider-routing",
            title: "Routing multi-provider AI",
            tags: &["concept", "routing", "provider", "ai"],
            body: r#"# Routing multi-provider AI

Nessun nome modello AI e' hardcoded nel codice Nexus. La scelta di provider+modello viene fatta a runtime da un **routing layer** basato su tabelle DB.

## Tabelle chiave

- `nexus_routing_matrix` - mappa `(intent, behavior_mode) -> (provider, model_id)` per le richieste utente.
- `nexus_purpose_model` - mappa `purpose -> (provider, model_id)` per task interni (changelog_significance, decision_extractor, change_drafter, autofix_planner, embedding, ecc.).
- `nexus_provider_default_model` - fallback se non esiste mapping specifico.

## Cache Rust

`crates/mcp-core/src/routing_matrix.rs` mantiene una cache in memoria (TTL 60s) per evitare query DB ad ogni inferenza. Refresh automatico in background.

## Vantaggi

- **Switch provider on-the-fly**: cambi il mapping DB, niente redeploy.
- **A/B testing**: routing matrix supporta varianti per percentuale di traffico.
- **Cost optimization**: i Q-learning workers possono auto-promuovere modelli economici quando si dimostrano sufficienti.

Vedi [[adr-0001-provider-abstraction-layer]] e [[routing-matrix]].

## Behavior modes

- `bilanciata` (default)
- `veloce` (modelli economici/fast)
- `approfondita` (modelli top-tier)
- `economica` (cap di costo aggressivo)

L'utente sceglie via dropdown nel composer chat.
"#,
        },
        ConceptSpec {
            kind: "other",
            folder: "concepts",
            slug: "isolamento-progetti",
            title: "Isolamento tra progetti",
            tags: &["concept", "security", "isolation"],
            body: r#"# Isolamento tra progetti

Ogni progetto registrato in Nexus e' un **mondo a se'**: codice, chat, knowledge, credenziali, container Docker, services systemd.

## Regole assolute (vedi CLAUDE.md sezione E)

- **Scope al progetto attivo**: ogni operazione MCP/agent vive dentro `project_root` del run corrente.
- **Cleanup Docker filtrato**: vietato `docker stop $(docker ps -q)` o `docker system prune` globali. Permesso solo con `-f <compose-progetto>` o `--filter "label=com.docker.compose.project=<slug>"`.
- **Container `ideai-*` intoccabili**: `ideai-postgres-nexus-1`, `ideai-qdrant-1`, `ideai-redis-1`, `ideai-grafana-1`. Mai fermarli/rimuoverli.
- **Letture massive ricorsive vietate** fuori dalla root progetto.

## Implementazione

- Sandbox Docker per processi agente (`nexus-sandbox:latest`).
- `ensure_project_access(db, user_id, project_id)` su ogni endpoint sensibile.
- File watcher per-progetto separati (uno per `.nexus/knowledge/` di ogni progetto).
- Port allocator `nexus_port_allocations` per evitare conflitti tra progetti.

Vedi [[postgres-tables]], [[runbook]].
"#,
        },
        ConceptSpec {
            kind: "other",
            folder: "concepts",
            slug: "pattern-learning-worker",
            title: "Pattern LearningWorker (scheduler async)",
            tags: &["concept", "pattern", "rust", "async"],
            body: r#"# Pattern LearningWorker

Worker scheduler pattern usato in `crates/nexus-orchestrator/src/learning_loop.rs`.

## Trait

```rust
#[async_trait]
trait LearningWorker {
    fn name(&self) -> &'static str;
    fn trigger(&self) -> WorkerTrigger; // OnTaskComplete | Periodic | Both
    fn interval(&self) -> Duration;
    async fn run(&self, ctx: &LearningContext) -> Result<()>;
    fn enabled(&self) -> bool { true }
}
```

## Worker registrati

- **Reactive** (OnTaskComplete): `UltralearnWorker`, `AuditWorker`, `MetricsAggregationWorker`, `VersioningWorker`
- **Periodic**: `ProfilingWorker`, `AnomalyDetectionWorker`, `MemoryConsolidationWorker`, `CleanupWorker`, `SessionPersistenceWorker`, `QLearningReplayWorker`, `ReplicationWorker`, `ClusteringWorker`
- **Meta-vault**: `MetaDocsRefreshWorker`, `NexusAutoFixWorker`

## Aggiungere un nuovo worker

1. File `crates/nexus-orchestrator/src/workers/my_worker.rs`
2. Impl `LearningWorker`
3. Aggiungere a `workers/mod.rs`
4. Registrare in `nexus_bridge.rs` (`scheduler.register(...)`)

Vedi [[crates-rust]].
"#,
        },
        ConceptSpec {
            kind: "other",
            folder: "concepts",
            slug: "pattern-mcp-tool",
            title: "Pattern MCP tool (agent_tools)",
            tags: &["concept", "pattern", "mcp", "agent"],
            body: r#"# Pattern MCP tool

Gli **MCP tools** sono funzioni callable dall'agent loop (e da Claude Code via MCP server).

## Tool esistenti (350+ in `crates/mcp-core/src/agent_tools/`)

- **File**: `read_file`, `write_file`, `edit_file`, `delete_file`, `list_files`, `search_in_files`
- **Git**: `git_commit`, `git_push`, `git_pull`, `git_status`, `git_stage`
- **Service**: `run_service`, `list_active_services`, `read_service_output`
- **Testing**: `run_playwright_tests`, `run_lint_fix`
- **Nexus orchestration**: `nexus_subagent_*`, `nexus_todo_write`, `nexus_mcp_tool_*`
- **Sandbox**: `get_sandbox_config`
- **Dispatcher**: `dispatcher_emit_event`, `dispatcher_post_notification`

## Aggiungere un tool

1. `crates/mcp-core/src/agent_tools/my_tool.rs`
2. `pub async fn tool_my_tool(ctx: &AgentToolContext, input: &Value) -> String`
3. Esposto in `agent_tools/mod.rs`
4. Schema JSON in `AGENT_TOOLS_JSON`
5. Dispatcher case in `agent_loop.rs`

Vedi [[mcp-tools]] per la lista completa.
"#,
        },
        ConceptSpec {
            kind: "other",
            folder: "concepts",
            slug: "sub-agents-claude-code",
            title: "Sub-agenti Claude Code (.claude/agents/)",
            tags: &["concept", "claude-code", "agent", "ai"],
            body: r#"# Sub-agenti Claude Code

Set di 7 sub-agenti specializzati registrati in `.claude/agents/*.md`. Vengono spawnati automaticamente da Claude Code quando la richiesta tocca un ambito specifico.

## Catalogo

- **nexus-rust-implementer** - backend Rust (crates/)
- **nexus-python-implementer** - brain Python
- **nexus-frontend-implementer** - apps/web-ide
- **nexus-db-architect** - migrazioni Postgres, Qdrant
- **nexus-doc-writer** - vault meta (docs/.nexus-vault/)
- **nexus-test-author** - test (Playwright, Rust, Python)
- **_nexus-orchestrator** - meta-agent per task multi-ambito

## Pattern

Ogni sub-agent:
1. Ha `description` con trigger semantici
2. Ha `tools` whitelist (subset MCP)
3. Carica il meta-vault prima di proporre modifiche
4. Restituisce un diff + razionale al main agent

Combinato con [[change-drafter]] forma il workflow di modifica codice supervisionata.
"#,
        },
        ConceptSpec {
            kind: "other",
            folder: "concepts",
            slug: "change-drafter",
            title: "ChangeDrafter (modifica supervisionata)",
            tags: &["concept", "change", "approval", "workflow"],
            body: r#"# ChangeDrafter

Workflow di modifica codice/doc supervisionata. Quando un agente o un sub-agent vuole applicare modifiche non triviali, **propone prima** una struttura formale all'utente.

## Output proposto

```json
{
  "razionale": "Perche' questa modifica e' necessaria",
  "impact_analysis": {
    "files_to_modify": [...],
    "breaking_changes": bool,
    "migration_required": bool,
    "tests_to_update": [...]
  },
  "diff_proposto": "<unified diff>",
  "verification_steps": [...],
  "alternative_considerate": [...]
}
```

## UI

Il componente `<ChangeDraftCard>` mostra il draft nella chat con 3 azioni:
- **Applica** - esegue il diff, ri-verify, commit
- **Modifica** - editor inline (max 3 iter)
- **Annulla** - draft `dismissed` per learning

## Tabella

`change_drafts` traccia ogni draft con `status` (pending/approved/rejected/applied/superseded/dismissed).

Vedi [[postgres-tables]], [[sub-agents-claude-code]].
"#,
        },
        ConceptSpec {
            kind: "other",
            folder: "concepts",
            slug: "auto-fix-workflow",
            title: "NexusAutoFix (PR automatiche)",
            tags: &["concept", "autofix", "ci", "pr"],
            body: r#"# NexusAutoFix

Worker che intercetta fallimenti E2E e propone fix via PR GitHub automatiche.

## Trigger

1. `NexusE2eSmokeWorker` esegue suite Playwright `apps/web-ide/e2e/nexus-self/`
2. Se un test fallisce: row in `nexus_e2e_runs` con `status='failed'`
3. `nexus_autofix_worker` (periodico 5 min) intercetta failure non ancora processati
4. Crea `change_drafts` con `trigger_kind='autofix'`

## Workflow (futuro: PR automatiche)

Vedi [[change-drafter]] per la pipeline di approvazione.

Il piano completo (step futuro) prevede:
- Worktree git in `/tmp/nexus-autofix-<uuid>`
- Apply patch via `edit_file`/`write_file`
- `pnpm verify` automatico
- Commit + push branch `nexus-autofix/<data>-<slug>`
- `gh pr create --base main`

## Tabelle

- `nexus_e2e_runs` - run di smoke test
- `change_drafts` - proposte di fix

Vedi [[postgres-tables]].
"#,
        },
        ConceptSpec {
            kind: "other",
            folder: "concepts",
            slug: "glossario",
            title: "Glossario Nexus",
            tags: &["concept", "glossario", "terminologia"],
            body: r#"# Glossario Nexus

| Termine | Significato |
|---|---|
| **Agent kind** | Categoria di agente AI (Coder, Tester, Reviewer, Architect, ...). 60+ varianti definite in `crates/nexus-orchestrator/src/agent_types.rs`. |
| **Behavior mode** | Modalita' del routing AI: bilanciata, veloce, approfondita, economica. |
| **Brain** | Servizio Python (FastAPI + LangGraph) che incapsula gli AI provider. Vedi [[brain-python]]. |
| **ChangeDrafter** | Workflow di modifica supervisionata. Vedi [[change-drafter]]. |
| **Intent** | Etichetta semantica per messaggio user (fix, feature, refactor, ...) classificata da LLM. |
| **Knowledge Base (KB)** | Vault per-progetto. Vedi [[knowledge-base-funzionamento]]. |
| **LearningWorker** | Pattern worker async. Vedi [[pattern-learning-worker]]. |
| **MCP tool** | Funzione callable da agent loop. Vedi [[pattern-mcp-tool]]. |
| **Meta-vault** | Doc di Nexus stesso. Vedi [[meta-vault-architettura]]. |
| **Provider** | Vendor AI (OpenAI, Anthropic, Google, Mistral, DeepSeek). |
| **Purpose** | Chiave usata per `nexus_purpose_model` (task interno specifico). |
| **Q-learning router** | Sistema di self-improvement che ottimizza scelta modelli via reward. |
| **Routing matrix** | Tabella DB che mappa intent+mode -> provider+model. Vedi [[multi-provider-routing]]. |
| **Sub-agent** | Agente specializzato Claude Code. Vedi [[sub-agents-claude-code]]. |
| **Vault** | Cartella Obsidian-compatible (`.md` + frontmatter YAML). |

Vedi anche [[nexus-funzionale]], [[nexus-architetturale]].
"#,
        },
        ConceptSpec {
            kind: "other",
            folder: "concepts",
            slug: "routing-matrix",
            title: "Routing matrix DB",
            tags: &["concept", "routing", "matrix", "ai"],
            body: r#"# Routing matrix

Tabella `nexus_routing_matrix`: unica fonte di verita' per scegliere quale modello AI usare per ogni richiesta utente.

## Schema

```sql
nexus_routing_matrix (
  intent           TEXT,    -- es. 'fix', 'feature', 'refactor', 'chat', 'docs', ...
  behavior_mode    TEXT,    -- 'bilanciata' | 'veloce' | 'approfondita' | 'economica'
  provider         TEXT,    -- 'openai' | 'anthropic' | 'google' | 'mistral' | 'deepseek'
  model_id         TEXT,    -- nome esatto del modello vendor
  PRIMARY KEY (intent, behavior_mode)
)
```

## API Rust

```rust
let matrix = state.orchestrator.routing_matrix.current_async().await?;
let (provider, model) = matrix.lookup(intent, behavior_mode)
    .unwrap_or_else(|| matrix.default_model("openai"));
```

## Auto-promote / Q-learning

Il worker `routing_matrix_auto_promoter` aggiorna le righe in base a:
- Reward medio (success rate, latenza, costo)
- Cap di costo per intent
- Black-list provider down

Vedi [[multi-provider-routing]], [[postgres-tables]].
"#,
        },
    ]
}

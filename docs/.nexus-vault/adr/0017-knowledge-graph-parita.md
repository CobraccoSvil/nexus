---
id: 0017-knowledge-graph-parita
kind: adr
title: "Knowledge Graph unificato — un solo sistema, scope come discriminante"
slug: 0017-knowledge-graph-parita
tags:
  - architecture
  - knowledge-graph
  - rag
  - triple-store
  - llm-extraction
  - unification
auto_generated: false
created_at: 2026-06-04T00:00:00Z
updated_at: 2026-07-02T00:00:00Z
nexus_meta_version: 2
---

# ADR 0017 — Knowledge Graph unificato (v2): un solo sistema, scope come discriminante

> **Status**: implementato (verificato 2026-07-02)
> **Aggiornamento 2026-07-02 (as-built)**: assorbito dal sistema wiki unificato — crate `nexus-wiki` (reingest, links_worker, triple_extractor, watcher), migrazioni 0295-0298, endpoint `/api/wiki/*`, WikiAcl come punto unico ACL. Il documento resta come razionale storico della decisione.
> **Versione**: 2 (sostituisce v1 "parità con storage separato")
> **Decisori**: team Nexus
> **Estende**: [[0015-rag-strutturale-unificato]], [[0016-rag-pipeline-completion]]
> **Principio cardine**: **un sistema unico, zero duplicazione**. Una tabella documenti, una tabella link, una collection Qdrant, un worker, una UI. Lo scope (`meta` vs `project`) è solo una colonna + un middleware ACL — non una tabella, non un endpoint, non un codice separato. I dati attuali (356 doc + 2.620 link + 317 vettori) **possono essere persi e rigenerati** dai vault Markdown e dai worker auto-link/extraction.

## Contesto

La v1 di questa ADR proponeva di mantenere `nexus_meta_docs` e `project_knowledge_notes` come tabelle separate, unificando solo il layer sopra (vocabolario, triple, UI). Era un compromesso prudente che lasciava **debito tecnico permanente**:

- Storage doppio → query doppia → bug doppi
- Cross-reference forzato in tabella `wiki_cross_scope_links` separata
- 3 tabelle di link (`nexus_meta_doc_links` + `project_knowledge_links` + `wiki_cross_scope_links`) per fare un grafo solo
- Codice `docs_core` parametrizzato su 2 backend SQL (storage path doppio)
- 2 worker auto-link, 2 worker triple-extractor, parametrizzati su scope come case-switch

Decisione corretta (utente, 2026-06-04): **un sistema unico**. I dati attuali sono perdibili e ricostruibili:

- 292 meta-docs → vivono come Markdown in `docs/.nexus-vault/`
- 64 note progetto → vivono nei vault Obsidian dei progetti
- 318 + 2.302 link → tutti `created_by=auto`, rigenerabili da `recompute-links`
- 317 vettori Qdrant → rigenerabili da `re-embed`

Quindi la migrazione "destructive" è ammessa: drop tabelle, crea nuova struttura, re-ingest dai vault. Zero data preservation, massima pulizia.

## Decisione

Costruire una **unica tabella `wiki_docs`** discriminata da colonna `scope ∈ {meta, project}`, con `project_id` nullable (NOT NULL solo se `scope=project`). Tutto il codice (storage, link, triple, worker, search, UI) **non distingue meta da project** se non in un singolo middleware ACL. Aggiungere triple semantiche, extraction LLM, grafo navigabile.

```
┌──────────────────────────────────────────────────────────────────────┐
│  TABELLA UNICA — wiki_docs (scope IN ('meta','project'))             │
│  ↓                                                                    │
│  TABELLA UNICA LINK — wiki_links (FK to wiki_docs)                    │
│  ↓                                                                    │
│  TABELLA UNICA TRIPLE — wiki_concept_triples (FK to wiki_docs)        │
│  ↓                                                                    │
│  COLLECTION UNICA — wiki_content (payload con scope + project_id)     │
│  ↓                                                                    │
│  MIDDLEWARE UNICO — WikiAcl::filter(query, user)                      │
│  ↓                                                                    │
│  CODICE UNICO — wiki_storage.rs, wiki_links_worker.rs,                │
│                 wiki_triple_extractor.rs, wiki_search.rs              │
│  ↓                                                                    │
│  UI UNICA — KnowledgeWorkspace (route varia, componente uguale)       │
└──────────────────────────────────────────────────────────────────────┘
```

## Schema dati

### Tabella `wiki_docs` (sostituisce `nexus_meta_docs` e `project_knowledge_notes`)

```sql
CREATE TABLE wiki_docs (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scope           TEXT NOT NULL CHECK (scope IN ('meta','project')),
    project_id      UUID REFERENCES projects(id) ON DELETE CASCADE,
    slug            TEXT NOT NULL,
    title           TEXT NOT NULL,
    body_md         TEXT NOT NULL DEFAULT '',
    body_hash       TEXT,                     -- sha256(body_md)
    kind            TEXT NOT NULL,            -- adr|note|runbook|architecture|api|changelog|concept|decision
    intent          TEXT,                     -- legacy per note progetti (debug|todo|reflection|...)
    tags            TEXT[] NOT NULL DEFAULT '{}',
    vault_file_path TEXT,                     -- relativo al vault dello scope
    qdrant_point_id TEXT,
    edit_lock       TEXT NOT NULL DEFAULT 'none' CHECK (edit_lock IN ('none','protected','frozen')),
    protected_sections TEXT[] NOT NULL DEFAULT '{}',
    manually_edited BOOLEAN NOT NULL DEFAULT FALSE,
    generated_hash  TEXT,                     -- hash dell'ultima auto-generazione
    edited_hash     TEXT,                     -- hash dell'ultima edit manuale
    last_generated_at TIMESTAMPTZ,
    last_edited_at  TIMESTAMPTZ,
    edited_by       TEXT,                     -- email o agent name
    current_version INT NOT NULL DEFAULT 1,
    auto_generated  BOOLEAN NOT NULL DEFAULT FALSE,
    public_read     BOOLEAN NOT NULL DEFAULT FALSE,  -- meta-doc consultabile da tutti i progetti
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Vincolo cardine: project_id obbligatorio se scope=project, vietato se scope=meta
    CONSTRAINT scope_project_consistency CHECK (
        (scope = 'project' AND project_id IS NOT NULL) OR
        (scope = 'meta'    AND project_id IS NULL)
    ),
    -- public_read ha senso solo per scope=meta
    CONSTRAINT public_read_meta_only CHECK (
        public_read = FALSE OR scope = 'meta'
    )
);

-- Slug unico per (scope, project_id) — i progetti possono avere stesso slug, meta è globale
CREATE UNIQUE INDEX uq_wiki_docs_slug ON wiki_docs (scope, COALESCE(project_id::text,''), slug);
CREATE INDEX idx_wiki_docs_scope ON wiki_docs (scope);
CREATE INDEX idx_wiki_docs_project ON wiki_docs (project_id) WHERE scope = 'project';
CREATE INDEX idx_wiki_docs_kind ON wiki_docs (kind);
CREATE INDEX idx_wiki_docs_tags ON wiki_docs USING gin (tags);
CREATE INDEX idx_wiki_docs_updated ON wiki_docs (updated_at DESC);
CREATE INDEX idx_wiki_docs_title_trgm ON wiki_docs USING gin (title gin_trgm_ops);
```

### Tabella `wiki_links` (sostituisce 3 tabelle: meta_doc_links + project_knowledge_links + cross_scope)

```sql
CREATE TABLE wiki_links (
    from_doc_id   UUID NOT NULL REFERENCES wiki_docs(id) ON DELETE CASCADE,
    to_doc_id     UUID NOT NULL REFERENCES wiki_docs(id) ON DELETE CASCADE,
    rel_type      TEXT NOT NULL DEFAULT 'relates',
    confidence    REAL NOT NULL DEFAULT 1.0,
    created_by    TEXT NOT NULL DEFAULT 'auto',
    evidence      TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (from_doc_id, to_doc_id, rel_type),
    CONSTRAINT wiki_links_no_self CHECK (from_doc_id <> to_doc_id),
    CONSTRAINT wiki_links_rel_type_check CHECK (rel_type IN (
        'relates','supersedes','depends_on','illustrates','contradicts',
        'followup','correction_of','refines','duplicate_of',
        'blocks','blocked_by','mentions','implements','tests'
    )),
    CONSTRAINT wiki_links_created_by_check CHECK (created_by IN (
        'auto','user','agent','llm','external'
    ))
);
CREATE INDEX idx_wiki_links_from ON wiki_links (from_doc_id, confidence DESC);
CREATE INDEX idx_wiki_links_to ON wiki_links (to_doc_id);
CREATE INDEX idx_wiki_links_predicate ON wiki_links (rel_type, confidence DESC);
```

Cross-scope è ora **invisibile**: un link da doc progetto a doc meta è solo `INSERT INTO wiki_links (from_doc_id, to_doc_id, ...)`. Niente tabella speciale, niente parser cross-scope dedicato.

### Tabella `wiki_concept_triples` (knowledge graph reale)

```sql
CREATE TABLE wiki_concept_triples (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    subj_doc_id   UUID NOT NULL REFERENCES wiki_docs(id) ON DELETE CASCADE,
    predicate     TEXT NOT NULL,
    obj_doc_id    UUID REFERENCES wiki_docs(id) ON DELETE CASCADE,
    obj_text      TEXT,                     -- concept libero ("RAG pipeline", "OAuth flow")
    obj_external  TEXT,                     -- URL o riferimento esterno
    source        TEXT NOT NULL,            -- wikilink|semantic|llm|user|agent|external
    confidence    REAL NOT NULL DEFAULT 0.5,
    evidence      TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- L'oggetto è uno e uno solo: doc, concetto libero, o riferimento esterno
    CONSTRAINT triple_obj_one CHECK (
        (obj_doc_id IS NOT NULL)::int +
        (obj_text IS NOT NULL)::int +
        (obj_external IS NOT NULL)::int = 1
    ),
    CONSTRAINT triple_predicate_check CHECK (predicate IN (
        'relates','supersedes','depends_on','illustrates','contradicts',
        'followup','correction_of','refines','duplicate_of',
        'blocks','blocked_by','mentions','implements','tests'
    )),
    CONSTRAINT triple_source_check CHECK (source IN (
        'wikilink','semantic','llm','user','agent','external'
    ))
);
CREATE INDEX idx_wct_subj ON wiki_concept_triples (subj_doc_id);
CREATE INDEX idx_wct_obj_doc ON wiki_concept_triples (obj_doc_id) WHERE obj_doc_id IS NOT NULL;
CREATE INDEX idx_wct_predicate ON wiki_concept_triples (predicate, confidence DESC);
CREATE INDEX idx_wct_obj_text_trgm ON wiki_concept_triples USING gin (obj_text gin_trgm_ops)
  WHERE obj_text IS NOT NULL;
```

`subj_scope` e `obj_scope` **non servono più**: si ricavano dalla JOIN `wiki_docs ON id = subj_doc_id`. Codice e indici più semplici.

### Tabella `wiki_doc_revisions` (versioning unificato)

```sql
CREATE TABLE wiki_doc_revisions (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    doc_id        UUID NOT NULL REFERENCES wiki_docs(id) ON DELETE CASCADE,
    version_no    INT NOT NULL,
    title         TEXT NOT NULL,
    body_md       TEXT NOT NULL,
    body_hash     TEXT NOT NULL,
    tags          TEXT[] NOT NULL DEFAULT '{}',
    source        TEXT NOT NULL CHECK (source IN ('auto','manual','import','revert')),
    author        TEXT,
    edit_summary  TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (doc_id, version_no)
);
CREATE INDEX idx_wdr_doc_version ON wiki_doc_revisions (doc_id, version_no DESC);
```

Niente più `scope` polimorfico — è già nel `doc_id` referenziato.

## Permission middleware (UN solo punto di enforcement)

### Rust — `crates/mcp-core/src/wiki/acl.rs`

```rust
pub struct WikiAcl {
    pub user_id: Uuid,
    pub is_admin: bool,
    pub project_ids: Vec<Uuid>,   // progetti di cui è membro
}

impl WikiAcl {
    /// Estende una query SELECT su wiki_docs con il filtro ACL.
    /// Restituisce la clausola WHERE da appendere (parametrizzata con $1..$N).
    pub fn scope_clause(&self) -> (String, Vec<Value>) {
        if self.is_admin {
            // Admin: vede tutto
            return ("TRUE".into(), vec![]);
        }
        // Utente normale: vede meta public OR meta_se_admin_su_qualche_progetto OR
        //                 doc dei propri progetti
        let projects_json = serde_json::to_value(&self.project_ids).unwrap();
        (
            "(wiki_docs.scope = 'meta' AND wiki_docs.public_read = TRUE) \
             OR (wiki_docs.scope = 'project' AND wiki_docs.project_id = ANY($1::uuid[]))"
                .into(),
            vec![projects_json],
        )
    }

    /// Check write permission su un doc esistente
    pub async fn can_write(&self, doc: &WikiDoc) -> bool {
        match doc.scope.as_str() {
            "meta"    => self.is_admin,
            "project" => doc.project_id
                .map(|pid| self.project_ids.contains(&pid))
                .unwrap_or(false),
            _ => false,
        }
    }
}
```

**Conseguenza**: ogni handler che legge `wiki_docs` deve costruire la query passando per `WikiAcl::scope_clause()`. Niente endpoint `/api/meta-docs/*` vs `/api/projects/:id/docs/*`: c'è **un solo set di endpoint** `/api/wiki/*` che accetta `scope` (e `project_id` se applicabile) come query param, e l'ACL filtra automaticamente. Più sicuro: se domani aggiungo `/api/wiki/triples`, l'ACL si applica gratis.

### URL design

| Endpoint vecchio | Endpoint nuovo |
|---|---|
| `GET /api/meta-docs` | `GET /api/wiki/docs?scope=meta` |
| `GET /api/projects/:id/knowledge/notes` | `GET /api/wiki/docs?scope=project&project_id=:id` |
| `PATCH /api/meta-docs/:id` | `PATCH /api/wiki/docs/:id` (scope ricavato da DB) |
| `GET /api/meta-docs/graph` | `GET /api/wiki/graph?scope=meta` |
| `GET /api/projects/:id/knowledge/graph` | `GET /api/wiki/graph?scope=project&project_id=:id` |

Endpoint vecchi restano come **thin redirect** verso i nuovi per 30 giorni (con log warn `deprecated: use /api/wiki/...`), poi rimossi.

## Migrazione (destructive, dati ricreati da vault + worker)

### Fase 0 — Backup

```bash
pg_dump -t nexus_meta_docs -t nexus_meta_doc_links \
        -t project_knowledge_notes -t project_knowledge_links \
        -t nexus_meta_doc_changes \
        -h ideai-postgres-nexus-1 -p 5433 -U nexus nexus \
  > backups/postgres/wiki_pre_unification_$(date +%Y%m%d_%H%M).sql
```

Non per restore: per audit forensico (capire post-mortem cosa c'era prima).

### Fase 1 — Schema unificato

Migrazione `0295_wiki_unified_schema.sql`:

```sql
BEGIN;

-- Drop vecchie tabelle (TRUNCATE non basta: vogliamo schema pulito)
DROP TABLE IF EXISTS project_knowledge_links CASCADE;
DROP TABLE IF EXISTS project_knowledge_notes CASCADE;
DROP TABLE IF EXISTS nexus_meta_doc_links CASCADE;
DROP TABLE IF EXISTS nexus_meta_doc_changes CASCADE;
DROP TABLE IF EXISTS nexus_meta_docs CASCADE;
DROP VIEW IF EXISTS wiki_docs CASCADE;  -- vecchia VIEW v1
DROP TABLE IF EXISTS wiki_doc_revisions CASCADE;

-- Crea nuove tabelle (vedi sezione "Schema dati" sopra)
CREATE TABLE wiki_docs ( ... );
CREATE TABLE wiki_links ( ... );
CREATE TABLE wiki_concept_triples ( ... );
CREATE TABLE wiki_doc_revisions ( ... );
-- Tutti gli indici e CHECK qui sopra

COMMIT;
```

### Fase 2 — Re-ingest dai vault

Worker one-shot `wiki_reingest.rs` lanciato manualmente dopo la mig:

```
for vault in [docs/.nexus-vault/, <project>/.nexus-vault/ per ogni progetto]:
  for ogni file Markdown:
    parse frontmatter + body
    INSERT INTO wiki_docs (scope, project_id, slug, title, body_md, kind, tags,
                            vault_file_path, generated_hash=sha256, ...)
    embedding via brain Python → upsert Qdrant collection `wiki_content`
    update wiki_docs.qdrant_point_id
```

Stima: 356 file × ~50ms parse + ~150ms embed = ~70 secondi.

### Fase 3 — Rebuild link

Dopo il re-ingest, lanciare manualmente `POST /api/wiki/recompute-links?scope=meta` e `POST /api/wiki/recompute-links?scope=project&project_id=*`:

- Parse `[[wikilink]]` da ogni body → INSERT wiki_links con `created_by=auto`, `rel_type=mentions`
- Semantic match Qdrant cosine ≥0.6 → INSERT wiki_links con `created_by=auto`, `rel_type=relates`

Stima: 356 doc × ~200ms = ~70 secondi.

### Fase 4 — Triple extraction LLM (opzionale, on-demand)

Worker `wiki_triple_extractor.rs` parte automaticamente dopo F3 secondo schedule (default ogni 30 min, cap 50 doc/giorno scope meta + 200/giorno per progetti).

Costo full re-extract: 356 doc × $0.0004 ≈ **$0.14 una volta sola**.

### Fase 5 — Switch UI

Deploy frontend con nuovi endpoint `/api/wiki/*`. Endpoint vecchi restano come redirect 308 + log warn.

## Codice unificato

### Backend Rust

| File nuovo | Sostituisce | Note |
|---|---|---|
| `crates/mcp-core/src/wiki/mod.rs` | `meta_docs/`, `knowledge/`, `docs_core/` | Modulo unico |
| `crates/mcp-core/src/wiki/acl.rs` | – | Middleware ACL |
| `crates/mcp-core/src/wiki/storage.rs` | `meta_docs/storage.rs`, `knowledge/storage.rs` | UN solo CRUD |
| `crates/mcp-core/src/wiki/vault.rs` | `meta_docs/vault.rs`, `knowledge/vault.rs` | Path: `docs/.nexus-vault/` per meta, `<project_root>/.nexus-vault/` per project |
| `crates/mcp-core/src/wiki/links_worker.rs` | `meta_docs/workers.rs`, `knowledge/auto_link.rs` | UN solo worker |
| `crates/mcp-core/src/wiki/triple_extractor.rs` | – | Nuovo, LLM-assisted |
| `crates/mcp-core/src/wiki/search.rs` | `knowledge/search.rs`, `meta_docs/search.rs` | Qdrant unificato |
| `crates/mcp-core/src/wiki/routes.rs` | `meta_docs/routes.rs`, `knowledge/routes.rs` | UN solo set endpoint `/api/wiki/*` |
| `crates/mcp-core/src/wiki/revisions.rs` | `docs_core/revisions.rs` | Polymorphic via FK |
| `crates/mcp-core/src/wiki/generators.rs` | `meta_docs/apply.rs`, `meta_docs/generators/*` | I generatori auto restano solo per scope=meta |

### Eliminazione moduli vecchi

Dopo il switch UI (fase 5):

```bash
rm -r crates/mcp-core/src/meta_docs/
rm -r crates/mcp-core/src/knowledge/
rm -r crates/mcp-core/src/docs_core/
```

Sostituiti completamente da `crates/mcp-core/src/wiki/`.

### Frontend

| File nuovo | Sostituisce |
|---|---|
| `apps/web-ide/components/wiki/knowledge-workspace.tsx` | `nexus-docs/page.tsx`, `knowledge/knowledge-panel.tsx` |
| `apps/web-ide/components/wiki/graph-full-page.tsx` | `knowledge/knowledge-graph.tsx` (modale) |
| `apps/web-ide/components/wiki/triple-browser.tsx` | – |
| `apps/web-ide/lib/wiki-client.ts` | `lib/meta-docs.ts`, parte di `lib/api-client.ts` |

Pagine:
- `apps/web-ide/app/admin/kb/page.tsx` → `<KnowledgeWorkspace scope="meta" />`
- `apps/web-ide/app/(project)/[projectId]/kb/page.tsx` → `<KnowledgeWorkspace scope="project" projectId={...} />`

**Stesso componente, props diverse**. Zero `if scope === 'meta'` ramificazioni nel codice — il componente non lo sa.

## Modello LLM e settings (regola G)

Migrazione `0296_wiki_settings.sql`:

```
agent.wiki.triple_extract_enabled = true
agent.wiki.triple_extract_interval_secs = 1800
agent.wiki.triple_extract_cap_per_day_meta = 50
agent.wiki.triple_extract_cap_per_day_project = 200
agent.wiki.triple_extract_min_confidence = 0.55
agent.wiki.graph_max_hops = 3
agent.wiki.graph_max_nodes_render = 500
agent.wiki.qdrant_collection = wiki_content
agent.wiki.semantic_link_threshold = 0.60
agent.wiki.semantic_link_top_k = 10
```

Purpose model:
```
nexus_purpose_model['wiki_triple_extract'] = google/gemini-2.5-flash-lite
```

JSON Schema strict per output LLM identico a v1 (predicate enum 12 valori).

## UI Knowledge Workspace

Componente `KnowledgeWorkspace` props:

```typescript
type Props = {
  scope: 'meta' | 'project';
  projectId?: string;  // required if scope === 'project'
};
```

Layout (full page, non modale):

```
┌─────────────────────────────────────────────────────────────────┐
│ Header — breadcrumb + search bar + filters                      │
├──────────────┬──────────────────────────────────┬───────────────┤
│              │                                  │               │
│  Tree        │  Tabs:                           │  Right rail   │
│  (vault file │   - Edit (raw md + preview)      │  - TOC        │
│   tree)      │   - Graph (Cytoscape full page)  │  - Backlinks  │
│              │   - Triples (table browser)      │  - Revisions  │
│              │   - History                      │  - Metadata   │
│              │                                  │  - Tags       │
└──────────────┴──────────────────────────────────┴───────────────┘
```

Features:
- **Graph navigabile**: click su nodo → grafo centrato con N hop (default 2). Breadcrumb back.
- **Drag&drop tripla**: drag da nodo X a nodo Y → modale `predicate` selector → POST `/api/wiki/triples` con `source='user'`.
- **Query box** (simil-SPARQL):
  ```
  ?x depends_on ?y AND ?x.scope = 'project' AND confidence >= 0.7
  ```
- **Cross-scope visibile**: se progetto X cita meta-doc Y, il nodo Y appare nel grafo del progetto con badge "meta". L'admin nel meta-vault vede tutti gli inbound da qualsiasi progetto (con badge progetto sorgente).

## Sequenza implementativa

| Fase | Tasks | Effort |
|---|---|---|
| **F1 — Schema unificato** | mig 0295 (drop+create), test schema, smoke create/select | 1 gg |
| **F2 — Backend storage** | modulo `wiki/`, `storage.rs`, `vault.rs`, ACL middleware, endpoint CRUD + revisions | 2 gg |
| **F3 — Re-ingest** | worker `wiki_reingest.rs`, lancio manuale, verifica 356 doc + Qdrant popolato | 1 gg |
| **F4 — Link worker** | `links_worker.rs` unificato (wikilink + semantic), endpoint `recompute-links` | 1 gg |
| **F5 — Triple extractor LLM** | mig 0296 settings, prompt template, worker, JSON Schema, validation | 2 gg |
| **F6 — Endpoint unificati** | `/api/wiki/*` (docs, links, triples, graph, search), redirect endpoint vecchi | 1 gg |
| **F7 — UI Knowledge Workspace** | `KnowledgeWorkspace`, `GraphFullPage`, `TripleBrowser`, pagine `/admin/kb` e `/:id/kb` | 3 gg |
| **F8 — Cleanup** | `rm -r meta_docs/ knowledge/ docs_core/`, retire endpoint vecchi, retire vecchie Qdrant collection | 0.5 gg |

**Totale: 11.5 giornate-uomo** (vs 14 della v1, perché eliminato codice duplicato).

## Metriche di Done

- ✅ `wiki_docs` popolata con ≈356 righe (counts pre-mig)
- ✅ `wiki_links` popolata con ≥2.000 righe (recompute deterministico)
- ✅ `wiki_concept_triples`: ≥3 triple/doc media dopo full extraction
- ✅ Endpoint `/api/wiki/graph?scope=meta` e `?scope=project&project_id=X` rispondono <2s
- ✅ Qdrant collection `wiki_content`: 356 vettori indicizzati
- ✅ ACL test: utente non-admin su progetto X **non vede** doc di progetto Y
- ✅ Cross-scope link: nota progetto con `[[adr-0017]]` produce riga `wiki_links` con `from_doc` scope=project + `to_doc` scope=meta
- ✅ UI: stesso componente `KnowledgeWorkspace` su `/admin/kb` e `/project/:id/kb`
- ✅ Cleanup: `crates/mcp-core/src/{meta_docs,knowledge,docs_core}/` non esistono più
- ✅ `pnpm verify` verde

## Permission matrix (riassunto)

| Azione | Admin Nexus | Membro progetto X |
|---|---|---|
| Read meta-doc | sempre | solo se `public_read=true` |
| Read doc progetto X | sì | sì |
| Read doc progetto Y | sì | no |
| Write meta-doc | sì | no |
| Write doc progetto X | sì | sì (se role ≥ editor) |
| Create link cross-scope (project X → meta) | sì | sì (richiede meta `public_read` per il target) |
| Create link cross-scope (meta → project X) | sì | no |
| Trigger LLM extraction | sì (qualsiasi scope) | solo su progetti propri |

ACL applicata in **un solo punto** (`WikiAcl::scope_clause()`), non duplicata per endpoint.

## Rischi e mitigazioni

| Rischio | Mitigazione |
|---|---|
| Migrazione perde dati non rigenerabili | Audit pre-mig: i 64+292 doc esistenti sono tutti rigenerabili dai vault Markdown. Backup forensico in `backups/postgres/`. Decisione utente esplicita di accettare la perdita. |
| Re-ingest produce slug duplicati | Worker include dedup logic: se slug esiste già con stesso `vault_file_path`, UPDATE invece di INSERT |
| ACL leak (utente vede meta non-public) | Test integrazione obbligatori: `test_acl_user_cannot_see_meta_private`, eseguito in CI |
| Cross-scope link a doc cancellato | FK `ON DELETE CASCADE` su `wiki_links` cancella automaticamente i link orfani |
| LLM extraction costoso | Cap diurno DB-driven + min confidence + JSON Schema strict |
| Grafo render lento >2000 nodi | `agent.wiki.graph_max_nodes_render = 500` cap, paginazione, filtri server-side |

## Cosa NON facciamo (decisioni esplicite, regola H)

- ❌ **Mantenere tabelle separate** "per sicurezza". L'unificazione è il punto. Sicurezza viene dal middleware ACL, non dalla separazione fisica.
- ❌ **Preservare dati esistenti** con migrazione complessa. Utente esplicitamente: "non ha importanza se perdiamo i dati, basta che sia possibile rigenerarli". Sono rigenerabili dai vault.
- ❌ **Endpoint vecchi paralleli a quelli nuovi**. Solo 30 giorni di redirect 308, poi rimossi.
- ❌ **`scope` come tabella separata per sicurezza**. Postgres CHECK + FK + middleware ACL sono sufficienti.
- ❌ **Knowledge graph distribuito** (Neo4j, ecc.). Postgres con `wiki_concept_triples` indicizzata regge 100k+ triple.

## Confronto v1 vs v2

| Aspetto | v1 (storage separato) | v2 (sistema unico) |
|---|---|---|
| Tabelle documenti | 2 + VIEW UNION ALL | **1** |
| Tabelle link | 3 (`meta_doc_links` + `project_knowledge_links` + `cross_scope_links`) | **1** |
| Tabella triple | 1 con `subj_scope`+`obj_scope` | **1 senza scope (FK risolve)** |
| Qdrant collections | 1 unificata | 1 unificata |
| Moduli Rust storage | `docs_core` + `meta_docs/` + `knowledge/` | **1: `wiki/`** |
| Worker auto-link | 2 (uno per scope) | **1** |
| Worker triple-extract | 2 (uno per scope) | **1** |
| Endpoint REST | `/api/meta-docs/*` + `/api/projects/:id/knowledge/*` | **`/api/wiki/*`** |
| Permission check | Implicito (endpoint diversi) | Esplicito (`WikiAcl` middleware) |
| Effort | 14 gg | **11.5 gg** |
| Migrazione dati | Armonizzazione legacy (preservante) | Drop+recreate (destructive, dati rigenerati) |
| Debito tecnico residuo | Storage path doppio per sempre | Zero |

## Riferimenti

- [[0015-rag-strutturale-unificato]] — RAG pipeline (precursore)
- [[0016-rag-pipeline-completion]] — pipeline RAG completa
- Audit dati 2026-06-04: 64 note progetto / 292 meta-doc / 2.302 link meta / 318 link nota / 317 vettori Qdrant
- Decisione utente 2026-06-04: "voglio avere un sistema unico non duplicato" + "non ha importanza se perdiamo i dati, basta che sia possibile rigenerarli"

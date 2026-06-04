---
id: 0020-build-graph-derivato-da-config
kind: adr
title: "Build graph derivato automaticamente dai config di progetto"
slug: 0020-build-graph-derivato-da-config
tags:
  - architecture
  - agent
  - filesystem
  - build-graph
  - tsconfig
  - cargo
  - structural
auto_generated: false
created_at: 2026-06-04T19:30:00Z
updated_at: 2026-06-04T19:30:00Z
nexus_meta_version: 1
---

# ADR 0020 — Build graph derivato automaticamente dai config di progetto

> **Status**: proposto
> **Decisori**: team Nexus
> **Sostituisce**: L1 e L2 di [[0019-file-picking-robusto-verify-chain]] (preflight grep + directory policy DB-driven)
> **Mantiene attivi**: L3 (`nexus_verify_change`), L4 (tool discovery hint), L5 (diagnostic escalation) — sono ortogonali e indipendenti da come rileviamo il build graph
> **Trigger**: incident M44 del 04/06/2026. ADR 0019 prevedeva una soluzione mista (grep importers + tabella policy con 12 seed). Analisi successiva ha mostrato che entrambi sono complementi pragmatici di una soluzione veramente strutturale: leggere i config di progetto e derivare automaticamente quali path sono "compilati/inclusi".

## Contesto

ADR 0019 L1 risolve "il file è importato da qualcuno?" con grep ricorsivo. ADR 0019 L2 risolve "questa directory è di solito non-produzione?" con una tabella DB + 12 pattern hardcoded come seed (`figma_export/`, `design/`, `mockups/`, ecc.).

Entrambi sono **proxy** della domanda vera:

> Questo file viene effettivamente compilato/incluso quando il progetto fa build?

Ogni progetto ha **già la risposta** nei suoi file di configurazione:

| Linguaggio | File di config | Contiene |
|---|---|---|
| TypeScript/JavaScript | `tsconfig.json` | `include`, `exclude`, `files`, `paths`, `references`, `extends` |
| Rust | `Cargo.toml` (workspace) | `[workspace] members`, `exclude`, `[bin]`/`[lib]` paths |
| Python | `pyproject.toml`, `setup.py`, `setup.cfg` | `packages`, `include-package-data`, `tool.setuptools.packages.find` |
| Go | `go.mod`, `go.work` | module root, build constraints |
| Java/Kotlin | `pom.xml`, `build.gradle` | `<sourceDirectory>`, `sourceSets` |

Leggere questi file restituisce la **mappa autoritativa** di quali path sono nel build graph, senza grep, senza pattern hardcoded, senza falsi positivi/negativi.

### Esempio: il caso Beauty-Book

```jsonc
// /home/administrator/projects/Beauty-Book/tsconfig.json
{
  "compilerOptions": { ... },
  "include": ["src"]
}
```

Una sola riga (`"include": ["src"]`) basta per sapere che:
- `src/app/pages/BookingPage.tsx` → **in build graph**
- `figma_export/src/app/pages/BookingPage.tsx` → **fuori dal build graph**

Nessun grep, nessuna policy, nessun seed.

## Decisione

Implementare un **build graph resolver** per ogni linguaggio supportato, persistere il risultato in DB con TTL, esporre via tool MCP + API interna usata dal preflight di write/edit.

```
┌──────────────────────────────────────────────────────────────────────┐
│  LAYER 1 — Parser config per linguaggio                              │
│   - TypeScriptResolver  (tsconfig.json + extends + references)       │
│   - RustResolver        (Cargo.toml workspace members + exclude)     │
│   - PythonResolver      (pyproject.toml + setup.py + setup.cfg)      │
│   - GoResolver          (go.mod + go.work)                           │
│   - JavaResolver        (pom.xml + build.gradle) [futuro]            │
│  Output comune: BuildGraphInfo {                                     │
│      project_id, language, include_paths, exclude_paths,             │
│      entry_points, monorepo_members, generated_dirs                  │
│  }                                                                    │
├──────────────────────────────────────────────────────────────────────┤
│  LAYER 2 — Cache DB + TTL                                            │
│  Tabella nexus_project_build_graph (project_id PK, language,         │
│      include_globs jsonb, exclude_globs jsonb, entry_points jsonb,   │
│      monorepo_members jsonb, generated_dirs jsonb,                   │
│      sources jsonb, // file di config letti per derivarlo            │
│      computed_at, ttl_secs)                                          │
│  Refresh: invalidato quando un file in `sources` cambia (via         │
│      wiki::watcher esistente esteso) o quando ttl scade.             │
├──────────────────────────────────────────────────────────────────────┤
│  LAYER 3 — API runtime                                               │
│  pub async fn is_in_build_graph(project_id, file_path)               │
│      -> Result<BuildGraphMembership>                                 │
│  enum BuildGraphMembership {                                         │
│      InGraph { reason: String },          // matched include glob    │
│      OutOfGraph { reason: String },       // not matched OR excluded │
│      Entrypoint { reason: String },       // App.tsx, main.rs, etc.  │
│      Generated { reason: String },        // dist/, target/, etc.    │
│      Unknown { reason: String },          // nessun config trovato   │
│  }                                                                    │
├──────────────────────────────────────────────────────────────────────┤
│  LAYER 4 — Integrazione preflight write/edit                         │
│  In agent_tools/filesystem.rs:                                       │
│    let membership = is_in_build_graph(project_id, path).await?;     │
│    match membership {                                                │
│      OutOfGraph { reason } => warning_to_tool_result(...),          │
│      Generated { reason }  => deny (regola: mai modificare build out)│
│      _ => proceed                                                    │
│    }                                                                  │
├──────────────────────────────────────────────────────────────────────┤
│  LAYER 5 — Tool MCP `nexus_build_graph_info`                         │
│  Espone la mappa al modello: dato un project_id, ritorna             │
│  include/exclude/entrypoints in forma leggibile. Cosi' il coder      │
│  agent puo' interrogarlo direttamente senza fare grep o cat config.  │
└──────────────────────────────────────────────────────────────────────┘
```

## Parser per linguaggio — dettaglio

### TypeScriptResolver

```rust
pub struct TsConfig {
    pub include: Vec<String>,           // glob patterns (default: ["**/*"])
    pub exclude: Vec<String>,           // glob (default: node_modules, bower_components, jspm_packages, outDir)
    pub files: Vec<String>,             // file espliciti
    pub paths: HashMap<String, Vec<String>>,  // path aliases
    pub references: Vec<String>,        // riferimenti ad altri tsconfig (project references)
    pub extends: Option<String>,        // ereditarietà
}

pub async fn resolve_typescript(project_root: &Path) -> Result<BuildGraphInfo> {
    let mut config_paths = vec![project_root.join("tsconfig.json")];
    // Cerca anche tsconfig.app.json, tsconfig.build.json, tsconfig.node.json
    for variant in ["tsconfig.app.json", "tsconfig.build.json", "tsconfig.node.json"] {
        let p = project_root.join(variant);
        if p.exists() { config_paths.push(p); }
    }
    let mut merged_include = HashSet::new();
    let mut merged_exclude = HashSet::new();
    for cfg_path in &config_paths {
        let cfg = parse_tsconfig_with_extends(cfg_path, project_root).await?;
        merged_include.extend(cfg.include);
        merged_exclude.extend(cfg.exclude);
    }
    // Default exclude se non specificati
    if merged_exclude.is_empty() {
        merged_exclude.insert("node_modules/**".into());
        merged_exclude.insert("dist/**".into());
        merged_exclude.insert("build/**".into());
    }
    Ok(BuildGraphInfo {
        language: "typescript".into(),
        include_globs: merged_include.into_iter().collect(),
        exclude_globs: merged_exclude.into_iter().collect(),
        entry_points: discover_ts_entrypoints(project_root).await?,
        monorepo_members: discover_ts_monorepo_members(project_root).await?,  // da package.json workspaces
        generated_dirs: vec!["dist".into(), "build".into(), ".next".into(), ".turbo".into()],
        sources: config_paths.into_iter().map(|p| p.to_string_lossy().into_owned()).collect(),
    })
}
```

Gestione `extends`: ricorsivamente carica il config padre e unisce le proprietà (TS standard: `compilerOptions` viene unito, `include`/`exclude` dal figlio sovrascrivono se presenti).

Gestione `paths` (alias TS): non serve per la membership ma utile per la risoluzione import — la salviamo per uso futuro.

Gestione `references` (project references TS): ogni reference è un altro tsconfig.json — ricorsivamente carica e include le sue regole nel set finale.

Gestione `package.json workspaces` (monorepo pnpm/yarn): leggi `workspaces` o `pnpm-workspace.yaml`, ogni member è un sotto-progetto con il suo tsconfig.

### RustResolver

```rust
pub async fn resolve_rust(project_root: &Path) -> Result<BuildGraphInfo> {
    let cargo_toml = project_root.join("Cargo.toml");
    let parsed: CargoToml = toml::from_str(&fs::read_to_string(&cargo_toml).await?)?;
    let mut include = vec!["src/**".into()];
    let mut exclude = vec!["target/**".into()];
    let mut monorepo_members: Vec<String> = vec![];

    // Workspace?
    if let Some(workspace) = parsed.workspace {
        // Members possono essere glob: "crates/*", "apps/*"
        for member_pattern in workspace.members {
            include.push(format!("{}/**", member_pattern));
            monorepo_members.push(member_pattern);
        }
        for excl in workspace.exclude.unwrap_or_default() {
            exclude.push(format!("{}/**", excl));
        }
    }
    // Bin custom paths
    if let Some(bins) = parsed.bin {
        for bin in bins {
            if let Some(path) = bin.path {
                include.push(path);
            }
        }
    }
    // Lib custom path
    if let Some(lib) = parsed.lib {
        if let Some(path) = lib.path {
            include.push(path);
        }
    }
    Ok(BuildGraphInfo {
        language: "rust".into(),
        include_globs: include,
        exclude_globs: exclude,
        entry_points: discover_rust_entrypoints(project_root).await?,  // main.rs, lib.rs
        monorepo_members,
        generated_dirs: vec!["target".into()],
        sources: vec![cargo_toml.to_string_lossy().into_owned()],
    })
}
```

### PythonResolver

Ordine di priorità (PEP 518 + setuptools):
1. `pyproject.toml` con `[tool.poetry]` o `[project]` o `[tool.setuptools]` — moderno
2. `setup.py` — legacy ma ancora diffuso
3. `setup.cfg` — intermedio

```rust
pub async fn resolve_python(project_root: &Path) -> Result<BuildGraphInfo> {
    let pyproject = project_root.join("pyproject.toml");
    if pyproject.exists() {
        let parsed: PyprojectToml = toml::from_str(...).await?;
        let packages = extract_python_packages(&parsed)?;
        return Ok(BuildGraphInfo {
            language: "python".into(),
            include_globs: packages.iter().map(|p| format!("{}/**", p)).collect(),
            exclude_globs: vec!["__pycache__/**".into(), "*.egg-info/**".into(), ".venv/**".into(), "venv/**".into()],
            entry_points: discover_python_entrypoints(project_root).await?,
            monorepo_members: vec![],
            generated_dirs: vec!["dist".into(), "build".into(), "__pycache__".into(), ".pytest_cache".into()],
            sources: vec![pyproject.to_string_lossy().into_owned()],
        });
    }
    // Fallback su setup.py (parser AST limitato) o euristica "trova file con `if __name__ == '__main__'`"
    discover_python_fallback(project_root).await
}
```

### GoResolver

`go.mod` definisce il modulo root. Tutti i `.go` sotto la root sono in build graph **eccetto** quelli con build constraint `//go:build ignore` o suffix `_test.go` (test, ma comunque "in build graph" per i tool di test).

`go.work` (workspace Go) lista più moduli, ognuno con la sua go.mod.

```rust
pub async fn resolve_go(project_root: &Path) -> Result<BuildGraphInfo> {
    let go_work = project_root.join("go.work");
    let go_mod = project_root.join("go.mod");
    let module_paths = if go_work.exists() {
        parse_go_work(&go_work).await?  // ritorna paths multipli
    } else if go_mod.exists() {
        vec![project_root.to_path_buf()]
    } else {
        return Err("no go.mod o go.work trovato".into());
    };
    Ok(BuildGraphInfo {
        language: "go".into(),
        include_globs: module_paths.iter().map(|p| format!("{}/**", p.to_string_lossy())).collect(),
        exclude_globs: vec!["vendor/**".into(), "**/*_test.go".into()],  // exclude opzionale, dipende dallo scope
        entry_points: vec!["main.go".into()],
        monorepo_members: module_paths.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
        generated_dirs: vec!["bin".into(), "vendor".into()],
        sources: vec![go_mod.to_string_lossy().into_owned()],
    })
}
```

## Schema DB

### Migrazione `db/migrations/0311_build_graph_cache.sql`

```sql
CREATE TABLE IF NOT EXISTS nexus_project_build_graph (
    project_id        UUID PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    language          TEXT NOT NULL,
    include_globs     JSONB NOT NULL DEFAULT '[]'::jsonb,
    exclude_globs     JSONB NOT NULL DEFAULT '[]'::jsonb,
    entry_points      JSONB NOT NULL DEFAULT '[]'::jsonb,
    monorepo_members  JSONB NOT NULL DEFAULT '[]'::jsonb,
    generated_dirs    JSONB NOT NULL DEFAULT '[]'::jsonb,
    sources           JSONB NOT NULL DEFAULT '[]'::jsonb,  -- file di config letti
    computed_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ttl_secs          INT NOT NULL DEFAULT 600  -- 10 minuti default
);

CREATE INDEX IF NOT EXISTS idx_pbg_language ON nexus_project_build_graph (language);
```

### Settings (regola G)

In `0311`:

```sql
INSERT INTO settings (key, value) VALUES
  ('agent.build_graph.default_ttl_secs', '600'),
  ('agent.build_graph.refresh_on_watcher', 'true'),
  ('agent.build_graph.warn_on_unknown', 'true')
ON CONFLICT (key) DO NOTHING;
```

## Cache + refresh

```rust
pub struct BuildGraphCache {
    inner: Arc<RwLock<HashMap<Uuid, (BuildGraphInfo, Instant)>>>,
    ttl: Duration,
}

impl BuildGraphCache {
    pub async fn get_or_compute(&self, db: &PgPool, project_id: Uuid) -> Result<BuildGraphInfo> {
        let now = Instant::now();
        {
            let r = self.inner.read().await;
            if let Some((info, computed_at)) = r.get(&project_id) {
                if now.duration_since(*computed_at) < self.ttl {
                    return Ok(info.clone());
                }
            }
        }
        // Cache miss o stale: ricalcola
        let info = self.compute(db, project_id).await?;
        // Persisti in DB
        sqlx::query("INSERT INTO nexus_project_build_graph
            (project_id, language, include_globs, exclude_globs, entry_points,
             monorepo_members, generated_dirs, sources, computed_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW())
            ON CONFLICT (project_id) DO UPDATE SET
                language = EXCLUDED.language,
                include_globs = EXCLUDED.include_globs,
                exclude_globs = EXCLUDED.exclude_globs,
                entry_points = EXCLUDED.entry_points,
                monorepo_members = EXCLUDED.monorepo_members,
                generated_dirs = EXCLUDED.generated_dirs,
                sources = EXCLUDED.sources,
                computed_at = NOW()
        ").bind(project_id).bind(&info.language).bind(...).execute(db).await?;
        self.inner.write().await.insert(project_id, (info.clone(), now));
        Ok(info)
    }

    async fn compute(&self, db: &PgPool, project_id: Uuid) -> Result<BuildGraphInfo> {
        let project = load_project(db, project_id).await?;
        let root = Path::new(&project.repository_root_path);
        // Detect linguaggio
        if root.join("Cargo.toml").exists() {
            return resolve_rust(root).await;
        }
        if root.join("package.json").exists() {
            return resolve_typescript(root).await;
        }
        if root.join("pyproject.toml").exists() || root.join("setup.py").exists() {
            return resolve_python(root).await;
        }
        if root.join("go.mod").exists() || root.join("go.work").exists() {
            return resolve_go(root).await;
        }
        Err(format!("nessun config riconosciuto in {}", root.display()))
    }

    /// Invalidato dal wiki::watcher quando un file in `sources` cambia
    pub async fn invalidate(&self, project_id: Uuid) {
        self.inner.write().await.remove(&project_id);
    }
}
```

### Integrazione con wiki::watcher (mig 0017 TODO 1)

Estendi `wiki::watcher` per:
- Osservare anche `tsconfig*.json`, `Cargo.toml`, `pyproject.toml`, `setup.py`, `go.mod`, `go.work` nei project roots
- Al cambio, chiamare `build_graph_cache.invalidate(project_id)` per quel progetto

## API runtime

```rust
pub async fn is_in_build_graph(
    state: &AppState,
    project_id: Uuid,
    file_path: &Path,
) -> Result<BuildGraphMembership> {
    let info = state.build_graph_cache.get_or_compute(&state.db, project_id).await?;
    let project = load_project(&state.db, project_id).await?;
    let root = Path::new(&project.repository_root_path);
    let rel = file_path.strip_prefix(root).unwrap_or(file_path);

    // Generated dirs => mai modificabili
    for gen_dir in &info.generated_dirs {
        if rel.starts_with(gen_dir) {
            return Ok(BuildGraphMembership::Generated {
                reason: format!("path in directory generata: {}", gen_dir)
            });
        }
    }
    // Entrypoint?
    if info.entry_points.iter().any(|ep| rel == Path::new(ep)) {
        return Ok(BuildGraphMembership::Entrypoint {
            reason: "entrypoint riconosciuto".into()
        });
    }
    // Exclude prima
    for excl in &info.exclude_globs {
        if glob_match(excl, rel) {
            return Ok(BuildGraphMembership::OutOfGraph {
                reason: format!("matcha exclude glob: {}", excl)
            });
        }
    }
    // Include
    for incl in &info.include_globs {
        if glob_match(incl, rel) {
            return Ok(BuildGraphMembership::InGraph {
                reason: format!("matcha include glob: {}", incl)
            });
        }
    }
    Ok(BuildGraphMembership::OutOfGraph {
        reason: "nessun include glob matchato".into()
    })
}
```

## Integrazione con preflight write/edit

In `agent_tools/filesystem.rs`:

```rust
pub async fn write_file_with_preflight(
    state: &AppState,
    project_id: Uuid,
    file_path: &Path,
    content: &str,
) -> Result<WriteResult> {
    let membership = is_in_build_graph(state, project_id, file_path).await?;
    let mut warnings: Vec<String> = vec![];
    match &membership {
        BuildGraphMembership::Generated { reason } => {
            return Err(format!(
                "Scrittura rifiutata: {} e' un file generato ({}). I file generati non vanno modificati manualmente.",
                file_path.display(), reason
            ));
        }
        BuildGraphMembership::OutOfGraph { reason } => {
            let info = state.build_graph_cache.get_or_compute(&state.db, project_id).await?;
            warnings.push(format!(
                "ATTENZIONE: {} NON e' nel build graph del progetto ({}). \
                 I file fuori dal build graph non vengono compilati ne eseguiti. \
                 Build graph derivato da: {}. \
                 Include patterns: {}. \
                 Se il tuo obiettivo e' modificare codice di produzione, verifica con `nexus_build_graph_info` quale path e' nel build graph.",
                file_path.display(),
                reason,
                info.sources.join(", "),
                info.include_globs.join(", "),
            ));
        }
        _ => {}
    }
    let result = do_write(file_path, content).await?;
    Ok(WriteResult {
        path: file_path.into(),
        bytes_written: content.len(),
        warnings,
        membership: Some(serde_json::to_value(&membership)?),
    })
}
```

## Tool MCP `nexus_build_graph_info`

Esposto al modello come tool nominato:

```
nexus_build_graph_info: ritorna la mappa del build graph del progetto (include glob, exclude, entrypoint, monorepo members). USA QUESTO STRUMENTO prima di modificare un file di codice se hai dubbi su quale file sia "quello vero" del progetto: ti dice esattamente quali path sono nel build graph in base a tsconfig.json/Cargo.toml/pyproject.toml.
Input: { project_id: uuid }
Output: { language, include_globs[], exclude_globs[], entry_points[], monorepo_members[], generated_dirs[], sources[] }
```

## Sostituzione di ADR 0019 L1 + L2

| ADR 0019 | Stato | Sostituito da ADR 0020 |
|---|---|---|
| L1 — Preflight grep importers | **Rimosso** | `is_in_build_graph()` è autoritativo, non serve grep |
| L2 — Tabella `nexus_project_directory_policies` | **Rimosso** | Le 12 policy default sono superflue: `figma_export/` viene rilevato automaticamente come "fuori da include glob `["src"]`". `node_modules`/`dist`/`build` sono nei `generated_dirs` derivati per linguaggio. |
| L3 — `nexus_verify_change` | **Mantenuto** | Indipendente |
| L4 — Tool discovery hint nel system prompt | **Aggiornato** | Sostituisce raccomandazione `grep` con raccomandazione `nexus_build_graph_info` |
| L5 — Diagnostic escalation | **Mantenuto** | Usa `is_in_build_graph()` invece di check ad-hoc |

## Vantaggi rispetto a 0019

| Aspetto | 0019 L1+L2 | 0020 |
|---|---|---|
| Veridico | grep proxy ("chi importa") | autoritativo ("è compilato?") |
| Falsi negativi | un file orfano ma compilato sembra orfano | impossibile: la config dice tutto |
| Falsi positivi | un file importato solo da test sembra in graph | gestito via include/exclude granulari |
| Manutenzione | 12 pattern hardcoded in DB | 0 pattern hardcoded, deriva da config reali |
| Costo runtime | grep ricorsivo a ogni write (lento monorepo) | lookup map cache in memoria (μs) |
| Generalizzazione | aggiungere lingua = aggiungere pattern | aggiungere lingua = aggiungere resolver |
| Trasparenza | warning non spiega perche | warning include include_globs + sources |
| Self-documenting | no | il tool `nexus_build_graph_info` insegna all'agente la struttura del progetto |

## Sequenza implementativa

| Fase | Task | Effort |
|---|---|---|
| F1 — Mig 0311 | Schema `nexus_project_build_graph` + settings | 0.2 gg |
| F2 — Resolver TypeScript | Parser tsconfig + extends + references, workspaces | 1.0 gg |
| F3 — Resolver Rust | Parser Cargo.toml workspace + bin/lib paths | 0.5 gg |
| F4 — Resolver Python | pyproject.toml + setup.py fallback | 0.7 gg |
| F5 — Resolver Go | go.mod + go.work | 0.3 gg |
| F6 — Cache + API runtime | `BuildGraphCache` + `is_in_build_graph()` | 0.5 gg |
| F7 — Integrazione preflight | Sostituisci grep di 0019 L1 con API | 0.3 gg |
| F8 — Tool MCP `nexus_build_graph_info` | Esponi al catalog | 0.3 gg |
| F9 — Integrazione wiki::watcher | Invalidazione cache su cambio config | 0.3 gg |
| F10 — Aggiornamento system prompt | Sostituisci 0019 L4 raccomandazioni con 0020 | 0.2 gg |
| F11 — Rollback 0019 L1+L2 | Rimuovi mig 0306, modulo directory_policy, cleanup | 0.3 gg |
| F12 — Test E2E | Caso Beauty-Book BookingPage figma_export ricostruito | 0.4 gg |
| **Totale** | | **5.0 gg** |

Sostituibile con 0019 L1+L2 (~2.0 gg) → costo netto +3 gg, ma con valore strutturale che vale.

## Metriche di Done

- ✅ Mig 0311 applicata
- ✅ 4 resolver (TS, Rust, Python, Go) testati con fixture sui repo reali Nexus + Beauty-Book + figma_export
- ✅ Cache TTL funziona: 2 chiamate consecutive su stesso progetto → seconda <1ms
- ✅ Invalidazione su cambio tsconfig.json: cambia il file, watcher invalida, nuova call ricomputa
- ✅ Test E2E: `is_in_build_graph(beauty_book, "figma_export/src/app/pages/BookingPage.tsx")` → `OutOfGraph { reason: "nessun include glob matchato (include: src/**)" }`
- ✅ Test E2E: `is_in_build_graph(beauty_book, "src/app/pages/BookingPage.tsx")` → `InGraph { reason: "matcha include glob: src/**" }`
- ✅ Test E2E: `is_in_build_graph(beauty_book, "node_modules/react/index.js")` → `Generated { reason: "path in directory generata: node_modules" }`
- ✅ Tool MCP `nexus_build_graph_info` espone i dati correttamente
- ✅ `cargo check --workspace` + `pnpm verify` verdi
- ✅ Mig 0306 (0019 L2) e modulo `directory_policy` rimossi

## Rischi e mitigazioni

| Rischio | Mitigazione |
|---|---|
| `tsconfig.json` ha sintassi non-strict-JSON (commenti, virgole) | Usa parser tollerante: `json5` crate o `serde_json` con `recover` |
| `extends` ciclico | Detect cycle con HashSet visited |
| Monorepo grande (>100 packages) → resolver lento | Cache aggressiva 10 min default; refresh solo on-change watcher |
| `pyproject.toml` ha sintassi divergente tra poetry/setuptools/hatchling | Implementa 3 parser separati con priorità; fallback su setup.py se serve |
| Lingua non supportata (es. Ruby, PHP, Elixir) | `BuildGraphMembership::Unknown { reason: "linguaggio non supportato" }` + warning non-bloccante; consumer fa best-effort senza preflight |
| Glob match performance su path lunghi | crate `globset` (compila glob in regex set, lookup O(log n)) |

## Cosa NON facciamo

- ❌ **Implementare un compilatore TS/Rust completo**. Il resolver legge solo i config, non risolve davvero import o type.
- ❌ **Tracciare ogni file Pyrhon/TS/Rust nel DB**. Solo i glob, l'instanziazione concreta resta lookup at runtime.
- ❌ **Sostituire `tsc --listFiles` o `cargo metadata`**. Quelli sono autoritativi ma lenti (secondi). Il nostro parser è euristico ma O(ms).
- ❌ **Forzare l'utente a registrare manualmente i build graph**. Tutto derivato dai file già presenti nel repo.

## Riferimenti

- [[0019-file-picking-robusto-verify-chain]] ADR 0019 (parzialmente sostituito)
- Incident M44 Beauty-Book 04/06/2026
- TypeScript handbook — tsconfig.json reference
- The Cargo Book — `[workspace]`
- PEP 518 — pyproject.toml
- Go modules reference — go.mod

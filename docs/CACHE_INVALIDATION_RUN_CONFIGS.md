# Invalidazione cache Run & Debug (Run Configurations)

## Contesto

Dopo il deploy di modifiche al detector di run-configuration in `crates/mcp-core/src/project_workspace.rs`, i progetti già cachati conservano i suggerimenti vecchi per fino a 7 giorni (TTL della cache in DB).

Per applicare i nuovi suggerimenti immediatamente, invalidare la cache con uno dei metodi seguenti.

## Metodo 1: HTTP (Nexus già in esecuzione)

Per ogni progetto, inviare una richiesta al endpoint di detection con flag `force=1`:

```bash
curl -X GET \
  "http://localhost:8080/api/projects/{PROJECT_ID}/run-configs/detect?force=1" \
  -H "Authorization: Bearer {JWT_TOKEN}"
```

Questo forza la riscansione filesystem e l'aggiornamento della cache in DB.

## Metodo 2: SQL (accesso diretto al DB)

Resettare la colonna `detected_suggestions_at` per tutti i progetti:

```sql
UPDATE projects SET detected_suggestions_at = NULL;
```

Il prossimo accesso a `GET /api/projects/:id/run-configs/detect` ricalcolerà i suggerimenti (modalità pigra).

Per resettare solo un progetto:

```sql
UPDATE projects SET detected_suggestions_at = NULL WHERE id = '{PROJECT_ID}';
```

## Timing del deploy

1. Compilare e avviare la nuova versione di `mcp-core` (che contiene i detector aggiornati).
2. Eseguire una delle due procedure sopra per invalidare la cache.
3. I client Nexus che aprono il wizard Run & Debug riceveranno i suggerimenti aggiornati.

## Riferimento: commit delle modifiche

- File: `crates/mcp-core/src/project_workspace.rs`
- Helper aggiunti: `collect_compose_files()`, `compose_file_rank()`, `extract_make_target_body()`
- Detector modificati: `compute_run_config_suggestions()` (docker-compose, Makefile), `detect_dotnet_suggestions()` (demotion quando containerizzato)
- Test aggiunti: 5 nuovi test unitari per il modulo `project_workspace::tests`

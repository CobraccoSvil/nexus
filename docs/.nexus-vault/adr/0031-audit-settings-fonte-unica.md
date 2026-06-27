# ADR 0031 — Audit configurazioni: ogni setting in DB deve avere un lettore, ogni categoria una via di amministrazione

Data: 2026-06-11. Stato: accettato, applicato (mig 0406-0408).

## Contesto

La tabella `settings` era cresciuta a 623 chiavi in 39 categorie tramite ~400
migrazioni, senza mai un censimento inverso ("chi la legge?"). La dashboard
admin navigava le categorie tramite DUE liste hardcoded divergenti
(`admin-sidebar.tsx` e `CATEGORY_ORDER` in `settings-panel.tsx`, violazione
regola L): ~26 categorie (~160 chiavi) erano invisibili e non amministrabili.

## Censimento (strumento permanente)

`cargo xtask audit-settings` (wrapper `scripts/audit-settings.sh`; portato da Python a Rust per lo zero-Python) incrocia:

- **A1** chiavi nel DB live (`docker exec ... psql`), **A2** chiavi nelle
  migrazioni (parser INSERT/DELETE);
- **B** lettori nel codice: call site dei lettori canonici
  (`get_setting*`/`resolve_port`, firme Rust e Python DIVERSE: chiave 2o vs 1o
  argomento) + set di TUTTE le stringhe quotate nei sorgenti (rilevatore
  primario: molte chiavi sono lette via dict batch / `key = ANY` / wrapper
  locali) + chiavi d'oggetto TS non quotate (`DB_KEY_MAP` del gateway) +
  whitelist di pattern dinamici (`*_api_key`, `project:*:playwright_enabled`,
  prefissi LIKE) + riconciliazione dei call site non spiegati;
- **C** superfici UI (categorie navigabili).

Classi: VIVA / MORTA / FANTASMA (letta ma assente in DB) / INVISIBILE /
RUNTIME-ONLY / TEST-ONLY. Gate ratchet in `verify.sh`
(`audit-settings.sh --gate`, baseline `scripts/audit-settings-baseline.json`,
i conteggi possono solo scendere; degrada a no-op senza DB).

## Esiti (validazione adversariale: 8 verificatori indipendenti + 1 fantasma)

- **83 chiavi MORTE eliminate** (mig 0406). Cause radice principali:
  `impact.*` orfane dell'endpoint cancellato da eb5e47a (ADR 0017 v2);
  `kb.*`/`knowledge.*`/`meta_docs.*` dei sistemi KB legacy pre-wiki;
  `wiki.retention*`/versioning mai implementati; seed storici mai letti
  (`sandbox_*`, `schema.*`, `supervisor_*`, `optimizer_*`).
- **3 falsi positivi salvati** dalla verifica adversariale:
  `rate_limit_per_{tenant,provider}_window_ms` (lette dal gateway via chiavi
  d'oggetto non quotate), `web_ide_port` (letta dinamicamente dal watchdog via
  `port_setting_key` e da `deploy-local.sh`).
- **9 chiavi FANTASMA materializzate** (mig 0407): lette dal codice con
  default che mascheravano l'assenza (violazione regola G), ora amministrabili
  (`agent.g1_max_nudges`, `ollama_url`, `providers.google.thinking_budget`,
  collection Qdrant, ecc.).
- **2 bug di wiring riparati** (chiave amministrabile ma mai caricata dal
  loader): `orchestrator.clarifying_questions_{enabled,max}` aggiunte a
  `orchestrator_config._KEYS`; `kb.intake.confirm_if_implemented` rinominata
  `clarify.confirm_if_implemented` (il loader legge solo `clarify.%`) con
  mapping aggiunto. Test di regressione: `brain/tests/test_settings_wiring.py`.
- **1 worker zombie rimosso**: `nexus_autofix_worker` pollava `nexus_e2e_runs`
  ogni 300s, tabella in cui nessun codice ha MAI scritto (runner E2E mai
  implementato, sistema meta-docs sostituito dal wiki).
- **1 bridge promesso costruito**: `nexus_profile` aggiunta a `DB_KEY_MAP`
  del gateway (il commit 5591746 la dichiarava "gestita da settings DB" senza
  che il bridge esistesse).
- **Feature rotta risolta** (mig 0410, 2026-06-11): il `regression_gate_node`
  del brain chiamava `/api/internal/impact/*`, rimosso da eb5e47a (ADR 0017 v2)
  insieme al writer che popolava le tabelle impact. Decisione: rimozione
  completa (coerente con la nota di eb5e47a "reimplementare quando servono").
  Rimossi il nodo dal grafo, `regression_gate_node.py`,
  `route_after_regression_gate`, il guard `gate_status` in `auto_commit`
  (`agent_types.rs`), le 6 settings `regression_gate.*` (le `impact.*` erano
  gia' uscite con la mig 0406) e le 4 tabelle morte di mig 0243
  (`project_code_nodes/edges/tests`, `project_impact_runs`).

## Decisione architetturale: categorie di navigazione derivate dai dati

- Endpoint `GET /api/admin/settings-categories` (mcp-core `settings.rs`,
  `SELECT category, count(*) ... GROUP BY`).
- Punto unico frontend `apps/web-ide/lib/settings-categories.ts`
  (`useSettingsCategories`): ordine/label per le categorie note
  (`KNOWN_CATEGORY_META`), categorie nuove in coda alfabetica — mai piu'
  invisibili. `admin-sidebar.tsx` consuma l'hook; `CATEGORY_ORDER` eliminata.
- Normalizzazione sinonimi (mig 0408): `agents/automation→agent`,
  `ai/monitoring/router→routing`, `runtime/system→infrastructure`,
  `vector→embeddings`, `general(agent.*)→agent`, `meta_docs→wiki`.

## Conseguenze

- Una migrazione che aggiunge una categoria nuova e' subito amministrabile.
- Una chiave senza lettore fa fallire il gate al primo `pnpm verify` (ratchet).
- Lo scanner ha limiti noti documentati nel codice (chiavi interamente
  costruite a runtime): la whitelist `DYNAMIC_READ_PATTERNS` e la
  riconciliazione dei call site sono il contratto per estenderlo.

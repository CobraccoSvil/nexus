---
id: adr-0030-selettore-modello-unico
kind: adr
title: "ADR 0030 - Selettore modello unico (consolidamento dei due algoritmi di selezione catalog)"
slug: 0030-selettore-modello-unico
tags:
  - adr
  - routing
  - catalog
  - model-selection
  - single-source-of-truth
  - regola-L
  - regola-G
nexus_meta_version: 1
---

# ADR 0030 - Selettore modello unico (consolidamento dei due algoritmi di selezione catalog)

## Stato

Accepted - 2026-06-09. Fasi 1 e 2 rilasciate; Fase 3 CHIUSA/ACQUISITA
(2026-06-10): il monitoraggio shadow su traffico reale ha mostrato che
l'obiettivo (routing per-intent risolto a runtime via tier) e' gia' realizzato
dal path slot-based; gli stadi 2-4 non sono necessari (vedi VERDETTO Fase 3).

## Contesto

Un audit (regola L) ha rilevato che la domanda "qual e' il/i miglior(i) modello/i
del catalog per `(tier, capability, requires_tool_use)`" era risolta da DUE/TRE
algoritmi distinti e divergenti:

1. `select_agentic_model` in `crates/mcp-core/src/orchestrator/model_routing.rs`
   (routing LIVE): query SQL diretta, eleggibilita' hard (`is_enabled`,
   `supports_tool_use`, `agentic_thinking_policy <> 'exclude'`, cooldown in-SQL),
   `tier_chain` con degradazione esatta, `ORDER BY` cost-first con pre-ordinamento
   `(agentic_thinking_policy = 'none') DESC`, top-1.
2. Il ramo NON-agentico inline di `best_model_for_tier` (stesso file): una TERZA
   query SQL quasi identica per i purpose non-tool (vision/chat/embedding).
3. `select_top_candidates` / `select_models_for_requirement` / `score_model` in
   `crates/mcp-core/src/routing_matrix_auto_promoter.rs` (auto-promoter OFFLINE +
   `route_by_slots`): catalog in RAM, scoring pesato (tier/cost/context/capability),
   top-N per provider.

Divergenze rilevate:

- Pesi `0.35/0.25/0.20/0.20` HARDCODED nel codice (violazione regola G).
- Normalizzazione provider case-sensitive incoerente (alcuni `LOWER`, altri RAW).
- Filtro `consecutive_failures = 0` solo offline (starvation, contro ADR 0025).
- `agentic_thinking_policy <> 'exclude'` solo nel live.
- Capability match parziale (`>= 0.5`) offline vs totale (`jsonb @>`) live.
- Errori SQL silenziati con `.ok().flatten()`.

## Decisione

Consolidamento IBRIDO INCREMENTALE in un nuovo modulo
`crates/mcp-core/src/orchestrator/model_selection.rs` (punto unico, regola L),
realizzato in fasi.

Scartati:

- approccio "sql-first" (scoring tradotto in SQL) per divergenza floating-point
  `f32` Rust vs Postgres e bassa testabilita';
- approccio "scoring-first" (promuovere lo scoring pesato al routing live) perche'
  cambierebbe il comportamento runtime per utenti reali, rischio inaccettabile.

### Fase 1 (rilasciata, regressione zero)

- `ScoringWeights` + `default_scoring_weights(db)`: pesi letti dalla riga
  sentinella `intent = '*'` / `behavior_mode = '*'` di
  `nexus_intent_routing_requirements` (migrazione 0379), cache 60s `TtlCache`.
  Elimina i pesi hardcoded (regola G). `load_requirements` esclude `intent = '*'`
  (non genera righe matrix spurie).
- `excluded_providers_lower(extra)`: costruzione unica e lowercase della lista
  provider esclusi (cooldown snapshot + extra). Fix del bug case-sensitivity nel
  ramo non-agentico di `best_model_for_tier` (ora `LOWER(provider)`).
- Sub-score (`tier_score` / `tier_rank` / `cost_score` / `context_score` /
  `capability_match_pct`) esposti `pub(crate)`; corretti due commenti fuorvianti.

### Fase 2 (rilasciata)

- `EligibilityFilter` (predicato di eleggibilita' UNICO, parametrico:
  `require_tool_use`, `require_thinking_non_exclude`, `capability`,
  `min_context_window`, `exclude_providers`, `apply_cooldown`) +
  `select_models_tierchain(db, filter, tier_chain, order_by, limit) ->
  Result<Vec<(String, String)>, String>`. Sostituisce le tre query SQL del path
  live: `select_agentic_model` e i due rami di `best_model_for_tier` diventano
  VISTE SOTTILI a firma INVARIATA (i ~9 call site non cambiano). Il
  pre-ordinamento `(agentic_thinking_policy = 'none') DESC` e' legato a
  `require_thinking_non_exclude` (path agentico). Propaga `Result` invece di
  `.ok().flatten()` (regola H: errori SQL loggati, non silenziati).
- Allineamento eleggibilita' OFFLINE al live (ADR 0025): rimosso il filtro
  `consecutive_failures = 0` da `load_catalog` (anti-starvation: la salute e'
  gia' garantita da `is_enabled`, il `model_health_probe` auto-disabilita a
  soglia); aggiunto `agentic_thinking_policy <> 'exclude'` per gli slot tool in
  `select_top_candidates`.

### Decisione data-driven (validare con diff prima di applicare)

Il piano prevedeva anche di unificare la capability su match TOTALE (`jsonb @>`)
anche offline. Un DIFF in staging sul catalog reale ha mostrato che il match
totale avrebbe SVUOTATO lo slot (`intent = 'test'`, `tier = heavy`,
`required_capabilities = {code, test, reasoning}`) perche' la capability `test`
e' FANTASMA (nessun modello la possiede) e avrebbe ridotto molti slot a 1-2
candidati senza beneficio (il `cap_score` graduale gia' premia il match
migliore). Percio' il match totale NON e' stato applicato: la soglia
`capability_match_pct >= 0.5` resta come soglia di robustezza deliberata. Il
dato fantasma `test` e' tracciato come follow-up (correzione del requirement via
migrazione).

Il diff ha anche confermato:

- 0 modelli enabled con `policy = 'exclude'` (filtro innocuo oggi);
- ~20 modelli con `consecutive_failures > 0` che rientrano nel pool
  (anti-starvation).

### Fase 3 (follow-up, in corso)

Convergenza completa dell'asse capability / `consecutive_failures` e
materializzazione matrix via un unico reconciler; osservazione di un round
auto-promote.

Materializzatore unico della routing matrix (primo passo FASE 3, rilasciato
2026-06-09): `auto_upgrade_models_and_routing` (`crates/mcp-core/src/models.rs`)
NON scrive piu' `model_id` su `nexus_routing_matrix`. Prima sostituiva il
modello per nome-famiglia (`FAMILY_RULES`) in conflitto con lo scoring di
`routing_matrix_auto_promoter::run_one_round` -> ping-pong non deterministico
sulle righe non-manual (vinceva l'ultimo worker a girare). Ora la
materializzazione del `model_id` della matrix e' ESCLUSIVA di `run_one_round`
(via il selettore unico). Verificato: tutte le righe non-manual attive sono
coperte da un requirement (`run_one_round` le materializza al 100%, nessun
buco). L'upgrade di versione avviene comunque: il modello nuovo viene abilitato
nel catalog (`is_enabled = true`) da `auto_upgrade` e selezionato via scoring; le
righe stale sono gestite da `heal_orphan_pinned_models` + `cleanup_stale_rows`.
`auto_upgrade` resta responsabile SOLO di: enable dei modelli di famiglia nel
catalog, aggiornamento di `nexus_provider_default_model` (tabella diversa, non
materializzata da `run_one_round`) e `auto_populate_escalations` (i campi
`escalation_*` che `run_one_round` non scrive). Validato in produzione: round
auto-promote con `no_candidates = 0`, 0 intent scoperti.

Consolidamento del fallback `__no_model__` (rilasciato 2026-06-09): rimossi i
settings `provider_model_*` / `default_model`; `resolve_model` converge su
`default_model_for_provider` = `nexus_provider_default_model` (tabella unica
fonte di verita', regola G). Chiude il finding
`g-core-provider-models-fallback-db`.

FASE 3 — Stadi 0+1 rilasciati (programma a 5 stadi, rollout incrementale con
kill-switch DB).

- STADIO 0 (infra, zero cambio comportamento): `RoutingMatrix` porta ora
  `manual_overrides: HashSet<(intent, behavior_mode)>` (campo PARALLELO a
  `by_intent_mode`, non tocca `lookup` -> zero impatto sui call site) + accessor
  `is_manual_override`; `fetch_from_db` seleziona `manual_override` e traccia la
  riga vincente per priorita'. Migrazione 0383 introduce il flag.
- STADIO 1 (shadow-compare, opt-in): `orchestrator::shadow_compare_per_intent`
  (`model_selection.rs`), attivo SOLO se il setting
  `routing.per_intent_runtime_shadow` = true (default false, kill-switch), calcola
  in parallelo la decisione tier-runtime (`select_models_for_requirement` +
  cooldown caller-side) e logga la divergenza vs il lookup statico (target tracing
  `routing_shadow`), SENZA cambiare la decisione servita; hook in
  `resolve_agent_provider` dopo la decisione statica; solo intent SENZA
  manual_override. Validato: health 200 con flag on/off, kill-switch funzionante.
- RESTANO stadi 2-4 (consolidamento dei 3 rami cooldown-fallback duplicati via
  snapshot; tier-runtime sul caso felice non-pin con matrix come cache
  trasparente; opzionale pin-in-cooldown). PREREQUISITO: attivare il flag shadow
  su traffico reale e osservare la parita' (>= 98% match sugli intent senza pin)
  prima di switchare. Lo snapshot in-memory ottimizzato (`RoutingResolveSnapshot`)
  e lo split del selettore in core puro + wrapper async sono rimandati allo Stadio
  2 (servono sul path critico, non per lo shadow opt-in).

VERDETTO Fase 3 (10/06/2026) — ACQUISITA via slot-path. Il monitoraggio
shadow-compare (flag `routing.per_intent_runtime_shadow`, Stadio 1) su traffico
reale ha mostrato: `route_by_slots` (path slot-based, gia' tier-runtime via
`select_models_for_requirement` + cooldown) ha gestito TUTTE le decisioni di
routing (hit=6, slot-miss=0), mentre il lookup statico `route_model_with_mode`
(dove e' posizionato lo shadow) NON e' mai stato raggiunto a runtime (0 campioni
shadow). Il path statico e' il fallback per slot-miss, che sul traffico osservato
non si verifica. CONCLUSIONE: l'obiettivo Fase 3 (routing per-intent risolto a
runtime via tier) e' di fatto GIA' realizzato dal path slot-based (`route_by_slots`,
mig 0133, `nexus_routing_slots_matrix`); gli stadi 2-4 (consolidamento/convergenza
del path statico residuo su tier) NON sono necessari, perche' convergerebbero un
ramo che a runtime non viene mai eseguito. Lo shadow e' stato disattivato
(flag=false). Resta in produzione, come fondazione, l'infra dello Stadio 0
(`RoutingMatrix.manual_overrides` + `is_manual_override`) e dello Stadio 1
(`shadow_compare_per_intent` + flag), riattivabile se in futuro il path statico
dovesse tornare rilevante. La FASE 3 si considera CHIUSA.

## Conseguenze

- Un solo punto definisce l'eleggibilita' del path live (regola L); pesi 100%
  DB-driven (regola G); case-sensitivity provider risolta; errori SQL non piu'
  silenziati (regola H); live e offline allineati su `consecutive_failures` e
  `policy <> 'exclude'`.
- L'ordinamento resta BI-MODALE per scelta esplicita: `TierChainSql` (cost-first
  deterministico) per il live, `WeightedScore` (scoring pesato) per offline/slot.
  Sono use case distinti (planner/clarify deterministico vs materializzazione
  morbida): NON si forza una semantica unica.
- Il contenuto materializzato di `nexus_routing_matrix` cambia al primo round
  auto-promote post-deploy (piu' candidati dal `consecutive_failures` rimosso).
  Validato sicuro dal diff (nessuno svuotamento).
- Test: 8 test `model_selection` (`default_scoring_weights` presente/assente,
  `excluded_providers_lower`, 5 golden su `select_models_tierchain`: agentico
  cost-first, preferenza `policy = none`, esclusione `policy = exclude`,
  degradazione tier, vision via `supports_vision`) + 24 test `auto_promoter`
  (inclusi i 5 scoring conservati e il nuovo
  `select_top_excludes_policy_exclude_when_tool`). `clippy -D warnings` pulito.

## Riferimenti

- `crates/mcp-core/src/orchestrator/model_selection.rs` (nuovo),
  `orchestrator/mod.rs` (registrazione), `orchestrator/model_routing.rs` (viste),
  `routing_matrix_auto_promoter.rs` (pesi DB + `load_catalog` + filtro policy),
  `db/migrations/0379_routing_default_scoring_weights.sql`.
- ADR [[0025-gestione-modelli-deterministica]] (eleggibilita' agentica,
  `agentic_thinking_policy`, anti-starvation),
  [[0026-punto-unico-de-duplicazione]] (catalogo punti unici),
  [[0024-capability-fonte-unica-classificazione]] (capability fonte unica).
- Regole `CLAUDE.md`: G (modelli e config DB-driven), H (fix definitivi, niente
  errori silenziati), L (punto unico di controllo).

## Consolidamenti correlati del dominio provider (rilasciati 2026-06-09)

Due interventi sul brain Python, completati dopo il selettore unico, allineano il
lato provider del brain agli stessi punti unici di Rust (regole G / L, ADR 0020 /
0024).

### Capability provider Python da vista (#42)

`mistral_provider` e `deepseek_provider` derivano `supports_tools` da
`cap.tool_use` (vista `0318`, [[0024-capability-fonte-unica-classificazione]])
invece che da euristiche-nome (`_TOOL_CAPABLE`, `_is_deepseek_reasoning`).
L'euristica-nome resta come SOLO fallback quando `cap is None` (degrado safe, lo
stesso pattern gia' usato da `google_provider`). NESSUNA migrazione: `cap.tool_use`
esisteva gia' nella vista. `openai_provider` mantiene `_is_o_series` come
detection-nome perche' e' un QUIRK DI PROTOCOLLO (`max_completion_tokens`, niente
`temperature`, ruolo `developer`, niente `tool_choice`), non una capability
semantica, quindi non e' un concern della vista capability. Validato a runtime: i
modelli tool-capable inviano i tool e rispondono con `tool_calls`.

### Cooldown writer unico (#43)

La fonte persistente `nexus_provider_health` e' ora scritta SOLO da mcp-core
(Rust): un unico writer per il cooldown (regola L,
[[0020-gate-disponibilita-provider]]). `propagate_billing_disable_to_db` fa UPSERT
di `billing_cooldown_until` (TTL DB-driven `cooldown_long_s`) e
`propagate_billing_reenable_to_db` lo azzera. Il brain
(`registry._mark_billing_cooldown`) non scrive piu' il DB: tiene la cache
in-memory locale per la cascade immediata e notifica il bridge tramite la nuova
`notify_provider_error_sync` (variante sync best-effort, perche' il call site e'
in `generate_agent_turn_sync`). `_clear_billing_cooldown` pulisce solo la cache
locale; la riabilitazione cross-process e' governata dal recovery loop Rust
probe-then-reenable (piu' sicuro della riattivazione cieca, latenza
~`billing_recovery_interval_s`). Validato E2E: `POST
/api/internal/provider-error` -> riga in `nexus_provider_health` scritta da Rust.

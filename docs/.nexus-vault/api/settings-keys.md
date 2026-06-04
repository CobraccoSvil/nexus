---
id: 59047b22-236b-4a0b-b6ad-985a133de3d6
kind: api
title: Settings keys
slug: settings-keys
tags:
  - api
  - settings
source_commit: 109bfafad79cbe4c32779f771e0982e92635cf47
source_files:
  - db/migrations/
auto_generated: true
created_at: 2026-05-23T07:20:00Z
updated_at: 2026-06-03T20:57:34Z
nexus_meta_version: 1
---

Tutte le chiavi di configurazione di Nexus (tabella `settings`). Generato dal DB.

Vedi anche: [[postgres-tables]], [[routing-matrix]], [[meta-vault-architettura]].


## `agent`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `agent.adaptive_budget_ttl_seconds` | `60` | TTL cache adaptive iteration budget (H-25) |
| `agent.attachment_budget_ttl_seconds` | `60` | TTL cache attachment session budget (H-37) |
| `agent.attachment.figma_make_ai_chat_max_load_bytes` | `536870912` | Guardia anti-OOM ESTREMA (NON un cap di contenuto) sul caricamento in RAM del file ai_chat.json di un archivio Figma Make prima del parsing. Default 512 MB: rete di sicurezza contro file patologici. I .make reali stanno nell'ordine dei MB. |
| `agent.attachment.image_max_bytes` | `2097152` | Massima dimensione (byte) di un immagine processabile dal tool nexus_describe_image_attachment. Default 2 MB. Oltre il limite il tool ritorna errore esplicito al modello. |
| `agent.attachment.inspector_header_bytes` | `32768` | Byte iniziali letti per magic-byte detection (H-68) |
| `agent.attachment.read_cache_ttl_seconds` | `300` | TTL (secondi) della cache LRU read_cache che deduplica chiamate identiche a nexus_read_attachment / nexus_read_archive_entry. Default 5 minuti. |
| `agent.attachment.read_chunk_max_bytes` | `102400` | Max byte per chiamata nexus_read_attachment (H-67) |
| `agent.clarify_max_tokens` | `400` | max_tokens per chiamata clarify (H-40) |
| `agent.command_loop_threshold` | `3` | Soglia stesso comando consecutivo per loop detection (H-36) |
| `agent.complexity.file_path_points` | `2` | Punti per ogni path o file menzionato nel prompt (es. /home/, src/, *.json). |
| `agent.complexity.keyword_weights` | `{"create":3,"write_file":2,"install":2,"build":2,"systemc...` | Pesi keyword per stima complessita' task agente (budget iterazioni adattivo). Chiave: substring da cercare nel prompt, valore: punti complessita'. Aggiornamento: aggiunti verbi italiani (implementa, sviluppa, costruisci, genera, scaffold) e sostantivi (progetto, applicazione). |
| `agent.complexity.step_marker_points` | `5` | Punti per ogni marker di step esplicito (1., 2., step, task, phase) nel prompt. |
| `agent.complexity.weak_model_multiplier` | `1.5` | Moltiplicatore budget se il modello iniziale e gpt-4o-mini / haiku / nano (necessita piu iter per G1 nudge). |
| `agent.context.aggressive_keep_recent` | `3` | Numero di messaggi piu recenti mantenuti integri dalla compressione aggressiva TOKEN-based. La richiesta originale (primo messaggio umano) e i riassunti rolling sono sempre preservati. Default 3. |
| `agent.context.aggressive_max_chars` | `200` | max_content_chars per la compressione aggressiva TOKEN-based: i messaggi vecchi (inclusi assistant) vengono troncati a questa lunghezza con marker [...troncato per limite contesto...]. Default 200. |
| `agent.context.auto_compact_enabled` | `true` | Flag master auto-compact. Se true (default), prima di ogni nuovo turno agente il sistema valuta il rapporto token sessione / context window del modello attivo e, se >= agent.context.auto_compact_ratio, compatta automaticamente la sessione (stesso meccanismo del pulsante "Compatta chat"). Se false, il compact resta solo manuale. |
| `agent.context.auto_compact_ratio` | `0.80` | Soglia ratio = session_tokens / context_window oltre la quale (>=) scatta l'auto-compact prima del turno agente. Default 0.80. Range valido [0.5, 0.95]: il codice clampa i valori fuori range. Token sessione = somma dei total/prompt tokens dei messaggi non soft-deleted (deleted_at IS NULL), con stima a ~4 char/token quando i token non sono persistiti. context_window dal catalog ai_price_catalog del modello risolto per il turno. |
| `agent.context.compress_phase_boundaries` | `5,10,20,50` | CSV crescente dei boundary di fase per compressione escalante. iter < primo = no compressione. Tra phase[i] e phase[i+1] = applica keep_recent[i] e max_chars[i]. Le tre liste boundaries/keep_recent/max_chars devono avere stessa lunghezza. |
| `agent.context.compress_phase_keep_recent` | `8,5,3,2` | CSV keep_recent per ogni fase di compressione (allineato a compress_phase_boundaries). |
| `agent.context.compress_phase_max_chars` | `2000,1000,500,150` | CSV max_content_chars per ogni fase di compressione (allineato a compress_phase_boundaries). |
| `agent.context.compress_start_iter` | `5` | Iterazione di executor a partire dalla quale attivare la compressione escalante dei tool_result. Prima viene applicata solo la dedup. Default 5 (FIX A). |
| `agent.context.dedup_tool_results_enabled` | `true` | Se true (default) ogni iter executor applica _dedup_tool_results_history: tool_result vecchi con stessa signature (sha256(tool_name+args_json)) vengono sostituiti con placeholder, tenendo solo l ultimo. FIX B. |
| `agent.context.drop_unused_base64_age` | `3` | Soglia (n messaggi successivi) entro la quale verificare se un blob base64 di un tool_result vecchio viene citato testualmente. Se non viene citato, il body base64 viene sostituito con un placeholder. FIX C. |
| `agent.context.max_chars` | `400000` | Budget chars totale per il contesto agente. Oltre questa soglia i tool result vecchi vengono compressi/sommarizzati. Approx 3.5 chars/token. DB-driven, cache 60s. |
| `agent.context.max_context_ratio` | `0.70` | Soglia (0.4-0.9) sul context_window del modello attivo: se la stima token del contesto in executor supera ratio*context_window, scatta la compressione aggressiva TOKEN-based che tronca anche i messaggi assistant lunghi. Default 0.70. |
| `agent.context_offload_ttl_seconds` | `60` | TTL cache context_offload (H-55) |
| `agent.context.predictive_cap_ratio` | `0.5` | Soglia (0.3-0.9) sul context_window del modello: se context_attuale + stima_tool_result supera ratio*context_window, la chiamata al tool viene intercettata e sostituita da tool_result sintetico di errore. FIX D. |
| `agent.context.rag_offload.enabled` | `true` | Flag master offload RAG lossless. Se true (default), prima di troncare/comprimere/scartare un tool result o messaggio vecchio il brain indicizza il contenuto COMPLETO in Qdrant (tool_results_chunks) cosi' nessun dato viene perso e resta recuperabile via nexus_search_semantic. Se false, degrada al vecchio troncamento distruttivo. |
| `agent.context.rag_offload.max_chunks_per_item` | `500` | Numero massimo di chunk indicizzati per singolo contenuto offloadato (anti-abuso: un file enorme non deve generare migliaia di point in un colpo). Oltre il cap il resto NON viene indicizzato e l'evento e' loggato come WARN. Default 500. |
| `agent.context.rag_offload.min_chars` | `2000` | Soglia minima caratteri sotto la quale NON si indicizza un contenuto in RAG: sotto soglia il contenuto sta gia' intero nel prompt, nessuna perdita possibile. Default 2000. |
| `agent.context.rag_offload.snippet_max_chars` | `4000` | Limite caratteri per ogni snippet RAG incluso nel contesto. Alzato da 400 (vecchio hardcoded) a 4000: snippet piu' ampi riducono i round-trip e non perdono il cuore del match. |
| `agent.context.rag_offload.top_k` | `12` | Numero di interazioni/snippet recuperati dal RAG inline per turno. Alzato da 5 (vecchio hardcoded) a 12: con l'offload lossless il RAG e' la fonte di verita' del contenuto troncato, quindi il recupero non deve essere artificialmente stretto. |
| `agent.context_window_ttl_seconds` | `120` | TTL cache context window per modello (H-35) |
| `agent.ctx_mgmt_ttl_seconds` | `60` | TTL cache context management (H-34) |
| `agent.db_query_timeout_seconds` | `5` | Timeout query DB nei nodi agente (H-39, H-44) |
| `agent.dev_diagnostics.max_findings` | `50` | Max findings per nexus_dev_server_diagnose (H-70 a) |
| `agent.dev_diagnostics.max_log_bytes` | `200000` | Max byte log dev_diagnostics (H-70 b) |
| `agent.enforce_port_allocation` | `true` | Se true, write_file/edit_file rifiutano sorgenti con porte TCP hardcoded fuori dal bucket Nexus 20000-39999 (vedi ADR 0010). |
| `agent.expand_max_tokens` | `512` | max_tokens per chiamata expand (H-41) |
| `agent.exploration_loop.default_threshold` | `6` | Soglia exploration tool consecutive per loop detection (H-27 b) |
| `agent.exploration_loop_threshold` | `6` | Numero di chiamate consecutive a tool di sola esplorazione (lettura/ispezione allegati e file) oltre il quale l'executor inietta un nudge verso la scrittura; a 2x la soglia abortisce. Una call produttiva (write_file, edit_file, run_command, request_port, ...) azzera il contatore. Intero >= 1. |
| `agent.exploration_loop.ttl_seconds` | `60` | TTL cache loop detector esplorazione (H-27 a) |
| `agent.fallback.soft_failure_enabled` | `true` | Abilita detection soft failure (M4) |
| `agent.figma.min_string_len` | `4` | Min char stringa estratta da figma (H-71) |
| `agent.final_gate.enabled` | `true` | Abilita il final gate generale fail-closed (anti-placeholder) per i task software senza plan_phase. |
| `agent.final_gate.max_cycles` | `2` | Numero massimo di cicli di retry del final gate prima di chiudere comunque (no loop infinito). |
| `agent.final_gate.software_intents` | `code,debug,scaffold,implement,build,frontend,fix,refactor` | CSV degli intent considerati task software per cui il final gate si attiva. |
| `agent.firstturn.canonical_hint` | `true` | Inietta hint canonico nel first turn per allegati strutturati (M8) |
| `agent.firstturn.tool_choice_force` | `true` | Forza tool_choice strict al first turn quando ci sono allegati strutturati |
| `agent.g1_nudge.default_max` | `3` | Max nudge G1 anti-narration per run (H-26 b) |
| `agent.g1_nudge.ttl_seconds` | `60` | TTL cache G1 anti-narration nudge (H-26 a) |
| `agent.import_staging_dirs` | `figma_export` | CSV delle directory di staging del codice importato (design) controllate dal gate no_orphan_imported. |
| `agent.iteration_budget.base` | `40` | Numero base iterazioni LangGraph per ogni run agente. Sommato a per_complexity_point*complexity_score. |
| `agent.iteration_budget.max` | `100` | Tetto massimo iterazioni anche per task molto complessi. Safety net runaway. |
| `agent.iteration_budget.per_complexity_point` | `4` | Iterazioni aggiuntive per ogni punto di complessita del prompt (score 0-100). |
| `agent.lang_reminder_ttl_seconds` | `60` | TTL cache language reminder (H-28) |
| `agent.language_reminder_enabled` | `true` | Abilita l'iniezione del reminder di lingua resiliente al contesto in coda al system prompt e all'ultimo messaggio utente (bug #88). Disabilita con "false" per rollback immediato senza rideploy. |
| `agent.language_reminder_text` | `Rispondi SEMPRE e SOLO in italiano. Mai cinese, giappones...` | Testo del reminder di lingua iniettato in coda al system prompt e all'ultimo messaggio utente per vincere il recency bias dei modelli small a contesto saturo (bug #88). |
| `agent.learning_cfg_ttl_seconds` | `60` | TTL cache learning config (H-32) |
| `agent.loop_detector_max_tokens` | `400` | max_tokens per chiamata LLM del loop detector (H-38) |
| `agent.meta_steps_flag_ttl_seconds` | `60` | TTL cache meta_steps flag (H-51) |
| `agent_narration_warn_after_chars` | `1500` | Caratteri di testo streamed senza tool call dopo i quali il badge UI passa in stato warning. |
| `agent_narration_warn_after_ms` | `30000` | Millisecondi di run senza tool call dopo i quali il badge UI passa in stato warning (possibile loop di narrazione). |
| `agent.no_orphan.min_ratio` | `0.4` | Frazione minima di moduli staged che l'entry servito deve raggiungere via grafo import per superare il gate. |
| `agent.orchestrator_cfg_ttl_seconds` | `60` | TTL cache orchestrator_config (H-56) |
| `agent_parallel_enabled` | `true` | Abilita l'esecuzione parallela di piu' agenti contemporaneamente per accelerare task complessi |
| `agent_parallel_max` | `5` | Numero massimo di agenti paralleli per sessione (1-5) |
| `agent.planner.full_max_tokens` | `4096` | max_tokens per planner completo (H-43) |
| `agent.planner.rationale_snippet_max` | `400` | Max char snippet rationale nel planner (H-42) |
| `agent.planner.short_max_tokens` | `512` | max_tokens per planner short (H-45) |
| `agent.port_gc.dedupe_dev_servers` | `true` | Se true, il GC termina i dev-server duplicati (Vite/Next/pnpm dev) per progetto, tenendo solo la istanza piu' recente. |
| `agent.port_gc.grace_seconds` | `180` | Grace period (secondi) prima di rilasciare un'allocazione porta dynamic senza listener. |
| `agent.port_gc.interval_seconds` | `120` | Intervallo (secondi) del GC delle porte orfane in mcp-core. |
| `agent.price_cache_ttl_seconds` | `300` | TTL cache prezzi modelli per cost estimation (H-30) |
| `agent.rag_min_score` | `0.5` | Soglia minima score RAG per inclusione contesto (H-33) |
| `agent.reasoning_bank.plan_reward_threshold` | `0.85` | Soglia reward per accettare un plan in reasoning_bank (H-53) |
| `agent.reflection_cfg_ttl_seconds` | `60` | TTL cache reflection_config (H-52) |
| `agent_router_enabled` | `true` | Abilita il server gRPC AgentRouter (porta 50072) che espone il router Q-Learning di nexus-orchestrator al brain Python. Quando attivo, il router_node consulta il Q-Learning per scegliere il profilo agente ottimale (es. coder, cloud_architect, tech_writer) in base alla cronologia dei reward osservati. Se disabilitato il brain usa il routing di fallback basato solo sull'intent. Richiede riavvio di mcp-core per applicare la modifica. |
| `agent.subagent.default_max_iterations` | `25` | Max iterations default per subagent (H-50, era in yaml loader) |
| `agent.summarizer.keep_recent` | `6` | Numero messaggi recenti preservati integralmente in summarization (H-47) |
| `agent.summarizer.max_tokens` | `800` | max_tokens per summarization (H-48) |
| `agent.summarizer.temperature` | `0.0` | Temperature LLM call summarizer (H-49 b) |
| `agent.summarizer.timeout_seconds` | `15` | Timeout LLM call summarizer (H-49 a) |
| `agent.summarizer.trigger_fraction` | `0.60` | Fraction del context window oltre cui triggerare summary (H-46) |
| `agent.thinking_cfg_ttl_seconds` | `60` | TTL cache thinking_config (H-54) |
| `agent.thinking_config_ttl_seconds` | `60` | TTL cache nexus_thinking config (H-29) |
| `agent.todos.carry_over_enabled` | `true` | M15.4: a fine run i todo pending/blocked vengono marcati carry_over=true (con origin_run_id) invece di restare orfani, cosi' il planner del run successivo li eredita come backlog. |
| `agent.todos.live_events` | `true` | M15.1: emette eventi SSE live (TodoUpdated per todo + PlanUpdated finale) quando lo status di un todo cambia, dopo il commit della transazione. |
| `agent.todos.user_editable` | `true` | M15.3: abilita l'endpoint POST /api/agent/todos/{run_id}/edit per modificare i todo del piano dall'interfaccia utente (add/edit/reorder/remove). |
| `agent.tools.core_whitelist` | `read_file,write_file,edit_file,list_files,search_in_files...` | CSV dei tool CORE essenziali sempre esposti col tool tiering (slim, mig 0254). Gli altri restano scopribili via nexus_mcp_tool_search. Set ridotto per stabilita function-calling Gemini 2.5. |
| `agent.tools.discovery_first_enabled` | `true` | M16: primo turno espone SOLO i tool di discovery (nexus_mcp_tool_search/call); i tool trovati diventano native per il turno successivo. Default ON dopo verifica E2E (mig 0257). |
| `agent.tools.discovery_first_whitelist` | `nexus_mcp_tool_search,nexus_mcp_tool_call,list_files,read...` | Tool esposti al primo turno quando discovery-first e' attivo (CSV). Include i meta di discovery + i tool core del filesystem sempre disponibili (lettura/scrittura/comando). Gli altri tool restano scopribili via nexus_mcp_tool_search. |
| `agent.tools.discovery_max_injected` | `20` | Numero massimo di tool scoperti via nexus_mcp_tool_search iniettati come native nel turno successivo. |
| `agent.tools.discovery_schema_max_bytes` | `8192` | Cap dimensione (byte) dell input_schema di un singolo tool scoperto, per isolare schemi malformati da plugin esterni. |
| `agent.tools.tiering_enabled` | `true` | Abilita il tool tiering: invia al modello solo il CORE di tool + discovery (nexus_mcp_tool_search/call). Disattivare per esporre tutti gli 80 tool. |
| `agent.verifier.fail_closed` | `true` | Se true il verifier_node, in assenza di acceptance_criteria sul todo software, esegue comunque i gate generali invece di marcare completed. |
| `agent.vision.image_max_bytes` | `2097152` | Max byte immagine per vision describe (H-69) |
| `agent.visual_compare.screenshot_timeout_secs` | `45` | Timeout (secondi) per la cattura dello screenshot via Playwright in nexus_visual_compare (launch + goto + wait + scatto). Default 45. |
| `agent.visual_compare.similarity_threshold` | `85` | Soglia di similarita' (0-100) raccomandata: sotto questa soglia, o in presenza di differenze severita' alta, l'agente in modalita' Continuo dovrebbe correggere stile/layout e ripetere nexus_visual_compare. Default 85. |
| `agent.visual_compare.viewport_height` | `800` | Altezza (px) del viewport usato da nexus_visual_compare per lo screenshot quando il parametro viewport non e' passato. Default 800. |
| `agent.visual_compare.viewport_width` | `1280` | Larghezza (px) del viewport usato da nexus_visual_compare per lo screenshot quando il parametro viewport non e' passato. Default 1280. |
| `agent.visual_compare.wait_ms` | `1500` | Attesa (ms) dopo il load della pagina prima dello scatto in nexus_visual_compare, per dare tempo a render/animazioni/fetch. Default 1500. Override per chiamata via parametro wait_ms. |
| `agent.watchdog.enabled` | `true` | Abilita il watchdog generale dei microservizi (TCP probe + auto-restart in dev). |
| `agent.watchdog.fail_threshold` | `2` | Numero di cicli down CONSECUTIVI prima di tentare il riavvio di un servizio. |
| `agent.watchdog.interval_seconds` | `30` | Intervallo (secondi) tra i cicli di probe del watchdog servizi. |
| `agent.watchdog.max_consecutive_restarts` | `5` | Riavvii consecutivi falliti oltre i quali il servizio e' considerato irrecuperabile (stop tentativi, log ERROR). |
| `agent.watchdog.restart_cooldown_seconds` | `120` | Cooldown (secondi) dopo un riavvio prima di poter ritentare lo stesso servizio. |
| `agent.watchdog.services` | `[{"name":"brain","port_setting_key":"brain_rest_port"},{"...` | Lista JSON dei microservizi monitorati dal watchdog. name = nome per deploy-local.sh --service; port_setting_key = chiave settings da cui risolvere la porta (regola G). mcp-core escluso (ospita il watchdog). |
| `anthropic_system_cache_ttl` | `1h` | TTL della cache prompt di sistema per Anthropic: 5m (default Anthropic) o 1h (extended-cache-ttl-2025-04-11). Il valore 1h massimizza il cache hit rate fra turni distanti (il system prompt cambia raramente). Override: NEXUS_ANTHROPIC_SYSTEM_CACHE_TTL. Richiede riavvio del brain. |
| `attachment.kb_excerpt_max_bytes` | `16384` | Max byte excerpt salvato in kb note per attachment (H-79 a) |
| `attachment.sanitized_filename_max_len` | `120` | Max char filename sanitizzato per attachment (H-79 b) |
| `catalog_sync.disable_missing` | `true` | Se TRUE, disabilita i modelli del catalog non piu esposti dall API. Se FALSE solo log. |
| `catalog_sync.enabled` | `true` | Attiva/disattiva il worker periodico di sync catalog modelli dai provider. |
| `catalog_sync.insert_new_disabled` | `true` | Se TRUE, modelli nuovi vengono inseriti con is_enabled=false (admin verifica prezzi prima di abilitare). |
| `catalog_sync.interval_hours` | `6` | Intervallo (ore) tra i tick del worker. Default 6 = 4 sync al giorno. |
| `catalog_sync.providers` | `anthropic,openai,mistral,deepseek,google` | Provider per cui eseguire l'auto-discovery dei modelli (CSV). Google passa per brain REST /providers/google/models/live (Vertex SDK). |
| `extended_thinking_budget_tokens` | `8000` | Budget massimo di token interni di ragionamento per turno quando extended_thinking_enabled=true. Range consigliato: 2000-16000. Valori piu' alti migliorano la qualita' ma aumentano i costi. |
| `extended_thinking_enabled` | `false` | Abilita il ragionamento interno esteso (Extended Thinking) di Anthropic sui modelli Sonnet/Opus. Genera token di ragionamento interni billati al prezzo output. Disabilitato di default per contenere i costi. Attivare solo per task che richiedono ragionamento profondo. |
| `llm_classifier_enabled` | `true` | Abilita il classificatore LLM degli intent (chiamata REST /classify-intent-agentic al brain Python). Se false usa solo keyword matching locale: piu' veloce ma meno preciso. Override di emergenza: NEXUS_LLM_CLASSIFIER_ENABLED=false. Richiede riavvio di mcp-core. |
| `terminal_default_shell` | `bash` | Shell di default per i terminali agente: bash, zsh, fish. Su Windows: powershell.exe. Override di emergenza: TERMINAL_SHELL. Richiede riavvio del brain e di mcp-core. |
| `tool_runner_enabled` | `true` | Abilita il server gRPC ToolRunner (porta 50071) usato dal brain LangGraph per eseguire i tool Nexus builtin. Override di emergenza: ENABLE_TOOL_RUNNER=1. Richiede riavvio di mcp-core per applicare la modifica. |

## `agent_tools`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `agent.edit_file.whitespace_tolerant` | `true` | Retry fuzzy whitespace su edit_file se match esatto fallisce (M8) |
| `agent_tools.read_cache_capacity` | `256` | LRU read cache capacity (H-66) |
| `agent_tools.read_file_hint_lines` | `300` | Soglia righe oltre cui read_file antepone structure hint (H-62) |
| `agent_tools.read_file_lines_max` | `100000` | Max righe leggibili con read_file_lines in singola chiamata (H-63) |
| `agent_tools.test_log_max_chars` | `200000` | Max char log output test (H-72) |

## `ai`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `model_catalog_sync_enabled` | `true` | Abilita il worker periodico che chiama run_catalog_sync (sync da LiteLLM GitHub). |
| `model_catalog_sync_interval_s` | `43200` | Intervallo in secondi tra sync catalog (default 12h, minimo 3600). |
| `model_health_probe_enabled` | `true` | Abilita il worker model_health_probe che pinga ogni modello enabled in catalog. |
| `model_health_probe_failure_threshold` | `3` | Numero di fallimenti consecutivi (model-specific) prima dell auto-disable. |
| `model_health_probe_interval_s` | `1800` | Intervallo in secondi tra cicli di probe (default 30 min, minimo 300). |
| `routing_matrix_auto_promote_enabled` | `true` | Abilita il worker che ricostruisce la routing matrix dal catalog. |
| `routing_matrix_auto_promote_interval_s` | `21600` | Cadenza ricalcolo routing matrix (default 6h, minimo 600). |

## `auth`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `github_client_id` | `Ov23licRITTczhgfA6aq` | GitHub OAuth App Client ID |
| `github_client_secret` | `9fc651c50d1a11f29eb865e579269126ffe9fb88` | GitHub OAuth App Client Secret |
| `jwt_secret` | `ecc559dd332908e3e73d06b6acb5c67fb8438c984a2797e1d7b417d17...` | JWT signing secret (auto-generated on first login) |
| `oauth_data_encryption_key` | `091f6d8fa79d6931c8562b2292c3a49fdc59f4155fd2c3a94f0e6519c...` | Secret per cifrare i token OAuth salvati a riposo |

## `automation`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `automation.study_mode_readonly_tools` | `read_file,read_file_lines,list_files,search_in_files,sear...` | CSV di tool esposti all'agente in automation_mode=study (gating difensivo: anche se l'LLM ignora il prompt di mode, NON puo' chiamare tool fuori da questa whitelist). |

## `billing`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `brain_billing_enabled` | `true` | Abilita la registrazione di utilizzo AI nel ledger billing (tabella ai_usage_ledger) dal brain Python. Tenere false in sviluppo locale per non inquinare i dati di produzione. Override di emergenza: NEXUS_BRAIN_BILLING=on. Richiede riavvio del brain. |

## `claude_agents`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `claude_agents.export_enabled` | `true` | Abilita la generazione dei file .claude/agents/*.md dalle definizioni DB. |
| `claude_agents.name_prefix` | `nexus-` | Prefisso del nome file/agente generato (kind rust_implementer -> nexus-rust-implementer.md). |
| `claude_agents.output_dir` | `.claude/agents` | Directory di output (relativa a NEXUS_REPO_ROOT) per i file agente generati. |
| `claude_agents.overwrite_unmanaged_default` | `false` | Se false, i file senza marker AUTO-GENERATO (curati a mano) NON vengono sovrascritti dalla rigenerazione di default. |
| `claude_agents.regen_on_post_commit` | `false` | Se true, un hook post-commit rigenera i file (opzionale; default off per non rallentare i commit). |

## `connectors`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `figma_client_id` | `` | OAuth Client ID Figma per MCP remote |
| `figma_client_secret` | `` | OAuth Client Secret Figma per MCP remote |
| `figma_last_oauth_error` | `` | Ultimo errore OAuth Figma |
| `figma_mcp_prefer_stdio` | `false` | Se true usa fallback stdio (figma-developer-mcp) invece di endpoint MCP HTTP |
| `figma_oauth_redirect_uri` | `` | Override callback OAuth Figma (default: backend/auth/figma/mcp/callback) |
| `figma_oauth_token` | `` | Token Figma (PAT figd_... o OAuth access token) usato dal plugin Figma MCP |
| `figma_refresh_token` | `` | Refresh token OAuth Figma |
| `figma_region` | `us-east-1` | Header X-Figma-Region per plugin MCP Figma |
| `figma_token_expires_at` | `` | Scadenza token OAuth Figma (ISO8601) |
| `figma_token_scope` | `` | Scope token OAuth Figma |
| `github_personal_access_token` | `` | Token GitHub personale per plugin GitHub MCP (Authorization: Bearer ...) |
| `github_token` | `` | Alias token GitHub per integrazioni MCP/legacy |
| `gitlab_personal_access_token` | `` | Token GitLab personale per plugin GitLab MCP |

## `embeddings`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `embedding_model` | `all-MiniLM-L6-v2` | Sentence-transformers model |

## `gateway`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `default_max_tokens` | `4096` | Token massimi di default per completion (se non specificati nella richiesta) |
| `health_check_interval_ms` | `60000` | Intervallo health check provider (ms) |
| `http_pool_max` | `20` | Numero massimo di connessioni idle per host nel pool HTTP Nexus (default 20). Aumentare in ambienti ad alto parallelismo (>10 utenti simultanei). Override: NEXUS_HTTP_POOL_MAX. Richiede riavvio di mcp-core. |
| `http_timeout_secs` | `30` | Timeout in secondi per il client HTTP Nexus verso i provider LLM e i servizi interni (default 30). Aumentare se si usano modelli lenti con streaming disabilitato. Override: NEXUS_HTTP_TIMEOUT_SECS. Richiede riavvio di mcp-core. |
| `nexus_profile` | `cloud` | Profilo operativo del gateway LLM: cloud, onprem, hybrid. Determina quale file di policy viene caricato da config/policies/ e quali tier di modelli sono consentiti. Override: NEXUS_PROFILE. Richiede riavvio di mcp-core. |
| `rate_limit_per_provider_requests` | `500` | Max richieste per provider per finestra temporale |
| `rate_limit_per_provider_window_ms` | `60000` | Durata finestra rate limit provider (ms) |
| `rate_limit_per_tenant_requests` | `1000` | Max richieste per tenant per finestra temporale |
| `rate_limit_per_tenant_window_ms` | `60000` | Durata finestra rate limit tenant (ms) |
| `supervisor_model` | `gemini-2.5-flash` | Modello usato dal supervisor AI |
| `supervisor_provider` | `google` | Provider usato dal supervisor AI (es: google, anthropic) |

## `general`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `agent.rag.chunk_overlap` | `200` | Overlap caratteri fra chunk consecutivi per la pipeline RAG. |
| `agent.rag.chunk_size` | `1000` | Dimensione caratteri di un chunk per la pipeline RAG. |
| `agent.rag.collection_attachments` | `attachment_chunks` | Nome collection Qdrant per chunks allegati. |
| `agent.rag.collection_chat_history` | `chat_history_chunks` | Nome collection Qdrant per chunks history chat. |
| `agent.rag.collection_kb` | `kb_chunks` | Nome collection Qdrant per chunks knowledge base. |
| `agent.rag.collection_tool_results` | `tool_results_chunks` | Nome collection Qdrant per chunks tool results di grandi dimensioni. |
| `agent.rag.embedding_dim` | `384` | Dimensione vettori embedding (all-MiniLM-L6-v2 = 384). |
| `agent.rag.embedding_endpoint` | `/embed` | Path REST sul brain per ottenere embeddings batch. Canonico /embed (riusa EmbeddingService). |
| `agent.rag.enabled` | `true` | Abilita la pipeline RAG strutturale per allegati/KB/chat-history/tool-results. |
| `agent.rag.qdrant_url` | `http://localhost:6333` | URL Qdrant per le collection RAG (attachment_chunks, kb_chunks, ecc.). |
| `agent.rag.top_k_default` | `8` | Numero default di hit ritornati da search_semantic se top_k non specificato. |
| `automation.o_series_essential_tools` | `read_file,read_file_lines,list_files,search_in_files,writ...` | Tool essenziali esposti ai modelli o-series (o1/o3/o4-mini). Gli altri tool sono disponibili via nexus_mcp_tool_search. CSV. |
| `knowledge.autolink_threshold` | `0.45` |  |
| `knowledge.cleanup_draft_days` | `30` |  |
| `knowledge.commit_vault_to_git` | `false` |  |
| `knowledge.link_worker_interval_secs` | `600` |  |
| `knowledge.similarity_banner_threshold` | `0.80` |  |
| `knowledge.vault_watcher_debounce_ms` | `500` |  |
| `nexus_app_admin_password` | `nexus_admin_secret` |  |
| `nexus_app_admin_user` | `nexus_admin` |  |
| `nexus_app_db_host` | `localhost` |  |
| `nexus_app_db_password` | `nexus_app_dev_secret` |  |
| `nexus_app_db_port` | `5434` |  |
| `nexus_app_db_user` | `nexus_app` |  |
| `orchestrator.auto_delegation_enabled` | `true` |  |
| `orchestrator.clarifying_questions_enabled` | `true` |  |
| `orchestrator.clarifying_questions_max` | `3` |  |
| `orchestrator.project_instructions_file` | `.nexus/project-instructions.md` |  |
| `orchestrator.project_instructions_max_chars` | `8000` |  |
| `orchestrator.subagent_parallel_in_round` | `true` |  |
| `orchestrator.subagent_project_override_enabled` | `true` |  |
| `sandbox_cpus` | `2` | Limite CPU sandbox (core) |
| `sandbox_enabled` | `true` | Abilita isolamento Docker per i processi agente |
| `sandbox_memory_mb` | `1024` | Limite memoria sandbox in MB |

## `impact`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `impact.depth_cap` | `2` | Profondita' massima di traversal nella forward closure dell'impact analysis (M13.4). |
| `impact.enabled` | `true` | Abilita il popolamento del code graph durante reindex_single_file (M13.1). |
| `impact.max_nodes` | `60` | Numero massimo di nodi raccolti in una singola impact run (anti-esplosione). |
| `impact.test_informed_enabled` | `true` | Abilita il blocco <impact_brief> nel planner (M13.6): il planner vede impact set e test esistenti e genera todo di test/verifica mirati. |
| `impact.test_informed_max_listed_tests` | `15` | Numero massimo di test esistenti elencati nel blocco <impact_brief> (anti-rumore nel prompt del planner). |
| `impact.test_informed_max_seed_paths` | `12` | Numero massimo di seed path (file citati dall utente) inviati a tests-for-run in fase di planning. |

## `infrastructure`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `admin_service_port` | `4010` | Porta HTTP del microservizio admin-service (default 4010). Override: ADMIN_SERVICE_PORT. |
| `agent_router_addr` | `127.0.0.1:50501` | Indirizzo host:porta del server gRPC AgentRouter esposto da mcp-core e usato dal brain Python per consultare il Q-Learning router. Override di emergenza: AGENT_ROUTER_ADDR. Richiede riavvio di mcp-core e del brain. |
| `billing_service_port` | `4040` | Porta HTTP del microservizio billing-service (default 4040). Override: BILLING_SERVICE_PORT. |
| `brain_grpc_port` | `50051` | Porta del server gRPC del brain Python (NeuralCoreService): classifier, embedding, routing. Default 50051. Override di emergenza: BRAIN_GRPC_PORT. Richiede riavvio del brain. |
| `brain_rest_port` | `8001` | Porta del server REST FastAPI del brain Python (default 8001). Override di emergenza: BRAIN_REST_PORT. Richiede riavvio del brain. Cambiandola serve aggiornare anche brain_rest_url. |
| `brain_rest_url` | `http://127.0.0.1:8001` | URL del server REST del brain Python (FastAPI su porta 8001). Usato da mcp-core per chiamare /agent/run/stream, /classify-intent-agentic, /catalog/sync e altri endpoint REST del brain. Override di emergenza: BRAIN_REST_URL o NEURAL_CORE_REST_URL. Richiede riavvio di mcp-core. |
| `browser_bridge_port` | `4055` | Porta HTTP del browser-bridge-mcp (default 4055). Override: BROWSER_BRIDGE_PORT. |
| `chat_service_port` | `4020` | Porta HTTP del microservizio chat-service (default 4020). Override: CHAT_SERVICE_PORT. |
| `doc_service_port` | `4030` | Porta HTTP del microservizio doc-service (default 4030). Override: DOC_SERVICE_PORT. |
| `mcp_core_http_port` | `4000` | Porta HTTP del server REST mcp-core (default 4000). Override di emergenza: MCP_SERVER_PORT o MCP_CORE_HTTP_PORT. Richiede riavvio di mcp-core. Cambiandola servono anche aggiornamenti a mcp_core_url e web-ide proxy. |
| `mcp_core_url` | `http://127.0.0.1:4000` | URL del server HTTP mcp-core (porta 4000). Usato dal brain Python per leggere settings via _get_core_setting(), dal router semantico, dal cooldown bridge e dall'agent router client. Override di emergenza: MCP_CORE_URL. Richiede riavvio del brain. |
| `network_dns_servers` | `1.1.1.1,8.8.8.8 ` | Server DNS personalizzati separati da virgola (es. 8.8.8.8,1.1.1.1). Usato dal Neural Core per risolvere i nomi host verso API AI esterne. |
| `neural_core_url` | `http://localhost:50051` | Neural Core gRPC URL |
| `nexus_external_proxy` | `` | Proxy HTTP/HTTPS per le chiamate verso API esterne (es. http://localhost:8002). Usato da tutti i backend Nexus tramite NEXUS_PROXY. Lascia vuoto per connessione diretta. |
| `nexus_gateway_port` | `4060` | Porta HTTP del nexus-gateway (Node.js, proxy LLM unificato). Default 4060. Override di emergenza: NEXUS_GATEWAY_PORT. |
| `plugin_service_port` | `4050` | Porta HTTP del microservizio plugin-service (default 4050). Override: PLUGIN_SERVICE_PORT. |
| `projects_base_root` | `/home/administrator/projects` | Root assoluta sotto cui e' consentita la registrazione/navigazione dei progetti |
| `qdrant_collection` | `code_embeddings` | Qdrant collection name |
| `qdrant_project_context_collection` | `project_context` | Qdrant collection per indicizzazione iniziale del contesto/storia progetto |
| `qdrant_url` | `http://localhost:6333` | Qdrant vector DB URL |
| `redis_url` | `redis://localhost:6379` | Redis connection URL |
| `tool_runner_addr` | `127.0.0.1:50500` | Indirizzo host:porta del server gRPC ToolRunner esposto da mcp-core e usato dal brain Python per eseguire i tool MCP (read_file, write_file, run_command, ecc.). Entrambi i servizi leggono questo valore. Override di emergenza: TOOL_RUNNER_ADDR. Richiede riavvio di mcp-core e del brain. NOTA: deve essere DIVERSO da agent_router_addr (porte distinte). |
| `web_ide_port` | `3000` | Porta HTTP del frontend web-ide Next.js (default 3000). Override di emergenza: WEB_APP_PORT o PORT. |

## `kb`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `kb.autolink.enabled` | `true` | Abilita il link composer post-create note (M12.3). |
| `kb.autolink.semantic_threshold` | `0.65` | Score minimo Qdrant per creare un link relates semantico. |
| `kb.autolink.semantic_top_k` | `3` | Top-K note semanticamente simili da considerare per link relates. |
| `kb.autolink.wikilink_max_per_note` | `10` | Cap wikilink esplicitamente risolti per nota (anti-DoS). |
| `kb.changelog_cross_enabled` | `true` | Abilita il cross-link dei changelog del meta-vault Nexus nella KB del meta-progetto Nexus (M12.4). No-op se Nexus non e' registrato come progetto. |
| `kb.code_doc.enabled` | `true` | W2: abilita la generazione della code-wiki (note code_doc per file). |
| `kb.code_doc.max_file_bytes` | `200000` | Dimensione massima file (byte) considerato dalla code-wiki. |
| `kb.code_doc.max_files` | `50` | Numero massimo di file documentati per esecuzione della code-wiki. |
| `kb.code_doc.max_source_chars` | `12000` | Caratteri di sorgente inviati all'LLM per file (troncamento). |
| `kb.ingest.body_max_chars` | `20000` | Max char del body_md ingestito (final_answer molto lunghi vengono troncati con suffisso). |
| `kb.ingest.enabled` | `true` | Abilita ingestione automatica del final_answer in project_knowledge_notes (M12.1). |
| `kb.ingest.min_chars` | `300` | Lunghezza minima del final_answer per essere ingestito come note (filtro substance). |
| `kb.ingest.title_max_chars` | `120` | Max char per il title della note generato dal final_answer. |
| `kb.intake.confirm_if_implemented` | `true` | M14.4: se true, una richiesta gia' implementata e verificata (contesto invariato) chiede conferma anche in modalita' automatica prima di rifarla. |
| `kb.lifecycle.auto_deprecate_on_correction` | `true` | M14.2: quando una richiesta utente corregge una decisione esistente (verdetto intake correction) e il run completa, marca la nota vecchia deprecated e crea un link correction dalla nuova nota. |
| `kb.lifecycle.context_stale_enabled` | `true` | M14.3: marca context-stale le note active i cui file coperti vengono modificati da un run successivo non collegato alla nota (segnalazione, non cancellazione). |
| `kb.lifecycle.promote_enabled` | `true` |  |

## `knowledge`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `kb.ingest.cjk_max_ratio_pct` | `20` | Hallucination guard kb.ingest: se >= N percento dei caratteri della final_answer e CJK (hiragana, katakana, hangul, hanzi), la nota agent_summary NON viene creata (probabile deriva semantica). 0 = disabilitato. |
| `knowledge.cleanup_inactive_days` | `90` | Eta' (giorni) dall'ultimo updated_at oltre cui una nota active viene archiviata dal worker di cleanup (M14.5). |
| `knowledge.cleanup_inactive_enabled` | `false` | Gate M14.5 per l'archiviazione delle note active inattive. OFF di default: non archiviare note attive a sorpresa. L'archiviazione delle draft vecchie resta sempre attiva. |
| `knowledge.context_injection_enabled` | `true` | Abilita iniezione automatica delle note KB rilevanti nel system prompt agente |
| `knowledge.context_injection_min_score` | `0.5` | Soglia minima di similarita cosine 0-1 |
| `knowledge.context_injection_top_k` | `5` | Numero massimo di note KB da iniettare (1-20) |
| `knowledge.graph_import_autolink` | `true` | Comp.2: dopo l'import collega i nodi importati ai nativi (recompute_links). |
| `knowledge.graph_import_enabled` | `true` | Comp.2: abilita l'import di grafi esterni (JSON node-link, Mermaid, DOT) nella KB. |
| `knowledge.graph_import_max_nodes` | `2000` | Comp.2: numero massimo di nodi importabili in un singolo grafo. |
| `knowledge.rag_injection_mode` | `index` | Iniezione KB nel prompt: index (solo indice + tool on-demand, leggero) | full (snippet completi). |

## `learning`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `learning_auto_extract` | `true` | Auto-extract patterns from code |
| `learning_min_confidence` | `0.6` | Minimum pattern confidence to keep |
| `learning_prompt_corrections_enabled` | `true` | Enable runtime prompt corrections retrieval from vector memory |
| `vector_compaction_schedule_cron` | `0 2 * * *` | Daily vector compaction schedule (server local time) |

## `meta_docs`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `meta_docs.autofix_enabled` | `true` | Abilita NexusAutoFixAgent |
| `meta_docs.autofix_target_branch` | `main` | Branch base per le PR di autofix |
| `meta_docs.changelog_min_significance` | `0.4` | Soglia di significance LLM per generare entry changelog |
| `meta_docs.e2e_smoke_cron` | `0 2 * * *` | Cron schedule per smoke test notturno |
| `meta_docs.e2e_smoke_url` | `http://localhost:3000` | URL base per smoke test E2E di Nexus stesso |
| `meta_docs.enabled` | `true` | Abilita la generazione documentazione meta-progetto |
| `meta_docs.obsidian_vault_name` | `` | Nome del vault Obsidian registrato per docs/.nexus-vault/ (vuoto = non configurato) |
| `meta_docs.refresh_worker_interval_secs` | `900` | Failsafe refresh ogni N secondi (default 15 min) |
| `meta_docs.vault_path` | `docs/.nexus-vault` | Path relativo del vault dentro la repository Nexus |
| `meta_docs.watcher_debounce_ms` | `500` | Debounce file watcher su docs/.nexus-vault/ |

## `monitoring`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `provider_health_probe_enabled` | `true` | Abilita il worker di health-check periodico dei provider LLM. Ogni ciclo invia una richiesta minimale a ciascun provider configurato per rilevare cooldown / quota esaurita prima del primo errore reale. Override: NEXUS_PROVIDER_HEALTH_PROBE_ENABLED=false. Richiede riavvio di mcp-core. |
| `provider_health_probe_interval_s` | `300` | Intervallo in secondi tra i cicli di health-check provider (minimo 60, default 300 = 5 minuti). Abbassarlo aumenta la reattivita' ma aggiunge costo token marginale. Override: NEXUS_PROVIDER_HEALTH_PROBE_INTERVAL_S. Richiede riavvio di mcp-core. |

## `nexus_tools`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `nexus_tools.fs_read_max_bytes` | `262144` | Max byte fs_read (H-74) |
| `nexus_tools.fs_write_max_bytes` | `4194304` | Max byte fs_write (H-73) |
| `nexus_tools.http_response_max_bytes` | `2097152` | Max byte body HTTP response in http_request (H-75) |
| `nexus_tools.project_db_max_rows` | `100` | Max righe ritornate da project_db_query (H-76) |

## `optimizer`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `mcp_tool_search_hard_limit` | `200` | Numero minimo di tool nel catalogo oltre il quale il prompt usa solo nexus_mcp_tool_search (discovery mode, riduzione token). Default 20. |
| `optimizer_auto_promote` | `true` | Se true, il worker promuove automaticamente le varianti che superano il test statistico (Wilson score, p<0.05). Default false = dry-run. |
| `optimizer_canary_traffic_pct` | `10` | Percentuale di traffico inviata alla variante sperimentale durante il canary test (1-50). Default 10%. |
| `optimizer_enabled` | `true` | Abilita il PromptOptimizerWorker. Con auto_promote_enabled=false genera varianti ma non le promuove (dry-run). Kill switch globale. |
| `optimizer_max_concurrent_experiments` | `3` | Numero massimo di esperimenti running in contemporanea. Evita instabilita' globale da troppi canary simultanei. |
| `optimizer_min_runs` | `30` | Numero minimo di run per cohort prima di considerare un esperimento statisticamente valido. Cohort con meno run vengono ignorati. |
| `optimizer_reflection_threshold` | `0.65` | Soglia di avg_reflection_score sotto cui un prompt e' candidato all'ottimizzazione. Richiede Fase 2 (reflection) attiva. |
| `optimizer_rollback_threshold` | `0.15` | Se dopo la promozione il success_rate scende di piu' di questa percentuale rispetto alla baseline, scatta il rollback automatico. |
| `optimizer_success_rate_threshold` | `0.60` | Soglia di success_rate sotto cui un prompt e' candidato all'ottimizzazione automatica. |
| `prompt_optimizer_use_batch_api` | `true` | BP9: usa Batch API Anthropic per le varianti del prompt_optimizer (50% sconto token, latenza fino a 24h). |

## `orchestrator`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `clarify.confirm_irreversible_in_auto` | `true` | Se true, le decisioni di prodotto/irreversibili chiedono conferma anche in modalita' automatica; le tecniche/reversibili proseguono autonome. |
| `clarify.decision_lookup_enabled` | `true` | Se true, prima di chiedere un chiarimento cerca se la decisione e' gia' stata presa (note intent=decision) e la applica. |
| `clarify.decision_min_score` | `0.7` | Soglia minima di similarita' per considerare una decisione passata come gia' presa. |
| `clarify.decision_topk` | `5` | Quante note decision recuperare nel lookup. |
| `clarify.intake_gate_enabled` | `true` | Comp.1: abilita il gate di intake (classifica la relazione richiesta vs KB: nuova/duplicate/refinement/correction). Assorbe il decision-lookup del Cluster 4. |
| `clarify.intake_match_min_score` | `0.7` | Comp.1: soglia minima di similarita per considerare la richiesta correlata a una nota esistente. |
| `clarify.intake_topk` | `5` | Comp.1: numero di note candidate recuperate dal gate di intake. |
| `orchestrator.adaptive_agentic_score_min` | `0.85` | Soglia di agentic_score sopra la quale attivare il planner forte. |
| `orchestrator.adaptive_classifier_enabled` | `true` | Se true, router_node invoca il classifier agentico LLM e scrive complexity/agentic_score/is_ambiguous nello state. |
| `orchestrator.adaptive_gating_enabled` | `true` | Se true, is_eligible_adaptive usa i segnali del classifier per gate-are il planner forte (oltre ai gate hard budget/behavior). |
| `orchestrator.adaptive_low_confidence_max` | `0.5` | Soglia di confidence sotto la quale (incertezza) attivare il planner forte. |
| `orchestrator.clarify.confidence_threshold` | `0.6` | Soglia di confidence sotto cui il nodo si attiva. Sopra -> bypass. |
| `orchestrator.clarify.enabled` | `true` | Feature flag globale per il clarify_or_expand_node. Off -> nodo no-op. |
| `orchestrator.clarify.max_question_chars` | `280` | Cap di lunghezza della domanda di chiarimento prima del troncamento. |
| `orchestrator.clarify.prompt_key` | `agent.clarify.base` | Indirezione per varianti A/B del prompt clarify. |
| `orchestrator.clarify.require_llm_classifier` | `false` | Se true, attiva il clarify solo quando NEXUS_LLM_CLASSIFIER_ENABLED=true; altrimenti usa anche il fallback keyword/embedding. |
| `orchestrator.dag_max_parallel` | `2` | Comp.3b: numero massimo di todo eseguiti in parallelo per ondata (cap conservativo). |
| `orchestrator.dag_parallel_enabled` | `false` | Comp.3b: se true (e dag presente), i todo ready vengono eseguiti in parallelo via dispatch_subagents. Mutuamente esclusivo col worker-mode. |
| `orchestrator.dag_topological_enabled` | `true` | Comp.3a: se true, il verifier sceglie il prossimo todo rispettando depends_on (ordine topologico) invece del solo seq lineare. |
| `orchestrator.dag_verify_layer` | `true` | Comp.3b: se true, dopo ogni ondata parallela verifica i todo completati prima di procedere al layer successivo. |
| `orchestrator.exploratory_verify_enabled` | `true` | Se true, dopo i criteri deterministici passati il verifier esegue un controllo LLM esplorativo (RAG-informed) per anomalie non coperte. |
| `orchestrator.exploratory_verify_max_cycles` | `1` | Cap di cicli della verifica esplorativa per todo (anti-loop). Al cap si promuove comunque (deterministico primario). |
| `orchestrator.exploratory_verify_min_score` | `0.5` | Soglia minima di similarita' per i pattern di fallimento recuperati. |
| `orchestrator.exploratory_verify_topk` | `5` | Quanti pattern di fallimento passati recuperare via ricerca semantica. |
| `orchestrator.max_parallel_subagents` | `3` | Concorrenza max sub-agent in-flight per singolo parent run. |
| `orchestrator.max_plan_revisions` | `2` | Cap replan strutturali ammessi dopo verifier exhaustion (PR-2). |
| `orchestrator.max_verify_cycles` | `3` | Cap re-iterazioni executor<->verifier per singolo todo (PR-2). |
| `orchestrator.meta_steps.clarify_enabled` | `true` | Pubblica le richieste di chiarimento (Fase 2 clarify_or_expand) come meta_step kind=clarify. |
| `orchestrator.meta_steps.fallback_enabled` | `true` | Pubblica i fallback automatici di provider/modello come meta_step kind=fallback. |
| `orchestrator.meta_steps.global_enabled` | `true` | Kill-switch globale per i meta_step in chat (plan/routing/clarify/fallback/reflection). |
| `orchestrator.meta_steps.plan_enabled` | `true` | Pubblica il piano del planner_node come meta_step kind=plan. |
| `orchestrator.meta_steps.reflection_enabled` | `false` | Pubblica la riflessione post-hoc come meta_step kind=reflection. Off di default (costo LLM extra). |
| `orchestrator.meta_steps.routing_enabled` | `true` | Pubblica la decisione di routing/profile come meta_step kind=routing. |
| `orchestrator.plan_behavior_modes` | `bilanciata,approfondita,veloce,economica` | CSV dei behavior_mode che attivano il flusso plan/act/verify. |
| `orchestrator.plan_intents` | `code,implement,fix,refactor,scaffold_app,architecture` | CSV degli intent eleggibili per il planner. |
| `orchestrator.plan_min_token_budget` | `1500` | Sotto questa soglia di token_budget il planner viene saltato (chat brevi). |
| `orchestrator.planner_prompt_key` | `agent.planner.base` | Indirezione per varianti A/B del prompt del planner. |
| `orchestrator.plan_phase_enabled` | `true` | Feature flag globale per il planner_node (PR-1). Off -> grafo si comporta come oggi. |
| `orchestrator.plan_rationale_enabled` | `true` | Se true, il planner recupera decisioni passate via RAG, produce rationale/constraints/alternatives e li tramanda all'executor. |
| `orchestrator.plan_rationale_min_score` | `0.55` | Soglia minima di similarita' per includere una decisione passata nel contesto del planner. |
| `orchestrator.plan_rationale_persist_as_note` | `false` | Se true, dopo la creazione del piano il razionale viene salvato come nota knowledge intent=decision (chiude il ciclo RAG). |
| `orchestrator.plan_rationale_rag_topk` | `5` | Quante decisioni/interazioni passate recuperare per informare il razionale del planner. |
| `orchestrator.subagent_cost_cap_per_run_usd` | `5.00` | Hard cap di spesa cumulativa sub-agents per singolo parent run. |
| `orchestrator.subagent_default_timeout_s` | `300` | Timeout default per kind se non specificato in nexus_subagent_definitions. |
| `orchestrator.subagent_inherit_plan_rationale` | `true` | Se true, il sub-agent riceve il rationale del piano del parent (nexus_agent_plans), solo strutturato. |
| `orchestrator.subagent_kinds_whitelist` | `plan,explore,implement,verify,review,rust_implementer,pyt...` | CSV dei kind ammessi per dispatch_subagent (filtra anche custom kinds). |
| `orchestrator.subagent_max_depth` | `2` | Profondita max di annidamento sub-agent (sub-of-sub). |
| `orchestrator.subagent_rag_grounding_enabled` | `true` | Se true, i sub-agent ricevono un grounding sulla memoria vettoriale del progetto (ricerca semantica locale) nel system_text. |
| `orchestrator.subagent_rag_grounding_min_score` | `0.55` | Soglia minima di similarita' per il grounding del sub-agent. |
| `orchestrator.subagent_rag_grounding_snippet_max` | `800` | Cap caratteri per snippet del grounding (controllo costi + superficie dati verso il provider). |
| `orchestrator.subagent_rag_grounding_topk` | `5` | Numero di note recuperate per il grounding del sub-agent. |
| `orchestrator.subagents_enabled` | `true` | Feature flag globale sub-agents pattern. Off -> dispatch_subagent ritorna errore al main. |
| `orchestrator.todo_reminder_every_n_steps` | `5` | Iniezione system reminder TODO ogni N tool use. |
| `orchestrator.todo_reminder_min_todos` | `3` | Sotto questa soglia di todos pending nessun reminder iniettato (anti-spam chat brevi). |
| `orchestrator.understanding_enabled` | `false` | Se true, prima del planner un nodo understanding fa grounding semantico (+ fan-out explore opzionale) per task complessi. |
| `orchestrator.understanding_fanout_enabled` | `false` | Se true (e subagents abilitati), l'understanding spawna sub-agent explore in parallelo via dispatch_subagent. |
| `orchestrator.understanding_max_explore` | `3` | Massimo numero di sub-agent explore spawnati in parallelo dall'understanding. |
| `orchestrator.understanding_min_token_budget` | `3000` | Gate hard: sotto questo budget il nodo understanding non si attiva (task piccoli). |
| `orchestrator.understanding_synthesize_enabled` | `false` | Se true, il context_brief viene sintetizzato da un LLM economico; altrimenti concatenazione strutturata dei risultati RAG. |
| `orchestrator.understanding_topk` | `8` | Numero di hit della ricerca semantica per il grounding. |
| `orchestrator.verifier_enabled` | `true` | Feature flag globale per il verifier_node (PR-2). Indipendente dal planner. |
| `orchestrator.verifier_timeout_s` | `30.0` | Timeout singolo criterion check (PR-2). |
| `orchestrator.worker_mode_enabled` | `false` | Se true, nel run principale (subagent_depth=0) dopo il planner l'executor usa il prompt agent.orchestrator.base e tool ridotti: delega ai worker invece di implementare inline. |
| `orchestrator.worker_mode_tool_whitelist` | `list_files,read_file,search_in_files,recall_context,searc...` | Tool consentiti all'orchestratore in worker-mode (CSV). Solo lettura/coordinamento + delega; niente write/exec (li fanno i worker). |

## `project`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `project:2758b7cd-f14c-4df1-843b-06c823b7dc56:playwright_enabled` | `true` | Playwright abilitato e configurato |
| `project:8e697e82-1524-4c53-9634-a3ea11ac69e9:playwright_enabled` | `true` | Playwright abilitato e configurato |

## `projects`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `extra_project_roots` | `` | Lista separata da virgola di percorsi extra ammessi per il browse progetti (es. /mnt/data,/opt/repos). Vuoto = solo la root del progetto attivo. Override di emergenza: NEXUS_EXTRA_ROOTS. Richiede riavvio di mcp-core. |

## `prompt_templates`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `prompt_templates.base_max` | `3` | BASE_MAX per template variants (H-80 a) |
| `prompt_templates.hard_max` | `8` | HARD_MAX per template variants (H-80 b) |

## `providers`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `anthropic_api_key` | `sk-ant-api03-gs2rtgD48VZ3ExJfKM3A0qZVXIG3vyCDcUJpPJjieA5T...` | Anthropic API Key |
| `anthropic_enabled` | `true` | Abilita il provider Anthropic (Claude) |
| `deepseek_api_key` | `sk-3e3d9e6fc06a4665a56668b8e5b90a90` | DeepSeek API Key |
| `deepseek_enabled` | `true` | Abilita il provider DeepSeek |
| `google_api_key` | `AIzaSyApHUGYSOLNonSo7oEtanmlFfGJjq_PJoc` | Google AI API Key |
| `google_batch_api_enabled` | `true` | Abilita Google Gemini Batch API per analisi approfondita (50% costo) |
| `google_batch_model` | `gemini-2.5-flash` | Modello Gemini per batch job |
| `google_batch_threshold` | `3` | Numero minimo di file per usare Batch API (altrimenti chiamate sincrone) |
| `google_enabled` | `true` | Abilita il provider Google (Gemini) |
| `google_provider_backend` | `vertex` | Backend Google provider: 'gemini' (Gemini API direct, API key) oppure 'vertex' (Vertex AI, Service Account dal DB). Default 'gemini'. Tutte le credenziali Vertex devono essere nel DB — niente env var. |
| `google_vertex_credentials_json` | `{   "type": "service_account",   "project_id": "nexus-492...` | Service Account JSON per Vertex AI (sensitive, contenuto del file di chiavi GCP). OBBLIGATORIO se backend=vertex: il brain NON eredita credenziali da env GOOGLE_APPLICATION_CREDENTIALS o ADC. Incolla qui l'intero contenuto del file JSON SA. |
| `google_vertex_location` | `europe-west4` | Region GCP per Vertex AI (es. europe-west4, europe-west8, us-central1). OBBLIGATORIO se backend=vertex. Consigliato europe-west4/europe-west8 per compliance EU/GDPR. Default: europe-west4. |
| `google_vertex_project` | `nexus-492307` | ID progetto GCP per Vertex AI (es. nexus-prod-123456). OBBLIGATORIO se backend=vertex. |
| `mistral_api_key` | `NXgtEYMVv5JGCeFHZlviOi8w40cdw1k7` | Mistral API Key |
| `mistral_enabled` | `true` | Abilita il provider Mistral |
| `openai_api_key` | `sk-proj-vrrZB-k1qfd9oZ8iBevLYPE7maRu4RREcP6MjCYVTHML_ZeJ_...` | OpenAI API Key |
| `openai_enabled` | `true` | Abilita il provider OpenAI (GPT) |
| `provider.billing_recovery_interval_s` | `60` | Cadenza (secondi) del billing_cooldown_recovery_loop che riabilita i provider a cooldown scaduto previo probe. |
| `provider.circuit_breaker_extended_cooldown_s` | `600` | Cooldown esteso (secondi) applicato quando il circuit breaker provider scatta. |
| `provider.circuit_breaker_threshold` | `3` | Numero di fallimenti entro la finestra che apre il circuit breaker provider. |
| `provider.circuit_breaker_window_s` | `60` | Finestra (secondi) del circuit breaker provider: N fallimenti entro questa finestra aprono il breaker. |
| `provider.cooldown_default_s` | `300` | Durata cooldown provider di default (secondi) quando il Retry-After non e fornito. |
| `provider.cooldown_long_s` | `21600` | Durata cooldown lungo (secondi) per errori billing/quota non risolvibili a breve. Default 6h. |
| `provider.cooldown_max_s` | `3600` | Cap superiore (secondi) del cooldown provider. |
| `provider.cooldown_min_s` | `10` | Cap inferiore (secondi) del cooldown provider per evitare hammering. |
| `provider.health_probe_timeout_s` | `30` | Timeout (secondi) per la singola chiamata del provider_health_probe. Oltre la soglia il provider e considerato slow. |
| `provider.outage_threshold` | `3` | Numero di provider falliti nello stesso round oltre cui si assume outage locale (rollback dei cooldown). |
| `provider.recovery_probe_timeout_s` | `30` | Timeout (secondi) del probe attivo eseguito prima di riabilitare un provider (probe-then-reenable). |
| `providers.api_key_cache_ttl_seconds` | `60` | TTL cache api_key_loader (H-07) |
| `providers.billing_cooldown_seconds` | `21600` | Durata cooldown billing-error per provider (H-11) |
| `providers.capability_cache_ttl_seconds` | `60` | TTL (secondi) della cache delle capability provider in brain/providers/capability_loader.py. |
| `providers.catalog_cache_ttl_seconds` | `60` | TTL cache ai_price_catalog (H-08) |
| `providers.cooldown_bridge_timeout_seconds` | `5` | Timeout HTTP cooldown bridge (H-09) |
| `providers.cooldown_circuit_breaker_threshold` | `3` | Soglia consecutive failure → circuit breaker (H-78) |
| `providers.dns_timeout_seconds` | `5` | Timeout DNS resolver in dns_transport (H-10) |
| `providers.health_probe_max_tokens` | `10` | Max tokens per health probe Anthropic (H-05) |
| `providers.health_probe_outage_threshold` | `3` | Soglia consecutive failure → outage (H-77) |
| `provider.slow_cooldown_s` | `60` | Cooldown breve (secondi) applicato a un provider slow/transient dal provider_health_probe. |
| `providers.ollama.list_timeout_seconds` | `3` | Timeout Ollama list_models (H-22) |
| `providers.quota_cooldown_seconds` | `3600` | Durata (s) del cooldown locale del brain per quota/credito esaurito persistente (insufficient_quota). Piu lungo del transitorio. DB-driven, cache 60s. |
| `providers.test_connection_timeout_seconds` | `15` | Timeout test_connection in sync wrap (H-12) |
| `providers.thinking_models_ttl_seconds` | `60` | TTL cache modulo Anthropic per detection modelli con thinking abilitato (H-01) |

## `quality`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `quality_auto_scan` | `true` | Auto-scan files on save |
| `quality_severity_threshold` | `high` | Minimum severity to report (low/medium/high) |

## `reflection`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `reflection_enabled` | `true` | Abilita il nodo di self-reflection post-esecuzione agente. Disabilita con "false" per rollback immediato senza rideploy. |
| `reflection_model` | `claude-3-haiku-20240307` | Modello LLM usato per la valutazione self-reflection. Preferire modelli veloci ed economici (es. claude-3-5-haiku-20241022). |
| `reflection_reasoning_bank_min_score` | `0.85` | Punteggio minimo (0.0-1.0) perche' un esempio di successo venga salvato nel reasoning bank per arricchire i few-shot futuri. |
| `reflection_reward_weight` | `0.3` | Peso del punteggio reflection nel reward Q-learning finale (0.0-1.0). 0.3 significa: final_reward = 0.7 * euristica + 0.3 * reflection_score. |
| `reflection_sample_rate` | `0.3` | Probabilita' campionamento reflection per ogni run agente (0.0-1.0). 0.3 = 30% dei run, 1.0 = tutti i run. Aumentare in ambienti di eval. |
| `reflection_timeout_s` | `10` | Timeout massimo in secondi per la chiamata LLM di valutazione. Se il modello non risponde entro questo limite, la reflection viene saltata. |

## `regression_gate`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `regression_gate.enabled` | `true` | Abilita il regression gate SOFT a fine run (M13.4): esegue i test dell impact set e avvisa senza bloccare. |
| `regression_gate.hard_block` | `false` | Abilita il blocco HARD del regression gate (M13.5): se i test dell impact set falliscono il run e bloccato e l auto-commit non committa. Default-OFF (rollout). |
| `regression_gate.max_cycles` | `1` | Numero massimo di cicli fix-and-retest che il gate hard concede prima di bloccare definitivamente il run. |
| `regression_gate.max_tests` | `10` | Numero massimo di test dell impact set eseguiti dal gate per run (cap anti-latenza). |
| `regression_gate.soft_only` | `true` | Forza modalita SOFT (solo warning, nota e todo). Il blocco hard e M13.5, non ancora implementato. |
| `regression_gate.test_timeout_s` | `120` | Timeout in secondi per singolo test eseguito dal regression gate. |

## `router`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `router.classifier.ambiguity_min_confidence` | `0.70` | Soglia confidence minima classifier (H-59 a) |
| `router.classifier.ambiguity_min_margin` | `0.15` | Margine minimo ambiguity classifier (H-59 b) |
| `router.classifier_cfg_ttl_seconds` | `60` | TTL cache config classifier (H-58) |
| `router.classifier_llm_timeout_seconds` | `5` | Timeout LLM call classifier agentico (H-57) |
| `router.db_connect_timeout_seconds` | `2` | Timeout DB connect del router (H-61) |
| `router.service.cache_ttl_seconds` | `30` | TTL cache router service (H-60 b) |
| `router.service.default_timeout_seconds` | `1.5` | Timeout default router service (H-60 a) |

## `routing`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `agent.catalog_sync_health_window_hours` | `24` | Finestra (ore) entro cui un health check healthy=true rende un modello "recentemente sano" per il catalog_sync. Se sano e assente da upstream, catalog_sync lo lascia is_enabled=true (la verita e l account, non la lista upstream). |
| `agent.model_tool_failure_threshold` | `3` | Numero di turni agentici (con tool esposti) consecutivi chiusi con MALFORMED/output-vuoto dopo i quali un modello viene marcato supports_tool_use=false. Reset al primo successo con tool. |
| `agent.model_tool_probe.enabled` | `true` | Se true, model_health_probe esegue (oltre al ping chat) un tool-probe sul path agente per i soli modelli supports_tool_use=true: forza una tool call su un tool fittizio. A soglia (agent.model_tool_failure_threshold) marca supports_tool_use=false senza toccare is_enabled. Disattivabile per ridurre il costo delle chiamate API. |
| `agent.routing_matrix_cleanup_stale_enabled` | `true` | Se true, l'auto-promoter disattiva (is_active=false) le righe della routing matrix non-manuali il cui (provider, model_id) non ha piu un modello sano nel catalog (is_enabled=true AND consecutive_failures=0). |
| `billing_base_currency` | `EUR` | Base currency used for AI accounting and quotas |
| `default_model` | `claude-sonnet-4-6` | Default model for chat |
| `default_provider` | `anthropic` | Default LLM provider |
| `max_token_budget` | `32000` | Maximum token budget allowed |
| `model_catalog_last_sync` | `2026-06-03T20:48:52.339139749+00:00` | Timestamp ultimo sync catalogo da LiteLLM |
| `nexus_active_routing_pct` | `50` | Percentuale di richieste chat gestite dal router Q-Learning Nexus (0=off, 100=tutto). A/B testing: imposta 10-50 per un rollout graduale. |
| `nexus_behavior_mode` | `dinamico` | Modalità comportamento Nexus: veloce|economica|bilanciata|approfondita |
| `provider_hierarchy` | `anthropic,openai,google,deepseek,mistral` | Ordered fallback chain for chat providers |
| `provider_model_anthropic` | `claude-sonnet-4-6` | Preferred Anthropic model for chat routing |
| `provider_model_deepseek` | `deepseek-chat` | Modello default DeepSeek |
| `provider_model_google` | `gemini-2.5-flash` | Preferred Google model for chat routing |
| `provider_model_mistral` | `mistral-small-latest` | Modello default Mistral |
| `provider_model_openai` | `gpt-4o-mini` | Preferred OpenAI model for chat routing |
| `routing.ambiguity_min_confidence` | `0.70` | Top intent confidence sotto questa soglia → richiesta disambiguazione all'utente (NLU best practice). Range [0.0, 1.0]. |
| `routing.ambiguity_min_margin` | `0.15` | Margine (top_confidence − second_candidate_confidence) sotto questa soglia → disambiguazione. Range [0.0, 0.5]. |
| `routing_architecture_providers` | `anthropic,openai,google,deepseek,mistral` | Provider order for architecture requests |
| `routing_chat_providers` | `anthropic,openai,google,deepseek,mistral` | Provider order for general chat requests |
| `routing.classifier_cache_max_entries` | `10000` | Numero massimo entry nella cache LRU del classifier LLM. |
| `routing.classifier_cache_ttl_seconds` | `86400` | TTL della cache in-memory del classifier LLM (default 24h). Riduce le chiamate ripetute LLM su prompt identici. |
| `routing.classifier_model` | `gemini-2.5-flash` | Modello specifico per il classifier intent agentic. Cambiare con UPDATE. |
| `routing.classifier_provider` | `google` | Provider per il classifier intent agentic (deve esistere in nexus_provider_default_model). |
| `routing.degradation.cooldown_seconds` | `3600` | Durata cooldown provider-intent (M7) |
| `routing.degradation.min_visits` | `5` | Min visite prima di applicare degradation (M7) |
| `routing.degradation.threshold` | `0.7` | Failure rate soglia per cooldown provider-intent (M7) |
| `routing_docs_providers` | `anthropic,openai,google,deepseek,mistral` | Provider order for documentation requests |
| `routing_fix_providers` | `anthropic,openai,google,deepseek,mistral` | Provider order for fix requests |
| `routing.intent_deterministic_high` | `0.85` | Confidence del classificatore deterministico keyword sopra la quale si SALTA l'LLM (pre-check robusto per task agentici evidenti). Range [0.0, 1.0]. |
| `routing.intent_deterministic_min` | `0.60` | Confidence minima del classificatore deterministico sotto la quale NON lo si usa nemmeno come fallback quando l'LLM degrada a chat. Range [0.0, 1.0]. |
| `routing.intent_health_cooldown_secs` | `600` | Durata (secondi) del cooldown M7 di un provider su un intent dopo aver superato la soglia di fallimenti. |
| `routing.intent_health_enabled` | `false` | M7 Q-value: registra esiti per (provider,model,intent) e salta i provider in cooldown nel fallback. OFF di default (attivare dopo aver raccolto dati). |
| `routing.intent_health_failure_threshold_pct` | `60` | Percentuale di fallimenti su un intent oltre cui un provider entra in cooldown M7. |
| `routing.intent_health_min_attempts` | `8` | Numero minimo di tentativi (success+failure) su un intent prima di poter mettere un provider in cooldown M7. |
| `routing.llm_classifier_min_confidence` | `0.60` | Soglia confidence sotto cui il risultato del classifier LLM viene scartato e si usa il fallback keyword. Valore [0.0, 1.0]. |
| `routing.llm_classifier_timeout_seconds` | `8.0` | Timeout in secondi per la chiamata HTTP al classifier LLM (POST /classify-intent-agentic). Su timeout, fallback keyword. |
| `routing_refactor_providers` | `anthropic,openai,google,deepseek,mistral` | Provider order for refactor requests |
| `routing.sse_heartbeat_max_silence_secs` | `120` | Secondi di silenzio sullo stream SSE brain→mcp-core prima di considerare il run bloccato. Il brain emette ping ogni 30s, quindi valore tipico 90-180. |
| `routing_test_providers` | `anthropic,openai,google,deepseek,mistral` | Provider order for test requests |
| `routing.token_threshold_chat_breve` | `400` | Soglia in token sotto cui chat e considerata breve (chat_breve key in routing matrix). |
| `routing.token_threshold_chat_media` | `1500` | Soglia in token sopra cui chat passa da media a lunga. |
| `routing.token_threshold_complex_fix` | `3000` | Soglia in token sopra cui fix/refactor passa da fix_semplice a fix_complesso (route_model_with_mode). |
| `routing.token_threshold_long_context` | `6000` | Soglia in token sopra cui anche intent generici (chat) richiedono modello tier=medium nel catalog dynamic routing. |
| `token_budget` | `4096` | Default token budget per request |

## `runtime`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `shutdown.force_exit_timeout_seconds` | `10` | Secondi massimi che mcp-core attende dopo aver ricevuto SIGTERM/Ctrl-C prima di forzare std::process::exit(0) via watchdog su thread OS dedicato. Garantisce che il processo (e il bind su :4000) venga sempre rilasciato anche se un worker detached non risponde a cancellation. Default 10. La unit systemd ha TimeoutStopSec come ulteriore rete (SIGKILL). |

## `schema`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `schema.descr_max` | `200` | Max char per description di property in JSON Schema (H-17) |
| `schema.enum_max` | `10` | Max numero enum values prima del troncamento (H-18) |
| `schema.tool_descr_max` | `400` | Max char per tool description (H-19) |

## `security`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `dlp_allow_cloud_tier2` | `true` | Se true, consente di inviare Tier 2 (sensibili) verso provider cloud. |
| `dlp_allow_cloud_tier3` | `true` | Se true, consente di inviare Tier 3 (critici) verso provider cloud (sconsigliato). |
| `dlp_enabled` | `true` | Abilita/disabilita il Data Loss Prevention (classificazione sensibilità Tier). |

## `system`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `brain_log_level` | `debug` | Livello di log del brain Python: debug, info, warning, error. In sviluppo locale si usa debug; in produzione info. Override di emergenza: LOG_LEVEL. Richiede riavvio del brain. |

## `vector`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `mcp_tool_search_min_score` | `0.35` | Score minimo coseno (0-1) per restituire un risultato dalla ricerca semantica tool MCP. Sotto soglia si usa ILIKE come fallback. |
| `qdrant_mcp_tools_collection` | `mcp_tools` | Nome della collection Qdrant per gli embedding dei tool MCP (nexus_mcp_tool_search semantico). |

---

**Totale chiavi**: 477

---
id: 59047b22-236b-4a0b-b6ad-985a133de3d6
kind: api
title: Settings keys
slug: settings-keys
tags:
  - api
  - settings
source_commit: cdd1589822b0955e72efeec44499813a32ad2602
source_files:
  - db/migrations/
auto_generated: true
created_at: 2026-05-23T07:20:00Z
updated_at: 2026-05-30T06:47:35Z
nexus_meta_version: 1
---

Tutte le chiavi di configurazione di Nexus (tabella `settings`). Generato dal DB.

Vedi anche: [[postgres-tables]], [[routing-matrix]], [[meta-vault-architettura]].


## `agent`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `agent.attachment.archive_entry_max_bytes` | `204800` | Max byte letti da una singola entry di archivio (nexus_read_archive_entry). Default 200KB. |
| `agent.attachment.archive_max_entries` | `1000` | Max entries elencate da nexus_list_archive_entries prima della troncatura. Default 1000. |
| `agent.attachment.figma_make_ai_chat_max_load_bytes` | `26214400` | Max byte caricati in RAM dal file ai_chat.json di un archivio Figma Make prima del parsing. Default 5 MB. Se il file e' piu' grande viene troncato (segnalato con ai_chat_truncated_at_load=true nella risposta del tool). |
| `agent.attachment.figma_make_assistant_message_max_chars` | `2000` | Max caratteri di un singolo messaggio assistant prima della truncatura. I messaggi user (prompt originale) non vengono mai troncati singolarmente. Default 2000. |
| `agent.attachment.figma_make_chat_messages_max_chars` | `51200` | Max caratteri cumulativi del testo estratto dai messaggi user+assistant del thread chat Figma Make. Default 50 KB. Oltre la soglia i messaggi residui vengono scartati (chat_messages_truncated=true). |
| `agent.attachment.figma_make_chat_messages_max_count` | `20` | Max numero di messaggi (user + assistant) restituiti dal thread chat Figma Make. Default 20. |
| `agent.attachment.figma_max_bytes` | `51200` | Max byte del payload canvas.fig estratti da nexus_extract_figma_structure. Default 50KB. |
| `agent.attachment.image_max_bytes` | `2097152` | Massima dimensione (byte) di un immagine processabile dal tool nexus_describe_image_attachment. Default 2 MB. Oltre il limite il tool ritorna errore esplicito al modello. |
| `agent.attachment.pdf_max_text_bytes` | `102400` | Max byte di testo estratto da nexus_extract_pdf_text in totale (su tutte le pagine richieste). Default 100KB. |
| `agent.attachment.preextract_enabled` | `true` | Pre-extraction automatica del contenuto strutturato di PDF/DOCX/Figma allegati. Default true. Disattivare se causa latenza eccessiva all'invio del primo messaggio. |
| `agent.attachment.preextract_max_chars` | `50000` | Limite totale (in caratteri) del contenuto pre-extracted sommando tutti gli allegati del turno. Se eccede, gli ultimi allegati non vengono pre-extracted. |
| `agent.attachment.read_cache_ttl_seconds` | `300` | TTL (secondi) della cache LRU read_cache che deduplica chiamate identiche a nexus_read_attachment / nexus_read_archive_entry. Default 5 minuti. |
| `agent.attachment.session_read_budget_bytes` | `500000` | Cap cumulativo (byte) delle letture nexus_read_attachment + nexus_read_archive_entry per sessione. Oltre la soglia, il brain risponde con tool_result sintetico che invita a usare gli estrattori strutturati. |
| `agent.attachment.xlsx_max_rows` | `1000` | Max righe restituite da nexus_extract_xlsx_data. Default 1000. |
| `agent.complexity.file_path_points` | `2` | Punti per ogni path o file menzionato nel prompt (es. /home/, src/, *.json). |
| `agent.complexity.keyword_weights` | `{"create":3,"write_file":2,"install":2,"build":2,"systemc...` | Pesi keyword per stima complessita' task agente (budget iterazioni adattivo). Chiave: substring da cercare nel prompt, valore: punti complessita'. Aggiornamento: aggiunti verbi italiani (implementa, sviluppa, costruisci, genera, scaffold) e sostantivi (progetto, applicazione). |
| `agent.complexity.step_marker_points` | `5` | Punti per ogni marker di step esplicito (1., 2., step, task, phase) nel prompt. |
| `agent.complexity.weak_model_multiplier` | `1.5` | Moltiplicatore budget se il modello iniziale e gpt-4o-mini / haiku / nano (necessita piu iter per G1 nudge). |
| `agent.context.compress_phase_boundaries` | `5,10,20,50` | CSV crescente dei boundary di fase per compressione escalante. iter < primo = no compressione. Tra phase[i] e phase[i+1] = applica keep_recent[i] e max_chars[i]. Le tre liste boundaries/keep_recent/max_chars devono avere stessa lunghezza. |
| `agent.context.compress_phase_keep_recent` | `8,5,3,2` | CSV keep_recent per ogni fase di compressione (allineato a compress_phase_boundaries). |
| `agent.context.compress_phase_max_chars` | `2000,1000,500,150` | CSV max_content_chars per ogni fase di compressione (allineato a compress_phase_boundaries). |
| `agent.context.compress_start_iter` | `5` | Iterazione di executor a partire dalla quale attivare la compressione escalante dei tool_result. Prima viene applicata solo la dedup. Default 5 (FIX A). |
| `agent.context.dedup_tool_results_enabled` | `true` | Se true (default) ogni iter executor applica _dedup_tool_results_history: tool_result vecchi con stessa signature (sha256(tool_name+args_json)) vengono sostituiti con placeholder, tenendo solo l ultimo. FIX B. |
| `agent.context.drop_unused_base64_age` | `3` | Soglia (n messaggi successivi) entro la quale verificare se un blob base64 di un tool_result vecchio viene citato testualmente. Se non viene citato, il body base64 viene sostituito con un placeholder. FIX C. |
| `agent.context.predictive_cap_ratio` | `0.5` | Soglia (0.3-0.9) sul context_window del modello: se context_attuale + stima_tool_result supera ratio*context_window, la chiamata al tool viene intercettata e sostituita da tool_result sintetico di errore. FIX D. |
| `agent.enforce_port_allocation` | `true` | Se true, write_file/edit_file rifiutano sorgenti con porte TCP hardcoded fuori dal bucket Nexus 20000-39999 (vedi ADR 0010). |
| `agent.iteration_budget.base` | `60` | Numero base iterazioni LangGraph per ogni run agente. Sommato a per_complexity_point*complexity_score. |
| `agent.iteration_budget.max` | `300` | Tetto massimo iterazioni anche per task molto complessi. Safety net runaway. |
| `agent.iteration_budget.per_complexity_point` | `4` | Iterazioni aggiuntive per ogni punto di complessita del prompt (score 0-100). |
| `agent_narration_warn_after_chars` | `1500` | Caratteri di testo streamed senza tool call dopo i quali il badge UI passa in stato warning. |
| `agent_narration_warn_after_ms` | `30000` | Millisecondi di run senza tool call dopo i quali il badge UI passa in stato warning (possibile loop di narrazione). |
| `agent_parallel_enabled` | `true` | Abilita l'esecuzione parallela di piu' agenti contemporaneamente per accelerare task complessi |
| `agent_parallel_max` | `5` | Numero massimo di agenti paralleli per sessione (1-5) |
| `agent_router_addr` | `127.0.0.1:50501` | Indirizzo host:porta del server gRPC AgentRouter esposto da mcp-core e usato dal brain Python per consultare il Q-Learning router. Override di emergenza: AGENT_ROUTER_ADDR. Richiede riavvio di mcp-core e del brain. |
| `agent_router_enabled` | `true` | Abilita il server gRPC AgentRouter (porta 50072) che espone il router Q-Learning di nexus-orchestrator al brain Python. Quando attivo, il router_node consulta il Q-Learning per scegliere il profilo agente ottimale (es. coder, cloud_architect, tech_writer) in base alla cronologia dei reward osservati. Se disabilitato il brain usa il routing di fallback basato solo sull'intent. Richiede riavvio di mcp-core per applicare la modifica. |
| `anthropic_system_cache_ttl` | `1h` | TTL della cache prompt di sistema per Anthropic: 5m (default Anthropic) o 1h (extended-cache-ttl-2025-04-11). Il valore 1h massimizza il cache hit rate fra turni distanti (il system prompt cambia raramente). Override: NEXUS_ANTHROPIC_SYSTEM_CACHE_TTL. Richiede riavvio del brain. |
| `catalog_sync.disable_missing` | `true` | Se TRUE, disabilita i modelli del catalog non piu esposti dall API. Se FALSE solo log. |
| `catalog_sync.enabled` | `true` | Attiva/disattiva il worker periodico di sync catalog modelli dai provider. |
| `catalog_sync.insert_new_disabled` | `true` | Se TRUE, modelli nuovi vengono inseriti con is_enabled=false (admin verifica prezzi prima di abilitare). |
| `catalog_sync.interval_hours` | `6` | Intervallo (ore) tra i tick del worker. Default 6 = 4 sync al giorno. |
| `catalog_sync.providers` | `anthropic,openai,mistral,deepseek,google` | Provider per cui eseguire l'auto-discovery dei modelli (CSV). Google passa per brain REST /providers/google/models/live (Vertex SDK). |
| `extended_thinking_budget_tokens` | `8000` | Budget massimo di token interni di ragionamento per turno quando extended_thinking_enabled=true. Range consigliato: 2000-16000. Valori piu' alti migliorano la qualita' ma aumentano i costi. |
| `extended_thinking_enabled` | `false` | Abilita il ragionamento interno esteso (Extended Thinking) di Anthropic sui modelli Sonnet/Opus. Genera token di ragionamento interni billati al prezzo output. Disabilitato di default per contenere i costi. Attivare solo per task che richiedono ragionamento profondo. |
| `llm_classifier_enabled` | `true` | Abilita il classificatore LLM degli intent (chiamata REST /classify-intent-agentic al brain Python). Se false usa solo keyword matching locale: piu' veloce ma meno preciso. Override di emergenza: NEXUS_LLM_CLASSIFIER_ENABLED=false. Richiede riavvio di mcp-core. |
| `terminal_default_shell` | `bash` | Shell di default per i terminali agente: bash, zsh, fish. Su Windows: powershell.exe. Override di emergenza: TERMINAL_SHELL. Richiede riavvio del brain e di mcp-core. |
| `tool_runner_addr` | `127.0.0.1:50071` | Indirizzo host:porta del server gRPC ToolRunner esposto da mcp-core e usato dal brain Python per eseguire i tool MCP (read_file, write_file, run_command, ecc.). Entrambi i servizi leggono questo valore. Override di emergenza: TOOL_RUNNER_ADDR. Richiede riavvio di mcp-core e del brain. |
| `tool_runner_enabled` | `true` | Abilita il server gRPC ToolRunner (porta 50071) usato dal brain LangGraph per eseguire i tool Nexus builtin. Override di emergenza: ENABLE_TOOL_RUNNER=1. Richiede riavvio di mcp-core per applicare la modifica. |

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

## `infrastructure`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `brain_rest_url` | `http://127.0.0.1:8001` | URL del server REST del brain Python (FastAPI su porta 8001). Usato da mcp-core per chiamare /agent/run/stream, /classify-intent-agentic, /catalog/sync e altri endpoint REST del brain. Override di emergenza: BRAIN_REST_URL o NEURAL_CORE_REST_URL. Richiede riavvio di mcp-core. |
| `mcp_core_url` | `http://127.0.0.1:4000` | URL del server HTTP mcp-core (porta 4000). Usato dal brain Python per leggere settings via _get_core_setting(), dal router semantico, dal cooldown bridge e dall'agent router client. Override di emergenza: MCP_CORE_URL. Richiede riavvio del brain. |
| `network_dns_servers` | `1.1.1.1,8.8.8.8 ` | Server DNS personalizzati separati da virgola (es. 8.8.8.8,1.1.1.1). Usato dal Neural Core per risolvere i nomi host verso API AI esterne. |
| `neural_core_url` | `http://localhost:50051` | Neural Core gRPC URL |
| `nexus_external_proxy` | `` | Proxy HTTP/HTTPS per le chiamate verso API esterne (es. http://localhost:8002). Usato da tutti i backend Nexus tramite NEXUS_PROXY. Lascia vuoto per connessione diretta. |
| `projects_base_root` | `/home/administrator/projects` | Root assoluta sotto cui e' consentita la registrazione/navigazione dei progetti |
| `qdrant_collection` | `code_embeddings` | Qdrant collection name |
| `qdrant_project_context_collection` | `project_context` | Qdrant collection per indicizzazione iniziale del contesto/storia progetto |
| `qdrant_url` | `http://localhost:6333` | Qdrant vector DB URL |
| `redis_url` | `redis://localhost:6379` | Redis connection URL |

## `knowledge`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `knowledge.context_injection_enabled` | `true` | Abilita iniezione automatica delle note KB rilevanti nel system prompt agente |
| `knowledge.context_injection_min_score` | `0.5` | Soglia minima di similarita cosine 0-1 |
| `knowledge.context_injection_top_k` | `5` | Numero massimo di note KB da iniettare (1-20) |

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
| `orchestrator.clarify.confidence_threshold` | `0.6` | Soglia di confidence sotto cui il nodo si attiva. Sopra -> bypass. |
| `orchestrator.clarify.enabled` | `true` | Feature flag globale per il clarify_or_expand_node. Off -> nodo no-op. |
| `orchestrator.clarify.max_question_chars` | `280` | Cap di lunghezza della domanda di chiarimento prima del troncamento. |
| `orchestrator.clarify.prompt_key` | `agent.clarify.base` | Indirezione per varianti A/B del prompt clarify. |
| `orchestrator.clarify.require_llm_classifier` | `false` | Se true, attiva il clarify solo quando NEXUS_LLM_CLASSIFIER_ENABLED=true; altrimenti usa anche il fallback keyword/embedding. |
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
| `orchestrator.plan_intents` | `code,implement,fix,refactor,scaffold_app,architecture,doc...` | CSV degli intent eleggibili per il planner. |
| `orchestrator.plan_min_token_budget` | `50` | Sotto questa soglia di token_budget il planner viene saltato (chat brevi). |
| `orchestrator.planner_prompt_key` | `agent.planner.base` | Indirezione per varianti A/B del prompt del planner. |
| `orchestrator.plan_phase_enabled` | `true` | Feature flag globale per il planner_node (PR-1). Off -> grafo si comporta come oggi. |
| `orchestrator.subagent_cost_cap_per_run_usd` | `5.00` | Hard cap di spesa cumulativa sub-agents per singolo parent run. |
| `orchestrator.subagent_default_timeout_s` | `300` | Timeout default per kind se non specificato in nexus_subagent_definitions. |
| `orchestrator.subagent_kinds_whitelist` | `plan,explore,implement,verify,review` | CSV dei kind ammessi per dispatch_subagent (filtra anche custom kinds). |
| `orchestrator.subagent_max_depth` | `2` | Profondita max di annidamento sub-agent (sub-of-sub). |
| `orchestrator.subagents_enabled` | `true` | Feature flag globale sub-agents pattern. Off -> dispatch_subagent ritorna errore al main. |
| `orchestrator.todo_reminder_every_n_steps` | `5` | Iniezione system reminder TODO ogni N tool use. |
| `orchestrator.todo_reminder_min_todos` | `3` | Sotto questa soglia di todos pending nessun reminder iniettato (anti-spam chat brevi). |
| `orchestrator.verifier_enabled` | `true` | Feature flag globale per il verifier_node (PR-2). Indipendente dal planner. |
| `orchestrator.verifier_timeout_s` | `30.0` | Timeout singolo criterion check (PR-2). |

## `project`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `project:8e697e82-1524-4c53-9634-a3ea11ac69e9:playwright_enabled` | `true` | Playwright abilitato e configurato |

## `projects`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `extra_project_roots` | `` | Lista separata da virgola di percorsi extra ammessi per il browse progetti (es. /mnt/data,/opt/repos). Vuoto = solo la root del progetto attivo. Override di emergenza: NEXUS_EXTRA_ROOTS. Richiede riavvio di mcp-core. |

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

## `routing`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `billing_base_currency` | `EUR` | Base currency used for AI accounting and quotas |
| `default_model` | `claude-sonnet-4-6` | Default model for chat |
| `default_provider` | `anthropic` | Default LLM provider |
| `max_token_budget` | `32000` | Maximum token budget allowed |
| `model_catalog_last_sync` | `2026-05-30T06:37:30.859398113+00:00` | Timestamp ultimo sync catalogo da LiteLLM |
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
| `routing_docs_providers` | `anthropic,openai,google,deepseek,mistral` | Provider order for documentation requests |
| `routing_fix_providers` | `anthropic,openai,google,deepseek,mistral` | Provider order for fix requests |
| `routing.intent_deterministic_high` | `0.85` | Confidence del classificatore deterministico keyword sopra la quale si SALTA l'LLM (pre-check robusto per task agentici evidenti). Range [0.0, 1.0]. |
| `routing.intent_deterministic_min` | `0.60` | Confidence minima del classificatore deterministico sotto la quale NON lo si usa nemmeno come fallback quando l'LLM degrada a chat. Range [0.0, 1.0]. |
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

## `security`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `dlp_allow_cloud_tier2` | `true` | Se true, consente di inviare Tier 2 (sensibili) verso provider cloud. |
| `dlp_allow_cloud_tier3` | `true` | Se true, consente di inviare Tier 3 (critici) verso provider cloud (sconsigliato). |
| `dlp_enabled` | `true` | Abilita/disabilita il Data Loss Prevention (classificazione sensibilità Tier). |

## `system`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `brain_log_level` | `info` | Livello di log del brain Python: debug, info, warning, error. In sviluppo locale si usa debug; in produzione info. Override di emergenza: LOG_LEVEL. Richiede riavvio del brain. |

## `vector`

| Chiave | Valore default | Descrizione |
|---|---|---|
| `mcp_tool_search_min_score` | `0.35` | Score minimo coseno (0-1) per restituire un risultato dalla ricerca semantica tool MCP. Sotto soglia si usa ILIKE come fallback. |
| `qdrant_mcp_tools_collection` | `mcp_tools` | Nome della collection Qdrant per gli embedding dei tool MCP (nexus_mcp_tool_search semantico). |

---

**Totale chiavi**: 254

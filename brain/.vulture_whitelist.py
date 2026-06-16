# Whitelist vulture per i falsi positivi da framework del brain Python.
#
# Questo file NON e' codice eseguibile: viene passato a vulture insieme ai
# sorgenti (vedi scripts/dead-code-report.sh) e ogni nome qui riferito viene
# considerato "usato". Servono a coprire i casi in cui il chiamante e' un
# framework e quindi invisibile all'analisi statica:
#   - metodi dell'interfaccia BaseCheckpointSaver invocati dal runtime LangGraph
#   - metodi PascalCase del Servicer dispatchati dal runtime grpcio
#   - handler FastAPI/Starlette registrati dai decoratori @app/@router
#   - campi di modelli Pydantic valorizzati dalla deserializzazione HTTP
#   - campi del TypedDict AgentState (schema LangGraph, accesso via chiavi stringa)
#   - assegnazioni di attributi lette da librerie terze (python-docx, dnspython,
#     google-genai)
#   - pattern pytest nei test (fixture per nome, pytestmark, mock context-manager)
#
# Aggiornare questo file SOLO per falsi positivi da framework: il dead code
# reale va eliminato, non whitelistato.

# ── brain/agents/postgres_checkpointer.py — interfaccia BaseCheckpointSaver ──
# Metodi e property invocati dal runtime del grafo LangGraph, mai dal codice
# applicativo. I parametri di firma non usati nel body sono imposti
# dall'interfaccia e non si possono snellire.
_.aput  # metodo interfaccia LangGraph
_.aget_tuple  # metodo interfaccia LangGraph
_.aput_writes  # metodo interfaccia LangGraph
_.aget  # metodo interfaccia LangGraph
_.alist  # metodo interfaccia LangGraph
_.config_specs  # property interfaccia LangGraph
writes  # parametro firma imposto dall'interfaccia (aput_writes)
task_path  # parametro firma imposto dall'interfaccia (aput_writes)
before  # parametro firma imposto dall'interfaccia (alist)

# ── brain/grpc_server/neural_service.py — Servicer gRPC ─────────────────────
# Metodi PascalCase dispatchati dal runtime grpcio in base al nome del servizio
# nei proto, mai chiamati direttamente dal codice Python.
_.EmbedText  # metodo Servicer gRPC
_.EmbedBatch  # metodo Servicer gRPC
_.ClassifyIntent  # metodo Servicer gRPC
_.RouteModel  # metodo Servicer gRPC
_.ClassifyError  # metodo Servicer gRPC
_.GenerateCompletion  # metodo Servicer gRPC
_.GenerateStructuredCompletion  # metodo Servicer gRPC
_.ListProviderModels  # metodo Servicer gRPC
_.SyncProviderModels  # metodo Servicer gRPC
_.TestProviderConnection  # metodo Servicer gRPC
_.SyncKnowledgeBundle  # metodo Servicer gRPC
_.GenerateAgentTurn  # metodo Servicer gRPC
_.GetProviderHealth  # metodo Servicer gRPC
_.SubmitBatchReview  # metodo Servicer gRPC
_.GetBatchJobStatus  # metodo Servicer gRPC
_.GenerateDocument  # metodo Servicer gRPC

# ── Handler FastAPI/Starlette decorati ───────────────────────────────────────
# Funzioni registrate da @app.on_event / @router.get|post|websocket: il
# chiamante e' il framework, vulture non vede l'invocazione.
# brain/grpc_server/app.py
startup_event  # handler @app.on_event("startup")
shutdown_event  # handler @app.on_event("shutdown")
# brain/grpc_server/routes/agent.py
subagent_run_endpoint  # handler FastAPI
subagent_poll_endpoint  # handler FastAPI
subagent_resume_endpoint  # handler FastAPI
clarifications_get  # handler FastAPI
clarifications_answer  # handler FastAPI
project_analyze  # handler FastAPI
prompt_revise  # handler FastAPI
batch_analyze_submit  # handler FastAPI
batch_analyze_status  # handler FastAPI
batch_analyze_results  # handler FastAPI
agent_run  # handler FastAPI
agent_approve  # handler FastAPI
agent_state  # handler FastAPI
agent_feedback  # handler FastAPI
agent_stats  # handler FastAPI
agent_run_stream  # handler FastAPI
# brain/grpc_server/routes/core.py
health  # handler FastAPI
billing_cooldown  # handler FastAPI
classify_intent  # handler FastAPI
classify_intent_agentic  # handler FastAPI
classify_intent_agentic_stats  # handler FastAPI
embed  # handler FastAPI
provider_health  # handler FastAPI
complete  # handler FastAPI
reload_settings  # handler FastAPI
# brain/grpc_server/routes/terminal.py
terminal_ws  # handler @router.websocket
# brain/grpc_server/routes/vision.py
vision_describe  # handler FastAPI
vision_compare  # handler FastAPI

# ── Campi di modelli Pydantic delle richieste API ────────────────────────────
# Valorizzati dalla deserializzazione del body HTTP e letti dal validatore
# Pydantic: l'accesso avviene via framework.
_.profile_id  # campo BaseModel (es. IntentRequest, routes/core.py)

# ── brain/agents/state.py — TypedDict AgentState (schema LangGraph) ─────────
# I campi sono dichiarazioni di schema: letti/scritti ovunque via
# state.get('nome') / merge di dict con chiavi stringa, invisibili a vulture.
_.messages  # campo AgentState
_.meta_steps  # campo AgentState
_.user_intent  # campo AgentState
_.intent_confidence  # campo AgentState
_.task_complexity  # campo AgentState
_.agentic_score  # campo AgentState
_.is_ambiguous  # campo AgentState
_.expanded_query  # campo AgentState
_.pending_clarify  # campo AgentState
_.clarify_attempts  # campo AgentState
_.intent_hint  # campo AgentState
_.action_oriented  # campo AgentState
_.declared_outcome  # campo AgentState
_.tool_infra_error  # campo AgentState
_.playbook_steps  # campo AgentState
_.playbook_key  # campo AgentState
_.declared_done_count  # campo AgentState
_.blocked_cap_rejected  # campo AgentState
_.discovered_tools_run  # campo AgentState
_.compress_cutoff_index  # campo AgentState
_.compress_cutoff_phase  # campo AgentState
_.run_notes  # campo AgentState
_.task_type  # campo AgentState
_.behavior_mode  # campo AgentState
_.token_budget  # campo AgentState
_.result  # campo AgentState
_.provider_used  # campo AgentState
_.model_used  # campo AgentState
_.feedback_score  # campo AgentState
_.latency_ms  # campo AgentState
_.token_usage  # campo AgentState
_.iterations  # campo AgentState
_.thread_id  # campo AgentState
_.pending_tool_uses  # campo AgentState
_.stop_reason  # campo AgentState
_.recent_tool_signatures  # campo AgentState
_.tools_json  # campo AgentState
_.discovered_tools_next_turn  # campo AgentState
_.system_text  # campo AgentState
_.session_id  # campo AgentState
_.approved  # campo AgentState
_.provider_override  # campo AgentState
_.model_override  # campo AgentState
_.profile_name  # campo AgentState
_.prompt_tokens  # campo AgentState
_.completion_tokens  # campo AgentState
_.cache_creation_tokens  # campo AgentState
_.cache_read_tokens  # campo AgentState
_.total_tokens  # campo AgentState
_.total_cost_usd  # campo AgentState
_.cache_hit_rate  # campo AgentState
_.temperature  # campo AgentState
_.top_p  # campo AgentState
_.created_at  # campo AgentState
_.completed_at  # campo AgentState
_.reflection_score  # campo AgentState
_.reflection_dimensions  # campo AgentState
_.reflection_weaknesses  # campo AgentState
_.reflection_suggestions  # campo AgentState
_.final_reward  # campo AgentState
_.plan_phase_active  # campo AgentState
_.plan_phase_skip_reason  # campo AgentState
_.current_plan_id  # campo AgentState
_.current_todos  # campo AgentState
_.acceptance_criteria  # campo AgentState
_.active_todo_id  # campo AgentState
_.plan_rationale  # campo AgentState
_.plan_constraints  # campo AgentState
_.plan_alternatives  # campo AgentState
_.plan_rationale_context  # campo AgentState
_.context_brief  # campo AgentState
_.understanding_active  # campo AgentState
_.understanding_skip_reason  # campo AgentState
_.since_last_todo_reminder  # campo AgentState
_.verify_cycle  # campo AgentState
_.exploratory_verify_cycle  # campo AgentState
_.exploratory_verify_total  # campo AgentState
_.final_gate_cycle  # campo AgentState
_.verifier_last_result  # campo AgentState
_.plan_revisions  # campo AgentState
_.parent_run_id  # campo AgentState
_.subagent_depth  # campo AgentState
_.subagent_results  # campo AgentState
_.active_subagent_runs  # campo AgentState
_.subagent_cost_cumulative_usd  # campo AgentState
_.attachment_read_bytes  # campo AgentState
_.action_nudge_count  # campo AgentState
_.g1_reroute_count  # campo AgentState
_.consecutive_exploration_calls  # campo AgentState
_.exploration_nudge_sent  # campo AgentState
_.repeated_cmd_nudge_sent  # campo AgentState
_.progress_guided_axes  # campo AgentState
_.progress_diagnosed_axes  # campo AgentState
_.forced_close_unverified  # campo AgentState
_.sticky_provider  # campo AgentState
_.sticky_model  # campo AgentState
_.planner_sticky_provider  # campo AgentState
_.planner_sticky_model  # campo AgentState
_.automation_mode  # campo AgentState

# ── brain/providers/_models.py — dataclass ProviderCapability ───────────────
# Mirror runtime 1:1 della tabella nexus_provider_capabilities (mig 0240,
# punto unico ADR 0024): il loader popola i campi POSIZIONALMENTE
# (capability_loader.py) e gli adapter li adottano progressivamente.
_.max_context_tokens  # campo ProviderCapability (specchio DB)
_.supports_prompt_cache  # campo ProviderCapability (specchio DB)
_.prompt_cache_dialect  # campo ProviderCapability (specchio DB)
_.supports_parallel_tools  # campo ProviderCapability (specchio DB)
_.stop_reason_dialect  # campo ProviderCapability (specchio DB)
_.history_keep_recent_messages  # campo ProviderCapability (specchio DB)
_.history_max_old_tool_result_chars  # campo ProviderCapability (specchio DB)
_.request_timeout_seconds  # campo ProviderCapability (specchio DB)
_.connect_timeout_seconds  # campo ProviderCapability (specchio DB)
_.tool_result_max_chars  # campo ProviderCapability (specchio DB)
_.tool_result_max_bytes  # campo ProviderCapability (specchio DB)
_.tool_result_max_lines  # campo ProviderCapability (specchio DB)

# ── Assegnazioni di attributi su oggetti di libreria (side-effect di config) ─
# L'attributo viene LETTO dalla libreria terza (python-docx al render,
# dnspython nel resolve, google-genai nella request); vulture vede solo la
# scrittura. Vedi brain/documents/styles.py, brain/providers/dns_transport.py,
# brain/grpc_server/runtime.py, brain/providers/google_provider.py.
_.rgb  # attributo python-docx
_.space_after  # attributo python-docx
_.space_before  # attributo python-docx
_.line_spacing  # attributo python-docx
_.bold  # attributo python-docx
_.alignment  # attributo python-docx
_.is_linked_to_previous  # attributo python-docx
_.top_margin  # attributo python-docx
_.bottom_margin  # attributo python-docx
_.left_margin  # attributo python-docx
_.right_margin  # attributo python-docx
_.lifetime  # attributo dnspython (resolver)
_.system_instruction  # attributo SDK google-genai (config request)

# ── brain/agents/nodes/__init__.py — uso dinamico via locals() ──────────────
cascade_did_fallback  # letta via locals().get(...) per sticky_provider/model

# ── brain/tests/** — pattern framework pytest ────────────────────────────────
# Fixture iniettate per nome, pytestmark letto dal collector, attributi dunder
# dei context-manager mock, parametri fixture richiesti per il solo side-effect.
pytestmark  # marker letto dal collector pytest
_._CHAIN_CACHE  # attributo patchato sui mock nei test
_.__enter__  # context-manager mock
_.__exit__  # context-manager mock
_patch_cfg  # fixture pytest (side-effect)
_flag_on  # fixture pytest (side-effect)
_flag_off  # fixture pytest (side-effect)
_mock_httpx_response  # fixture pytest
monkeypatch_cfg  # parametro fixture richiesto per side-effect
vectors_config  # parametro fixture richiesto per side-effect
out3  # variabile di test

"""Schema TypedDict per lo stato condiviso del grafo LangGraph."""
from __future__ import annotations

from operator import add
from typing import Annotated, Sequence

from langchain_core.messages import BaseMessage
from typing_extensions import TypedDict


class AgentState(TypedDict, total=False):
    """Stato condiviso tra tutti i nodi del grafo Nexus Agent.

    `messages` usa l'annotazione `add` per l'aggregazione automatica LangGraph.
    """

    messages: Annotated[Sequence[BaseMessage], add]
    # Meta-step semantici pubblicati al frontend chat (plan/routing/clarify/
    # fallback/reflection). Ogni nodo che vuole emettere uno step semantico
    # aggiunge un dict {kind,title,payload,correlation_id?,created_at} a questa
    # lista; il generator SSE in grpc_server li converte in eventi
    # `{"type":"meta_step", ...}` al volo. L'annotazione `add` ne consente
    # l'accumulo cross-nodo senza conflitti di reducer.
    meta_steps: Annotated[list[dict], add]
    user_intent: str
    # Confidence della classificazione intent (0..1). Popolato da router_node.
    # Consumato da clarify_or_expand_node per decidere se attivarsi.
    intent_confidence: float
    # PR-D: segnali del classifier agentico per il gating adattivo del planner
    # forte (popolati da router_node solo se adaptive_classifier_enabled).
    # task_complexity: 'low'|'medium'|'high'; agentic_score 0..1; is_ambiguous bool.
    task_complexity: str | None
    agentic_score: float | None
    is_ambiguous: bool | None
    # Query arricchita prodotta dal clarify_or_expand_node (mode=expand).
    # USATA solo dal retrieve RAG, NON sostituisce il prompt utente al modello.
    expanded_query: str | None
    # True quando clarify_or_expand_node ha emesso una richiesta di chiarimento
    # e il turno deve fermarsi in attesa di risposta utente.
    pending_clarify: bool
    task_type: str
    behavior_mode: str
    token_budget: int
    result: str | None
    provider_used: str | None
    model_used: str | None
    feedback_score: float | None
    latency_ms: float | None
    token_usage: int | None
    iterations: int
    thread_id: str

    # ── Agent tool loop (Fase 2) ─────────────────────────────────────────────
    # Popolati da executor_node quando il modello risponde con tool_use.
    pending_tool_uses: list[dict]
    # "tool_use" → loop verso tool_dispatch; "end_turn"/"stop" → learner.
    # "loop_detected" → executor ha rilevato chiamate ripetute, forza chiusura.
    stop_reason: str | None
    # Firme delle ultime tool calls (tool_name + hash input) per loop detection.
    recent_tool_signatures: list[str]
    # Tools dichiarati al modello (schema Anthropic-compatible).
    tools_json: list[dict]
    # M16 — Tool scoperti via nexus_mcp_tool_search da iniettare come native nel
    # SOLO turno successivo. Reducer di default (overwrite): [] azzera i
    # discovered del turno precedente, garantendo durata esatta 1 turno.
    discovered_tools_next_turn: list[dict]
    # System prompt del profilo agente (vuoto = default).
    system_text: str
    # Session/tool runner wiring (iniettati dal chiamante di /agent/run).
    session_id: str | None
    # Flag HITL: True dopo /agent/approve, permette di saltare interrupt in loop.
    approved: bool
    # Provider/model forzati dall'esterno (override routing).
    provider_override: str | None
    model_override: str | None
    # Profilo agente selezionato (core/github/specialized/general).
    # None = nessun profilo (comportamento legacy).
    profile_name: str | None

    # ── Metriche AI estese ──────────────────────────────────────────────────
    # Tokens e costi tracciati dal nodo executor
    prompt_tokens: int | None
    completion_tokens: int | None
    cache_creation_tokens: int | None
    cache_read_tokens: int | None
    total_tokens: int | None
    total_cost_usd: float | None
    cache_hit_rate: float | None
    temperature: float | None
    top_p: float | None
    created_at: str | None  # ISO8601
    completed_at: str | None  # ISO8601

    # ── Self-reflection (Fase 2) ────────────────────────────────────────────
    # Punteggio prodotto dal reflection_node (0.0-1.0). None = skip per sampling.
    reflection_score: float | None
    # Dettaglio per dimensione: correctness, completeness, efficiency, safety
    reflection_dimensions: dict | None
    # Punti deboli rilevati dal valutatore
    reflection_weaknesses: list | None
    # Suggerimenti di miglioramento del prompt
    reflection_suggestions: list | None
    # Reward finale fuso: 0.7 * heuristic + 0.3 * reflection_score
    final_reward: float | None

    # ── Plan/Act/Verify (PR-1+, opt-in via orchestrator.plan_phase_enabled) ─
    # Flag: True quando il planner_node ha prodotto un piano per questo run.
    plan_phase_active: bool
    # Motivo dello skip del planner (per logging/debug). None se attivo.
    plan_phase_skip_reason: str | None
    # UUID del plan corrente (= run_id Nexus = thread_id LangGraph).
    current_plan_id: str | None
    # Snapshot del plan letto dal DB al termine del planner_node.
    current_todos: list[dict]
    # Acceptance criteria globali del plan (popolato in PR-1; usato in PR-2 dal verifier).
    acceptance_criteria: list[dict]
    # ID del todo "attivo" (in_progress o primo pending). Aggiornato dal verifier in PR-2.
    active_todo_id: str | None
    # ── Cluster 1 (plan_rationale): contesto decisionale del planner forte ──
    # Popolati dal planner_node (gated plan_rationale_enabled); iniettati
    # dall'executor nel system_text per non perdere il "perche'" tra le fasi.
    plan_rationale: str | None
    plan_constraints: list[str] | None
    plan_alternatives: list[dict] | None
    # Contesto RAG (decisioni/interazioni passate) recuperato PRIMA di pianificare.
    plan_rationale_context: str | None
    # ── Cluster 2 (nodo understanding): comprensione pre-planning ──
    # context_brief: grounding semantico + esplorazioni, iniettato nel planner.
    context_brief: str | None
    understanding_active: bool
    understanding_skip_reason: str | None
    # Contatore per il reminder injection: incrementato in tool_dispatch_node,
    # reset post-injection. Soglia in orchestrator.todo_reminder_every_n_steps.
    since_last_todo_reminder: int
    # PR-2: ciclo verifier corrente per active_todo (reset a 0 ad ogni todo).
    verify_cycle: int
    # Cluster 3: ciclo della verifica esplorativa LLM (cap dedicato, reset per todo).
    exploratory_verify_cycle: int
    # Cap GLOBALE per run della verifica esplorativa (cumulativo, MAI resettato
    # per todo): evita il loop su molti todo (exploratory_verify_max_total).
    exploratory_verify_total: int
    # Final gate generale (fail-closed) per task software senza plan_phase:
    # ciclo corrente del gate anti-placeholder (cap final_gate_max_cycles).
    final_gate_cycle: int
    # PR-2: ultimo risultato del verifier (criteria_results).
    verifier_last_result: dict | None
    # PR-2: contatore revisioni strutturali del plan (cap max_plan_revisions).
    plan_revisions: int
    # PR-3 (sub-agents): popolati quando lo state e' di una sub-run.
    parent_run_id: str | None
    subagent_depth: int
    subagent_results: list[dict]
    active_subagent_runs: list[str]
    subagent_cost_cumulative_usd: float

    # FIX 4 (ADR 0012): byte cumulativi letti via nexus_read_attachment /
    # nexus_read_archive_entry nella sessione. Confrontato con il setting
    # agent.attachment.session_read_budget_bytes per bloccare letture seriali
    # che saturano il context window (caso reale: 4 chunk binari di canvas.fig).
    attachment_read_bytes: int

    # G1: contatore nudge iniettati per richieste d'azione senza tool call.
    # Reset a 0 ad ogni nuovo run. Cap a 2 (evita loop di nudge).
    action_nudge_count: int

    # G1: contatore di re-routing eseguiti da route_after_executor verso
    # executor per motivo "risposta descrittiva su action request".
    # Indipendente da action_nudge_count (che e' alzato solo se il nudge
    # viene effettivamente iniettato): conta i giri di re-execution G1,
    # serve per il cap configurabile via settings DB (agent.g1_max_nudges)
    # ed evita loop infiniti quando il nudge non puo' essere iniettato
    # (es. history contiene gia' tool call). Reset a 0 ad ogni nuovo run.
    g1_reroute_count: int

    # Loop-detection semantica: numero di chiamate a tool di SOLA esplorazione
    # (lettura/ispezione allegati e file) consecutive, senza tool produttivi in
    # mezzo. Una sola call produttiva (write_file, edit_file, run_command, ...)
    # azzera il contatore. Oltre la soglia (agent.exploration_loop_threshold)
    # iniettiamo un nudge; a 2x la soglia abortiamo. Reset a 0 ad ogni nuovo run.
    consecutive_exploration_calls: int
    # True dopo aver iniettato il nudge anti-esplorazione, per non ripeterlo a
    # ogni giro mentre il contatore resta sopra soglia. Reset quando il
    # contatore viene azzerato da una call produttiva.
    exploration_nudge_sent: bool
    # True dopo aver iniettato il nudge anti-loop-comando ripetuto (fix
    # 30/05/2026): evita di ripetere il nudge ad ogni giro se il modello
    # continua a chiamare lo stesso comando fallito.
    repeated_cmd_nudge_sent: bool

    # M61 sticky cascade fallback: dopo un cascade riuscito, persisti il
    # provider/model effettivo cosi' le iter successive partono direttamente
    # da li' invece di ri-tentare il primario fallito ad ogni round.
    sticky_provider: str | None
    sticky_model: str | None

    # M69 sticky cascade specifico per planner_node: dopo un cascade riuscito
    # nel planner, memorizza il provider/model effettivo cosi' i replan
    # successivi (o riusi del planner) partono direttamente da li' senza
    # ri-tentare la chain completa (anthropic -> openai -> google -> deepseek).
    planner_sticky_provider: str | None
    planner_sticky_model: str | None

    # Modalita' automazione del turno chat propagata da mcp-core.
    # Valori attesi: "none" | "confirm" | "automatic" | "continuous".
    # Usato dal clarify_or_expand_node per saltare la domanda di chiarimento
    # quando l'utente ha scelto un livello autonomo ("automatic"/"continuous"):
    # l'agente downstream esplora invece di bloccare l'utente.
    automation_mode: str | None

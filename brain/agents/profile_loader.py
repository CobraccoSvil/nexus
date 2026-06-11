"""Loader dei profili agent (equivalente Python di nexus-agents).

I profili definiscono:
- `name`: identificatore univoco (match di AgentType in nexus-agents).
- `category`: core | github | specialized | general.
- `prompt_key`: chiave in `nexus_prompt_templates` (popolata dalla migrazione
  0059). A runtime viene risolta via `prompt_registry.get_prompt(key)`.
- `allowed_tools`: whitelist di tool names esposti al modello per il profilo.
  `["*"]` = nessun filtro (tutti i tool passati dal chiamante).
- `model`: override provider/model di default (opzionale).
- `temperature`: override temperature (opzionale).

I profili sono caricati dalla directory `brain/agents/profiles/*.yaml`
se presente, altrimenti sono forniti inline (fallback deterministico per
test/sviluppo senza dipendenze filesystem).

`route_profile_for_intent()` mappa l'intent prodotto dal `SemanticRouter`
sul profilo piu' adatto. Se nessun profilo matcha, ritorna `None`
(il router_node lascia il default: nessun system_text).
"""
from __future__ import annotations

import logging
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)

# Directory dei profili YAML (opzionale).
PROFILES_DIR = Path(__file__).parent / "profiles"


@dataclass
class AgentProfile:
    """Profilo di un agente specializzato."""
    name: str
    category: str  # core | github | specialized | general
    prompt_key: str
    allowed_tools: list[str] = field(default_factory=lambda: ["*"])
    model: str | None = None
    temperature: float | None = None
    description: str = ""

    # Tool sempre disponibili indipendentemente da profilo/intent.
    # recall_context: infrastruttura contesto (sempre necessario).
    # write_file, edit_file: tool di scrittura essenziali — senza questi l'agente
    # perde la capacita' di creare/modificare file quando il classifier intent
    # sceglie un subset restrittivo (es. "analyze", "review"). Il costo token
    # delle definizioni e' trascurabile (~200 token) rispetto al rischio di
    # bloccare l'agente.
    # delete_file, rename_file: i fratelli completi del set write/edit. Senza
    # questi, su intent restrittivi (es. code_read) l'agente che riceve
    # "cancella il file X" o "rinomina X" allucina la risposta (bug audit
    # 28/05/2026: utente chiede delete, classifier va su code_read, tool
    # delete_file non disponibile, agente risponde "fatto" senza fare nulla).
    # run_command, run_service: tool di esecuzione essenziali — senza questi
    # l'agente non puo' buildare, installare dipendenze, avviare server di
    # sviluppo ne' verificare il proprio lavoro. Il gating difensivo per
    # automation_mode study avviene a monte (Rust, build_tools_json_for_agent)
    # quindi questi tool NON bypassano il filtro study-mode.
    # nexus_mcp_tool_search, nexus_mcp_tool_call: infrastruttura di tool
    # discovery dinamico (lazy mode). Senza questi sul profilo, il modello
    # NON puo' scoprire ne' invocare tool oltre quelli statici in allowed_tools
    # (es. nexus_inspect_attachment per leggere allegati binari, nexus_extract_*
    # per PDF/DOCX/Figma). Bug osservato 30/05/2026: profilo architect su prompt
    # "Crea applicazione da .make" non aveva accesso a nexus_inspect_attachment
    # — Vertex ha chiamato run_command, visto bytes binari e si e' arreso
    # con risposta descrittiva (G1 cap). Soluzione: rendere always-on i tool
    # di discovery cosi' lazy mode funziona davvero su ogni profilo.
    # nexus_inspect_attachment: punto di ingresso obbligato per riconoscere
    # il tipo reale di un allegato (magic byte detection) e ottenere il
    # next_action_recommended. Senza questo, il modello tenta read_file e
    # fallisce sui binari. Gli altri tool nexus_extract_* / nexus_read_*
    # vengono invocati via nexus_mcp_tool_call con server_id="builtin",
    # cosi' il toolset principale resta snello (sistema lazy discovery).
    _ALWAYS_ON_TOOLS = {
        "recall_context",
        "write_file", "edit_file", "delete_file", "rename_file",
        "run_command", "run_service",
        "nexus_mcp_tool_search", "nexus_mcp_tool_call",
        "nexus_inspect_attachment",
        # Tool scaffolding shadcn (mig 0231): risolve loop su 'npx shadcn add'
        # creando stub funzionali in src/components/ui/ senza npm.
        "nexus_install_shadcn_components",
        # Tool auto-healing dev server (mig 0232): diagnose pattern + fix.
        "nexus_dev_server_diagnose",
        # Tool verify scaffolding post-extract: check completezza prima del run.
        "nexus_verify_scaffold",
        # Tool gestione DB applicativo del progetto (31/05/2026): query ad-hoc,
        # lista tabelle, describe. Sostituiscono psql (non installato) per
        # SELECT/INSERT/DDL sul DB dedicato del progetto.
        "nexus_db_query",
        "nexus_db_tables",
        "nexus_db_describe",
    }

    def filter_tools(self, tools_json: list[dict]) -> list[dict]:
        """Restituisce solo i tool ammessi per il profilo.

        Se `allowed_tools == ["*"]` non filtra. Altrimenti include solo i tool
        il cui `name` compare nella whitelist.
        I tool in `_ALWAYS_ON_TOOLS` bypassano il filtro (infrastruttura contesto).
        """
        if not self.allowed_tools or self.allowed_tools == ["*"]:
            return list(tools_json)
        allow = set(self.allowed_tools)
        return [t for t in tools_json if t.get("name") in allow or t.get("name") in self._ALWAYS_ON_TOOLS]

    def filter_tools_for_intent(
        self, tools_json: list[dict], intent: str | None,
    ) -> list[dict]:
        """Doppio filtraggio: whitelist profilo + restrizione per intent (BP5).

        Per intent specifici (chat, analyze, review) non serve l'intero
        toolset edit/run -- riduciamo le tool defs inviate al modello.
        Per intent edit-heavy o sconosciuti torniamo al filtraggio standard.
        """
        first_pass = self.filter_tools(tools_json)
        if not intent:
            return first_pass
        intent_subset = _INTENT_TOOL_SUBSET.get(intent)
        if intent_subset is None:
            return first_pass
        if "*" in intent_subset:
            return first_pass
        allow = set(intent_subset)
        return [t for t in first_pass
                if t.get("name") in allow or t.get("name") in self._ALWAYS_ON_TOOLS]


# ── Lazy Tool Discovery Toolkit ──────────────────────────────────────────────
# Subset minimo passato al modello per intent generici (code/implement/fix).
# Il modello cerca i tool che gli servono via `nexus_mcp_tool_search` e li
# invoca via `nexus_mcp_tool_call`, evitando di caricare tutti i ~479 tool
# nel context. Sostituisce il vecchio comportamento ["*"] che saturava ctx.
#
# Composizione:
#   - nexus_mcp_tool_search: ricerca semantica + ILIKE su nome/descrizione
#   - nexus_mcp_tool_call: invocazione tool dato server_id + tool_name
#   - read_file/read_file_lines/list_files/search_in_files: lettura base
#     (sempre utile per orientarsi rapidamente senza scoperta dinamica)
#   - git_status: stato repo (frequentissimo nei task code/fix/debug)
#
# I tool _ALWAYS_ON_TOOLS (recall_context, write_file, edit_file, run_command,
# run_service) bypassano comunque questo filtro, quindi il modello mantiene
# capacita' di scrittura/esecuzione anche in lazy mode.
_LAZY_MINIMAL_TOOLKIT = [
    "nexus_mcp_tool_search",
    "nexus_mcp_tool_call",
    "read_file",
    "read_file_lines",
    "list_files",
    "search_in_files",
    "search_codebase_semantic",
    "git_status",
    # UI-side: l'agente puo' aprire un file nell'editor del web-ide quando
    # l'utente lo chiede (es. "apri main.rs") o per evidenziare un file
    # menzionato nella risposta. Il tool ritorna { _ui_action: "open_file" }
    # che il frontend intercetta per dispatchare l'evento.
    "nexus_open_file_in_editor",
    # Delega a sub-agent (gia' abilitata via orchestrator.subagents_enabled):
    # esposta direttamente nel toolkit agentico cosi' il modello puo' delegare
    # task multi-file/multi-dominio a sub-agent specializzati senza doverla
    # scoprire prima via nexus_mcp_tool_search (riduce i giri M16). I guard-rail
    # (whitelist kind, max_depth, cost_cap) restano lato server.
    "dispatch_subagent",
    "dispatch_subagents",
    # Governance porte: verifica (read-only) e allocazione. Inclusi nel lazy
    # toolkit cosi' il modello li ha nativi senza un giro di discovery — il
    # messaggio di rifiuto dello scanner dice "chiama request_port" e non deve
    # richiedere un nexus_mcp_tool_search per essere eseguibile.
    "nexus_list_ports",
    "request_port",
]


# ── Mapping intent → subset di tool (BP5 piano riduzione token) ─────────────
# Gli intent sono prodotti dal SemanticRouter (brain/agents/router.py).
# Per ogni intent dichiariamo un sottoinsieme massimo di tool consentiti.
# Il filtro effettivo intersecta questo subset con la whitelist del profilo:
# entrambi devono permettere il tool perche' arrivi al modello.
#
# Convenzioni:
# - "*" come unico elemento: nessun filtro per intent (usa solo whitelist profilo)
# - lista vuota: nessun tool inviato (utile per chat puramente conversazionali)
#
# I tool in AgentProfile._ALWAYS_ON_TOOLS bypassano sempre questo filtro.
# Mantenere coerente con i nomi tool definiti nel registry Rust/Python.
_INTENT_TOOL_SUBSET: dict[str, list[str]] = {
    # Conversazione generica: solo apri file (utile per "apri X" anche in chat),
    # niente tool di esecuzione/scrittura.
    "chat": ["nexus_open_file_in_editor"],
    "general_chat": ["nexus_open_file_in_editor"],
    # Analisi e review: solo lettura.
    "analyze": [
        "read_file", "read_file_lines", "list_files", "search_in_files",
        "search_codebase_semantic", "search_file_semantic",
        "scan_code_quality", "git_status", "nexus_list_ports",
    ],
    "review": [
        "read_file", "read_file_lines", "list_files", "search_in_files",
        "search_codebase_semantic", "git_status", "nexus_list_ports",
    ],
    "code_read": [
        "read_file", "read_file_lines", "list_files", "search_in_files",
        "search_codebase_semantic", "search_file_semantic", "nexus_list_ports",
    ],
    # Refactor: lettura + edit, niente run_command/run_tests (evita side-effects
    # accidentali su intent ambigui). request_port + nexus_list_ports: senza
    # request_port lo scanner bloccherebbe ogni edit con porta senza via d'uscita.
    "refactor": [
        "read_file", "read_file_lines", "list_files", "search_in_files",
        "search_codebase_semantic", "write_file", "edit_file",
        "git_status", "git_stage", "nexus_list_ports", "request_port",
    ],
    # Edit/code/implement/fix: lazy tool discovery via nexus_mcp_tool_search.
    # Prima: ["*"] passava TUTTI i ~479 tool al modello, saturando il context
    # (visti 114K token, 89% ctx prima ancora del primo step) e causando loop
    # di esplorazione invece di azione. Ora il modello riceve un MINIMAL TOOLKIT
    # (~10 tool: search/call meta-tool + lettura base) e cerca i tool specifici
    # via nexus_mcp_tool_search → nexus_mcp_tool_call.
    "code": _LAZY_MINIMAL_TOOLKIT,
    "code_edit": _LAZY_MINIMAL_TOOLKIT,
    "implement": _LAZY_MINIMAL_TOOLKIT,
    "fix": _LAZY_MINIMAL_TOOLKIT,
    "debug": _LAZY_MINIMAL_TOOLKIT,
    # file_ops: intent generico per operazioni su file (lettura + scrittura).
    # Visto classificare anche messaggi conversazionali come "ciao" se la chat
    # precedente conteneva generazione doc/file ops (contaminazione contesto).
    # Lazy toolkit + always-on (write_file, edit_file) sono sufficienti.
    "file_ops": _LAZY_MINIMAL_TOOLKIT,
    "scaffold_app": _LAZY_MINIMAL_TOOLKIT,
    "architecture": _LAZY_MINIMAL_TOOLKIT,
    # agentic_default: fallback neutro quando il classifier LLM non e'
    # disponibile. Diamo il lazy toolkit (discovery + lettura) cosi' l'agente
    # interpreta da se' e scopre i tool necessari, invece di limitarlo a priori.
    "agentic_default": _LAZY_MINIMAL_TOOLKIT,
    # Doc generation: SOLO nexus_doc_generate (single shot).
    # NON includere write_file/edit_file: bypassano il catalogo DB nexus_docs.
    # NON includere read_file/list_files: il backend handle_doc_generate
    # genera il content_json automaticamente via purpose `docs_generator`,
    # quindi l'agente non deve esplorare il codebase — chiama solo il tool.
    # Includere troppi tool causa loop di esplorazione (visti 19+ step e timeout)
    # invece di chiamare immediatamente nexus_doc_generate.
    "doc_generate": ["nexus_doc_generate"],
    # Alias usato dal classificatore quando l'utente chiede di generare doc
    # dal pannello DOCUMENTI (prompt comincia con "ISTRUZIONE PRIORITARIA").
    "docs": ["nexus_doc_generate"],
}


# ── Catalog interno: 4 core + 13 github + 20 specialized + 23 general = 60 ──

# Set di tool di default per categoria (allinea comportamento ai registry Rust).
_READ_ONLY_TOOLS = [
    "read_file", "read_file_lines", "list_files", "search_in_files",
    "git_status", "search_codebase_semantic", "search_file_semantic",
    "scan_code_quality", "batch_analyze_code", "recall_context",
    # Verifica/audit dello stato porte governato (sola lettura): bucket +
    # allocazioni. Senza questo, un task "verifica le porte del progetto"
    # non aveva alcun tool Nexus e deduceva porte hardcoded dai sorgenti.
    "nexus_list_ports",
]
_CODE_EDIT_TOOLS = _READ_ONLY_TOOLS + [
    "write_file", "edit_file", "delete_file", "rename_file",
    "git_stage", "git_commit", "run_command", "run_tests",
    # Allocazione porta governata (unico modo sanzionato). Nessun profilo lo
    # whitelistava: causa radice dell'incidente (l'agente non poteva chiamarlo).
    "request_port",
]
_SERVICE_TOOLS = _CODE_EDIT_TOOLS + [
    "run_service", "read_service_output", "stop_service",
    "build_project_image",
]
_GIT_ONLY_TOOLS = [
    "git_status", "git_stage", "git_commit", "git_push", "git_pull",
    "read_file", "list_files", "search_in_files",
]


def _core_profiles() -> list[AgentProfile]:
    return [
        AgentProfile("coder", "core", "agent.coder.base",
                     _CODE_EDIT_TOOLS, description="Implementazione feature e bugfix"),
        AgentProfile("tester", "core", "agent.tester.base",
                     _CODE_EDIT_TOOLS + ["run_tests"],
                     description="Generazione ed esecuzione test"),
        AgentProfile("reviewer", "core", "agent.reviewer.general",
                     _READ_ONLY_TOOLS, description="Code review e bug detection"),
        AgentProfile("architect", "core", "agent.architect.general",
                     _READ_ONLY_TOOLS + ["write_file"],
                     description="Progettazione architettura e schema"),
    ]


def _github_profiles() -> list[AgentProfile]:
    gh_roles = [
        ("github_pr_manager",       "agent.github.pr_manager"),
        ("github_code_reviewer",    "agent.github.code_reviewer"),
        ("github_issue_analyzer",   "agent.github.issue_analyzer"),
        ("github_release_manager",  "agent.github.release_manager"),
        ("github_workflow_manager", "agent.github.workflow_manager"),
        ("github_security_analyzer","agent.github.security_analyzer"),
        ("github_dependency_manager","agent.github.dependency_manager"),
        ("github_project_manager",  "agent.github.project_manager"),
        ("github_wiki_manager",     "agent.github.wiki_manager"),
        ("github_discussion_moderator","agent.github.discussion_moderator"),
        ("github_actions_optimizer","agent.github.actions_optimizer"),
        ("github_status_monitor",   "agent.github.status_monitor"),
        ("github_integration_bot",  "agent.github.integration_bot"),
    ]
    return [
        AgentProfile(name, "github", key, _GIT_ONLY_TOOLS,
                     description=f"GitHub workflow: {name.replace('github_', '').replace('_', ' ')}")
        for name, key in gh_roles
    ]


def _specialized_profiles() -> list[AgentProfile]:
    spec_roles = [
        ("security_architect",   "agent.specialized.security_architect",   _READ_ONLY_TOOLS),
        ("performance_engineer", "agent.specialized.performance_engineer", _CODE_EDIT_TOOLS),
        ("database_designer",    "agent.specialized.database_designer",    _CODE_EDIT_TOOLS),
        ("frontend_specialist",  "agent.specialized.frontend_specialist",  _CODE_EDIT_TOOLS),
        ("backend_specialist",   "agent.specialized.backend_specialist",   _SERVICE_TOOLS),
        ("devops_engineer",      "agent.specialized.devops_engineer",      _SERVICE_TOOLS),
        ("cloud_architect",      "agent.specialized.cloud_architect",      _SERVICE_TOOLS),
        ("mobile_specialist",    "agent.specialized.mobile_specialist",    _CODE_EDIT_TOOLS),
        ("data_scientist",       "agent.specialized.data_scientist",       _CODE_EDIT_TOOLS),
        ("ml_engineer",          "agent.specialized.ml_engineer",          _CODE_EDIT_TOOLS),
        ("qa_specialist",        "agent.specialized.qa_specialist",        _CODE_EDIT_TOOLS),
        ("tech_lead",            "agent.specialized.tech_lead",            _READ_ONLY_TOOLS),
        ("researcher",           "agent.specialized.researcher",           _READ_ONLY_TOOLS),
        ("analyst",              "agent.specialized.analyst",              _READ_ONLY_TOOLS),
        ("optimizer",            "agent.specialized.optimizer",            _CODE_EDIT_TOOLS),
        ("documenter",           "agent.specialized.documenter",           _CODE_EDIT_TOOLS),
        ("sre_engineer",         "agent.specialized.sre_engineer",         _SERVICE_TOOLS),
        ("api_designer",         "agent.specialized.api_designer",         _CODE_EDIT_TOOLS),
        ("prompt_engineer",      "agent.specialized.prompt_engineer",      _READ_ONLY_TOOLS),
        ("agent_engineer",       "agent.specialized.agent_engineer",       _CODE_EDIT_TOOLS),
    ]
    return [
        AgentProfile(name, "specialized", key, tools,
                     description=f"Specialista: {name.replace('_', ' ')}")
        for name, key, tools in spec_roles
    ]


def _general_profiles() -> list[AgentProfile]:
    gen_roles = [
        ("debugger",                "agent.general.debugger",                _CODE_EDIT_TOOLS),
        ("refactorer",              "agent.general.refactorer",              _CODE_EDIT_TOOLS),
        ("profiler",                "agent.general.profiler",                _CODE_EDIT_TOOLS),
        ("infra_engineer",          "agent.general.infra_engineer",          _SERVICE_TOOLS),
        ("database_admin",          "agent.general.database_admin",          _SERVICE_TOOLS),
        ("security_auditor",        "agent.general.security_auditor",        _READ_ONLY_TOOLS),
        ("compliance_officer",      "agent.general.compliance_officer",      _READ_ONLY_TOOLS),
        ("ui_designer",             "agent.general.ui_designer",             _CODE_EDIT_TOOLS),
        ("accessibility_engineer",  "agent.general.accessibility_engineer",  _CODE_EDIT_TOOLS),
        ("data_engineer",           "agent.general.data_engineer",           _CODE_EDIT_TOOLS),
        ("etl_engineer",            "agent.general.etl_engineer",            _CODE_EDIT_TOOLS),
        ("automation_engineer",     "agent.general.automation_engineer",     _SERVICE_TOOLS),
        ("integration_engineer",    "agent.general.integration_engineer",    _SERVICE_TOOLS),
        ("monitoring_engineer",     "agent.general.monitoring_engineer",     _SERVICE_TOOLS),
        ("migration_engineer",      "agent.general.migration_engineer",      _CODE_EDIT_TOOLS),
        ("chatbot_engineer",        "agent.general.chatbot_engineer",        _CODE_EDIT_TOOLS),
        ("embedding_engineer",      "agent.general.embedding_engineer",      _CODE_EDIT_TOOLS),
        ("tech_writer",             "agent.general.tech_writer",             _READ_ONLY_TOOLS + ["write_file"]),
        ("product_owner",           "agent.general.product_owner",           _READ_ONLY_TOOLS),
        ("benchmark_engineer",      "agent.general.benchmark_engineer",      _CODE_EDIT_TOOLS),
        ("test_automation_engineer","agent.general.test_automation_engineer",_CODE_EDIT_TOOLS),
        ("reporting_engineer",      "agent.general.reporting_engineer",      _CODE_EDIT_TOOLS),
        ("i18n_engineer",           "agent.general.i18n_engineer",           _CODE_EDIT_TOOLS),
    ]
    return [
        AgentProfile(name, "general", key, tools,
                     description=f"Generalista: {name.replace('_', ' ')}")
        for name, key, tools in gen_roles
    ]


def _builtin_profiles() -> dict[str, AgentProfile]:
    profiles = []
    profiles.extend(_core_profiles())
    profiles.extend(_github_profiles())
    profiles.extend(_specialized_profiles())
    profiles.extend(_general_profiles())
    return {p.name: p for p in profiles}


# ── Caching + accessor pubblici ─────────────────────────────────────────────

_cache: dict[str, AgentProfile] | None = None


def _load_yaml_dir() -> dict[str, AgentProfile]:
    """Prova a caricare YAML dalla directory profiles/. Best-effort."""
    if not PROFILES_DIR.exists():
        return {}
    try:
        import yaml  # type: ignore[import-untyped]
    except ImportError:
        logger.debug("PyYAML non installato: uso profili inline")
        return {}
    out: dict[str, AgentProfile] = {}
    for path in sorted(PROFILES_DIR.glob("*.yaml")):
        try:
            with path.open(encoding="utf-8") as fh:
                data = yaml.safe_load(fh) or {}
            profile = AgentProfile(
                name=data["name"],
                category=data.get("category", "general"),
                prompt_key=data.get("prompt_key", f"agent.{data['name']}"),
                allowed_tools=list(data.get("allowed_tools", ["*"])),
                model=data.get("model"),
                temperature=data.get("temperature"),
                description=data.get("description", ""),
            )
            out[profile.name] = profile
        except Exception as exc:
            logger.warning("profile YAML invalido %s: %s", path.name, exc)
    return out


def _profiles() -> dict[str, AgentProfile]:
    global _cache
    if _cache is None:
        base = _builtin_profiles()
        overrides = _load_yaml_dir()
        base.update(overrides)
        _cache = base
        logger.info("profile_loader: caricati %d profili (%d da YAML)",
                    len(base), len(overrides))
    return _cache


def get_profile(name: str) -> AgentProfile | None:
    """Restituisce il profilo per nome, o None se sconosciuto."""
    return _profiles().get(name)


def list_profiles() -> list[AgentProfile]:
    """Lista ordinata di tutti i profili disponibili."""
    return sorted(_profiles().values(), key=lambda p: (p.category, p.name))


def reset_cache() -> None:
    """Helper per test: forza reload al prossimo get_profile."""
    global _cache
    _cache = None


# ── Routing intent → profile ────────────────────────────────────────────────

# Mapping minimo intent-to-profile; il router semantico puo' essere esteso
# con classificazioni piu' granulari. Non copre tutti gli intent: in assenza
# di match il chiamante usa il fallback (nessun profilo).
_INTENT_TO_PROFILE: dict[str, str] = {
    "code_generation": "coder",
    "code_modification": "coder",
    "bug_fix": "debugger",
    "refactoring": "refactorer",
    "test_generation": "tester",
    "code_review": "reviewer",
    "architecture": "architect",
    "database_schema_change": "database_designer",
    "performance_tuning": "performance_engineer",
    "security_audit": "security_auditor",
    "documentation": "tech_writer",
    "devops": "devops_engineer",
    "frontend": "frontend_specialist",
    "backend": "backend_specialist",
    "deployment": "devops_engineer",
    "debugging": "debugger",
    "github_pr": "github_pr_manager",
    "github_issue": "github_issue_analyzer",
    "chat": None,  # type: ignore[dict-item]
}


def route_profile_for_intent(intent: str) -> AgentProfile | None:
    """Seleziona un profilo a partire dall'intent del router semantico."""
    name = _INTENT_TO_PROFILE.get(intent)
    if not name:
        return None
    return get_profile(name)

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

    _ALWAYS_ON_TOOLS = {"recall_context"}

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


# ── Catalog interno: 4 core + 13 github + 20 specialized + 23 general = 60 ──

# Set di tool di default per categoria (allinea comportamento ai registry Rust).
_READ_ONLY_TOOLS = [
    "read_file", "read_file_lines", "list_files", "search_in_files",
    "git_status", "search_codebase_semantic", "search_file_semantic",
    "scan_code_quality", "batch_analyze_code", "recall_context",
]
_CODE_EDIT_TOOLS = _READ_ONLY_TOOLS + [
    "write_file", "edit_file", "delete_file", "rename_file",
    "git_stage", "git_commit", "run_command", "run_tests",
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

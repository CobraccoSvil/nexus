"""Smoke test dei 60 profili agent (Fase 5)."""
from __future__ import annotations

from brain.agents import profile_loader


def test_builtin_count_is_60():
    profiles = profile_loader.list_profiles()
    assert len(profiles) == 60, f"attesi 60 profili, trovati {len(profiles)}"


def test_categories_distribution():
    profiles = profile_loader.list_profiles()
    by_cat: dict[str, int] = {}
    for p in profiles:
        by_cat[p.category] = by_cat.get(p.category, 0) + 1
    assert by_cat == {"core": 4, "github": 13, "specialized": 20, "general": 23}


def test_every_profile_has_prompt_key():
    for p in profile_loader.list_profiles():
        assert p.prompt_key, f"profile {p.name} senza prompt_key"
        assert p.prompt_key.startswith("agent.")


def test_core_profiles_resolvable():
    for name in ("coder", "tester", "reviewer", "architect"):
        assert profile_loader.get_profile(name) is not None


def test_github_profiles_resolvable():
    p = profile_loader.get_profile("github_pr_manager")
    assert p is not None
    assert p.category == "github"


def test_unknown_profile_is_none():
    assert profile_loader.get_profile("nonexistent_agent") is None


def test_filter_tools_respects_allowlist():
    p = profile_loader.get_profile("reviewer")
    assert p is not None
    # I tool fuori allowlist e fuori _ALWAYS_ON_TOOLS vengono filtrati.
    # delete_file invece e' in _ALWAYS_ON_TOOLS dal commit 8101fd7 (bug
    # "cancella il file": senza tool il modello allucinava l'eliminazione)
    # e bypassa il filtro profilo — il gating difensivo per i task read-only
    # avviene a monte (report_only nel router + study mode lato Rust).
    tools = [
        {"name": "read_file"},
        {"name": "delete_file"},
        {"name": "tool_non_ammesso"},
        {"name": "list_files"},
    ]
    filtered = p.filter_tools(tools)
    names = {t["name"] for t in filtered}
    assert "read_file" in names
    assert "list_files" in names
    assert "delete_file" in names  # always-on, bypassa la allowlist
    assert "tool_non_ammesso" not in names


def test_filter_tools_wildcard_passthrough():
    # Creiamo un profilo ad-hoc con wildcard.
    p = profile_loader.AgentProfile(
        name="test_wildcard", category="core", prompt_key="agent.x",
        allowed_tools=["*"],
    )
    tools = [{"name": "a"}, {"name": "b"}]
    assert p.filter_tools(tools) == tools


def test_route_intent_to_profile():
    assert profile_loader.route_profile_for_intent("code_generation").name == "coder"
    assert profile_loader.route_profile_for_intent("bug_fix").name == "debugger"
    assert profile_loader.route_profile_for_intent("unknown_xyz") is None


def test_chat_intent_maps_to_no_profile():
    # "chat" e' esplicitamente None (nessun profilo specializzato).
    assert profile_loader.route_profile_for_intent("chat") is None

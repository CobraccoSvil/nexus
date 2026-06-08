"""Test del matcher del Task-Playbook Engine (brain/agents/task_playbook.py).

Il matcher e' una funzione PURA sugli assi di trigger_json: lo testiamo senza DB
ne LLM, iniettando una cache di playbook sintetici. Copre: match per keyword,
match per attachment_kind, match per project_marker, gate intent, nessun match
(no catch-all involontario), ordinamento per priority, cap _MAX_PLAYBOOKS.
"""
import time

from brain.agents import task_playbook as tp


def _seed(playbooks):
    """Forza la cache dei playbook (bypassa il DB) per il test corrente."""
    tp._pb_cache = playbooks
    tp._pb_cache_ts = time.monotonic()


def _figma_pb(priority=100):
    return {
        "key": "implement.figma_make",
        "trigger": {
            "keywords": ["figma", ".make"],
            "attachment_kind": "figma_make",
            "project_markers": ["figma_export"],
        },
        "guidance": "Estrai con nexus_extract_figma_code; mancano shadcn/Tailwind.",
        "priority": priority,
    }


def test_match_per_keyword():
    _seed([_figma_pb()])
    m = tp.match({"intent": "implement", "text": "apri il figma e realizza l'app"})
    assert [p["key"] for p in m] == ["implement.figma_make"]


def test_match_per_attachment_kind():
    _seed([_figma_pb()])
    m = tp.match({"intent": "x", "text": "realizzala", "attachment_kinds": ["figma_make"]})
    assert [p["key"] for p in m] == ["implement.figma_make"]


def test_match_per_project_marker():
    _seed([_figma_pb()])
    m = tp.match({"intent": "x", "text": "continua", "project_markers": ["figma_export"]})
    assert [p["key"] for p in m] == ["implement.figma_make"]


def test_nessun_match_su_task_estraneo():
    _seed([_figma_pb()])
    m = tp.match({"intent": "fix", "text": "correggi il bug di null pointer nel backend"})
    assert m == []


def test_gate_intent_esclude():
    pb = _figma_pb()
    pb["trigger"]["intent"] = ["implement"]
    _seed([pb])
    # keyword presente ma intent non ammesso -> niente match.
    m = tp.match({"intent": "chat", "text": "parlami di figma"})
    assert m == []


def test_trigger_vuoto_non_e_catch_all():
    _seed([{"key": "empty", "trigger": {}, "guidance": "x", "priority": 100}])
    m = tp.match({"intent": "implement", "text": "qualsiasi cosa"})
    assert m == []


def test_ordinamento_per_priority():
    _seed([
        {"key": "low", "trigger": {"keywords": ["figma"]}, "guidance": "a", "priority": 10},
        {"key": "high", "trigger": {"keywords": ["figma"]}, "guidance": "b", "priority": 200},
    ])
    m = tp.match({"intent": "x", "text": "figma"})
    assert [p["key"] for p in m] == ["high", "low"]


def test_build_block_cap_e_formato():
    pbs = [
        {"key": "a", "trigger": {}, "guidance": "GA", "priority": 1},
        {"key": "b", "trigger": {}, "guidance": "GB", "priority": 1},
        {"key": "c", "trigger": {}, "guidance": "GC", "priority": 1},
    ]
    block = tp.build_block(pbs)
    # Cap a _MAX_PLAYBOOKS: il terzo non compare.
    assert block.count("<task_playbook") == tp._MAX_PLAYBOOKS
    assert '<task_playbook key="a">' in block
    assert "GA" in block

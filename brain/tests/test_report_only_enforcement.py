"""Regressione: task di VERIFICA/REPORT non deve diventare un FIX.

Incidente "verifica->fix" (2026-06-10): "controlla che il backend compili e
il frontend buildi, riporta l'esito" -> l'agente, trovati errori, li
CORREGGEVA invece di riportarli (scope-creep), bloccandosi su un edit. Causa:
i tool write/edit/delete/rename sono in _ALWAYS_ON_TOOLS, disponibili anche
sui task di sola lettura.

Fix: il router_node deriva report_only dal classifier LLM (action_verb
read/analyze con slot affidabili, o intent code_read) e RIMUOVE i tool di
modifica file anche se always-on; inietta la direttiva <modalita_verifica>.
Qui si fissa la logica di derivazione + il filtro tool (funzioni pure,
nessuna chiamata LLM/DB).
"""


def _derive_report_only(authorizes_changes, intent_hint=None):
    """Replica la logica del router_node (punto unico) per testarla isolata.

    Fonte: il giudizio agentico DIRETTO del classifier (authorizes_changes),
    non un proxy su action_verb.
    """
    return (not intent_hint) and (not authorizes_changes)


_MUTATING_FILE_TOOLS = {
    "write_file", "edit_file", "delete_file", "rename_file",
    "nexus_install_shadcn_components",
}


def _filter_report_only(tools):
    return [t for t in tools if t.get("name") not in _MUTATING_FILE_TOOLS]


def test_non_autorizza_modifiche_e_report_only():
    # L'LLM ha giudicato che l'utente vuole solo un report.
    assert _derive_report_only(authorizes_changes=False) is True


def test_autorizza_modifiche_non_e_report_only():
    # "verifica e CORREGGI" / fix / implement -> l'LLM autorizza le modifiche.
    assert _derive_report_only(authorizes_changes=True) is False


def test_default_fail_safe_non_blocca():
    # Classifier-down / campo assente -> default True -> mai report_only.
    assert _derive_report_only(authorizes_changes=True) is False


def test_disambiguazione_risolta_mai_report_only():
    # intent_hint presente: l'utente ha scelto un'azione, mai report-only,
    # anche se per qualche motivo authorizes_changes risultasse False.
    assert _derive_report_only(authorizes_changes=False, intent_hint="fix") is False


def test_filtro_rimuove_solo_tool_di_modifica():
    tools = [
        {"name": "read_file"}, {"name": "list_files"}, {"name": "run_command"},
        {"name": "write_file"}, {"name": "edit_file"}, {"name": "delete_file"},
        {"name": "search_in_files"},
    ]
    out = _filter_report_only(tools)
    names = {t["name"] for t in out}
    # I check restano (read/list/run_command/search), le modifiche spariscono
    assert "run_command" in names
    assert "read_file" in names and "search_in_files" in names
    assert "write_file" not in names and "edit_file" not in names
    assert "delete_file" not in names
    assert len(out) == 4


def test_filtro_noop_senza_tool_di_modifica():
    tools = [{"name": "read_file"}, {"name": "run_command"}]
    assert _filter_report_only(tools) == tools

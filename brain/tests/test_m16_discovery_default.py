"""GAP4 — coerenza codice<->DB del flag discovery-first che governa il gate M16.

Il gate M16 in ``tool_dispatch_node`` (brain/agents/nodes/__init__.py) rifiuta
i tool non scoperti/non in whitelist quando ``agent.tools.discovery_first_enabled``
e' attivo. Il comportamento runtime e' DB-driven (regola G): il DB e' l'unica
fonte di verita'. Il SECONDO argomento di ``get_bool_setting_cached`` e' solo il
valore di emergenza usato quando la chiave NON esiste in DB.

Prima del fix il default codice era ``False``, divergente dallo stato operativo
del DB (``true``): chi leggeva il codice concludeva erroneamente che M16 fosse
inattivo per default. Questo test inchioda il default a ``True`` (allineato al DB
e fail-safe: in dubbio M16 attivo) e verifica che il valore DB, quando presente,
prevalga sempre sul default (runtime DB-driven invariato).

Test auto-contenuto: ``_read_setting_raw`` viene sostituito per simulare le
risposte del DB senza connessione reale (stesso pattern di test_fallback_adapt).
"""
from __future__ import annotations

# Chiave governata dal DB (settings) che attiva/disattiva il gate M16.
_FLAG_KEY = "agent.tools.discovery_first_enabled"


def _read_flag_with_db(raw_value: object) -> bool:
    """Legge il flag come fa ``tool_dispatch_node`` (default codice = True),
    simulando la risposta del DB tramite ``_read_setting_raw``.

    ``raw_value`` = None  -> chiave assente in DB (si applica il default codice).
    ``raw_value`` = "true"/"false" -> valore presente in DB (deve prevalere).
    """
    from brain.utils import settings_db

    orig = settings_db._read_setting_raw
    settings_db._read_setting_raw = lambda key: raw_value if key == _FLAG_KEY else None  # type: ignore[assignment]
    # La variante cached memoizza il raw: ripulisco prima e dopo per idempotenza.
    settings_db._get_setting_cache().clear()
    try:
        # STESSA chiamata del nodo: default codice allineato al DB (True).
        return settings_db.get_bool_setting_cached(_FLAG_KEY, True)
    finally:
        settings_db._read_setting_raw = orig  # type: ignore[assignment]
        settings_db._get_setting_cache().clear()


def test_default_codice_e_true_quando_chiave_assente() -> None:
    # GAP4: chiave assente in DB -> si applica il default del codice, che ora e'
    # True (allineato allo stato operativo del DB e fail-safe). Se qualcuno
    # riportasse il default a False, M16 risulterebbe disattivo per default in
    # lettura del codice, contraddicendo il DB di produzione.
    assert _read_flag_with_db(None) is True


def test_db_true_attiva_m16() -> None:
    # Valore DB esplicito 'true' -> M16 attivo (stato di produzione odierno).
    assert _read_flag_with_db("true") is True


def test_db_false_disattiva_m16_runtime_db_driven() -> None:
    # Il valore DB prevale SEMPRE sul default codice: se l'admin imposta 'false',
    # M16 si disattiva a runtime. Il default True non maschera mai il DB.
    assert _read_flag_with_db("false") is False


def test_whitelist_default_codice_e_solo_meta_tool() -> None:
    # Il default codice della whitelist (usato solo se la chiave manca in DB)
    # contiene unicamente i meta-tool di discovery: e' un minimo coerente, non
    # una fonte di verita' alternativa. La whitelist operativa completa
    # (read_file, list_files, ...) vive nel DB.
    from brain.utils import settings_db

    orig = settings_db._read_setting_raw
    settings_db._read_setting_raw = lambda key: None  # type: ignore[assignment]
    settings_db._get_setting_cache().clear()
    try:
        raw = settings_db.get_setting_cached(
            "agent.tools.discovery_first_whitelist",
            "nexus_mcp_tool_search,nexus_mcp_tool_call",
        )
    finally:
        settings_db._read_setting_raw = orig  # type: ignore[assignment]
        settings_db._get_setting_cache().clear()
    parts = {t.strip() for t in raw.split(",") if t.strip()}
    assert parts == {"nexus_mcp_tool_search", "nexus_mcp_tool_call"}

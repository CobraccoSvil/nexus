"""Test per la feature "scelte di proseguimento" (meta_step next_actions).

Copre la logica PURA di brain/agents/next_actions.py: parsing del blocco
<suggested_actions>, rimozione dal testo visibile, euristica del fallback,
normalizzazione/cap delle scelte e costruzione del meta_step. Niente DB, niente
LLM: tutti i test sono idempotenti e indipendenti dall'ordine.
"""
from brain.agents import next_actions as na


def test_extract_block_parsa_e_rimuove() -> None:
    """Il blocco <suggested_actions> viene parsato e rimosso dal testo visibile."""
    text = (
        "Ho creato la landing page.\n\n"
        "<suggested_actions>\n"
        '[{"label":"Aggiungi galleria","prompt":"Aggiungi una galleria immagini alla landing."},'
        '{"label":"Aggiungi form","prompt":"Integra un form di contatto nella landing."}]\n'
        "</suggested_actions>"
    )
    choices, cleaned = na.extract_block(text)
    assert len(choices) == 2
    assert choices[0]["label"] == "Aggiungi galleria"
    assert "prompt" in choices[1]
    assert "<suggested_actions>" not in cleaned
    assert cleaned.strip() == "Ho creato la landing page."


def test_extract_block_assente() -> None:
    """Senza blocco: nessuna scelta, testo invariato."""
    text = "Risposta normale senza scelte."
    choices, cleaned = na.extract_block(text)
    assert choices == []
    assert cleaned == text


def test_extract_block_json_malformato() -> None:
    """Blocco malformato: nessuna scelta ma il blocco grezzo viene comunque
    rimosso (l'utente non deve mai vederlo)."""
    text = "Testo.\n<suggested_actions>\n[non json]\n</suggested_actions>"
    choices, cleaned = na.extract_block(text)
    assert choices == []
    assert "<suggested_actions>" not in cleaned


def test_extract_block_con_fence_markdown() -> None:
    """Tollera fence markdown attorno al JSON interno."""
    text = (
        "Fatto.\n<suggested_actions>\n```json\n"
        '[{"label":"A","prompt":"Prompt A completo."}]\n'
        "```\n</suggested_actions>"
    )
    choices, _ = na.extract_block(text)
    assert len(choices) == 1
    assert choices[0]["label"] == "A"


def test_coerce_scarta_entry_malformate_e_applica_cap() -> None:
    """Entry senza label/prompt scartate; label troncata; lista limitata a 6."""
    raw = [
        {"label": "ok", "prompt": "p"},
        {"label": "", "prompt": "vuoto"},          # label vuota -> scartata
        {"label": "no-prompt"},                      # manca prompt -> scartata
        {"prompt": "no-label"},                      # manca label -> scartata
        {"label": "x" * 100, "prompt": "lungo"},    # label troncata
    ] + [{"label": f"c{i}", "prompt": "p"} for i in range(10)]
    out = na._coerce_choices(raw)
    assert len(out) <= na._MAX_CHOICES
    assert all(c["label"] and c["prompt"] for c in out)
    assert all(len(c["label"]) <= na._MAX_LABEL_CHARS for c in out)


def test_looks_like_choices_euristica() -> None:
    """L'euristica scatta su domande multiple o formule italiane di proposta."""
    # Due punti interrogativi -> scatta.
    assert na.looks_like_choices("Vuoi una galleria? Aggiungo un form?") is True
    # Due formule di proposta (anche con un solo "?") -> scatta.
    assert na.looks_like_choices("Vuoi che aggiunga immagini? Preferisci un form?") is True
    # Conservativa: un solo segnale non basta.
    assert na.looks_like_choices("Vuoi che aggiunga immagini al sito.") is False
    assert na.looks_like_choices("Ho completato il lavoro richiesto.") is False
    assert na.looks_like_choices("") is False


def test_build_step_none_se_vuoto() -> None:
    """Nessuna scelta -> nessun meta_step."""
    assert na.build_step([]) is None


def test_build_step_contratto_frontend() -> None:
    """Il meta_step rispetta il contratto kind/title/payload concordato col FE."""
    choices = [{"label": "L", "prompt": "P completo"}]
    step = na.build_step(choices)
    # make() puo' tornare None se il kind e' disabilitato dai flag settings; in
    # ambiente test senza DATABASE_URL i flag sono ai default (global_enabled).
    assert step is not None
    assert step["kind"] == na.META_KIND == "next_actions"
    assert step["title"] == "Prossimi passi"
    assert step["payload"]["choices"] == choices


def test_redact_non_espone_contenuto() -> None:
    """Il redactor per i log non rivela il testo in chiaro (regola F)."""
    out = na._redact("contenuto segreto del prompt")
    assert "segreto" not in out
    assert "len=" in out and "sha1=" in out

"""Regressione G1: la chiusura dopo azioni produttive e' il resoconto finale.

Bug (2026-06-10): route_after_executor ri-mandava all'executor QUALSIASI
chiusura end_turn senza tool call su un turno action-oriented — anche il
resoconto finale di un run che aveva GIA' eseguito il lavoro (edit_file
applicato, "Considero l'intervento concluso."). Il nudge G1 poi NON veniva
iniettato (filtrato da _has_tool_calls_in_history: routing e nudge erano
incoerenti), il contatore reroute saliva a vuoto, scattava l'escalation di
modello e infine il cap-text contraddittorio veniva accodato alla risposta
buona ("Modello non risponde con azione dopo 3 tentativi..." DOPO il lavoro
concluso). Impressione utente: "Nexus non pubblica mai il risultato finale".

Fix: guard strutturale has_productive_action_in_history (helpers.py, punto
unico) valutato PRIMA dei trigger G1: se il run ha eseguito almeno un tool
NON esplorativo (write_file/edit_file/run_command/...), la chiusura testuale
e' legittima -> niente reroute. Fatto strutturale, nessuna analisi lessicale.
"""
from langchain_core.messages import AIMessage, HumanMessage

from brain.agents.nodes.helpers import has_productive_action_in_history


def _ai_with_tools(*names: str) -> AIMessage:
    return AIMessage(
        content="",
        additional_kwargs={
            "anthropic_content": [
                {"type": "tool_use", "id": f"t{i}", "name": n, "input": {}}
                for i, n in enumerate(names)
            ]
        },
    )


def test_edit_file_e_azione_produttiva():
    msgs = [HumanMessage(content="fixa il bug"), _ai_with_tools("edit_file")]
    assert has_productive_action_in_history(msgs) is True


def test_run_command_e_azione_produttiva():
    msgs = [_ai_with_tools("run_command")]
    assert has_productive_action_in_history(msgs) is True


def test_sola_esplorazione_non_e_produttiva():
    # read_file/list_files/grep sono esplorativi: il G1 deve poter scattare.
    msgs = [
        HumanMessage(content="fixa il bug"),
        _ai_with_tools("read_file", "list_files", "grep", "search_in_files"),
    ]
    assert has_productive_action_in_history(msgs) is False


def test_nessun_tool_non_e_produttivo():
    msgs = [HumanMessage(content="fixa"), AIMessage(content="Faro' questo e quello...")]
    assert has_productive_action_in_history(msgs) is False


def test_mix_esplorazione_poi_scrittura():
    # Scenario incidente: read app.ts -> edit app.ts -> resoconto finale.
    msgs = [
        _ai_with_tools("read_file"),
        _ai_with_tools("edit_file"),
        AIMessage(content="Ho diagnosticato e risolto. Considero l'intervento concluso."),
    ]
    assert has_productive_action_in_history(msgs) is True


def test_history_vuota():
    assert has_productive_action_in_history([]) is False


# ── Raffinamento guard (incidente "non finisce il lavoro" 2026-06-10 pom.) ──
# Il guard salta il G1 solo se la chiusura e' un resoconto CONCLUSIVO: una
# frase che ANNUNCIA un'azione imminente (intenzione non compiuta) deve
# tornare all'executor anche se il run ha gia' azioni produttive (es. un
# run_command di solo check). Qui si fissa il comportamento del rilevatore
# sulle frasi REALI dei due run incriminati.
from brain.agents.nodes.helpers import _detect_unfulfilled_intent


def test_intenzione_annunciata_run_reale_1():
    txt = ("1. Forza una ricompilazione pulita.\n"
           "2. Verifica eventuali errori di compilazione.\n\n"
           "Inizio con la verifica dei file generati in dist/services/.")
    assert _detect_unfulfilled_intent(txt) is True


def test_intenzione_annunciata_run_reale_2():
    txt = "Backend compila senza errori. Ora verifico il frontend:"
    assert _detect_unfulfilled_intent(txt) is True


def test_resoconto_conclusivo_non_e_intenzione():
    txt = ("Ho diagnosticato e risolto il problema. La correzione e' stata "
           "applicata con successo. Considero l'intervento concluso.")
    assert _detect_unfulfilled_intent(txt) is False

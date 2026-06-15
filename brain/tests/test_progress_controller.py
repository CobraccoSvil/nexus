"""Test del punto unico progress_controller.decide (funzione pura).

Verificano la gerarchia coordinata guida -> escalate -> abort-verso-verifica per
ogni asse di stallo, l'assenza di abort prematuro (il bug dominante: l'esplorazione
abortiva senza prima forzare l'azione) e i default neutri.
"""
from brain.agents.progress_controller import (
    ABORT_STOP_REASON,
    ProgressDecision,
    ProgressSignals,
    _is_build_or_test_label,
    decide,
)


def test_no_stallo_proceed():
    """Sotto soglia su ogni asse -> proceed, niente nudge ne' abort."""
    d = decide(ProgressSignals(exploration_count=3, exploration_threshold=6))
    assert d.action == "proceed"
    assert d.force_action is False
    assert d.nudge_text is None
    assert d.stop_reason is None


def test_esplorazione_2x_prima_volta_guida_non_aborta():
    """Caso dominante: a 2x soglia, se mai guidata, NON aborta -> forza-azione.

    E' il fix di fondo: prima si costringe ad agire (rimuovi read-only +
    tool_choice required), non si chiude il run.
    """
    d = decide(ProgressSignals(exploration_count=12, exploration_threshold=6))
    assert d.action == "guide"
    assert d.axis == "exploration"
    assert d.force_action is True
    assert d.nudge_text and "STOP esplorazione" in d.nudge_text
    assert d.stop_reason is None  # NON aborta


def test_esplorazione_2x_gia_guidata_senza_escalation_aborta_verso_verifica():
    """Gia' forzata e ancora bloccata, nessun candidato escalation -> ABORT, ma
    con lo stop_reason coordinato che instrada alla verifica E2E."""
    d = decide(
        ProgressSignals(
            exploration_count=12,
            exploration_threshold=6,
            already_guided=frozenset({"exploration"}),
            has_escalation_candidate=False,
        )
    )
    assert d.action == "abort"
    assert d.stop_reason == ABORT_STOP_REASON
    assert d.force_action is False


def test_esplorazione_2x_gia_guidata_con_candidato_escala_prima_di_abortire():
    """Gia' forzata, c'e' un candidato e budget disponibile -> ESCALATE, non abort."""
    d = decide(
        ProgressSignals(
            exploration_count=12,
            exploration_threshold=6,
            already_guided=frozenset({"exploration"}),
            has_escalation_candidate=True,
            escalations=0,
            max_escalations=3,
        )
    )
    assert d.action == "escalate"
    assert d.axis == "exploration"
    assert d.stop_reason is None


def test_escalation_budget_esaurito_aborta():
    """Budget escalation esaurito -> abort anche se ci sarebbe un candidato."""
    d = decide(
        ProgressSignals(
            exploration_count=12,
            exploration_threshold=6,
            already_guided=frozenset({"exploration"}),
            has_escalation_candidate=True,
            escalations=3,
            max_escalations=3,
        )
    )
    assert d.action == "abort"
    assert d.stop_reason == ABORT_STOP_REASON


def test_signature_loop_prima_volta_guida():
    """Loop su tool identico, mai guidato -> forza-azione con nudge mirato."""
    d = decide(ProgressSignals(signature_loop_tool="read_file"))
    assert d.action == "guide"
    assert d.axis == "signature"
    assert d.force_action is True
    assert d.nudge_text and "read_file" in d.nudge_text


def test_g1_descriptive_prima_volta_guida():
    """Risposta descrittiva su richiesta d'azione, mai guidata -> forza-azione."""
    d = decide(ProgressSignals(g1_over_cap=True))
    assert d.action == "guide"
    assert d.axis == "g1_descriptive"
    assert d.force_action is True


def test_priorita_esplorazione_su_signature():
    """Se due assi sono in stallo, l'esplorazione ha priorita' (ordine del report)."""
    d = decide(
        ProgressSignals(
            exploration_count=12,
            exploration_threshold=6,
            signature_loop_tool="grep",
        )
    )
    assert d.axis == "exploration"


def test_soglia_zero_non_divide_per_zero():
    """exploration_threshold=0 non causa errori (clamp a 1 nel confronto)."""
    d = decide(ProgressSignals(exploration_count=2, exploration_threshold=0))
    # 2 >= 2*max(1,0)=2 -> stallo esplorazione, guida.
    assert isinstance(d, ProgressDecision)
    assert d.action == "guide"


# ── Asse resource_reallocation (loop request_port) ──────────────────────────

def test_reallocation_sotto_soglia_proceed():
    """Una sola request_port (count=1, soglia=3) non e' un loop -> proceed.

    Una richiesta porta legittima per un servizio nuovo non deve scattare.
    """
    d = decide(ProgressSignals(reallocation_count=1, reallocation_threshold=3))
    assert d.action == "proceed"
    assert d.axis is None
    assert d.nudge_text is None
    assert d.stop_reason is None


def test_reallocation_sopra_soglia_prima_volta_guida_grounded():
    """N request_port ravvicinate (oltre soglia), mai guidato -> GUIDE con nudge
    GROUNDED che ordina il riuso, senza forzare una nuova tool call."""
    d = decide(
        ProgressSignals(
            reallocation_count=4,
            reallocation_threshold=3,
            has_active_resources=True,
        )
    )
    assert d.action == "guide"
    assert d.axis == "resource_reallocation"
    # NON forza una nuova tool call (rischierebbe un ennesimo request_port).
    assert d.force_action is False
    assert d.stop_reason is None
    # Grounding: il nudge ordina il riuso/riavvio dei servizi attivi, non
    # l'allocazione. La direzione (riusa/riavvia) e' segnalata senza prescrivere
    # il tool specifico (principio "segnala, non prescrivere").
    assert d.nudge_text is not None
    _txt = d.nudge_text.lower()
    assert "riusa" in _txt
    assert "riavvia" in _txt
    assert "non riallocare" in _txt or "non allocarne" in _txt
    assert "richiesto porte" in _txt or "request_port" in _txt


def test_reallocation_gia_guidata_senza_escalation_aborta_verso_verifica():
    """Gia' guidato e ancora in loop, nessun candidato escalation -> ABORT con lo
    stop_reason coordinato (final_gate), non chiusura morta."""
    d = decide(
        ProgressSignals(
            reallocation_count=5,
            reallocation_threshold=3,
            already_guided=frozenset({"resource_reallocation"}),
            has_escalation_candidate=False,
        )
    )
    assert d.action == "abort"
    assert d.axis == "resource_reallocation"
    assert d.stop_reason == ABORT_STOP_REASON


def test_reallocation_gia_guidata_con_candidato_escala():
    """Gia' guidato, c'e' un candidato e budget -> ESCALATE prima dell'abort."""
    d = decide(
        ProgressSignals(
            reallocation_count=5,
            reallocation_threshold=3,
            already_guided=frozenset({"resource_reallocation"}),
            has_escalation_candidate=True,
            escalations=0,
            max_escalations=3,
        )
    )
    assert d.action == "escalate"
    assert d.axis == "resource_reallocation"
    assert d.stop_reason is None


def test_reallocation_priorita_su_repeated_action():
    """Se sono in stallo sia resource_reallocation sia repeated_action, vince il
    primo: il loop request_port e' specifico e va intercettato prima del generico."""
    d = decide(
        ProgressSignals(
            reallocation_count=4,
            reallocation_threshold=3,
            repeated_action=("write_file: x", 3),
        )
    )
    assert d.axis == "resource_reallocation"


def test_reallocation_nudge_grounded_anche_senza_active_resources():
    """Anche senza has_active_resources il nudge resta grounded sul riuso: il
    blocco RISORSE PROGETTO e' la fonte, il nudge non deve riproporre l'allocazione
    come default. has_active_resources modula i nudge esplorativi, non questo."""
    d = decide(ProgressSignals(reallocation_count=3, reallocation_threshold=3))
    assert d.action == "guide"
    assert d.axis == "resource_reallocation"
    assert d.nudge_text is not None
    _txt = d.nudge_text.lower()
    assert "riusa" in _txt
    assert "riavvia" in _txt


# ── Asse repeated_action: nudge build/test-aware ────────────────────────────
# Incidente "qualita': final_gate vede 20 errori TS" / loop "npm run build"
# ripetuto: ri-eseguire un build NON riduce gli errori, li riduce solo
# correggere i file. Il nudge GUIDE per repeated_action su un label di build
# deve segnalare la correzione dei file (non ripetere il comando) leggendo
# l'output completo, NON il generico "cambia approccio/comando diverso".


def test_is_build_or_test_label_riconosce_i_build():
    """Detection sul label (comando), non sull'output."""
    assert _is_build_or_test_label("run_command: npm run build")
    assert _is_build_or_test_label("run_command: cargo check --workspace")
    assert _is_build_or_test_label("run_command: tsc --noEmit")
    assert _is_build_or_test_label("run_command: pnpm verify")
    assert _is_build_or_test_label("run_tests: pytest brain/tests")
    assert _is_build_or_test_label("run_command: npx eslint .")


def test_is_build_or_test_label_ignora_non_build():
    """Un edit_file o un comando non-build non e' build/test."""
    assert not _is_build_or_test_label("edit_file: src/app.ts")
    assert not _is_build_or_test_label("run_command: ls -la")
    assert not _is_build_or_test_label("write_file: config.json")


def test_repeated_action_build_guida_ordina_correzione_batch():
    """repeated_action su un build, mai guidato -> GUIDE con nudge build-aware.

    Il nudge deve segnalare la causa (ri-eseguire non riduce gli errori, vanno
    corretti i file) senza prescrivere i tool specifici; NON deve forzare una nuova
    tool call (force_action False per repeated_action) ne' suggerire un "comando
    diverso".
    """
    d = decide(ProgressSignals(repeated_action=("run_command: npm run build", 3)))
    assert d.action == "guide"
    assert d.axis == "repeated_action"
    assert d.force_action is False  # NON forza una nuova tool call (= un altro build)
    assert d.nudge_text is not None
    _txt = d.nudge_text.lower()
    # Segnala la causa: ri-eseguire non riduce gli errori, vanno corretti i file.
    assert "non riduce gli errori" in _txt
    assert "corregg" in _txt
    assert "file" in _txt
    # Non deve cadere nel testo generico "cambia approccio".
    assert "cambia approccio" not in _txt


def test_repeated_action_non_build_resta_generico():
    """repeated_action su un edit (non build) mantiene il nudge generico."""
    d = decide(ProgressSignals(repeated_action=("edit_file: src/app.ts", 3)))
    assert d.action == "guide"
    assert d.axis == "repeated_action"
    assert d.nudge_text is not None
    _txt = d.nudge_text.lower()
    # Il generico parla di "cambia approccio"; non deve ordinare il batch build.
    assert "cambia approccio" in _txt
    assert "correzione batch" not in _txt


def test_force_diagnose_build_aware_ordina_fix_non_altro_comando():
    """Stadio FORCE_DIAGNOSE su un build gia' guidato: ordina la correzione dei
    file (non un comando diverso) e ammette la dichiarazione di blocco.

    force_diagnose scatta solo con flag abilitato, gia' guidato e non ancora
    diagnosticato (vedi gerarchia decide)."""
    d = decide(
        ProgressSignals(
            repeated_action=("run_command: cargo build", 4),
            already_guided=frozenset({"repeated_action"}),
            force_diagnose_enabled=True,
        )
    )
    assert d.action == "force_diagnose"
    assert d.axis == "repeated_action"
    assert d.force_action is False
    assert d.nudge_text is not None
    _txt = d.nudge_text.lower()
    assert "corregg" in _txt
    assert "causa radice" in _txt
    # Per un build, "ri-eseguire non e' un'azione diversa".
    assert "non e' un'azione diversa" in _txt


def test_force_diagnose_non_build_resta_generico():
    """FORCE_DIAGNOSE su un'azione non-build mantiene il testo generico."""
    d = decide(
        ProgressSignals(
            repeated_action=("write_file: data.json", 4),
            already_guided=frozenset({"repeated_action"}),
            force_diagnose_enabled=True,
        )
    )
    assert d.action == "force_diagnose"
    assert d.nudge_text is not None
    _txt = d.nudge_text.lower()
    assert "edit_file" not in _txt  # il generico non nomina edit_file in batch
    assert "causa radice" in _txt

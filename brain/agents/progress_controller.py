"""progress_controller: punto unico (regola L) del controllo di avanzamento del
ciclo agentico.

Contesto e causa radice
-----------------------
L'executor implementava N meccanismi anti-loop indipendenti (G1 reroute,
esplorazione, comando ripetuto, signature-loop, forced-text, cap iterazioni),
ognuno con i propri contatori e una propria reazione DISOMOGENEA: alcuni
iniettavano un nudge, altri ABORTIVANO subito (`stop_reason=loop_detected` /
`g1_cap_reached`). Inoltre `route_after_executor` instradava gli abort dritti al
`learner`, SCAVALCANDO il `final_gate` di verifica E2E gia' esistente. Risultato
osservato dal vivo: l'agente o esplorava all'infinito e veniva ABORTITO senza mai
essere costretto ad agire, o chiudeva "fatto" senza che il flusso reale fosse
provato.

Questo modulo centralizza UNA sola domanda: "di fronte a uno stallo, qual e' la
prossima mossa?". La risposta segue SEMPRE la stessa gerarchia, identica per ogni
asse di stallo (regola L):

    1. GUIDE     -> forza-azione guidata: rimuovi i tool di sola lettura e
                    obbliga una tool call produttiva (tool_choice required) +
                    nudge assertivo. E' il livello che mancava all'esplorazione.
    2. ESCALATE  -> promuovi il turno a un modello piu' capace (budget unico).
    3. ABORT     -> solo dopo aver esaurito guida+escalation. E l'abort NON
                    chiude piu' "morto": instrada alla verifica E2E (final_gate),
                    cosi' un eventuale risultato parziale viene comunque
                    validato e, se rotto, l'agente e' rimandato a correggerlo.

La funzione `decide` e' PURA (nessun IO, nessuna lettura DB): i parametri arrivano
gia' risolti dal chiamante, come `should_force_tool_choice`. Cosi' resta
deterministica e testabile in isolamento (pytest, senza DB ne' LLM).
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Literal

# Assi di stallo riconosciuti. Stringhe stabili: usate come chiavi negli insiemi
# di stato ("assi gia' guidati") e nei log/meta_step.
Axis = Literal[
    "exploration", "signature", "g1_descriptive", "repeated_action",
    "resource_reallocation",
]

# Azioni possibili, in ordine di severita' crescente.
# force_diagnose: stadio intermedio per l'asse repeated_action (dopo GUIDE, prima
# di ESCALATE/ABORT). Obbliga l'agente a leggere l'errore, dichiarare la causa
# radice e cambiare azione, cosi' un eventuale esito FAILED porta sempre una
# diagnosi (mai una chiusura grezza). Vedi mig 0386.
Action = Literal["proceed", "guide", "force_diagnose", "escalate", "abort"]

# stop_reason UNICO emesso da un abort coordinato. route_after_executor lo
# instrada alla verifica E2E (final_gate) per i task software, non al learner
# morto. Tenuto distinto dai legacy "loop_detected"/"g1_cap_reached" per non
# rompere i consumatori downstream gia' esistenti durante la convergenza.
ABORT_STOP_REASON = "loop_abort"


@dataclass(frozen=True)
class ProgressSignals:
    """Segnali grezzi del turno corrente, raccolti dall'executor.

    Tutti opzionali con default neutri: il chiamante popola solo quelli che
    conosce nel punto in cui chiama (pre-LLM vs post-LLM).
    """

    # Esplorazione: chiamate consecutive di sola lettura e relativa soglia.
    exploration_count: int = 0
    exploration_threshold: int = 6
    # Signature-loop: nome del tool ripetuto identico (None = nessun loop).
    signature_loop_tool: str | None = None
    # G1 descrittivo: il modello descrive senza agire (reroute_count >= max).
    g1_over_cap: bool = False
    # Azione produttiva (scrittura/comando) ripetuta identica oltre soglia:
    # (label, conteggio) oppure None. Indipendente dall'esito (anche se riesce).
    repeated_action: tuple[str, int] | None = None
    # Riallocazione risorse: numero di chiamate request_port ravvicinate, contate
    # A PRESCINDERE dal label (il variare del label E' il sintomo del loop). Sopra
    # soglia indica che l'agente continua ad allocare porte invece di riusare i
    # servizi gia' attivi del progetto. Segnale STRUTTURALE, non lessicale.
    reallocation_count: int = 0
    reallocation_threshold: int = 3
    # Indica se il run ha gia' allocazioni/servizi attivi noti (porte governate o
    # servizi in ascolto). Quando True il nudge di riallocazione e' GROUNDED: punta
    # esplicitamente al riuso/riavvio invece di lasciare aperta l'opzione "alloca".
    has_active_resources: bool = False
    # Budget di escalation gia' consumato e candidato disponibile.
    escalations: int = 0
    max_escalations: int = 3
    has_escalation_candidate: bool = False
    # Assi per i quali la forza-azione (GUIDE) e' GIA' stata applicata in un
    # turno precedente di questo run: per loro la prossima mossa sale di livello.
    already_guided: frozenset[str] = field(default_factory=frozenset)
    # Assi per i quali la DIAGNOSI FORZATA (force_diagnose) e' GIA' stata applicata:
    # per loro la prossima mossa dopo lo stallo sale a escalate/abort.
    already_diagnosed: frozenset[str] = field(default_factory=frozenset)
    # Se True abilita lo stadio intermedio force_diagnose per l'asse
    # repeated_action (setting agent.repeated_action_force_diagnose_enabled).
    force_diagnose_enabled: bool = False


@dataclass(frozen=True)
class ProgressDecision:
    """Esito della decisione. Il chiamante lo APPLICA (inietta nudge, rimuove
    tool, escala, fa il return di abort); la LOGICA vive qui.
    """

    action: Action
    axis: Axis | None = None
    # GUIDE: rimuovi i tool di sola lettura e obbliga tool_choice required.
    force_action: bool = False
    # Testo del nudge assertivo da iniettare (None se non serve).
    nudge_text: str | None = None
    # Solo per ABORT: lo stop_reason coordinato (instradato a final_gate).
    stop_reason: str | None = None
    # Spiegazione breve per log/meta_step (perche' questa mossa).
    reason: str = ""


def _port_directive(has_active_resources: bool) -> str:
    """Direttiva porte CONDIZIONALE (grounding).

    Quando il run ha gia' risorse attive (porte governate / servizi in ascolto)
    NON suggerisce request_port come azione produttiva di default: indirizza al
    riuso/riavvio dei servizi esistenti. Cosi' i nudge che oggi propongono sempre
    request_port (esplorazione, anti-esplorazione) non spingono verso un nuovo
    loop di allocazione. Quando non risulta nulla di attivo, request_port resta la
    via corretta per un servizio NUOVO.
    """
    if has_active_resources:
        return (
            "per le porte NON allocarne di nuove: i servizi del progetto sono gia' "
            "attivi (vedi blocco RISORSE PROGETTO), usa list_active_services e "
            "riusa/riavvia con service_restart, oppure punta i tool alle porte gia' "
            "allocate"
        )
    return "request_port SOLO per un servizio NUOVO (non ancora in ascolto)"


def _exploration_nudge(count: int, has_active_resources: bool = False) -> str:
    """Nudge ASSERTIVO per l'esplorazione (piu' forte del nudge soft a 1x soglia).

    Non chiede gentilmente: ordina di agire ORA, perche' i tool di sola lettura
    sono stati rimossi e una tool call produttiva e' obbligata. La direttiva sulle
    porte e' CONDIZIONALE: se il run ha gia' risorse attive non propone
    request_port (eviterebbe di alimentare il loop di riallocazione).
    """
    return (
        f"STOP esplorazione: hai gia' letto/cercato {count} volte di fila senza "
        "produrre nulla. I tool di sola lettura sono ora DISABILITATI per questo "
        "turno. DEVI agire ORA con una tool call produttiva: se devi modificare il "
        f"progetto usa write_file/edit_file ({_port_directive(has_active_resources)}) "
        "oppure run_command per eseguire/verificare; se invece la richiesta era una "
        "domanda, RISPONDI subito a parole con il risultato. Niente altre letture."
    )


def _resource_reallocation_nudge(count: int) -> str:
    """Nudge GROUNDED per il loop di riallocazione porte (asse resource_reallocation).

    Il sintomo (diagnosi confermata): l'agente chiama request_port ripetutamente
    variando il label, mentre i servizi del progetto sono GIA' attivi. Il loop
    sfugge a signature/repeated_action proprio perche' il label cambia. Qui non si
    forza una nuova tool call (rischierebbe un ennesimo request_port): si ORDINA il
    riuso, ancorato alle risorse reali gia' presenti nel contesto.
    """
    return (
        f"STOP: hai gia' richiesto porte {count} volte di fila. I servizi del "
        "progetto sono gia' attivi: NON riallocare e NON variare il label per "
        "ottenere una porta nuova (request_port e' idempotente per scopo e ti "
        "ridarebbe comunque la porta esistente). Usa il blocco RISORSE PROGETTO nel "
        "contesto: se il servizio del tuo scopo e' ATTIVO RIUSA la sua porta "
        "(punta i tool/le richieste a quella); se e' allocato ma spento, "
        "RIAVVIALO con service_restart; verifica lo stato reale con "
        "list_active_services. Chiama request_port SOLO per un servizio NUOVO che "
        "non e' ancora in ascolto."
    )


def _signature_nudge(tool: str) -> str:
    """Nudge per il loop su tool identico ripetuto."""
    return (
        f"STOP: hai ripetuto la stessa tool call ('{tool}', stesso input) senza "
        "progresso. NON ripeterla. Cambia strategia ORA: se ti mancano "
        "informazioni fai UNA richiesta diversa e piu' specifica, altrimenti "
        "procedi con l'azione concreta successiva (write_file/edit_file/"
        "run_command) o riassumi lo stato a parole."
    )


def _g1_nudge() -> str:
    """Nudge per la risposta descrittiva su richiesta d'azione (G1)."""
    return (
        "STOP: hai descritto i passi senza eseguirli. NON descrivere: ESEGUI ORA "
        "il prossimo step concreto con una tool call (write_file/edit_file/"
        "run_command). I tool di sola lettura sono disabilitati per questo turno."
    )


# Vocabolario build/test per il nudge build-aware. Stesso ordine-di-grandezza
# del ramo build di classify_command_error (mcp-core helpers.rs), ma qui in
# Python e applicato al LABEL del comando ripetuto ("name: valore"), non
# all'output. Resta lessicale ma e' solo un NUDGE guidato (testo del sollecito),
# non una decisione di flusso: le decisioni strutturali restano in `decide`.
_BUILD_TEST_LABEL_KEYWORDS: tuple[str, ...] = (
    "build", "tsc", "compile", "cargo check", "cargo build", "cargo test",
    "npm run", "npm test", "pnpm", "yarn", "lint", "eslint", "make",
    "pytest", "run_tests", "test", "gradle", "mvn", "go build", "go test",
)


def _is_build_or_test_label(label: str) -> bool:
    """True se il label del comando ripetuto e' un build/compilazione/test.

    Un build/test ripetuto NON va attaccato con un "comando diverso": va
    attaccato leggendo gli errori RESIDUI e correggendo i file. Il rilevamento
    e' sul comando (label "run_command: npm run build"), non sull'output.
    """
    _l = label.lower()
    return any(k in _l for k in _BUILD_TEST_LABEL_KEYWORDS)


def _repeated_action_nudge(label: str, count: int) -> str:
    """Nudge per la ripetizione identica di una azione produttiva.

    Caso reale (run f1db9550): stessa sequenza edit_file -> npm install ->
    npm run build ripetuta integralmente. Ordina di NON ripeterla e di
    procedere/concludere verificando l'esito.

    Variante BUILD/TEST-AWARE: se il label e' un build/compilazione/test
    (incidente "qualita': final_gate vede 20 errori TS"), ri-eseguire il
    comando NON riduce gli errori — li riduce solo correggere i file. Il
    nudge generico ("cambia approccio") puo' essere letto come "usa un comando
    diverso" (stesso difetto del classify generico): per i build serve invece
    l'ordine esplicito di leggere TUTTO l'output e correggere i file in batch
    PRIMA di ri-eseguire una sola volta.
    """
    if _is_build_or_test_label(label):
        return (
            f"STOP: hai gia' eseguito '{label}' {count} volte. Ri-eseguire un "
            "build/test NON riduce gli errori — li riduce solo correggere i "
            "file. NON ripetere il comando. Invece, in quest'ordine: (1) leggi "
            "l'output COMPLETO dell'ultima esecuzione qui sopra: ogni errore ha "
            "file:riga e in fondo c'e' il totale (es. 'Found N errors'); (2) apri "
            "con read_file OGNI file segnalato e correggilo con edit_file, TUTTI "
            "in questo turno (correzione batch, non uno solo); (3) SOLO DOPO aver "
            "corretto tutti i file ri-esegui il comando UNA volta per confermare. "
            "Se l'output era troncato e non vedi tutti gli errori, correggi quelli "
            "visibili e segnala esplicitamente che ne mancano: non ri-eseguire per "
            "scoprirli."
        )
    return (
        f"STOP: hai gia' eseguito la stessa azione ({label}) {count} volte. "
        "Ripeterla identica NON cambia il risultato. NON ripeterla: leggi "
        "l'esito dell'esecuzione precedente, e poi (a) se l'azione e' riuscita, "
        "PROCEDI al passo successivo o concludi verificando il risultato reale; "
        "(b) se e' fallita, cambia approccio (causa radice diversa), non rieseguire "
        "lo stesso comando/edit."
    )


def _force_diagnose_nudge(label: str, count: int) -> str:
    """Nudge di DIAGNOSI FORZATA: lo stadio tra GUIDE e ABORT per l'azione ripetuta.

    Il nudge soft (GUIDE) non ha cambiato nulla: l'agente ha ri-ripetuto. Qui non
    si chiude ancora: si OBBLIGA a capire perche' l'azione fallisce e a cambiare
    strategia, oppure a dichiararsi bloccato con una causa precisa. Cosi' l'esito
    successivo e' una diagnosi (FailedDiagnosed/BlockedNeedsInput), mai una
    chiusura grezza "ho ripetuto N volte".

    Variante BUILD/TEST-AWARE: per un build/test ripetuto "azione diversa" NON
    significa "comando diverso" ma "correggi i file segnalati dagli errori".
    Lo stadio intermedio lo rende esplicito perche' la GUIDE soft non ha morso.
    """
    if _is_build_or_test_label(label):
        return (
            f"STOP: hai ripetuto '{label}' {count} volte e il sollecito precedente "
            "non ha cambiato nulla. PRIMA di qualunque altra mossa DEVI, in "
            "quest'ordine: (1) leggere l'output COMPLETO dell'ultima esecuzione "
            "(ogni errore ha file:riga, in fondo il totale tipo 'Found N errors'); "
            "(2) dichiarare in una frase la CAUSA RADICE (es. tipo mancante, import "
            "errato, simbolo non definito), non il sintomo; (3) correggere con "
            "edit_file OGNI file segnalato, in questo turno (correzione batch). "
            "Ri-eseguire il build NON e' un'azione diversa: e' la stessa di prima. "
            "Se non riesci a correggere (errore in dipendenza esterna/codice "
            "generato non tuo), dichiara ESPLICITAMENTE che sei bloccato e perche': "
            "il turno chiudera' con la diagnosi, non con un'altra esecuzione."
        )
    return (
        f"STOP: hai ripetuto '{label}' {count} volte e il sollecito precedente non "
        "ha cambiato nulla. PRIMA di qualunque altra mossa DEVI, in quest'ordine: "
        "(1) leggere l'output/errore ESATTO dell'ultima esecuzione; (2) dichiarare "
        "in una frase la CAUSA RADICE del fallimento (non il sintomo); (3) eseguire "
        "UN'AZIONE DIVERSA che attacchi quella causa (comando o edit diverso), NON "
        "la stessa di prima. Se non esiste un'azione diversa praticabile, dichiara "
        "ESPLICITAMENTE che sei bloccato e perche' (es. dipendenza/credenziale/"
        "permesso/servizio mancante): il turno chiudera' con la diagnosi e il "
        "prossimo passo, non con una ripetizione."
    )


def decide(signals: ProgressSignals) -> ProgressDecision:
    """Punto unico: data la fotografia del progresso, decide la prossima mossa.

    Gerarchia, applicata all'asse di stallo a priorita' piu' alta:
        - se l'asse NON e' ancora stato guidato (forza-azione) -> GUIDE
        - altrimenti, se c'e' un candidato di escalation nel budget -> ESCALATE
        - altrimenti -> ABORT (verso la verifica E2E)

    Priorita' tra assi (dal report di analisi): esplorazione, signature-loop,
    resource_reallocation (loop request_port), repeated_action, g1-descrittivo.

    resource_reallocation: il loop di richieste porte (request_port ripetuto con
    label che varia) sfugge a signature/repeated_action proprio perche' il label
    cambia. Il suo segnale e' STRUTTURALE (conteggio request_port ravvicinate). La
    GUIDE emette un nudge GROUNDED che ordina il riuso/riavvio dei servizi gia'
    attivi (no force_action: non si forza un'altra tool call). Se persiste sale a
    ESCALATE/ABORT (verso final_gate) come gli altri assi.

    Nessuno stallo -> proceed.
    """
    # Determina l'asse di stallo prioritario (None = nessuno stallo bloccante).
    # resource_reallocation sta tra signature e repeated_action: e' un loop
    # strutturale specifico (request_port ripetuto) che va intercettato prima del
    # generico repeated_action (request_port NON e' tra i tool di repeated_action).
    axis: Axis | None = None
    if signals.exploration_count >= 2 * max(1, signals.exploration_threshold):
        axis = "exploration"
    elif signals.signature_loop_tool:
        axis = "signature"
    elif signals.reallocation_count >= max(1, signals.reallocation_threshold):
        axis = "resource_reallocation"
    elif signals.repeated_action is not None:
        axis = "repeated_action"
    elif signals.g1_over_cap:
        axis = "g1_descriptive"

    if axis is None:
        return ProgressDecision(action="proceed", reason="nessuno stallo bloccante")

    already = axis in signals.already_guided
    already_diagnosed = axis in signals.already_diagnosed
    can_escalate = (
        signals.has_escalation_candidate
        and signals.escalations < signals.max_escalations
    )

    # Livello 1 — GUIDE (forza-azione): solo se non gia' tentato per questo asse.
    if not already:
        if axis == "exploration":
            nudge = _exploration_nudge(
                signals.exploration_count, signals.has_active_resources
            )
        elif axis == "signature":
            nudge = _signature_nudge(signals.signature_loop_tool or "")
        elif axis == "resource_reallocation":
            nudge = _resource_reallocation_nudge(signals.reallocation_count)
        elif axis == "repeated_action":
            _ra_label, _ra_count = signals.repeated_action or ("", 0)
            nudge = _repeated_action_nudge(_ra_label, _ra_count)
        else:
            nudge = _g1_nudge()
        # Per repeated_action e resource_reallocation NON forziamo una nuova tool
        # call: nel primo caso rischierebbe di rieseguire la stessa azione, nel
        # secondo un ennesimo request_port. Il nudge ordina di riusare/procedere.
        # Per gli altri assi la forza-azione rimuove i read-only.
        _no_force_axes = {"repeated_action", "resource_reallocation"}
        _force = axis not in _no_force_axes
        if axis == "repeated_action":
            _reason = f"stallo {axis}: nudge anti-ripetizione (procedi/verifica)"
        elif axis == "resource_reallocation":
            _reason = f"stallo {axis}: nudge riusa-porte (no nuova allocazione)"
        else:
            _reason = f"stallo {axis}: forza-azione (rimuovo read-only + tool_choice required)"
        return ProgressDecision(
            action="guide",
            axis=axis,
            force_action=_force,
            nudge_text=nudge,
            reason=_reason,
        )

    # Livello 1.5 — FORCE_DIAGNOSE: solo per l'azione ripetuta, dopo che la GUIDE
    # soft non ha cambiato nulla e PRIMA di escalation/abort. Obbliga la diagnosi
    # del fallimento e un cambio di azione (o la dichiarazione esplicita di blocco),
    # cosi' l'esito porta sempre una diagnosi. Abilitato da flag DB-driven.
    if (
        axis == "repeated_action"
        and signals.force_diagnose_enabled
        and not already_diagnosed
    ):
        _ra_label, _ra_count = signals.repeated_action or ("", 0)
        return ProgressDecision(
            action="force_diagnose",
            axis=axis,
            force_action=False,
            nudge_text=_force_diagnose_nudge(_ra_label, _ra_count),
            reason="stallo repeated_action: diagnosi forzata prima di escalation/abort",
        )

    # Livello 2 — ESCALATE: gia' guidato ma ancora bloccato, c'e' budget.
    if can_escalate:
        return ProgressDecision(
            action="escalate",
            axis=axis,
            reason=f"stallo {axis} persiste dopo forza-azione: escalation modello "
            f"({signals.escalations + 1}/{signals.max_escalations})",
        )

    # Livello 3 — ABORT verso verifica: guida+escalation esaurite.
    return ProgressDecision(
        action="abort",
        axis=axis,
        stop_reason=ABORT_STOP_REASON,
        reason=f"stallo {axis}: guida ed escalation esaurite -> chiusura con "
        "verifica E2E (final_gate)",
    )

//! Nodo REVIEW GATE: la review adversariale entra nel funnel di chiusura.
//!
//! Prima la review era post-processing del finalizzatore mcp-core: girava DOPO
//! che il grafo aveva raggiunto `End`, quindi su bocciatura poteva solo mutare
//! l'esito in memoria (nota nel resoconto + `review_panel_rejected`) senza
//! alcuna possibilita' di correzione — il resume di un run a `End` e' un no-op
//! per costruzione (`engine.rs`: `current == End -> Completed`). L'utente lo ha
//! chiesto due volte: "se non superata dovrebbe tentare di sistemare".
//!
//! Da nodo del grafo, la bocciatura usa ESATTAMENTE il meccanismo del ramo FAIL
//! del final_gate (regola L: stesso rientro, secondo chiamante): messaggio
//! Human col verdetto e i findings + `StopReason::ToolUse` +
//! `pending_tool_uses` azzerato, e l'edge condizionale rimanda all'Executor.
//! Il run non arriva mai a `End` con una bocciatura correggibile pendente.
//!
//! Il rimando e la chiusura sono DICHIARATI (`gate_routing`, regola M), mai
//! dedotti dallo `stop_reason`: quello lo scrive anche l'executor, e finche'
//! l'edge lo leggeva, i rami di chiusura di questo nodo (che non lo riscrivono)
//! venivano instradati come rimandi — il run rientrava nell'executor dopo una
//! review APPROVATA. Vedi `GateRouting` per la misura sul run 609000c1.
//!
//! Anti-loop: contatore DEDICATO `review_gate_cycle` (mai `final_gate_cycle`: il
//! residuo di un contatore altrui ha gia' prodotto un falso `FailedDiagnosed`,
//! vedi doc di `FinalGateVerdict`), cap `orchestrator.review_max_correction_cycles`
//! (DB-driven, regola G, risolto a monte). Al cap la bocciatura diventa
//! DEFINITIVA (`RejectedFinal`) e il run chiude bocciato — mai un loop.
//!
//! Il contatore conta i RIMANDI, non le convocazioni del panel: e' l'unica
//! grandezza commensurabile col cap, che limita i rimandi. Contando le
//! convocazioni, una bocciatura preceduta da N approvazioni trovava il contatore
//! gia' oltre il cap e chiudeva DEFINITIVA senza un solo tentativo di
//! correzione, e l'etichetta "(n/max)" mostrava un numeratore che il cap non
//! governava.
//!
//! ## Il contatore misura i tentativi, la porta misura il PROGRESSO (28/07/2026)
//!
//! Contare i rimandi non basta: un rimando in cui l'agente non modifica nulla ne
//! consumava uno esattamente come uno in cui correggeva. Misurato sul progetto
//! `gestione-spese`: rilievo corretto del panel su `vite.config.js`, tre volte la
//! risposta "Nessuna azione necessaria. Il task e' stato completato e verificato
//! nei turni precedenti", tre bocciature, ZERO file toccati, run chiuso al cap
//! con 1.243.417 token (1.178.170 in ingresso contro 6.138 in uscita) e i due
//! revisori convocati tre volte sullo stesso identico codice.
//!
//! Il gate valutava cio' che l'agente DICEVA; il fatto — ha modificato dei file?
//! — era gia' registrato in forma strutturata (`file_mutations`, hash del
//! contenuto) e nessuno lo leggeva. Ora il gate lo legge dalla porta
//! [`MutationProgressPort`] e ne trae due conseguenze:
//!
//!  1. **Non riconvoca il panel** dopo un rimando a vuoto: stesso codice, stesso
//!     verdetto, spesa certa e inutile (sul run osservato, 2 convocazioni su 3).
//!  2. **Il rimando smette di essere liquidabile**: il fatto misurato entra nel
//!     rimando successivo come contestazione opponibile ("nessun file e'
//!     cambiato"), invece di lasciare che "gia' fatto" valga come risposta.
//!  3. **Il rimando a vuoto e' un trigger di ESCALATION** (30/07/2026): un
//!     modello che riceve il verdetto e non tocca un file ha dimostrato di non
//!     farcela, e ridargli lo stesso rimando e' la definizione di aspettarsi un
//!     esito diverso ripetendo l'input. Misurato su bacheca-attivita (run
//!     e8433555, claude-haiku): il ciclo di correzione restava sul modello small
//!     fino al cap mentre il percorso principale aveva gia' la sua escalation
//!     (`FINAL_GATE_ESCALATION_KEY`) — il run del giorno prima aveva sciolto uno
//!     stallo proprio cambiando modello. Il gate posa
//!     [`REVIEW_GATE_ESCALATION_KEY`] sul rimando NON definitivo; l'executor lo
//!     consuma al rientro delegando al punto unico
//!     `maybe_escalate_nonconvergence` (regola L: il gate non ha la porta di
//!     escalation, l'executor si' — stesso paradigma del final_gate). A catena
//!     esaurita l'executor prosegue col modello corrente: il cap dei rimandi
//!     resta il backstop, il flag lo governa questo nodo (posato solo dal
//!     rimando a vuoto, rimosso da ogni esito successivo del panel).
//!
//! Il criterio di cosa sia progresso NON vive qui: e' il punto unico puro
//! [`crate::decisions::correction_progress`] (regola L), cosi' la stessa domanda
//! non riceve due risposte diverse dal nodo e dalla query.

use async_trait::async_trait;
use nexus_graph::node::{GraphNode, NodeError, NodeId};
use nexus_graph::StateDelta as OpaqueDelta;

use crate::decisions::{classify_correction_progress, CorrectionProgress, PanelOutcome};
use crate::runtime::ports::{
    MutationProgressPort, ReviewPanelReport, ReviewPanelRequest, ReviewSkipReason,
};
use crate::state::{
    AgentState, GateRouting, Message, MessageContent, ReviewGateVerdict, StopReason,
};
use crate::state::delta::StateDelta;
use crate::AgentNodeCtx;

/// Trigger STRUTTURATO di escalation (regola M): posato in `extra` quando un
/// rimando in correzione risulta a vuoto (`CorrectionProgress` non-Effettivo) e
/// il run rientra comunque in correzione. L'executor lo consuma al rientro
/// promuovendo il modello via `maybe_escalate_nonconvergence` (gemello di
/// `FINAL_GATE_ESCALATION_KEY`, regola L: stesso meccanismo, secondo produttore).
/// Nasce SOLO dalla misura sulle scritture registrate, mai dal testo con cui
/// l'agente ha liquidato il rimando.
pub const REVIEW_GATE_ESCALATION_KEY: &str = "review_gate_escalation_pending";

/// Config DB-driven del gate (regola G: risolta dal chiamante, mai letta qui).
#[derive(Debug, Clone)]
pub struct ReviewGateConfig {
    /// `orchestrator.review_panel_autoconvene_enabled` (default true). OFF ->
    /// pass-through.
    pub enabled: bool,
    /// `orchestrator.review_max_correction_cycles` (default 1): numero massimo
    /// di RIMANDI in correzione. I panel convocati sono al piu' N+1 (la
    /// ri-review dopo l'ultima correzione).
    pub max_cycles: i64,
}

/// Un rimando che il gate emette SENZA riconvocare il panel, nei numeri che lo
/// descrivono. Viaggiano come una cosa sola perche' si leggono insieme:
/// `cycle`/`max_cycles` dicono a che punto del cap siamo, `a_vuoto` quante volte
/// l'agente non ha prodotto nulla, `progresso` che cosa e' stato misurato — e il
/// verdetto nasce dal loro rapporto, non da uno di essi.
#[derive(Debug, Clone, Copy)]
struct RimandoAVuoto {
    /// Numero del tentativo mostrato all'agente e in UI.
    cycle: i64,
    max_cycles: i64,
    /// Rimandi che NON hanno prodotto modifiche, questo compreso.
    a_vuoto: i64,
    /// Il cap e' raggiunto: il run chiude invece di rimandare.
    definitiva: bool,
    progresso: CorrectionProgress,
}

impl RimandoAVuoto {
    fn nuovo(
        state: &AgentState,
        rimandi_fatti: i64,
        max_cycles: i64,
        progresso: CorrectionProgress,
    ) -> Self {
        let definitiva = rimandi_fatti >= max_cycles;
        Self {
            // Su una chiusura definitiva non nasce un nuovo rimando: il numero
            // resta quello dei rimandi gia' spesi (stessa regola di `boccia`).
            cycle: if definitiva {
                rimandi_fatti
            } else {
                rimandi_fatti + 1
            },
            max_cycles,
            a_vuoto: state.review_correction_no_progress.unwrap_or(0) + 1,
            definitiva,
            // `rimandi_fatti` non si conserva: serve solo per il confronto con
            // `a_vuoto`, che e' gia' risolto in `verdetto`.
            progresso,
        }
    }

    /// Verdetto della bocciatura raggiunta per questa via. E' LA decisione del
    /// ramo: `a_vuoto >= cycle` dice che OGNI tentativo si e' chiuso senza
    /// toccare un file.
    ///
    /// Quando la misura e' mancata per qualche giro il contatore sottostima e si
    /// cade su [`ReviewGateVerdict::RejectedFinal`], il verdetto storico e
    /// conservativo: meglio non accusare di inerzia un run che forse aveva
    /// tentato.
    fn verdetto(&self) -> ReviewGateVerdict {
        if !self.definitiva {
            ReviewGateVerdict::PendingCorrection
        } else if self.a_vuoto >= self.cycle {
            ReviewGateVerdict::RejectedNoCorrection
        } else {
            ReviewGateVerdict::RejectedFinal
        }
    }
}

pub struct ReviewGateNode {
    cfg: ReviewGateConfig,
    /// Porta del panel (mcp-core convoca i sub-run revisori).
    panel: std::sync::Arc<dyn crate::runtime::ports::ReviewPanelPort>,
    /// Narrazione live (pattern emit+persist, punto unico `emit_phase_meta`).
    meta_steps: std::sync::Arc<dyn crate::runtime::ports::MetaStepStore>,
    /// Misura del progresso fra un rimando e il successivo. `None` = misura non
    /// disponibile (grafi di scaffold, fixture topologiche): il gate ricade sul
    /// comportamento storico e convoca sempre. Mai un default che finge di aver
    /// misurato: "non lo so" e "non e' cambiato niente" portano a decisioni
    /// opposte.
    mutations: Option<std::sync::Arc<dyn MutationProgressPort>>,
}

impl ReviewGateNode {
    /// Costruisce il nodo con la config DB-driven gia' risolta dal chiamante.
    pub fn new(
        cfg: ReviewGateConfig,
        panel: std::sync::Arc<dyn crate::runtime::ports::ReviewPanelPort>,
        meta_steps: std::sync::Arc<dyn crate::runtime::ports::MetaStepStore>,
    ) -> Self {
        Self {
            cfg,
            panel,
            meta_steps,
            mutations: None,
        }
    }

    /// Innesta la misura del progresso (mcp-core la implementa su
    /// `file_mutations`). Senza, il gate si comporta come prima del 28/07/2026.
    pub fn with_mutation_progress(
        mut self,
        port: std::sync::Arc<dyn MutationProgressPort>,
    ) -> Self {
        self.mutations = Some(port);
        self
    }

    /// Emissione narrativa del gate (punto unico del kind e del payload:
    /// i letterali "review_gate"/"verdict" vivono solo qui).
    async fn emit(
        &self,
        ctx: &AgentNodeCtx,
        title: String,
        mut payload: serde_json::Map<String, serde_json::Value>,
        verdict: Option<&str>,
    ) {
        if let Some(v) = verdict {
            payload.insert("verdict".to_string(), serde_json::Value::String(v.to_string()));
        }
        crate::nodes::emit_phase_meta(
            ctx.emit.as_ref(),
            self.meta_steps.as_ref(),
            "review_gate",
            title,
            serde_json::Value::Object(payload),
        )
        .await;
    }

    /// Uscita senza giudizio (gate spento, run gia' bocciato in via definitiva):
    /// il run PROSEGUE verso la chiusura. Dichiara `Chiude` (regola M): senza,
    /// l'edge ereditava lo `stop_reason` dell'executor e rispediva indietro un
    /// run che il gate non aveva nemmeno esaminato.
    fn pass_through() -> OpaqueDelta {
        StateDelta {
            gate_routing: Some(Some(GateRouting::Chiude)),
            ..Default::default()
        }
        .into_opaque()
    }

    /// Delta di solo verdetto (nessun rimando): il run prosegue verso la
    /// chiusura, l'esito strutturato resta leggibile dal finalizzatore.
    fn verdict_delta(cycle: Option<i64>, verdict: ReviewGateVerdict) -> OpaqueDelta {
        StateDelta {
            review_gate_cycle: cycle.map(Some),
            review_gate_verdict: Some(Some(verdict)),
            gate_routing: Some(Some(GateRouting::Chiude)),
            ..Default::default()
        }
        .into_opaque()
    }

    /// Salva l'esito del panel nello stato (`extra.review_panel_last`) per il
    /// titolo onesto del resoconto lato finalizzatore. `put_extra` (punto
    /// unico): il delta `extra` ha semantica overwrite TOTALE, una mappa
    /// parziale cancellerebbe le altre chiavi dello schema aperto.
    fn extra_with_panel(
        state: &AgentState,
        panel: &PanelOutcome,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut extra = crate::state::delta::put_extra(state, "review_panel_last", panel.to_value());
        // Un nuovo esito del panel SOSTITUISCE il segnale del rimando a vuoto
        // precedente: se il trigger di escalation e' rimasto pendente (catena
        // esaurita al rientro), non deve sopravvivere a un giro in cui il codice
        // e' cambiato e il panel ha giudicato di nuovo — l'escalation che ne
        // nascesse dichiarerebbe un fatto ("rimando senza correzioni") non piu'
        // vero (regola M).
        extra.remove(REVIEW_GATE_ESCALATION_KEY);
        extra
    }

    /// Elenco dei difetti, PUNTO UNICO del formato: lo usano sia il rimando dopo
    /// una convocazione sia quello che non riconvoca. Con due rendering, il
    /// secondo rimando avrebbe descritto gli stessi difetti in un formato diverso
    /// e l'agente avrebbe avuto motivo di leggerli come rilievi nuovi.
    fn render_findings(findings: &[serde_json::Value]) -> Vec<String> {
        let mut lines: Vec<String> = findings
            .iter()
            .take(12)
            .map(|f| {
                let file = f.get("file").and_then(|v| v.as_str()).unwrap_or("?");
                let severity = f.get("severity").and_then(|v| v.as_str()).unwrap_or("?");
                let desc = f
                    .get("description")
                    .or_else(|| f.get("evidence"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                format!("- [{severity}] {file}: {desc}")
            })
            .collect();
        if findings.len() > 12 {
            lines.push(format!("- (altri {} findings omessi)", findings.len() - 12));
        }
        lines
    }

    /// Blocco di correzione iniettato come messaggio Human (gemello del
    /// `render_failed_block` del final_gate): verdetto + findings con file ed
    /// evidenza, e la consegna esplicita di correggere e ridichiarare.
    fn render_correction_block(panel: &PanelOutcome, cycle: i64, max_cycles: i64) -> String {
        let mut lines = vec![format!(
            "## Review adversariale NON superata (tentativo {cycle}/{max_cycles})\n\n\
             Un panel di revisori indipendenti ha esaminato le modifiche di questo run \
             e ha emesso verdetto '{}' ({} voti validi su {}).\n\nDifetti rilevati:",
            panel.verdict.as_str(),
            panel.valid,
            panel.total_reviews
        )];
        lines.extend(Self::render_findings(&panel.findings));
        lines.push(
            "\nCORREGGI i difetti elencati usando i tool disponibili, poi dichiara di nuovo \
             la chiusura con task_complete. La review verra' ripetuta sulle modifiche."
                .to_string(),
        );
        lines.join("\n")
    }

    /// Blocco del rimando che NON ha riconvocato il panel: apre col FATTO
    /// misurato invece che col verdetto, perche' il verdetto l'agente lo ha gia'
    /// ricevuto e liquidato.
    ///
    /// E' la parte che rende il rimando non liquidabile. "Il task e' stato
    /// completato e verificato nei turni precedenti" e' una tesi sul passato;
    /// qui c'e' una misura sul presente che la contraddice, e con essa la
    /// richiesta di un esito verificabile: correggere, oppure dire QUALE rilievo
    /// e' infondato e perche'. Restano ammesse entrambe le uscite, ma nessuna
    /// delle due e' "ho gia' fatto".
    fn render_stalled_block(
        findings: &[serde_json::Value],
        progresso: CorrectionProgress,
        cycle: i64,
        max_cycles: i64,
    ) -> String {
        let fatto = progresso
            .fatto_opponibile()
            .unwrap_or_else(|| "nessuna modifica risulta registrata".to_string());
        let mut lines = vec![format!(
            "## Il rimando precedente non ha prodotto alcuna correzione \
             (tentativo {cycle}/{max_cycles})\n\n\
             Misura sulle scritture registrate di questo run: {fatto}. I difetti \
             segnalati dalla review sono quindi ancora tutti aperti, e il panel non e' \
             stato riconvocato: rivedrebbe codice identico.\n\n\
             Dichiarare che il lavoro era \"gia' fatto nei turni precedenti\" non chiude \
             questi rilievi: la misura dice che i file non sono cambiati.\n\n\
             Difetti ancora aperti:"
        )];
        lines.extend(Self::render_findings(findings));
        lines.push(
            "\nHai due uscite, entrambe verificabili: (1) CORREGGI i difetti con i tool \
             di scrittura e ridichiara la chiusura con task_complete; (2) se ritieni che \
             un rilievo sia INFONDATO, dillo indicando quale e portando l'evidenza \
             (contenuto del file, comportamento atteso) che lo smentisce. Ripetere che il \
             task e' completo, senza modifiche e senza evidenza, non e' nessuna delle due."
                .to_string(),
        );
        lines.join("\n")
    }
}

#[async_trait]
impl GraphNode<AgentState, AgentNodeCtx> for ReviewGateNode {
    fn id(&self) -> NodeId {
        NodeId::ReviewGate
    }

    async fn run(&self, state: &AgentState, ctx: &AgentNodeCtx) -> Result<OpaqueDelta, NodeError> {
        if !self.cfg.enabled {
            return Ok(Self::pass_through());
        }
        // Solo su una chiusura dichiarata come RIUSCITA: rivedere un lavoro
        // dichiarato incompleto (blocked/needs_input/partial) e' rumore; un run
        // gia' bocciato dal final_gate sta gia' chiudendo fallito.
        let declared = crate::routing::declared_outcome_kind(state);
        if matches!(
            declared.as_deref(),
            Some("blocked") | Some("needs_input") | Some("partial")
        ) || state.final_gate_passed == Some(false)
        {
            return Ok(Self::verdict_delta(None, ReviewGateVerdict::NotApplicable));
        }

        // GUARD ANTI-LOOP: se il run e' GIA' stato bocciato in modo DEFINITIVO
        // (RejectedFinal = cycle > max_cycles gia' raggiunto), NON ri-convocare il
        // panel. Senza, ogni re-ingresso nel funnel di chiusura (ondate
        // todo-isolation, rientri) ri-convocava i 2 revisori, incrementava
        // `review_gate_cycle` e ri-bocciava -> loop "(4/3), (5/3), ... (N/3)" visto
        // in UI, che brucia token (panel avversario a ogni giro). Il commento di
        // modulo ("DEFINITIVA -> il run chiude bocciato, mai un loop") era l'INTENTO;
        // questo guard lo rende vero: il verdetto resta RejectedFinal, si esce senza
        // nuova spesa.
        if state
            .review_gate_verdict
            .is_some_and(|v| v.e_bocciatura_definitiva())
        {
            return Ok(Self::pass_through());
        }

        // RIMANDI in correzione gia' effettuati. `review_gate_cycle` conta i
        // rimandi (cosi' lo documenta il campo, e solo cosi' e' commensurabile
        // con `review_max_correction_cycles` = "numero massimo di RIMANDI"), NON
        // le convocazioni del panel: contando le convocazioni, una bocciatura
        // preceduta da N approvazioni trovava il contatore gia' oltre il cap e
        // diventava DEFINITIVA senza che il run avesse mai avuto un tentativo di
        // correzione — accaduto al ciclo 8 del run 609000c1, dove i cicli 4-7
        // erano tutti `pass`. Ed era la stessa discrepanza a far leggere in UI
        // "(2/3)" e "(3/3)" con altri cinque cicli a seguire.
        let rimandi_fatti = state.review_gate_cycle.unwrap_or(0);
        let max_cycles = self.cfg.max_cycles.max(0);

        // Il rimando precedente ha prodotto qualcosa? Se non ha prodotto NULLA, i
        // revisori guarderebbero lo stesso identico codice ed emetterebbero lo
        // stesso identico verdetto: la convocazione e' spesa certa e inutile. La
        // misura vale solo dopo un rimando (`rimandi_fatti > 0`) e solo se e'
        // disponibile: `None` = non lo sappiamo -> si convoca, come prima.
        if let Some(progresso) = self.progresso_dal_rimando(state).await {
            if !progresso.e_progresso() {
                tracing::info!(
                    target: "nexus_agent_graph::review_gate",
                    progresso = progresso.as_str(),
                    rimandi_fatti,
                    "review_gate: rimando a vuoto, panel NON riconvocato"
                );
                return Ok(self
                    .rimando_a_vuoto(state, ctx, rimandi_fatti, max_cycles, progresso)
                    .await);
            }
        }

        let panel = match self.convoca(state, rimandi_fatti + 1).await {
            Ok(panel) => panel,
            Err(delta) => return Ok(delta),
        };

        if !panel.verdict.rejects() {
            return Ok(self
                .close_not_rejected(state, ctx, rimandi_fatti, &panel)
                .await);
        }
        let definitiva = rimandi_fatti >= max_cycles;
        Ok(self
            .boccia(state, ctx, rimandi_fatti, max_cycles, &panel, definitiva)
            .await)
    }
}

impl ReviewGateNode {
    /// Convoca il panel via porta. `Err(delta)` = esito gia' deciso senza
    /// giudizio (porta in errore -> Unavailable; skip -> NotApplicable, salvo
    /// NoValidVerdict -> Unavailable). Best-effort come il post-processing
    /// storico: un guasto della porta non uccide un run in chiusura, ma l'esito
    /// resta ONESTO, mai un silenzioso "approvato".
    async fn convoca(
        &self,
        state: &AgentState,
        cycle: i64,
    ) -> Result<PanelOutcome, OpaqueDelta> {
        let report = match self
            .panel
            .review(ReviewPanelRequest {
                run_id: state.thread_id.clone().unwrap_or_default(),
                cost_spent_usd: state.total_cost_usd.unwrap_or(0.0),
                cycle,
            })
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    target: "nexus_agent_graph::review_gate",
                    error = %e,
                    "review_gate: porta panel in errore, giudizio non disponibile"
                );
                return Err(Self::verdict_delta(None, ReviewGateVerdict::Unavailable));
            }
        };
        match report {
            ReviewPanelReport::Skipped(reason) => {
                let verdict = match reason {
                    ReviewSkipReason::AutoconveneDisabled
                    | ReviewSkipReason::NoCodeChanges
                    | ReviewSkipReason::AlreadyReviewed
                    | ReviewSkipReason::SizedToZero => ReviewGateVerdict::NotApplicable,
                    ReviewSkipReason::NoValidVerdict => ReviewGateVerdict::Unavailable,
                };
                Err(Self::verdict_delta(None, verdict))
            }
            ReviewPanelReport::Convened(panel) => Ok(panel),
        }
    }

    /// Progresso prodotto DAL rimando precedente, o `None` quando la domanda non
    /// si puo' porre.
    ///
    /// I tre `None` non sono lo stesso non-detto, ma portano tutti alla stessa
    /// decisione conservativa (convoca, come prima della misura):
    ///  - porta assente: il grafo gira senza misura (scaffold, fixture);
    ///  - nessun watermark: non c'e' un "prima" con cui confrontare. Misurare
    ///    dall'inizio del run conterebbe come correzione le scritture che hanno
    ///    PRODOTTO i difetti — un progresso inventato, peggio che nessuna misura;
    ///  - errore di lettura: non lo sappiamo, e un `Ok(vuoto)` di ripiego direbbe
    ///    "nessun progresso" sopprimendo una review dovuta (regola G: niente
    ///    fallback che maschera un guasto).
    async fn progresso_dal_rimando(&self, state: &AgentState) -> Option<CorrectionProgress> {
        let port = self.mutations.as_ref()?;
        let da = state.review_correction_watermark?;
        match port.scan_writes(Some(da)).await {
            Ok(scan) => Some(classify_correction_progress(&scan.facts)),
            Err(e) => {
                tracing::warn!(
                    target: "nexus_agent_graph::review_gate",
                    error = %e,
                    "review_gate: misura del progresso non disponibile, si convoca il panel"
                );
                None
            }
        }
    }

    /// Watermark da cui misurare il rimando che si sta per emettere.
    ///
    /// Si legge QUI e non all'ingresso del nodo: fra i due istanti c'e' la
    /// convocazione del panel, e le scritture dei revisori cadrebbero dentro la
    /// finestra del ciclo di correzione facendo passare per progresso dell'agente
    /// il lavoro dei suoi giudici.
    async fn watermark_corrente(&self) -> Option<i64> {
        let port = self.mutations.as_ref()?;
        match port.scan_writes(None).await {
            Ok(scan) => Some(scan.watermark),
            Err(e) => {
                tracing::warn!(
                    target: "nexus_agent_graph::review_gate",
                    error = %e,
                    "review_gate: watermark non leggibile, il prossimo rimando non sara' misurato"
                );
                None
            }
        }
    }

    /// Rimando (o chiusura) SENZA convocare il panel: dal rimando precedente non
    /// risulta alcuna modifica ai file.
    ///
    /// Consuma un tentativo come qualunque altro rimando — il cap governa i
    /// rimandi, e questo lo e' — ma non spende un panel. Al cap la bocciatura
    /// distingue le due cause: se TUTTI i rimandi sono andati a vuoto il run non
    /// ha mai tentato una correzione, ed e' un esito diverso da "ha tentato e non
    /// ci e' riuscito".
    /// Narrazione del rimando a vuoto. `panel_convened: false` nel payload e' il
    /// dato che rende leggibile in UI il risparmio: la riga REVIEW compare senza
    /// i revisori sotto, e si vede che il giro non e' costato un panel.
    async fn narra_a_vuoto(&self, ctx: &AgentNodeCtx, r: &RimandoAVuoto) {
        let RimandoAVuoto {
            cycle,
            max_cycles,
            a_vuoto,
            // `definitiva` non basta a scegliere la frase: dice CHE si chiude, non
            // PERCHE'. La natura del rifiuto la decide `verdetto()`, qui sotto.
            definitiva: _,
            progresso,
        } = *r;
        let mut payload = serde_json::Map::new();
        payload.insert("cycle".into(), cycle.into());
        payload.insert("max_cycles".into(), max_cycles.into());
        payload.insert("progress".into(), progresso.as_str().into());
        payload.insert("no_progress_cycles".into(), a_vuoto.into());
        payload.insert("panel_convened".into(), false.into());
        // Titolo e `phase` DERIVANO dal verdetto (punto unico
        // [`RimandoAVuoto::verdetto`]), lo stesso valore che il delta persiste in
        // `review_gate_verdict`. Ricomporre qui il giudizio dal solo `definitiva`
        // faceva dire alla riga letta dall'utente l'opposto di cio' che il
        // verdetto registrava: su `8365a347` (bacheca-attivita, 29/07/2026) il
        // titolo affermava "nessuna correzione applicata in alcun tentativo"
        // mentre il payload accanto portava `no_progress_cycles: 1` su 3 cicli, e
        // il verdetto era `RejectedFinal` — l'agente aveva corretto due volte su
        // tre. Misurati 5 casi su 9 con la stessa contraddizione.
        //
        // Non e' una sfumatura di stile: la doc di [`ReviewGateVerdict::
        // RejectedNoCorrection`] motiva l'esistenza della variante col fatto che
        // le due cause portano ad AZIONI diverse — "ha provato e non ci e'
        // riuscito" si guarda il codice, "non ha provato" si cambia figura o
        // prompt. Detta con la stessa frase, la distinzione non arriva a chi
        // legge, che e' l'unico posto in cui serviva.
        //
        // Plurale del conteggio: sta FUORI dal match perche' e' formattazione, non
        // una decisione del ramo — e il blocco che serviva a calcolarlo dentro
        // spingeva la stringa a sei livelli di indentazione.
        let giri = if a_vuoto == 1 { "tentativo" } else { "tentativi" };
        let (titolo, phase) = match r.verdetto() {
            ReviewGateVerdict::RejectedNoCorrection => (
                format!(
                    "Review NON superata al cap ({cycle}/{max_cycles}): nessuna correzione \
                     applicata in alcun tentativo"
                ),
                "rejected_no_correction",
            ),
            // Almeno un tentativo ha toccato dei file. Si dichiara il fatto
            // MISURATO (quanti giri risultano a vuoto) senza affermare che gli
            // altri abbiano corretto: `RejectedFinal` copre anche il caso in cui
            // la misura e' mancata per qualche giro e il contatore sottostima
            // (vedi `verdetto`), e li' "ha corretto" sarebbe un'affermazione che
            // nessuno ha verificato.
            ReviewGateVerdict::RejectedFinal => (
                format!(
                    "Review NON superata al cap ({cycle}/{max_cycles}): rilievi ancora \
                     aperti ({a_vuoto} {giri} su {cycle} senza modifiche)"
                ),
                "rejected_final",
            ),
            // Cap non raggiunto: il run rientra in correzione. E' l'unico altro
            // esito che `verdetto` produce (`PendingCorrection`).
            _ => (
                format!(
                    "Rimando senza correzione ({cycle}/{max_cycles}): nessun file modificato, \
                     panel non riconvocato"
                ),
                "stalled",
            ),
        };
        payload.insert("phase".into(), phase.into());
        self.emit(ctx, titolo, payload, None).await;
    }

    async fn rimando_a_vuoto(
        &self,
        state: &AgentState,
        ctx: &AgentNodeCtx,
        rimandi_fatti: i64,
        max_cycles: i64,
        progresso: CorrectionProgress,
    ) -> OpaqueDelta {
        let r = RimandoAVuoto::nuovo(state, rimandi_fatti, max_cycles, progresso);
        self.narra_a_vuoto(ctx, &r).await;

        // Findings dell'ULTIMA convocazione: sono ancora i difetti aperti, visto
        // che il codice non e' cambiato. Ripescati da `extra` invece di
        // riconvocare, che e' esattamente il punto.
        let findings: Vec<serde_json::Value> = state
            .extra
            .get("review_panel_last")
            .and_then(|v| v.get("findings"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut delta = StateDelta {
            review_gate_cycle: Some(Some(r.cycle)),
            review_correction_no_progress: Some(Some(r.a_vuoto)),
            review_gate_verdict: Some(Some(r.verdetto())),
            gate_routing: Some(Some(if r.definitiva {
                GateRouting::Chiude
            } else {
                GateRouting::RimandaInCorrezione
            })),
            ..Default::default()
        };
        if !r.definitiva {
            let block = Self::render_stalled_block(&findings, progresso, r.cycle, r.max_cycles);
            delta.messages = Some(vec![Message::Human {
                content: MessageContent::text(block),
            }]);
            delta.stop_reason = Some(Some(StopReason::ToolUse));
            delta.pending_tool_uses = Some(Some(vec![]));
            delta.review_correction_watermark = Some(self.watermark_corrente().await);
            // Il modello ha ricevuto il verdetto e non ha toccato un file: prima
            // di ridargli lo STESSO rimando, l'executor deve poter promuovere a
            // un modello piu' capace. Il trigger e' la misura (`progresso`
            // non-Effettivo), mai la prosa dell'agente (regola M). Solo sul
            // rimando: su una chiusura definitiva non c'e' un turno da promuovere.
            delta.extra = Some(crate::state::delta::put_extra(
                state,
                REVIEW_GATE_ESCALATION_KEY,
                serde_json::json!(true),
            ));
        }
        delta.into_opaque()
    }

    /// Esito non-rifiuto (Approved/Inconclusive): il run chiude, verdetto
    /// registrato per il finalizzatore.
    async fn close_not_rejected(
        &self,
        state: &AgentState,
        ctx: &AgentNodeCtx,
        rimandi_fatti: i64,
        panel: &PanelOutcome,
    ) -> OpaqueDelta {
        let cycle = rimandi_fatti;
        let verdict = if panel.verdict.is_approved() {
            ReviewGateVerdict::Approved
        } else {
            // Inconclusive: quorum non raggiunto, limite infra (mai rifiuto).
            ReviewGateVerdict::Inconclusive
        };
        let mut payload = serde_json::Map::new();
        payload.insert("cycle".into(), cycle.into());
        payload.insert("phase".into(), "closed".into());
        payload.insert("valid".into(), panel.valid.into());
        payload.insert("total".into(), panel.total_reviews.into());
        // Chi ha votato: senza, il nastro mostra sulle righe REVIEW l'icona del
        // run padre e un panel su piu' provider sembra girato tutto sullo stesso.
        payload.insert("reviewers".into(), panel.reviewers_json());
        self.emit(
            ctx,
            format!(
                "Review adversariale: {} ({}/{} voti validi)",
                panel.verdict.as_str(),
                panel.valid,
                panel.total_reviews
            ),
            payload,
            Some(panel.verdict.as_str()),
        )
        .await;
        StateDelta {
            // Il contatore NON si tocca: un'approvazione non e' un rimando.
            review_gate_verdict: Some(Some(verdict)),
            // Dichiarazione esplicita di CHIUSURA (regola M). Senza, l'edge
            // ereditava lo `stop_reason=ToolUse` dell'executor e rimandava in
            // correzione un run appena APPROVATO: il ping-pong
            // `review_gate -> executor` che ha convocato i revisori 8 volte.
            gate_routing: Some(Some(GateRouting::Chiude)),
            extra: Some(Self::extra_with_panel(state, panel)),
            ..Default::default()
        }
        .into_opaque()
    }

    /// Bocciatura, nelle sue due nature. `definitiva=true` (cap dei rimandi
    /// raggiunto): il run chiude bocciato, l'edge NON rimanda. `definitiva=false`:
    /// rimando in correzione con lo STESSO meccanismo del ramo FAIL del
    /// final_gate (messaggio Human + ToolUse + pending azzerato), regola L.
    async fn boccia(
        &self,
        state: &AgentState,
        ctx: &AgentNodeCtx,
        rimandi_fatti: i64,
        max_cycles: i64,
        panel: &PanelOutcome,
        definitiva: bool,
    ) -> OpaqueDelta {
        // Numero del rimando che questa bocciatura produce. Su bocciatura
        // definitiva non c'e' nessun nuovo rimando: il contatore resta quello
        // dei rimandi gia' spesi, e l'etichetta li mostra tutti e soli quelli
        // (niente piu' `cycle - 1`, aggiustamento che serviva solo perche' il
        // numeratore contava le convocazioni).
        let cycle = if definitiva {
            rimandi_fatti
        } else {
            rimandi_fatti + 1
        };
        let mut payload = serde_json::Map::new();
        payload.insert("cycle".into(), cycle.into());
        payload.insert("max_cycles".into(), max_cycles.into());
        let (titolo, phase) = if definitiva {
            (
                format!(
                    "Review NON superata al cap dei tentativi ({cycle}/{max_cycles}): \
                     il run chiude bocciato"
                ),
                "rejected_final",
            )
        } else {
            tracing::info!(
                target: "nexus_agent_graph::review_gate",
                cycle,
                max_cycles,
                "review_gate: bocciata -> re-executor per correzione"
            );
            payload.insert("findings".into(), panel.findings.len().into());
            (
                format!("Review NON superata: rimando in correzione ({cycle}/{max_cycles})"),
                "failed",
            )
        };
        payload.insert("phase".into(), phase.into());
        payload.insert("reviewers".into(), panel.reviewers_json());
        self.emit(ctx, titolo, payload, Some(panel.verdict.as_str()))
            .await;
        // Watermark del rimando che si sta emettendo, letto DOPO la convocazione
        // (vedi `watermark_corrente`). Sulla bocciatura definitiva non serve:
        // nessun rimando da misurare.
        let watermark = if definitiva {
            None
        } else {
            self.watermark_corrente().await
        };
        Self::boccia_delta(state, cycle, max_cycles, panel, definitiva, watermark)
    }

    /// Delta della bocciatura (puro). Sul rimando: messaggio Human + ToolUse +
    /// pending azzerato -- `Some(Some(vec![]))` e' AZZERA, distinto da None
    /// (no-op): senza, il route dell'executor cadrebbe su tool_dispatch.
    fn boccia_delta(
        state: &AgentState,
        cycle: i64,
        max_cycles: i64,
        panel: &PanelOutcome,
        definitiva: bool,
        watermark: Option<i64>,
    ) -> OpaqueDelta {
        let mut delta = StateDelta {
            review_gate_cycle: Some(Some(cycle)),
            review_gate_verdict: Some(Some(if definitiva {
                ReviewGateVerdict::RejectedFinal
            } else {
                ReviewGateVerdict::PendingCorrection
            })),
            // La bocciatura DEFINITIVA chiude: e' la dichiarazione che il
            // commento di modulo prometteva ("il run chiude bocciato, mai un
            // loop") e che l'edge non poteva vedere finche' instradava sullo
            // `stop_reason` altrui. Sul run 609000c1 il `rejected_final` del
            // ciclo 8 fu seguito da altri 10 `task_complete` e da 4 minuti di
            // giri a vuoto, fino allo Stop dell'utente.
            gate_routing: Some(Some(if definitiva {
                GateRouting::Chiude
            } else {
                GateRouting::RimandaInCorrezione
            })),
            extra: Some(Self::extra_with_panel(state, panel)),
            ..Default::default()
        };
        if !definitiva {
            let block = Self::render_correction_block(panel, cycle, max_cycles);
            delta.messages = Some(vec![Message::Human {
                content: MessageContent::text(block),
            }]);
            delta.stop_reason = Some(Some(StopReason::ToolUse));
            delta.pending_tool_uses = Some(Some(vec![]));
            // Da qui si misurera' se questo rimando ha prodotto qualcosa. Scritto
            // sempre, anche `None`: azzerarlo quando la misura non e' disponibile
            // e' piu' onesto che lasciare in piedi il watermark del rimando
            // PRECEDENTE, che farebbe leggere come progresso di questo giro le
            // scritture del giro prima.
            delta.review_correction_watermark = Some(watermark);
        }
        delta.into_opaque()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nexus_graph::node::{GraphNode, NodeId};
    use nexus_graph::GraphState as _;
    use serde_json::{json, Value};
    use sqlx::postgres::PgPoolOptions;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::*;
    use crate::decisions::{compose_panel_verdict, QuorumPolicy};
    use crate::runtime::test_doubles::{
        NullEventSink, StubLlmGateway, StubMetaStepStore, StubToolExecutor,
    };
    use crate::runtime::AgentNodeCtx;
    use crate::routing::RoutingConfig;

    fn ctx_with() -> AgentNodeCtx {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://test:test@127.0.0.1:1/test")
            .expect("connect_lazy non si connette");
        AgentNodeCtx {
            isolation_available: false,
            db: pool,
            llm: Arc::new(StubLlmGateway::with_text("non usato")),
            tools: Arc::new(StubToolExecutor::with_success(json!("ok"))),
            emit: Arc::new(NullEventSink),
            cfg: RoutingConfig::default(),
            cancel: CancellationToken::new(),
            run_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            thread_id: Uuid::new_v4(),
            advisory_gate: None,
        step_gate: None,
        }
    }

    fn apply(base: AgentState, delta: nexus_graph::StateDelta) -> AgentState {
        let mut s = base;
        s.merge(delta);
        s
    }

    /// Porta stub che ritorna sempre lo stesso report.
    struct StubPanel(ReviewPanelReport);
    #[async_trait::async_trait]
    impl crate::runtime::ports::ReviewPanelPort for StubPanel {
        async fn review(
            &self,
            _req: ReviewPanelRequest,
        ) -> Result<ReviewPanelReport, crate::runtime::ports::PortError> {
            Ok(self.0.clone())
        }
    }

    /// Il verdetto arriva dal PRODUTTORE di produzione (`compose_panel_verdict`,
    /// regola O), a partire dagli outcome nella forma che `run_single_subagent`
    /// mette in `outcome.review` -- mai un PanelOutcome fabbricato a mano.
    fn panel_bocciato() -> PanelOutcome {
        let outcomes: Vec<Value> = vec![json!({
            "success": true,
            "review": {
                "verdict": "fail",
                "findings": [{
                    "file": "backend/server.cjs",
                    "severity": "alta",
                    "description": "request_port non definita: ReferenceError all'avvio",
                }],
            },
        })];
        compose_panel_verdict(
            &outcomes,
            &QuorumPolicy {
                min_valid_verdicts: 1,
                fail_on_high_severity: true,
                min_severity_per_rimando: crate::decisions::severity::Severity::Medium,
            },
        )
        .expect("panel di review valido")
    }

    fn panel_approvato() -> PanelOutcome {
        let outcomes: Vec<Value> = vec![json!({
            "success": true,
            "review": { "verdict": "pass", "findings": [] },
        })];
        compose_panel_verdict(
            &outcomes,
            &QuorumPolicy {
                min_valid_verdicts: 1,
                fail_on_high_severity: true,
                min_severity_per_rimando: crate::decisions::severity::Severity::Medium,
            },
        )
        .expect("panel valido")
    }

    fn stato_done() -> AgentState {
        AgentState {
            thread_id: Some(Uuid::new_v4().to_string()),
            declared_outcome: Some(json!({"outcome": "done", "summary": "fatto"})),
            ..Default::default()
        }
    }

    fn nodo(max_cycles: i64, report: ReviewPanelReport) -> ReviewGateNode {
        ReviewGateNode::new(
            ReviewGateConfig {
                enabled: true,
                max_cycles,
            },
            Arc::new(StubPanel(report)),
            Arc::new(StubMetaStepStore::default()),
        )
    }

    /// REGRESSIONE (il difetto chiesto due volte dall'utente: "se non superata
    /// dovrebbe tentare di sistemare"): la bocciatura RIMANDA in correzione.
    /// Si asserisce la CONSEGUENZA: l'edge del ReviewGate risolve su Executor,
    /// il verdetto e' PendingCorrection, e il messaggio Human porta il file del
    /// finding (la consegna di correzione e' azionabile).
    #[tokio::test]
    async fn bocciatura_rimanda_in_correzione() {
        let node = nodo(1, ReviewPanelReport::Convened(panel_bocciato()));
        let delta = node
            .run(&stato_done(), &ctx_with())
            .await
            .expect("nodo ok");
        let s = apply(stato_done(), delta);

        assert_eq!(s.review_gate_verdict, Some(ReviewGateVerdict::PendingCorrection));
        assert_eq!(s.review_gate_cycle, Some(1));
        // Il rimando usa il predicato UNICO dei gate: l'edge deve andare a Executor.
        assert!(
            crate::routing::gate_rimanda_in_correzione(&s),
            "stop_reason ToolUse atteso: senza, l'edge chiude su Reflection e la \
             correzione non avviene mai"
        );
        assert_eq!(
            s.pending_tool_uses.as_deref(),
            Some(&[][..]),
            "pending azzerato: senza, il route cade su tool_dispatch"
        );
        let ultimo_human = s
            .messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Message::Human { content } => Some(content.flatten_text()),
                _ => None,
            })
            .expect("messaggio di correzione presente");
        assert!(
            ultimo_human.contains("backend/server.cjs"),
            "la consegna deve citare il file del finding: {ultimo_human}"
        );
    }

    /// Al cap dei rimandi la bocciatura e' DEFINITIVA: nessun rimando (il run
    /// chiude), verdetto RejectedFinal. E' l'anti-loop.
    #[tokio::test]
    async fn al_cap_la_bocciatura_diventa_definitiva() {
        let node = nodo(1, ReviewPanelReport::Convened(panel_bocciato()));
        let gia_rimandato = AgentState {
            review_gate_cycle: Some(1),
            ..stato_done()
        };
        let delta = node
            .run(&gia_rimandato, &ctx_with())
            .await
            .expect("nodo ok");
        let s = apply(gia_rimandato, delta);

        assert_eq!(s.review_gate_verdict, Some(ReviewGateVerdict::RejectedFinal));
        assert!(
            !crate::routing::gate_rimanda_in_correzione(&s),
            "al cap NON si rimanda: il run deve chiudere (bocciato), mai un loop"
        );
    }

    /// Anti-loop: un run GIA' bocciato in modo definitivo (RejectedFinal) NON
    /// ri-convoca il panel a un nuovo ingresso. Senza il guard, `run` convocherebbe
    /// di nuovo i revisori e incrementerebbe `review_gate_cycle` (4/3, 5/3, ...) ->
    /// il loop visto in UI. Test di mutazione: rimuovendo il guard, il cycle passa
    /// da 5 a 6 e questo assert rosseggia.
    #[tokio::test]
    async fn gia_rejected_final_non_riconvoca() {
        // Panel che boccerebbe SE convocato: il guard deve impedire la convocazione.
        let node = nodo(1, ReviewPanelReport::Convened(panel_bocciato()));
        let gia_definitivo = AgentState {
            review_gate_verdict: Some(ReviewGateVerdict::RejectedFinal),
            review_gate_cycle: Some(5),
            ..stato_done()
        };
        let delta = node
            .run(&gia_definitivo, &ctx_with())
            .await
            .expect("nodo ok");
        let s = apply(gia_definitivo, delta);
        // pass_through: verdetto e cycle INVARIATI (nessuna ri-review, nessuna spesa).
        assert_eq!(s.review_gate_verdict, Some(ReviewGateVerdict::RejectedFinal));
        assert_eq!(
            s.review_gate_cycle,
            Some(5),
            "il cycle NON incrementa: nessuna ri-convocazione del panel"
        );
        assert!(!crate::routing::gate_rimanda_in_correzione(&s));
    }

    /// Porta stub che CONTA le convocazioni: il costo del panel e' il danno
    /// misurato (10 sub-run di review a pagamento sul run 609000c1), quindi il
    /// test lo asserisce sul contatore, non su un effetto collaterale.
    struct CountingPanel {
        report: ReviewPanelReport,
        convocazioni: Arc<std::sync::atomic::AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl crate::runtime::ports::ReviewPanelPort for CountingPanel {
        async fn review(
            &self,
            _req: ReviewPanelRequest,
        ) -> Result<ReviewPanelReport, crate::runtime::ports::PortError> {
            self.convocazioni
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self.report.clone())
        }
    }

    /// REGRESSIONE del run 609000c1 (26/07/2026): il ping-pong
    /// `review_gate -> executor -> review_gate` che ha convocato i revisori 8
    /// volte e non ha mai chiuso il run.
    ///
    /// Lo strumento arriva all'oggetto per la strada della produzione (regola O):
    /// nodo REALE, edge REALE preso da `build_edges` (la topologia che gira nel
    /// motore), e stato iniziale copiato dal CHECKPOINT del run — `stop_reason =
    /// ToolUse` con tool pendenti, cioe' quello che l'executor lascia a ogni
    /// turno. Il loop qui sotto e' il loop del motore: esegui il nodo, merge,
    /// risolvi l'edge; se torna all'executor, il gate verra' rieseguito.
    ///
    /// MUTAZIONE: rimuovendo `gate_routing: Chiude` da `close_not_rejected` (o
    /// facendo tornare `gate_rimanda_in_correzione` a `stop_reason == ToolUse`)
    /// il test fallisce mostrando la convocazione ripetuta: `giri` arriva al cap
    /// e le convocazioni salgono a 8, esattamente come in produzione.
    #[tokio::test]
    async fn approvazione_chiude_e_non_riconvoca_i_revisori() {
        let convocazioni = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let node = ReviewGateNode::new(
            ReviewGateConfig {
                enabled: true,
                max_cycles: 3,
            },
            Arc::new(CountingPanel {
                report: ReviewPanelReport::Convened(panel_approvato()),
                convocazioni: Arc::clone(&convocazioni),
            }),
            Arc::new(StubMetaStepStore::default()),
        );
        let edges = crate::graph::build_edges(
            RoutingConfig::default(),
            crate::nodes::PlannerConfig::default(),
            crate::decisions::supervisor::SupervisorConfig::default(),
        );
        let edge = edges.get(&NodeId::ReviewGate).expect("edge review_gate");

        // Stato come nel checkpoint 173 del run: l'executor ha appena lasciato
        // ToolUse + pending, il funnel di chiusura porta al gate.
        let mut s = AgentState {
            stop_reason: Some(StopReason::ToolUse),
            pending_tool_uses: Some(vec![json!({
                "type": "tool_use", "id": "t1", "name": "read_file", "input": {}
            })]),
            ..stato_done()
        };

        let mut giri = 0;
        let mut destinazione = loop {
            giri += 1;
            let delta = node.run(&s, &ctx_with()).await.expect("nodo ok");
            s = apply(s, delta);
            let next = edge.resolve(&s);
            if next != NodeId::Executor || giri >= 8 {
                break next;
            }
        };
        // Il gate ha APPROVATO: l'edge deve portare alla chiusura, non
        // all'executor. In produzione portava all'executor e il run rientrava.
        assert_eq!(
            destinazione,
            NodeId::Reflection,
            "review approvata: il grafo deve proseguire verso la chiusura, non \
             rientrare nell'executor"
        );
        assert_eq!(giri, 1, "il gate non deve essere rieseguito dopo un'approvazione");
        assert_eq!(
            convocazioni.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "i revisori (sub-run A PAGAMENTO) vanno convocati una volta sola: \
             sul run 609000c1 furono 10 sub-run per $0.058355"
        );

        // Stessa asserzione per la bocciatura DEFINITIVA (punto b del difetto):
        // `rejected_final` deve essere terminale. In produzione fu seguita da
        // altri 10 task_complete e da 4 minuti di giri a vuoto.
        let definitivo = AgentState {
            review_gate_verdict: Some(ReviewGateVerdict::RejectedFinal),
            stop_reason: Some(StopReason::ToolUse),
            ..stato_done()
        };
        let delta = node.run(&definitivo, &ctx_with()).await.expect("nodo ok");
        let dopo = apply(definitivo, delta);
        destinazione = edge.resolve(&dopo);
        assert_eq!(
            destinazione,
            NodeId::Reflection,
            "rejected_final e' DEFINITIVA: il run deve chiudere"
        );
    }

    /// Il contatore misura cio' che il cap limita (punto a del difetto): le
    /// APPROVAZIONI non consumano rimandi. Con il contatore delle convocazioni,
    /// una bocciatura preceduta da 3 approvazioni trovava `cycle=4 > max=3` e
    /// diventava DEFINITIVA senza che il run avesse mai avuto un tentativo di
    /// correzione — ed era la stessa discrepanza a stampare "(2/3)" e "(3/3)" su
    /// un run che poi mostrava i cicli 4..8.
    ///
    /// MUTAZIONE: tornando a contare le convocazioni, il verdetto qui e'
    /// `RejectedFinal` e l'assert rosseggia.
    #[tokio::test]
    async fn le_approvazioni_non_consumano_i_rimandi() {
        let approva = nodo(3, ReviewPanelReport::Convened(panel_approvato()));
        let mut s = stato_done();
        for _ in 0..3 {
            let delta = approva.run(&s, &ctx_with()).await.expect("nodo ok");
            s = apply(s, delta);
        }
        assert_eq!(
            s.review_gate_cycle, None,
            "nessun rimando effettuato: il contatore dei rimandi resta intatto"
        );

        // Prima bocciatura dopo tre approvazioni: ha diritto al rimando 1/3.
        let boccia = nodo(3, ReviewPanelReport::Convened(panel_bocciato()));
        let delta = boccia.run(&s, &ctx_with()).await.expect("nodo ok");
        let s = apply(s, delta);
        assert_eq!(
            s.review_gate_verdict,
            Some(ReviewGateVerdict::PendingCorrection),
            "la prima bocciatura deve poter rimandare in correzione"
        );
        assert_eq!(s.review_gate_cycle, Some(1), "e' il rimando numero 1");
        assert!(crate::routing::gate_rimanda_in_correzione(&s));
    }

    /// Approvazione: nessun rimando, verdetto Approved, il run chiude pulito.
    #[tokio::test]
    async fn approvazione_chiude_senza_rimando() {
        let node = nodo(1, ReviewPanelReport::Convened(panel_approvato()));
        let delta = node
            .run(&stato_done(), &ctx_with())
            .await
            .expect("nodo ok");
        let s = apply(stato_done(), delta);
        assert_eq!(s.review_gate_verdict, Some(ReviewGateVerdict::Approved));
        assert!(!crate::routing::gate_rimanda_in_correzione(&s));
    }

    // ── MISURA DEL PROGRESSO: il ciclo conta i tentativi, ora anche i fatti ────

    /// Porta di misura che risponde con fatti FISSI. Il watermark avanza a ogni
    /// lettura, cosi' il nodo non puo' passare il test per l'accidente di un
    /// watermark che resta fermo.
    struct StubMutations {
        facts: Vec<crate::decisions::WriteFact>,
        /// Letture ricevute: prova che il gate misura invece di indovinare.
        letture: Arc<std::sync::atomic::AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl MutationProgressPort for StubMutations {
        async fn scan_writes(
            &self,
            after: Option<i64>,
        ) -> Result<crate::runtime::ports::WriteScan, crate::runtime::ports::PortError> {
            self.letture
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(match after {
                // Richiesta del solo watermark (emissione di un rimando).
                None => crate::runtime::ports::WriteScan {
                    watermark: 100,
                    facts: Vec::new(),
                },
                Some(w) => crate::runtime::ports::WriteScan {
                    watermark: w + 1,
                    facts: self.facts.clone(),
                },
            })
        }
    }

    /// Porta di misura GUASTA: il DB non risponde.
    struct MutationsInErrore;
    #[async_trait::async_trait]
    impl MutationProgressPort for MutationsInErrore {
        async fn scan_writes(
            &self,
            _after: Option<i64>,
        ) -> Result<crate::runtime::ports::WriteScan, crate::runtime::ports::PortError> {
            Err(crate::runtime::ports::PortError::Tool("DB irraggiungibile".into()))
        }
    }

    fn scrittura(before: &str, after: &str) -> crate::decisions::WriteFact {
        crate::decisions::WriteFact {
            before_sha256: Some(before.to_string()),
            after_sha256: Some(after.to_string()),
            solo_fine_riga: None,
        }
    }

    /// Esito di due giri di gate su un panel che boccia sempre: quante volte i
    /// revisori sono stati convocati, e lo stato finale.
    ///
    /// I due giri sono il difetto in miniatura: primo giro -> bocciatura e
    /// rimando; secondo giro -> il gate rientra dopo che l'executor ha (o non ha)
    /// corretto. E' il punto in cui la spesa si ripete.
    async fn due_giri(
        max_cycles: i64,
        mutations: Option<Arc<dyn MutationProgressPort>>,
    ) -> (usize, AgentState) {
        let convocazioni = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut node = ReviewGateNode::new(
            ReviewGateConfig {
                enabled: true,
                max_cycles,
            },
            Arc::new(CountingPanel {
                report: ReviewPanelReport::Convened(panel_bocciato()),
                convocazioni: Arc::clone(&convocazioni),
            }),
            Arc::new(StubMetaStepStore::default()),
        );
        if let Some(port) = mutations {
            node = node.with_mutation_progress(port);
        }
        let mut s = stato_done();
        for _ in 0..2 {
            let delta = node.run(&s, &ctx_with()).await.expect("nodo ok");
            s = apply(s, delta);
        }
        (
            convocazioni.load(std::sync::atomic::Ordering::SeqCst),
            s,
        )
    }

    /// (a) Il rimando HA prodotto una modifica: il panel va riconvocato, perche'
    /// il codice che deve giudicare e' cambiato.
    ///
    /// E' il caso che tiene onesta la misura: un controllo che sopprimesse anche
    /// questa convocazione romperebbe la review invece di risparmiarla.
    #[tokio::test]
    async fn con_mutazione_reale_il_panel_viene_riconvocato() {
        let (convocazioni, s) = due_giri(
            3,
            Some(Arc::new(StubMutations {
                facts: vec![scrittura("prima", "dopo")],
                letture: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            })),
        )
        .await;
        assert_eq!(
            convocazioni, 2,
            "il codice e' cambiato: i revisori devono rivederlo"
        );
        assert_eq!(s.review_gate_cycle, Some(2), "due rimandi");
        assert_eq!(
            s.review_correction_no_progress, None,
            "nessun giro a vuoto da contare"
        );
    }

    /// (b) Il rimando NON ha prodotto nulla: il panel NON va riconvocato.
    /// Stesso codice, stesso verdetto — la convocazione e' spesa certa e inutile.
    /// E' il caso del run osservato: tre convocazioni sullo stesso identico
    /// codice, 1.243.417 token.
    ///
    /// MUTAZIONE: togliere il ramo `if !progresso.e_progresso()` da
    /// `ReviewGateNode::run` porta le convocazioni da 1 a 2 e questo assert
    /// rosseggia.
    #[tokio::test]
    async fn senza_mutazioni_il_panel_non_viene_riconvocato() {
        let letture = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (convocazioni, s) = due_giri(
            3,
            Some(Arc::new(StubMutations {
                facts: Vec::new(),
                letture: Arc::clone(&letture),
            })),
        )
        .await;
        assert_eq!(
            convocazioni, 1,
            "nessun file e' cambiato: i revisori vedrebbero lo stesso codice"
        );
        assert!(
            letture.load(std::sync::atomic::Ordering::SeqCst) > 0,
            "il gate deve aver MISURATO, non dedotto"
        );
        // Il tentativo e' comunque consumato: il cap governa i rimandi, e questo
        // lo e'. Cio' che non si spende e' il panel.
        assert_eq!(s.review_gate_cycle, Some(2));
        assert_eq!(s.review_correction_no_progress, Some(1));
        assert!(
            crate::routing::gate_rimanda_in_correzione(&s),
            "sotto il cap si rimanda comunque: l'agente ha ancora tentativi"
        );
    }

    /// (c) Il write c'e' stato ma il contenuto e' IDENTICO
    /// (`before_sha256 == after_sha256`): non e' una correzione, ed e' il modo in
    /// cui un agente puo' simulare attivita' senza produrne. Un contatore di
    /// chiamate a `write_file` qui direbbe "ha lavorato".
    ///
    /// MUTAZIONE: togliere il ramo `if !progresso.e_progresso()` da
    /// `ReviewGateNode::run` (o far ritornare `true` costante a
    /// `WriteFact::cambia_il_contenuto`) porta le convocazioni da 1 a 2.
    #[tokio::test]
    async fn riscrittura_identica_non_riconvoca_il_panel() {
        let (convocazioni, s) = due_giri(
            3,
            Some(Arc::new(StubMutations {
                facts: vec![scrittura("uguale", "uguale"), scrittura("idem", "idem")],
                letture: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            })),
        )
        .await;
        assert_eq!(
            convocazioni, 1,
            "due write a contenuto invariato non sono una correzione"
        );
        assert_eq!(s.review_correction_no_progress, Some(1));
        // Il rimando dice all'agente COSA e' stato misurato: senza il fatto, il
        // messaggio sarebbe di nuovo liquidabile con "gia' fatto".
        let ultimo_human = s
            .messages
            .iter()
            .rev()
            .find_map(|m| match m {
                Message::Human { content } => Some(content.flatten_text()),
                _ => None,
            })
            .expect("messaggio di rimando presente");
        assert!(
            ultimo_human.contains("NESSUNA ha cambiato"),
            "il rimando deve opporre la misura, non ripetere il verdetto: {ultimo_human}"
        );
        assert!(
            ultimo_human.contains("backend/server.cjs"),
            "i difetti ancora aperti restano elencati: {ultimo_human}"
        );
    }

    // ── TRIGGER DI ESCALATION: il rimando a vuoto lo posa, un esito lo rimuove ─

    /// Il rimando a vuoto NON definitivo posa [`REVIEW_GATE_ESCALATION_KEY`]:
    /// e' il segnale strutturato (regola M) con cui l'executor promuove il
    /// modello PRIMA di ridargli lo stesso rimando. E' il difetto misurato su
    /// bacheca-attivita (run e8433555): il ciclo di correzione restava su
    /// claude-haiku fino al cap, senza mai la salita di modello che il percorso
    /// principale aveva gia'.
    ///
    /// MUTAZIONE: togliere la posa del flag da `rimando_a_vuoto` (reintroduce il
    /// bug: correzione muta per l'escalation) rende rosso questo assert.
    #[tokio::test]
    async fn rimando_a_vuoto_posa_il_trigger_di_escalation() {
        let (_, s) = due_giri(
            3,
            Some(Arc::new(StubMutations {
                facts: Vec::new(),
                letture: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            })),
        )
        .await;
        assert!(
            crate::routing::gate_rimanda_in_correzione(&s),
            "premessa: siamo su un rimando, non su una chiusura"
        );
        assert_eq!(
            s.extra.get(REVIEW_GATE_ESCALATION_KEY),
            Some(&json!(true)),
            "il rimando a vuoto deve chiedere l'escalation all'executor"
        );
    }

    /// Sulla chiusura DEFINITIVA (cap dei rimandi raggiunto) il flag NON nasce:
    /// non c'e' un turno di correzione da promuovere, il run sta chiudendo
    /// bocciato.
    #[tokio::test]
    async fn rimando_a_vuoto_definitivo_non_posa_il_trigger() {
        let (_, s) = due_giri(
            1,
            Some(Arc::new(StubMutations {
                facts: Vec::new(),
                letture: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            })),
        )
        .await;
        assert_eq!(
            s.review_gate_verdict,
            Some(ReviewGateVerdict::RejectedNoCorrection),
            "premessa: il secondo giro chiude al cap"
        );
        assert!(
            s.extra.get(REVIEW_GATE_ESCALATION_KEY).is_none(),
            "nessuna escalation da chiedere su una chiusura definitiva"
        );
    }

    /// Un flag rimasto pendente (catena di escalation esaurita al rientro) NON
    /// sopravvive a un nuovo esito del panel: se il giro successivo ha prodotto
    /// modifiche e il panel ha giudicato di nuovo, un'escalation nata da quel
    /// flag dichiarerebbe un fatto ("rimando senza correzioni") non piu' vero
    /// (regola M). La rimozione sta in `extra_with_panel`, il punto da cui
    /// passano ENTRAMBI gli esiti (bocciatura e approvazione).
    #[tokio::test]
    async fn un_nuovo_esito_del_panel_rimuove_il_trigger_pendente() {
        let node = nodo(3, ReviewPanelReport::Convened(panel_bocciato()))
            .with_mutation_progress(Arc::new(StubMutations {
                facts: vec![scrittura("prima", "dopo")],
                letture: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }));
        let mut s = stato_done();
        s.review_gate_cycle = Some(1);
        s.review_correction_watermark = Some(50);
        s.extra
            .insert(REVIEW_GATE_ESCALATION_KEY.to_string(), json!(true));
        let delta = node.run(&s, &ctx_with()).await.expect("nodo ok");
        let out = apply(s, delta);
        assert_eq!(
            out.review_gate_verdict,
            Some(ReviewGateVerdict::PendingCorrection),
            "premessa: il panel ha giudicato di nuovo (bocciatura, nuovo rimando)"
        );
        assert!(
            out.extra.get(REVIEW_GATE_ESCALATION_KEY).is_none(),
            "il segnale del rimando a vuoto precedente non deve sopravvivere a \
             un esito nuovo del panel"
        );
    }

    /// Punto 4 del difetto: al cap, "non ha mai tentato" e' un esito DIVERSO da
    /// "ha tentato e non ci e' riuscito", e l'azione dell'utente e' diversa.
    ///
    /// Con `max_cycles = 1`: primo giro boccia e rimanda; secondo giro misura il
    /// vuoto e chiude. Tutti i rimandi (uno) sono andati a vuoto.
    ///
    /// MUTAZIONE: far ritornare sempre `RejectedFinal` a `rimando_a_vuoto`
    /// confonde le due cause e questo assert rosseggia.
    #[tokio::test]
    async fn al_cap_senza_una_sola_correzione_l_esito_e_distinto() {
        let (convocazioni, s) = due_giri(
            1,
            Some(Arc::new(StubMutations {
                facts: Vec::new(),
                letture: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            })),
        )
        .await;
        assert_eq!(convocazioni, 1);
        assert_eq!(
            s.review_gate_verdict,
            Some(ReviewGateVerdict::RejectedNoCorrection),
            "nessun rimando ha prodotto modifiche: la causa non e' la difficolta' \
             del rilievo"
        );
        assert!(
            !crate::routing::gate_rimanda_in_correzione(&s),
            "al cap il run chiude"
        );
        assert!(
            s.review_gate_verdict
                .expect("verdetto")
                .e_bocciatura_definitiva(),
            "resta una bocciatura definitiva: chi chiede solo questo non deve \
             elencare le varianti"
        );
    }

    /// Un tentativo che AVEVA prodotto modifiche esclude la causa "non ha mai
    /// tentato": al cap si chiude col verdetto storico. Senza questa distinzione
    /// il nuovo esito diventerebbe un'etichetta appiccicata a ogni bocciatura.
    ///
    /// Tre giri con `max_cycles = 2`: (1) boccia -> rimando 1; (2) misura
    /// PROGRESSO -> riconvoca, boccia -> rimando 2; (3) misura il vuoto -> al cap,
    /// chiude. Un giro a vuoto su due rimandi.
    /// Porta di misura che risponde PROGRESSO alla prima lettura e VUOTO dalle
    /// successive: e' lo scenario "ha tentato una volta, poi si e' fermato", il
    /// solo che separa le due nature del rifiuto finale. Sta qui, e non dentro un
    /// test, perche' la esercitano in due.
    struct MutazioniAlternate {
        chiamate: std::sync::atomic::AtomicUsize,
    }
    impl MutazioniAlternate {
        fn nuova() -> Self {
            Self {
                chiamate: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }
    #[async_trait::async_trait]
    impl MutationProgressPort for MutazioniAlternate {
        async fn scan_writes(
            &self,
            after: Option<i64>,
        ) -> Result<crate::runtime::ports::WriteScan, crate::runtime::ports::PortError> {
            let Some(w) = after else {
                return Ok(crate::runtime::ports::WriteScan {
                    watermark: 100,
                    facts: Vec::new(),
                });
            };
            // Prima misura: ha corretto. Seconda: si e' fermato.
            let n = self
                .chiamate
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let facts = if n == 0 {
                vec![crate::decisions::WriteFact {
                    before_sha256: Some("prima".into()),
                    after_sha256: Some("dopo".into()),
                    solo_fine_riga: None,
                }]
            } else {
                Vec::new()
            };
            Ok(crate::runtime::ports::WriteScan {
                watermark: w + 1,
                facts,
            })
        }
    }

    #[tokio::test]
    async fn un_tentativo_riuscito_esclude_l_esito_senza_correzioni() {
        let convocazioni = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let node = ReviewGateNode::new(
            ReviewGateConfig {
                enabled: true,
                max_cycles: 2,
            },
            Arc::new(CountingPanel {
                report: ReviewPanelReport::Convened(panel_bocciato()),
                convocazioni: Arc::clone(&convocazioni),
            }),
            Arc::new(StubMetaStepStore::default()),
        )
        .with_mutation_progress(Arc::new(MutazioniAlternate::nuova()));

        let mut s = stato_done();
        for _ in 0..3 {
            let delta = node.run(&s, &ctx_with()).await.expect("nodo ok");
            s = apply(s, delta);
        }
        assert_eq!(
            convocazioni.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "il giro con progresso riconvoca, quello a vuoto no"
        );
        assert_eq!(s.review_correction_no_progress, Some(1), "un solo giro a vuoto");
        assert_eq!(
            s.review_gate_verdict,
            Some(ReviewGateVerdict::RejectedFinal),
            "un tentativo aveva prodotto modifiche: la causa e' il rilievo, non l'inerzia"
        );
    }

    /// La RIGA CHE L'UTENTE LEGGE dice la stessa cosa del verdetto registrato.
    ///
    /// Il verdetto strutturato distingueva gia' le due cause; la narrazione no:
    /// emetteva "nessuna correzione applicata in alcun tentativo" su OGNI chiusura
    /// al cap, anche quando il payload accanto portava `no_progress_cycles: 1` su
    /// 3 cicli. Misurato sul progetto `bacheca-attivita` (29/07/2026): 5 run su 9
    /// chiusi al cap affermavano l'inerzia con la propria misura che la smentiva
    /// — fra questi `8365a347`, dove l'agente aveva corretto due volte su tre.
    ///
    /// Il test riusa lo scenario del precedente, che gia' attraversa il nodo reale
    /// (regola O), e prosegue fino a CIO' CHE VIENE SCRITTO: fermarsi al verdetto
    /// e' esattamente il punto cieco che ha lasciato passare il difetto per tre
    /// giorni, perche' il verdetto era giusto e non e' mai stato lui a mentire.
    ///
    /// MUTAZIONE: rimettere `if definitiva` al posto del `match r.verdetto()` in
    /// `narra_a_vuoto` rende questo test rosso su `phase`, che torna
    /// `rejected_no_correction`.
    /// Tre giri di gate con `max_cycles = 2` su un panel che boccia sempre, con
    /// la misura del progresso data dal chiamante. Ritorna lo stato finale e i
    /// meta-step NARRATI: i due test che seguono guardano l'uno il verdetto,
    /// l'altro la riga scritta, e devono vederli nascere dallo stesso giro.
    async fn tre_giri_narrati(
        mutations: Arc<dyn MutationProgressPort>,
    ) -> (AgentState, Vec<Value>) {
        let meta = Arc::new(StubMetaStepStore::default());
        let node = ReviewGateNode::new(
            ReviewGateConfig {
                enabled: true,
                max_cycles: 2,
            },
            Arc::new(StubPanel(ReviewPanelReport::Convened(panel_bocciato()))),
            Arc::clone(&meta) as Arc<dyn crate::runtime::ports::MetaStepStore>,
        )
        .with_mutation_progress(mutations);

        let mut s = stato_done();
        for _ in 0..3 {
            let delta = node.run(&s, &ctx_with()).await.expect("nodo ok");
            s = apply(s, delta);
        }
        let steps = meta.meta_steps.lock().expect("lock meta_steps").clone();
        (s, steps)
    }

    #[tokio::test]
    async fn la_narrazione_al_cap_non_contraddice_il_verdetto() {
        let (s, steps) = tre_giri_narrati(Arc::new(MutazioniAlternate::nuova())).await;

        // Il verdetto: un tentativo aveva corretto, quindi NON e' inerzia.
        assert_eq!(
            s.review_gate_verdict,
            Some(ReviewGateVerdict::RejectedFinal),
            "premessa dello scenario"
        );

        let chiusura = steps
            .iter()
            .rev()
            .find(|m| {
                m["payload"]["panel_convened"] == serde_json::json!(false)
                    && m["payload"]["phase"] != serde_json::json!("stalled")
            })
            .expect("la chiusura al cap e' stata narrata");

        assert_eq!(
            chiusura["payload"]["phase"],
            serde_json::json!("rejected_final"),
            "la phase deve seguire il verdetto, non il solo fatto che si chiuda"
        );
        let titolo = chiusura["title"].as_str().expect("titolo testuale");
        assert!(
            !titolo.contains("in alcun tentativo"),
            "un tentativo aveva corretto: il titolo non puo' negarlo -- {titolo}"
        );
        // Il fatto misurato compare, ed e' quello che il payload dichiara.
        assert!(
            titolo.contains("1 tentativo su 2"),
            "il titolo deve portare la misura, non un giudizio generico -- {titolo}"
        );
        assert_eq!(
            chiusura["payload"]["no_progress_cycles"],
            serde_json::json!(1),
            "titolo e payload raccontano lo stesso numero"
        );
    }

    /// Il verso opposto: quando l'inerzia c'e' DAVVERO, la frase che la denuncia
    /// resta. Senza questo, "non contraddire il verdetto" si otterrebbe anche
    /// cancellando la distinzione — cioe' perdendo l'informazione che il ramo
    /// esiste per dare.
    #[tokio::test]
    async fn zero_correzioni_conserva_la_denuncia_di_inerzia() {
        // Nessuna scrittura, mai: l'agente non ha alzato un dito.
        let (s, steps) = tre_giri_narrati(Arc::new(StubMutations {
            facts: Vec::new(),
            letture: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }))
        .await;

        assert_eq!(
            s.review_gate_verdict,
            Some(ReviewGateVerdict::RejectedNoCorrection),
            "nessun giro ha toccato un file"
        );
        let chiusura = steps
            .iter()
            .rev()
            .find(|m| m["payload"]["phase"] == serde_json::json!("rejected_no_correction"))
            .expect("la chiusura per inerzia e' narrata come tale");
        assert!(
            chiusura["title"]
                .as_str()
                .expect("titolo testuale")
                .contains("in alcun tentativo"),
            "qui l'affermazione e' vera e deve restare"
        );
    }

    /// FAIL-SAFE (regola G): misura NON disponibile — porta assente o in errore —
    /// il gate si comporta come prima e convoca.
    ///
    /// E' il verso che conta: un `Ok(vuoto)` di ripiego direbbe "nessun
    /// progresso" e sopprimerebbe una review dovuta a ogni singhiozzo del DB,
    /// cioe' il difetto opposto e piu' grave di quello che la misura chiude.
    #[tokio::test]
    async fn senza_misura_il_gate_convoca_come_prima() {
        let (senza_porta, _) = due_giri(3, None).await;
        assert_eq!(senza_porta, 2, "porta assente: comportamento storico");

        let (porta_guasta, s) = due_giri(3, Some(Arc::new(MutationsInErrore))).await;
        assert_eq!(porta_guasta, 2, "porta in errore: non si sopprime la review");
        assert_eq!(
            s.review_correction_no_progress, None,
            "un guasto non e' un giro a vuoto dell'agente"
        );
    }

    /// Il watermark nasce dal rimando e delimita la finestra della misura. Senza,
    /// il gate confronterebbe con l'inizio del run e leggerebbe come correzione
    /// il lavoro che ha PRODOTTO i difetti.
    #[tokio::test]
    async fn il_rimando_scrive_il_watermark_da_cui_misurare() {
        let node = nodo(3, ReviewPanelReport::Convened(panel_bocciato()))
            .with_mutation_progress(Arc::new(StubMutations {
                facts: Vec::new(),
                letture: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }));
        let delta = node.run(&stato_done(), &ctx_with()).await.expect("nodo ok");
        let s = apply(stato_done(), delta);
        assert_eq!(
            s.review_correction_watermark,
            Some(100),
            "il rimando registra da dove misurare il giro successivo"
        );
    }

    /// Dichiarazione non-done (blocked): il gate non si applica, mai un panel.
    #[tokio::test]
    async fn dichiarazione_blocked_non_convoca() {
        let node = nodo(1, ReviewPanelReport::Convened(panel_bocciato()));
        let blocked = AgentState {
            declared_outcome: Some(json!({"outcome": "blocked", "summary": "x"})),
            ..stato_done()
        };
        let delta = node.run(&blocked, &ctx_with()).await.expect("nodo ok");
        let s = apply(blocked, delta);
        assert_eq!(s.review_gate_verdict, Some(ReviewGateVerdict::NotApplicable));
        assert!(!crate::routing::gate_rimanda_in_correzione(&s));
    }
}

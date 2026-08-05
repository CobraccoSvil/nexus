//! «Questa sospensione la sciogliera' qualcuno?» — punto unico (regola L) della
//! scadenza di un run sospeso.
//!
//! CAUSA RADICE (rilievo A4 del processo standard figure, ADR 0043). Il gate
//! duale sui passi critici sospende in HITL anche in Automatic: e' il punto del
//! requisito — su un passo irreversibile con validatori discordi decide l'umano.
//! Ma in Automatic/Continuous quell'umano non esiste, e nessuno degli apparati
//! che chiudono i run lo raccoglieva:
//!
//!   - `run_reaper` esclude `awaiting_confirmation` PER CONTRATTO (mig 0392): e'
//!     uno stato resumibile via checkpoint, ucciderlo distruggerebbe il lavoro;
//!   - `ACTIVE_RUN_STATUSES` lo conta fra i run che OCCUPANO la sessione, quindi
//!     i run successivi vengono rifiutati.
//!
//! Un run notturno con due validatori in disaccordo restava quindi appeso per
//! sempre e ingorgava la sessione. Al mattino non c'era un esito da leggere: non
//! c'era niente.
//!
//! LA DOMANDA E' UNA SOLA, e non e' «chi ha sospeso»: e' «chi e' atteso». Le
//! DUE sospensioni HITL ordinarie nascono solo dove un umano c'e' (`should_suspend_for_hitl`
//! e `should_suspend_for_plan_approval` pretendono entrambe Confirm), mentre il
//! gate duale e' l'unico che sospende dove non c'e' nessuno. Il discriminante e'
//! quindi la MODALITA', delegata al punto unico gia' esistente
//! [`super::hitl::automation_requires_hitl`], non l'origine: un criterio scritto
//! sull'origine coprirebbe il solo caso di oggi e lascerebbe scoperta la
//! prossima sospensione che nascesse in Automatic — che avrebbe identico difetto.
//!
//! L'origine resta [`SuspensionOrigin`] perche' risponde a un'ALTRA domanda, e
//! serve dopo: quale `blocker` dichiarare quando la scadenza matura. Derivarlo
//! dall'origine (mai riscriverlo nei call site) tiene una sola verita' su cosa
//! ha fermato il run.
//!
//! Cio' che questo modulo NON fa: non chiude nulla e non legge il DB. Dice se
//! una sospensione ha un termine e quale; chi la fa maturare e' `run_reaper`,
//! chi porta i fatti (residuo del run, modalita') e' mcp-core.
//!
//! PERIMETRO, dichiarato perche' non si deduca dal silenzio: la scadenza la
//! ricevono i run di SESSIONE, sui due percorsi che li sospendono. Un SUB-RUN
//! che si sospendesse sul gate duale non la riceve, e non e' una dimenticanza:
//! ha gia' un tetto proprio (il `tokio::time::timeout` esterno sul suo
//! `timeout_s`) e non ingorga la sessione, perche' il gate anti-concorrenza lo
//! esclude per costruzione (`nexus_agent_type IS DISTINCT FROM 'subagent'`).
//! Resta un residuo noto e minore: la sua riga `agent_runs` puo' restare
//! `awaiting_confirmation`, quindi il suo checkpoint non viene potato.

use super::hitl::automation_requires_hitl;
use crate::state::AutomationMode;

/// Chiave `extra` con cui il nodo che sospende DICHIARA la propria origine.
///
/// Dichiarata e non dedotta (regola Q): l'alternativa era leggere la presenza
/// di [`super::step_gate::STEP_GATE_VERDICTS_EXTRA_KEY`], cioe' usare come
/// firma un dato che esiste per un altro scopo — e che resta nell'`extra`
/// checkpointato anche dopo che quella sospensione e' stata sciolta. Una
/// chiave riscritta a ogni sospensione non puo' diventare un fossile.
pub const SUSPENSION_ORIGIN_EXTRA_KEY: &str = "suspension_origin";

/// CHI ha sospeso il run — e quindi, a scadenza maturata, che cosa dichiarare
/// come causa. Vocabolario canonico inglese (regola N), persistito in
/// `agent_runs.suspension_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuspensionOrigin {
    /// Sospensione HITL ordinaria: tool mutativi in Confirm, o approvazione del
    /// piano (W2). Nasce SOLO dove un umano e' al terminale.
    HumanReview,
    /// Gate duale sui passi critici (W3, mig 0677): i due validatori non sono
    /// arrivati a un'unanimita' e la decisione passa all'umano — anche in
    /// Automatic, dove nessuno la raccogliera'.
    StepGate,
}

impl SuspensionOrigin {
    /// Stringa persistita in `agent_runs.suspension_kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HumanReview => "human_review",
            Self::StepGate => "step_gate",
        }
    }

    /// Lettura dalla colonna. `None` = valore ignoto o assente: NON degrada a
    /// una delle due varianti (regola Q — l'ignoto non si traveste da caso
    /// noto), e chi legge decide cosa farne.
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s.trim() {
            "human_review" => Some(Self::HumanReview),
            "step_gate" => Some(Self::StepGate),
            _ => None,
        }
    }

    /// `blocker` ADR 0034 da dichiarare quando la sospensione scade senza che
    /// nessuno l'abbia sciolta. Appartiene al vocabolario chiuso
    /// [`super::meta_reason::VALID_BLOCKERS`] — verificato dal test
    /// `blocker_dal_vocabolario_chiuso`, cosi' una voce rimossa di la' non puo'
    /// sopravvivere qui.
    ///
    /// `safety` per il gate duale: cio' che ha fermato il run e' il disaccordo
    /// su un passo pericoloso, non un permesso mancante. `permission` per la
    /// revisione umana: mancava un consenso che nessuno ha dato.
    pub fn blocker(self) -> &'static str {
        match self {
            Self::HumanReview => "permission",
            Self::StepGate => "safety",
        }
    }

    /// Perche' il run e' fermo, in una riga per l'umano che lo legge dopo.
    /// Composto DAI campi in un punto solo (regola Q): il reaper scrive il
    /// messaggio di chat da qui, non lo compone a mano.
    pub fn motivo(self) -> &'static str {
        match self {
            Self::HumanReview => {
                "Il run attendeva la conferma di un'azione e nessuno l'ha data."
            }
            // NON dice «i due validatori hanno dissentito»: il gate manda
            // all'umano anche quando due valutazioni indipendenti non sono
            // state POSSIBILI — un solo provider utilizzabile su un
            // Irreversible e' gia' `NeedsHuman`, ed e' il caso frequente
            // quando i fornitori sono in cooldown o senza credito. Affermare un
            // disaccordo che non c'e' stato manderebbe a cercare la causa
            // sbagliata; la causa vera (astensione e suo motivo) e' nel
            // meta_step `step_validation` del run, che la porta in campi.
            Self::StepGate => {
                "Il run si e' fermato su un passo classificato critico: non e' stata \
                 raggiunta l'approvazione concorde di due valutatori indipendenti, quindi \
                 la decisione spettava a una persona. Nessuno l'ha presa. Il dettaglio dei \
                 validatori — chi ha risposto, chi si e' astenuto e perche' — e' nel passo \
                 di validazione registrato nel run."
            }
        }
    }
}

/// Messaggio con cui una sospensione scaduta viene chiusa, per la chat.
///
/// Sta qui e non nel reaper perche' e' il testo del VOCABOLARIO: la stessa
/// ragione per cui il `blocker` si deriva dal kind invece di essere riscritto
/// a valle. `origin: None` = kind della riga illeggibile: si dichiara cio' che
/// si sa (la scadenza) senza inventare una causa.
pub fn nota_scadenza(origin: Option<SuspensionOrigin>) -> String {
    let causa = origin
        .map(SuspensionOrigin::motivo)
        .unwrap_or("Il run era sospeso in attesa di un intervento umano che non e' arrivato.");
    format!(
        "{causa}\n\nLa sospensione e' scaduta e il run e' stato chiuso come BLOCCATO: \
         nessun lavoro e' stato eseguito oltre il punto di stop, e la sessione e' \
         di nuovo libera. Per procedere, invia un nuovo messaggio dicendo come \
         vuoi che il passo venga affrontato."
    )
}

/// L'esito della domanda: la sospensione ha un termine, oppure resta finche'
/// l'umano non arriva.
///
/// Due varianti e non un `Option<i64>`: `None` direbbe «nessuna scadenza» e
/// «non ho potuto calcolarla» con lo stesso silenzio, e la seconda e' proprio
/// il caso che ha prodotto il difetto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspensionWatch {
    /// Qualcuno e' atteso al terminale (o la sorveglianza e' spenta
    /// dall'admin): la sospensione resta, come e' sempre stato.
    Indefinite,
    /// Nessuno la sciogliera': scade fra `after_s` secondi. `0` = gia' scaduta
    /// (il run aveva finito il proprio tempo).
    Expiring { after_s: i64 },
}

impl SuspensionWatch {
    /// Secondi al termine, se un termine c'e'. Per il chiamante che deve
    /// scrivere `suspension_expires_at` (NULL quando `Indefinite`).
    pub fn after_s(self) -> Option<i64> {
        match self {
            Self::Indefinite => None,
            Self::Expiring { after_s } => Some(after_s),
        }
    }
}

/// Decide se una sospensione ha una scadenza, e quale.
///
/// - `mode`: modalita' del run. Se attende conferme umane per contratto
///   (Confirm/None), la sospensione NON scade: l'utente e' al terminale, ed e'
///   il caso in cui una scadenza chiuderebbe un run che stava per essere
///   approvato.
/// - `remaining_run_s`: residuo della deadline di run, quando esiste
///   (`run_time_remaining_s`). Fa da TETTO: una sospensione non puo'
///   sopravvivere al run che la contiene.
/// - `fallback_s`: sorveglianza dedicata dal DB
///   (`orchestrator.suspension_watch_timeout_s`). `<= 0` = sorveglianza SPENTA
///   (kill-switch dichiarato della migrazione): tutto torna al comportamento
///   storico.
///
/// Perche' `fallback_s` esiste e non basta il residuo del run: `agent.run_time_budget_s`
/// vale `0` per policy dichiarata (mig 0604, confermata dalla 0607 e MISURATA
/// sul DB vivo il 05/08/2026), quindi per un run primario `remaining_run_s` e'
/// `None` — ed e' esattamente il run notturno del difetto. Una regola che
/// dipendesse dal solo budget residuo non produrrebbe MAI un termine proprio
/// nel caso per cui nasce: reale nel codice, irraggiungibile nei dati.
pub fn classify_suspension(
    mode: Option<AutomationMode>,
    remaining_run_s: Option<i64>,
    fallback_s: i64,
) -> SuspensionWatch {
    if automation_requires_hitl(mode) {
        return SuspensionWatch::Indefinite;
    }
    if fallback_s <= 0 {
        return SuspensionWatch::Indefinite;
    }
    let after_s = match remaining_run_s {
        Some(residuo) => residuo.min(fallback_s),
        None => fallback_s,
    };
    SuspensionWatch::Expiring {
        after_s: after_s.max(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: i64 = 1800;

    /// IL TEST CHE CONTA: la sospensione del gate duale in Automatic ha una
    /// scadenza. Mutazione che lo rosseggia: far ritornare `Indefinite` a
    /// prescindere (il difetto A4 originale) — cioe' il comportamento che
    /// lasciava il run notturno appeso per sempre.
    #[test]
    fn in_automatic_la_sospensione_scade() {
        assert_eq!(
            classify_suspension(Some(AutomationMode::Automatic), None, TIMEOUT),
            SuspensionWatch::Expiring { after_s: TIMEOUT },
            "in Automatic nessuno arriva a sciogliere la sospensione: senza \
             scadenza il run resta appeso e ACTIVE_RUN_STATUSES ingorga la sessione"
        );
    }

    /// L'altro lato, ed e' il danno opposto: in Confirm l'utente E' al
    /// terminale, e una scadenza chiuderebbe un run che stava per approvare.
    /// Mutazione: togliere la guardia `automation_requires_hitl` -> rosso.
    #[test]
    fn in_confirm_la_sospensione_non_scade_mai() {
        for mode in [Some(AutomationMode::Confirm), None] {
            assert_eq!(
                classify_suspension(mode, None, TIMEOUT),
                SuspensionWatch::Indefinite,
                "in {mode:?} la conferma umana e' il contratto: la sospensione attende"
            );
        }
    }

    /// Il residuo del run fa da TETTO: una sospensione non sopravvive al run
    /// che la contiene (e' il caso dei SUB-RUN, che un budget ce l'hanno).
    #[test]
    fn il_residuo_del_run_fa_da_tetto() {
        assert_eq!(
            classify_suspension(Some(AutomationMode::Automatic), Some(120), TIMEOUT),
            SuspensionWatch::Expiring { after_s: 120 },
            "col residuo piu' stretto del timeout dedicato vince il residuo"
        );
        assert_eq!(
            classify_suspension(Some(AutomationMode::Automatic), Some(9_000), TIMEOUT),
            SuspensionWatch::Expiring { after_s: TIMEOUT },
            "col residuo piu' largo la sorveglianza dedicata resta il termine"
        );
    }

    /// Residuo gia' esaurito: la sospensione e' gia' scaduta, non "quasi".
    /// Mutazione: togliere il `.max(0)` -> un `after_s` negativo, che scritto
    /// come `NOW() + interval` darebbe una scadenza NEL PASSATO travestita da
    /// futura.
    #[test]
    fn residuo_esaurito_scade_subito_mai_negativo() {
        assert_eq!(
            classify_suspension(Some(AutomationMode::Automatic), Some(-500), TIMEOUT),
            SuspensionWatch::Expiring { after_s: 0 }
        );
    }

    /// Kill-switch dell'admin (setting a 0): tutto torna al comportamento
    /// storico, in OGNI modalita'.
    #[test]
    fn sorveglianza_spenta_e_un_kill_switch() {
        assert_eq!(
            classify_suspension(Some(AutomationMode::Automatic), Some(120), 0),
            SuspensionWatch::Indefinite,
            "timeout <= 0 = sorveglianza spenta: nessuna scadenza, nemmeno dal residuo"
        );
    }

    /// Il vocabolario dei blocker non e' una stringa scritta qui: e' quello
    /// chiuso dell'ADR 0034. Se una voce sparisse di la', questo test lo dice
    /// invece di lasciare un run chiuso con un blocker che nessuno riconosce.
    #[test]
    fn blocker_dal_vocabolario_chiuso() {
        use super::super::meta_reason::VALID_BLOCKERS;
        for origin in [SuspensionOrigin::HumanReview, SuspensionOrigin::StepGate] {
            assert!(
                VALID_BLOCKERS.contains(&origin.blocker()),
                "il blocker {} di {origin:?} non e' nel vocabolario ADR 0034",
                origin.blocker()
            );
        }
        assert_eq!(SuspensionOrigin::StepGate.blocker(), "safety");
    }

    /// Il vocabolario sul WIRE (serde, dentro `AgentRunResult`) e quello della
    /// COLONNA (`as_str`) sono la stessa lista: se divergessero, un run
    /// serializzato e riletto cambierebbe origine per strada. Mutazione:
    /// togliere `rename_all = "snake_case"` -> rosso.
    #[test]
    fn serde_e_colonna_dicono_la_stessa_parola() {
        for origin in [SuspensionOrigin::HumanReview, SuspensionOrigin::StepGate] {
            let wire = serde_json::to_string(&origin).expect("serializza");
            assert_eq!(
                wire,
                format!("\"{}\"", origin.as_str()),
                "il wire e la colonna devono dire la stessa parola"
            );
        }
    }

    /// Il vocabolario persistito e' un giro chiuso: cio' che si scrive si
    /// rilegge. L'ignoto resta ignoto (nessun degrado silenzioso).
    #[test]
    fn kind_persistito_e_un_giro_chiuso() {
        for origin in [SuspensionOrigin::HumanReview, SuspensionOrigin::StepGate] {
            assert_eq!(SuspensionOrigin::from_db_str(origin.as_str()), Some(origin));
        }
        assert_eq!(SuspensionOrigin::from_db_str("qualcosa_altro"), None);
        assert_eq!(SuspensionOrigin::from_db_str(""), None);
    }
}

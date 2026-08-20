//! Punto unico (regola L): «esiste una superficie di dialogo per QUESTO run —
//! cioe' c'e' qualcuno che vedra' una domanda e potra' rispondere?»
//!
//! La domanda non era ponibile, e i nodi che ne avevano bisogno rispondevano a
//! un'ALTRA: «l'automation_mode e' autonomo?». Sono due cose diverse e la
//! confusione fra loro ha un costo misurato.
//!
//! ## Il fatto (18/08/2026, progetto `app-libri-18-08`, run `abdbc7c4`)
//!
//! Il Consiglio convoca sei figure con mandato BYTE-IDENTICO (md5
//! `77b60652...` per tutte e sei). Sette producono un parere fra 183 e 4000
//! caratteri; `ui_ux_designer` chiude in 2,6 secondi con `status='completed'`,
//! `iterations=0`, `tokens_prompt=0`, `final_summary=''`, `cost=0.000000`, e
//! nessun errore registrato. Zero `agent_steps`, zero `nexus_agent_meta_steps`:
//! l'executor non e' mai stato raggiunto. La conseguenza si vede nel prodotto —
//! l'app esce con una UI in Arial e una tabella bordata, perche' la figura che
//! doveva occuparsene non ha detto niente.
//!
//! La catena, dal log della stessa finestra (T00:29:44-50, otto chiamate clarify
//! in tre secondi):
//!
//! 1. ogni sub-run entra in [`crate::nodes::ClarifyOrExpandNode`] con
//!    `intent_hint: None` (per costruzione: il dispatcher lo dichiara «diretto»)
//!    e `automation_mode='automatic'`;
//! 2. il setting `clarify.confirm_irreversible_in_auto=true` accende
//!    `force_classify`, che DISARMA sia il gate di autonomia sia quello di
//!    confidence: la chiamata LLM parte sempre;
//! 3. la decisione torna `mode=ask`. Per CINQUE figure su sei e' `Technical` +
//!    reversibile, il gate residuo la assorbe e il run prosegue (executor entro
//!    5 ms, misurato). Per `ui_ux_designer` e' `category=Product`, che NON viene
//!    assorbita: il nodo emette `pending_clarify=true`;
//! 4. l'edge del grafo instrada `pending_clarify` a `End` come stato TERMINALE:
//!    il run chiude «completed» prima che l'executor entri in scena, per porre
//!    una domanda a un interlocutore che in un sub-run non esiste.
//!
//! Non c'e' NULLA di specifico in `ui_ux_designer`: il nodo legge solo l'ultimo
//! messaggio umano, provider e model della chiamata li sceglie il purpose
//! `clarify_expand` (le otto chiamate hanno `prompt_tokens` 430 identico), e la
//! vittima la sceglie una moneta lanciata dal modello. Il 17/08, sullo stesso
//! difetto, era toccato a un sub-run `review` (firma identica a 3,7 ms). Un
//! caso speciale sul kind sarebbe stato la toppa (regola H): la classe e'
//! «figura che decide di chiedere in un contesto senza interlocutore».
//!
//! ## Perche' il criterio e' la SUPERFICIE e non il MODO
//!
//! Le due domande sono ortogonali e vanno tenute separate:
//!
//! - **«c'e' qualcuno a cui chiedere?»** — questo modulo. Per un run di chat la
//!   risposta e' si' in QUALUNQUE modalita': il turno si ferma, la domanda
//!   compare in chat e il messaggio successivo dell'utente apre un run nuovo.
//!   Per un sub-run la risposta e' no, e lo e' STRUTTURALMENTE: il suo prodotto
//!   e' un `tool_result` letto da un altro agente, nessun umano lo legge e non
//!   esiste un canale per rispondergli.
//! - **«visto che qualcuno c'e', vale la pena disturbarlo?»** — quella e' la
//!   modalita', e ha gia' il suo punto unico in
//!   [`crate::decisions::hitl::automation_requires_hitl`] piu' la policy
//!   `clarify.confirm_irreversible_in_auto`, che esiste APPOSTA per intercettare
//!   le decisioni irreversibili in automatico.
//!
//! Fonderle avrebbe spento quella policy in silenzio: e' il rimedio che sembra
//! ovvio («in automatico non si chiede, regola D») e che toglie al run di chat
//! la sola rete sulle azioni difficili da annullare, senza toccare il fatto che
//! un sub-run non ha nessuno a cui chiedere. Il difetto misurato non e' che si
//! chieda in automatico: e' che si chieda a NESSUNO.
//!
//! ## I fatti da cui si decide
//!
//! `subagent_depth` e `parent_run_id`, entrambi valorizzati all'origine dal
//! dispatcher (`subagent_native::prepare_subagent_run`: `parent_run_id:
//! Some(anchor)`, `subagent_depth: Some(current_chain_depth + 1)`, quindi >= 1).
//! Basta UNO dei due: sono scritti insieme, e pretenderli entrambi renderebbe
//! il criterio muto per un chiamante che ne popolasse uno solo — l'errore
//! cadrebbe dalla parte di far morire il run, che e' il difetto misurato.
//!
//! NON e' `prodotto_del_run`: quello dice se la figura produce il LAVORO o un
//! PARERE, ed e' un'altra domanda. Una figura `implement` produce il lavoro e
//! non ha comunque nessuno a cui chiedere.
//!
//! ## Portata deliberata: la CHIAMATA resta
//!
//! Il criterio governa la CONSEGUENZA della decisione clarify, non il fatto che
//! la decisione si prenda. Il nodo continua a interrogare l'LLM anche in un
//! sub-run (misurato: otto chiamate in tre secondi, 7715 token, tutte su groq,
//! e la nona ha ricevuto il 429 causato dalle otto precedenti). Restano:
//!
//! - il ramo `expand`, che non e' implicato nel difetto e resta consumabile;
//! - il `suggested_default`, che senza la chiamata non esisterebbe e senza il
//!   quale il taglio degraderebbe a «ignora la domanda».
//!
//! La meta' dannosa di quel costo — la saturazione del fornitore — ha gia' il
//! proprio punto unico altrove (`provider_inflight`, `capienza_tpm`): non e'
//! una domanda a cui questo modulo debba rispondere una seconda volta.

use crate::state::AgentState;

/// Chi puo' rispondere a una domanda posta da questo run.
///
/// `Umano` e' il [`Default`] deliberato: un run che non dichiara di essere un
/// sub-run e' il run di chat, dove la superficie di dialogo esiste e il
/// comportamento non cambia. L'assenza di dichiarazione non deve mai STRINGERE
/// il comportamento di chi i campi non li popola — stessa disciplina di
/// [`crate::decisions::prodotto_del_run::ProdottoDelRun`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Interlocutore {
    /// Esiste una superficie in cui la domanda viene mostrata e a cui si puo'
    /// rispondere: la chat della sessione. Il turno puo' fermarsi in attesa.
    #[default]
    Umano,
    /// Nessuno: il prodotto di questo run e' un `tool_result` letto da un altro
    /// agente. Una domanda posta qui non raggiunge nessuno e non ricevera' mai
    /// risposta; fermarsi per porla non e' un'attesa, e' una morte.
    Nessuno,
}

/// Identificatore canonico (regola N, inglese come gli altri motivi di skip che
/// finiscono nei payload) del motivo per cui la domanda non e' ponibile.
pub const MOTIVO_NESSUN_INTERLOCUTORE: &str = "no_dialogue_surface";

impl Interlocutore {
    /// Il criterio, dai due fatti che il dispatcher scrive all'origine.
    ///
    /// Puro: nessuna lettura di stato globale, nessun I/O. Basta uno dei due
    /// segnali (vedi la doc del modulo per il perche' non sono congiunti).
    pub fn del_run(subagent_depth: Option<i64>, parent_run_id: Option<&str>) -> Self {
        let annidato = subagent_depth.unwrap_or(0) > 0;
        let ha_padre = parent_run_id.is_some_and(|p| !p.trim().is_empty());
        if annidato || ha_padre {
            Self::Nessuno
        } else {
            Self::Umano
        }
    }

    /// Lo stesso criterio letto dallo stato del grafo (l'unico punto in cui i due
    /// campi si prendono: due letture darebbero due idee di «sub-run»).
    pub fn dello_stato(state: &AgentState) -> Self {
        Self::del_run(state.subagent_depth, state.parent_run_id.as_deref())
    }

    /// Questo run puo' fermarsi per porre una domanda?
    ///
    /// E' l'unica conseguenza che il criterio autorizza: chi risponde `false`
    /// non emette `pending_clarify` e prosegue dichiarando l'assunzione
    /// (regola D — in autonomia si procede, non si tace).
    pub fn puo_porre_una_domanda(self) -> bool {
        matches!(self, Self::Umano)
    }

    /// Identificatore canonico (regola N): e' cio' che attraversa un wire
    /// quando il criterio non puo' essere calcolato dov'e' consumato.
    ///
    /// Serve alla spec del criterio del piano di verifica, che gira DENTRO il
    /// final gate: li' lo stato del grafo non c'e', quindi il fatto lo inietta
    /// il nodo (`piano_di_verifica::con_piano`) e lo rilegge il runner. Nessun
    /// secondo criterio: attraversa il valore, non la sua derivazione.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Umano => "human",
            Self::Nessuno => "none",
        }
    }

    /// L'inversa di [`Self::as_str`]. Un valore ASSENTE o fuori vocabolario
    /// vale [`Self::Umano`], che e' il [`Default`] gia' dichiarato: chi non
    /// popola il campo — un produttore anteriore a questo contratto — non deve
    /// vedersi STRINGERE il comportamento, e la conseguenza qui e' solo QUALE
    /// causa il referto dichiara, mai se un comando parta.
    pub fn parse(v: Option<&str>) -> Self {
        match v.map(str::trim) {
            Some(s) if s == Self::Nessuno.as_str() => Self::Nessuno,
            _ => Self::Umano,
        }
    }

    /// Motivo da persistire accanto all'assunzione applicata, quando la domanda
    /// non e' ponibile.
    ///
    /// `None` per `Umano`: li' non c'e' nulla da spiegare, e un motivo scritto
    /// dove non c'e' stata alcuna deviazione non si distinguerebbe da una che e'
    /// avvenuta (regola Q).
    pub fn motivo_assenza(self) -> Option<&'static str> {
        match self {
            Self::Umano => None,
            Self::Nessuno => Some(MOTIVO_NESSUN_INTERLOCUTORE),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Il criterio, nelle due direzioni.
    ///
    /// MUTAZIONE: far ritornare sempre `Umano` — il difetto reale, cioe' il
    /// criterio assente — fa rosseggiare le tre asserzioni sul sub-run, che sono
    /// il caso di `ui_ux_designer`.
    #[test]
    fn un_sub_run_non_ha_nessuno_a_cui_chiedere() {
        assert_eq!(
            Interlocutore::del_run(None, None),
            Interlocutore::Umano,
            "run di chat: la domanda compare in chat e l'utente puo' rispondere"
        );
        assert_eq!(
            Interlocutore::del_run(Some(0), None),
            Interlocutore::Umano,
            "profondita' zero e' il run principale, non un sub-run"
        );
        assert_eq!(
            Interlocutore::del_run(Some(1), None),
            Interlocutore::Nessuno,
            "figura convocata: il suo prodotto e' un tool_result"
        );
        assert_eq!(
            Interlocutore::del_run(None, Some("abdbc7c4-66c3-4dcb-8dec-b37bcaf0a916")),
            Interlocutore::Nessuno,
            "basta il padre: i due segnali si scrivono insieme"
        );
        assert_eq!(
            Interlocutore::del_run(Some(2), Some("abdbc7c4")),
            Interlocutore::Nessuno
        );
    }

    /// Un `parent_run_id` presente ma VUOTO non e' un padre: la stringa vuota e'
    /// il modo in cui un campo non popolato attraversa un wire testuale, e
    /// trattarla come dichiarazione zittirebbe un run di chat.
    #[test]
    fn il_padre_vuoto_non_e_un_padre() {
        assert_eq!(Interlocutore::del_run(None, Some("")), Interlocutore::Umano);
        assert_eq!(
            Interlocutore::del_run(None, Some("   ")),
            Interlocutore::Umano
        );
    }

    /// La conseguenza autorizzata, e il motivo che l'accompagna.
    #[test]
    fn solo_chi_ha_un_interlocutore_puo_chiedere() {
        assert!(Interlocutore::Umano.puo_porre_una_domanda());
        assert!(!Interlocutore::Nessuno.puo_porre_una_domanda());
        assert_eq!(Interlocutore::Umano.motivo_assenza(), None);
        assert_eq!(
            Interlocutore::Nessuno.motivo_assenza(),
            Some(MOTIVO_NESSUN_INTERLOCUTORE)
        );
    }

    /// Il default non stringe nessuno: chi non dichiara si comporta come prima.
    #[test]
    fn il_default_e_l_umano() {
        assert_eq!(Interlocutore::default(), Interlocutore::Umano);
        assert!(Interlocutore::default().puo_porre_una_domanda());
    }

    /// Il valore attraversa un wire e torna indietro identico, e cio' che non
    /// riconosce cade sul default — mai su `Nessuno`, che e' la variante che
    /// toglie a un run la possibilita' di fermarsi a chiedere.
    #[test]
    fn il_giro_sul_wire_conserva_il_criterio() {
        for i in [Interlocutore::Umano, Interlocutore::Nessuno] {
            assert_eq!(Interlocutore::parse(Some(i.as_str())), i);
        }
        assert_eq!(Interlocutore::parse(None), Interlocutore::Umano);
        assert_eq!(Interlocutore::parse(Some("")), Interlocutore::Umano);
        assert_eq!(Interlocutore::parse(Some("chissa")), Interlocutore::Umano);
        assert_eq!(Interlocutore::parse(Some(" none ")), Interlocutore::Nessuno);
    }

    /// Il criterio letto dallo STATO e' lo stesso letto dai campi: se divergesse,
    /// il nodo deciderebbe su una nozione di sub-run diversa da quella provata.
    #[test]
    fn dallo_stato_e_dai_campi_e_lo_stesso_criterio() {
        let mut s = AgentState::default();
        assert_eq!(Interlocutore::dello_stato(&s), Interlocutore::Umano);
        s.subagent_depth = Some(1);
        s.parent_run_id = Some("abdbc7c4".to_string());
        assert_eq!(Interlocutore::dello_stato(&s), Interlocutore::Nessuno);
        assert_eq!(
            Interlocutore::dello_stato(&s),
            Interlocutore::del_run(s.subagent_depth, s.parent_run_id.as_deref())
        );
    }
}

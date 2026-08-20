//! PUNTO UNICO (regola L) della domanda: **come si e' chiuso questo turno del
//! modello — e in particolare, l'abbiamo TAGLIATO noi?**
//!
//! ## Il difetto (MISURATO il 19/08/2026, progetto `t4-prove-consiglio`)
//!
//! `estrai_verdetto` leggeva l'esito dai CAMPI della tool-call e non dalla prosa
//! (regola M rispettata), ma davanti a una risposta senza tool-call utilizzabile
//! aveva UNA sola causa da dichiarare: `schema_mismatch`. Sono due fatti
//! OPPOSTI, e collassarli fa danno perche' le conseguenze sono opposte:
//!
//! - **forma sbagliata**: quel modello non regge lo schema STRICT del verdetto.
//!   E' un fatto su QUELLA coppia, la natura e' `Strutturale`, e riconvocarla
//!   dara' lo stesso esito — quindi si annota come giudice inadatto;
//! - **troncamento**: il modello stava rispondendo bene e la risposta e' finita
//!   contro il TETTO DI OUTPUT che abbiamo dichiarato NOI. Il modello e' sano, e
//!   la causa e' nostra.
//!
//! Sullo stesso run `deepseek/deepseek-v4-flash` ha risposto ai batch da 1-2
//! passi con 57 e 74 token di completion e verdetti validi, e al batch da 25
//! prove con `completion_tokens` **512 ESATTI** — il tetto calcolato per quella
//! coppia. Tool-call incompleta -> `schema_mismatch` -> natura `Strutturale` ->
//! [`crate::decisions::step_gate::NaturaAstensione::richiede_un_altro_giudice`]
//! -> la coppia finisce nel registro dei giudici inadatti per un'ora
//! (`orchestrator.step_validator_inadatto_ttl_s = 3600`). Dal ledger: dalle
//! 23:05 in poi quel modello non riceve piu' una chiamata di taglia-giudice, e
//! le convocazioni successive hanno UN SOLO giudice. **Un modello sano
//! squalificato per un'ora a causa di un nostro parametro.**
//!
//! ## Il segnale e' STRUTTURATO (regola M)
//!
//! Il troncamento NON si riconosce da una sottostringa del contenuto: il campo
//! esiste ed e' [`crate::runtime::ports::LlmResponse::stop_reason`], che il
//! gateway riempie per ogni fornitore.
//!
//! Il vocabolario e' CHIUSO alla fonte, e i due valori qui sotto sono le due
//! forme in cui lo stesso fatto puo' arrivare:
//!
//! - `length` e' il valore di WIRE del gateway. Tutti e tre gli adapter vi
//!   convergono: `openai_compat::normalize_finish_reason` (`"length" =>
//!   "length"`, tutto il resto collassa a `stop`), `google::map_finish_reason`
//!   (`"MAX_TOKENS" => "length"`), `anthropic::map_stop_reason` (`"max_tokens"
//!   => "length"`);
//! - `max_tokens` e' il valore di PORTA, prodotto da
//!   `mcp-core::agent_graph_adapter::llm_gateway::normalize_gw_finish_reason`
//!   (`"length" => "max_tokens"`), ed e' quello che i consumatori dentro il
//!   grafo vedono davvero.
//!
//! Il criterio li accetta ENTRAMBI, e il confronto e' case-insensitive. Non e'
//! prudenza generica: `normalize_gw_finish_reason` fa PASSTHROUGH dei valori che
//! non conosce, quindi un percorso che saltasse una delle due normalizzazioni
//! consegnerebbe qui il valore grezzo del fornitore (`MAX_TOKENS` di Google).
//! La POLARITA' dell'elenco e' scelta: un falso positivo costa una
//! riconvocazione della stessa coppia che si astiene di nuovo — limitata e
//! visibile — mentre un falso negativo e' il difetto misurato, cioe' un modello
//! sano squalificato per un'ora.
//!
//! ## Il ponte con i produttori (regola O)
//!
//! I due normalizzatori del gateway sono privati dei loro moduli e
//! `nexus-agent-graph` non puo' chiamarli: l'elenco qui e' percio' una
//! DICHIARAZIONE, non una derivazione. Il ponte che la tiene onesta vive dove il
//! valore nasce davvero — `mcp-core`, test
//! `il_valore_del_gateway_e_riconosciuto_come_troncamento` — e attraversa
//! `normalize_gw_finish_reason` invece di riscriverne la mappa.

/// Il valore di WIRE con cui il gateway dichiara una risposta tagliata dal tetto
/// di output. Ci convergono tutti e tre gli adapter di fornitore.
pub const FINE_TURNO_WIRE_TRONCATO: &str = "length";

/// Il valore di PORTA corrispondente, prodotto dalla normalizzazione di
/// mcp-core: e' quello che i consumatori dentro il grafo vedono.
pub const FINE_TURNO_PORTA_TRONCATO: &str = "max_tokens";

/// Come si e' chiuso il turno, per la sola domanda che ha una conseguenza:
/// **l'abbiamo tagliato noi?**
///
/// TRE varianti e non un `bool`, perche' l'assenza del segnale non e' un «no»
/// (regola Q): un turno di cui non sappiamo come si e' chiuso e uno che si e'
/// chiuso da se' portano a decisioni diverse, e collassarli rimetterebbe in
/// piedi il difetto in forma piu' educata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FineTurno {
    /// Il fornitore dichiara di aver smesso contro il tetto di output.
    Troncato,
    /// Il fornitore dichiara una chiusura sua (fine turno, tool call, filtro
    /// contenuti): qualunque cosa sia, non e' il nostro tetto.
    Concluso,
    /// Nessun `stop_reason`: la risposta non e' passata da un produttore che lo
    /// dichiara, oppure e' stata ricostruita. NON e' un troncamento — non si
    /// inventa una causa nostra su un fatto mai osservato.
    NonDichiarata,
}

impl FineTurno {
    /// Identificatore canonico (regola N) per log e payload.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Troncato => "truncated",
            Self::Concluso => "completed",
            Self::NonDichiarata => "not_declared",
        }
    }

    /// L'unica conseguenza che i consumatori derivano da qui: la risposta e'
    /// finita contro un tetto che abbiamo dichiarato NOI?
    pub fn tagliata_dal_nostro_tetto(self) -> bool {
        matches!(self, Self::Troncato)
    }
}

/// IL CRITERIO, puro: dal `stop_reason` di una risposta al fatto.
///
/// Il confronto e' case-insensitive ASCII e sul valore TRIMMATO: le due
/// varianti di maiuscole sono le due che i fornitori usano davvero
/// (`max_tokens` di Anthropic, `MAX_TOKENS` di Google), e nessuna delle due deve
/// dipendere da quante normalizzazioni ha attraversato.
pub fn fine_turno(stop_reason: Option<&str>) -> FineTurno {
    let Some(s) = stop_reason.map(str::trim).filter(|s| !s.is_empty()) else {
        return FineTurno::NonDichiarata;
    };
    if [FINE_TURNO_PORTA_TRONCATO, FINE_TURNO_WIRE_TRONCATO]
        .iter()
        .any(|v| s.eq_ignore_ascii_case(v))
    {
        return FineTurno::Troncato;
    }
    FineTurno::Concluso
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn il_valore_di_porta_e_quello_di_wire_dicono_lo_stesso_fatto() {
        assert_eq!(fine_turno(Some("max_tokens")), FineTurno::Troncato);
        assert_eq!(fine_turno(Some("length")), FineTurno::Troncato);
    }

    /// Il grezzo di Google: arriva qui solo se un percorso salta una
    /// normalizzazione, ed e' esattamente il caso che il confronto
    /// case-insensitive copre.
    #[test]
    fn il_grezzo_del_fornitore_e_riconosciuto_lo_stesso() {
        assert_eq!(fine_turno(Some("MAX_TOKENS")), FineTurno::Troncato);
        assert_eq!(fine_turno(Some(" Length ")), FineTurno::Troncato);
    }

    #[test]
    fn una_chiusura_del_modello_non_e_un_troncamento() {
        for v in ["end_turn", "stop", "tool_use", "tool_calls", "content_filter"] {
            assert_eq!(fine_turno(Some(v)), FineTurno::Concluso, "{v}");
            assert!(!fine_turno(Some(v)).tagliata_dal_nostro_tetto(), "{v}");
        }
    }

    /// MUTAZIONE: far degradare l'assenza a `Concluso` non si vedrebbe da un
    /// `bool`; con tre varianti la differenza e' asserita.
    #[test]
    fn il_segnale_assente_non_e_ne_troncato_ne_concluso() {
        assert_eq!(fine_turno(None), FineTurno::NonDichiarata);
        assert_eq!(fine_turno(Some("   ")), FineTurno::NonDichiarata);
        assert!(!fine_turno(None).tagliata_dal_nostro_tetto());
    }
}

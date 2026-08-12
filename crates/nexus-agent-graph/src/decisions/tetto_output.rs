//! Punto unico (regola L) di «quale tetto di output posso imporre a questa
//! coppia (fornitore, modello) senza tagliare via la risposta?».
//!
//! INVERTE IL CONTRATTO dei chiamanti, e l'inversione E' il fix. Prima ognuno
//! dichiarava il TOTALE — «hai 1024 token» — che per un modello il cui pensiero
//! e' obbligatorio significa «pensa per 1024 e non dire niente». Ora il chiamante
//! dichiara solo cio' che deve VEDERE (la tool-call di un verdetto sta in ~256
//! token) e il margine per il ragionamento lo calcola questo modulo, dai fatti
//! che il catalogo gia' possiede.
//!
//! MISURATO il 12/08/2026. `step_validation.rs` mandava `max_tokens: Some(1024)`
//! come letterale, uguale per qualunque modello, mentre il purpose
//! `step_validator` seleziona apposta modelli con `required_capability =
//! 'reasoning'`. Per il dialetto Kimi quel tetto viaggia su
//! `max_completion_tokens`, che per contratto limita output visibile E
//! ragionamento INSIEME, e su quel fornitore il pensiero non si spegne. Esito:
//! il modello consuma il tetto pensando, risponde HTTP 200 con `content` vuoto e
//! `finish_reason = length`, e il gateway lo classifica — correttamente —
//! `empty_completion`.
//!
//! La prova sta nel ledger e non ammette repliche: TUTTE le 15 righe
//! `degenerate_hollow` di due giorni hanno `completion_tokens` ESATTAMENTE 1024,
//! su tre fornitori diversi (kimi 8, openrouter 7). Non e' una coincidenza
//! statistica: e' il soffitto. Costo: 15.360 token di output FATTURATI per zero
//! verdetti.
//!
//! IL DANNO PEGGIORE E' SECONDARIO. `empty_completion` viene conteggiato come
//! degrado del MODELLO e al terzo colpo scatta l'auto-disable: il 12/08 sono
//! stati spenti `kimi-k2.6` (16:56) e `kimi-k3` (16:58), mentre l'ultimo probe
//! sano di k2.6 era del 09/08. Il sistema stava disabilitando fornitori per
//! colpa di un proprio parametro.
//!
//! CHE LA STRADA GIUSTA SIA QUESTA lo dice il sistema stesso: il
//! `model_health_probe` interroga i modelli SENZA `max_tokens`, e la sua doc
//! dichiara il perche' — «max_tokens generosi per evitare falsi positivi su
//! modelli thinking-only». Quella conoscenza viveva in un worker solo e non era
//! delegabile: due risposte alla stessa domanda, una per soffitto.
//!
//! CONFINE: qui SOLO il criterio puro. I fatti (le colonne di
//! `v_model_capabilities`) li raccoglie mcp-core, che vede il DB.

use serde::{Deserialize, Serialize};

/// Cosa il catalogo dichiara di una coppia (fornitore, modello), per questa
/// domanda soltanto.
///
/// Ogni campo e' `Option` perche' l'assenza e' un'informazione: un catalogo che
/// non dichiara nulla su un modello non autorizza a inventargli un tetto
/// (regola Q). `Default` = nessuna dichiarazione.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FattiTetto {
    /// Il modello produce un ragionamento che consuma budget di output?
    ///
    /// `None` = non dichiarato, ed e' il caso PRUDENTE: si tratta come se
    /// ragionasse. Un `false` sbagliato costa un vuoto fatturato piu'
    /// l'auto-disable del fornitore; un `true` sbagliato costa un tetto piu'
    /// alto di quanto serviva, che nessuno paga finche' il modello non lo usa.
    pub ragiona: Option<bool>,
    /// Il tetto che il catalogo considera normale per questo modello.
    pub default_output: Option<u32>,
    /// Il massimo che il FORNITORE accetta: oltre, e' un HTTP 400.
    pub massimo_fornitore: Option<u32>,
}

/// Il tetto da mandare, oppure la dichiarazione che non se ne puo' mandare uno.
///
/// E' un TIPO e non un numero perche' i casi sono tre e uno di essi non e' un
/// numero: «non vincolare» e' una decisione, e uno `0` la renderebbe
/// indistinguibile da «tetto nullo» — cioe' dal difetto di partenza in forma
/// estrema (regola Q).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TettoOutput {
    /// Manda `totale`. `visibile` resta dichiarato per chi legge il rilievo: e'
    /// la parte che il chiamante ha davvero chiesto.
    Dichiarato { visibile: u32, totale: u32 },
    /// NON mandare alcun tetto: il modello ragiona e nessuno ha dichiarato
    /// quanto gli serve. E' la strategia gia' adottata dal `model_health_probe`,
    /// e l'unica che non produce un vuoto fatturato.
    NonVincolabile { motivo: &'static str },
}

impl TettoOutput {
    /// Il valore da mettere in `max_tokens`. `None` = non mandare il campo.
    pub fn max_tokens(&self) -> Option<u32> {
        match self {
            Self::Dichiarato { totale, .. } => Some(*totale),
            Self::NonVincolabile { .. } => None,
        }
    }
}

/// Quanto spazio lasciare al ragionamento, in multipli del visibile.
///
/// Non e' una stima della lunghezza del pensiero — non e' dichiarata da nessuno
/// e varia col compito. E' il rapporto sotto il quale il TAGLIO diventa il caso
/// normale: col fattore 1 (cioe' il comportamento di prima) i modelli misurati
/// riempivano il tetto e non dicevano nulla, e i verdetti riusciti stavano fra
/// 826 e 1003 token di completion contro 256 di verdetto atteso — circa 4x.
pub const MARGINE_RAGIONAMENTO: u32 = 8;

/// Il criterio.
///
/// `visibile` e' cio' che il chiamante deve poter LEGGERE (una tool-call di
/// verdetto, un titolo, un riassunto). Il ragionamento non e' cosa sua e non
/// deve conoscerlo.
pub fn tetto_per(visibile: u32, fatti: &FattiTetto) -> TettoOutput {
    let visibile = visibile.max(1);
    // Il fornitore ha l'ultima parola in ogni caso: oltre il suo massimo la
    // richiesta e' un 400, e un tetto che fa fallire la chiamata e' peggio di
    // un tetto stretto.
    let limita = |v: u32| match fatti.massimo_fornitore {
        Some(h) if h > 0 => v.min(h),
        _ => v,
    };

    // Non ragiona (DICHIARATO): il visibile e' tutto cio' che serve, con un
    // margine minimo per la chiusura del messaggio.
    if fatti.ragiona == Some(false) {
        let totale = limita(visibile.saturating_mul(2));
        return TettoOutput::Dichiarato { visibile, totale };
    }

    // Ragiona, o non lo dichiara. Se il catalogo dice quanto output regge
    // normalmente, quello E' la risposta: e' il numero che il fornitore stesso
    // considera di lavoro.
    match fatti.default_output {
        Some(d) if d > 0 => {
            let totale = limita(d.max(visibile.saturating_mul(MARGINE_RAGIONAMENTO)));
            TettoOutput::Dichiarato { visibile, totale }
        }
        // Nessun default: il pensiero c'e' (o potrebbe esserci) e nessuno ha
        // dichiarato quanto gli serve. Un numero inventato qui e' esattamente il
        // difetto che questo modulo esiste per chiudere.
        _ => TettoOutput::NonVincolabile {
            motivo: "il modello ragiona e il catalogo non dichiara quanto output regge",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// IL CASO MISURATO. Kimi: pensiero obbligatorio, default di catalogo 8192.
    /// Col contratto vecchio il chiamante mandava 1024 e riceveva il vuoto.
    ///
    /// MUTAZIONE: far ritornare `Dichiarato{totale: visibile}` per il ramo che
    /// ragiona -> questo test cade col valore del difetto reale (1024 quando
    /// servivano 8192).
    #[test]
    fn un_modello_che_ragiona_riceve_lo_spazio_del_catalogo() {
        let fatti = FattiTetto {
            ragiona: Some(true),
            default_output: Some(8192),
            massimo_fornitore: Some(16384),
        };
        let t = tetto_per(256, &fatti);
        assert_eq!(t.max_tokens(), Some(8192));
        assert!(
            t.max_tokens().unwrap() > 1024,
            "1024 e' il soffitto che produceva le 15 righe degeneri"
        );
    }

    /// Il caso che ha prodotto l'auto-disable: nessun default dichiarato e
    /// pensiero non spegnibile. Non si inventa un numero, non si manda nulla.
    #[test]
    fn senza_default_dichiarato_non_si_manda_un_tetto() {
        let fatti = FattiTetto {
            ragiona: Some(true),
            ..Default::default()
        };
        assert_eq!(tetto_per(256, &fatti).max_tokens(), None);
    }

    /// L'ASSENZA di dichiarazione si comporta come il ragionamento, non come la
    /// sua assenza: `thinking = false` era misurabilmente FALSO su tutti e
    /// quattro i modelli kimi, e il caso prudente e' quello che non produce un
    /// vuoto fatturato.
    #[test]
    fn l_ignoto_non_degrada_a_non_ragiona() {
        let ignoto = FattiTetto::default();
        assert_eq!(tetto_per(256, &ignoto).max_tokens(), None);
        let ignoto_con_default = FattiTetto {
            default_output: Some(4096),
            ..Default::default()
        };
        assert_eq!(tetto_per(256, &ignoto_con_default).max_tokens(), Some(4096));
    }

    /// Un modello che NON ragiona non si porta dietro il margine: il visibile
    /// (piu' la chiusura) basta, e alzare il tetto per tutti sarebbe pagare il
    /// caso peggiore su ogni chiamata.
    #[test]
    fn chi_non_ragiona_resta_stretto() {
        let fatti = FattiTetto {
            ragiona: Some(false),
            default_output: Some(8192),
            massimo_fornitore: None,
        };
        assert_eq!(tetto_per(256, &fatti).max_tokens(), Some(512));
    }

    /// Il massimo del fornitore vince sempre: oltre e' un HTTP 400, e una
    /// chiamata rifiutata e' peggio di una risposta corta.
    #[test]
    fn il_massimo_del_fornitore_ha_l_ultima_parola() {
        let fatti = FattiTetto {
            ragiona: Some(true),
            default_output: Some(32000),
            massimo_fornitore: Some(4096),
        };
        assert_eq!(tetto_per(256, &fatti).max_tokens(), Some(4096));
        // Vale anche per chi non ragiona.
        let stretto = FattiTetto {
            ragiona: Some(false),
            default_output: None,
            massimo_fornitore: Some(300),
        };
        assert_eq!(tetto_per(256, &stretto).max_tokens(), Some(300));
    }

    /// Un visibile grande porta con se' il proprio margine anche quando il
    /// default del catalogo e' modesto: il chiamante che chiede 2000 token di
    /// risposta non puo' riceverne 2048 in tutto se il modello ragiona.
    #[test]
    fn il_margine_segue_il_visibile_richiesto() {
        let fatti = FattiTetto {
            ragiona: Some(true),
            default_output: Some(2048),
            massimo_fornitore: None,
        };
        assert_eq!(
            tetto_per(2000, &fatti).max_tokens(),
            Some(2000 * MARGINE_RAGIONAMENTO)
        );
    }
}

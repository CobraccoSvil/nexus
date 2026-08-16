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

/// Cosa un chiamante CHIEDE, quando chiede spazio di output.
///
/// E' un TIPO e non un `u32` perche' le due domande sono diverse e un numero
/// nudo non le distingue: «devo poter leggere 512 token» e «manda il tetto 512»
/// si scrivono identiche e significano l'opposto su un modello che ragiona.
/// MISURATO il 13/08/2026 su `groq/openai/gpt-oss-20b` col prompt VERO del
/// supervisore (template `automation.supervisor_monitoring` dal DB): col tetto
/// 512 la risposta e' `finish_reason=length`, `completion_tokens` ESATTAMENTE
/// 512, 2314 caratteri di ragionamento e un JSON troncato a meta' stringa;
/// senza tetto e' `finish_reason=stop`, 348 token e il JSON intero. Stessa
/// firma delle 15 righe `degenerate_hollow` da 1024 token esatti che hanno
/// fatto nascere questo modulo — su un altro fornitore e con un altro numero.
///
/// Finche' il parametro e' un `u32` chiamato `max_tokens`, il difetto e'
/// SEMPRE riscrivibile: il tipo e' il solo posto in cui la distinzione non si
/// puo' dimenticare (regola Q, il contratto e' la firma).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RichiestaOutput {
    /// «Devo poter LEGGERE questo». Il margine per il ragionamento NON e' cosa
    /// del chiamante: lo calcola [`tetto_per`] dai fatti del catalogo.
    Visibile(u32),
    /// «Manda ESATTAMENTE questo tetto, non chiedere al catalogo».
    ///
    /// Esiste per chi MISURA il modello — il probe e il qualificatore — e non
    /// puo' ereditare dal catalogo i fatti che sta derivando: leggerli li'
    /// significherebbe misurare la propria premessa (regola O). Il `perche` e'
    /// obbligatorio perche' questa variante scavalca il criterio, e uno
    /// scavalco senza motivo scritto e' indistinguibile da una svista.
    TotaleDichiarato {
        totale: u32,
        perche: &'static str,
    },
}

impl RichiestaOutput {
    /// Quanto chiedere, dati i fatti del catalogo. Unico punto in cui la
    /// richiesta diventa un numero: le due varianti non si mescolano altrove.
    pub fn tetto(&self, fatti: &FattiTetto) -> TettoOutput {
        match self {
            Self::Visibile(v) => tetto_per(*v, fatti),
            Self::TotaleDichiarato { totale, .. } => TettoOutput::Dichiarato {
                visibile: *totale,
                totale: *totale,
            },
        }
    }

    /// `true` se il catalogo non va interrogato: chi misura non eredita.
    pub fn scavalca_il_catalogo(&self) -> bool {
        matches!(self, Self::TotaleDichiarato { .. })
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
        // Nessun default curato, ma il FORNITORE dichiara il proprio massimo
        // (dal wire di discovery, mig 0716): il margine e' lo stesso del ramo
        // curato (`visibile * MARGINE_RAGIONAMENTO`), col massimo dichiarato
        // come tetto duro. NON e' un numero inventato: il moltiplicatore e' il
        // criterio gia' misurato (~4x reale, fattore 8) e il limite superiore
        // e' una dichiarazione del fornitore. Senza questo ramo, verso un
        // modello openrouter da discovery non parte alcun `max_tokens` e la
        // PRENOTAZIONE sale al massimo del modello (65536): un 402 su crediti
        // bassi per una richiesta da 512 token visibili.
        _ => match fatti.massimo_fornitore {
            Some(h) if h > 0 => TettoOutput::Dichiarato {
                visibile,
                totale: visibile.saturating_mul(MARGINE_RAGIONAMENTO).min(h),
            },
            // Nessun fatto: il pensiero c'e' (o potrebbe esserci) e nessuno ha
            // dichiarato quanto gli serve. Un numero inventato qui e'
            // esattamente il difetto che questo modulo esiste per chiudere.
            _ => TettoOutput::NonVincolabile {
                motivo: "il modello ragiona e il catalogo non dichiara quanto output regge",
            },
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

    /// IL CASO MISURATO IL 13/08/2026, nella forma in cui il chiamante lo
    /// produceva: `SUPERVISOR_MAX_TOKENS = 512` come TOTALE, su un modello che
    /// ragiona e che il catalogo non dichiara affatto (groq non ha nemmeno una
    /// riga nella vista).
    ///
    /// Le due richieste hanno lo stesso numero e devono dare esiti OPPOSTI:
    /// chiedere «512 di totale» e' cio' che ha prodotto il turno vuoto
    /// fatturato; chiedere «512 di visibile» su fatti ignoti non manda alcun
    /// tetto, che e' l'unico esito che quel turno lo evita.
    ///
    /// MUTAZIONE: far ritornare a `RichiestaOutput::Visibile` lo stesso tetto
    /// del `TotaleDichiarato` (cioe' reintrodurre il letterale) -> questo test
    /// cade col valore del difetto reale, 512.
    #[test]
    fn lo_stesso_numero_significa_l_opposto_nelle_due_richieste() {
        let ignoto = FattiTetto::default();
        assert_eq!(
            RichiestaOutput::Visibile(512).tetto(&ignoto).max_tokens(),
            None,
            "512 da LEGGERE su un modello non dichiarato: nessun tetto, o il \
             ragionamento se lo mangia e il turno esce vuoto e fatturato"
        );
        assert_eq!(
            RichiestaOutput::TotaleDichiarato {
                totale: 512,
                perche: "misura",
            }
            .tetto(&ignoto)
            .max_tokens(),
            Some(512),
            "chi MISURA riceve esattamente cio' che ha dichiarato"
        );
    }

    /// Chi misura non eredita: il catalogo non va nemmeno interrogato, o si
    /// misurerebbe la propria premessa (regola O).
    #[test]
    fn solo_il_totale_dichiarato_scavalca_il_catalogo() {
        assert!(!RichiestaOutput::Visibile(256).scavalca_il_catalogo());
        assert!(RichiestaOutput::TotaleDichiarato {
            totale: 256,
            perche: "il probe non puo' leggere cio' che sta derivando",
        }
        .scavalca_il_catalogo());
    }

    /// Il `TotaleDichiarato` ignora i fatti anche quando ci SONO: e' il punto
    /// della variante. Se li leggesse, il qualificatore proverebbe il modello
    /// col tetto che il catalogo gia' gli attribuisce.
    #[test]
    fn il_totale_dichiarato_non_guarda_i_fatti() {
        let ricchi = FattiTetto {
            ragiona: Some(true),
            default_output: Some(8192),
            massimo_fornitore: Some(16384),
        };
        assert_eq!(
            RichiestaOutput::TotaleDichiarato {
                totale: 256,
                perche: "misura",
            }
            .tetto(&ricchi)
            .max_tokens(),
            Some(256)
        );
        // La gemella, sugli STESSI fatti, delega al criterio.
        assert_eq!(
            RichiestaOutput::Visibile(256).tetto(&ricchi).max_tokens(),
            Some(8192)
        );
    }

    /// IL RAMO NUOVO (mig 0716): nessun default curato, ma il FORNITORE
    /// dichiara il proprio massimo nel listing di discovery. E' la condizione
    /// dei modelli openrouter/google scoperti a runtime: prima usciva
    /// `NonVincolabile` e verso quei modelli non partiva alcun `max_tokens`,
    /// con la prenotazione al massimo del modello.
    ///
    /// Il margine e' lo stesso del ramo curato (`visibile * 8`), col massimo
    /// dichiarato come tetto duro; quando il dichiarato e' PIU' stretto del
    /// margine, vince il dichiarato (oltre e' un 400/402).
    ///
    /// MUTAZIONE: rimuovere il ramo (tornare a `NonVincolabile` su default
    /// assente) -> entrambi gli assert cadono con `None`.
    #[test]
    fn il_massimo_dichiarato_dal_fornitore_basta_a_vincolare() {
        let dal_wire = FattiTetto {
            ragiona: None,
            default_output: None,
            massimo_fornitore: Some(65_536),
        };
        assert_eq!(
            tetto_per(512, &dal_wire).max_tokens(),
            Some(512 * MARGINE_RAGIONAMENTO),
            "il margine del ragionamento sotto il tetto dichiarato"
        );
        let stretto = FattiTetto {
            ragiona: None,
            default_output: None,
            massimo_fornitore: Some(1000),
        };
        assert_eq!(
            tetto_per(512, &stretto).max_tokens(),
            Some(1000),
            "il dichiarato del fornitore ha l'ultima parola anche qui"
        );
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

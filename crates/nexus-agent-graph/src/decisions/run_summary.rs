//! `run_summary`: PUNTO UNICO (regola L) di «qual e' il riassunto di questo run?».
//!
//! La domanda se la pongono i DUE finalizzatori — la chiusura di un SUB-run
//! (`mcp-core::agent_tools::subagent_native::finalize_success`, che scrive
//! `nexus_subagent_runs.final_summary` e la gemella `agent_runs.final_answer`) e
//! quella del run PRINCIPALE (`mcp-core::chat_messages::agent_run::
//! native_outcome_to_run_result`) — e la risposta era data in due modi diversi:
//!
//! - il run principale aveva gia' il ripiego, ma per UN SOLO finalizzatore
//!   (`declared_outcome.summary`, cioe' `task_complete`);
//! - il sub-run non ne aveva nessuno: prendeva il testo libero di chiusura e
//!   basta (`o.final_answer.unwrap_or_default()`).
//!
//! Il testo libero non e' il prodotto di una figura di verdetto. Gli schemi di
//! `advisory_verdict`, `review_verdict`, `debate_position` e `task_complete`
//! dichiarano `summary` come campo OBBLIGATORIO, e le rispettive descrizioni
//! ordinano di chiamare il tool «come ULTIMISSIMA azione»: una figura che
//! obbedisce chiude con la sola tool_use, senza prosa in piu'. Il suo parere
//! esisteva, era strutturato e obbligatorio — e il riassunto restava vuoto.
//!
//! MISURATO il 08/08/2026 sui tre DB-progetto vivi (gestione_corsi_nexus,
//! agenda_medica_nexus, biblioteca_scolastica_nexus), 148 sub-run storici:
//!
//! | finalizzatore     | run | riassunto vuoto | ...con `summary` dichiarato |
//! |-------------------|----:|----------------:|----------------------------:|
//! | `advisory_verdict`|  75 |         23 (31%)|                          23 |
//! | `review_verdict`  |  19 |          3 (16%)|                           3 |
//! | `task_complete`   |  17 |          4 (24%)|                           4 |
//! | `debate_position` |   6 |               0 |                           0 |
//! | nessuno (timeout) |  31 |               3 |                           0 |
//!
//! 30 riassunti vuoti su 30 avevano il campo `summary` compilato: il lavoro
//! c'era, in un campo che nessun lettore mostrava. La causa NON e' «il tool
//! sbagliato»: nessuno dei quattro alimentava il riassunto, e `task_complete`
//! perdeva un run su quattro esattamente come gli altri. E' l'assenza del punto
//! unico, non una differenza fra i tool.
//!
//! Funzione PURA: nessun I/O, replay-stabile. Legge il campo STRUTTURATO
//! `summary` dei blocchi gia' normalizzati da [`super::tool_dispatch`], mai la
//! prosa della conversazione (regola M).

use serde_json::Value;

/// Il tool con cui il run ha dichiarato il proprio esito.
///
/// Vocabolario canonico in inglese (regola N): i nomi che questo enum espone
/// sono GLI STESSI che il dispatcher confronta col `name` della tool_use — le
/// costanti di [`crate::nodes::tool_dispatch`] li derivano da qui invece di
/// ripetere il letterale, cosi' un rinominamento non puo' spaiare il
/// riconoscimento dalla attribuzione del riassunto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finalizzatore {
    /// `review_verdict`: verdetto strutturato del revisore.
    Review,
    /// `advisory_verdict`: parere strutturato di una figura del consiglio.
    Advisory,
    /// `debate_position`: posizione strutturata di un avvocato del dibattito.
    Debate,
    /// `task_complete`: esito finale generico del run (ADR 0034).
    TaskComplete,
}

impl Finalizzatore {
    /// Nome canonico del tool (regola N).
    pub const fn nome_tool(self) -> &'static str {
        match self {
            Self::Review => "review_verdict",
            Self::Advisory => "advisory_verdict",
            Self::Debate => "debate_position",
            Self::TaskComplete => "task_complete",
        }
    }
}

/// ORDINE di consultazione dei blocchi dichiarati quando il testo libero manca.
///
/// I tre finalizzatori di RUOLO precedono `task_complete` perche' in quei run il
/// verdetto E' il prodotto: un revisore che dichiarasse entrambi avrebbe scritto
/// la review in `review_verdict` e una chiusura generica in `task_complete`, e a
/// chi legge serve la prima.
///
/// Oggi la precedenza non e' osservabile — MISURATO sui tre DB vivi: nessun run
/// ha piu' di un blocco valorizzato (0 o 1 su 148, mai 2). Resta dichiarata
/// esplicitamente perche' l'alternativa e' un ordine implicito, deciso dalla
/// sequenza degli `or_else` e diverso a ogni riscrittura.
const ORDINE: [Finalizzatore; 4] = [
    Finalizzatore::Review,
    Finalizzatore::Advisory,
    Finalizzatore::Debate,
    Finalizzatore::TaskComplete,
];

/// Le fonti da cui il riassunto puo' venire, per NOME e non per posizione: i
/// quattro blocchi hanno lo stesso tipo (`Option<&Value>`) e una firma
/// posizionale renderebbe uno scambio fra due di essi invisibile al compilatore
/// e ai test (tutti portano un campo `summary`).
///
/// Costruita dal punto unico `NativeRunOutcome::fonti_riassunto` in mcp-core:
/// nessun chiamante assembla queste quattro fonti a mano (regola O — una fonte
/// ricomposta a mano non misura il run, misura la sua imitazione).
#[derive(Debug, Default, Clone, Copy)]
pub struct FontiRiassunto<'a> {
    /// Testo libero di chiusura prodotto dal modello (campo `result` dello stato
    /// del grafo a fine run). Vuoto o soli spazi = assente.
    pub testo_libero: Option<&'a str>,
    /// Blocco normalizzato di `review_verdict`.
    pub review: Option<&'a Value>,
    /// Blocco normalizzato di `advisory_verdict`.
    pub advisory: Option<&'a Value>,
    /// Blocco normalizzato di `debate_position`.
    pub debate: Option<&'a Value>,
    /// Blocco normalizzato di `task_complete` (ADR 0034).
    pub declared: Option<&'a Value>,
}

impl<'a> FontiRiassunto<'a> {
    /// Il blocco di un dato finalizzatore, se presente e non `null`.
    fn blocco(&self, f: Finalizzatore) -> Option<&'a Value> {
        let v = match f {
            Finalizzatore::Review => self.review,
            Finalizzatore::Advisory => self.advisory,
            Finalizzatore::Debate => self.debate,
            Finalizzatore::TaskComplete => self.declared,
        };
        v.filter(|v| !v.is_null())
    }
}

/// Il riassunto di un run e la sua PROVENIENZA.
///
/// Enum con payload e non `{ testo: Option<String>, fonte: ... }`: cosi'
/// «assente» e «presente» non possono contraddirsi, e nessuna variante puo'
/// portare una stringa vuota (regola Q — l'ignoto e' una variante, non un valore
/// comodo).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiassuntoRun {
    /// Il modello ha chiuso con del testo: e' quello il resoconto.
    TestoLibero(String),
    /// Il testo libero mancava; il resoconto viene dal campo `summary`
    /// OBBLIGATORIO del finalizzatore dichiarato.
    Dichiarato {
        testo: String,
        da: Finalizzatore,
    },
    /// Nessuna delle due fonti porta testo: il run non ha lasciato un
    /// riassunto. NON e' una stringa vuota da scrivere come se fosse un
    /// resoconto — e' l'assenza, e chi la riceve decide cosa farne (i rami
    /// terminali senza dichiarazione, es. timeout, stanno qui).
    Assente,
}

impl RiassuntoRun {
    /// Il testo, se c'e'. Mai vuoto per costruzione.
    pub fn testo(&self) -> Option<&str> {
        match self {
            Self::TestoLibero(t) => Some(t),
            Self::Dichiarato { testo, .. } => Some(testo),
            Self::Assente => None,
        }
    }

    /// Il finalizzatore da cui viene il testo, `None` se il testo e' del modello
    /// o se non c'e' testo. Serve alla diagnostica: un riassunto derivato e uno
    /// scritto dal modello sono due fatti diversi.
    pub fn derivato_da(&self) -> Option<Finalizzatore> {
        match self {
            Self::Dichiarato { da, .. } => Some(*da),
            _ => None,
        }
    }
}

/// Risolve il riassunto di un run dalle sue fonti.
///
/// PRECEDENZA: il testo libero vince quando c'e'. E' cio' che il modello ha
/// scritto per chiudere, ed e' anche il comportamento gia' in esercizio per i
/// run che un testo lo producono — il ripiego riempie il vuoto, non riscrive
/// cio' che gia' funziona.
///
/// Il testo libero passa INTEGRO (non si tocca cio' che il modello ha composto);
/// il `summary` dichiarato viene trimmato, perche' li' e' un campo di form e gli
/// spazi ai bordi non sono contenuto.
pub fn riassunto_del_run(fonti: FontiRiassunto<'_>) -> RiassuntoRun {
    if let Some(t) = fonti.testo_libero.filter(|t| !t.trim().is_empty()) {
        return RiassuntoRun::TestoLibero(t.to_string());
    }
    for f in ORDINE {
        let Some(blocco) = fonti.blocco(f) else {
            continue;
        };
        let testo = blocco
            .get("summary")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(testo) = testo {
            return RiassuntoRun::Dichiarato {
                testo: testo.to_string(),
                da: f,
            };
        }
    }
    RiassuntoRun::Assente
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decisions::tool_dispatch::{
        normalize_advisory_verdict, normalize_debate_position, normalize_declared_outcome,
        normalize_review_verdict,
    };
    use serde_json::json;

    /// I blocchi dei test nascono dai PRODUTTORI reali (regola O): in produzione
    /// il dict che finisce nello stato passa da queste `normalize_*`, e scriverlo
    /// a mano fisserebbe qui l'assunto che il test dovrebbe verificare — se un
    /// normalizzatore smettesse di emettere `summary`, un test con JSON scritto a
    /// mano resterebbe verde.
    fn advisory(summary: &str) -> Value {
        normalize_advisory_verdict(&json!({
            "verdict": "proceed_with_changes",
            "summary": summary,
            "requirements": [{"text": "usa la porta allocata", "direction": "must_be_present"}],
        }))
        .expect("advisory_verdict valido")
    }

    fn review(summary: &str) -> Value {
        normalize_review_verdict(&json!({
            "verdict": "pass",
            "summary": summary,
        }))
        .expect("review_verdict valido")
    }

    fn debate(summary: &str) -> Value {
        normalize_debate_position(&json!({
            "assigned_position": "isolare i sub-run in worktree",
            "stance": "support",
            "summary": summary,
            "key_arguments": ["nessuna race sui file"],
        }))
        .expect("debate_position valida")
    }

    fn declared(summary: &str) -> Value {
        normalize_declared_outcome(&json!({
            "outcome": "done",
            "summary": summary,
        }))
        .expect("task_complete valido")
    }

    #[test]
    fn testo_libero_vince_quando_ce() {
        let a = advisory("parere della figura");
        let r = riassunto_del_run(FontiRiassunto {
            testo_libero: Some("resoconto scritto dal modello"),
            advisory: Some(&a),
            ..Default::default()
        });
        assert_eq!(
            r,
            RiassuntoRun::TestoLibero("resoconto scritto dal modello".into())
        );
        assert_eq!(r.derivato_da(), None);
    }

    /// IL DIFETTO MISURATO: la figura chiude con la sola `advisory_verdict`,
    /// senza prosa. Prima il riassunto era la stringa vuota.
    ///
    /// MUTAZIONE dichiarata: rimuovendo il ciclo su `ORDINE` da
    /// `riassunto_del_run` (cioe' tornando al solo testo libero) questo test
    /// fallisce con `Assente` invece di `Dichiarato`, che e' esattamente il
    /// valore del difetto in produzione.
    #[test]
    fn senza_testo_libero_il_riassunto_viene_dal_parere_dichiarato() {
        let a = advisory("Tailwind e' installato ma non configurato: mancano config e CSS di ingresso.");
        let r = riassunto_del_run(FontiRiassunto {
            testo_libero: None,
            advisory: Some(&a),
            ..Default::default()
        });
        assert_eq!(
            r.testo(),
            Some("Tailwind e' installato ma non configurato: mancano config e CSS di ingresso.")
        );
        assert_eq!(r.derivato_da(), Some(Finalizzatore::Advisory));
    }

    /// Il testo di soli spazi non e' un resoconto: e' il vuoto travestito.
    #[test]
    fn testo_libero_di_soli_spazi_non_conta_come_resoconto() {
        let v = review("nessun difetto reale trovato");
        let r = riassunto_del_run(FontiRiassunto {
            testo_libero: Some("   \n\t "),
            review: Some(&v),
            ..Default::default()
        });
        assert_eq!(r.derivato_da(), Some(Finalizzatore::Review));
        assert_eq!(r.testo(), Some("nessun difetto reale trovato"));
    }

    /// Tutti e quattro i finalizzatori alimentano il riassunto: era proprio la
    /// loro asimmetria (solo `task_complete`, e solo sul run principale) il
    /// difetto.
    #[test]
    fn ogni_finalizzatore_alimenta_il_riassunto() {
        let casi: [(Finalizzatore, Value); 4] = [
            (Finalizzatore::Review, review("esito della review")),
            (Finalizzatore::Advisory, advisory("parere della figura")),
            (Finalizzatore::Debate, debate("arringa dell'avvocato")),
            (Finalizzatore::TaskComplete, declared("resoconto del lavoro")),
        ];
        for (atteso, blocco) in &casi {
            let mut fonti = FontiRiassunto::default();
            match atteso {
                Finalizzatore::Review => fonti.review = Some(blocco),
                Finalizzatore::Advisory => fonti.advisory = Some(blocco),
                Finalizzatore::Debate => fonti.debate = Some(blocco),
                Finalizzatore::TaskComplete => fonti.declared = Some(blocco),
            }
            let r = riassunto_del_run(fonti);
            assert_eq!(
                r.derivato_da(),
                Some(*atteso),
                "{} deve alimentare il riassunto",
                atteso.nome_tool()
            );
            assert!(r.testo().is_some_and(|t| !t.is_empty()));
        }
    }

    /// Senza testo e senza dichiarazione (timeout, errore del motore) il
    /// riassunto e' ASSENTE: non una stringa vuota spacciata per resoconto.
    #[test]
    fn senza_fonti_il_riassunto_e_assente() {
        assert_eq!(
            riassunto_del_run(FontiRiassunto::default()),
            RiassuntoRun::Assente
        );
        assert_eq!(riassunto_del_run(FontiRiassunto::default()).testo(), None);
    }

    /// Un blocco `null` non e' una dichiarazione: `structured_verdict` scrive
    /// `Value::Null` nei campi dei finalizzatori non usati, e leggerne il
    /// `summary` darebbe comunque `None` — ma la variante deve restare `Assente`
    /// e non un `Dichiarato` con testo vuoto.
    #[test]
    fn blocco_null_non_e_una_dichiarazione() {
        let nullo = Value::Null;
        assert_eq!(
            riassunto_del_run(FontiRiassunto {
                advisory: Some(&nullo),
                declared: Some(&nullo),
                ..Default::default()
            }),
            RiassuntoRun::Assente
        );
    }

    /// Un `summary` dichiarato VUOTO (i normalizzatori lo ammettono: mettono
    /// stringa vuota se il campo manca) non produce un riassunto vuoto — si
    /// prosegue coi finalizzatori successivi e, se nessuno parla, si dichiara
    /// l'assenza.
    #[test]
    fn summary_dichiarato_vuoto_non_diventa_un_riassunto() {
        let muto = normalize_declared_outcome(&json!({"outcome": "done"}))
            .expect("task_complete senza summary e' accettato dal normalizzatore");
        assert_eq!(muto.get("summary").and_then(Value::as_str), Some(""));
        assert_eq!(
            riassunto_del_run(FontiRiassunto {
                declared: Some(&muto),
                ..Default::default()
            }),
            RiassuntoRun::Assente
        );
    }

    /// La precedenza dichiarata da [`ORDINE`]: il verdetto di RUOLO e' il
    /// prodotto del run, la chiusura generica e' il ripiego.
    #[test]
    fn il_verdetto_di_ruolo_precede_la_chiusura_generica() {
        let v = review("esito della review");
        let d = declared("chiusura generica");
        let r = riassunto_del_run(FontiRiassunto {
            review: Some(&v),
            declared: Some(&d),
            ..Default::default()
        });
        assert_eq!(r.derivato_da(), Some(Finalizzatore::Review));
    }

    /// Il nome del tool e' UNO: quello che il dispatcher confronta e quello che
    /// il riassunto attribuisce. Il test lega i due lati (regola O: la costante
    /// del dispatcher DERIVA da qui, e questo lo verifica sul consumatore).
    #[test]
    fn i_nomi_dei_tool_sono_quelli_canonici() {
        assert_eq!(Finalizzatore::Review.nome_tool(), "review_verdict");
        assert_eq!(Finalizzatore::Advisory.nome_tool(), "advisory_verdict");
        assert_eq!(Finalizzatore::Debate.nome_tool(), "debate_position");
        assert_eq!(Finalizzatore::TaskComplete.nome_tool(), "task_complete");
    }
}

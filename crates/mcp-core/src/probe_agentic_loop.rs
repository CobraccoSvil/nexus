//! Il LOOP multi-step dei profili `tool_chain` e `tool_recovery`.
//!
//! E' l'unica parte di questi profili che tocca la rete, ed e' sottile apposta: il
//! mondo che risponde sta in [`crate::probe_world`], i fatti che ne derivano in
//! [`crate::probe_chain_measure`]. Qui c'e' solo la meccanica del giro — chiedi,
//! raccogli le tool-call, rispondi, ripeti — perche' cio' che decide dev'essere
//! provabile senza un provider vivo.
//!
//! # Multi-STEP, non multi-turn
//!
//! BFCL distingue: multi-turn e' quando l'utente riparla, multi-step e' quando il
//! modello incatena chiamate dentro lo stesso compito. Qui l'utente parla una volta
//! sola. Non promettiamo cio' che non misuriamo.

use serde_json::{json, Value};

use crate::probe_chain_measure::{firma, misura, AttemptMeasures, TokenEmesso, TracedCall};
use crate::probe_world::{ScriptedWorld, WorldKind, WorldReply};

/// Quanti giri al massimo. Dal payload del profilo, con clamp: e' l'unico campo che
/// governa la durata del giro, e senza tetto un modello che non si ferma mai
/// prosciugherebbe il round della batteria.
const TURNI_MIN: usize = 2;
const TURNI_MAX: usize = 8;

/// L'esito di UN tentativo multi-step.
pub(crate) struct LoopOutcome {
    pub measures: AttemptMeasures,
    /// `Some` = il giro non e' attribuibile al modello (troncato dal nostro cap,
    /// errore di provider): il chiamante lo tratta come INCONCLUSIVO, mai come una
    /// bocciatura.
    pub inconclusive: Option<String>,
    pub turni: usize,
}

/// Cosa serve al loop per chiamare il modello. Un trait perche' il test possa
/// esercitare il giro senza rete: il loop e' meccanica, e la meccanica va provata.
#[allow(async_fn_in_trait)]
pub(crate) trait TurnSource {
    /// Un turno: riceve i messaggi (JSON array) e ritorna il Value del turno nella
    /// forma di `agent_turn_value_from_gw`.
    async fn turn(&self, messages_json: &str) -> Value;
}

/// Il giro completo.
///
/// Ogni iterazione: manda la conversazione, legge `tool_use_blocks`, fa rispondere
/// il mondo, riappende assistant + tool_result. Si ferma quando il modello smette
/// di chiamare tool o quando finiscono i turni.
pub(crate) async fn run_loop(
    fonte: &impl TurnSource,
    kind: WorldKind,
    mondo: &mut ScriptedWorld,
    istruzione: &str,
    max_turns: usize,
) -> LoopOutcome {
    let turni_max = max_turns.clamp(TURNI_MIN, TURNI_MAX);
    let mut messaggi: Vec<Value> = vec![json!({ "role": "user", "content": istruzione })];
    let mut traccia: Vec<TracedCall> = Vec::new();
    let mut emessi: Vec<TokenEmesso> = Vec::new();
    let mut firme_fallite: Vec<String> = Vec::new();
    let mut token_errore: Option<String> = None;
    let mut turno_errore = usize::MAX;

    for turno in 0..turni_max {
        let turn = fonte.turn(&Value::Array(messaggi.clone()).to_string()).await;

        // Un errore di provider non e' un verdetto sul modello: il giro si chiude
        // inconclusivo con la classe che il ponte ha gia' stabilito (regola M).
        if let Some(ec) = turn.get("error_class").and_then(Value::as_str) {
            return LoopOutcome {
                measures: misura(&traccia, &emessi, token_errore.as_deref(), turno_errore, &firme_fallite),
                inconclusive: Some(format!("provider:{ec}")),
                turni: turno,
            };
        }
        // Troncato dal NOSTRO cap di token: misurerebbe il nostro budget, non lui.
        if turn.get("stop_reason").and_then(Value::as_str) == Some("max_tokens") {
            return LoopOutcome {
                measures: misura(&traccia, &emessi, token_errore.as_deref(), turno_errore, &firme_fallite),
                inconclusive: Some("truncated_max_tokens".to_string()),
                turni: turno,
            };
        }

        let blocchi: Vec<Value> = turn
            .get("tool_use_blocks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if blocchi.is_empty() {
            // Ha smesso di usare tool: il giro finisce qui, e i fatti sono quelli
            // raccolti. Non e' un errore — e' una scelta sua, che il predicato
            // giudichera'.
            break;
        }

        // L'assistant va riappeso PRIMA dei tool_result, o la conversazione e'
        // incoerente e il provider la rifiuta.
        messaggi.push(assistant_da_blocchi(&blocchi, turn.get("content").and_then(Value::as_str)));

        let mut stato = StatoGiro {
            traccia: &mut traccia,
            emessi: &mut emessi,
            firme_fallite: &mut firme_fallite,
            token_errore: &mut token_errore,
            turno_errore: &mut turno_errore,
            messaggi: &mut messaggi,
        };
        esegui_blocchi(&blocchi, turno, mondo, &mut stato);
    }

    let _ = kind; // il kind vive nel mondo: qui il giro e' lo stesso per entrambi
    LoopOutcome {
        measures: misura(&traccia, &emessi, token_errore.as_deref(), turno_errore, &firme_fallite),
        inconclusive: None,
        turni: traccia.last().map(|c| c.turno + 1).unwrap_or(0),
    }
}

/// Cio' che un turno accumula. Raggruppato per non far crescere `run_loop` oltre
/// la soglia: il giro e' una cosa, cio' che il giro registra e' un'altra.
struct StatoGiro<'a> {
    traccia: &'a mut Vec<TracedCall>,
    emessi: &'a mut Vec<TokenEmesso>,
    firme_fallite: &'a mut Vec<String>,
    token_errore: &'a mut Option<String>,
    turno_errore: &'a mut usize,
    messaggi: &'a mut Vec<Value>,
}

/// Fa rispondere il mondo a ogni tool-call del turno e registra i fatti.
fn esegui_blocchi(blocchi: &[Value], turno: usize, mondo: &mut ScriptedWorld, s: &mut StatoGiro) {
    for b in blocchi {
        let nome = b.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
        let input = b.get("input").cloned().unwrap_or(json!({}));
        let id = b.get("id").and_then(Value::as_str).unwrap_or("call").to_string();

        let reply: WorldReply = mondo.answer(&nome, &input);
        s.traccia.push(TracedCall {
            turno,
            nome: nome.clone(),
            input: input.clone(),
            // Il produttore collassa gli arguments non parsabili in `{}`: un input
            // vuoto su un tool che ne richiede uno e' il sintomo.
            input_malformato: input.as_object().is_some_and(|o| o.is_empty()),
        });
        if let Some(tok) = &reply.planted {
            s.emessi.push(TokenEmesso { token: tok.clone(), turno });
        }
        if reply.is_error {
            s.firme_fallite.push(firma(&nome, &input));
            if s.token_errore.is_none() {
                if let Some(t) = mondo.token_errore_emesso() {
                    *s.token_errore = Some(t.to_string());
                    *s.turno_errore = turno;
                }
            }
        }
        s.messaggi.push(json!({
            "role": "tool",
            "tool_call_id": id,
            "content": reply.text,
        }));
    }
}

/// Il messaggio assistant da riappendere, ricostruito dai blocchi.
///
/// LIMITE DICHIARATO: il Value del turno espone `tool_use_blocks` ma non i
/// `tool_calls` originali del filo, quindi la `thought_signature` per-call di
/// Gemini 3 non sopravvive al giro e quel provider puo' rifiutare il secondo turno
/// (400 INVALID_ARGUMENT). Si vedra' come `provider:invalid_request` concentrato su
/// google — inconclusivo, non una bocciatura del modello. Il fix e' propagare
/// `tool_calls` nel produttore: e' additivo e va fatto, ma non qui.
fn assistant_da_blocchi(blocchi: &[Value], contenuto: Option<&str>) -> Value {
    let calls: Vec<Value> = blocchi
        .iter()
        .map(|b| {
            json!({
                "id": b.get("id").and_then(Value::as_str).unwrap_or("call"),
                "type": "function",
                "function": {
                    "name": b.get("name").and_then(Value::as_str).unwrap_or_default(),
                    "arguments": b.get("input").cloned().unwrap_or(json!({})).to_string(),
                }
            })
        })
        .collect();
    json!({
        "role": "assistant",
        "content": contenuto.unwrap_or(""),
        "tool_calls": calls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe_world::TokenSeed;
    use std::cell::RefCell;

    fn seme(profilo: &str) -> TokenSeed {
        TokenSeed {
            provider: "p".into(),
            model: "m".into(),
            profile_key: profilo.into(),
            attempt: 1,
            seed: 7,
        }
    }

    /// Un modello finto che gioca una sceneggiatura: a ogni turno emette le
    /// tool-call che gli diciamo. Serve a provare la MECCANICA del giro senza rete.
    struct ModelloScritto {
        /// Per ogni turno: le chiamate (nome, input). Vuoto = smette.
        copione: RefCell<Vec<Vec<(String, Value)>>>,
        /// L'ultima conversazione vista: per verificare che il giro sia coerente.
        vista: RefCell<String>,
    }

    impl ModelloScritto {
        fn new(copione: Vec<Vec<(&str, Value)>>) -> Self {
            Self {
                copione: RefCell::new(
                    copione
                        .into_iter()
                        .map(|t| t.into_iter().map(|(n, i)| (n.to_string(), i)).collect())
                        .collect(),
                ),
                vista: RefCell::new(String::new()),
            }
        }
    }

    /// Il segnaposto che un copione usa per dire "qui il modello LEGGE la risposta
    /// e riporta il token che ha trovato". Serve perche' un modello vero il token
    /// non lo puo' conoscere in anticipo: se il test glielo passasse gia' pronto,
    /// proverebbe che sappiamo scrivere un copione, non che il giro funziona.
    const LEGGI: &str = "@TOKEN_LETTO@";

    /// Il token che l'errore ha piantato in `current_epoch`: e' cio' che un modello
    /// che ha LETTO l'errore avrebbe sotto gli occhi. Si legge dal campo, non a
    /// tentoni nel testo — anche il modello finto rispetta la regola.
    fn token_dalla_conversazione(messages_json: &str) -> Option<String> {
        let coda = messages_json.split("current_epoch").nth(1)?;
        let i = coda.find("E-")?;
        let tok: String = coda[i..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        (tok.len() > 3).then_some(tok)
    }

    impl TurnSource for ModelloScritto {
        async fn turn(&self, messages_json: &str) -> Value {
            *self.vista.borrow_mut() = messages_json.to_string();
            let mut c = self.copione.borrow_mut();
            if c.is_empty() {
                return json!({ "content": "fatto", "tool_use_blocks": [], "stop_reason": "end_turn" });
            }
            let passo = c.remove(0);
            let letto = token_dalla_conversazione(messages_json);
            let blocchi: Vec<Value> = passo
                .iter()
                .enumerate()
                .map(|(i, (n, inp))| {
                    // Il copione dice "leggi": il modello finto prende il token dalla
                    // conversazione, come farebbe uno vero.
                    let inp = match (inp.to_string().contains(LEGGI), &letto) {
                        (true, Some(t)) => {
                            serde_json::from_str(&inp.to_string().replace(LEGGI, t)).unwrap_or(json!({}))
                        }
                        _ => inp.clone(),
                    };
                    json!({ "id": format!("c{i}"), "name": n, "input": inp })
                })
                .collect();
            json!({ "content": "", "tool_use_blocks": blocchi, "stop_reason": "tool_use" })
        }
    }

    /// UN MODELLO CHE CONCATENA: segue gli handle e chiude 3 anelli.
    #[tokio::test]
    async fn un_modello_che_segue_gli_handle_chiude_la_catena() {
        let mut mondo = ScriptedWorld::new(WorldKind::Catena, seme("agentic_chain"), &[]).unwrap();
        let s = seme("agentic_chain");
        let copione = vec![
            // Il primo handle e' nell'istruzione: usarlo NON prova dipendenza (il
            // modello lo copia). Gli anelli si contano dai token che il mondo ha
            // consegnato in risposta, quindi 3 anelli chiedono 4 chiamate.
            vec![("read_file", json!({ "path": s.handle(0) }))],
            vec![("read_file", json!({ "path": s.handle(1) }))],
            vec![("read_file", json!({ "path": s.handle(2) }))],
            vec![("read_file", json!({ "path": s.handle(3) }))],
        ];
        let m = ModelloScritto::new(copione);
        let out = run_loop(&m, WorldKind::Catena, &mut mondo, "istruzione", 6).await;
        assert!(out.inconclusive.is_none());
        assert_eq!(out.measures.chained_links, 3, "tre token consegnati dal mondo e riportati");
    }

    /// UN MODELLO CHE NON CONCATENA: chiama a caso. Il giro finisce senza anelli, e
    /// non e' inconclusivo — e' una bocciatura vera.
    #[tokio::test]
    async fn un_modello_che_chiama_a_caso_non_ottiene_anelli() {
        let mut mondo = ScriptedWorld::new(WorldKind::Catena, seme("agentic_chain"), &[]).unwrap();
        let copione = vec![
            vec![("list_files", json!({ "path": "/tmp" }))],
            vec![("read_file", json!({ "path": "src/main.rs" }))],
            vec![("list_files", json!({ "path": "/etc" }))],
        ];
        let m = ModelloScritto::new(copione);
        let out = run_loop(&m, WorldKind::Catena, &mut mondo, "istruzione", 6).await;
        assert!(out.inconclusive.is_none(), "il modello ha risposto: e' un verdetto vero");
        assert_eq!(out.measures.chained_links, 0);
    }

    /// IL RECUPERO: il primo contatto fallisce, il modello legge il token
    /// dell'errore e lo porta.
    #[tokio::test]
    async fn il_modello_che_legge_l_errore_recupera() {
        let mut mondo = ScriptedWorld::new(WorldKind::Recupero, seme("agentic_recovery"), &[]).unwrap();
        // Il token dell'errore il modello NON puo' conoscerlo in anticipo: il
        // copione dice "leggi", e il modello finto lo prende dalla conversazione —
        // come farebbe uno vero. Passarglielo gia' pronto proverebbe solo che
        // sappiamo scrivere un copione.
        let copione = vec![
            vec![("read_file", json!({ "path": "x" }))],
            vec![("read_file", json!({ "epoch": LEGGI }))],
        ];
        let m = ModelloScritto::new(copione);
        let out = run_loop(&m, WorldKind::Recupero, &mut mondo, "istruzione", 6).await;
        assert!(out.measures.recovered, "ha portato il token che solo l'errore conteneva");
        assert!(!out.measures.repeated_failed);
    }

    /// CHI RIPETE non recupera, e la ripetizione e' un fatto registrato.
    #[tokio::test]
    async fn il_modello_che_ripete_non_recupera() {
        let mut mondo = ScriptedWorld::new(WorldKind::Recupero, seme("agentic_recovery"), &[]).unwrap();
        let copione = vec![
            vec![("read_file", json!({ "path": "x" }))],
            vec![("read_file", json!({ "path": "x" }))],
        ];
        let m = ModelloScritto::new(copione);
        let out = run_loop(&m, WorldKind::Recupero, &mut mondo, "istruzione", 6).await;
        assert!(!out.measures.recovered);
        assert!(out.measures.repeated_failed, "stessa firma di una chiamata fallita");
    }

    /// Il troncamento dal NOSTRO cap non e' colpa del modello.
    #[tokio::test]
    async fn il_troncamento_e_inconclusivo_non_una_bocciatura() {
        struct Troncato;
        impl TurnSource for Troncato {
            async fn turn(&self, _m: &str) -> Value {
                json!({ "content": "", "tool_use_blocks": [], "stop_reason": "max_tokens" })
            }
        }
        let mut mondo = ScriptedWorld::new(WorldKind::Catena, seme("agentic_chain"), &[]).unwrap();
        let out = run_loop(&Troncato, WorldKind::Catena, &mut mondo, "x", 6).await;
        assert_eq!(out.inconclusive.as_deref(), Some("truncated_max_tokens"));
    }

    /// Un errore di provider chiude il giro inconclusivo con la classe gia'
    /// stabilita dal ponte: la batteria non ri-classifica, e non punisce il modello.
    #[tokio::test]
    async fn un_errore_di_provider_non_e_una_bocciatura() {
        struct Rotto;
        impl TurnSource for Rotto {
            async fn turn(&self, _m: &str) -> Value {
                json!({ "error_class": "transient", "stop_reason": "error", "tool_use_blocks": [] })
            }
        }
        let mut mondo = ScriptedWorld::new(WorldKind::Catena, seme("agentic_chain"), &[]).unwrap();
        let out = run_loop(&Rotto, WorldKind::Catena, &mut mondo, "x", 6).await;
        assert_eq!(out.inconclusive.as_deref(), Some("provider:transient"));
    }

    /// La conversazione che mandiamo dev'essere coerente: l'assistant con le
    /// tool_calls PRIMA dei tool_result, o il provider rifiuta il secondo turno.
    #[tokio::test]
    async fn la_conversazione_riappende_assistant_prima_dei_risultati() {
        let mut mondo = ScriptedWorld::new(WorldKind::Catena, seme("agentic_chain"), &[]).unwrap();
        let s = seme("agentic_chain");
        let m = ModelloScritto::new(vec![
            vec![("read_file", json!({ "path": s.handle(0) }))],
            vec![("read_file", json!({ "path": s.handle(1) }))],
        ]);
        run_loop(&m, WorldKind::Catena, &mut mondo, "istruzione", 6).await;
        let vista: Value = serde_json::from_str(&m.vista.borrow()).unwrap();
        let ruoli: Vec<&str> = vista
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.get("role").and_then(Value::as_str).unwrap_or(""))
            .collect();
        assert_eq!(ruoli, vec!["user", "assistant", "tool", "assistant", "tool"]);
    }

    /// Il cap dei turni si rispetta: un modello che non smette mai non prosciuga il
    /// giro della batteria.
    #[tokio::test]
    async fn il_cap_dei_turni_ferma_un_modello_che_non_si_ferma() {
        struct Infinito;
        impl TurnSource for Infinito {
            async fn turn(&self, _m: &str) -> Value {
                json!({ "content": "", "stop_reason": "tool_use",
                        "tool_use_blocks": [{ "id": "c", "name": "list_files", "input": {"path": "/"} }] })
            }
        }
        let mut mondo = ScriptedWorld::new(WorldKind::Catena, seme("agentic_chain"), &[]).unwrap();
        let out = run_loop(&Infinito, WorldKind::Catena, &mut mondo, "x", 3).await;
        assert_eq!(out.turni, 3, "si ferma al cap, non gira per sempre");
    }
}

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
        // Si legge `finish_reason` (il segnale del gateway, normalizzato al
        // vocabolario della porta), NON `stop_reason`: quest'ultimo il produttore lo
        // DERIVA dalla presenza di tool-call e vale solo "tool_use"/"end_turn" — non
        // puo' valere "max_tokens" per costruzione. Finche' il controllo guardava li'
        // era inerte, e un modello troncato dal nostro cap veniva letto come "ha
        // smesso di chiamare tool": una bocciatura al posto di un inconcludente.
        if turn.get("finish_reason").and_then(Value::as_str) == Some("max_tokens") {
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
        messaggi.push(assistant_dal_turno(&turn));

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

/// Il messaggio assistant da riappendere: RIECHEGGIATO dal turno, non ricostruito.
///
/// Il produttore (`agent_turn_value_from_gw`) espone gia' le `tool_calls` nella forma
/// esatta che il gateway riaccetta in richiesta, con la `thought_signature` per-call
/// che Gemini 3 esige di ritorno sulla stessa `functionCall` (HTTP 400
/// INVALID_ARGUMENT se manca). Qui non si ricostruisce nulla: ricostruire dai
/// `tool_use_blocks` sarebbe una SECONDA versione di "com'e' fatto un turno
/// assistant" (regola L) e perderebbe per costruzione cio' che i blocchi non
/// portano — la firma, e gli `arguments` letterali del modello.
fn assistant_dal_turno(turn: &Value) -> Value {
    let mut msg = json!({
        "role": "assistant",
        "content": turn.get("content").and_then(Value::as_str).unwrap_or(""),
    });
    // Verbatim: cio' che il produttore ha messo, esattamente com'e'. `tool_calls`
    // porta dentro di se' la firma per-call; le altre due sono per-messaggio
    // (Anthropic / DeepSeek). Assenti -> omesse, come le vuole il contratto.
    for campo in ["tool_calls", "thinking_signature", "reasoning"] {
        if let Some(v) = turn.get(campo) {
            msg[campo] = v.clone();
        }
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nexus_gateway::{GwResponse, GwToolCall, GwToolFunctionCall, GwUsage};
    use crate::orchestrator::neural_client::agent_turn_value_from_gw;
    use crate::probe_world::TokenSeed;
    use std::cell::RefCell;

    /// Il provider che esige la firma di ritorno, e un modello della sua famiglia:
    /// qui non sono configurazione (regola G), sono la SCENA dell'incidente.
    const PROVIDER: &str = "google";
    const MODELLO: &str = "gemini-3-pro-preview";

    fn seme(profilo: &str) -> TokenSeed {
        TokenSeed {
            provider: "p".into(),
            model: "m".into(),
            profile_key: profilo.into(),
            attempt: 1,
            seed: 7,
        }
    }

    /// Il turno come lo costruisce la PRODUZIONE: una [`GwResponse`] del gateway fatta
    /// passare per `agent_turn_value_from_gw`, l'UNICO produttore di questo Value.
    ///
    /// Fabbricare il turno a mano (`json!({"tool_use_blocks": ...})`) fisserebbe
    /// l'assunto che vogliamo verificare: codice e test condividerebbero l'errore e
    /// resterebbero verdi per sempre — e' cosi' che la chiave inventata `turn["result"]`
    /// e' sopravvissuta (regola O).
    fn turno_dal_gateway(
        content: &str,
        chiamate: &[(String, String)],
        finish_reason: &str,
        firma: Option<&str>,
    ) -> Value {
        let tool_calls: Vec<GwToolCall> = chiamate
            .iter()
            .enumerate()
            .map(|(i, (nome, arguments))| GwToolCall {
                id: format!("c{i}"),
                kind: "function".to_string(),
                function: GwToolFunctionCall {
                    name: nome.clone(),
                    arguments: arguments.clone(),
                },
                // La firma per-call che Gemini 3 emette su OGNI functionCall.
                thought_signature: firma.map(str::to_string),
            })
            .collect();
        let resp = GwResponse {
            content: content.to_string(),
            tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
            usage: GwUsage::default(),
            model_used: MODELLO.to_string(),
            provider_used: PROVIDER.to_string(),
            latency_ms: 12,
            finish_reason: finish_reason.to_string(),
            privacy_rerouted: None,
            reasoning: None,
            thinking_signature: None,
            citations: None,
            ledger: None,
        };
        agent_turn_value_from_gw(PROVIDER, MODELLO, &resp)
    }

    /// Un modello finto che gioca una sceneggiatura: a ogni turno emette le
    /// tool-call che gli diciamo. Serve a provare la MECCANICA del giro senza rete.
    /// Il TURNO pero' non se lo inventa: lo fa produrre al produttore vero.
    struct ModelloScritto {
        /// Per ogni turno: le chiamate (nome, input). Vuoto = smette.
        copione: RefCell<Vec<Vec<(String, Value)>>>,
        /// L'ultima conversazione vista: per verificare che il giro sia coerente.
        vista: RefCell<String>,
        /// La firma di pensiero che il provider appiccica a ogni tool-call.
        firma: Option<String>,
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
                firma: None,
            }
        }

        /// Come [`ModelloScritto::new`], ma il modello firma il proprio pensiero.
        fn con_firma(copione: Vec<Vec<(&str, Value)>>, firma: &str) -> Self {
            Self {
                firma: Some(firma.to_string()),
                ..Self::new(copione)
            }
        }
    }

    /// Il segnaposto che un copione usa per dire "qui il modello LEGGE la risposta
    /// e riporta il token che ha trovato". Serve perche' un modello vero il token
    /// non lo puo' conoscere in anticipo: se il test glielo passasse gia' pronto,
    /// proverebbe che sappiamo scrivere un copione, non che il giro funziona.
    const LEGGI: &str = "@TOKEN_LETTO@";

    /// Il token che l'errore ha piantato in `current_epoch`: e' cio' che un modello
    /// che ha LETTO l'errore avrebbe sotto gli occhi. Si scandiscono TUTTE le
    /// occorrenze del nome campo, non solo la prima: da quando il `message`
    /// dell'errore nomina 'current_epoch' in prosa (com'e' giusto: l'errore dice
    /// cosa fare), la prima occorrenza puo' essere la menzione e non il campo.
    /// Il vecchio `nth(1)` era un tentoni travestito: si e' rotto esattamente
    /// quando l'errore e' diventato piu' realistico.
    fn token_dalla_conversazione(messages_json: &str) -> Option<String> {
        for coda in messages_json.split("current_epoch").skip(1) {
            if let Some(i) = coda.find("E-") {
                let tok: String = coda[i..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect();
                if tok.len() > 3 {
                    return Some(tok);
                }
            }
        }
        None
    }

    impl TurnSource for ModelloScritto {
        async fn turn(&self, messages_json: &str) -> Value {
            *self.vista.borrow_mut() = messages_json.to_string();
            let mut c = self.copione.borrow_mut();
            if c.is_empty() {
                return turno_dal_gateway("fatto", &[], "stop", None);
            }
            let passo = c.remove(0);
            let letto = token_dalla_conversazione(messages_json);
            let chiamate: Vec<(String, String)> = passo
                .iter()
                .map(|(n, inp)| {
                    // Il copione dice "leggi": il modello finto prende il token dalla
                    // conversazione, come farebbe uno vero.
                    let grezzo = inp.to_string();
                    let arguments = match (grezzo.contains(LEGGI), &letto) {
                        (true, Some(t)) => grezzo.replace(LEGGI, t),
                        _ => grezzo,
                    };
                    (n.clone(), arguments)
                })
                .collect();
            // "tool_calls" e' il vocabolario WIRE del gateway per "ha chiamato un
            // tool"; e' il produttore a tradurlo.
            turno_dal_gateway("", &chiamate, "tool_calls", self.firma.as_deref())
        }
    }

    /// UN MODELLO CHE CONCATENA: segue gli handle fino a dove la pista glielo
    /// concede, e ogni token riportato e' un anello.
    ///
    /// Il copione si ferma PRIMA dell'anello cieco (`anello_cieco()`, deciso dal
    /// seme): qui si prova la MECCANICA del giro — il mondo consegna, il modello
    /// riporta, il taint tracking conta — non la capacita' di rientrare da una pista
    /// chiusa, che ha i suoi test dedicati sul giro completo (model_qualification:
    /// `la_traiettoria_intesa_arriva_in_fondo`). Il limite si legge dal seme invece
    /// di essere scritto a mano: se domani l'interruzione si sposta, questo test
    /// segue il mondo invece di rompersi.
    #[tokio::test]
    async fn un_modello_che_segue_gli_handle_chiude_la_catena() {
        let mut mondo = ScriptedWorld::new(WorldKind::Catena, seme("agentic_chain"), &[]).unwrap();
        let s = seme("agentic_chain");
        let ultimo = s.anello_cieco() - 1;
        // Il primo handle e' nell'istruzione: usarlo NON prova dipendenza (il
        // modello lo copia). Gli anelli si contano dai token che il mondo ha
        // consegnato in risposta, quindi N anelli chiedono N+1 chiamate.
        let copione: Vec<Vec<(&str, Value)>> = (0..=ultimo)
            .map(|k| vec![("read_file", json!({ "path": s.handle(k) }))])
            .collect();
        let m = ModelloScritto::new(copione);
        let out = run_loop(&m, WorldKind::Catena, &mut mondo, "istruzione", 6).await;
        assert!(out.inconclusive.is_none());
        assert_eq!(
            out.measures.chained_links, ultimo,
            "tanti anelli quanti token il mondo ha consegnato e il modello ha riportato"
        );
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
    ///
    /// Il turno viene dal produttore con `finish_reason="length"` — cio' che il
    /// gateway dice DAVVERO quando il cap taglia la risposta. Prima questo test
    /// fabbricava `stop_reason:"max_tokens"`, un valore che il produttore non puo'
    /// emettere (lo DERIVA dalle tool-call: solo "tool_use"/"end_turn"): il test era
    /// verde e il controllo nel loop non poteva scattare mai.
    #[tokio::test]
    async fn il_troncamento_e_inconclusivo_non_una_bocciatura() {
        struct Troncato;
        impl TurnSource for Troncato {
            async fn turn(&self, _m: &str) -> Value {
                turno_dal_gateway("", &[], "length", None)
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
                turno_dal_gateway(
                    "",
                    &[("list_files".to_string(), json!({"path": "/"}).to_string())],
                    "tool_calls",
                    None,
                )
            }
        }
        let mut mondo = ScriptedWorld::new(WorldKind::Catena, seme("agentic_chain"), &[]).unwrap();
        let out = run_loop(&Infinito, WorldKind::Catena, &mut mondo, "x", 3).await;
        assert_eq!(out.turni, 3, "si ferma al cap, non gira per sempre");
    }

    /// Una firma opaca come quelle che Gemini 3 emette per-call.
    const FIRMA: &str = "CtcBAVSoXO9dGRkm0Xq2hVLZ4bWnNr1sQqYd8vTgJhPzKcM3Fx";

    /// L'assistant che il loop rimanda indietro, letto DOVE conta: sul wire.
    /// `gw_message_from_value` e' l'ultimo cancello prima del gateway, quindi e' lui
    /// a dire cosa parte davvero (regola O: si asserisce la conseguenza).
    fn assistant_sul_wire(vista: &str) -> crate::nexus_gateway::GwMessage {
        let conversazione: Vec<Value> = serde_json::from_str(vista).unwrap();
        let assistant = conversazione
            .iter()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
            .expect("il giro deve riappendere l'assistant prima dei tool_result")
            .clone();
        crate::orchestrator::neural_client::gw_message_from_value(assistant)
    }

    /// LA FIRMA DI PENSIERO SOPRAVVIVE AL SECONDO TURNO (Gemini 3).
    ///
    /// Gemini 3 esige che la `thought_signature` torni indietro VERBATIM sulla stessa
    /// `functionCall`, pena HTTP 400 INVALID_ARGUMENT: se si perde, google fallisce
    /// ogni profilo multi-step per un difetto NOSTRO e viene declassato a torto.
    ///
    /// Il giro parte dalla `GwResponse` del gateway (produttore reale) e arriva al
    /// `GwMessage` che parte: entrambi gli estremi sono codice di produzione.
    #[tokio::test]
    async fn la_firma_di_pensiero_di_gemini_sopravvive_al_giro() {
        let mut mondo = ScriptedWorld::new(WorldKind::Catena, seme("agentic_chain"), &[]).unwrap();
        let s = seme("agentic_chain");
        let m = ModelloScritto::con_firma(
            vec![
                vec![("read_file", json!({ "path": s.handle(0) }))],
                vec![("read_file", json!({ "path": s.handle(1) }))],
            ],
            FIRMA,
        );
        run_loop(&m, WorldKind::Catena, &mut mondo, "istruzione", 6).await;

        let msg = assistant_sul_wire(&m.vista.borrow());
        let calls = msg
            .tool_calls
            .expect("l'assistant deve ripartire con le sue tool_calls, o la coppia tool_use/tool_result si rompe");
        assert_eq!(
            calls[0].thought_signature.as_deref(),
            Some(FIRMA),
            "la firma di pensiero deve tornare indietro verbatim: senza, google \
             rifiuta il secondo turno con 400 INVALID_ARGUMENT e la batteria \
             declassa il modello per un difetto nostro"
        );
        assert_eq!(calls[0].kind, "function", "il discriminante OpenAI resta");
    }

    /// Gli `arguments` tornano LETTERALI, non ri-serializzati.
    ///
    /// La firma di Gemini copre la functionCall: ricostruirla ri-serializzando
    /// l'input parsato la riscrive (spaziatura, ordine, forma dei numeri) e la firma
    /// non combacia piu'. Qui il modello emette `arguments` spaziati: se tornano
    /// compattati, qualcuno li ha ricostruiti invece di riecheggiarli.
    #[test]
    fn gli_arguments_tornano_letterali_non_ricostruiti() {
        let spaziati = r#"{"path": "alfa", "peso": 1.0}"#.to_string();
        let turno = turno_dal_gateway(
            "",
            &[("read_file".to_string(), spaziati.clone())],
            "tool_calls",
            Some(FIRMA),
        );
        let msg = crate::orchestrator::neural_client::gw_message_from_value(assistant_dal_turno(
            &turno,
        ));
        let calls = msg.tool_calls.expect("tool_calls sul wire");
        assert_eq!(
            calls[0].function.arguments, spaziati,
            "gli arguments devono ripartire byte per byte come il modello li ha \
             emessi: ri-serializzarli invaliderebbe la firma che li copre"
        );
        assert_eq!(calls[0].thought_signature.as_deref(), Some(FIRMA));
    }
}

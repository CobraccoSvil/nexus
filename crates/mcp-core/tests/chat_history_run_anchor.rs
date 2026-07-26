//! Contract test: la storia della chat deve CONSEGNARE l'aggancio al run.
//!
//! Regressione catturata (difetto osservato il 20/07: "la sezione consiglio
//! scompare quando si aggiorna"). `GET /api/chat/sessions/:id/messages`
//! agganciava il run con `LEFT JOIN agent_runs ar ON ar.run_message_id = cm.id`,
//! ma `run_message_id` punta al messaggio UTENTE che ha innescato il run, mai
//! alla risposta. Conseguenza misurata sul DB del progetto: `run_status`
//! presente su 6/6 messaggi utente e 0/6 assistant, e `runId` assente ovunque
//! (il campo veniva letto da `metadata.runId`, chiave che non compare in NESSUN
//! messaggio: 0 su 6).
//!
//! Il frontend apre il nastro attivita' con
//! `if (isUser || !message.runId || ...) return null` (message-list.tsx): senza
//! `runId` l'INTERO nastro del turno spariva al reload -- timeline, tracce e
//! Consiglio delle Competenze inclusi -- pur essendo tutti i meta-step nel DB.
//! Live la card restava visibile perche' il pannello usa `agentRun.runId` dalla
//! memoria, non il messaggio: da qui il "sparisce quando si aggiorna".
//!
//! Perche' al wire e non sul DB (regola O): mcp-core e' bin-only, quindi un test
//! in `tests/` non puo' importare la SELECT di produzione e dovrebbe RICOPIARLA
//! -- misurando una propria imitazione, che divergerebbe al primo ritocco. E il
//! DB del gate (meta) non ha nemmeno `chat_messages`/`agent_runs`, che vivono
//! solo nel DB del progetto. Interrogare l'endpoint pone la domanda al sistema
//! invece di riscriverla.
//!
//! ATTENZIONE allo skip: senza server o JWT questo test si presenta come "ok"
//! nel gate. Per non passare in modo VACUO (una sessione senza turni agentici
//! renderebbe verde qualunque implementazione), quando la storia e' raggiungibile
//! ma non contiene alcun assistant agganciabile il test FALLISCE dichiarandolo.

mod support;

use serde_json::Value;
use support::{base_url, jwt_o_salta, salta, Motivo};

async fn get_json(token: &str, path: &str) -> Option<Value> {
    let res = reqwest::Client::new()
        .get(format!("{}{path}", base_url()))
        .bearer_auth(token)
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        // Uno status di rifiuto NON e' un servizio giu': con
        // REQUIRE_INTEGRATION_TESTS=1 diventa un fallimento, cosi' un 401
        // sistematico non puo' piu' presentarsi come "nessuna sessione con turni".
        salta(Motivo::RispostaInattesa {
            status: res.status().as_u16(),
            path,
        });
        return None;
    }
    res.json::<Value>().await.ok()
}

/// Una sessione con almeno un turno agentico: e' li' che l'aggancio conta.
async fn sessione_con_turni(token: &str) -> Option<String> {
    let body = get_json(token, "/api/chat/sessions").await?;
    let sessions = body
        .get("sessions")
        .and_then(Value::as_array)
        .or_else(|| body.as_array())?;
    for s in sessions {
        let id = s.get("id").and_then(Value::as_str)?;
        let msgs = get_json(token, &format!("/api/chat/sessions/{id}/messages")).await?;
        let n = msgs
            .get("messages")
            .and_then(Value::as_array)
            .map(|m| m.iter().filter(|x| ruolo(x) == "assistant").count())
            .unwrap_or(0);
        if n > 0 {
            return Some(id.to_string());
        }
    }
    None
}

fn ruolo(m: &Value) -> &str {
    m.get("role").and_then(Value::as_str).unwrap_or_default()
}

#[tokio::test]
async fn un_messaggio_assistant_porta_il_run_che_lo_ha_prodotto() {
    let Some(token) = jwt_o_salta() else { return };
    let Some(session_id) = sessione_con_turni(&token).await else {
        salta(Motivo::DatiAssenti(
            "nessuna sessione con turni assistant raggiungibile",
        ));
        return;
    };
    let Some(body) = get_json(&token, &format!("/api/chat/sessions/{session_id}/messages")).await
    else {
        salta(Motivo::ServizioGiu(&base_url()));
        return;
    };

    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let assistants: Vec<&Value> = messages.iter().filter(|m| ruolo(m) == "assistant").collect();

    // Guardia anti-vacuita': senza questa, una storia priva di assistant
    // renderebbe verde anche l'implementazione difettosa.
    assert!(
        !assistants.is_empty(),
        "campione vuoto: la sessione {session_id} non ha messaggi assistant, \
         il contratto non e' stato verificato"
    );

    let con_run = assistants
        .iter()
        .filter(|m| {
            m.get("runId")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
        })
        .count();

    assert_eq!(
        con_run,
        assistants.len(),
        "ogni messaggio assistant di un turno agentico deve portare il runId del \
         run che lo ha prodotto: senza, message-list.tsx fa `return null` e il \
         nastro attivita' del turno (Consiglio incluso) sparisce al reload. \
         Trovati {con_run}/{} con runId",
        assistants.len()
    );
}

/// L'altra meta' dello stesso aggancio: `run_status` alimenta il badge di stato
/// persistente della riga storica. Cadeva sul messaggio utente, dove nessuno lo
/// legge, lasciando il badge dell'assistant sempre vuoto.
#[tokio::test]
async fn un_messaggio_assistant_porta_anche_lo_stato_del_run() {
    let Some(token) = jwt_o_salta() else { return };
    let Some(session_id) = sessione_con_turni(&token).await else {
        salta(Motivo::DatiAssenti(
            "nessuna sessione con turni assistant raggiungibile",
        ));
        return;
    };
    let Some(body) = get_json(&token, &format!("/api/chat/sessions/{session_id}/messages")).await
    else {
        salta(Motivo::ServizioGiu(&base_url()));
        return;
    };

    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let assistants: Vec<&Value> = messages.iter().filter(|m| ruolo(m) == "assistant").collect();
    assert!(
        !assistants.is_empty(),
        "campione vuoto: contratto non verificato sulla sessione {session_id}"
    );

    let con_stato = assistants
        .iter()
        .filter(|m| {
            m.get("runStatus")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
        })
        .count();

    assert_eq!(
        con_stato,
        assistants.len(),
        "lo stato del run appartiene al messaggio assistant (era 0/6 sugli \
         assistant e 6/6 sugli utenti). Trovati {con_stato}/{}",
        assistants.len()
    );
}

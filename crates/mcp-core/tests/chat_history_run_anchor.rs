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
//! -- misurando una propria imitazione, che divergerebbe al primo ritocco.
//! Interrogare l'endpoint pone la domanda al sistema invece di riscriverla.
//!
//! # Cosa e' cambiato il 2026-07-26
//!
//! Il test cercava una sessione con turni assistant GIA' PRESENTE nell'ambiente,
//! e senza quella si dichiarava non misurabile. Due difetti in quella forma:
//!
//! 1. dipendeva da dati che nessuno garantisce — in un DB appena migrato non
//!    c'era nulla da trovare, e il test restava un verde vacuo;
//! 2. firmava con `bearer_auth`, ma `nexus_auth::validate_token` estrae il token
//!    SOLO dal cookie: con l'ambiente completo ogni chiamata avrebbe preso 401,
//!    che la vecchia forma leggeva come "nessuna sessione agganciabile". Il
//!    difetto si sarebbe mascherato da mancanza di dati.
//!
//! Ora il test SEMINA la propria conversazione nel DB-progetto (dove il dominio
//! chat vive dopo il cutover della 0507) nella stessa forma che scrive la
//! produzione, poi interroga l'endpoint e rimuove tutto. Non dipende da nessun
//! dato preesistente e non puo' passare in modo vacuo: se l'aggancio non torna,
//! il messaggio che ha seminato lui e' li' a dimostrarlo.

mod progetto;

use nexus_test_preconditions::{base_url, db_o_salta, jwt_o_salta, salta, Motivo};
use progetto::{progetto_o_salta, pulisci_conversazione, semina_conversazione, utente_del_token_o_salta};
use serde_json::Value;

/// GET autenticato. Il token viaggia nel COOKIE: `nexus_auth::validate_token` lo
/// estrae solo da li', quindi con `bearer_auth` la risposta sarebbe 401 qualunque
/// sia il contratto sotto test.
async fn get_json(token: &str, path: &str) -> Option<Value> {
    let res = reqwest::Client::new()
        .get(format!("{}{path}", base_url()))
        .header("Cookie", format!("token={token}"))
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        // Uno status di rifiuto NON e' un servizio giu': con
        // REQUIRE_INTEGRATION_TESTS=1 diventa un fallimento, cosi' un 401
        // sistematico non puo' piu' presentarsi come "dati mancanti".
        salta(Motivo::RispostaInattesa {
            status: res.status().as_u16(),
            path,
        });
        return None;
    }
    res.json::<Value>().await.ok()
}

/// I messaggi della sessione, come li consegna l'endpoint.
fn messaggi(body: &Value) -> Vec<Value> {
    body.get("messages")
        .and_then(Value::as_array)
        .or_else(|| body.as_array())
        .cloned()
        .unwrap_or_default()
}

fn ruolo(m: &Value) -> &str {
    m.get("role").and_then(Value::as_str).unwrap_or_default()
}

fn campo(m: &Value, nome: &str) -> Option<String> {
    m.get(nome).and_then(Value::as_str).map(|s| s.to_string())
}

/// Il contratto in una frase: il messaggio ASSISTANT prodotto da un run deve
/// portare l'id di quel run, anche se il run e' agganciato al messaggio UTENTE.
#[tokio::test]
async fn un_messaggio_assistant_porta_il_run_che_lo_ha_prodotto() {
    let Some(token) = jwt_o_salta() else { return };
    let Some(meta) = db_o_salta().await else { return };
    let Some(progetto) = progetto_o_salta(&meta).await else {
        return;
    };
    let Some(user_id) = utente_del_token_o_salta(&meta, &token).await else {
        return;
    };

    let conv = semina_conversazione(&progetto, user_id, "completed", "anthropic", "claude-haiku-4-5")
        .await
        .expect("seed della conversazione nel DB-progetto");

    let body = get_json(
        &token,
        &format!("/api/chat/sessions/{}/messages", conv.session_id),
    )
    .await;

    let esito = body.map(|b| {
        let msgs = messaggi(&b);
        let assistant = msgs
            .iter()
            .find(|m| campo(m, "id").as_deref() == Some(&conv.messaggio_assistant_id.to_string()))
            .cloned();
        (msgs.len(), assistant)
    });

    pulisci_conversazione(&progetto, &conv).await;

    // Il cleanup PRIMA delle asserzioni: un panic qui lascerebbe altrimenti
    // sessione, messaggi e run nel DB del progetto.
    let Some((totale, assistant)) = esito else {
        return; // lo skip l'ha gia' dichiarato get_json
    };

    let assistant = assistant.unwrap_or_else(|| {
        panic!(
            "la storia della sessione non contiene il messaggio assistant appena seminato \
             ({} messaggi restituiti): l'endpoint non consegna cio' che e' nel DB del progetto",
            totale
        )
    });

    assert_eq!(
        ruolo(&assistant),
        "assistant",
        "il messaggio seminato come assistant torna con un altro ruolo"
    );
    assert_eq!(
        campo(&assistant, "runId").as_deref(),
        Some(conv.run_id.to_string().as_str()),
        "il messaggio assistant deve portare il runId del run che lo ha prodotto. \
         Il run e' agganciato al messaggio UTENTE via run_message_id: se il JOIN \
         non usa COALESCE(request_message_id, id), qui torna null e il nastro \
         attivita' sparisce al reload"
    );
}

/// Il contro-caso dello stesso JOIN: oltre all'id deve arrivare lo STATO del run,
/// che e' quanto la UI usa per sapere se il turno e' finito.
#[tokio::test]
async fn un_messaggio_assistant_porta_anche_lo_stato_del_run() {
    let Some(token) = jwt_o_salta() else { return };
    let Some(meta) = db_o_salta().await else { return };
    let Some(progetto) = progetto_o_salta(&meta).await else {
        return;
    };
    let Some(user_id) = utente_del_token_o_salta(&meta, &token).await else {
        return;
    };

    // Stato volutamente diverso dal default 'running': cosi' un handler che
    // restituisse una costante invece del valore letto verrebbe scoperto.
    let conv = semina_conversazione(&progetto, user_id, "failed", "deepseek", "deepseek-chat")
        .await
        .expect("seed della conversazione nel DB-progetto");

    let body = get_json(
        &token,
        &format!("/api/chat/sessions/{}/messages", conv.session_id),
    )
    .await;

    let stato = body.map(|b| {
        messaggi(&b)
            .iter()
            .find(|m| campo(m, "id").as_deref() == Some(&conv.messaggio_assistant_id.to_string()))
            .and_then(|m| campo(m, "runStatus"))
    });

    pulisci_conversazione(&progetto, &conv).await;

    let Some(stato) = stato else {
        return; // skip gia' dichiarato
    };
    assert_eq!(
        stato.as_deref(),
        Some("failed"),
        "il messaggio assistant deve portare lo stato del run agganciato \
         (seminato 'failed'): senza, la UI non sa se il turno e' concluso"
    );
}

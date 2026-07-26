//! Contract test (M71): `GET /api/chat/agent-runs/:id` consegna il breakdown dei
//! costi per coppia provider/modello, aggregato da `ai_usage_ledger`.
//!
//! Il campo `usageBreakdown` e' quello che la card dei costi mostra all'utente:
//! un run agentico attraversa piu' provider (cascade fallback, figure del
//! consiglio) e il totale da solo non dice DOVE sono finiti i soldi.
//!
//! # Le due meta' vivono in DUE DB, ed e' il punto del test
//!
//! Dopo il cutover della separazione (migrazione 0507) il run sta nel DB del
//! PROGETTO, mentre `ai_usage_ledger` e' contabilita' di piattaforma e resta nel
//! META. L'handler li tiene insieme: risolve il pool del progetto per leggere
//! `agent_runs` (e i suoi sub-run) e interroga il meta per il ledger. Un test che
//! semina tutto da una parte sola non esercita quella giunzione.
//!
//! # Cosa e' cambiato il 2026-07-26
//!
//! Questo test seminava run e sessione nel META, dove la 0507 ha decommissionato
//! `chat_sessions` e `agent_runs`: le SELECT tornavano vuote e l'INSERT falliva
//! contro tabelle rinominate. Restava un test che non poteva passare in nessun
//! ambiente — e che, saltando, si presentava come contratto verificato. Ora
//! semina il run dove il run vive, con la stessa fixture di
//! `chat_history_run_anchor`.
//!
//! Rispetto alla forma precedente asserisce anche i VALORI, non solo la presenza
//! dei provider: due chiamate allo stesso modello devono comparire come UNA riga
//! con i token sommati e `calls: 2` — cioe' il `GROUP BY provider, model`
//! dell'handler. Contare le righe non lo avrebbe mai messo alla prova.

mod progetto;

use nexus_test_preconditions::{base_url, db_o_salta, jwt_o_salta, salta, Motivo};
use progetto::{
    progetto_o_salta, pulisci_conversazione, semina_conversazione, utente_del_token_o_salta,
    ProgettoDiProva,
};
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

/// Una chiamata LLM come la registra il gateway nel ledger.
struct RigaLedger {
    provider: &'static str,
    model: &'static str,
    prompt_tokens: i32,
    completion_tokens: i32,
    costo: f64,
}

/// Le chiamate seminate. Le prime due sono allo STESSO modello: e' il caso che
/// mette alla prova l'aggregazione, e nella forma precedente non era coperto.
const CHIAMATE: &[RigaLedger] = &[
    RigaLedger {
        provider: "anthropic",
        model: "claude-haiku-4-5",
        prompt_tokens: 1000,
        completion_tokens: 200,
        costo: 0.0012,
    },
    RigaLedger {
        provider: "anthropic",
        model: "claude-haiku-4-5",
        prompt_tokens: 500,
        completion_tokens: 100,
        costo: 0.0006,
    },
    RigaLedger {
        provider: "deepseek",
        model: "deepseek-chat",
        prompt_tokens: 1500,
        completion_tokens: 400,
        costo: 0.0008,
    },
];

/// Scrive nel ledger del META le chiamate attribuite al run.
async fn semina_ledger(
    meta: &PgPool,
    run_id: Uuid,
    user_id: Uuid,
    project_id: Uuid,
) -> Result<(), sqlx::Error> {
    for c in CHIAMATE {
        sqlx::query(
            "INSERT INTO ai_usage_ledger
                (id, run_id, user_id, project_id, provider, model,
                 prompt_tokens, completion_tokens, total_tokens, total_cost,
                 input_cost, output_cost, currency, status, finalized_at, created_at)
             VALUES (gen_random_uuid(), $1, $2, $3, $4, $5,
                     $6, $7, $8, $9, 0, 0, 'USD', 'finalized', NOW(), NOW())",
        )
        .bind(run_id)
        .bind(user_id)
        .bind(project_id)
        .bind(c.provider)
        .bind(c.model)
        .bind(c.prompt_tokens)
        .bind(c.completion_tokens)
        .bind(c.prompt_tokens + c.completion_tokens)
        .bind(c.costo)
        .execute(meta)
        .await?;
    }
    Ok(())
}

async fn pulisci_ledger(meta: &PgPool, run_id: Uuid) {
    let _ = sqlx::query("DELETE FROM ai_usage_ledger WHERE run_id = $1")
        .bind(run_id)
        .execute(meta)
        .await;
}

/// La riga del breakdown per una coppia provider/modello.
fn riga<'a>(breakdown: &'a [serde_json::Value], provider: &str, model: &str) -> &'a serde_json::Value {
    breakdown
        .iter()
        .find(|e| {
            e.get("provider").and_then(|v| v.as_str()) == Some(provider)
                && e.get("model").and_then(|v| v.as_str()) == Some(model)
        })
        .unwrap_or_else(|| {
            panic!("breakdown senza la riga {provider}/{model}: {breakdown:?}")
        })
}

fn intero(v: &serde_json::Value, campo: &str) -> i64 {
    v.get(campo).and_then(serde_json::Value::as_i64).unwrap_or(-1)
}

fn decimale(v: &serde_json::Value, campo: &str) -> f64 {
    v.get(campo)
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(f64::NAN)
}

#[tokio::test]
async fn agent_run_endpoint_include_usage_breakdown_aggregato() {
    let Some(token) = jwt_o_salta() else { return };
    let Some(meta) = db_o_salta().await else { return };
    let Some(user_id) = utente_del_token_o_salta(&meta, &token).await else {
        return;
    };
    // Il progetto va scelto DOPO l'utente: deve essere uno a cui quell'utente ha
    // accesso, altrimenti gli handler rispondono 403 e il test misurerebbe la
    // propria scelta sbagliata invece del contratto.
    let Some(progetto) = progetto_o_salta(&meta, user_id).await else {
        return;
    };

    // Il run vive nel DB del progetto; il ledger nel meta. Le due meta' vanno
    // seminate dove l'handler andra' a cercarle.
    let conv = semina_conversazione(&progetto, user_id, "completed", "anthropic", "claude-haiku-4-5")
        .await
        .expect("seed della conversazione nel DB-progetto");
    if let Err(e) = semina_ledger(&meta, conv.run_id, user_id, progetto.project_id).await {
        pulisci_conversazione(&progetto, &conv).await;
        panic!("seed del ledger nel meta-DB fallito: {e}");
    }

    let esito = interroga_endpoint(&token, conv.run_id).await;

    pulisci_ledger(&meta, conv.run_id).await;
    pulisci_conversazione(&progetto, &conv).await;

    // Asserzioni dopo il cleanup: un panic prima lascerebbe righe in due DB.
    let Some(body) = esito else {
        return; // skip gia' dichiarato
    };

    let breakdown = body
        .get("usageBreakdown")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("usageBreakdown assente nella risposta: {body}"))
        .clone();

    assert_eq!(
        breakdown.len(),
        2,
        "tre chiamate su DUE coppie provider/modello devono dare due righe \
         (il GROUP BY dell'handler), trovate {}: {breakdown:?}",
        breakdown.len()
    );

    // Coppia ripetuta: i token si sommano e le chiamate si contano.
    let anthropic = riga(&breakdown, "anthropic", "claude-haiku-4-5");
    assert_eq!(intero(anthropic, "calls"), 2, "due chiamate allo stesso modello");
    assert_eq!(
        intero(anthropic, "promptTokens"),
        1500,
        "prompt token sommati sulle due chiamate (1000 + 500)"
    );
    assert_eq!(
        intero(anthropic, "completionTokens"),
        300,
        "completion token sommati (200 + 100)"
    );
    assert!(
        (decimale(anthropic, "totalCost") - 0.0018).abs() < 1e-9,
        "costo sommato (0.0012 + 0.0006), trovato {}",
        decimale(anthropic, "totalCost")
    );

    // Coppia singola: passa invariata.
    let deepseek = riga(&breakdown, "deepseek", "deepseek-chat");
    assert_eq!(intero(deepseek, "calls"), 1);
    assert_eq!(intero(deepseek, "totalTokens"), 1900);

    // Il totale del run e' la somma di cio' che il breakdown dettaglia: se
    // divergessero, la card mostrerebbe un totale che le sue righe non spiegano.
    let totale_atteso: f64 = CHIAMATE.iter().map(|c| c.costo).sum();
    let totale_breakdown: f64 = breakdown.iter().map(|r| decimale(r, "totalCost")).sum();
    assert!(
        (totale_breakdown - totale_atteso).abs() < 1e-9,
        "la somma delle righe del breakdown ({totale_breakdown}) deve valere \
         quanto le chiamate seminate ({totale_atteso})"
    );
}

/// Chiama l'endpoint del run. `None` quando la precondizione manca (skip gia'
/// dichiarato dal punto unico).
async fn interroga_endpoint(token: &str, run_id: Uuid) -> Option<serde_json::Value> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok()?;
    let url = format!("{}/api/chat/agent-runs/{run_id}", base_url());
    let resp = match client
        .get(&url)
        .header("Cookie", format!("token={token}"))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => {
            salta(Motivo::ServizioGiu(&base_url()));
            return None;
        }
    };
    if !resp.status().is_success() {
        salta(Motivo::RispostaInattesa {
            status: resp.status().as_u16(),
            path: "/api/chat/agent-runs/:id",
        });
        return None;
    }
    resp.json::<serde_json::Value>().await.ok()
}

/// Silenzia l'avviso su `ProgettoDiProva` importato per il tipo di ritorno della
/// fixture: serve alla firma, non e' usato per nome in questo file.
#[allow(dead_code)]
type _FixtureInUso = ProgettoDiProva;

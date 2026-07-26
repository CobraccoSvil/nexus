//! Fixture per i contract test che devono seminare nel DB-PROGETTO.
//!
//! # Perche' esiste
//!
//! Dopo il cutover della separazione DB (migrazione 0507) il dominio chat/run
//! non vive piu' nel meta: `chat_sessions`, `chat_messages` e `agent_runs` sono
//! state RINOMINATE nel meta-DB e vivono nel DB di ciascun progetto
//! (`<slug>_nexus`). Due contract test erano rimasti a seminare e leggere sul
//! meta e non potevano piu' funzionare in nessun ambiente: uno cercava righe in
//! tabelle che non esistono piu' li', l'altro sperava di trovare una sessione
//! con turni assistant gia' pronta nell'ambiente.
//!
//! Qui vive il pezzo che i due condividono: risolvere un progetto REALE col suo
//! pool (dallo stesso punto unico che usa la produzione,
//! `nexus_project_pools::project_data_pool`) e seminarci una conversazione
//! completa — sessione, messaggio utente, risposta assistant, run agganciato.
//!
//! # Cosa NON fa
//!
//! Non provisiona un progetto: se nell'ambiente non ce n'e' uno con il suo DB,
//! la precondizione manca e il test lo dichiara (`Motivo::DatiAssenti`). Creare
//! qui un DB-progetto significherebbe che il test si fabbrica l'oggetto della
//! misura invece di trovarlo (regola O): il provisioning e' un percorso di
//! produzione con un suo codice, e un test che lo aggira non ne prova nulla.
//!
//! # Idempotenza (regola F)
//!
//! Ogni fixture usa UUID nuovi a ogni esecuzione e ha il suo `cleanup`, che
//! rimuove nell'ordine inverso delle dipendenze. Due esecuzioni concorrenti non
//! si incontrano mai.

#![allow(dead_code)]

use nexus_test_preconditions::{salta, Motivo};
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Progetto su cui seminare, col pool del suo DB gia' aperto.
pub struct ProgettoDiProva {
    pub project_id: Uuid,
    pub pool: PgPool,
}

/// Cerca un progetto PROVISIONATO (che abbia cioe' un DB raggiungibile) e ne
/// restituisce il pool, risolto dal punto unico della produzione.
///
/// Scandaglia i progetti del meta finche' uno risponde: un progetto in elenco ma
/// mai provisionato da' `NotProvisioned`, che qui non e' un errore ma un
/// candidato scartato. Se nessuno risponde la precondizione manca.
pub async fn progetto_o_salta(meta: &PgPool) -> Option<ProgettoDiProva> {
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM projects ORDER BY created_at")
        .fetch_all(meta)
        .await
        .unwrap_or_default();

    if ids.is_empty() {
        salta(Motivo::DatiAssenti("nessun progetto in projects"));
        return None;
    }

    for project_id in ids {
        if let Ok(pool) = nexus_project_pools::project_data_pool(meta, project_id).await {
            return Some(ProgettoDiProva { project_id, pool });
        }
    }

    salta(Motivo::DatiAssenti(
        "nessun progetto con DB provisionato (project_data_pool non risolve per nessuno)",
    ));
    None
}

/// L'utente per cui il token e' stato emesso, verificato nel meta.
///
/// I test seminano righe INTESTATE a chi poi interroga l'API: gli handler
/// filtrano per `user_id` preso dal token (`fetch_owned_run_row`,
/// `load_session_context`), quindi seminare per un utente diverso darebbe 403 o
/// 404 e il test misurerebbe il proprio errore. L'id si legge dal token, non si
/// sceglie: e' l'unico che l'API accettera'.
pub async fn utente_del_token_o_salta(meta: &PgPool, token: &str) -> Option<Uuid> {
    let Some(sub) = sub_dal_jwt(token) else {
        salta(Motivo::DatiAssenti(
            "il token non contiene un claim 'sub' leggibile",
        ));
        return None;
    };
    let Ok(user_id) = Uuid::parse_str(&sub) else {
        salta(Motivo::DatiAssenti("il claim 'sub' non e' un UUID"));
        return None;
    };

    let esiste: Option<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(meta)
        .await
        .unwrap_or(None);

    if esiste.is_none() {
        // Non lo si crea qui: `ai_usage_ledger.user_id` ha una FK su `users`, e
        // un utente inventato dal test resterebbe nel DB dopo il cleanup. Che
        // l'utente del token esista e' una proprieta' dell'ambiente.
        salta(Motivo::DatiAssenti(
            "l'utente del token non esiste nella tabella users del meta-DB",
        ));
        return None;
    }
    Some(user_id)
}

/// Legge il claim `sub` dal payload del JWT SENZA verificarne la firma.
///
/// Qui non si sta autenticando nessuno: si sta solo chiedendo al token per chi
/// e' stato emesso, per seminare le righe a suo nome. La verifica della firma la
/// fa il servizio a ogni richiesta del test — se il token non fosse valido, le
/// chiamate darebbero 401 e il test fallirebbe li'.
fn sub_dal_jwt(token: &str) -> Option<String> {
    let payload_b64 = token.split('.').nth(1)?;
    let payload = base64_url_decode(payload_b64)?;
    let json: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    json.get("sub")?.as_str().map(|s| s.to_string())
}

/// base64url senza padding, come lo scrive un JWT.
fn base64_url_decode(input: &str) -> Option<Vec<u8>> {
    const TAVOLA: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut valori = Vec::with_capacity(input.len());
    for c in input.bytes() {
        valori.push(TAVOLA.iter().position(|t| *t == c)? as u32);
    }
    let mut out = Vec::with_capacity(valori.len() * 3 / 4);
    for chunk in valori.chunks(4) {
        let mut acc = 0u32;
        for (i, v) in chunk.iter().enumerate() {
            acc |= v << (18 - 6 * i);
        }
        let byte_utili = chunk.len() * 6 / 8;
        for i in 0..byte_utili {
            out.push(((acc >> (16 - 8 * i)) & 0xFF) as u8);
        }
    }
    Some(out)
}

/// Una conversazione completa nel DB-progetto: la sessione, il messaggio utente
/// che ha innescato il turno, la risposta assistant e il run che l'ha prodotta.
///
/// La forma riproduce quella che scrive la produzione, perche' e' la forma su cui
/// gli handler fanno il JOIN: `agent_runs.run_message_id` punta al messaggio
/// UTENTE, e il messaggio assistant vi si aggancia con `request_message_id`
/// (`ar.run_message_id = COALESCE(cm.request_message_id, cm.id)`). Seminare il
/// run sul messaggio assistant farebbe passare un test che in produzione non
/// aggancia niente.
pub struct Conversazione {
    pub session_id: Uuid,
    pub messaggio_utente_id: Uuid,
    pub messaggio_assistant_id: Uuid,
    pub run_id: Uuid,
}

/// Semina la conversazione. `stato_run` finisce in `agent_runs.status` ed e'
/// quello che l'API deve restituire come `runStatus`.
pub async fn semina_conversazione(
    progetto: &ProgettoDiProva,
    user_id: Uuid,
    stato_run: &str,
    provider: &str,
    model: &str,
) -> Result<Conversazione, sqlx::Error> {
    let session_id = Uuid::new_v4();
    let messaggio_utente_id = Uuid::new_v4();
    let messaggio_assistant_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO chat_sessions (id, project_id, user_id, title, status)
         VALUES ($1, $2, $3, 'contract test', 'active')",
    )
    .bind(session_id)
    .bind(progetto.project_id)
    .bind(user_id)
    .execute(&progetto.pool)
    .await?;

    sqlx::query(
        "INSERT INTO chat_messages (id, session_id, project_id, role, content)
         VALUES ($1, $2, $3, 'user', 'domanda del contract test')",
    )
    .bind(messaggio_utente_id)
    .bind(session_id)
    .bind(progetto.project_id)
    .execute(&progetto.pool)
    .await?;

    sqlx::query(
        "INSERT INTO chat_messages (id, session_id, project_id, role, content, request_message_id)
         VALUES ($1, $2, $3, 'assistant', 'risposta del contract test', $4)",
    )
    .bind(messaggio_assistant_id)
    .bind(session_id)
    .bind(progetto.project_id)
    .bind(messaggio_utente_id)
    .execute(&progetto.pool)
    .await?;

    sqlx::query(
        "INSERT INTO agent_runs
            (id, session_id, project_id, user_id, run_message_id, status, automation_mode,
             provider, model, iteration_count)
         VALUES ($1, $2, $3, $4, $5, $6, 'confirm', $7, $8, 1)",
    )
    .bind(run_id)
    .bind(session_id)
    .bind(progetto.project_id)
    .bind(user_id)
    .bind(messaggio_utente_id)
    .bind(stato_run)
    .bind(provider)
    .bind(model)
    .execute(&progetto.pool)
    .await?;

    Ok(Conversazione {
        session_id,
        messaggio_utente_id,
        messaggio_assistant_id,
        run_id,
    })
}

/// Rimuove quanto seminato, nell'ordine inverso delle dipendenze. Non fallisce
/// mai: un cleanup che panica nasconderebbe l'esito vero del test e lascerebbe
/// comunque righe indietro.
pub async fn pulisci_conversazione(progetto: &ProgettoDiProva, c: &Conversazione) {
    let _ = sqlx::query("DELETE FROM agent_runs WHERE id = $1")
        .bind(c.run_id)
        .execute(&progetto.pool)
        .await;
    let _ = sqlx::query("DELETE FROM chat_messages WHERE session_id = $1")
        .bind(c.session_id)
        .execute(&progetto.pool)
        .await;
    let _ = sqlx::query("DELETE FROM chat_sessions WHERE id = $1")
        .bind(c.session_id)
        .execute(&progetto.pool)
        .await;
}

/// Legge un campo testo da una riga, per le asserzioni dei chiamanti.
pub fn testo(row: &sqlx::postgres::PgRow, colonna: &str) -> String {
    row.try_get::<String, _>(colonna).unwrap_or_default()
}

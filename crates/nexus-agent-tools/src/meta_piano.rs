//! Punto unico (regola L) della RIGA `nexus_agent_meta_steps` che porta il
//! PIANO di un run: quale titolo, quale payload, e l'invariante "una riga sola".
//!
//! # La domanda
//!
//! «Qual e' il piano di QUESTO run?» ha una sola risposta, ed e' uno STATO, non
//! una cronologia: al replay deve valere l'ultimo, non quello del momento in cui
//! il piano nacque (altrimenti dopo un refresh le spunte tornerebbero indietro).
//!
//! # Perche' esiste
//!
//! A rispondere erano DUE produttori, ognuno con la propria disciplina:
//!
//! - il TOOL `nexus_todo_write` ([`crate::todos`]), che scrive lo STATO dei todo
//!   e applicava gia' "una riga per run" (UPDATE, e INSERT solo se manca);
//! - il nodo PLANNER (`nexus_agent_graph::nodes::planner`), che pubblica la
//!   PROVENIENZA del piano (`plan_id`, `provider`, `model`, `active_todo_id`) e
//!   passa dalla porta generica `MetaStepStore`, la cui impl e' una INSERT cieca
//!   — corretta per ogni altro kind, che e' append-only per natura.
//!
//! Le due discipline non si vedevano: l'UPDATE del tool girava per primo (non
//! trovava nulla, inseriva), poi il planner inseriva la SECONDA riga. MISURATO
//! il 10/08/2026 sul progetto batteria-todo-deepseek, run
//! `92a6c7f2-5f2b-4b96-a786-70f166289e9c`: due righe `kind='plan'` a 2,3 ms
//! l'una dall'altra (`Piano — 8 step` alle 15:21:04.513664, `Piano creato — 8
//! step` alle .516001), con lo STESSO array di todo — stessi id, stessi stati.
//! Non due versioni successive: due copie. Il nastro attivita' le rendeva
//! entrambe, e in chat il piano compariva due volte, identico.
//!
//! # Dove sta l'invariante
//!
//! Nello SCHEMA: indice unico parziale su `(run_id) WHERE kind = 'plan'` (mig
//! project 0018). Una regola applicata solo dal codice vale finche' tutti i
//! produttori la conoscono, e il difetto misurato e' nato proprio da un
//! produttore che non la conosceva; un terzo produttore, domani, la incontra
//! comunque. Qui il codice la usa (`ON CONFLICT`) invece di aggirarla.
//!
//! # Fusione invece di sostituzione
//!
//! I due produttori portano meta' informazione ciascuno. Il payload si FONDE
//! (`vecchio || nuovo`): chi scrive vince sulle proprie chiavi, e cio' che tace
//! resta. Cosi' l'aggiornamento degli stati del tool non cancella la provenienza
//! del planner, e viceversa.
//!
//! `n` non e' un campo indipendente: e' la lunghezza di `todos`, e lo DERIVA
//! [`componi_riga`] a ogni scrittura. Lasciarlo al chiamante rendeva
//! rappresentabile una riga con `n = 8` e tre todo. Per la stessa ragione il
//! TITOLO si compone dai campi e non lo sceglie il chiamante (regola Q): i due
//! produttori ne dichiaravano due diversi per lo stesso identico piano.

use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// `kind` della riga che porta il piano. Lo nominano l'indice unico della
/// migrazione, i due produttori e il replay: una costante sola, o
/// l'ON CONFLICT punterebbe a un indice che non copre le righe scritte.
pub const KIND_PIANO: &str = "plan";

/// La riga come la si scrive: titolo e payload gia' normalizzati.
#[derive(Debug, Clone, PartialEq)]
pub struct RigaPiano {
    /// Titolo derivato dal numero di todo. `None` quando il payload non porta
    /// un array `todos`: li' non c'e' nulla da cui derivarlo, e il titolo gia'
    /// scritto vale piu' di uno inventato.
    pub titolo: Option<String>,
    /// Payload da fondere con quello esistente.
    pub payload: Value,
}

/// Normalizza cio' che un produttore vuole scrivere: `n` derivato da `todos`,
/// titolo composto dai campi. PURA: nessun I/O, e' il criterio.
pub fn componi_riga(payload_nuovo: &Value) -> RigaPiano {
    let mut payload = payload_nuovo.clone();
    let quanti = payload
        .get("todos")
        .and_then(Value::as_array)
        .map(|t| t.len());
    match quanti {
        Some(n) => {
            if let Some(oggetto) = payload.as_object_mut() {
                oggetto.insert("n".to_string(), json!(n));
            }
            RigaPiano {
                titolo: Some(format!("Piano — {n} step")),
                payload,
            }
        }
        None => {
            // Senza l'array dei todo, `n` sarebbe una misura di cio' che non si
            // e' visto: si toglie invece di lasciarlo mentire su un piano che
            // questo produttore non sta dichiarando.
            if let Some(oggetto) = payload.as_object_mut() {
                oggetto.remove("n");
            }
            RigaPiano {
                titolo: None,
                payload,
            }
        }
    }
}

/// Scrive il piano del run: una riga sola, payload fuso col precedente.
///
/// Best-effort presso i chiamanti: qui l'errore SQL risale, e chi chiama decide
/// (per entrambi i produttori la scrittura del piano non deve far fallire il
/// lavoro vero, che e' scrivere i todo ed eseguire il run).
pub async fn scrivi(pool: &PgPool, run_id: Uuid, payload_nuovo: &Value) -> Result<(), sqlx::Error> {
    let riga = componi_riga(payload_nuovo);
    // I cast su $3 sono necessari, non ornamentali: `COALESCE($3, '')` lascia il
    // literal come `unknown` e Postgres rifiuta la Parse con 42P18 ("could not
    // determine data type of parameter"), cioe' la scrittura del piano
    // fallirebbe a ogni chiamata — best-effort, quindi in silenzio.
    sqlx::query(
        "INSERT INTO nexus_agent_meta_steps (run_id, kind, title, payload) \
         VALUES ($1, $2, COALESCE($3::text, ''), $4) \
         ON CONFLICT (run_id) WHERE kind = 'plan' DO UPDATE SET \
           title = COALESCE($3::text, nexus_agent_meta_steps.title), \
           payload = nexus_agent_meta_steps.payload || EXCLUDED.payload",
    )
    .bind(run_id)
    .bind(KIND_PIANO)
    .bind(riga.titolo.as_deref())
    .bind(&riga.payload)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Il piano nella forma che la card si aspetta, letto dallo STATO ATTUALE dei
/// todo del run. `None` quando non ci sono todo o il DB non risponde: senza voci
/// non c'e' piano da mostrare, e un payload vuoto renderebbe una card vuota.
pub async fn payload_dai_todo(pool: &PgPool, run_id: Uuid) -> Option<Value> {
    let righe = sqlx::query(
        "SELECT id, seq, content, status, priority FROM nexus_agent_todos \
         WHERE run_id = $1 ORDER BY seq",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    .ok()?;
    if righe.is_empty() {
        return None;
    }
    let todos: Vec<Value> = righe
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<Uuid, _>("id").ok().map(|v| v.to_string()),
                "seq": r.try_get::<i32, _>("seq").ok(),
                "content": r.try_get::<String, _>("content").ok(),
                "status": r.try_get::<String, _>("status").ok(),
                "priority": r.try_get::<String, _>("priority").ok(),
            })
        })
        .collect();
    Some(json!({ "todos": todos }))
}

/// Riscrive il piano dallo stato attuale dei todo. Best-effort: un errore qui
/// non fa fallire la scrittura dei todo, che e' il lavoro vero del tool.
pub async fn scrivi_dai_todo(pool: &PgPool, run_id: Uuid) {
    let Some(payload) = payload_dai_todo(pool, run_id).await else {
        return;
    };
    if let Err(e) = scrivi(pool, run_id, &payload).await {
        tracing::warn!(
            run_id = %run_id,
            error = %e,
            "meta_piano: scrittura della riga plan fallita (best-effort)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn il_titolo_e_l_ennesimo_campo_derivano_dai_todo() {
        let riga = componi_riga(&json!({"todos": [{"id": "a"}, {"id": "b"}, {"id": "c"}]}));
        assert_eq!(riga.titolo.as_deref(), Some("Piano — 3 step"));
        assert_eq!(riga.payload.get("n").and_then(Value::as_u64), Some(3));
    }

    #[test]
    fn un_n_dichiarato_dal_chiamante_non_puo_divergere_dai_todo() {
        // Il difetto che questa derivazione rende irrappresentabile: `n` scritto
        // a mano e todo cambiati sotto.
        let riga = componi_riga(&json!({"n": 8, "todos": [{"id": "a"}]}));
        assert_eq!(riga.payload.get("n").and_then(Value::as_u64), Some(1));
        assert_eq!(riga.titolo.as_deref(), Some("Piano — 1 step"));
    }

    #[test]
    fn senza_array_todos_non_si_inventa_un_titolo() {
        let riga = componi_riga(&json!({"provider": "mistral", "n": 8}));
        assert_eq!(riga.titolo, None);
        assert!(
            riga.payload.get("n").is_none(),
            "n senza todos e' una misura di cio' che non si e' visto"
        );
    }

    #[test]
    fn la_provenienza_del_planner_resta_nel_payload() {
        let riga = componi_riga(&json!({
            "todos": [{"id": "a"}],
            "plan_id": "r1",
            "provider": "mistral",
            "model": "mistral-small-latest",
            "active_todo_id": "a",
        }));
        assert_eq!(
            riga.payload.get("provider").and_then(Value::as_str),
            Some("mistral")
        );
        assert_eq!(
            riga.payload.get("active_todo_id").and_then(Value::as_str),
            Some("a")
        );
    }
}

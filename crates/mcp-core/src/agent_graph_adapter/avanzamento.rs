//! Adapter del trait [`nexus_agent_graph::runtime::ports::AvanzamentoPort`].
//!
//! Porta all'executor i FATTI su cui si decide se una figura merita ancora
//! tempo: i passi che ha lasciato (`agent_steps`, DB del PROGETTO) e le
//! scritture che ha prodotto (`file_mutations`, DB META), ciascuno col proprio
//! istante. Nient'altro: il criterio e' del modulo puro
//! [`nexus_agent_graph::decisions::avanzamento_figura`].
//!
//! ## Perche' la query non filtra
//!
//! Sarebbe comodo scartare in SQL i passi ripetuti (`DISTINCT ON`) o le
//! riscritture a contenuto invariato (`WHERE before_sha256 IS DISTINCT FROM
//! after_sha256`): sarebbero righe in meno da trasportare. Il costo e' che il
//! criterio finirebbe per vivere in due posti (regola L) e — peggio — che i
//! fatti che dicono «non sta avanzando» sparirebbero prima di essere contati
//! come tali. Il criterio ha bisogno di vedere le ripetizioni per chiamarle
//! ripetizioni: sono cio' che misura, non rumore da togliere.
//!
//! E' la stessa scelta gia' dichiarata da
//! [`crate::agent_graph_adapter::mutation_progress`], e per la stessa ragione.
//!
//! ## Perche' la firma la costruisce la porta
//!
//! «E' la stessa cosa che ha gia' fatto?» dipende dalla forma dell'input del
//! tool, che il criterio non conosce e non deve conoscere. La costruisce il
//! punto unico [`build_signature`] (nome del tool + hash dell'input canonico),
//! lo STESSO che l'executor usa per la rilevazione dei loop in memoria: due
//! nozioni diverse di «gia' fatto» darebbero due risposte diverse alla stessa
//! domanda a due metri di distanza.
//!
//! La granularita' e' quella FINE di proposito. La firma grossolana di
//! `agent_tools::subagent_timeout` (tool + primo token del comando) risponde a
//! un'altra domanda — «stava ripetendo la stessa strada quando e' morto?» — e
//! li' e' giusta perche' tre gestori di pacchetti diversi non sono una
//! ripetizione. Qui confonderebbe `npm test` con `npm run build` e direbbe
//! «ripete» a una figura che sta alternando due comandi diversi.
//!
//! ## Perche' le scritture sono filtrate per SESSIONE e non per run
//!
//! Identica alla motivazione di `mutation_progress`: le scritture di un sub-run
//! sono registrate col `run_id` del SUB-run, quindi una figura che delega a un
//! coder risulterebbe ferma guardando il solo proprio run — e verrebbe fermata
//! mentre il suo delegato lavora. La sessione e' il confine piu' stretto
//! disponibile da un lato solo (le due tabelle vivono in DB diversi).
//!
//! Il rovescio e' dichiarato: in una sessione con piu' run concorrenti le
//! scritture altrui tengono in vita anche questa figura. E' l'errore nella
//! direzione scelta di proposito dal criterio (proseguire), e il tetto assoluto
//! resta a coprirlo.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{postgres::PgRow, PgPool, Row};
use uuid::Uuid;

use nexus_agent_graph::decisions::loop_signatures::build_signature;
use nexus_agent_graph::decisions::{
    FattiAvanzamento, PassoOsservato, ScritturaOsservata, WriteFact,
};
use nexus_agent_graph::runtime::ports::{AvanzamentoPort, PortError};

/// Tetto di passi letti, dai piu' RECENTI. Un run lungo puo' averne centinaia e
/// la domanda («ha fatto qualcosa di nuovo di recente?») non ne richiede la
/// storia intera.
///
/// Il taglio ha una direzione dichiarata: cadono i piu' VECCHI, quindi una
/// strada tentata molto tempo fa puo' tornare a sembrare nuova e il criterio
/// concede tempo invece di toglierlo. E' la stessa direzione in cui erra tutto
/// il resto del criterio, e il tetto assoluto resta a coprirla.
const MAX_PASSI: i64 = 400;

/// Tetto di scritture lette, dalle piu' recenti. Stessa logica.
const MAX_SCRITTURE: i64 = 400;

/// Adapter [`AvanzamentoPort`] -> `agent_steps` (progetto) + `file_mutations` (META).
pub struct AvanzamentoAdapter {
    /// Pool del PROGETTO: `agent_steps` vive qui.
    run_db: PgPool,
    /// Pool META: `file_mutations` vive qui.
    meta_db: PgPool,
    /// Il run di cui si guardano i passi.
    run_id: Uuid,
    /// La sessione di cui si guardano le scritture (vedi doc di modulo).
    session_id: Uuid,
}

impl AvanzamentoAdapter {
    /// Costruisce l'adapter con le dipendenze gia' risolte dal call site.
    pub fn new(run_db: PgPool, meta_db: PgPool, run_id: Uuid, session_id: Uuid) -> Self {
        Self {
            run_db,
            meta_db,
            run_id,
            session_id,
        }
    }

    /// I passi del run, in ordine CRONOLOGICO.
    async fn passi(&self) -> Result<Vec<PassoOsservato>, PortError> {
        // DESC + LIMIT prende la CODA (l'ordine per `step_index` e' quello dei
        // passi, punto unico `STEP_INDEX_STRIDE`); l'inversione la rimette in
        // cronologia, che e' come il criterio la pretende.
        let rows = sqlx::query(
            "SELECT tool_name, tool_input, created_at \
               FROM agent_steps \
              WHERE run_id = $1 \
              ORDER BY step_index DESC \
              LIMIT $2",
        )
        .bind(self.run_id)
        .bind(MAX_PASSI)
        .fetch_all(&self.run_db)
        .await
        .map_err(|e| PortError::Tool(format!("lettura agent_steps: {e}").into()))?;

        Ok(rows
            .into_iter()
            .rev()
            .filter_map(|r| {
                let tool: String = r.try_get("tool_name").unwrap_or_default();
                // Un blocco senza tool (testo del modello) non e' una STRADA:
                // non ha invocato niente, e contarlo darebbe a ogni turno di
                // sola prosa un avanzamento che non c'e'.
                if tool.trim().is_empty() {
                    return None;
                }
                let input: serde_json::Value =
                    r.try_get("tool_input").unwrap_or(serde_json::Value::Null);
                let istante: DateTime<Utc> = r.try_get("created_at").ok()?;
                Some(PassoOsservato {
                    firma: build_signature(&tool, &input),
                    istante_s: istante.timestamp(),
                })
            })
            .collect())
    }

    /// Le scritture della sessione, in ordine CRONOLOGICO.
    async fn scritture(&self) -> Result<Vec<ScritturaOsservata>, PortError> {
        let rows = sqlx::query(
            "SELECT before_sha256, after_sha256, solo_fine_riga, created_at \
               FROM file_mutations \
              WHERE session_id = $1 \
              ORDER BY id DESC \
              LIMIT $2",
        )
        .bind(self.session_id)
        .bind(MAX_SCRITTURE)
        .fetch_all(&self.meta_db)
        .await
        .map_err(|e| PortError::Tool(format!("lettura file_mutations: {e}").into()))?;

        Ok(rows.into_iter().rev().filter_map(scrittura_da_riga).collect())
    }
}

/// Una riga di `file_mutations` -> il fatto grezzo, senza giudizio.
///
/// `None` solo se la riga non ha un istante leggibile: senza quello la
/// scrittura non e' collocabile nella storia, ed e' l'unica cosa che il criterio
/// non puo' supplire.
fn scrittura_da_riga(r: PgRow) -> Option<ScritturaOsservata> {
    let istante: DateTime<Utc> = r.try_get("created_at").ok()?;
    Some(ScritturaOsservata {
        fatto: WriteFact {
            before_sha256: r.try_get("before_sha256").ok().flatten(),
            after_sha256: r.try_get("after_sha256").ok().flatten(),
            // NULL sulle righe anteriori alla mig 0680: resta `None`, e il
            // criterio ricade sul confronto degli hash. La porta PORTA i fatti e
            // non li giudica — anche quando il fatto e' "non misurato".
            solo_fine_riga: r.try_get("solo_fine_riga").ok().flatten(),
        },
        istante_s: istante.timestamp(),
    })
}

#[async_trait]
impl AvanzamentoPort for AvanzamentoAdapter {
    async fn fatti_avanzamento(&self) -> Result<FattiAvanzamento, PortError> {
        // I due canali si leggono ENTRAMBI e l'errore si propaga: un canale muto
        // per guasto non deve passare per "non ha scritto niente", che il
        // criterio leggerebbe come assenza di avanzamento. Meglio dichiarare che
        // non si e' potuto guardare (regola Q) e proseguire.
        let (passi, scritture) = tokio::try_join!(self.passi(), self.scritture())?;
        Ok(FattiAvanzamento { passi, scritture })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_agent_graph::decisions::{
        decidi_prosecuzione, CausaArresto, Prosecuzione, SoglieAvanzamento,
    };
    use nexus_agent_graph::runtime::ports::{AgentStepStore, PersistedStep, StepStatus};
    use serde_json::json;

    use crate::agent_graph_adapter::agent_step_store::PgAgentStepStore;

    /// Scrive un passo col PRODUTTORE di produzione (regola O): la riga di
    /// `agent_steps` la compone `PgAgentStepStore::persist_step`, non una INSERT
    /// scritta nel test. Se un domani il produttore cambiasse colonna o forma,
    /// qui diventerebbe rosso invece di restare verde su una tabella immaginaria.
    async fn passo(store: &PgAgentStepStore, run_id: Uuid, iterazione: i64, tool: &str, input: serde_json::Value) {
        store
            .persist_step(
                &run_id.to_string(),
                iterazione,
                0,
                PersistedStep {
                    tool_name: tool.to_string(),
                    tool_input: input,
                    tool_result: Some("ok".to_string()),
                    status: StepStatus::Completed,
                },
            )
            .await
            .expect("step persistito");
    }

    fn soglie(inattivita_max_s: u64, tetto_assoluto_s: u64) -> SoglieAvanzamento {
        SoglieAvanzamento {
            inattivita_max_s,
            tetto_assoluto_s,
        }
    }

    /// IL CASO MISURATO (09/08/2026): una figura che ha prodotto passi su strade
    /// diverse continua, anche oltre il tetto storico di 240s. La catena e'
    /// quella vera — produttore -> colonna -> porta -> criterio — e non una
    /// costruzione a mano dei fatti.
    ///
    /// MUTAZIONE: far tornare a [`build_signature`] il solo nome del tool nella
    /// porta (cioe' perdere la granularita' dell'input) rende questi tre
    /// `read_file` una sola strada, e la figura viene fermata con `no_progress`
    /// — il valore del difetto.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn una_figura_che_esplora_strade_nuove_prosegue(pool: PgPool) {
        let run_id = crate::test_support::seed_agent_run(&pool).await;
        let store = PgAgentStepStore::new(pool.clone());
        for (i, path) in ["src/a.rs", "src/b.rs", "src/c.rs"].iter().enumerate() {
            passo(
                &store,
                run_id,
                i as i64 + 1,
                "read_file",
                json!({ "path": path }),
            )
            .await;
        }
        // Le scritture vivono nel META: qui la porta usa lo stesso pool per
        // entrambi e la query su `file_mutations` fallirebbe. Si guarda percio'
        // il solo canale dei passi, che e' quello che questo test misura.
        let porta = AvanzamentoAdapter::new(pool.clone(), pool.clone(), run_id, Uuid::new_v4());
        let passi = porta.passi().await.expect("passi leggibili");
        assert_eq!(passi.len(), 3, "tre passi persistiti, tre passi letti");
        let firme: std::collections::HashSet<&str> =
            passi.iter().map(|p| p.firma.as_str()).collect();
        assert_eq!(firme.len(), 3, "tre path diversi sono tre strade: {passi:?}");

        // Il run e' partito 255s fa — OLTRE il tetto storico di 240s — e l'ultimo
        // passo risale a 5 secondi fa: e' la forma di una figura che sta
        // lavorando, cioe' esattamente quella delle quattro misurate. Gli istanti
        // dei passi sono quelli VERI scritti dal produttore (`now()` del DB), non
        // costanti del test: il tempo lo si sposta attorno a loro.
        let ultimo_s = passi.last().expect("tre passi").istante_s;
        let adesso = ultimo_s + 5;
        let avvio = adesso - 255;
        assert!(
            adesso - avvio > 240,
            "il caso deve stare oltre il tetto storico, altrimenti non prova niente"
        );
        let fatti = FattiAvanzamento {
            passi,
            scritture: Vec::new(),
        };
        let d = decidi_prosecuzione(&fatti, avvio, adesso, soglie(90, 960));
        assert!(
            !d.e_arresto(),
            "una figura che esplora non si ferma al vecchio tetto: {d:?}"
        );
    }

    /// L'altra meta', sulla stessa catena reale: la STESSA identica chiamata
    /// ripetuta e' una strada sola, e la figura si ferma molto prima del tetto.
    ///
    /// MUTAZIONE: aggiungere `DISTINCT ON (tool_name, tool_input)` alla query
    /// (il filtro "comodo" che sposterebbe il criterio nell'SQL) fa sparire le
    /// ripetizioni prima di essere contate: `passi_a_vuoto` scende a 0 e la
    /// figura prosegue.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn la_stessa_chiamata_ripetuta_e_una_strada_sola(pool: PgPool) {
        let run_id = crate::test_support::seed_agent_run(&pool).await;
        let store = PgAgentStepStore::new(pool.clone());
        for i in 1..=6 {
            passo(
                &store,
                run_id,
                i,
                "run_command",
                json!({"command": "npm test"}),
            )
            .await;
        }
        let porta = AvanzamentoAdapter::new(pool.clone(), pool.clone(), run_id, Uuid::new_v4());
        let passi = porta.passi().await.expect("passi leggibili");
        assert_eq!(passi.len(), 6, "la porta NON deduplica: {passi:?}");
        let firme: std::collections::HashSet<&str> =
            passi.iter().map(|p| p.firma.as_str()).collect();
        assert_eq!(firme.len(), 1, "sei chiamate identiche sono una strada sola");

        let avvio = passi[0].istante_s - 5;
        let fatti = FattiAvanzamento {
            passi,
            scritture: Vec::new(),
        };
        let d = decidi_prosecuzione(&fatti, avvio, avvio + 100, soglie(90, 960));
        match d.causa().expect("deve fermarsi") {
            CausaArresto::NonAvanza { passi_a_vuoto, .. } => {
                assert_eq!(*passi_a_vuoto, 5, "la prima e' nuova, le altre cinque no");
            }
            altro => panic!("attesa no_progress, ottenuto {altro:?}"),
        }
    }

    /// I passi arrivano al criterio in ordine CRONOLOGICO. Senza il `.rev()` la
    /// storia risulterebbe invertita: la strada tentata per PRIMA sembrerebbe
    /// l'ultima novita', e l'istante dell'"ultimo avanzamento" sarebbe quello
    /// del passo piu' vecchio — cioe' una figura viva verrebbe dichiarata ferma.
    ///
    /// MUTAZIONE: togliere `.rev()` da [`AvanzamentoAdapter::passi`] rende questo
    /// test rosso sull'ordine degli istanti.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn i_passi_arrivano_in_ordine_cronologico(pool: PgPool) {
        let run_id = crate::test_support::seed_agent_run(&pool).await;
        let store = PgAgentStepStore::new(pool.clone());
        for (i, path) in ["primo.rs", "secondo.rs", "terzo.rs"].iter().enumerate() {
            passo(
                &store,
                run_id,
                i as i64 + 1,
                "read_file",
                json!({ "path": path }),
            )
            .await;
        }
        let porta = AvanzamentoAdapter::new(pool.clone(), pool.clone(), run_id, Uuid::new_v4());
        let passi = porta.passi().await.expect("passi leggibili");
        assert_eq!(
            passi[0].firma,
            build_signature("read_file", &json!({"path": "primo.rs"})),
            "il primo passo persistito deve essere il primo letto"
        );
        assert!(
            passi.windows(2).all(|w| w[0].istante_s <= w[1].istante_s),
            "gli istanti devono essere non decrescenti: {passi:?}"
        );
    }

    /// Un blocco senza tool (prosa del modello) non e' una strada: contarlo
    /// darebbe un avanzamento a ogni turno di solo testo, cioe' proprio al modo
    /// in cui un run gira a vuoto.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn la_prosa_del_modello_non_e_una_strada(pool: PgPool) {
        let run_id = crate::test_support::seed_agent_run(&pool).await;
        let store = PgAgentStepStore::new(pool.clone());
        store
            .persist_step(
                &run_id.to_string(),
                1,
                0,
                PersistedStep {
                    tool_name: String::new(),
                    tool_input: json!({}),
                    tool_result: Some("sto ragionando sul problema".to_string()),
                    status: StepStatus::Completed,
                },
            )
            .await
            .expect("step persistito");
        let porta = AvanzamentoAdapter::new(pool.clone(), pool.clone(), run_id, Uuid::new_v4());
        assert!(
            porta.passi().await.expect("leggibile").is_empty(),
            "un blocco senza tool non entra fra le strade"
        );
    }

    /// Un run senza passi: nessun fatto, e il criterio lo dichiara invece di
    /// dedurne uno stallo. E' il ramo che impedisce di rifare il difetto in
    /// forma piu' severa (una figura dentro una chiamata al modello tace).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn un_run_senza_passi_non_autorizza_nessun_arresto(pool: PgPool) {
        let run_id = crate::test_support::seed_agent_run(&pool).await;
        let porta = AvanzamentoAdapter::new(pool.clone(), pool.clone(), run_id, Uuid::new_v4());
        let passi = porta.passi().await.expect("leggibile");
        assert!(passi.is_empty());
        let d = decidi_prosecuzione(
            &FattiAvanzamento {
                passi,
                scritture: Vec::new(),
            },
            1_000,
            1_300,
            soglie(90, 960),
        );
        assert!(matches!(d, Prosecuzione::ProseguePerIgnoto { .. }), "{d:?}");
    }
}

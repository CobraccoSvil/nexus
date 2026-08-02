//! I FATTI su cui si dichiara la causa di un timeout: la coda della storia di un
//! sub-run, letta da `agent_steps`.
//!
//! Porta (regola L) del punto unico
//! [`nexus_agent_graph::decisions::timeout_cause`]: qui si LEGGE, di la' si
//! GIUDICA. La stessa separazione di `agent_graph_adapter::mutation_progress`, e
//! per la stessa ragione — un filtro «comodo» messo nell'SQL (`WHERE tool_result
//! LIKE '%errore%'`) sarebbe un secondo criterio, scritto in un linguaggio in cui
//! nessuno lo riconoscerebbe come tale.
//!
//! ## Da dove viene l'esito
//!
//! `agent_steps.tool_result` e' TESTO: la colonna non ha un campo per l'esito, e
//! per i tool non ancora migrati a `RispostaTool` (regola Q) quel testo e'
//! l'unico canale esistente. La ricostruzione passa quindi dal PONTE unico
//! [`RispostaTool::da_testo_legacy`] — che e' l'unico punto del sistema
//! autorizzato a rileggere il marker e `EXIT CODE: N` — e mai da un
//! riconoscimento scritto qui: il giorno in cui la colonna portera' l'esito in un
//! campo proprio, cambia il ponte e questo modulo non se ne accorge.
//!
//! ## Perche' la firma la costruisce la porta
//!
//! «E' la stessa strada?» dipende dalla forma dell'INPUT del tool, che il
//! criterio non conosce e non deve conoscere: per `run_command` due tentativi
//! sono la stessa strada se lo e' il programma invocato, non perche' il tool lo
//! e'. Il primo token della riga di comando e' un DATO di input (la command line
//! E' l'oggetto della domanda, come in `agent_tools::playwright_cli`), non il
//! racconto di un esito: leggerlo non e' la regola M al contrario.

use nexus_agent_graph::decisions::{classifica_causa_timeout, CausaTimeout, TentativoOsservato};
use nexus_types::tool_outcome::RispostaTool;
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Quanti passi finali si guardano. La domanda e' «su cosa e' finito il budget»,
/// non «cosa ha fatto in tutto il run»: la coda basta, e un tetto tiene la
/// lettura a una riga di indice anche per un run che ha prodotto centinaia di
/// step.
const MAX_PASSI: i64 = 30;

/// Campi di `tool_input` che portano la riga di comando, in ordine di
/// preferenza. Sono i nomi che gli schema dei tool dichiarano (`run_command`,
/// `run_service`): un tool senza nessuno di questi non ha una «strada» piu' fine
/// del proprio nome, ed e' giusto che la firma resti il nome.
const CAMPI_COMANDO: [&str; 2] = ["command", "cmd"];

/// La causa del timeout di `run_id`, dai passi che ha lasciato.
///
/// Non fallisce verso l'alto: un DB che non risponde produce
/// [`CausaTimeout::NotObservable`], che e' esattamente cio' che si sa in quel
/// caso — «non ho potuto guardare», mai «non e' successo niente».
pub(crate) async fn causa_del_timeout(pool: &PgPool, run_id: Uuid) -> CausaTimeout {
    classifica_causa_timeout(&tentativi_finali(pool, run_id).await)
}

/// Gli ultimi passi del run, in ordine CRONOLOGICO (il criterio guarda la coda).
async fn tentativi_finali(pool: &PgPool, run_id: Uuid) -> Vec<TentativoOsservato> {
    // DESC + LIMIT prende la CODA (l'ordine per `step_index` e' quello dei passi,
    // punto unico `STEP_INDEX_STRIDE`); l'inversione la rimette in cronologia,
    // che e' come il criterio la pretende.
    let rows = match sqlx::query(
        "SELECT tool_name, tool_input, tool_result, status \
           FROM agent_steps \
          WHERE run_id = $1 \
          ORDER BY step_index DESC \
          LIMIT $2",
    )
    .bind(run_id)
    .bind(MAX_PASSI)
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                run_id = %run_id,
                error = %e,
                "causa del timeout: agent_steps non leggibili, la causa resta non osservabile"
            );
            return Vec::new();
        }
    };
    rows.into_iter()
        .rev()
        .filter_map(|r| {
            let strumento: String = r.try_get("tool_name").unwrap_or_default();
            // Un blocco senza tool (testo del modello) non e' un TENTATIVO: non
            // ha invocato niente e non puo' essere fallito.
            if strumento.trim().is_empty() {
                return None;
            }
            let input: Value = r.try_get("tool_input").unwrap_or(Value::Null);
            // Uno step senza risultato e' un tentativo di cui non si conosce
            // l'esito: entra come NON fallito, perche' «non lo so» non e' «e'
            // andata male».
            let risultato: Option<String> = r.try_get("tool_result").unwrap_or(None);
            // L'ESITO viene dalla colonna strutturata: dal 02/08/2026 il
            // produttore la scrive dal flag `is_error` (PersistedStep, regola
            // Q), e leggerla e' la regola M applicata — prima si ricostruiva
            // dal testo col ponte legacy, cioe' dal racconto invece che dal
            // campo. Il ponte resta SOLO per l'`exit_code`, che una colonna
            // non ce l'ha ancora, e per le righe storiche in cui lo status
            // era il letterale "completed" su ogni riga: un fallito
            // dichiarato da UNA delle due fonti resta fallito.
            let status_colonna: String = r.try_get("status").unwrap_or_default();
            let risposta = RispostaTool::da_testo_legacy(risultato.unwrap_or_default());
            Some(TentativoOsservato {
                firma: firma(&strumento, &input),
                strumento,
                fallito: status_colonna == "failed" || risposta.esito.e_fallito(),
                exit_code: risposta.exit_code,
                messaggio: risposta.testo,
            })
        })
        .collect()
}

/// Identita' della STRADA tentata: il tool, piu' il programma invocato quando il
/// tool ne esegue uno.
///
/// Il criterio confronta questa stringa per decidere se il run stava ripetendo:
/// senza il programma, tre `run_command` diversi sembrerebbero lo stesso
/// tentativo ripetuto tre volte — e la diagnosi direbbe «strada chiusa» a un run
/// che stava provando alternative.
fn firma(strumento: &str, input: &Value) -> String {
    match primo_token_comando(input) {
        Some(p) => format!("{strumento}({p})"),
        None => strumento.to_string(),
    }
}

/// Primo token della riga di comando dichiarata nell'input, se c'e'.
fn primo_token_comando(input: &Value) -> Option<String> {
    let riga = CAMPI_COMANDO
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))?;
    riga.split_whitespace().next().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_agent_graph::runtime::ports::{AgentStepStore, PersistedStep, StepStatus};
    use nexus_types::tool_outcome::tool_failure;
    use serde_json::json;

    use crate::agent_graph_adapter::agent_step_store::PgAgentStepStore;

    /// Scrive un passo con il PRODUTTORE di produzione (regola O): la riga di
    /// `agent_steps` la compone `PgAgentStepStore::persist_step`, non una INSERT
    /// scritta nel test. E' l'unico modo perche' questo test misuri la giunzione
    /// vera — se un domani il produttore cambiasse colonna o forma del blocco,
    /// qui diventerebbe rosso invece di restare verde su una tabella immaginaria.
    async fn passo(
        store: &PgAgentStepStore,
        run_id: Uuid,
        iterazione: i64,
        tool: &str,
        input: Value,
        risultato: &str,
    ) {
        store
            .persist_step(
                &run_id.to_string(),
                iterazione,
                0,
                PersistedStep {
                    tool_name: tool.to_string(),
                    tool_input: input,
                    tool_result: Some(risultato.to_string()),
                    // Il consumatore sotto misura il FALLIMENTO leggendo il
                    // risultato col ponte legacy, non questo campo. Lo status si
                    // deriva dallo STESSO ponte invece che da un letterale, cosi'
                    // la riga resta coerente col risultato passato senza che il
                    // test debba conoscere la forma del marker (regola O).
                    status: StepStatus::from_is_error(
                        nexus_types::tool_outcome::RispostaTool::da_testo_legacy(
                            risultato.to_string(),
                        )
                        .esito
                        .e_fallito(),
                    ),
                },
            )
            .await
            .expect("step persistito");
    }

    /// L'esito viene dalla COLONNA, non dal racconto: un tool migrato a
    /// `RispostaTool` (regola Q) scrive un testo senza marker, e il suo
    /// fallimento vive solo in `agent_steps.status`. Prima di leggere la
    /// colonna, questa diagnosi lo contava come riuscito — cioe' il primo tool
    /// migrato spariva dalla causa del timeout proprio mentre falliva.
    ///
    /// MUTAZIONE: togliere `status_colonna == "failed"` dall'OR in
    /// `tentativi_finali` fa tornare `NoFailureAtEnd` su un run il cui ultimo
    /// passo e' un fallimento dichiarato in colonna — il valore del difetto.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn il_fallimento_dichiarato_in_colonna_conta_anche_senza_marker(pool: PgPool) {
        let run_id = crate::test_support::seed_agent_run(&pool).await;
        let store = PgAgentStepStore::new(pool.clone());
        // Testo PULITO (nessun marker legacy), fallimento SOLO in colonna: e'
        // la forma che ogni tool migrato produce da oggi in avanti.
        store
            .persist_step(
                &run_id.to_string(),
                7,
                0,
                PersistedStep {
                    tool_name: "run_command".to_string(),
                    tool_input: json!({"command": "npm test"}),
                    tool_result: Some("3 test falliti su 41".to_string()),
                    status: StepStatus::Failed,
                },
            )
            .await
            .expect("step persistito");
        let causa = causa_del_timeout(&pool, run_id).await;
        assert_eq!(
            causa.key(),
            "last_attempt_failed",
            "un fallimento dichiarato in colonna deve contare senza marker nel testo"
        );
    }

    /// LA prova, sulla catena misurata il 02/08/2026 (sub-run a5f7419c): dopo
    /// `which jq` a vuoto e un accertamento riuscito, il budget finisce su
    /// `sudo apt-get update` che non puo' funzionare su questo host. Prima la
    /// chiusura diceva solo «timeout».
    ///
    /// MUTAZIONE: togliere `.rev()` dalla lettura (leggere la coda ma lasciarla
    /// in ordine decrescente) fa diventare "ultimo" il primo passo: la causa
    /// dichiarata sarebbe `which`, cioe' un errore vero raccontato al contrario.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn la_causa_nomina_l_ultimo_tentativo_fallito(pool: PgPool) {
        let run_id = crate::test_support::seed_agent_run(&pool).await;
        let store = PgAgentStepStore::new(pool.clone());
        passo(
            &store,
            run_id,
            12,
            "run_command",
            json!({"command": "which jq"}),
            "EXIT CODE: 1\nwhich: no jq in (/mingw64/bin:/usr/bin)",
        )
        .await;
        passo(
            &store,
            run_id,
            15,
            "run_command",
            json!({"command": "echo check"}),
            "EXIT CODE: 0\njq is NOT installed",
        )
        .await;
        passo(
            &store,
            run_id,
            16,
            "run_command",
            json!({"command": "sudo apt-get update"}),
            &tool_failure("[sudo] apt-get update fallito: binary nexus-sudo-runner non trovato"),
        )
        .await;

        let causa = causa_del_timeout(&pool, run_id).await;
        assert_eq!(causa.key(), "last_attempt_failed");
        let nota = causa.nota();
        assert!(nota.contains("run_command(sudo)"), "{nota}");
        assert!(nota.contains("nexus-sudo-runner"), "{nota}");
    }

    /// Lo stesso comando ritentato: la porta deve dare al criterio firme UGUALI,
    /// altrimenti la ripetizione non si vede.
    ///
    /// MUTAZIONE: far tornare a [`firma`] il solo `strumento` (togliere il primo
    /// token) tiene VERDE questo test ma rompe il gemello sotto — ed e' il
    /// motivo per cui i due casi stanno insieme.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn tre_tentativi_identici_sono_una_ripetizione(pool: PgPool) {
        let run_id = crate::test_support::seed_agent_run(&pool).await;
        let store = PgAgentStepStore::new(pool.clone());
        for i in 1..=3 {
            passo(
                &store,
                run_id,
                i,
                "run_command",
                json!({"command": "apt-get install -y jq"}),
                &tool_failure("apt-get: command not found"),
            )
            .await;
        }
        let causa = causa_del_timeout(&pool, run_id).await;
        match &causa {
            CausaTimeout::RepeatedFailures {
                tentativi, firma, ..
            } => {
                assert_eq!(*tentativi, 3);
                assert_eq!(firma, "run_command(apt-get)");
            }
            altro => panic!("attesa una ripetizione, ottenuto {altro:?}"),
        }
    }

    /// Tre gestori DIVERSI provati e falliti non sono una strada chiusa: e' un
    /// agente che cerca alternative. La firma deve distinguerli, o la diagnosi
    /// direbbe «bloccato» a chi stava lavorando.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn alternative_diverse_non_sono_una_ripetizione(pool: PgPool) {
        let run_id = crate::test_support::seed_agent_run(&pool).await;
        let store = PgAgentStepStore::new(pool.clone());
        for (i, cmd) in ["apt-get install jq", "brew install jq", "winget install jq"]
            .iter()
            .enumerate()
        {
            passo(
                &store,
                run_id,
                i as i64 + 1,
                "run_command",
                json!({ "command": cmd }),
                &tool_failure("non disponibile"),
            )
            .await;
        }
        assert_eq!(
            causa_del_timeout(&pool, run_id).await.key(),
            "last_attempt_failed"
        );
    }

    /// Un run senza passi: la causa non e' osservabile, e lo si dichiara. Un run
    /// che scade prima di fare qualunque cosa esiste (modello lento, primo turno
    /// mai completato) e non va confuso con uno che procedeva.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn senza_passi_la_causa_non_e_osservabile(pool: PgPool) {
        let run_id = crate::test_support::seed_agent_run(&pool).await;
        assert_eq!(causa_del_timeout(&pool, run_id).await.key(), "not_observable");
    }

    /// Il tempo finito su lavoro che procedeva: e' la sola diagnosi che rende
    /// legittima la domanda sul dimensionamento del budget. Se il criterio la
    /// collassasse con le altre, alzare il timeout resterebbe una scelta al buio.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn una_coda_riuscita_dichiara_lavoro_in_corso(pool: PgPool) {
        let run_id = crate::test_support::seed_agent_run(&pool).await;
        let store = PgAgentStepStore::new(pool.clone());
        passo(
            &store,
            run_id,
            1,
            "run_command",
            json!({"command": "npm test"}),
            "EXIT CODE: 0\n12 passing",
        )
        .await;
        let causa = causa_del_timeout(&pool, run_id).await;
        assert_eq!(causa.key(), "no_failure_at_end");
    }
}

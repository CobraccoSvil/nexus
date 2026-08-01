//! Remediation (capacita' 1): trigger automatico dell'agente Debugger su crash
//! runtime rilevato dal service_observer.
//!
//! Gated da `agent.observer.auto_diagnose_enabled` (default false). Anti-loop:
//! cooldown per firma errore + cap orario per progetto, su `service_diagnoses`.
//! Scope rigoroso al progetto (regola E). Riusa il punto unico di avvio agente
//! `spawn_agent_run` (regola L), inserendo un messaggio sintetico con un prompt
//! di debug esplicito (stesso pattern di "Risolvi con Nexus").

use nexus_events::event::ProjectEvent;
use uuid::Uuid;

use crate::agent_types::SupervisorMode;
use crate::chat_messages::{
    insert_message, session_has_active_run, spawn_agent_run, SpawnAgentParams, SpawnOutcome,
};
use crate::AppState;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn maybe_trigger_debugger(
    state: &AppState,
    auto_enabled: bool,
    cooldown_s: i64,
    max_per_hour: i64,
    project_id: Uuid,
    unit: &str,
    kind: &str,
    last_log: &str,
    sig: &str,
    diag_id: Option<Uuid>,
) {
    if !auto_enabled {
        return;
    }

    // Boot-grace (regola H, causa radice "la chat riparte da sola" dopo un deploy):
    // subito dopo un restart di mcp-core i servizi del progetto sono nel transitorio
    // di riavvio (porte ancora occupate, servizio non ancora in ascolto). L'observer
    // li vedrebbe "giu'" e li scambierebbe per crash, auto-triggerando un run di
    // auto-debug che nessuno ha chiesto (incidente Chat 11 Beaty-Book). Entro la
    // finestra dall'avvio del processo NON si auto-diagnostica: si lascia stabilizzare.
    if within_boot_grace(state).await {
        tracing::info!(
            "service_observer: boot-grace attivo, skip auto-debug per {} ({}) — mcp-core appena riavviato",
            unit,
            kind
        );
        return;
    }

    // Cooldown: stessa firma gia' diagnosticata entro la finestra.
    let recent: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM service_diagnoses \
         WHERE project_id = $1 AND unit = $2 AND error_signature_hash = $3 \
           AND triggered_run_id IS NOT NULL \
           AND ts > NOW() - make_interval(secs => $4)",
    )
    .bind(project_id)
    .bind(unit)
    .bind(sig)
    .bind(cooldown_s as f64)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    if recent > 0 {
        return;
    }

    // Un rimedio e' GIA' in corso su questa unit (diagnosi in 'diagnosing'):
    // niente secondo trigger. La verifica dell'esito riavvia il servizio due
    // volte e osserva per una finestra: in quei minuti l'observer vede
    // legittimamente un servizio che va giu' e torna su, e ogni riavvio produce
    // una firma NUOVA — quindi il cooldown-per-firma non frena. Senza questa
    // guardia il verificatore e un nuovo auto-debug lavorerebbero sullo stesso
    // servizio nello stesso momento, ciascuno riavviandolo sotto i piedi
    // dell'altro. Fratello dei guard su cooldown, cap orario e run attivo: la
    // diagnosi torna 'open' (o terminale) a verifica conclusa, e da li' il
    // trigger e' di nuovo possibile.
    let rimedio_in_corso: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM service_diagnoses \
         WHERE project_id = $1 AND unit = $2 AND signal_kind = 'crash' \
           AND status = 'diagnosing'",
    )
    .bind(project_id)
    .bind(unit)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    if rimedio_in_corso > 0 {
        tracing::debug!(
            unit = %unit,
            "service_observer: rimedio gia' in corso su questa unit, skip auto-debug"
        );
        return;
    }

    // Cap orario per progetto (anti-loop di run costosi).
    let last_hour: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM service_diagnoses \
         WHERE project_id = $1 AND triggered_run_id IS NOT NULL \
           AND ts > NOW() - INTERVAL '1 hour'",
    )
    .bind(project_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);
    if last_hour >= max_per_hour {
        tracing::warn!(
            "service_observer: cap diagnosi/ora ({}) raggiunto per progetto {}, skip auto-debug",
            max_per_hour,
            project_id
        );
        return;
    }

    // Owner del progetto (user_id per l'agent run).
    let owner: Option<Uuid> =
        sqlx::query_scalar("SELECT owner_user_id FROM projects WHERE id = $1")
            .bind(project_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let owner = match owner {
        Some(o) => o,
        None => return,
    };

    // Pool dati del progetto (separazione DB): chat_sessions e' migrata, va letta
    // sul DB del progetto. Non disponibile -> niente auto-debug per questo giro
    // (trigger best-effort, WARN + return).
    let proj_pool =
        match crate::project_db_routes::project_data_pool_from(&state.db, project_id).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    project_id = %project_id,
                    error = %e,
                    "service_observer: DB progetto non disponibile, skip auto-debug"
                );
                return;
            }
        };

    // Sessione chat esistente del progetto (non ne creiamo: l'auto-debug e'
    // opt-in e ha senso solo dove l'utente vede la conversazione).
    let session: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM chat_sessions WHERE project_id = $1 ORDER BY updated_at DESC LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&proj_pool)
    .await
    .ok()
    .flatten();
    let session = match session {
        Some(s) => s,
        None => {
            tracing::info!(
                "service_observer: nessuna sessione chat per progetto {}, skip auto-debug",
                project_id
            );
            return;
        }
    };

    // Guard anti-loop (stesso principio di process_resume, regola L): se sulla
    // sessione c'e' GIA' un run attivo — tipicamente un Debugger lanciato da un
    // crash precedente che sta ancora lavorando — NON spawnarne un altro. Lo
    // supererebbe via supersede (last-wins), uccidendo il run in corso e
    // innescando il loop di run che si superano a vicenda mentre il servizio
    // resta in crash-loop (es. dev server con porta occupata: ogni crash ha una
    // firma diversa, quindi il cooldown-per-firma non frena). Si lascia finire il
    // run attivo; al prossimo crash, se il servizio e' ancora giu', si potra'
    // ri-triggerare quando la sessione e' di nuovo libera.
    //
    // POOL DEL PROGETTO, non meta: agent_runs e' tabella migrata per-progetto
    // (separazione DB). Sul meta la tabella e' vuota a flag ON -> il guard non
    // scattava mai (stesso gap di process_resume, fix 2026-07-02).
    if session_has_active_run(&proj_pool, session).await {
        tracing::debug!(
            "service_observer: run gia' attivo sulla sessione {}, skip auto-debug per {} ({})",
            session,
            unit,
            kind
        );
        return;
    }

    // EVIDENZA GREZZA (regola M: fatti, non interpretazione). Prima il prompt
    // portava solo unit + log, e l'agente doveva riscoprire una chiamata tool
    // alla volta cio' che era gia' tutto leggibile: quali porte spettano a questo
    // servizio secondo il registro, chi le occupa DAVVERO adesso, con che comando
    // e da quale directory gira. Nel caso reale (frontend con la porta del
    // backend nel proprio .env) quei tre fatti, messi in fila, SONO la diagnosi.
    // Stesso raccoglitore che poi verifichera' l'esito: cio' che si mostra
    // all'agente e cio' su cui si giudica vengono dalla stessa fonte (regola L).
    let facts = crate::project_workspace::service_recovery::collect_service_facts(
        state, project_id, unit,
    )
    .await;
    // Il vincolo di governance sulle porte sta NEL prompt del task, non solo nei
    // system prompt: questo messaggio e' l'unico contratto del run di diagnosi
    // (canale sintetico, regola D "fuori chat"), e senza il vincolo esplicito
    // l'agente che non riesce a far ripartire il servizio AGGIRA. Misurato il
    // 31/07/2026 su bacheca-attivita: backend avviato come processo nudo sulla
    // porta 3001, FUORI dal bucket del progetto, mentre la porta allocata 24826
    // restava libera — esattamente il workaround che la governance esiste per
    // impedire.
    let content = format!(
        "Crash rilevato automaticamente nel servizio `{unit}` (tipo: {kind}).\n\n\
         Fatti osservati al momento del guasto:\n```\n{}\n```\n\n\
         Log rilevante:\n```\n{last_log}\n```\n\n\
         Diagnostica la causa radice con metodo scientifico (ipotesi -> falsificazione \
         -> fix -> verifica): leggi i log completi e i file coinvolti prima di concludere, \
         poi proponi la correzione.\n\n\
         Vincolo di governance (porte e servizi): i servizi del progetto si avviano \
         ESCLUSIVAMENTE con i tool servizio (`run_service` / riavvio del servizio), che \
         li legano alla porta ALLOCATA nel registro. Mai avviare processi nudi su porte \
         scelte a mano o fuori dal bucket del progetto: un servizio su una porta non \
         allocata e' un guasto mascherato, non una riparazione. Se la porta allocata \
         risulta occupata, identifica l'occupante e liberala con i tool di gestione \
         processi/porte; non ripiegare su un'altra porta.",
        facts.render()
    );
    let meta = serde_json::json!({
        "kind": "auto_diagnosis",
        "synthetic": true,
        "service_unit": unit,
        "error_kind": kind,
        "source": "service_observer",
    });

    let user_message_id =
        match insert_message(&state.db, session, project_id, "user", &content, meta, None).await {
            Ok(id) => id,
            Err(_) => {
                tracing::warn!("service_observer: insert messaggio sintetico fallito");
                return;
            }
        };

    let system_context = crate::prompt_templates::get_template_or_default(
        &state.db,
        &state.template_cache,
        "system.nexus_base",
    )
    .await;

    // Modello del rimedio dal purpose tier-aware 'auto_remediation' (mig 0626,
    // regola G): un run di debug affidato al default piccolo del routing
    // fallisce e brucia i tentativi. Se il purpose non e' risolvibile ->
    // (None, None): decide il routing di default, il rimedio non si blocca.
    let (provider_override, model_override) = crate::internal_routing::purpose_override_or_default(
        state,
        crate::internal_routing::PURPOSE_AUTO_REMEDIATION,
    )
    .await;

    let params = SpawnAgentParams {
        user_id: owner,
        session_id: session,
        project_id,
        user_message_id,
        content,
        // Eredita la modalita' della sessione (mig 0371) invece di hardcodare
        // Confirm, cosi' l'auto-debug rispetta la scelta dell'utente.
        // chat_sessions e' migrata: lettura dal pool progetto gia' risolto,
        // non dal meta (dove la riga non esiste e tornava sempre il default).
        automation_mode: crate::chat_messages::read_session_automation_mode(&proj_pool, session)
            .await,
        supervisor_mode: SupervisorMode::None,
        profile_prompt_block: String::new(),
        system_context,
        // Come sopra (rimedio di sistema): preferenza, mai vincolo.
        provider_choice: crate::orchestrator::ProviderChoice::resolve(
            None,
            crate::orchestrator::ProviderOverrideMode::Preferred,
            provider_override.as_deref(),
        ),
        model_override,
        profile_provider: None,
        profile_model: None,
        attachments: Vec::new(),
        // Forza l'agente Debugger (il prompt esplicito nel content rinforza).
        nexus_agent_type_hint: Some("debugger".to_string()),
    };

    match spawn_agent_run(state, params).await {
        SpawnOutcome::Started(result) => {
            let run_id = result.run_id;
            if let Some(id) = diag_id {
                let _ = sqlx::query(
                "UPDATE service_diagnoses SET status = 'diagnosing', triggered_run_id = $1 WHERE id = $2",
            )
            .bind(run_id)
            .bind(id)
            .execute(&state.db)
            .await;
            }
            nexus_events::dispatcher::emit_global(
                project_id,
                ProjectEvent::ServiceDiagnosisStarted {
                    unit: unit.to_string(),
                    run_id: run_id.to_string(),
                },
            );
            tracing::info!(
                "service_observer: auto-debug avviato run={} per {} ({})",
                run_id,
                unit,
                kind
            );

            spawn_verifica_esito(state.clone(), project_id, unit.to_string(), run_id, diag_id);
        }
        SpawnOutcome::Disambiguation(_) => {
            // Non dovrebbe accadere: passiamo nexus_agent_type_hint="debugger"
            // che forza l'AgentType e salta il gate di disambiguazione.
            tracing::warn!(
                "service_observer: disambiguazione inattesa per {} ({}), nessun auto-debug avviato",
                unit,
                kind
            );
        }
        SpawnOutcome::NotStarted => {
            // Progetto non caricabile o altro fallback: nessun run (come prima).
        }
    }
}

/// CHIUSURA DEL CICLO sul contratto oggettivo (regola M/O): quando il debugger
/// termina, riavvia il servizio e VERIFICA che risponda sulla porta che il
/// registro assegna a lui e che continui a farlo dopo un ulteriore riavvio, poi
/// scrive quell'esito sulla diagnosi.
///
/// Prima qui c'era il solo riavvio, e la chiusura arrivava dall'observer, che
/// marcava `resolved` appena il marcatore d'avvio cambiava — cioe' appena il
/// processo NASCEVA. Il caso reale del 28/07 (gestione-spese) e' morto subito
/// dopo quel "riavvio effettuato", e il guasto e' proseguito senza piu' alcuna
/// riga aperta a testimoniarlo.
///
/// In BACKGROUND: la verifica dura minuti (due riavvii, due finestre di
/// osservazione) e non deve trattenere il ciclo dell'observer.
fn spawn_verifica_esito(
    state: AppState,
    project_id: Uuid,
    unit: String,
    run_id: Uuid,
    diag_id: Option<Uuid>,
) {
    tokio::spawn(async move {
        if !attendi_fine_run(&state, project_id, run_id).await {
            return;
        }
        let (verdict, facts) = crate::project_workspace::service_recovery::restart_and_verify(
            &state, project_id, &unit,
        )
        .await;
        match diag_id {
            Some(id) => {
                // `retry_left: false`: qui l'AI ha gia' lavorato sul problema e il
                // contratto non e' soddisfatto lo stesso. Un altro giro identico
                // non aggiunge niente — la riga va in stato terminale con
                // l'evidenza, che e' cio' che un umano deve vedere.
                let esito = crate::project_workspace::service_recovery::RepairOutcome::Judged {
                    verdict: verdict.clone(),
                    retry_left: false,
                };
                let scritto = crate::project_workspace::service_recovery::apply_repair_outcome(
                    &state.db,
                    id,
                    &esito,
                    &facts.render(),
                )
                .await;
                tracing::info!(
                    unit = %unit, run_id = %run_id, verdetto = ?verdict, stato_diagnosi = ?scritto,
                    "service_remediation: esito verificato sul servizio, non sul riavvio"
                );
            }
            // Nessuna diagnosi da chiudere (trigger senza riga): restano il
            // verdetto a log e la notifica, cosi' un rimedio che non ha riparato
            // non passa comunque per riuscito.
            None => tracing::info!(
                unit = %unit, run_id = %run_id, verdetto = ?verdict,
                "service_remediation: esito verificato (nessuna diagnosi agganciata)"
            ),
        }
        crate::project_workspace::service_recovery::announce_recovery(
            project_id, &unit, &verdict, diag_id,
        );
    });
}

/// Attende che il run del debugger termini (max ~5 min). `false` se non si e'
/// potuto nemmeno guardare: il DB del progetto — dove `agent_runs` e' migrata —
/// non e' raggiungibile, e senza sapere se l'agente ha finito non ha senso
/// riavviargli il servizio sotto i piedi.
///
/// Lo stato del run e' il segnale strutturato `is_active_run_status` (punto
/// unico, regola M), mai il testo dell'ultimo messaggio.
async fn attendi_fine_run(state: &AppState, project_id: Uuid, run_id: Uuid) -> bool {
    let Ok(proj_pool) =
        crate::project_db_routes::project_data_pool_from(&state.db, project_id).await
    else {
        tracing::warn!(
            project_id = %project_id,
            "service_observer: DB progetto non disponibile, salto attesa e verifica post-debug"
        );
        return false;
    };
    for _ in 0..60u32 {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM agent_runs WHERE id = $1")
                .bind(run_id)
                .fetch_optional(&proj_pool)
                .await
                .ok()
                .flatten();
        match status.as_deref() {
            Some(s) if crate::agent_types::is_active_run_status(s) => continue,
            None => continue,
            _ => break, // terminato (completed/failed/...)
        }
    }
    true
}

/// `true` se mcp-core e' stato avviato da meno di `agent.observer.boot_grace_seconds`
/// (regola G, default 90s): finestra in cui gli auto-trigger di remediation restano
/// inerti perche' i servizi osservati sono nel transitorio di riavvio da deploy e i
/// loro segnali (porte occupate, non-listening) non distinguono un crash reale dalla
/// stabilizzazione. Punto unico (regola L): ogni auto-remediation che nasce
/// dall'osservazione dei servizi delega a questa guardia. `boot_at` e' timbrato in
/// `build_app_state`.
pub(crate) async fn within_boot_grace(state: &AppState) -> bool {
    let grace = crate::settings::get_setting(&state.db, "agent.observer.boot_grace_seconds")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(90);
    state.boot_at.elapsed().as_secs() < grace
}

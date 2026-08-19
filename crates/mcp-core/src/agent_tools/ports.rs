//! Fix M51: tool MCP `request_port` per agenti.
//! Esteso nel PR hardening per usare `find_or_allocate_port` (quota check + audit).
//!
//! L'agente AI chiama questo tool quando deve scegliere una porta per un
//! servizio del progetto (al posto di hardcodare 3002/5173 o di costruire
//! curl shell verso l'endpoint REST allocate-port).
//!
//! Comportamento, nell'ORDINE in cui `find_or_allocate` lo esegue:
//! 1. Idempotenza: se esiste gia un'allocazione con la stessa label, ritorna
//!    quella porta (adottando o riallocando secondo chi la occupa)
//! 2. Quota: `security::quotas::check_can_allocate_port` (max_ports per progetto)
//! 3. Altrimenti alloca dal bucket deterministico tramite `find_free_project_port`
//! 4. INSERT in `nexus_port_allocations` con allocation_mode='dynamic'
//! 5. Audit allocato in `nexus_resource_audit`
//! 6. Emit `ProjectEvent::PortAllocated` per i pannelli UI
//!
//! Ritorna JSON: {"port": <num>, "label": "<lbl>", "allocation_mode":
//! "existing" | "dynamic" | "adopted" | "reallocated"} — i quattro valori che
//! `AllocatedPort::mode` puo' assumere. L'elenco ne dichiarava due, e gli altri
//! due li produce il ramo idempotente, cioe' quello piu' frequente.

use super::AgentToolContext;
use crate::project_workspace::allocate_port::{
    AllocatedPort, ErroreAllocazione, RichiedenteAllocazione,
};
use nexus_agent_tools::{
    input_contract::InputTool,
    tool_inputs::{NexusListPortsInput, RequestPortInput},
};
use nexus_types::tool_outcome::RispostaTool;
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::Row;

pub async fn tool_request_port(ctx: &AgentToolContext, input: &Value) -> RispostaTool {
    let params = match RequestPortInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    // Il contratto pretende `label` presente; la STRINGA VUOTA (o di soli spazi)
    // lo soddisfa e non e' un'identita' di servizio: `find_or_allocate` la
    // rifiuterebbe comunque, ma qui il messaggio nomina il campo e mostra i
    // valori attesi, che e' cio' che rende il fallimento davvero rimediabile.
    let label = params.label.trim();
    if label.is_empty() {
        return RispostaTool::fallito_rimediabile(
            "[Errore: 'label' non puo' essere vuota. Passa il nome logico del servizio, \
             es. 'backend', 'frontend', 'api'.]",
        );
    }
    let label = label.to_string();

    // find_or_allocate_port applica quota check, idempotency, audit. Vedi
    // crates/mcp-core/src/project_workspace/allocate_port.rs.
    match crate::project_workspace::find_or_allocate_port(
        &ctx.db,
        &ctx.port_registry,
        ctx.project_id,
        &label,
        richiedente_del_contesto(ctx),
    )
    .await
    {
        Ok(alloc) => {
            // Emit evento dispatcher per i pannelli UI (riusa transport esistente)
            nexus_events::dispatcher::emit(
                &ctx.project_channels,
                ctx.project_id,
                nexus_events::event::ProjectEvent::PortAllocated {
                    port: alloc.port as i32,
                    label: label.clone(),
                    pid: None,
                },
            );

            esito_allocazione(&label, &alloc)
        }
        Err(e) => fallimento_allocazione(&label, &e),
    }
}

/// CHI sta chiedendo la porta, cioe' che cosa la terra' prenotata finche' il
/// servizio non parte (mig 0741).
///
/// `ctx.core.run_id` e' `None` fuori dal grafo nativo (server gRPC, dispatch
/// legacy): li' non si finge una prenotazione, si dichiara che non c'e' — e
/// l'esito lo dira' al chiamante invece di lasciargli un numero che sembra
/// un'assegnazione.
fn richiedente_del_contesto(ctx: &AgentToolContext) -> RichiedenteAllocazione {
    match ctx.core.run_id {
        Some(run_id) => RichiedenteAllocazione::Run(run_id),
        None => RichiedenteAllocazione::FuoriDaUnRun,
    }
}

/// L'ESITO di un'allocazione riuscita come lo riceve il modello.
///
/// # Perche' un campo e non solo un numero (regola Q)
///
/// Ritornare la porta e basta E' una promessa: «questa porta e' del progetto».
/// Fino alla mig 0741 quella promessa aveva due significati indistinguibili —
/// una riga trattenuta e una riga che il port_gc avrebbe raccolto entro cinque
/// minuti — e il modello riceveva lo stesso identico JSON in entrambi i casi.
/// Misurato il 18/08/2026 su biblioteca-18-08: quattro chiamate riuscite, zero
/// righe sopravvissute, e il gate duale che poi rifiuta l'avvio perche' nel
/// registro non c'e' nessuna allocazione per quel servizio.
///
/// `tenuta` e' il campo in cui le due situazioni smettono di coincidere, e il
/// testo si compone DA quel campo — mai il contrario. Non e' un fallimento:
/// la porta esiste ed e' usabile adesso; e' una promessa piu' debole, e va
/// detta, perche' il rimedio del modello e' diverso (avvia il servizio subito,
/// invece di rimandare).
fn esito_allocazione(label: &str, alloc: &AllocatedPort) -> RispostaTool {
    let esito = json!({
        "port": alloc.port,
        "label": label,
        "allocation_mode": alloc.mode,
        "tenuta": alloc.tenuta.as_str(),
    });
    if alloc.tenuta.e_ancorata() {
        return RispostaTool::riuscito(esito.to_string());
    }
    RispostaTool::riuscito(format!(
        "{esito}\n[Attenzione: la porta {} e' registrata ma NON e' trattenuta da nulla \
         (tenuta: {}). Nessun run la prenota e nessun servizio la usa ancora: il \
         garbage collector delle porte la rilascera' se nessuno si mette in ascolto. \
         Avvia il servizio su questa porta ora, oppure rifai la richiesta quando sei \
         pronto ad avviarlo.]",
        alloc.port,
        alloc.tenuta.as_str()
    ))
}

/// Il fallimento di `find_or_allocate` come lo riceve il modello.
///
/// La natura NON si ricostruisce qui: la DICHIARA [`ErroreAllocazione::natura`],
/// cioe' il punto in cui la causa e' ancora nota. Finche' quell'errore era una
/// `String`, l'unica alternativa a rileggerne il messaggio (regola M al
/// contrario) era dichiararle tutte dello stesso tipo — e le due cause
/// raggiungibili sono di tipo OPPOSTO: la quota superata non si supera
/// ripetendo, la tabella dei listener non interrogabile passa da sola. Su
/// quest'ultima il modello riceveva la direttiva «ripeterla non cambiera'
/// l'esito» accanto a un testo che dice «riprova fra poco».
///
/// E' una funzione e non il corpo del braccio `match` per poterla provare senza
/// un DB, partendo dall'errore che la produzione produce (regola O).
fn fallimento_allocazione(label: &str, e: &ErroreAllocazione) -> RispostaTool {
    RispostaTool::fallito(format!("[Errore allocazione porta per label '{label}': {e}]"))
        .con_natura(e.natura())
}

/// Tool READ-ONLY per i task di verifica/audit della gestione porte. Non alloca
/// nulla: legge il bucket deterministico del progetto e le allocazioni
/// registrate in `nexus_port_allocations`. Risolve l'incidente in cui un task
/// "verifica le porte del progetto" non aveva alcun tool per ispezionare lo
/// stato governato e finiva per dedurre porte hardcoded leggendo i sorgenti.
pub async fn tool_nexus_list_ports(ctx: &AgentToolContext, input: &Value) -> RispostaTool {
    use crate::project_workspace::services::{
        project_bucket_range, PROJECT_PORT_RANGE_END, PROJECT_PORT_RANGE_START,
    };

    // Il tool non ha parametri: la lettura serve a rifiutare cio' che il
    // catalogo non promette, invece di eseguire ignorandolo — un campo scartato
    // in silenzio fa credere al modello di aver filtrato qualcosa.
    if let Err(risposta) = NexusListPortsInput::leggi(input) {
        return risposta;
    }

    let (bucket_start, bucket_end) = project_bucket_range(&ctx.project_id);

    // `service_unit` non e' piu' nel SELECT: nessuno lo legge (vedi
    // `riga_allocazione`), e una colonna che si chiede e non si usa fa credere
    // al lettore che esca ancora.
    let rows = sqlx::query(
        "SELECT port, label, allocation_mode, created_at \
         FROM nexus_port_allocations WHERE project_id = $1 ORDER BY port",
    )
    .bind(ctx.project_id)
    .fetch_all(ctx.db.as_ref())
    .await;

    let allocations: Vec<Value> = match rows {
        Ok(rows) => rows.iter().map(riga_allocazione).collect(),
        // Il DB non risponde o la query e' rotta: l'agente non ha modo di
        // rimediare riformulando la chiamata (non ha parametri da correggere).
        Err(e) => {
            return RispostaTool::fallito_di_sistema(format!(
                "[Errore lettura allocazioni porte: {e}]"
            ));
        }
    };

    // Nessuna allocazione registrata e' una RISPOSTA, non un fallimento: il
    // progetto non ha ancora chiesto porte, e il bucket resta l'informazione
    // utile. `count: 0` lo dice nei campi.
    let esito = json!({
        "bucket": { "start": bucket_start, "end": bucket_end },
        "nexus_range": { "min": PROJECT_PORT_RANGE_START, "max": PROJECT_PORT_RANGE_END },
        "allocations": allocations,
        "count": allocations.len(),
        "hint": "Sola lettura. Per ottenere una NUOVA porta usa request_port(label=...). \
                 Le porte hardcoded fuori bucket, o nel bucket ma non allocate (inclusi i \
                 fallback tipo `process.env.PORT || 5000`), vengono rifiutate in scrittura e \
                 i processi su porte non allocate vengono terminati dal port enforcer."
    });
    RispostaTool::riuscito(esito.to_string())
}

/// Una riga di `nexus_port_allocations` come la vede l'agente.
///
/// `service_unit` NON esce accanto a `label`.
///
/// E' un valore DERIVATO (`service_unit_name` = `{slug}-{label}.service`) e
/// stava nello stesso oggetto dell'identificatore primitivo, senza nulla che li
/// distinguesse. Misurato su DUE progetti indipendenti: il modello lo ha copiato
/// dentro `label` alla chiamata seguente (agenda-corsi a 76 secondi,
/// bacheca-attivita a 47 tre giorni prima), e da li' il ciclo si autoalimenta —
/// 10 righe su 26 con label gia' derivata, fino alla terza generazione. Il
/// difetto non era la disattenzione del modello: era offrirgli due stringhe
/// intercambiabili e chiamarne una sola "identita'".
///
/// Chi ha bisogno del nome unit lo RICOSTRUISCE dal punto unico a partire da
/// label e slug; qui non serve a nessuna decisione che l'agente debba prendere.
fn riga_allocazione(r: &PgRow) -> Value {
    json!({
        "port": r.try_get::<i32, _>("port").unwrap_or(0),
        "label": r.try_get::<String, _>("label").unwrap_or_default(),
        "allocation_mode": r.try_get::<String, _>("allocation_mode").unwrap_or_default(),
        "created_at": r
            .try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")
            .map(|t| t.to_rfc3339())
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_types::tool_outcome::NaturaFallimento;

    /// La direttiva che il modello riceve e il testo dell'errore devono dire la
    /// STESSA cosa. Con la natura dichiarata a mano (`fallito_di_sistema` per
    /// ogni causa) questo caso arrivava come «ripeterla non cambiera' l'esito»
    /// accanto a un testo che dice «riprova fra poco»: due istruzioni opposte
    /// nella stessa risposta.
    ///
    /// Test di mutazione: rimettere una natura fissa in `fallimento_allocazione`
    /// fa rosseggiare questo caso e non gli altri due.
    #[test]
    fn la_natura_del_fallimento_viene_da_chi_conosce_la_causa() {
        let transitorio =
            fallimento_allocazione("backend", &ErroreAllocazione::StatoPorteNonInterrogabile);
        assert_eq!(
            transitorio.natura,
            Some(NaturaFallimento::Transitorio),
            "la tabella dei listener non interrogabile passa da sola: ritentare e' la strategia giusta"
        );
        assert!(
            transitorio.testo.contains("riprova fra poco"),
            "il testo e la direttiva devono concordare, testo: {}",
            transitorio.testo
        );

        let sistema = fallimento_allocazione(
            "backend",
            &ErroreAllocazione::QuotaSuperata("quota porte esaurita".to_string()),
        );
        assert_eq!(
            sistema.natura,
            Some(NaturaFallimento::DelSistema),
            "la quota non si alza ripetendo la chiamata"
        );

        let rimediabile = fallimento_allocazione("", &ErroreAllocazione::LabelVuota);
        assert_eq!(rimediabile.natura, Some(NaturaFallimento::Rimediabile));

        // La porta scelta ma non registrata NON e' piu' un successo: prima
        // `find_or_allocate` inghiottiva l'errore di scrittura e ritornava `Ok`,
        // quindi il tool dichiarava riuscito l'ottenimento di una porta che il
        // port enforcer avrebbe poi terminato.
        let fantasma = fallimento_allocazione(
            "backend",
            &ErroreAllocazione::RegistroNonScritto {
                porta: 31904,
                causa: "connessione al DB caduta".to_string(),
            },
        );
        assert!(fantasma.esito.e_fallito());
        assert_eq!(fantasma.natura, Some(NaturaFallimento::DelSistema));
        assert!(
            fantasma.testo.contains("31904"),
            "il testo deve nominare la porta rimasta fuori dal registro: {}",
            fantasma.testo
        );
    }

    /// Il contratto si prova come lo prova la produzione: passando all'`InputTool`
    /// il `Value` che il modello manda, non costruendo la struct a mano.
    #[test]
    fn il_contratto_di_nexus_list_ports_accetta_il_vuoto_e_rifiuta_l_ignoto() {
        assert!(
            NexusListPortsInput::leggi(&json!({})).is_ok(),
            "il tool non ha parametri: l'oggetto vuoto e' la chiamata legittima"
        );
        let ignoto = NexusListPortsInput::leggi(&json!({"label": "backend"}));
        assert!(
            ignoto.is_err(),
            "un filtro che il catalogo non promette non va eseguito ignorandolo"
        );
    }

    /// La stringa vuota SODDISFA il contratto (`label` e' presente): il controllo
    /// nell'handler non e' ridondante, ed e' l'unico fallimento del tool che
    /// l'agente possa correggere da solo.
    #[test]
    fn il_contratto_di_request_port_non_ferma_la_label_vuota() {
        let letto = RequestPortInput::leggi(&json!({"label": "   "}))
            .expect("'label' e' presente: il contratto la accetta");
        assert!(letto.label.trim().is_empty());
        assert!(
            RequestPortInput::leggi(&json!({})).is_err(),
            "'label' e' obbligatoria nel catalogo"
        );
    }
}

//! Bucket deterministico delle porte di progetto + porte riservate Nexus.
//! Punto unico (regola L), estratto da project_workspace::services nello
//! split 7.4 fase B: il sandbox (in questo crate) valida le PORT contro
//! il bucket; mcp-core re-esporta da project_workspace::services.

use uuid::Uuid;

/// Porte riservate da Nexus e dai suoi servizi di infrastruttura.
/// I processi di progetto NON devono mai usare queste porte.
///
/// Range riservato HTTP:  4000–4079  (microservizi Nexus)
/// Range riservato gRPC:  4100–4139  (canali gRPC interni, target migrazione)
/// Porte gRPC attuali:    50051–50501 (in uso finché non migrati)
/// Progetti utente:       20000-39999 (bucket assegnati da find_free_project_port)
pub const NEXUS_RESERVED_PORTS: &[u16] = &[
    // Porte di sistema
    80, 443,
    // ── HTTP Nexus (4000-4079) ─────────────────────────────────────────────
    4000, // mcp-core HTTP
    4001, // web-ide (target migrazione da 3000)
    4010, // admin-service
    4020, // ex chat-service (crate rimosso, porta resta riservata nel bucket)
    4030, // doc-service
    4040, // ex billing-service (crate rimosso, porta resta riservata nel bucket)
    4050, // plugin-service
    4060, // nexus-gateway
    4070, // neural-core REST (target migrazione da 8001)
    // ── gRPC interno Nexus (4100-4139, target migrazione) ─────────────────
    4100, // neural-core gRPC (target da 50051)
    4110, // tool-runner gRPC (target da 50500)
    4120, // agent-router gRPC (target da 50501)
    4130, // presidio gRPC (target da 50052)
    // ── web-ide attuale ───────────────────────────────────────────────────
    3000, // Nexus web-ide (attuale)
    // ── Porte gRPC attuali (porte alte, in uso finché non migrati) ────────
    8001,  // neural-core REST (attuale)
    50051, // neural-core gRPC
    50052, // presidio gRPC
    50500, // tool-runner gRPC (reale, vedi mig 0239)
    50501, // agent-router gRPC (reale, vedi mig 0190/0239)
    // ── Database e infrastruttura ─────────────────────────────────────────
    5432, 5433, 5434, // PostgreSQL (5432 host, 5433 cluster meta, 5434 cluster app)
    6333, 6334, // Qdrant REST + gRPC
    6379, // Redis
    8080, // nginx interno
    // ── Monitoring e observability ────────────────────────────────────────
    3001,  // Grafana
    4055,  // browser-bridge-mcp
    4317,  // OpenTelemetry Collector gRPC
    4318,  // OpenTelemetry Collector HTTP
    9090,  // Prometheus
    16686, // Jaeger UI
];

/// Range dedicato ai servizi dei progetti gestiti (deve evitare conflitti con Nexus e con servizi host comuni).
/// Scelta conservativa: porte alte non privilegiate, fuori dal range Nexus e fuori dai DB.
pub const PROJECT_PORT_RANGE_START: u16 = 20000;
pub const PROJECT_PORT_RANGE_END: u16 = 39999;
/// Numero porte per progetto nel bucket deterministico.
pub const PROJECT_PORT_BUCKET_SIZE: u16 = 50;

pub fn project_bucket_start(project_id: &Uuid) -> u16 {
    // Hash stabile: usa i primi 8 byte (big-endian) del UUID.
    let b = project_id.as_bytes();
    let mut v: u64 = 0;
    for &byte in b.iter().take(8) {
        v = (v << 8) | (byte as u64);
    }
    let buckets: u64 = ((PROJECT_PORT_RANGE_END - PROJECT_PORT_RANGE_START + 1) as u64)
        / (PROJECT_PORT_BUCKET_SIZE as u64);
    let idx = if buckets == 0 { 0 } else { v % buckets };
    PROJECT_PORT_RANGE_START + (idx as u16) * PROJECT_PORT_BUCKET_SIZE
}

/// Ultima porta INCLUSA nel bucket del progetto.
///
/// Esiste per togliere di mezzo la doppia convenzione che girava nei call site:
/// alcuni scrivevano `start + PROJECT_PORT_BUCKET_SIZE` (estremo ESCLUSO), altri
/// `start + PROJECT_PORT_BUCKET_SIZE - 1` (INCLUSO). Due modi di dire la stessa
/// cosa sono due occasioni di sbagliare il confronto sull'estremo.
pub fn project_bucket_end(project_id: &Uuid) -> u16 {
    project_bucket_start(project_id).saturating_add(PROJECT_PORT_BUCKET_SIZE - 1)
}

/// Estremi INCLUSIVI del bucket, per chi deve mostrarli (messaggi d'errore,
/// pannello Porte, prompt di remediation).
pub fn project_bucket_range(project_id: &Uuid) -> (u16, u16) {
    (
        project_bucket_start(project_id),
        project_bucket_end(project_id),
    )
}

/// True se `port` appartiene al bucket deterministico di QUESTO progetto.
///
/// Punto unico (regola L) della domanda "questa porta e' sua?": prima ogni
/// consumatore (sandbox, port_enforcer, resource_resolver, port_recovery)
/// ricalcolava gli estremi per conto proprio.
pub fn port_in_project_bucket(project_id: &Uuid, port: u16) -> bool {
    let (start, end) = project_bucket_range(project_id);
    (start..=end).contains(&port)
}

/// Esito della domanda "questa porta e' registrabile come porta DI QUESTO
/// progetto?". Segnale STRUTTURATO (regola M): i chiamanti decidono su questa
/// variante e ne ricavano il messaggio, non il contrario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortRegistrability {
    /// Nel bucket del progetto: registrabile.
    Registrable,
    /// Riservata a Nexus o all'infrastruttura condivisa.
    Reserved,
    /// Fuori dal range globale dei progetti.
    OutOfProjectRange,
    /// Nel range dei progetti, ma NEL BUCKET DI UN ALTRO progetto: e' esattamente
    /// la collisione che il bucket deterministico esiste per impedire (regola E).
    OutOfProjectBucket { bucket_start: u16, bucket_end: u16 },
}

impl PortRegistrability {
    /// Motivo in forma breve e stabile, per log e audit (mai per decidere).
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Registrable => "registrable",
            Self::Reserved => "reserved",
            Self::OutOfProjectRange => "out_of_project_range",
            Self::OutOfProjectBucket { .. } => "out_of_project_bucket",
        }
    }
}

/// Classifica `port` RISPETTO AL PROGETTO che la esporrebbe.
///
/// Punto unico (regola L) del criterio di registrabilita': il rilevamento
/// porta-da-output dei servizi e la registrazione in `nexus_port_allocations`
/// delegano qui invece di replicare lista riservata, range o bucket.
///
/// Il `project_id` non e' un ornamento: senza, l'unica domanda ponibile era "e'
/// di QUALCHE progetto?", e una porta del bucket altrui passava il controllo. Le
/// porte d'infrastruttura condivise (es. Postgres :5434) compaiono nei log come
/// destinazione di CONNESSIONE, non come listener del servizio, e restano fuori.
pub fn classify_project_port(project_id: &Uuid, port: u16) -> PortRegistrability {
    if NEXUS_RESERVED_PORTS.contains(&port) {
        return PortRegistrability::Reserved;
    }
    if !(PROJECT_PORT_RANGE_START..=PROJECT_PORT_RANGE_END).contains(&port) {
        return PortRegistrability::OutOfProjectRange;
    }
    let (bucket_start, bucket_end) = project_bucket_range(project_id);
    if !(bucket_start..=bucket_end).contains(&port) {
        return PortRegistrability::OutOfProjectBucket {
            bucket_start,
            bucket_end,
        };
    }
    PortRegistrability::Registrable
}

/// True se `port` puo' essere REGISTRATA come porta esposta da un servizio di
/// questo progetto. Scorciatoia booleana di [`classify_project_port`]: chi deve
/// spiegare il rifiuto usa direttamente la classificazione.
pub fn is_project_registrable_port(project_id: &Uuid, port: u16) -> bool {
    classify_project_port(project_id, port) == PortRegistrability::Registrable
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Progetto "gestione-spese", l'incidente da cui nasce il vincolo sul bucket.
    fn progetto_gestione_spese() -> Uuid {
        Uuid::parse_str("39802bb6-9540-4d70-82c1-fcf35c3a9b65").unwrap()
    }

    #[test]
    fn bucket_deterministico_e_stabile_e_dentro_il_range() {
        // `project_bucket_start` era senza test: e' condivisa con il sandbox e con
        // l'allocatore, quindi la sua stabilita' e' un contratto, non un dettaglio.
        let progetto = progetto_gestione_spese();
        let (start, end) = project_bucket_range(&progetto);
        assert_eq!(
            (start, end),
            (33600, 33649),
            "il bucket di gestione-spese e' quello da cui l'allocatore aveva gia' \
             assegnato 33649: cambiarlo sposterebbe le porte di tutti i progetti"
        );
        // Stabilita': la stessa domanda, due volte, la stessa risposta.
        assert_eq!(project_bucket_start(&progetto), start);
        // L'estremo incluso non esce mai dal range globale.
        assert!(end <= PROJECT_PORT_RANGE_END);
        assert_eq!(end - start + 1, PROJECT_PORT_BUCKET_SIZE);
    }

    #[test]
    fn porta_del_bucket_altrui_non_e_registrabile() {
        // Caso reale: l'agente ha scritto a mano `process.env.PORT || 20001` e il
        // servizio e' partito su 20001. La porta e' nel range dei progetti, quindi
        // il vecchio predicato (che vedeva solo il range globale) la accettava:
        // 20001 sta pero' nel bucket 20000-20049, di un ALTRO progetto.
        let progetto = progetto_gestione_spese();
        assert_eq!(
            classify_project_port(&progetto, 20001),
            PortRegistrability::OutOfProjectBucket {
                bucket_start: 33600,
                bucket_end: 33649,
            }
        );
        assert!(!is_project_registrable_port(&progetto, 20001));

        // La porta che Nexus aveva assegnato per la via corretta e' registrabile.
        assert!(is_project_registrable_port(&progetto, 33649));
        assert!(port_in_project_bucket(&progetto, 33600));
        assert!(!port_in_project_bucket(&progetto, 33650));
    }

    #[test]
    fn riservate_e_fuori_range_restano_distinte() {
        let progetto = progetto_gestione_spese();
        // Postgres del cluster app: nei log e' una destinazione di CONNESSIONE.
        assert_eq!(
            classify_project_port(&progetto, 5434),
            PortRegistrability::Reserved
        );
        assert_eq!(
            classify_project_port(&progetto, 3000),
            PortRegistrability::Reserved
        );
        assert_eq!(
            classify_project_port(&progetto, 19999),
            PortRegistrability::OutOfProjectRange
        );
        assert_eq!(
            classify_project_port(&progetto, 40000),
            PortRegistrability::OutOfProjectRange
        );
    }

    #[test]
    fn ogni_porta_del_range_appartiene_a_un_solo_bucket() {
        // L'invariante che rende il bucket una difesa: due progetti con bucket
        // diversi non possono dirsi entrambi proprietari della stessa porta.
        let a = progetto_gestione_spese();
        let b = Uuid::parse_str("00000000-0000-4000-8000-000000000000").unwrap();
        let (start_a, _) = project_bucket_range(&a);
        let (start_b, _) = project_bucket_range(&b);
        assert_ne!(start_a, start_b, "bucket distinti per il caso di prova");
        for port in [start_a, start_b] {
            assert_ne!(
                port_in_project_bucket(&a, port),
                port_in_project_bucket(&b, port),
                "la porta {port} non puo' appartenere a entrambi"
            );
        }
    }
}


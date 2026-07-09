//! Tipi degli eventi pubblicati dal dispatcher.
//!
//! `ProjectEvent` e' una enum tipizzata con `#[serde(tag = "kind")]`: il
//! frontend filtra/routea per `kind`, niente JSON generico unsafe.
//! `EnvelopedEvent` aggiunge metadati (seq, event_id, ts, ui_hint).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Versione dello schema. Incrementare a ogni breaking change negli enum
/// payload (aggiunta campi e' compatibile, rimozione/rinomina no).
pub const SCHEMA_VERSION: u8 = 1;

/// Topic principali — usati dal frontend per filtrare lo stream SSE.
///
/// Il client passa `?topics=playwright,ports` e il server filtra in base
/// al topic di pertinenza dell'evento (derivato da `ProjectEvent::topic()`).
pub const TOPIC_PLAYWRIGHT: &str = "playwright";
pub const TOPIC_PORTS: &str = "ports";
pub const TOPIC_PROBLEMS: &str = "problems";
pub const TOPIC_SERVICES: &str = "services";
pub const TOPIC_FILES: &str = "files";
pub const TOPIC_GIT: &str = "git";
pub const TOPIC_DATABASE: &str = "database";
pub const TOPIC_FLAGS: &str = "flags";
pub const TOPIC_MONITOR: &str = "monitor";
pub const TOPIC_AGENT: &str = "agent";
pub const TOPIC_NOTIFICATION: &str = "notification";
pub const TOPIC_CUSTOM: &str = "custom";
pub const TOPIC_SYSTEM: &str = "system";
/// Eventi relativi alle sessioni chat (compact, message added, status changed).
/// Usati per riconciliare TokenUsageBar e contatori chat senza polling.
pub const TOPIC_CHAT: &str = "chat";
/// Catch-all per mutazioni HTTP intercettate dal middleware `event_capture`.
/// Permette ai pannelli di reagire anche ad endpoint non cablati esplicitamente.
pub const TOPIC_MUTATION: &str = "mutation";
/// Eventi di arricchimento (re-emit asincrono dopo classifier LLM) — payload
/// `EventEnriched` con `event_id` originale + `ui_hint`/`semantic_tags` aggiunti.
pub const TOPIC_META: &str = "meta";
pub const TOPIC_KNOWLEDGE: &str = "knowledge";
pub const TOPIC_DOCUMENTS: &str = "documents";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ProjectEvent {
    // ── Playwright / job ───────────────────────────────────────────────
    JobCreated {
        id: Uuid,
        job_kind: String,
        status: String,
        label: String,
        summary: Option<String>,
        #[serde(default)]
        artifacts: serde_json::Value,
    },
    JobUpdated {
        id: Uuid,
        status: String,
        label: Option<String>,
        summary: Option<String>,
    },
    JobsCleared {
        job_kind: String,
        deleted: u64,
    },

    // ── Ports ──────────────────────────────────────────────────────────
    PortAllocated {
        port: i32,
        label: String,
        pid: Option<i32>,
    },
    PortReleased {
        port: i32,
    },

    // ── Quality / problems ─────────────────────────────────────────────
    FindingsUpdated {
        scan_id: Option<Uuid>,
        total: i64,
        critical: i64,
        warnings: i64,
        /// Findings risolti in questo scan (id da `project_quality_findings`).
        /// Permette al frontend di marcare in-place senza ri-scansionare.
        /// Vuoto se lo scan non ha rilevato risoluzioni o se l'emittente non
        /// e' in grado di calcolare il delta (es. emit da nuovo scan a freddo).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        resolved_ids: Vec<Uuid>,
    },

    // ── Services ───────────────────────────────────────────────────────
    ServiceStarted {
        name: String,
        port: Option<i32>,
        pid: Option<i32>,
    },
    ServiceStopped {
        name: String,
    },
    ServiceRestarted {
        name: String,
    },
    /// Stato systemd/processo cambiato (active, failed, activating, ...).
    /// Emesso dal service_observer a ogni transizione — elimina polling Run panel.
    ServiceStatusChanged {
        name: String,
        status: String,
        port: Option<i32>,
        pid: Option<i32>,
    },

    // ── Filesystem ─────────────────────────────────────────────────────
    FileChanged {
        path: String,
        op: String, // "created" | "modified" | "deleted"
    },

    // ── Git ────────────────────────────────────────────────────────────
    GitStatusChanged {
        branch: String,
        ahead: i32,
        behind: i32,
        modified_count: i32,
    },

    // ── Database ───────────────────────────────────────────────────────
    DbQueryRun {
        query_id: Option<Uuid>,
        duration_ms: i64,
        rows: i64,
        statement_kind: String, // "select" | "insert" | "update" | "ddl" | ...
    },
    /// Configurazione DB del progetto creata/aggiornata/rimossa.
    /// Emesso da `project_db_set_connection` tool — il pannello DB frontend
    /// ascolta questo evento per ricaricare automaticamente i dati.
    DbConfigUpdated {
        name: String,
        engine: Option<String>,
        action: String, // "created" | "updated" | "deleted"
    },

    // ── Agent meta (osservabilita') ────────────────────────────────────
    AgentToolUsed {
        run_id: String,
        tool: String,
        target_resource: Option<String>,
    },

    // ── M15.1 — Progresso todo live in chat ───────────────────────────
    TodoUpdated {
        run_id: String,
        todo_id: String,
        seq: Option<i32>,
        status: String,
    },

    // ── M15 — Aggiornamento del piano (checklist todo) ────────────────
    // Emesso quando la composizione del piano cambia (creazione/modifica/edit
    // utente dei todo di un run): permette alla UI di aggiornare il contatore
    // di avanzamento senza ricaricare l'intera lista.
    PlanUpdated {
        run_id: String,
        total: i32,
        completed: i32,
    },

    // ── Pilotaggio diretto dall'agente ────────────────────────────────
    Notification {
        severity: String, // "info" | "success" | "warning" | "error"
        message: String,
        panel: Option<String>,
        ttl_ms: Option<u64>,
        run_id: Option<String>,
    },
    FlagChanged {
        key: String,
        value: serde_json::Value,
    },
    MonitorUpdated {
        monitor_id: String,
        value: serde_json::Value,
        label: Option<String>,
    },
    HighlightPanel {
        panel: String,
        duration_ms: u64,
    },
    /// Evento custom non coperto dalle varianti tipizzate. Per estensibilita'
    /// futura (plugin, telemetria custom). Il classifier ricade su LLM/regole
    /// per dedurre UiHint. Il campo `event_name` e' la chiave logica
    /// (non puo' chiamarsi `kind` perche' confligge con il tag serde).
    Custom {
        event_name: String,
        resource: String,
        payload: serde_json::Value,
    },

    // ── Chat sessions ──────────────────────────────────────────────────
    /// Sessione chat compattata: il backend ha generato un summary e salvato
    /// un point vettoriale (Qdrant). Il frontend usa `total_tokens` e
    /// `total_cost_usd` per riallineare immediatamente la `TokenUsageBar`
    /// senza dover ricaricare la pagina.
    ChatSessionCompacted {
        session_id: Uuid,
        summary_point_id: Option<String>,
        total_tokens: i64,
        total_cost_usd: f64,
    },
    /// Nuovo messaggio inserito in `chat_messages`. Inviato per role="user"
    /// e role="assistant" (sintetico incluso). Permette ai pannelli di
    /// aggiornarsi senza polling e a `use-chat` di accumulare i token.
    ChatMessageAdded {
        session_id: Uuid,
        message_id: Uuid,
        role: String,
        /// Totale token contati dal backend al momento dell'INSERT (cumulativo
        /// della sessione, non delta). Idempotente lato client.
        total_tokens: Option<i64>,
        total_cost_usd: Option<f64>,
    },
    /// Cambio di stato di una sessione chat (attiva, compactata, archiviata).
    /// Triggera badge/icone di stato nei tab.
    ChatSessionStatusChanged {
        session_id: Uuid,
        status: String,
    },

    // ── Catch-all HTTP middleware ─────────────────────────────────────
    /// Mutazione HTTP intercettata dal middleware `event_capture`. Garantisce
    /// copertura totale per qualsiasi endpoint POST/PUT/DELETE/PATCH che
    /// muta lo stato del progetto, anche se non cablato esplicitamente con
    /// emit tipizzato. Il classifier LLM puo' arricchirla con `ui_hint`.
    MutationRecorded {
        method: String,
        path: String,
        status_code: u16,
        session_id: Option<Uuid>,
        /// Riassunto opzionale (es. nome operazione) — non include payload
        /// per evitare leak di dati sensibili nei log/SSE.
        summary: Option<String>,
        actor_user_id: Option<Uuid>,
    },

    // ── Arricchimento asincrono (re-emit dopo classifier LLM) ─────────
    /// Re-emit asincrono di un evento precedente con metadati AI aggiunti.
    /// Il frontend memorizza per `event_id` e fa merge dei metadata in modo
    /// idempotente (ordine tollerato: l'evento originale puo' essere gia'
    /// stato processato).
    EventEnriched {
        /// `event_id` dell'evento originale (UUID v7).
        event_id: Uuid,
        #[serde(skip_serializing_if = "Option::is_none")]
        ui_hint: Option<UiHint>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        semantic_tags: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        severity_inferred: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        panel_target: Option<String>,
    },

    // ── Project lifecycle ──────────────────────────────────────────────
    /// Progetto registrato (creato). Emesso da `register_project`.
    /// La sidebar progetti ascolta per aggiornare la lista in tempo reale.
    ProjectCreated {
        name: String,
        slug: String,
    },
    /// Progetto eliminato. Emesso da `delete_project`.
    ProjectDeleted {
        name: String,
    },

    // ── Database migrations ─────────────────────────────────────────────
    /// Migrazione applicata con successo.
    MigrationApplied {
        migration_name: String,
        version: String,
    },
    /// Migrazione annullata (rollback).
    MigrationRolledBack {
        migration_name: String,
        version: String,
    },

    // ── Run configurations ──────────────────────────────────────────────
    /// Configurazione run creata/aggiornata/eliminata.
    RunConfigChanged {
        config_id: String,
        label: String,
        action: String, // "created" | "updated" | "deleted"
    },

    // ── Memory / reasoning bank ─────────────────────────────────────────
    /// Elemento di memoria inserito/aggiornato/eliminato.
    MemoryUpdated {
        category: String, // "pattern" | "fact" | "decision" | ...
        count_delta: i32,
    },

    // ── Provider health ───────────────────────────────────────────────────
    /// Cambio stato salute di un provider AI.
    ProviderHealthChanged {
        provider: String,
        status: String, // "up" | "down" | "degraded"
        latency_ms: Option<i64>,
    },

    // ── Plugin lifecycle ─────────────────────────────────────────────────
    /// Plugin installato/disinstallato/abilitato/disabilitato.
    PluginChanged {
        plugin_id: String,
        slug: String,
        action: String, // "installed" | "uninstalled" | "enabled" | "disabled"
    },

    // ── Settings ─────────────────────────────────────────────────────────
    /// Impostazione di sistema modificata.
    SettingChanged {
        namespace: String,
        key: String,
    },

    // ── Subagent lifecycle ───────────────────────────────────────────────
    /// Ciclo di vita di un sub-agente orchestrato.
    SubagentRunChanged {
        run_id: String,
        status: String, // "started" | "completed" | "failed"
        parent_run_id: Option<String>,
    },

    // ── Quality scan progress ────────────────────────────────────────────
    /// Progresso dello scan qualita' (oltre il FindingsUpdated finale).
    QualityScanProgress {
        scan_id: String,
        phase: String, // "started" | "progress" | "completed"
        percent: Option<u8>,
    },

    // ── Output channels ──────────────────────────────────────────────────
    /// Nuovo canale di output registrato (es. agent:xxx).
    OutputChannelCreated {
        channel_id: String,
        label: String,
    },

    // ── Knowledge Base ────────────────────────────────────────────────────────────────
    KnowledgeNoteCreated {
        note_id: Uuid,
        title: String,
        intent: Option<String>,
    },
    KnowledgeNoteUpdated {
        note_id: Uuid,
        status: String,
    },
    KnowledgeLinkCreated {
        link_id: Uuid,
        from: Uuid,
        to: Uuid,
        rel_type: String,
        created_by: String,
    },

    // ── Documenti di progetto ──────────────────────────────────────────────
    /// Emesso quando un documento (analisi funzionale/tecnica, ecc.) viene
    /// generato e registrato in `project_documents`. Permette al pannello
    /// DOCUMENTI di aggiornarsi in realtime anche quando la generazione avviene
    /// via chat (il tool builtin non ha accesso ad AppState: usa emit_global).
    DocumentGenerated {
        document_id: Uuid,
        doc_type: String,
        title: String,
        version: String,
        file_path: String,
    },

    // ── Observability servizi app utente (service_observer, mig 0355/0356) ──
    /// Metriche OS per processo di un servizio utente (capacita' 4). Effimero:
    /// non persistito su DB, solo event-stream + ring in-memory per snapshot.
    ServiceMetrics {
        unit: String,
        pid: Option<i32>,
        cpu_pct: f32,
        rss_bytes: u64,
        io_read_bytes: u64,
        io_write_bytes: u64,
        latency_ms: Option<u64>,
    },
    /// Riga di log significativa dal tail continuo di un servizio utente.
    ServiceLogLine {
        unit: String,
        level: String, // "error" | "warn" | "info"
        line: String,
    },
    /// Anomalia rilevata su un servizio utente (capacita' 3).
    ServiceAnomaly {
        unit: String,
        metric: String, // latency | restart | error_rate | cpu | rss
        value: f64,
        threshold: f64,
        severity: String, // "warning" | "critical"
    },
    /// Crash/eccezione runtime rilevato nei log di un servizio utente (cap 1).
    ServiceCrashDetected {
        unit: String,
        error_kind: String,
        last_log: String,
    },
    /// Errori di build strutturati associati a un servizio (capacita' 2).
    ServiceBuildErrors {
        unit: String,
        count: i64,
        findings: serde_json::Value,
    },
    /// Avviata una diagnosi automatica (run dell'agente Debugger) per un crash.
    ServiceDiagnosisStarted {
        unit: String,
        run_id: String,
    },

    // ── Eventi di servizio del dispatcher ──────────────────────────────────
    /// Inviato quando il consumer e' rimasto indietro oltre la capacita'
    /// del ring buffer. Il client deve ricaricare lo snapshot REST.
    SnapshotRequired {
        reason: String,
        last_known_seq: u64,
    },
}

impl ProjectEvent {
    /// Topic associato al payload (filtrabile dal client via `?topics=...`).
    pub fn topic(&self) -> &'static str {
        match self {
            Self::JobCreated { .. } | Self::JobUpdated { .. } | Self::JobsCleared { .. } => {
                TOPIC_PLAYWRIGHT
            }
            Self::PortAllocated { .. } | Self::PortReleased { .. } => TOPIC_PORTS,
            Self::FindingsUpdated { .. } => TOPIC_PROBLEMS,
            Self::ServiceStarted { .. }
            | Self::ServiceStopped { .. }
            | Self::ServiceRestarted { .. }
            | Self::ServiceStatusChanged { .. } => TOPIC_SERVICES,
            Self::FileChanged { .. } => TOPIC_FILES,
            Self::GitStatusChanged { .. } => TOPIC_GIT,
            Self::DbQueryRun { .. } | Self::DbConfigUpdated { .. } => TOPIC_DATABASE,
            Self::AgentToolUsed { .. }
            | Self::TodoUpdated { .. }
            | Self::PlanUpdated { .. } => TOPIC_AGENT,
            Self::Notification { .. } => TOPIC_NOTIFICATION,
            Self::FlagChanged { .. } => TOPIC_FLAGS,
            Self::MonitorUpdated { .. } | Self::HighlightPanel { .. } => TOPIC_MONITOR,
            Self::Custom { .. } => TOPIC_CUSTOM,
            Self::ChatSessionCompacted { .. }
            | Self::ChatMessageAdded { .. }
            | Self::ChatSessionStatusChanged { .. } => TOPIC_CHAT,
            Self::MutationRecorded { .. } => TOPIC_MUTATION,
            Self::EventEnriched { .. } => TOPIC_META,
            Self::ProjectCreated { .. } | Self::ProjectDeleted { .. } => TOPIC_SYSTEM,
            Self::MigrationApplied { .. } | Self::MigrationRolledBack { .. } => TOPIC_DATABASE,
            Self::RunConfigChanged { .. } => TOPIC_SERVICES,
            Self::MemoryUpdated { .. } => TOPIC_AGENT,
            Self::ProviderHealthChanged { .. } => TOPIC_SYSTEM,
            Self::PluginChanged { .. } => TOPIC_SYSTEM,
            Self::SettingChanged { .. } => TOPIC_FLAGS,
            Self::SubagentRunChanged { .. } => TOPIC_AGENT,
            Self::QualityScanProgress { .. } => TOPIC_PROBLEMS,
            Self::OutputChannelCreated { .. } => TOPIC_SERVICES,
            Self::KnowledgeNoteCreated { .. }
            | Self::KnowledgeNoteUpdated { .. }
            | Self::KnowledgeLinkCreated { .. } => TOPIC_KNOWLEDGE,
            Self::DocumentGenerated { .. } => TOPIC_DOCUMENTS,
            Self::ServiceMetrics { .. } | Self::ServiceAnomaly { .. } => TOPIC_MONITOR,
            Self::ServiceBuildErrors { .. } => TOPIC_PROBLEMS,
            Self::ServiceLogLine { .. }
            | Self::ServiceCrashDetected { .. }
            | Self::ServiceDiagnosisStarted { .. } => TOPIC_SERVICES,
            Self::SnapshotRequired { .. } => TOPIC_SYSTEM,
        }
    }

    /// Nome breve del tipo (per regole classifier e logging).
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::JobCreated { .. } => "JobCreated",
            Self::JobUpdated { .. } => "JobUpdated",
            Self::JobsCleared { .. } => "JobsCleared",
            Self::PortAllocated { .. } => "PortAllocated",
            Self::PortReleased { .. } => "PortReleased",
            Self::FindingsUpdated { .. } => "FindingsUpdated",
            Self::ServiceStarted { .. } => "ServiceStarted",
            Self::ServiceStopped { .. } => "ServiceStopped",
            Self::ServiceRestarted { .. } => "ServiceRestarted",
            Self::ServiceStatusChanged { .. } => "ServiceStatusChanged",
            Self::FileChanged { .. } => "FileChanged",
            Self::GitStatusChanged { .. } => "GitStatusChanged",
            Self::DbQueryRun { .. } => "DbQueryRun",
            Self::DbConfigUpdated { .. } => "DbConfigUpdated",
            Self::AgentToolUsed { .. } => "AgentToolUsed",
            Self::TodoUpdated { .. } => "TodoUpdated",
            Self::PlanUpdated { .. } => "PlanUpdated",
            Self::Notification { .. } => "Notification",
            Self::FlagChanged { .. } => "FlagChanged",
            Self::MonitorUpdated { .. } => "MonitorUpdated",
            Self::HighlightPanel { .. } => "HighlightPanel",
            Self::Custom { .. } => "Custom",
            Self::ChatSessionCompacted { .. } => "ChatSessionCompacted",
            Self::ChatMessageAdded { .. } => "ChatMessageAdded",
            Self::ChatSessionStatusChanged { .. } => "ChatSessionStatusChanged",
            Self::MutationRecorded { .. } => "MutationRecorded",
            Self::EventEnriched { .. } => "EventEnriched",
            Self::ProjectCreated { .. } => "ProjectCreated",
            Self::ProjectDeleted { .. } => "ProjectDeleted",
            Self::MigrationApplied { .. } => "MigrationApplied",
            Self::MigrationRolledBack { .. } => "MigrationRolledBack",
            Self::RunConfigChanged { .. } => "RunConfigChanged",
            Self::MemoryUpdated { .. } => "MemoryUpdated",
            Self::ProviderHealthChanged { .. } => "ProviderHealthChanged",
            Self::PluginChanged { .. } => "PluginChanged",
            Self::SettingChanged { .. } => "SettingChanged",
            Self::SubagentRunChanged { .. } => "SubagentRunChanged",
            Self::QualityScanProgress { .. } => "QualityScanProgress",
            Self::OutputChannelCreated { .. } => "OutputChannelCreated",
            Self::KnowledgeNoteCreated { .. } => "KnowledgeNoteCreated",
            Self::KnowledgeNoteUpdated { .. } => "KnowledgeNoteUpdated",
            Self::KnowledgeLinkCreated { .. } => "KnowledgeLinkCreated",
            Self::DocumentGenerated { .. } => "DocumentGenerated",
            Self::ServiceMetrics { .. } => "ServiceMetrics",
            Self::ServiceLogLine { .. } => "ServiceLogLine",
            Self::ServiceAnomaly { .. } => "ServiceAnomaly",
            Self::ServiceCrashDetected { .. } => "ServiceCrashDetected",
            Self::ServiceBuildErrors { .. } => "ServiceBuildErrors",
            Self::ServiceDiagnosisStarted { .. } => "ServiceDiagnosisStarted",
            Self::SnapshotRequired { .. } => "SnapshotRequired",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UiHint {
    /// Pannello da evidenziare / portare in primo piano.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlight_panel: Option<String>,
    /// Severita' del toast (info|success|warning|error). None = nessun toast.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toast_severity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toast_msg: Option<String>,
    /// Incrementa un badge numerico su un pannello (es: badge "3" sul tab Problemi).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub badge_increment: Option<(String, i32)>,
    /// Durata flash animation in ms.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flash_duration_ms: Option<u64>,
}

/// Busta che avvolge ogni `ProjectEvent` con metadati per ordering, dedup,
/// replay e routing UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvelopedEvent {
    /// UUID v7 (ordinato per tempo) — usato per dedup lato client.
    pub event_id: Uuid,
    /// Sequenza monotona per project_id — usato per gap detection e replay.
    pub seq: u64,
    pub project_id: Uuid,
    /// Unix epoch in millisecondi.
    pub ts: i64,
    pub topic: String,
    pub payload: ProjectEvent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_hint: Option<UiHint>,
    pub schema_version: u8,
}

impl EnvelopedEvent {
    pub fn new(project_id: Uuid, seq: u64, payload: ProjectEvent, ui_hint: Option<UiHint>) -> Self {
        let topic = payload.topic().to_string();
        Self {
            event_id: Uuid::now_v7(),
            seq,
            project_id,
            ts: chrono::Utc::now().timestamp_millis(),
            topic,
            payload,
            ui_hint,
            schema_version: SCHEMA_VERSION,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enveloped_event_serializes_with_tag() {
        let pid = Uuid::new_v4();
        let ev = EnvelopedEvent::new(
            pid,
            1,
            ProjectEvent::PortReleased { port: 3000 },
            None,
        );
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"kind\":\"PortReleased\""));
        assert!(s.contains("\"topic\":\"ports\""));
    }

    #[test]
    fn topic_routing_is_stable() {
        assert_eq!(
            ProjectEvent::FileChanged {
                path: "x".into(),
                op: "created".into()
            }
            .topic(),
            TOPIC_FILES
        );
        assert_eq!(
            ProjectEvent::Notification {
                severity: "info".into(),
                message: "m".into(),
                panel: None,
                ttl_ms: None,
                run_id: None,
            }
            .topic(),
            TOPIC_NOTIFICATION
        );
    }

    #[test]
    fn chat_variants_route_to_chat_topic() {
        let sid = Uuid::new_v4();
        assert_eq!(
            ProjectEvent::ChatSessionCompacted {
                session_id: sid,
                summary_point_id: None,
                total_tokens: 0,
                total_cost_usd: 0.0,
            }
            .topic(),
            TOPIC_CHAT
        );
        assert_eq!(
            ProjectEvent::ChatMessageAdded {
                session_id: sid,
                message_id: Uuid::new_v4(),
                role: "user".into(),
                total_tokens: Some(10),
                total_cost_usd: Some(0.001),
            }
            .topic(),
            TOPIC_CHAT
        );
        assert_eq!(
            ProjectEvent::ChatSessionStatusChanged {
                session_id: sid,
                status: "compacted".into(),
            }
            .topic(),
            TOPIC_CHAT
        );
    }

    #[test]
    fn mutation_and_enriched_route_correctly() {
        assert_eq!(
            ProjectEvent::MutationRecorded {
                method: "POST".into(),
                path: "/api/projects/x/files".into(),
                status_code: 200,
                session_id: None,
                summary: None,
                actor_user_id: None,
            }
            .topic(),
            TOPIC_MUTATION
        );
        assert_eq!(
            ProjectEvent::EventEnriched {
                event_id: Uuid::new_v4(),
                ui_hint: None,
                semantic_tags: vec!["foo".into()],
                severity_inferred: None,
                panel_target: None,
            }
            .topic(),
            TOPIC_META
        );
    }

    #[test]
    fn findings_updated_serializes_resolved_ids() {
        let env = EnvelopedEvent::new(
            Uuid::new_v4(),
            1,
            ProjectEvent::FindingsUpdated {
                scan_id: None,
                total: 5,
                critical: 1,
                warnings: 2,
                resolved_ids: vec![Uuid::new_v4()],
            },
            None,
        );
        let s = serde_json::to_string(&env).unwrap();
        assert!(s.contains("\"resolved_ids\""));
    }
}

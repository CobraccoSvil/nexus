//! Definizioni dei tool disponibili all'agente e funzioni di esecuzione.
//!
//! I tool sono sicuri: nessuna esecuzione di shell arbitraria.
//! Tutte le operazioni file sono vincolate alla root del progetto.
//!
//! Coordinatore del package `agent_tools`. La logica e' splittata per dominio
//! nei sottomoduli; questo file dichiara i moduli e re-esporta i simboli che il
//! resto del crate (e i sottomoduli che fanno `use super::*`) si aspettano.
//!
//! Splitting interno (refactor god-file):
//! - `tool_schema`    — costante `AGENT_TOOLS_JSON` (schema tool, dato puro; in nexus-agent-tools)
//! - `context`        — `AgentToolContext` (wrapper di `ToolContextCore` + campi mcp-core)
//! - `helpers`        — costanti lettura file, pattern protetti, helper condivisi
//! - `dispatch`       — `execute_agent_tool` (routing nome-tool -> handler)
//! - `semantic_tools` — ricerca semantica (codebase, recall, in-file)
//!
//! Sottomoduli per dominio operativo:
//! - `git`     — comandi Git
//! - `service` — gestione processi long-running e build immagine progetto
//! - `sandbox` — configurazione sandbox del progetto
//! - `command` — esecuzione comandi shell e test runner

// Split 7.4: i moduli senza AgentToolContext (passo 1) e i tool che usano
// solo i campi core del contesto (passo 2, `ToolContextCore`) vivono nel
// crate nexus-agent-tools; il re-export mantiene i path crate::agent_tools::*.
pub use nexus_agent_tools::*;

pub(crate) mod command;
pub(crate) mod context;
pub(crate) mod dispatch;
pub(crate) mod helpers;
pub(crate) mod playwright_cli;
pub(crate) mod port_scanner;
pub(crate) mod ports;
pub(crate) mod privileged;
pub(crate) mod project_db_query;
pub(crate) mod rag_search;
pub(crate) mod sandbox;
pub(crate) mod semantic_tools;
pub(crate) mod service;
// Orchestrazione NATIVA dei sub-agenti (porting di /agent/subagent-run): vive in
// mcp-core perche' richiede crate::native_engine (la gerarchia mcp-core ->
// nexus-agent-tools impedisce a subagent.rs di chiamarlo). Intercetta i tool
// dispatch_subagent* prima della delega.
pub(crate) mod subagent_native;
pub(crate) mod testing;
pub(crate) mod tool_not_found;
// Catena di verifica post-modifica nexus_verify_change (ADR 0019 L3).
pub(crate) mod verify;
pub(crate) mod visual_compare;

// ── API pubblica del package (call site esterni: invariata) ─────────────────
pub use context::AgentToolContext;
pub use dispatch::execute_agent_tool;
pub use tool_schema::AGENT_TOOLS_JSON;

// Re-export per uso interno crate (tool_run_tests è chiamato da agent_loop, in teoria).

// ── Re-export per i sottomoduli che usano `use super::*` ────────────────────
// Mantengono risolvibili i simboli che prima vivevano in questo file: tipi base,
// helper condivisi e path di crate referenziati via `super::`.
pub(crate) use std::path::{Path, PathBuf};

pub(crate) use serde_json::Value;
pub(crate) use sqlx::Row;
pub(crate) use uuid::Uuid;

pub(crate) use crate::projects::resolve_relative_path;

pub(crate) use helpers::{
    classify_command_error, format_process_output, is_long_oneshot,
    looks_like_long_running_command,
};

#[cfg(test)]
mod adr0034_contract_tests {
    /// COERENZA CROSS-CRATE (regola L): gli enum dello schema esposto al
    /// modello (nexus-agent-tools, AGENT_TOOLS_JSON) e quelli della
    /// validazione (nexus-agent-graph, VALID_OUTCOMES/VALID_BLOCKERS) sono
    /// duplicati per necessita' (il grafo non dipende dal crate dei tool):
    /// questo test li lega — un drift (es. outcome aggiunto solo allo schema:
    /// il modello lo dichiara, normalize lo scarta in silenzio) diventa un
    /// test rosso.
    #[test]
    fn enum_schema_coerenti_con_normalize() {
        let v: serde_json::Value =
            serde_json::from_str(super::AGENT_TOOLS_JSON).expect("catalogo parsa");
        let tc = v
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t.get("name").and_then(|n| n.as_str()) == Some("task_complete"))
            .expect("task_complete nel catalogo");
        let schema = &tc["input_schema"]["properties"];
        // Confronto come INSIEMI: l'ordine di presentazione nello schema non
        // e' contrattuale, conta che i valori accettati coincidano.
        let set = |vals: &serde_json::Value| -> std::collections::BTreeSet<String> {
            vals.as_array()
                .expect("enum array")
                .iter()
                .filter_map(|x| x.as_str())
                .map(str::to_string)
                .collect()
        };
        let valid = |vals: &[&str]| -> std::collections::BTreeSet<String> {
            vals.iter().map(|s| s.to_string()).collect()
        };
        assert_eq!(
            set(&schema["outcome"]["enum"]),
            valid(nexus_agent_graph::decisions::tool_dispatch::VALID_OUTCOMES),
            "enum outcome dello schema divergente da VALID_OUTCOMES"
        );
        assert_eq!(
            set(&schema["blocker"]["enum"]),
            valid(nexus_agent_graph::decisions::tool_dispatch::VALID_BLOCKERS),
            "enum blocker dello schema divergente da VALID_BLOCKERS"
        );
    }

    /// Stesso legame per gli ENDPOINT dichiarati in `task_complete` (le prove
    /// HTTP che il final_gate eseguira'): l'enum dei metodi nello schema deve
    /// coincidere con quello che `endpoint_probes::normalize_endpoints` accetta.
    /// Un metodo aggiunto solo allo schema sarebbe dichiarato dal modello e
    /// scartato in silenzio, cioe' un endpoint mai provato con l'aria di una
    /// verifica — la stessa forma del difetto che ha introdotto questo campo.
    #[test]
    fn enum_metodi_endpoint_coerenti_con_normalize() {
        let v: serde_json::Value =
            serde_json::from_str(super::AGENT_TOOLS_JSON).expect("catalogo parsa");
        let tc = v
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t.get("name").and_then(|n| n.as_str()) == Some("task_complete"))
            .expect("task_complete nel catalogo");
        let metodi = &tc["input_schema"]["properties"]["endpoints"]["items"]["properties"]["method"]
            ["enum"];
        let set: std::collections::BTreeSet<String> = metodi
            .as_array()
            .expect("enum method array")
            .iter()
            .filter_map(|x| x.as_str())
            .map(str::to_string)
            .collect();
        let attesi: std::collections::BTreeSet<String> =
            nexus_agent_graph::decisions::endpoint_probes::VALID_ENDPOINT_METHODS
                .iter()
                .map(|s| s.to_string())
                .collect();
        assert_eq!(
            set, attesi,
            "enum method dello schema divergente da VALID_ENDPOINT_METHODS"
        );
    }

    /// Stesso legame cross-crate per il canale del REVISORE (Fase B ultracode):
    /// gli enum di `review_verdict` nello schema (verdict, findings.severity)
    /// devono coincidere con quelli del normalizzatore — un valore aggiunto
    /// solo allo schema verrebbe dichiarato dal modello e scartato in silenzio
    /// da `normalize_review_verdict`.
    #[test]
    fn enum_review_verdict_coerenti_con_normalize() {
        let v: serde_json::Value =
            serde_json::from_str(super::AGENT_TOOLS_JSON).expect("catalogo parsa");
        let rv = v
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t.get("name").and_then(|n| n.as_str()) == Some("review_verdict"))
            .expect("review_verdict nel catalogo");
        let schema = &rv["input_schema"]["properties"];
        let set = |vals: &serde_json::Value| -> std::collections::BTreeSet<String> {
            vals.as_array()
                .expect("enum array")
                .iter()
                .filter_map(|x| x.as_str())
                .map(str::to_string)
                .collect()
        };
        let valid = |vals: &[&str]| -> std::collections::BTreeSet<String> {
            vals.iter().map(|s| s.to_string()).collect()
        };
        assert_eq!(
            set(&schema["verdict"]["enum"]),
            valid(nexus_agent_graph::decisions::tool_dispatch::VALID_REVIEW_VERDICTS),
            "enum verdict dello schema divergente da VALID_REVIEW_VERDICTS"
        );
        assert_eq!(
            set(&schema["findings"]["items"]["properties"]["severity"]["enum"]),
            valid(nexus_agent_graph::decisions::tool_dispatch::VALID_FINDING_SEVERITIES),
            "enum severity dello schema divergente da VALID_FINDING_SEVERITIES"
        );
    }

    /// Helper condiviso dai test di coerenza: enum dichiarato nello schema di un
    /// tool del catalogo.
    fn schema_enum(tool: &str, path: &[&str]) -> std::collections::BTreeSet<String> {
        let v: serde_json::Value =
            serde_json::from_str(super::AGENT_TOOLS_JSON).expect("catalogo parsa");
        let t = v
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t.get("name").and_then(|n| n.as_str()) == Some(tool))
            .unwrap_or_else(|| panic!("{tool} nel catalogo"));
        let mut node = &t["input_schema"]["properties"];
        for p in path {
            node = &node[p];
        }
        node["enum"]
            .as_array()
            .unwrap_or_else(|| panic!("{tool}: enum array atteso in {path:?}"))
            .iter()
            .filter_map(|x| x.as_str())
            .map(str::to_string)
            .collect()
    }

    fn as_set(vals: &[&str]) -> std::collections::BTreeSet<String> {
        vals.iter().map(|s| s.to_string()).collect()
    }

    /// Il legame cross-crate mancava per il canale delle FIGURE del consiglio
    /// (esisteva solo per il revisore): un valore aggiunto allo schema di
    /// `advisory_verdict` ma non a `VALID_ADVISORY_VERDICTS` verrebbe dichiarato
    /// dal modello e scartato in silenzio dal normalizzatore.
    #[test]
    fn enum_advisory_verdict_coerenti_con_normalize() {
        assert_eq!(
            schema_enum("advisory_verdict", &["verdict"]),
            as_set(nexus_agent_graph::decisions::tool_dispatch::VALID_ADVISORY_VERDICTS),
            "enum verdict dello schema divergente da VALID_ADVISORY_VERDICTS"
        );
        assert_eq!(
            schema_enum("advisory_verdict", &["risks", "items", "properties", "severity"]),
            as_set(nexus_agent_graph::decisions::tool_dispatch::VALID_FINDING_SEVERITIES),
            "enum severity dei risks divergente da VALID_FINDING_SEVERITIES"
        );
    }

    /// Stesso legame per il canale dell'AVVOCATO del dibattito: `stance` guida
    /// la selezione dell'opzione (un `oppose` con evidenza grave squalifica una
    /// posizione), quindi una divergenza schema/normalizzatore falserebbe
    /// l'esito del confronto in silenzio.
    #[test]
    fn enum_debate_position_coerenti_con_normalize() {
        assert_eq!(
            schema_enum("debate_position", &["stance"]),
            as_set(nexus_agent_graph::decisions::tool_dispatch::VALID_DEBATE_STANCES),
            "enum stance dello schema divergente da VALID_DEBATE_STANCES"
        );
        assert_eq!(
            schema_enum("debate_position", &["risks", "items", "properties", "severity"]),
            as_set(nexus_agent_graph::decisions::tool_dispatch::VALID_FINDING_SEVERITIES),
            "enum severity dei risks divergente da VALID_FINDING_SEVERITIES"
        );
    }

    /// I tool a canale di chiusura di RUOLO non devono finire nel catalogo del
    /// run principale (il coordinatore non ha una posizione assegnata da
    /// difendere): `debate_position` deve stare fra i SUBAGENT_ONLY_TOOLS come i
    /// due gemelli.
    #[test]
    fn debate_position_e_subagent_only() {
        assert!(
            nexus_agent_tools::tool_schema::SUBAGENT_ONLY_TOOLS.contains(&"debate_position"),
            "debate_position deve essere riservato ai sub-agenti"
        );
    }

    /// La ricerca di riferimenti e' l'unico tool che porta dentro contenuto
    /// scritto fuori da Nexus. Deve restare ai due kind di sola lettura che
    /// l'hanno in whitelist: nel catalogo del run principale — che scrive file
    /// ed esegue comandi — un ingresso esterno sarebbe a un passo
    /// dall'esecuzione.
    ///
    /// Il catalogo dei layout NON e' riservato, e questa meta' del test conta
    /// quanto l'altra: la figura cita il pattern per chiave, e chi implementa
    /// deve poterne leggere la scheda.
    #[test]
    fn la_ricerca_esterna_e_riservata_ma_il_catalogo_layout_no() {
        let riservati = nexus_agent_tools::tool_schema::SUBAGENT_ONLY_TOOLS;
        assert!(
            riservati.contains(&"ui_reference_search"),
            "la ricerca web non deve stare nel catalogo di chi scrive file"
        );
        assert!(
            !riservati.contains(&"ui_layout_patterns"),
            "il catalogo dei layout serve anche a chi implementa"
        );
    }
}

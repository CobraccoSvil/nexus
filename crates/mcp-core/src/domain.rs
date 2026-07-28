use chrono::{DateTime, Utc};
use nexus_types::build_info::BuildStampSource;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthSummary {
    pub service: String,
    pub version: String,
    /// Secondi Unix (stringa) dell'ultima modifica del binario IN ESECUZIONE.
    /// Non e' il momento della compilazione: vedi `nexus_types::build_info` per
    /// il perche' un timestamp inciso da `build.rs` restava indietro.
    pub build_time: String,
    /// Premessa di `build_time`: da dove quel numero e' stato letto. Chi
    /// interroga `/health` per sapere se il deploy ha preso deve poter
    /// distinguere una misura dall'assenza di misura (`unknown` + `"0"`).
    #[serde(default)]
    pub build_time_source: BuildStampSource,
    pub status: String,
    pub timestamp: DateTime<Utc>,
    pub components: ComponentHealth,
}

impl HealthSummary {
    /// Costruisce la risposta di `/health`. PUNTO UNICO dei campi di identita'
    /// dell'artefatto (`build_time` + `build_time_source`): li prende da
    /// [`nexus_types::build_info::running_binary`] invece di lasciarli scegliere
    /// al call site, cosi' l'endpoint con cui si verifica quale binario stia
    /// girando non puo' dichiarare una data diversa da quella del binario che
    /// risponde (regola O).
    pub fn new(
        service: &str,
        version: &str,
        status: &str,
        timestamp: DateTime<Utc>,
        components: ComponentHealth,
    ) -> Self {
        let stamp = nexus_types::build_info::running_binary();
        Self {
            service: service.to_string(),
            version: version.to_string(),
            build_time: stamp.wire_value(),
            build_time_source: stamp.source,
            status: status.to_string(),
            timestamp,
            components,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentHealth {
    pub database: bool,
    pub redis: bool,
    pub neural_core: bool,
    /// gRPC ToolRunner (porta 50071): se giù, l'AI non può eseguire tool MCP
    /// (read_file, str_replace, ecc.) e gli agenti finiscono con "0 step".
    #[serde(default)]
    pub tools_grpc: bool,
    /// Qdrant vector DB: se giù, le operazioni vettoriali (arricchimento
    /// quality scan, ricerca semantica) vengono saltate. Aggiornato dal
    /// task_watchdog ogni 60s.
    #[serde(default)]
    pub qdrant: bool,
    /// Embedder (gRPC al brain Python): se giù, nessuna vettorializzazione.
    #[serde(default)]
    pub embedder: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorAudit {
    pub project_id: String,
    pub profile_id: String,
    pub intent: String,
    pub provider: String,
    pub model: String,
    pub token_budget: u32,
    pub tokens_saved: u32,
    pub resources: Vec<String>,
    pub guardrail_result: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, sqlx::FromRow)]
pub struct TokenStats {
    pub total_consumed: i64,
    pub total_cost: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn componenti_inerti() -> ComponentHealth {
        ComponentHealth {
            database: true,
            redis: true,
            neural_core: true,
            tools_grpc: true,
            qdrant: true,
            embedder: true,
        }
    }

    /// La conseguenza, non la stringa: il JSON che `/health` mette sul wire deve
    /// portare il timestamp del binario che sta rispondendo. Con il difetto
    /// originale (`env!("BUILD_TIMESTAMP")` da un `build.rs` che cargo non
    /// rieseguiva) qui comparirebbe la data dell'ultima modifica di quel file.
    #[test]
    fn health_espone_il_timestamp_del_binario_in_esecuzione() {
        let exe = std::env::current_exe().expect("path dell'eseguibile di test");
        let atteso = nexus_types::build_info::mtime_unix_seconds(&exe)
            .expect("mtime dell'eseguibile di test");

        let summary = HealthSummary::new(
            "mcp-core",
            "0.0.0-test",
            "ok",
            Utc::now(),
            componenti_inerti(),
        );
        let wire = serde_json::to_value(&summary).expect("serializzazione di HealthSummary");

        assert_eq!(wire["build_time"], atteso.to_string());
        assert_eq!(wire["build_time_source"], "exe_mtime");
    }

    /// Il consumatore del wire e' uno script bash che fa `grep` sulla forma
    /// `"build_time":"<cifre>"` e poi confronta numericamente (vedi
    /// `scripts/deploy-nexus.sh`): il campo deve restare una stringa di sole
    /// cifre, non diventare un numero JSON.
    #[test]
    fn build_time_resta_una_stringa_di_cifre_sul_wire() {
        let summary = HealthSummary::new(
            "mcp-core",
            "0.0.0-test",
            "ok",
            Utc::now(),
            componenti_inerti(),
        );
        let wire = serde_json::to_value(&summary).expect("serializzazione di HealthSummary");

        let build_time = wire["build_time"]
            .as_str()
            .expect("build_time e' una stringa JSON");
        assert!(
            build_time.chars().all(|c| c.is_ascii_digit()),
            "build_time = {build_time:?}: gli script di deploy lo confrontano con -ge"
        );
    }
}

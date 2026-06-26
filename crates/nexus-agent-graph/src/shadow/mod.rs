//! Modalita' SHADOW: confronto read-only fra l'output del run primario e quello
//! del run shadow, per validare la parita' Python<->Rust senza side-effect.
//!
//! Proprieta' fondamentali (opt-in, read-only):
//!
//! - **Opt-in**: lo shadow gira SOLO quando la tabella di routing del motore
//!   (`nexus_orchestrator_engine`, mig 0451) seleziona `shadow` per lo scope.
//!   In default globale (`python`) questo modulo non viene mai invocato.
//! - **Read-only**: il run shadow non emette eventi verso l'utente (EventSink
//!   no-op nel ctx shadow) e i suoi tool girano in `ExecMode::Replay` (rileggono
//!   il tool_result del primario, ZERO side-effect). Questo modulo si limita a
//!   CONFRONTARE due `serde_json::Value` e a PERSISTERE il diff in telemetria.
//! - **Niente decisioni**: il diff e' solo osservabilita'. Non influenza il run
//!   primario (l'output verso l'utente resta quello del primario).
//!
//! Lo schema della tabella telemetria e' VERSIONATO (mig 0453, regola H): NON
//! viene creato con `CREATE TABLE IF NOT EXISTS` a runtime.

use serde_json::Value;
use sqlx::PgPool;
use thiserror::Error;
use uuid::Uuid;

/// Errore di persistenza del diff shadow. Conserva il dettaglio sqlx (regola H:
/// non ingoiare l'errore).
#[derive(Debug, Error)]
pub enum ShadowError {
    /// Fallimento dell'INSERT in `nexus_shadow_telemetry`.
    #[error("persistenza diff shadow: {0}")]
    Store(String),
}

/// Diff per-nodo fra output primario e shadow.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeDiff {
    /// Nome del nodo che ha prodotto i due output (es. "router").
    pub node_name: String,
    /// Output (StateDelta serializzato) del run primario.
    pub primary_output: Value,
    /// Output (StateDelta serializzato) del run shadow.
    pub shadow_output: Value,
    /// Chiavi top-level che divergono fra i due output.
    pub divergent_keys: Vec<String>,
}

/// Raccoglitore in-memory dei diff per-nodo di un run shadow.
///
/// Stato + comportamento -> struct incapsulata (regola L, "composition over
/// inheritance"): accumula i `NodeDiff` man mano che i nodi shadow girano; il
/// chiamante li persiste a fine run (o per-nodo) via `persist_node_diff`.
#[derive(Debug, Default)]
pub struct DiffCollector {
    diffs: Vec<NodeDiff>,
}

impl DiffCollector {
    /// Crea un raccoglitore vuoto.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registra il confronto di un nodo, calcolando le chiavi divergenti.
    /// Ritorna un riferimento al `NodeDiff` appena inserito.
    pub fn record(&mut self, node_name: &str, primary: Value, shadow: Value) -> &NodeDiff {
        let divergent_keys = compute_diff(&primary, &shadow);
        self.diffs.push(NodeDiff {
            node_name: node_name.to_string(),
            primary_output: primary,
            shadow_output: shadow,
            divergent_keys,
        });
        // Appena inserito: l'unwrap su last() e' su un Vec non vuoto.
        self.diffs.last().expect("appena inserito")
    }

    /// Tutti i diff raccolti (in ordine di registrazione).
    pub fn diffs(&self) -> &[NodeDiff] {
        &self.diffs
    }

    /// `true` se nessun nodo ha divergenze (parita' perfetta sul run).
    pub fn is_converged(&self) -> bool {
        self.diffs.iter().all(|d| d.divergent_keys.is_empty())
    }
}

/// Calcola le chiavi top-level DIVERGENTI fra due output JSON.
///
/// Confronto a livello di chiave (non ricorsivo nei valori): una chiave e'
/// divergente se presente in uno solo dei due output, oppure presente in
/// entrambi con valore diverso. Per output non-oggetto (raro per uno StateDelta
/// serializzato, che e' sempre una mappa), si confronta l'intero valore sotto la
/// pseudo-chiave `"<root>"`. Le chiavi del risultato sono ORDINATE (output
/// deterministico, utile per i test e la diffabilita' della telemetria).
pub fn compute_diff(primary: &Value, shadow: &Value) -> Vec<String> {
    match (primary, shadow) {
        (Value::Object(p), Value::Object(s)) => {
            let mut keys: Vec<String> = Vec::new();
            // Unione delle chiavi dei due oggetti.
            let mut all: std::collections::BTreeSet<&String> = std::collections::BTreeSet::new();
            all.extend(p.keys());
            all.extend(s.keys());
            for k in all {
                match (p.get(k), s.get(k)) {
                    (Some(pv), Some(sv)) if pv == sv => {}
                    _ => keys.push(k.clone()),
                }
            }
            keys
        }
        // Output non-oggetto: se differiscono, segnala la pseudo-radice.
        _ => {
            if primary == shadow {
                Vec::new()
            } else {
                vec!["<root>".to_string()]
            }
        }
    }
}

/// Persiste un diff per-nodo nella telemetria shadow (`nexus_shadow_telemetry`,
/// mig 0453). INSERT puro (read-only rispetto al run: scrive solo telemetria).
/// L'id e' generato lato Rust (`Uuid::new_v4`) per non dipendere da un default
/// DB (esplicito = testabile).
pub async fn persist_node_diff(
    db: &PgPool,
    run_id: Uuid,
    node_name: &str,
    primary: &Value,
    shadow: &Value,
) -> Result<Uuid, ShadowError> {
    let id = Uuid::new_v4();
    let divergent = compute_diff(primary, shadow);
    sqlx::query(
        "INSERT INTO nexus_shadow_telemetry \
         (id, run_id, node_name, primary_output, shadow_output, divergent_keys) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(run_id)
    .bind(node_name)
    .bind(primary)
    .bind(shadow)
    .bind(&divergent)
    .execute(db)
    .await
    .map_err(|e| ShadowError::Store(e.to_string()))?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// compute_diff: chiavi assenti da un lato, valori diversi, e parita'.
    #[test]
    fn compute_diff_chiavi_divergenti() {
        let primary = json!({
            "user_intent": "code_write",
            "token_budget": 400,
            "solo_primary": true,
        });
        let shadow = json!({
            "user_intent": "code_write",   // uguale -> non diverge
            "token_budget": 500,           // diverso -> diverge
            "solo_shadow": false,          // solo shadow -> diverge
        });

        let diff = compute_diff(&primary, &shadow);
        // Ordinato: solo_primary, solo_shadow, token_budget.
        assert_eq!(
            diff,
            vec![
                "solo_primary".to_string(),
                "solo_shadow".to_string(),
                "token_budget".to_string()
            ]
        );
    }

    /// compute_diff: due output identici non hanno divergenze.
    #[test]
    fn compute_diff_parita_nessuna_divergenza() {
        let v = json!({"a": 1, "b": [1, 2, 3], "c": null});
        assert!(compute_diff(&v, &v).is_empty());
    }

    /// compute_diff: output non-oggetto diversi -> pseudo-radice.
    #[test]
    fn compute_diff_non_oggetto() {
        assert_eq!(compute_diff(&json!(1), &json!(2)), vec!["<root>".to_string()]);
        assert!(compute_diff(&json!("x"), &json!("x")).is_empty());
    }

    /// DiffCollector: accumula e calcola la convergenza globale.
    #[test]
    fn diff_collector_accumula_e_converge() {
        let mut c = DiffCollector::new();
        c.record("router", json!({"intent": "a"}), json!({"intent": "a"}));
        assert!(c.is_converged(), "nessuna divergenza finora");

        let d = c.record("planner", json!({"plan": 1}), json!({"plan": 2}));
        assert_eq!(d.divergent_keys, vec!["plan".to_string()]);
        assert!(!c.is_converged(), "ora c'e' una divergenza");
        assert_eq!(c.diffs().len(), 2);
    }
}

#[cfg(test)]
mod db_tests {
    use super::*;
    use serde_json::json;
    use sqlx::PgPool;

    /// Crea la tabella telemetria nel DB di test iniettato da `#[sqlx::test]`
    /// (stesso pattern di checkpoint_pg.rs: lo schema reale e' nella mig 0453).
    async fn create_telemetry_table(pool: &PgPool) {
        sqlx::query(
            "CREATE TABLE nexus_shadow_telemetry ( \
                 id             UUID PRIMARY KEY, \
                 run_id         UUID NOT NULL, \
                 node_name      TEXT NOT NULL, \
                 primary_output JSONB NOT NULL, \
                 shadow_output  JSONB NOT NULL, \
                 divergent_keys TEXT[] NOT NULL DEFAULT '{}', \
                 created_at     TIMESTAMPTZ NOT NULL DEFAULT now() \
             )",
        )
        .execute(pool)
        .await
        .expect("create table nexus_shadow_telemetry");
    }

    #[sqlx::test]
    async fn persist_node_diff_inserisce_riga_con_divergenze(pool: PgPool) {
        create_telemetry_table(&pool).await;
        let run_id = uuid::Uuid::new_v4();
        let primary = json!({"user_intent": "code_write", "token_budget": 400});
        let shadow = json!({"user_intent": "code_write", "token_budget": 500});

        let id = persist_node_diff(&pool, run_id, "router", &primary, &shadow)
            .await
            .expect("insert telemetria");

        // Verifica la riga persistita: divergent_keys deve contenere token_budget.
        let row = sqlx::query_as::<_, (uuid::Uuid, String, Vec<String>)>(
            "SELECT run_id, node_name, divergent_keys FROM nexus_shadow_telemetry WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("select riga inserita");

        assert_eq!(row.0, run_id);
        assert_eq!(row.1, "router");
        assert_eq!(row.2, vec!["token_budget".to_string()]);
    }

    #[sqlx::test]
    async fn persist_node_diff_parita_divergent_keys_vuoto(pool: PgPool) {
        create_telemetry_table(&pool).await;
        let run_id = uuid::Uuid::new_v4();
        let v = json!({"user_intent": "chat", "token_budget": 400});

        let id = persist_node_diff(&pool, run_id, "router", &v, &v)
            .await
            .expect("insert telemetria");

        let keys = sqlx::query_scalar::<_, Vec<String>>(
            "SELECT divergent_keys FROM nexus_shadow_telemetry WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("select divergent_keys");

        assert!(keys.is_empty(), "parita' -> nessuna chiave divergente");
    }
}

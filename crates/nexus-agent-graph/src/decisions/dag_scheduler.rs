//! `dag_scheduler`: funzioni pure decisionali per l'esecuzione parallela dei
//! layer del DAG. Porting 1:1 di `brain/agents/dag_scheduler.py` (solo le parti
//! pure: [`compute_ready_layer`] e [`should_parallelize`]; `run_dag_layer`
//! richiede IO/tool e resta lato brain).
//!
//! Punto unico (regola L) della decisione "quali todo sono eseguibili in
//! parallelo ora" e "conviene attivare il DAG parallelo": l'executor delega qui
//! invece di re-implementare la guardia.

use serde::{Deserialize, Serialize};

/// Stato di un todo del piano. Stringhe stabili (serde rename) coerenti con
/// `nexus_agent_todos.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TodoStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "in_progress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "skipped")]
    Skipped,
    #[serde(rename = "blocked")]
    Blocked,
}

/// Un todo del DAG. `id` e `depends_on` sono identificatori opachi (stringhe),
/// coerenti con il cast `::text[]` lato Python (le deps arrivano come stringhe).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Todo {
    pub id: String,
    pub status: TodoStatus,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// Config del DAG parallelo (PARAMETRO esplicito, no lettura DB: regola G).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DagConfig {
    /// Numero minimo di todo ready per parallelizzare in assenza di dipendenze.
    pub dag_parallel_min_ready: i64,
}

impl Default for DagConfig {
    fn default() -> Self {
        // Default documentato Python: `cfg.get("dag_parallel_min_ready", 2)`.
        Self {
            dag_parallel_min_ready: 2,
        }
    }
}

/// Ritorna i todo pending le cui dipendenze sono tutte completed/skipped.
///
/// E' il fronte eseguibile in parallelo del DAG. Se nessun todo ha dipendenze,
/// ritorna tutti i pending (il chiamante applichera' il cap). Vedi
/// `compute_ready_layer` Python.
pub fn compute_ready_layer(todos: &[Todo]) -> Vec<Todo> {
    let done: std::collections::HashSet<&str> = todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Completed | TodoStatus::Skipped))
        .map(|t| t.id.as_str())
        .collect();
    todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Pending))
        .filter(|t| t.depends_on.iter().all(|d| done.contains(d.as_str())))
        .cloned()
        .collect()
}

/// Decide se attivare il DAG parallelo (Ultra, decomposizione parallela).
///
/// True se esiste un ready layer e:
///   - ci sono dipendenze esplicite fra i todo (comportamento storico), OPPURE
///   - ci sono almeno `dag_parallel_min_ready` todo ready (con min_ready >= 2).
///
/// Con `dag_parallel_min_ready` <= 1 resta il comportamento storico. Vedi
/// `should_parallelize` Python.
pub fn should_parallelize(ready: &[Todo], todos: &[Todo], cfg: &DagConfig) -> bool {
    if ready.is_empty() {
        return false;
    }
    let has_deps = todos.iter().any(|t| !t.depends_on.is_empty());
    let min_ready = cfg.dag_parallel_min_ready;
    has_deps || (min_ready >= 2 && (ready.len() as i64) >= min_ready)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn todo(id: &str, status: TodoStatus, deps: &[&str]) -> Todo {
        Todo {
            id: id.to_string(),
            status,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn ready_layer_senza_dipendenze() {
        let todos = vec![
            todo("a", TodoStatus::Pending, &[]),
            todo("b", TodoStatus::Pending, &[]),
        ];
        let ready = compute_ready_layer(&todos);
        assert_eq!(ready.len(), 2);
    }

    #[test]
    fn ready_layer_con_dipendenza_non_soddisfatta() {
        let todos = vec![
            todo("a", TodoStatus::Pending, &[]),
            todo("b", TodoStatus::Pending, &["a"]),
        ];
        let ready = compute_ready_layer(&todos);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "a");
    }

    #[test]
    fn parallelize_min_ready() {
        let todos = vec![
            todo("a", TodoStatus::Pending, &[]),
            todo("b", TodoStatus::Pending, &[]),
        ];
        let ready = compute_ready_layer(&todos);
        assert!(should_parallelize(&ready, &todos, &DagConfig::default()));
    }

    #[test]
    fn no_parallelize_singolo_ready_senza_deps() {
        let todos = vec![todo("a", TodoStatus::Pending, &[])];
        let ready = compute_ready_layer(&todos);
        assert!(!should_parallelize(&ready, &todos, &DagConfig::default()));
    }
}

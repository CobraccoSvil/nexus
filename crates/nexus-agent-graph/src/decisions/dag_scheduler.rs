//! `dag_scheduler`: PUNTO UNICO (regola L) di TUTTA la logica DAG dei todo del
//! grafo agentico. Porting 1:1 della parte PURA (no IO) di due moduli Python che
//! oggi condividono la stessa logica DAG:
//!   - `brain/agents/dag_scheduler.py`: [`compute_ready_layer`],
//!     [`should_parallelize`] e [`descendants`] (`_descendants`).
//!   - `brain/agents/verifier_node.py`: [`pick_next_todo`] (`_pick_next_todo`).
//! (`run_dag_layer`/`_advance_or_end` richiedono IO/tool e restano lato brain.)
//!
//! Regola L: la selezione/scheduling dei todo (quale eseguire ora, quali sono
//! pronti in parallelo, quali discendono da un fallimento) ha UN solo punto
//! autoritativo. I nodi che la useranno (`verifier_node`, `todo_runner_node`,
//! `dag_scheduler`) NON re-implementano la logica: la chiamano. Un nuovo
//! requisito (es. un nuovo status, una nuova guardia) si aggiunge UNA volta qui.
//!
//! Tutte le funzioni sono PURE: nessun IO, nessuna lettura DB. La config
//! DB-driven (es. `dag_topological_enabled`, `dag_parallel_min_ready`) arriva
//! come PARAMETRO esplicito (regola G), mai letta dentro le funzioni. L'I/O DB
//! e' dietro il trait [`crate::runtime::TodoStore`] (impl concreta = mcp-core).
//!
//! INVARIANTE `depends_on` (regola H, bug 2026-06-10): `Todo::depends_on` e' un
//! `Vec<String>`, MAI una stringa. Lato Python `nexus_agent_todos.depends_on` e'
//! `uuid[]` e psycopg2 senza array-uuid typecaster lo ritornava come STRINGA
//! `"{...}"`; iterando "sui caratteri" il fronte parallelo collassava a vuoto e
//! il verifier andava in falso deadlock. Il fix fu il cast `::text[]` in
//! `todo_store.list_todos`. In Rust l'invariante e' fissato DAL TIPO: la
//! `TodoStore` concreta deve garantire `depends_on` come `Vec` (cast `::text[]`),
//! la deserializzazione di una stringa qui fallisce (non itera sui char).

use serde::{Deserialize, Serialize};

/// Stato di un todo del piano. Stringhe stabili (serde rename) coerenti con il
/// `CHECK` di `nexus_agent_todos.status` (migrazione 0148): i SOLI cinque valori
/// ammessi per una riga todo sono `pending`, `in_progress`, `completed`,
/// `blocked`, `skipped`. NON esiste `completed_verified` per un todo: quello e'
/// uno status del RUN agentico (vedi `mcp-core::agent_types`), non della riga
/// todo. `todo_runner_node.py:208` lo confronta sull'esito del SUB-RUN, non sul
/// todo. Per le dipendenze DAG conta quindi solo {`completed`, `skipped`}.
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

impl TodoStatus {
    /// `true` se questo status SODDISFA una dipendenza DAG. Punto unico (regola
    /// L) del set "soddisfatto": sia `compute_ready_layer` che `pick_next_todo`
    /// lo usano, cosi' il criterio non e' duplicato in due query.
    ///
    /// Verificato 1:1 nel Python: `dag_scheduler.compute_ready_layer`
    /// (`done = {... status in ("completed", "skipped")}`) e
    /// `verifier_node._pick_next_todo` (stesso set). Solo `Completed` e
    /// `Skipped` contano: un `Blocked`/`InProgress`/`Pending` NON soddisfa.
    pub fn satisfies_dependency(self) -> bool {
        matches!(self, TodoStatus::Completed | TodoStatus::Skipped)
    }
}

/// Un todo del DAG. `id` e `depends_on` sono identificatori opachi (stringhe),
/// coerenti col cast `::text[]` lato Python (id e deps arrivano come stringhe).
///
/// INVARIANTE: `depends_on` e' un `Vec<String>`, MAI una stringa `"{...}"` (vedi
/// la nota di modulo, bug 2026-06-10). La `TodoStore` concreta deve garantirlo
/// col cast `::text[]`.
///
/// `seq` e' l'ordine del piano (`nexus_agent_todos.seq`): la `TodoStore`
/// restituisce gli elementi GIA' ordinati per `seq` ascendente (come
/// `todo_store.list_todos`, `ORDER BY seq ASC`); la logica DAG si basa
/// sull'ORDINE dello slice (tie-break deterministico), `seq` e' trasportato per
/// diagnostica/round-trip ma non riordina dentro le funzioni pure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Todo {
    pub id: String,
    pub status: TodoStatus,
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Ordine del piano (`seq` ascendente). Opzionale per i golden minimali.
    #[serde(default)]
    pub seq: Option<i64>,
    /// Aree file (path/prefissi relativi alla root) che il todo DICHIARA di voler
    /// scrivere. Popolato in un PR successivo dalla colonna `nexus_agent_todos`
    /// quando `dispatch_wave` la consuma per verificare la DISGIUNZIONE della wave
    /// parallela via [`crate::decisions::orchestration_reason::subtasks_are_disjoint`]
    /// (punto unico, regola L). In PR1 e' solo il campo Rust (nessuna persistenza):
    /// resta vuoto -> il comportamento e' invariato. `#[serde(default)]` per
    /// retrocompat (golden/checkpoint pre-esistenti non hanno il campo).
    #[serde(default)]
    pub write_scope: Vec<String>,
    /// Testo del todo (`nexus_agent_todos.content`). TRASPORTATO per la
    /// presentazione (il meta-step "plan" del nastro lo pubblica in chat), NON
    /// usato dallo scheduling DAG (che ordina per dipendenze/seq): stesso spirito
    /// di `seq`/`write_scope`, campo portato non decisionale. Senza, il piano nel
    /// nastro appariva come righe vuote "[ ] -" (content=null nel payload).
    /// `#[serde(default)]` per retrocompat golden/checkpoint pre-esistenti.
    #[serde(default)]
    pub content: Option<String>,
    /// Priorita' del todo (`nexus_agent_todos.priority`), trasportata per il
    /// meta-step di presentazione; non usata dallo scheduling.
    #[serde(default)]
    pub priority: Option<String>,
    /// Criteri di accettazione (`nexus_agent_todos.acceptance_criteria`, JSONB).
    /// TRASPORTATI come `content`/`priority`, e come loro non usati dallo
    /// scheduling: li consuma il `VerifierNode`.
    ///
    /// Senza questo campo il verifier non ne eseguiva MAI uno. Il dato c'era —
    /// la colonna esiste dalla migrazione project 0002 e il tool `todos` la
    /// scrive — ma il verifier lo cercava dentro la ri-serializzazione di questo
    /// tipo (`todo_value_of`), che non lo portava: risultato, lista sempre vuota
    /// e ramo "nessun criterion" a ogni giro. La prova indipendente e' che
    /// `nexus_agent_verifier_runs` era a zero righe su 104 todo con criteri.
    ///
    /// `Vec<Value>` e non un tipo strutturato: la forma la normalizza il
    /// verifier (`normalize_criteria`), che e' il punto unico dove il
    /// vocabolario dei criteri viene interpretato.
    #[serde(default)]
    pub acceptance_criteria: Vec<serde_json::Value>,
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

/// Insieme degli id dei todo che soddisfano una dipendenza (`completed` o
/// `skipped`). Helper interno: criterio "soddisfatto" centralizzato in
/// [`TodoStatus::satisfies_dependency`] (regola L, niente filtro duplicato fra
/// `compute_ready_layer` e `pick_next_todo`).
fn satisfied_ids(todos: &[Todo]) -> std::collections::HashSet<&str> {
    todos
        .iter()
        .filter(|t| t.status.satisfies_dependency())
        .map(|t| t.id.as_str())
        .collect()
}

/// Ritorna i todo pending le cui dipendenze sono tutte completed/skipped.
///
/// E' il fronte eseguibile in parallelo del DAG. Se nessun todo ha dipendenze,
/// ritorna tutti i pending (il chiamante applichera' il cap). Vedi
/// `compute_ready_layer` Python (`dag_scheduler.py:38-52`).
pub fn compute_ready_layer(todos: &[Todo]) -> Vec<Todo> {
    let done = satisfied_ids(todos);
    todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Pending))
        .filter(|t| t.depends_on.iter().all(|d| done.contains(d.as_str())))
        .cloned()
        .collect()
}

/// Sceglie il prossimo todo da eseguire (1:1 con `verifier_node._pick_next_todo`,
/// `verifier_node.py:508-534`). Punto unico (regola L) della selezione
/// sequenziale: il verifier delega qui invece di re-implementare la cascata.
///
/// Cascata (nell'ordine, identica al Python):
///   1. `pending` = todo con status `Pending`. Se vuoto -> `None` (nessun lavoro
///      pendente; il chiamante terminera').
///   2. Se `dag_topological_enabled` e' `false` OPPURE nessun todo ha
///      `depends_on` -> primo `pending` per ordine (comportamento storico: lo
///      slice arriva gia' ordinato per `seq`, il tie-break e' l'ordine).
///   3. Con DAG topologico ON e dipendenze presenti -> primo `pending` (per
///      ordine, tie-break deterministico) le cui dipendenze sono TUTTE
///      soddisfatte (`completed`/`skipped`).
///   4. Fallback deadlock: se nessun pending ha le dipendenze soddisfatte
///      (dipendenza `blocked` o ciclo residuo) -> primo `pending`, per non
///      bloccare il loop.
///
/// `dag_topological_enabled` e' un PARAMETRO esplicito (regola G): la funzione
/// non legge il DB. Ritorna un riferimento al todo scelto (preso dallo slice
/// d'ingresso), o `None`.
pub fn pick_next_todo(todos: &[Todo], dag_topological_enabled: bool) -> Option<&Todo> {
    let first_pending = todos
        .iter()
        .find(|t| matches!(t.status, TodoStatus::Pending));
    let first_pending = first_pending?; // nessun pending -> None (passo 1)

    let has_deps = todos.iter().any(|t| !t.depends_on.is_empty());
    if !dag_topological_enabled || !has_deps {
        return Some(first_pending); // passo 2: primo pending per ordine
    }

    let done = satisfied_ids(todos);
    let executable = todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Pending))
        .find(|t| t.depends_on.iter().all(|d| done.contains(d.as_str())));
    match executable {
        Some(t) => Some(t),          // passo 3: primo eseguibile
        None => Some(first_pending), // passo 4: fallback deadlock
    }
}

/// `true` se il piano dei todo e' PIENAMENTE RISOLTO CON LAVORO REALE: OGNI todo
/// e' in uno stato terminale che soddisfa una dipendenza
/// ([`TodoStatus::satisfies_dependency`], cioe' `completed`/`skipped`) E almeno
/// uno e' `completed`.
///
/// Punto unico (regola L) del criterio "il piano si e' concluso avendo prodotto
/// lavoro". NON e' equivalente a `pick_next_todo(..) == None`: quello ritorna
/// `None` anche quando restano todo `blocked` (piano FERMO, non completato) e
/// non distingue un piano tutto `skipped` (nessun lavoro reale) da uno eseguito.
///
/// A COSA SERVE (regola M, falso positivo hollow su todo-isolation): quando
/// `supervisor_mode=continuous` i todo vengono eseguiti come SUB-RUN isolati e il
/// run PRINCIPALE e' un semplice dispatcher: `route_after_todo_runner` lo manda a
/// FinalGate/Learner e MAI all'Executor, percio' non scrive `agent_steps` sul
/// proprio `run_id` e non produce un `final_answer` (nessun turno di sintesi).
/// La detection "hollow completion" del finalizzatore vedrebbe quindi 0 step +
/// risposta vuota e lo scambierebbe per un completamento allucinato: questo
/// predicato e' il SEGNALE STRUTTURATO che distingue "lavoro svolto per delega"
/// da "nessun lavoro". Un piano vuoto, con `pending`/`in_progress`/`blocked`, o
/// tutto `skipped`, NON lo soddisfa: quei run restano soggetti alla detection.
pub fn plan_todos_all_completed(todos: &[Todo]) -> bool {
    !todos.is_empty()
        && todos.iter().all(|t| t.status.satisfies_dependency())
        && todos.iter().any(|t| matches!(t.status, TodoStatus::Completed))
}

/// Insieme dei todo che dipendono (diretta o transitivamente) da `todo_id`
/// (1:1 con `dag_scheduler._descendants`, `dag_scheduler.py:76-90`). Usato per
/// il cascade-skip: se un todo fallisce, tutti i suoi discendenti vengono
/// marcati `skipped`.
///
/// Costruisce la mappa figli `dep -> [todo che la dichiarano]` e fa una DFS
/// iterativa con set di visitati: gestisce cicli e diamanti SENZA loop infinito
/// (un nodo gia' in `out` non viene riaccodato). `todo_id` stesso NON e' incluso
/// nel risultato (solo i discendenti).
pub fn descendants<'a>(todos: &'a [Todo], todo_id: &str) -> std::collections::HashSet<&'a str> {
    use std::collections::HashMap;
    // children[dep] = id dei todo che dichiarano `dep` in depends_on.
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for t in todos {
        for d in &t.depends_on {
            children.entry(d.as_str()).or_default().push(t.id.as_str());
        }
    }
    let mut out: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut stack: Vec<&str> = vec![todo_id];
    while let Some(cur) = stack.pop() {
        if let Some(kids) = children.get(cur) {
            for &c in kids {
                if out.insert(c) {
                    // insert ritorna true solo se non era presente: niente
                    // riaccodamento -> niente loop infinito su cicli/diamanti.
                    stack.push(c);
                }
            }
        }
    }
    out
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
            seq: None,
            write_scope: Vec::new(),
            content: None,
            priority: None,
            acceptance_criteria: Vec::new(),
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

    // --- plan_todos_all_completed: segnale "piano concluso con lavoro" -------
    // Boundary del predicato che salva i run dispatcher di todo-isolation dal
    // falso positivo "hollow completion" (0 step + risposta vuota per delega).

    #[test]
    fn piano_tutto_completed_e_concluso_con_lavoro() {
        let todos = vec![
            todo("a", TodoStatus::Completed, &[]),
            todo("b", TodoStatus::Completed, &["a"]),
        ];
        assert!(plan_todos_all_completed(&todos));
    }

    #[test]
    fn piano_completed_piu_skipped_resta_concluso_con_lavoro() {
        // `skipped` soddisfa una dipendenza (cascade-skip legittimo): finche'
        // ALMENO UNO e' completed il piano ha prodotto lavoro reale.
        let todos = vec![
            todo("a", TodoStatus::Completed, &[]),
            todo("b", TodoStatus::Skipped, &["a"]),
        ];
        assert!(plan_todos_all_completed(&todos));
    }

    #[test]
    fn piano_tutto_skipped_non_e_lavoro() {
        // Nessun todo eseguito davvero: il run NON va esentato dalla detection
        // hollow, altrimenti si inghiottirebbe un dispatch a vuoto.
        let todos = vec![
            todo("a", TodoStatus::Skipped, &[]),
            todo("b", TodoStatus::Skipped, &[]),
        ];
        assert!(!plan_todos_all_completed(&todos));
    }

    #[test]
    fn piano_con_blocked_non_e_concluso() {
        // Dispatch parzialmente FALLITO: deve restare hollow-eligible, cosi' il
        // fallimento vero emerge invece di essere mascherato.
        let todos = vec![
            todo("a", TodoStatus::Completed, &[]),
            todo("b", TodoStatus::Blocked, &["a"]),
        ];
        assert!(!plan_todos_all_completed(&todos));
    }

    #[test]
    fn piano_con_pending_o_in_progress_non_e_concluso() {
        let con_pending = vec![
            todo("a", TodoStatus::Completed, &[]),
            todo("b", TodoStatus::Pending, &[]),
        ];
        assert!(!plan_todos_all_completed(&con_pending));
        let con_in_progress = vec![
            todo("a", TodoStatus::Completed, &[]),
            todo("b", TodoStatus::InProgress, &[]),
        ];
        assert!(!plan_todos_all_completed(&con_in_progress));
    }

    #[test]
    fn piano_vuoto_non_e_concluso() {
        // Run SENZA piano (la maggioranza): nessuna esenzione, detection hollow
        // invariata (nessuna regressione sull'incidente 0-step b07c7e78).
        assert!(!plan_todos_all_completed(&[]));
    }

    #[test]
    fn non_e_equivalente_a_pick_next_todo_none() {
        // TRANELLO da non reintrodurre: `pick_next_todo == None` NON e' il
        // segnale giusto. Qui non c'e' alcun pending (quindi pick_next -> None)
        // ma il piano e' FERMO su un blocked, non concluso.
        let todos = vec![
            todo("a", TodoStatus::Completed, &[]),
            todo("b", TodoStatus::Blocked, &["a"]),
        ];
        assert!(pick_next_todo(&todos, true).is_none());
        assert!(
            !plan_todos_all_completed(&todos),
            "un piano con todo blocked non e' concluso: usare pick_next_todo==None \
             come proxy esenterebbe un dispatch fallito"
        );
    }

    #[test]
    fn pick_next_nessun_pending_e_none() {
        let todos = vec![
            todo("a", TodoStatus::Completed, &[]),
            todo("b", TodoStatus::Skipped, &[]),
        ];
        assert!(pick_next_todo(&todos, true).is_none());
        assert!(pick_next_todo(&todos, false).is_none());
    }

    #[test]
    fn pick_next_off_primo_pending_per_ordine() {
        // DAG off: primo pending nell'ordine, ignora le deps.
        let todos = vec![
            todo("a", TodoStatus::Completed, &[]),
            todo("b", TodoStatus::Pending, &["c"]), // dep non soddisfatta, ma DAG off
            todo("c", TodoStatus::Pending, &[]),
        ];
        let next = pick_next_todo(&todos, false).unwrap();
        assert_eq!(next.id, "b");
    }

    #[test]
    fn pick_next_on_senza_deps_primo_pending() {
        // DAG on ma nessun depends_on -> comportamento storico (primo pending).
        let todos = vec![
            todo("a", TodoStatus::Completed, &[]),
            todo("b", TodoStatus::Pending, &[]),
            todo("c", TodoStatus::Pending, &[]),
        ];
        let next = pick_next_todo(&todos, true).unwrap();
        assert_eq!(next.id, "b");
    }

    #[test]
    fn pick_next_on_deps_soddisfatte() {
        // DAG on + deps: salta b (dep non pronta), sceglie c (deps soddisfatte).
        let todos = vec![
            todo("a", TodoStatus::Completed, &[]),
            todo("b", TodoStatus::Pending, &["x"]), // x non esiste/non soddisfatta
            todo("c", TodoStatus::Pending, &["a"]), // a completed -> pronto
        ];
        let next = pick_next_todo(&todos, true).unwrap();
        assert_eq!(next.id, "c");
    }

    #[test]
    fn pick_next_deadlock_fallback_primo_pending() {
        // DAG on + deps: nessun pending eseguibile (dep blocked) -> primo pending.
        let todos = vec![
            todo("a", TodoStatus::Blocked, &[]),
            todo("b", TodoStatus::Pending, &["a"]),
            todo("c", TodoStatus::Pending, &["a"]),
        ];
        let next = pick_next_todo(&todos, true).unwrap();
        assert_eq!(next.id, "b", "fallback al primo pending per ordine");
    }

    #[test]
    fn descendants_lineare() {
        // a -> b -> c (b dipende da a, c da b). Discendenti di a = {b, c}.
        let todos = vec![
            todo("a", TodoStatus::Blocked, &[]),
            todo("b", TodoStatus::Pending, &["a"]),
            todo("c", TodoStatus::Pending, &["b"]),
        ];
        let desc = descendants(&todos, "a");
        assert_eq!(desc, ["b", "c"].into_iter().collect());
        // c non ha discendenti.
        assert!(descendants(&todos, "c").is_empty());
    }

    #[test]
    fn descendants_diamante() {
        // a -> b, a -> c, b -> d, c -> d. Discendenti di a = {b, c, d} (d una volta).
        let todos = vec![
            todo("a", TodoStatus::Blocked, &[]),
            todo("b", TodoStatus::Pending, &["a"]),
            todo("c", TodoStatus::Pending, &["a"]),
            todo("d", TodoStatus::Pending, &["b", "c"]),
        ];
        let desc = descendants(&todos, "a");
        assert_eq!(desc, ["b", "c", "d"].into_iter().collect());
    }

    #[test]
    fn descendants_ciclo_non_loop_infinito() {
        // Ciclo degenerato a -> b -> a: la DFS termina (set visitati).
        let todos = vec![
            todo("a", TodoStatus::Pending, &["b"]),
            todo("b", TodoStatus::Pending, &["a"]),
        ];
        let desc = descendants(&todos, "a");
        assert_eq!(desc, ["a", "b"].into_iter().collect());
    }
}

/// Golden-test di PARITA' 1:1 vs Python per la logica DAG dei todo.
///
/// Lo script `crates/nexus-agent-graph/scripts/gen_golden_todo_dag.py` (versionato)
/// importa le funzioni REALI del brain (`_pick_next_todo` da `verifier_node`,
/// `_descendants`/`compute_ready_layer` da `dag_scheduler`) se importabili senza
/// I/O, altrimenti ne usa una replica byte-fedele, ed emette
/// `/tmp/golden_todo_dag.json`. Questo test carica quel JSON e verifica che la
/// funzione Rust produca lo STESSO output del Python.
///
/// `#[ignore]` perche' dipende dal file generato. Comando:
///   python3 crates/nexus-agent-graph/scripts/gen_golden_todo_dag.py
///   cargo test -p nexus-agent-graph --lib -- --ignored
#[cfg(test)]
mod golden {
    use super::*;
    use serde::Deserialize;
    use serde_json::Value;

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        case_id: String,
        function: String,
        input: Value,
        output: Value,
    }

    #[derive(Debug, Deserialize)]
    struct TodosInput {
        todos: Vec<Todo>,
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        dag_topological_enabled: Option<bool>,
    }

    #[test]
    #[ignore = "richiede /tmp/golden_todo_dag.json generato da gen_golden_todo_dag.py"]
    fn golden_todo_dag_parita() {
        let Some(raw) =
            crate::golden_util::load_golden("golden_todo_dag.json", "gen_golden_todo_dag.py")
        else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(
            cases.len() >= 25,
            "attesi >=25 casi, trovati {}",
            cases.len()
        );

        let mut checked = 0usize;
        for c in &cases {
            let input: TodosInput = serde_json::from_value(c.input.clone()).expect("TodosInput");
            let got: Value = match c.function.as_str() {
                "compute_ready_layer" => {
                    let ready = compute_ready_layer(&input.todos);
                    // L'oracolo Python emette gli id nell'ordine dello slice.
                    let ids: Vec<String> = ready.into_iter().map(|t| t.id).collect();
                    serde_json::to_value(ids).expect("serialize ready ids")
                }
                "descendants" => {
                    let id = input.id.expect("descendants richiede 'id'");
                    let desc = descendants(&input.todos, &id);
                    // L'oracolo emette un set ordinato (sorted): ordiniamo anche qui.
                    let mut ids: Vec<&str> = desc.into_iter().collect();
                    ids.sort_unstable();
                    serde_json::to_value(ids).expect("serialize descendants")
                }
                "pick_next_todo" => {
                    let dag = input.dag_topological_enabled.unwrap_or(false);
                    let sel = pick_next_todo(&input.todos, dag);
                    match sel {
                        Some(t) => Value::String(t.id.clone()),
                        None => Value::Null,
                    }
                }
                other => panic!("funzione golden sconosciuta: {other} (caso {})", c.case_id),
            };
            assert_eq!(
                got, c.output,
                "PARITA' FALLITA caso {} ({}):\n  rust   = {}\n  python = {}",
                c.case_id, c.function, got, c.output
            );
            checked += 1;
        }
        println!("golden todo_dag: {checked} casi verificati, tutti verdi");
    }
}

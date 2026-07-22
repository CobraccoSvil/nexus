//! `orchestration_reason`: parte PURA (regola L) del meta-reasoner di
//! ORCHESTRAZIONE. GEMELLO di [`crate::decisions::meta_reason`] (stesso stile,
//! golden-abile in isolamento), su tipi DISGIUNTI: nessun tipo condiviso col
//! recovery, nessun enum wrapper (regola L, design v2).
//!
//! Il ragionamento LLM contestuale (SE/COME fare la plan-phase, decomporre,
//! delegare) sostituisce l'euristica fissa (`is_eligible`/`should_parallelize`),
//! ma la parte non-deterministica (la chiamata LLM) vive dietro il metodo
//! [`crate::runtime::ports::MetaReasonerPort::orchestrate`]. Qui sta SOLO la
//! logica deterministica (regola M: nessuna prosa, solo segnali strutturati):
//!   - [`build_orchestration_context`]: costruzione DETERMINISTICA del
//!     [`OrchestrationContext`] dai segnali gia' risolti a monte
//!     (routing/context_reduction/depth/cost).
//!   - [`orch_epoch`]: epoca di lavoro STABILE (chiave idempotenza/replay). In
//!     Fase 1 (plan-entry) e' per-run: la decisione di orchestrazione all'ingresso
//!     e' presa UNA volta per run (segue la disciplina di
//!     [`crate::decisions::meta_reason::work_epoch`]).
//!   - [`validate_orch_move`]: PUNTO UNICO di validazione dell'output JSON dell'LLM
//!     contro l'enum CHIUSO [`OrchestrationMove`]; qualunque forma malformata /
//!     `Decompose` senza blocchi / `DelegateSubagents` senza task o vietata /
//!     `ParallelIsolated` senza isolamento fisico degrada a
//!     [`OrchestrationMove::Fallback`] (rete di sicurezza: euristica esistente).

use crate::runtime::ports::{
    ContextPressure, Coordination, OrchPhase, OrchestrationContext, OrchestrationMove,
};

/// Soglia (rapporto used/limit) oltre cui la pressione del contesto e' MEDIA.
/// Soglia di CALCOLO PURO deterministico (non config di business: e' la
/// derivazione strutturata di [`ContextPressure`] dai token, come le soglie
/// aritmetiche di [`crate::decisions::meta_reason::work_epoch`]), non un magic
/// fallback su un comportamento (regola G): non accende feature ne' sceglie
/// modelli, mappa solo un rapporto in un enum.
const PRESSURE_MEDIUM_RATIO: f64 = 0.60;

/// Soglia (rapporto used/limit) oltre cui la pressione del contesto e' ALTA.
const PRESSURE_HIGH_RATIO: f64 = 0.85;

/// Deriva la [`ContextPressure`] DETERMINISTICAMENTE dal rapporto
/// `context_tokens_used / context_window_limit` (regola M: segnale strutturato,
/// mai prosa). `limit <= 0` (ignoto) o `used <= 0` -> [`ContextPressure::Low`]
/// (nessuna informazione = nessuna pressione presunta). Punto unico del calcolo:
/// [`build_orchestration_context`] e i test lo riusano.
pub fn context_pressure_from_tokens(used: i64, limit: i64) -> ContextPressure {
    if limit <= 0 || used <= 0 {
        return ContextPressure::Low;
    }
    let ratio = used as f64 / limit as f64;
    if ratio >= PRESSURE_HIGH_RATIO {
        ContextPressure::High
    } else if ratio >= PRESSURE_MEDIUM_RATIO {
        ContextPressure::Medium
    } else {
        ContextPressure::Low
    }
}

/// Epoca di lavoro STABILE per la chiave di idempotenza/replay del meta-reasoner
/// di orchestrazione (`orch_move::{phase}::{orch_epoch}`). In Fase 1 la sola fase
/// e' [`OrchPhase::PlanEntry`]: la decisione di orchestrazione e' presa
/// all'INGRESSO del run, UNA volta -> l'epoca e' banalmente per-run (0). Segue la
/// disciplina di [`crate::decisions::meta_reason::work_epoch`]: valore stabile e
/// deterministico, non la coda-segnali volatile. Le fasi successive (worktree)
/// potranno far avanzare l'epoca su cambi macroscopici senza toccare i chiamanti.
pub fn orch_epoch(phase: OrchPhase) -> i64 {
    match phase {
        OrchPhase::PlanEntry => 0,
    }
}

/// Costruisce il [`OrchestrationContext`] deterministicamente dai segnali gia'
/// risolti a monte (regola M: tutti strutturati). `delegation_forbidden` NON e'
/// dedotto qui da prosa: e' l'aggregazione DETERMINISTICA delle guard di delega
/// (depth oltre soglia / cost oltre cap / policy esplicita), calcolata dal punto
/// unico [`delegation_forbidden`]. La [`ContextPressure`] deriva da used/limit col
/// punto unico [`context_pressure_from_tokens`]. `isolation_available` e' un
/// segnale strutturato in ingresso (regola M): il call site che conosce
/// project_root/is_git_repo lo calcola; qui viene solo trasportato nel contesto (in
/// Fase 1 e' sempre `false`).
#[allow(clippy::too_many_arguments)]
pub fn build_orchestration_context(
    phase: OrchPhase,
    user_intent: Option<&str>,
    behavior_mode: &str,
    token_budget: i64,
    task_complexity: i64,
    agentic_score: i64,
    is_ambiguous: bool,
    plan_exists: bool,
    context_tokens_used: i64,
    context_window_limit: i64,
    history_len: i64,
    subagent_depth: i64,
    subagent_depth_limit: i64,
    cost_spent_usd: f64,
    cost_cap_usd: f64,
    policy_forbids_delegation: bool,
    isolation_available: bool,
) -> OrchestrationContext {
    let context_pressure = context_pressure_from_tokens(context_tokens_used, context_window_limit);
    let delegation_forbidden = delegation_forbidden(
        subagent_depth,
        subagent_depth_limit,
        cost_spent_usd,
        cost_cap_usd,
        policy_forbids_delegation,
    );
    OrchestrationContext {
        phase,
        user_intent: user_intent.map(str::to_string),
        behavior_mode: behavior_mode.to_string(),
        token_budget,
        task_complexity,
        agentic_score,
        is_ambiguous,
        plan_exists,
        context_tokens_used,
        context_window_limit,
        history_len,
        context_pressure,
        subagent_depth,
        cost_spent_usd,
        cost_cap_usd,
        delegation_forbidden,
        // Segnale strutturato in ingresso (regola M): calcolato al call site che
        // conosce project_root/is_git_repo, MAI dedotto qui. In Fase 1 il planner
        // passa sempre `false` -> ParallelIsolated degradata a Sequential.
        isolation_available,
    }
}

/// Aggrega DETERMINISTICAMENTE le guard di delega in un solo booleano (regola M +
/// L: punto unico della guard, i chiamanti non re-implementano i confronti). La
/// delega e' VIETATA se ALMENO UNA guard scatta:
///   - `subagent_depth >= subagent_depth_limit` (limite > 0): annidamento oltre
///     soglia (evita ricorsione incontrollata di sub-agenti);
///   - `cost_cap_usd > 0 && cost_spent_usd >= cost_cap_usd`: budget di costo del
///     run esaurito;
///   - `policy_forbids_delegation`: divieto esplicito di policy (a monte).
/// Un `subagent_depth_limit <= 0` o `cost_cap_usd <= 0` DISATTIVA quella guard
/// (nessun cap configurato) — non e' un magic fallback: e' l'assenza esplicita del
/// vincolo (regola G, i valori arrivano dai settings risolti a monte).
pub fn delegation_forbidden(
    subagent_depth: i64,
    subagent_depth_limit: i64,
    cost_spent_usd: f64,
    cost_cap_usd: f64,
    policy_forbids_delegation: bool,
) -> bool {
    let depth_exceeded = subagent_depth_limit > 0 && subagent_depth >= subagent_depth_limit;
    let cost_exceeded = cost_cap_usd > 0.0 && cost_spent_usd >= cost_cap_usd;
    policy_forbids_delegation || depth_exceeded || cost_exceeded
}

/// DENYLIST di path scritti IMPLICITAMENTE da build/install o condivisi/generati:
/// se un `write_scope` li tocca, la wave NON e' parallelizzabile (li' scrivono N
/// sub-run in modo invisibile alla dichiarazione, rompendo l'isolamento). E' un
/// INVARIANTE DI SICUREZZA documentato (non config di business, regola G): questi
/// file/dir sono un fatto del toolchain (lockfile, artefatti, VCS, dipendenze), non
/// una scelta configurabile. Il match e' su segmento di path normalizzato (regola
/// M: struttura, non prosa): un elemento con `/` finale e' un PREFISSO di directory,
/// gli altri sono nomi-file esatti confrontati su qualunque segmento del path.
const WRITE_SCOPE_DENYLIST: &[&str] = &[
    // Lockfile di dipendenze (rigenerati da install/build, condivisi).
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "cargo.lock",
    "poetry.lock",
    "composer.lock",
    "gemfile.lock",
    "go.sum",
    // Directory di dipendenze/artefatti/build (scritte implicitamente).
    "node_modules/",
    "target/",
    "dist/",
    "build/",
    "out/",
    ".next/",
    "__pycache__/",
    ".venv/",
    "vendor/",
    "coverage/",
    // Metadati VCS: mai in scope di un sub-run (condiviso, corrompe l'isolamento).
    ".git/",
];

/// Normalizza un path dichiarato in `write_scope` in modo DETERMINISTICO (regola M):
/// trim, separatori `\` -> `/`, rimozione di `./` iniziale e di `/` iniziali/finali
/// ridondanti, minuscolo (il match di disgiunzione/denylist e' case-insensitive:
/// su Windows/macOS il filesystem lo e', e un LLM puo' variare il case). Preserva un
/// eventuale `/` finale significativo? No: il confronto di prefisso lo tratta a
/// livello di SEGMENTO (vedi [`scopes_overlap`]), quindi il trailing `/` non serve.
/// Ritorna `None` se, dopo la normalizzazione, il path e' vuoto (scope non valido).
fn normalize_scope_path(raw: &str) -> Option<String> {
    let cleaned: String = raw.trim().replace('\\', "/");
    let cleaned = cleaned.trim_matches('/').trim();
    let cleaned = cleaned.strip_prefix("./").unwrap_or(cleaned);
    let cleaned = cleaned.trim_matches('/');
    if cleaned.is_empty() {
        return None;
    }
    Some(cleaned.to_ascii_lowercase())
}

/// `true` se il path normalizzato tocca un elemento della [`WRITE_SCOPE_DENYLIST`].
/// Un'entry con `/` finale matcha se e' un SEGMENTO qualsiasi del path (prefisso di
/// directory); un'entry senza `/` matcha un SEGMENTO esatto (nome file) in qualunque
/// posizione. Deterministico, nessun IO.
fn scope_hits_denylist(norm_path: &str) -> bool {
    let segments: Vec<&str> = norm_path.split('/').filter(|s| !s.is_empty()).collect();
    for entry in WRITE_SCOPE_DENYLIST {
        if let Some(dir) = entry.strip_suffix('/') {
            // Directory: matcha se compare come segmento (qualunque livello).
            if segments.contains(&dir) {
                return true;
            }
        } else if segments.contains(entry) {
            // Nome file esatto: matcha come segmento (di solito ultimo).
            return true;
        }
    }
    false
}

/// `true` se due path normalizzati (a livello di SEGMENTO) si sovrappongono: uguali
/// o uno prefisso-di-directory dell'altro (`src/a` sovrappone `src/a/b`, ma NON
/// `src/ab`). Il confronto e' su confine di segmento per evitare falsi overlap tra
/// nomi con prefisso comune. Simmetrico, deterministico, nessun IO.
fn scopes_overlap(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (shorter, longer) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    // `longer` inizia con `shorter` seguito da un confine di segmento `/`.
    longer.starts_with(shorter) && longer.as_bytes().get(shorter.len()) == Some(&b'/')
}

/// PUNTO UNICO (regola L) della verifica STATICA di DISGIUNZIONE degli scope di
/// scrittura dichiarati dai sub-task. Funzione PURA (nessun IO, regola M: decide su
/// segnali strutturati, mai su prosa). Ritorna `true` (parallelizzabile in
/// isolamento) SOLO se TUTTE queste condizioni valgono:
///   (a) ogni task dichiara ALMENO un path in `write_scope` (uno scope vuoto ->
///       non parallelizzabile: non sappiamo cosa scrivera' -> `false`);
///   (b) nessuno scope tocca un path della [`WRITE_SCOPE_DENYLIST`] (lock/generati/
///       VCS scritti implicitamente da build/install: romperebbero l'isolamento);
///   (c) gli scope sono a due a due DISGIUNTI (nessuna sovrapposizione di path o
///       prefisso di directory, vedi [`scopes_overlap`]).
/// Un path che dopo la normalizzazione risulta vuoto invalida lo scope (`false`):
/// una dichiarazione non normalizzabile non e' una dichiarazione valida.
///
/// USATO SIA da [`validate_orch_move`] (decisione) SIA (in un PR successivo) dal
/// coordinatore di dispatch (esecuzione): una sola verifica, nessuna divergenza.
pub fn subtasks_are_disjoint(scopes: &[Vec<String>]) -> bool {
    // Normalizza tutti gli scope di tutti i task. (a) scope vuoto -> non disgiunto.
    let mut per_task: Vec<Vec<String>> = Vec::with_capacity(scopes.len());
    for task_scope in scopes {
        if task_scope.is_empty() {
            return false;
        }
        let mut norm_task: Vec<String> = Vec::with_capacity(task_scope.len());
        for raw in task_scope {
            match normalize_scope_path(raw) {
                // (b) tocca la denylist -> non parallelizzabile.
                Some(p) if scope_hits_denylist(&p) => return false,
                Some(p) => norm_task.push(p),
                // Path non normalizzabile (vuoto dopo trim) -> scope non valido.
                None => return false,
            }
        }
        per_task.push(norm_task);
    }
    // (c) disgiunzione a coppie tra path di task DIVERSI. (Overlap DENTRO lo stesso
    // task e' irrilevante: un task scrive tutto nel proprio scope.)
    for i in 0..per_task.len() {
        for j in (i + 1)..per_task.len() {
            for pa in &per_task[i] {
                for pb in &per_task[j] {
                    if scopes_overlap(pa, pb) {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// PUNTO UNICO (regola L) della domanda "posso far scrivere PIU' sub-run in
/// PARALLELO?". Vero solo se esiste l'isolamento FISICO (worktree git effimeri)
/// E gli scope di scrittura dichiarati sono DISGIUNTI.
///
/// Servono ENTRAMBI i termini, e per ragioni diverse: la disgiunzione e' una
/// promessa DICHIARATIVA del pianificatore, l'isolamento e' la guard FISICA.
/// Senza isolamento i sub-run condividono la root reale del progetto, quindi
/// qualunque svista o incompletezza del piano si traduce in una race sul
/// filesystem. Incidente del 2026-07-22 (progetto non-git, wave da 8): sette
/// sub-run hanno scritto lo stesso file, `server.js` e' stato troncato da 2.3KB a
/// 595B da un edit concorrente e sono nati file duplicati.
///
/// Chi risponde "no" NON deve degradare l'ISOLAMENTO (eseguendo comunque in
/// parallelo sulla root condivisa, che e' il difetto trovato): deve degradare il
/// PARALLELISMO, cioe' procedere un todo per volta.
pub fn parallel_writers_allowed(isolation_available: bool, scopes: &[Vec<String>]) -> bool {
    isolation_available && subtasks_are_disjoint(scopes)
}

/// Valida l'output JSON dell'LLM contro l'enum CHIUSO [`OrchestrationMove`]. Punto
/// unico (regola L): il nodo/gate di orchestrazione e l'impl della porta chiamano
/// SOLO questa funzione. Qualunque forma malformata / con collezione vuota dove
/// serve / mossa non applicabile per una guard deterministica degrada a
/// [`OrchestrationMove::Fallback`] (rete di sicurezza: l'euristica esistente
/// `is_eligible`/`should_parallelize`).
///
/// `isolation_available` e' la guard fisica anti-race (regola verificata sul
/// codice: `dag_scheduler` non ha campi file, i sub-run condividono la root
/// per-sessione -> due paralleli che scrivono si pesterebbero). La delega
/// [`Coordination::ParallelIsolated`] e' ammessa SOLO se `isolation_available ==
/// true` E gli scope di scrittura dichiarati sono DISGIUNTI (punto unico
/// [`subtasks_are_disjoint`]); altrimenti la coordinazione DEGRADA a
/// [`Coordination::Sequential`] (la delega resta valida, cade solo il parallelismo —
/// NON `Fallback`). In Fase 1 `isolation_available` e' sempre `false` -> ogni
/// `ParallelIsolated` degrada a `Sequential` (comportamento invariato: la delega
/// sequenziale era gia' l'unica ammessa).
///
/// `delegation_forbidden` (aggregato deterministico di depth/cost/policy dal
/// [`OrchestrationContext`]) e' passato ESPLICITAMENTE: se `true`, qualunque
/// [`OrchestrationMove::DelegateSubagents`] degrada a `Fallback` (la decisione LLM
/// non puo' scavalcare la guard deterministica).
pub fn validate_orch_move(
    raw: &serde_json::Value,
    isolation_available: bool,
    delegation_forbidden: bool,
) -> OrchestrationMove {
    let mv: OrchestrationMove = match serde_json::from_value(raw.clone()) {
        Ok(m) => m,
        Err(_) => return OrchestrationMove::Fallback,
    };
    match mv {
        // Decompose senza blocchi: mossa vuota, inutile -> euristica.
        OrchestrationMove::Decompose { ref blocks } if blocks.is_empty() => {
            OrchestrationMove::Fallback
        }
        // Delega senza task: mossa vuota -> euristica.
        OrchestrationMove::DelegateSubagents { ref tasks, .. } if tasks.is_empty() => {
            OrchestrationMove::Fallback
        }
        // Delega vietata da guard deterministica (depth/cost/policy): la decisione
        // LLM non scavalca la guard -> euristica.
        OrchestrationMove::DelegateSubagents { .. } if delegation_forbidden => {
            OrchestrationMove::Fallback
        }
        // Delega parallela isolata: ammessa SOLO se l'isolamento fisico e'
        // disponibile E gli scope dichiarati sono disgiunti (punto unico
        // subtasks_are_disjoint). Altrimenti DEGRADA a Sequential (la delega resta,
        // cade solo il parallelismo — non Fallback). In Fase 1 isolation_available
        // e' sempre false -> degrado sistematico a Sequential (invariato).
        OrchestrationMove::DelegateSubagents {
            tasks,
            coordination: Coordination::ParallelIsolated,
        } => {
            let scopes: Vec<Vec<String>> = tasks.iter().map(|t| t.write_scope.clone()).collect();
            // Delega al punto unico (regola L): la stessa domanda la pone anche il
            // TodoRunner quando apre l'ondata dei todo. Prima erano due luoghi, e
            // solo questo — il meno battuto — la poneva davvero.
            let coordination = if parallel_writers_allowed(isolation_available, &scopes) {
                Coordination::ParallelIsolated
            } else {
                Coordination::Sequential
            };
            OrchestrationMove::DelegateSubagents {
                tasks,
                coordination,
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ports::{PlanBlock, SubTask};
    use serde_json::json;

    #[test]
    fn context_pressure_deriva_da_used_su_limit() {
        // Limite/used ignoti -> Low (nessuna pressione presunta).
        assert_eq!(context_pressure_from_tokens(0, 0), ContextPressure::Low);
        assert_eq!(context_pressure_from_tokens(100, 0), ContextPressure::Low);
        assert_eq!(context_pressure_from_tokens(0, 1000), ContextPressure::Low);
        // Sotto la soglia media -> Low.
        assert_eq!(
            context_pressure_from_tokens(500, 1000),
            ContextPressure::Low
        );
        // Tra media e alta -> Medium.
        assert_eq!(
            context_pressure_from_tokens(700, 1000),
            ContextPressure::Medium
        );
        // Oltre la soglia alta -> High.
        assert_eq!(
            context_pressure_from_tokens(900, 1000),
            ContextPressure::High
        );
        // Esattamente sulla soglia alta -> High (>=).
        assert_eq!(
            context_pressure_from_tokens(850, 1000),
            ContextPressure::High
        );
    }

    #[test]
    fn orch_epoch_plan_entry_e_stabile() {
        assert_eq!(orch_epoch(OrchPhase::PlanEntry), 0);
        // Deterministica a chiamate ripetute (idempotenza/replay).
        assert_eq!(
            orch_epoch(OrchPhase::PlanEntry),
            orch_epoch(OrchPhase::PlanEntry)
        );
    }

    #[test]
    fn delegation_forbidden_scatta_su_ogni_guard() {
        // Nessuna guard attiva -> permessa.
        assert!(!delegation_forbidden(0, 3, 0.0, 10.0, false));
        // Policy esplicita -> vietata.
        assert!(delegation_forbidden(0, 3, 0.0, 10.0, true));
        // Depth oltre soglia -> vietata.
        assert!(delegation_forbidden(3, 3, 0.0, 10.0, false));
        assert!(delegation_forbidden(4, 3, 0.0, 10.0, false));
        // Depth sotto soglia -> permessa.
        assert!(!delegation_forbidden(2, 3, 0.0, 10.0, false));
        // Cost oltre cap -> vietata.
        assert!(delegation_forbidden(0, 3, 10.0, 10.0, false));
        assert!(delegation_forbidden(0, 3, 12.5, 10.0, false));
        // Cost sotto cap -> permessa.
        assert!(!delegation_forbidden(0, 3, 9.9, 10.0, false));
        // Cap/limite disattivati (<= 0): guard inattiva.
        assert!(!delegation_forbidden(99, 0, 999.0, 0.0, false));
    }

    #[test]
    fn build_orchestration_context_da_segnali_strutturati() {
        let ctx = build_orchestration_context(
            OrchPhase::PlanEntry,
            Some("aggiungi endpoint REST"),
            "automatic",
            50_000,
            7,
            8,
            false,
            false,
            700,
            1000,
            12,
            1,
            3,
            0.5,
            10.0,
            false,
            false,
        );
        assert_eq!(ctx.phase, OrchPhase::PlanEntry);
        assert_eq!(ctx.user_intent.as_deref(), Some("aggiungi endpoint REST"));
        assert_eq!(ctx.behavior_mode, "automatic");
        assert_eq!(ctx.token_budget, 50_000);
        assert_eq!(ctx.task_complexity, 7);
        assert_eq!(ctx.agentic_score, 8);
        assert!(!ctx.is_ambiguous);
        assert!(!ctx.plan_exists);
        assert_eq!(ctx.context_tokens_used, 700);
        assert_eq!(ctx.context_window_limit, 1000);
        assert_eq!(ctx.history_len, 12);
        // 700/1000 = 0.70 -> Medium (punto unico context_pressure).
        assert_eq!(ctx.context_pressure, ContextPressure::Medium);
        assert_eq!(ctx.subagent_depth, 1);
        assert_eq!(ctx.cost_spent_usd, 0.5);
        assert_eq!(ctx.cost_cap_usd, 10.0);
        // depth 1 < 3, cost 0.5 < 10.0, policy false -> delega permessa.
        assert!(!ctx.delegation_forbidden);
        // Fase 1: isolamento fisico non disponibile (hardwired false al call site).
        assert!(!ctx.isolation_available);
    }

    #[test]
    fn build_orchestration_context_aggrega_guard_delega() {
        // depth 3 >= limite 3 -> delegation_forbidden nel contesto costruito.
        let ctx = build_orchestration_context(
            OrchPhase::PlanEntry,
            None,
            "confirm",
            0,
            0,
            0,
            true,
            true,
            0,
            0,
            0,
            3,
            3,
            0.0,
            0.0,
            false,
            false,
        );
        assert!(ctx.delegation_forbidden);
        assert!(ctx.is_ambiguous);
        assert!(ctx.plan_exists);
        // limite/used contesto ignoti -> Low.
        assert_eq!(ctx.context_pressure, ContextPressure::Low);
    }

    #[test]
    fn validate_orch_move_forme_valide() {
        let m = validate_orch_move(&json!({"move": "run_inline"}), false, false);
        assert_eq!(m, OrchestrationMove::RunInline);

        let m = validate_orch_move(
            &json!({"move": "plan_phase", "decompose": true}),
            false,
            false,
        );
        assert_eq!(m, OrchestrationMove::PlanPhase { decompose: true });

        let m = validate_orch_move(
            &json!({
                "move": "decompose",
                "blocks": [{"title": "setup", "description": "prepara lo scaffold"}]
            }),
            false,
            false,
        );
        assert_eq!(
            m,
            OrchestrationMove::Decompose {
                blocks: vec![PlanBlock {
                    title: "setup".into(),
                    description: "prepara lo scaffold".into()
                }]
            }
        );

        // Delega sequenziale con task e senza guard -> valida.
        let m = validate_orch_move(
            &json!({
                "move": "delegate_subagents",
                "tasks": [{"task_description": "scrivi i test", "kind": "coder"}],
                "coordination": "sequential"
            }),
            false,
            false,
        );
        assert_eq!(
            m,
            OrchestrationMove::DelegateSubagents {
                tasks: vec![SubTask {
                    task_description: "scrivi i test".into(),
                    kind: "coder".into(),
                    write_scope: vec![]
                }],
                coordination: Coordination::Sequential
            }
        );
    }

    #[test]
    fn validate_orch_move_malformato_degrada_a_fallback() {
        // Enum sconosciuto.
        assert_eq!(
            validate_orch_move(&json!({"move": "boh"}), false, false),
            OrchestrationMove::Fallback
        );
        // Nessun tag "move".
        assert_eq!(
            validate_orch_move(&json!({"foo": "bar"}), false, false),
            OrchestrationMove::Fallback
        );
        // Decompose senza blocchi.
        assert_eq!(
            validate_orch_move(&json!({"move": "decompose", "blocks": []}), false, false),
            OrchestrationMove::Fallback
        );
        // Delega senza task.
        assert_eq!(
            validate_orch_move(
                &json!({"move": "delegate_subagents", "tasks": [], "coordination": "sequential"}),
                false,
                false
            ),
            OrchestrationMove::Fallback
        );
    }

    #[test]
    fn validate_orch_move_parallel_isolated_senza_isolamento_degrada_a_sequential() {
        // Scope disgiunti dichiarati, ma isolation_available=false (Fase 1): la
        // delega resta valida, cade solo il parallelismo -> Sequential (NON Fallback).
        let raw = json!({
            "move": "delegate_subagents",
            "tasks": [
                {"task_description": "crate a", "kind": "coder", "write_scope": ["crates/a"]},
                {"task_description": "crate b", "kind": "coder", "write_scope": ["crates/b"]}
            ],
            "coordination": "parallel_isolated"
        });
        // isolation_available = false -> degrado a Sequential.
        match validate_orch_move(&raw, false, false) {
            OrchestrationMove::DelegateSubagents {
                coordination: Coordination::Sequential,
                tasks,
            } => assert_eq!(tasks.len(), 2),
            other => panic!("atteso DelegateSubagents/Sequential, ottenuto {other:?}"),
        }
    }

    #[test]
    fn validate_orch_move_parallel_isolated_ammessa_con_isolamento_e_scope_disgiunti() {
        let raw = json!({
            "move": "delegate_subagents",
            "tasks": [
                {"task_description": "crate a", "kind": "coder", "write_scope": ["crates/a"]},
                {"task_description": "crate b", "kind": "coder", "write_scope": ["crates/b"]}
            ],
            "coordination": "parallel_isolated"
        });
        // isolation_available=true E scope disgiunti -> ParallelIsolated ammessa.
        assert_eq!(
            validate_orch_move(&raw, true, false),
            OrchestrationMove::DelegateSubagents {
                tasks: vec![
                    SubTask {
                        task_description: "crate a".into(),
                        kind: "coder".into(),
                        write_scope: vec!["crates/a".into()]
                    },
                    SubTask {
                        task_description: "crate b".into(),
                        kind: "coder".into(),
                        write_scope: vec!["crates/b".into()]
                    }
                ],
                coordination: Coordination::ParallelIsolated
            }
        );
    }

    #[test]
    fn validate_orch_move_parallel_isolated_scope_sovrapposti_degrada_a_sequential() {
        // isolation_available=true ma scope sovrapposti (src/a vs src/a/b) -> Sequential.
        let raw = json!({
            "move": "delegate_subagents",
            "tasks": [
                {"task_description": "t1", "kind": "coder", "write_scope": ["src/a"]},
                {"task_description": "t2", "kind": "coder", "write_scope": ["src/a/b"]}
            ],
            "coordination": "parallel_isolated"
        });
        assert!(matches!(
            validate_orch_move(&raw, true, false),
            OrchestrationMove::DelegateSubagents {
                coordination: Coordination::Sequential,
                ..
            }
        ));
    }

    #[test]
    fn validate_orch_move_parallel_isolated_scope_vuoto_degrada_a_sequential() {
        // isolation_available=true ma un task senza write_scope -> Sequential.
        let raw = json!({
            "move": "delegate_subagents",
            "tasks": [
                {"task_description": "t1", "kind": "coder", "write_scope": ["src/a"]},
                {"task_description": "t2", "kind": "coder"}
            ],
            "coordination": "parallel_isolated"
        });
        assert!(matches!(
            validate_orch_move(&raw, true, false),
            OrchestrationMove::DelegateSubagents {
                coordination: Coordination::Sequential,
                ..
            }
        ));
    }

    #[test]
    fn validate_orch_move_parallel_isolated_scope_lockfile_degrada_a_sequential() {
        // isolation_available=true ma uno scope tocca un lockfile (denylist) -> Sequential.
        let raw = json!({
            "move": "delegate_subagents",
            "tasks": [
                {"task_description": "t1", "kind": "coder", "write_scope": ["Cargo.lock"]},
                {"task_description": "t2", "kind": "coder", "write_scope": ["crates/b"]}
            ],
            "coordination": "parallel_isolated"
        });
        assert!(matches!(
            validate_orch_move(&raw, true, false),
            OrchestrationMove::DelegateSubagents {
                coordination: Coordination::Sequential,
                ..
            }
        ));
    }

    #[test]
    fn validate_orch_move_delega_vietata_da_guard_deterministica() {
        let raw = json!({
            "move": "delegate_subagents",
            "tasks": [{"task_description": "x", "kind": "coder"}],
            "coordination": "sequential"
        });
        // delegation_forbidden = true (depth/cost/policy a monte) -> Fallback:
        // la decisione LLM non scavalca la guard deterministica (regola M).
        assert_eq!(
            validate_orch_move(&raw, false, true),
            OrchestrationMove::Fallback
        );
        // Senza guard -> la stessa mossa e' valida.
        assert!(matches!(
            validate_orch_move(&raw, false, false),
            OrchestrationMove::DelegateSubagents { .. }
        ));
    }

    // ── subtasks_are_disjoint (funzione pura, punto unico regola L) ──────────

    fn sc(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn subtasks_are_disjoint_scope_disgiunti_true() {
        assert!(subtasks_are_disjoint(&[
            sc(&["crates/a"]),
            sc(&["crates/b"])
        ]));
        // Multi-path per task, tutti disgiunti tra task.
        assert!(subtasks_are_disjoint(&[
            sc(&["src/api", "docs/api.md"]),
            sc(&["src/db", "docs/db.md"]),
        ]));
        // Un solo task: banalmente disgiunto (nessuna coppia).
        assert!(subtasks_are_disjoint(&[sc(&["src/only"])]));
        // Nessun task: vacuamente disgiunto (nessuna coppia, nessuno scope vuoto).
        assert!(subtasks_are_disjoint(&[]));
    }

    #[test]
    fn subtasks_are_disjoint_scope_sovrapposti_false() {
        // Path identico tra due task.
        assert!(!subtasks_are_disjoint(&[sc(&["src/a"]), sc(&["src/a"])]));
        // Overlap su un path pur avendone altri disgiunti.
        assert!(!subtasks_are_disjoint(&[
            sc(&["src/a", "src/shared"]),
            sc(&["src/b", "src/shared"]),
        ]));
    }

    #[test]
    fn parallel_writers_richiede_isolamento_e_disgiunzione() {
        let disgiunti = [sc(&["src/a"]), sc(&["src/b"])];
        let sovrapposti = [sc(&["src/a"]), sc(&["src/a"])];
        // Unico caso ammesso: guard fisica presente E promessa del piano coerente.
        assert!(parallel_writers_allowed(true, &disgiunti));
        // ISOLAMENTO ASSENTE (ogni progetto non-git): anche con scope
        // PERFETTAMENTE disgiunti il fronte parallelo non e' ammesso. La
        // disgiunzione e' una promessa dichiarativa del pianificatore, non una
        // guard fisica: se il piano sbaglia, i sub-run si pestano sulla root reale.
        assert!(!parallel_writers_allowed(false, &disgiunti));
        // Isolamento presente ma aree dichiarate sovrapposte.
        assert!(!parallel_writers_allowed(true, &sovrapposti));
        assert!(!parallel_writers_allowed(false, &sovrapposti));
        // Scope non dichiarato -> nessun parallelismo (coerente con subtasks_are_disjoint).
        assert!(!parallel_writers_allowed(true, &[sc(&[]), sc(&["src/b"])]));
    }

    #[test]
    fn subtasks_are_disjoint_prefisso_sovrapposto_false() {
        // src/a e' prefisso-di-directory di src/a/b -> sovrapposti.
        assert!(!subtasks_are_disjoint(&[sc(&["src/a"]), sc(&["src/a/b"])]));
        // Ordine inverso: stessa decisione (simmetrico).
        assert!(!subtasks_are_disjoint(&[sc(&["src/a/b"]), sc(&["src/a"])]));
        // Prefisso NON di directory (src/a vs src/ab) -> NON sovrapposti.
        assert!(subtasks_are_disjoint(&[sc(&["src/a"]), sc(&["src/ab"])]));
    }

    #[test]
    fn subtasks_are_disjoint_scope_vuoto_false() {
        // Un task con write_scope vuoto -> non parallelizzabile.
        assert!(!subtasks_are_disjoint(&[sc(&["src/a"]), sc(&[])]));
        // Tutti vuoti -> false.
        assert!(!subtasks_are_disjoint(&[sc(&[]), sc(&[])]));
        // Path che si normalizza a vuoto (solo separatori) -> scope non valido.
        assert!(!subtasks_are_disjoint(&[sc(&["src/a"]), sc(&["  ///  "])]));
    }

    #[test]
    fn subtasks_are_disjoint_lockfile_e_generati_false() {
        // Lockfile: rigenerato implicitamente, condiviso.
        assert!(!subtasks_are_disjoint(&[
            sc(&["Cargo.lock"]),
            sc(&["crates/b"]),
        ]));
        assert!(!subtasks_are_disjoint(&[
            sc(&["package-lock.json"]),
            sc(&["src/b"]),
        ]));
        // Directory generata come segmento.
        assert!(!subtasks_are_disjoint(&[
            sc(&["node_modules/foo"]),
            sc(&["src/b"]),
        ]));
        assert!(!subtasks_are_disjoint(&[
            sc(&["target/debug"]),
            sc(&["src/b"])
        ]));
        // .git non deve mai stare in scope.
        assert!(!subtasks_are_disjoint(&[
            sc(&[".git/config"]),
            sc(&["src/b"])
        ]));
    }

    #[test]
    fn subtasks_are_disjoint_normalizza_separatori_e_case() {
        // Separatori Windows, ./ iniziale, / finale, case misto: normalizzati.
        // "./Src/A/" e "src\\a" collidono dopo normalizzazione -> non disgiunti.
        assert!(!subtasks_are_disjoint(&[
            sc(&["./Src/A/"]),
            sc(&["src\\a"])
        ]));
        // Stessi separatori misti ma path diversi -> disgiunti.
        assert!(subtasks_are_disjoint(&[sc(&["src\\a"]), sc(&["src\\b"])]));
    }
}

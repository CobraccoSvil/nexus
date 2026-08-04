//! Adapter del trait [`nexus_agent_graph::runtime::ports::CriteriaRunner`].
//!
//! Implementa `CriteriaRunner::run` eseguendo i criteri generali del final gate
//! (parita' con `brain/agents/criteria_runner.py`). I criteri che orchestrano
//! tool delegano al PUNTO UNICO dell'esecuzione tool ([`ToolExecutor`], regola L):
//! NON ricostruiscono il dispatch.
//!
//! COPERTURA DEI CRITERI (segnalata esplicitamente):
//!
//! | criterio              | strategia | note |
//! |-----------------------|-----------|------|
//! | `run_command` (build) | NATIVO    | tool `run_command` -> confronto `exit_code` vs `expected.exit_code`; evidence con `output_excerpt`/`output_total_chars`/`output_truncated` (chiavi lette da `final_gate::render_failed_block`, ramo build) |
//! | `service_logs_clean`  | NATIVO    | tool `run_command` -> match dei `patterns` sulle righe di log; inconclusive su comando fallito |
//! | `http`                | NATIVO    | chiamata REALE via `reqwest` (parita' httpx Python); status singolo o lista + `body_contains` opzionale |
//! | `file_exists`         | NATIVO    | interrogazione DIRETTA del filesystem sulla radice del run (vedi [`EsistenzaFile`]) |
//! | `outputs_exist`       | NATIVO    | lettura `agent_steps` (tool mutativi file del run) + verifica esistenza via `file_exists` |
//! | `no_orphan_imported`  | TODO (F3) | grafo degli import BFS (~150 righe Python non portabili rapidamente): ritorna `Inconclusive` finche' non portato — un criterio non valutabile NON deve far fallire il gate, ne' fargli dichiarare una verifica che non c'e' stata |
//!
//! RISCHIO AL CUTOVER (lezione "final_gate deve verificare il build"): il criterio
//! BUILD (`run_command` con `expected.exit_code=0`) e' NATIVO e funzionante: al
//! cutover il final_gate Rust verifica davvero che il codice COMPILI. L'unico
//! criterio non coperto e' `no_orphan_imported` (anti-placeholder Figma/v0): resta
//! inconclusive finche' F3 non porta il grafo import — il gate non lo conteggia,
//! quindi non degrada la qualita' degli altri criteri, ma l'anti-placeholder e'
//! temporaneamente cieco (TODO F3 tracciato).
//!
//! PARITA' ERRORE (parita' col try/except `final_gate.py:381-385`): un criterio
//! fallito NON propaga un errore di porta — diventa un [`CriterionResult`] con
//! [`CriterionOutcome::Failed`] + `evidence.error`/`evidence.verdict`. Il
//! [`PortError`] resta per un guasto infrastrutturale del runner stesso (es.
//! lettura `agent_steps` fallita in `outputs_exist`).
//!
//! MISURA vs NON MISURA: una prova che non si e' potuta ESEGUIRE (grafo import
//! non portato, radice del run non risolta, log illeggibili, comando senza exit
//! code) ritorna [`CriterionOutcome::Inconclusive`], mai un `passed`. Prima
//! erano tutti `true`: il verifier li escludeva dal conteggio leggendo un flag
//! nell'evidence, il final_gate no — e chiudeva "verifica superata" un run in
//! cui nessuno aveva misurato niente. Distinto da NON APPLICABILE (`skipped`:
//! nessun path, nessun pattern, nessuno step mutativo), che resta un pass
//! perche' non c'e' nulla da misurare.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{
    CriteriaRunner, CriterionOutcome, CriterionResult, CriterionSpec, PortError, ToolCall,
    ToolExecutor, ToolOutcome,
};

/// Timeout di default per i criteri (parita' col Python: `30.0s`). I criteri con
/// `timeout_s` valorizzato (solo il build) lo sovrascrivono.
const DEFAULT_TIMEOUT_S: f64 = 30.0;

/// Default storico dell'excerpt di output dei criteri (`final_gate.py`: 600).
const DEFAULT_MAX_OUTPUT_CHARS: usize = 600;

/// Tool mutativi file -> chiave dell'input col path di OUTPUT (parita'
/// `criteria_runner._OUTPUT_PATH_KEYS`). Usato da `outputs_exist`.
const OUTPUT_PATH_KEYS: &[(&str, &str)] = &[
    ("write_file", "path"),
    ("edit_file", "path"),
    ("create_file", "path"),
    ("apply_patch", "path"),
    ("rename_file", "to"),
    ("fs_move", "to"),
];

/// Adapter [`CriteriaRunner`] -> criteri NATIVI su [`ToolExecutor`] + HTTP/DB.
pub struct FinalGateCriteriaRunnerAdapter {
    /// Esecutore tool (PUNTO UNICO, regola L): i criteri che usano `run_command`/
    /// `list_files` delegano qui invece di ricostruire il dispatch.
    tool_executor: Arc<dyn ToolExecutor>,
    /// Pool Postgres: lettura `agent_steps` per il criterio `outputs_exist`.
    db: PgPool,
    /// Client HTTP per il criterio `http` (chiamata reale all'endpoint).
    http_client: reqwest::Client,
    /// Radice dell'albero su cui questo run LAVORA: la project_root, oppure il
    /// worktree effimero di un sub-run isolato. E' la stessa radice che il ctx
    /// dei tool risolve (`tool_runner_server::resolve_ctx_root`), e va risolta
    /// con quel punto unico: un criterio che guardasse la project_root mentre il
    /// run scrive nel worktree misurerebbe un albero diverso da quello prodotto,
    /// dichiarando l'altro (regola O).
    ///
    /// `None` quando la sessione non e' mappata a un progetto: in quel caso
    /// `file_exists` non puo' guardare, e lo DICHIARA
    /// ([`EsistenzaFile::NonInterrogabile`]) invece di rispondere "non esiste".
    run_root: Option<PathBuf>,
    /// Pool META + progetto della sessione: servono al PUNTO UNICO della
    /// verifica a suite ([`crate::suite_verification`]), a cui il criterio
    /// `run_command` delega quando il comando dello step e' una suite
    /// Playwright. `None` = sessione senza progetto: il criterio esegue come un
    /// comando qualunque (nessuna memoria, nessuna classificazione), che e' il
    /// comportamento storico.
    progetto: Option<(PgPool, Uuid)>,
}

impl FinalGateCriteriaRunnerAdapter {
    /// Costruisce il runner sull'esecutore tool condiviso + pool Postgres.
    ///
    /// `run_root`: radice dell'albero del run (vedi il campo omonimo). Argomento
    /// esplicito e non opzionale nella firma di proposito: un criterio che
    /// interroga il filesystem senza sapere DOVE guardare non e' un criterio, e
    /// dimenticarsene deve essere un errore di compilazione, non un `file_exists`
    /// che tace.
    pub fn new(tool_executor: Arc<dyn ToolExecutor>, db: PgPool, run_root: Option<PathBuf>) -> Self {
        Self {
            tool_executor,
            db,
            http_client: reqwest::Client::new(),
            run_root,
            progetto: None,
        }
    }

    /// Aggancia il progetto della sessione: e' cio' che permette al criterio
    /// `run_command` di DELEGARE una suite di test al punto unico della
    /// verifica invece di eseguirla per conto proprio. Senza, il gate resta il
    /// terzo esecutore cieco agli altri due.
    pub fn con_progetto(mut self, meta_db: PgPool, project_id: Uuid) -> Self {
        self.progetto = Some((meta_db, project_id));
        self
    }

    /// Se il comando dello step e' una suite di test, la verifica passa dal
    /// punto unico: memoria degli esiti per stato del codice e classificazione
    /// del rosso non riprodotto.
    ///
    /// `None` = non e' una suite, o manca cio' che serve per delegare
    /// (progetto, radice del run): il criterio esegue come prima. `Some(Err)` =
    /// delegata e NON riuscita: il criterio fallisce dichiarandolo, senza
    /// rilanciare la suite per conto proprio — un ripiego che rieseguisse
    /// rimetterebbe in piedi il terzo esecutore, e dopo un timeout
    /// raddoppierebbe anche l'attesa.
    async fn verifica_suite_delegata(
        &self,
        cmd: &str,
        working_dir: Option<&str>,
        timeout_s: f64,
    ) -> Option<Result<crate::suite_verification::SuiteVerification, String>> {
        if !crate::suite_verification::e_suite_playwright(cmd) {
            return None;
        }
        let (meta_db, project_id) = self.progetto.clone()?;
        let run_root = self.run_root.clone()?;

        let env = crate::agent_tools::testing::SuiteEnv {
            meta_db: meta_db.clone(),
            project_id,
            root: run_root.clone(),
            // Il gate non ha un consumatore SSE agganciato: la riga `jobs` si
            // scrive comunque, ed e' quella a rendere l'esecuzione del gate
            // visibile nel pannello (prima non lo era affatto).
            playwright_channels: None,
            project_channels: None,
        };
        let deps = crate::agent_tools::testing::suite_deps(env, run_root).await;
        let inv = crate::suite_verification::SuiteInvocation::suite(
            cmd,
            working_dir.map(str::to_string),
            // I criteri comando del gate portano il timeout dei BUILD (180s di
            // default): per una suite E2E e' un'interruzione, non una verifica.
            // Si prende il piu' largo fra i due (punto unico del tetto).
            (timeout_s.max(1.0) as u64).max(crate::suite_verification::TIMEOUT_SUITE_DEFAULT_S),
        );
        Some(deps.verifica(&inv).await)
    }

    /// Traduce l'esito della verifica a suite nel verdetto del criterio.
    ///
    /// `flaky` PASSA: e' la decisione centrale del presidio — un rosso non
    /// riprodotto a codice invariato non e' un difetto dell'app, e bocciare il
    /// gate per quello e' cio' che mandava il correttore a modificare codice
    /// sano. Passa DICHIARANDOLO: l'evidence porta l'esito canonico e i nomi
    /// dei test instabili, cosi' il debito resta visibile invece di sparire in
    /// un verde.
    fn evidenza_suite(
        cmd: &str,
        esito: Result<crate::suite_verification::SuiteVerification, String>,
        baseline_exit: Option<i64>,
    ) -> (CriterionOutcome, Value) {
        let v = match esito {
            Ok(v) => v,
            // GUASTO DELL'ESECUZIONE, non misura del codice: radice non
            // risolvibile, DB del progetto non disponibile, errore di avvio del
            // processo, timeout. Nessuno di questi dice nulla sul codice sotto
            // esame, e prima uscivano `false` — cioe' `Failed`, cioe' il gate
            // bocciava codice che nessuno aveva provato. `Inconclusive` esiste
            // apposta ed e' gia' usato dal ramo gemello `check_run_command`
            // venticinque righe piu' sotto: qui mancava solo perche' la firma
            // `-> (bool, Value)` non aveva un posto dove metterlo.
            Err(e) => {
                return (
                    CriterionOutcome::Inconclusive,
                    json!({
                        "command": cmd,
                        "error": format!("verifica a suite non riuscita: {e}"),
                        "verdict": format!(
                            "La suite non ha prodotto un esito: {e}. E' un guasto dell'esecuzione,                              non una misura del codice: il criterio non conta nel gate e la                              chiusura resta NON verificata."
                        ),
                    }),
                )
            }
        };
        // Delta-aware sui criteri (invariato dal ramo generico): una suite che
        // falliva GIA' con lo stesso exit code sull'albero pre-lavoro e' debito
        // del progetto, non una regressione di questo run. Senza questo ramo la
        // delega avrebbe introdotto proprio la bocciatura che il gate
        // delta-aware esiste per evitare.
        let preesistente = v.outcome.blocca_la_chiusura()
            && baseline_exit.is_some_and(|b| b != 0 && v.exit_code.map(i64::from) == Some(b));
        let passed = !v.outcome.blocca_la_chiusura() || preesistente;
        // L'output di una suite e' lungo: il gate ne espone quanto ne espone di
        // un build fallito (`build_output_max_chars`), non i 600 caratteri del
        // criterio generico, altrimenti l'agente vedrebbe la sola coda.
        const MAX_OUTPUT_SUITE: usize = 4000;
        let excerpt = truncate_chars(&v.testo, MAX_OUTPUT_SUITE);
        (
            // La suite ha prodotto un esito: qui si e' MISURATO, e il verdetto
            // segue il passed (che il ramo delta-aware puo' aver gia' corretto).
            CriterionOutcome::measured(passed),
            json!({
                "command": cmd,
                "exit_code": v.exit_code,
                "suite_outcome": v.outcome.as_str(),
                "suite_origine": match &v.origine {
                    crate::suite_verification::OrigineEsito::Eseguita => "eseguita",
                    crate::suite_verification::OrigineEsito::Memoria { .. } => "memoria",
                },
                "flaky_tests": v.test_instabili,
                "passed_tests": v.stats.passed,
                "failed_tests": v.stats.failed,
                "output_excerpt": excerpt,
                "output_total_chars": v.testo.chars().count(),
                "output_truncated": v.testo.chars().count() > MAX_OUTPUT_SUITE,
                "baseline_exit_code": baseline_exit,
                "preexisting_bootstrap": preesistente,
                "verdict": verdetto_suite(&v, preesistente),
            }),
        )
    }

    /// Esegue un tool via il PUNTO UNICO [`ToolExecutor`] e ritorna il suo esito
    /// INTERO. Errore di porta -> propagato al chiamante (che lo mappa su
    /// evidence o lo gestisce).
    ///
    /// Ritorna il [`ToolOutcome`] e non il solo testo perche' l'esito del tool
    /// (`exit_code`, `is_error`) e' gia' strutturato quando arriva qui: buttarlo
    /// via al confine costringeva i chiamanti a ri-derivarlo dalla stringa
    /// appiattita — `extract_exit_code` su un testo che l'`exit_code` ce l'aveva
    /// gia' accanto, e la lettura del fallimento di `list_files` da quattro
    /// parole cercate nei suoi primi 80 caratteri (regola M).
    async fn run_tool(&self, name: &str, input: Value) -> Result<ToolOutcome, PortError> {
        let call = ToolCall {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            input,
            thought_signature: None,
        };
        self.tool_executor.execute(call).await
    }

    /// Misura l'exit code di un comando sull'albero CORRENTE (baseline
    /// pre-lavoro degli step gate, delta-aware sui criteri). Legge l'exit code
    /// dal campo STRUTTURATO dell'esito (regola M: mai ri-parsato dal testo).
    /// `None` = comando non eseguibile o esito senza exit code -> il chiamante
    /// NON persiste baseline (fail-closed nel gate). Chiamata SOLO dal run
    /// primario, prima che l'executor tocchi file.
    pub async fn measure_command_exit(
        &self,
        command: &str,
        working_dir: Option<&str>,
    ) -> Option<i64> {
        let mut tool_input = json!({ "command": command });
        if let Some(wd) = working_dir {
            tool_input["working_dir"] = json!(wd);
        }
        self.run_tool("run_command", tool_input).await.ok()?.exit_code
    }

    /// Esegue UN criterio. Parita' col `run_criterion` Python: il dispatch per
    /// `criterion_type` + il try/except che mappa un fallimento su
    /// `passed=false`/`evidence.error` (mai un panico). Il [`PortError`] risale solo
    /// per un guasto infrastrutturale del runner (es. DB).
    async fn run_one(
        &self,
        c: &CriterionSpec,
    ) -> Result<CriterionResult, PortError> {
        let timeout_s = c.timeout_s.unwrap_or(DEFAULT_TIMEOUT_S);
        let (outcome, mut evidence) = match c.criterion_type.as_str() {
            "run_command" => {
                self.check_run_command(&c.spec, &c.expected, timeout_s)
                    .await
            }
            "design_verify" => misurato(Self::check_design_verify(&c.spec)),
            // Criteri STRUTTURALI (ADR 0018 leva 3): PURI, i fatti sono gia'
            // nella spec (estratti dallo stato in FinalGateNode::build_criteria).
            "action_requested" => misurato(Self::check_action_requested(&c.spec)),
            "tool_capability" => misurato(Self::check_tool_capability(&c.spec)),
            "completion_confirmed" => misurato(Self::check_completion_confirmed(&c.spec)),
            "service_logs_clean" => {
                self.check_service_logs_clean(&c.spec, timeout_s)
                    .await
            }
            "http" => misurato(self.check_http(&c.spec, &c.expected, timeout_s).await),
            "file_exists" => {
                self.check_file_exists(&c.spec, &c.expected, timeout_s)
                    .await
            }
            "outputs_exist" => self.check_outputs_exist(&c.spec, timeout_s).await?,
            // Anti-placeholder grafo import: non ancora portato (F3). NON
            // MISURATO: prima diceva "passato", cioe' assolveva il codice per
            // una lacuna del verificatore.
            "no_orphan_imported" | "imported_code_mounted" => (
                CriterionOutcome::Inconclusive,
                json!({
                    "skipped_reason": "no_orphan_imported non ancora portato in Rust (grafo import BFS): criterio inconcludente, escluso dal gate (TODO F3)",
                }),
            ),
            other => (
                CriterionOutcome::Failed,
                json!({ "error": format!("tipo di criterion sconosciuto: '{other}'") }),
            ),
        };
        // Eco del tipo nell'evidence (parita' `run_criterion`: `ev["type"]`) +
        // eco dell'ESITO. Sono proiezioni per persistenza/render: la DECISIONE
        // resta il tipo `CriterionOutcome`, e nessun consumatore la ri-deduce da
        // qui (regola M). Un solo punto di scrittura, derivato dall'enum.
        if let Value::Object(map) = &mut evidence {
            map.insert("type".to_string(), json!(c.criterion_type));
            map.insert("outcome".to_string(), json!(outcome.as_str()));
        }
        Ok(CriterionResult {
            criterion_type: c.criterion_type.clone(),
            outcome,
            evidence,
        })
    }

    // ── run_command (BUILD): exit_code vs expected ───────────────────────────

    // design_verify (P5): resa visiva conforme al figma. PURO - lo similarity_score
    // e' gia' nella spec (estratto dalla history da build_criteria), niente vision qui.
    fn check_design_verify(spec: &Value) -> (bool, Value) {
        let score = spec
            .get("similarity_score")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let min = spec.get("min_score").and_then(Value::as_i64).unwrap_or(0);
        let passed = score >= min;
        let verdict = if passed {
            "resa visiva conforme al design".to_string()
        } else {
            format!(
                "resa visiva sotto soglia ({score}/{min}): continua ad allineare il layout \
al figma ed esegui di nuovo nexus_visual_compare finche similarity_score >= {min}"
            )
        };
        (
            passed,
            json!({ "similarity_score": score, "min_score": min, "verdict": verdict }),
        )
    }

    // ── Criteri STRUTTURALI (ADR 0018 leva 3): puri, fatti nella spec ────────

    /// action_requested: una richiesta d'AZIONE chiusa senza alcuna azione
    /// produttiva in history non passa. Non-action-oriented -> passa sempre.
    fn check_action_requested(spec: &Value) -> (bool, Value) {
        let action_oriented = spec
            .get("action_oriented")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let acted = spec
            .get("has_productive_action")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let passed = !action_oriented || acted;
        let verdict = if passed {
            "nessuna azione richiesta o azione produttiva presente".to_string()
        } else {
            "richiesta d'azione chiusa SENZA alcuna azione produttiva: esegui \
il lavoro con i tool prima di chiudere"
                .to_string()
        };
        (
            passed,
            json!({ "action_oriented": action_oriented, "has_productive_action": acted, "verdict": verdict }),
        )
    }

    /// tool_capability: un task software con ZERO tool esposti e senza tool call
    /// gia' osservate e' una misconfigurazione del catalogo/whitelist. La history
    /// con tool_use e' un segnale strutturato che il catalogo era disponibile
    /// durante il lavoro, anche se `tools_json` non e' arrivato al gate/resume.
    fn check_tool_capability(spec: &Value) -> (bool, Value) {
        let tools_count = spec.get("tools_count").and_then(Value::as_i64).unwrap_or(0);
        let has_tool_calls = spec
            .get("has_tool_calls")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let passed = tools_count > 0 || has_tool_calls;
        let verdict = if tools_count > 0 {
            format!("{tools_count} tool esposti al modello")
        } else if has_tool_calls {
            "catalogo tool non presente nello stato finale, ma la history contiene \
tool_use gia' osservati: capacita' tool confermata dal segnale strutturato"
                .to_string()
        } else {
            "ZERO tool esposti su un task software: misconfigurazione modello/\
whitelist (verificare supports_tool_use e le whitelist agent.tools.*)"
                .to_string()
        };
        (
            passed,
            json!({ "tools_count": tools_count, "has_tool_calls": has_tool_calls, "verdict": verdict }),
        )
    }

    /// completion_confirmed: la chiusura va CONFERMATA da una dichiarazione
    /// strutturata task_complete (ADR 0034). Qualunque outcome onesto passa.
    ///
    /// Delega post-subagente: quando il PADRE coordinatore delega il lavoro a un
    /// sub-agente che arriva a chiusura (`subagent_completed`, segnale MACCHINA
    /// dalla history, regola M) e chiude senza ri-dichiarare, la dichiarazione
    /// onesta del run ESISTE gia' (quella del figlio) -> passa. Il criterio ne
    /// cerca UNA, non che sia del padre; la verifica tecnica (build/typecheck)
    /// resta a guardia della correttezza per un figlio che ha lasciato incompleto.
    fn check_completion_confirmed(spec: &Value) -> (bool, Value) {
        let declared = spec
            .get("declared_outcome")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty());
        let subagent_completed = spec
            .get("subagent_completed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let passed = declared.is_some() || subagent_completed;
        let verdict = match (declared, subagent_completed) {
            (Some(o), _) => format!("esito dichiarato: {o}"),
            (None, true) => "chiusura strutturata delegata a un sub-agente \
completato (task_complete del figlio)"
                .to_string(),
            (None, false) => "nessuna dichiarazione strutturata: chiudi con il tool \
task_complete (outcome + summary)"
                .to_string(),
        };
        (
            passed,
            json!({
                "declared_outcome": declared,
                "subagent_completed": subagent_completed,
                "verdict": verdict,
            }),
        )
    }

    async fn check_run_command(
        &self,
        spec: &Value,
        expected: &Value,
        _timeout_s: f64,
    ) -> (CriterionOutcome, Value) {
        let cmd = spec.get("command").and_then(Value::as_str).unwrap_or("");
        if cmd.is_empty() {
            return (
                CriterionOutcome::Failed,
                json!({ "error": "spec.command obbligatorio" }),
            );
        }
        let working_dir = spec.get("working_dir").and_then(Value::as_str);

        // Una suite di test NON e' un comando qualunque: il suo esito vale per
        // lo stato del codice su cui e' girata, e un rosso non riprodotto non e'
        // un difetto. Delega al punto unico (regola L) — il gate era il terzo
        // esecutore, e non riconosceva l'esito degli altri due.
        if let Some(esito) = self
            .verifica_suite_delegata(cmd, working_dir, _timeout_s)
            .await
        {
            let baseline = spec.get("baseline_exit_code").and_then(Value::as_i64);
            return Self::evidenza_suite(cmd, esito, baseline);
        }

        let mut tool_input = json!({ "command": cmd });
        if let Some(wd) = working_dir {
            tool_input["working_dir"] = json!(wd);
        }
        // Delega al ToolExecutor. Un guasto di porta -> evidence.error (parita'
        // col try/except Python), non propaga.
        let outcome = match self.run_tool("run_command", tool_input).await {
            Ok(o) => o,
            // Il tool non e' partito: e' un guasto dell'ESECUZIONE, non una
            // misura del codice. Bocciare qui significherebbe rimandare in
            // correzione un lavoro che nessuno ha mai provato.
            Err(e) => {
                return (
                    CriterionOutcome::Inconclusive,
                    json!({ "error": format!("execute_tool: {e}"), "command": cmd }),
                )
            }
        };
        // exit_code STRUTTURATO dell'esito: arriva gia' cosi' dal confine del
        // dispatch, non si ri-estrae dal testo che lo trasportava (regola M).
        let actual_exit = outcome.exit_code;
        let raw = outcome_text(&outcome);
        let expected_exit = expected
            .get("exit_code")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        // `Option::==` faceva di un exit code ASSENTE lo stesso `false` di uno
        // SBAGLIATO: il criterio bocciava il codice quando il comando non aveva
        // prodotto alcuno stato d'uscita. Le due domande restano separate —
        // "e' stato misurato?" e "il valore misurato e' quello atteso?".
        //
        // Un'invocazione RIFIUTATA (`is_error` senza exit code: comando
        // malformato, working_dir duplicato, guardia che nega l'esecuzione) non
        // e' un criterio non misurabile — e' un criterio la cui INVOCAZIONE e'
        // fallita, e la risposta e' rieseguirlo corretto, non assolverlo. La
        // distinzione e' necessaria perche' il caso dominante di "exit code
        // assente" e' proprio quello: misurato il 01/08/2026 sui criteri falliti
        // di gestione-spese, 309 su 329 run_command senza exit code e senza
        // errori nell'output, di cui 237 rifiuti per working_dir duplicato e 26
        // che riportano in prosa un servizio NON avviato, cioe' difetti reali.
        // Trattarli come non misurabili renderebbe il gate muto proprio dove
        // oggi insegna all'agente come correggere.
        let esito_misurato = actual_exit.is_some() || outcome.is_error;
        let exit_ok = actual_exit == Some(expected_exit);

        // RETE DI SICUREZZA (regola H): alcuni build ESCONO 0 anche quando il
        // bundling FALLISCE (es. `vite build` con "Could not resolve" / "error
        // during build" che in certe config esce 0). Affidarsi al solo exit_code
        // -> falso verde -> il final_gate chiude "completed" un'app rotta
        // (incidente Beauty-Book). Se l'output contiene pattern di errore di
        // build (punto unico count_build_errors, regola L) il criterio FALLISCE
        // comunque, anche con exit 0.
        let build_errors = nexus_agent_graph::count_build_errors(&raw);

        // ── Gate DELTA-aware (regola H) ──────────────────────────────────────
        // Un errore di build conta come REGRESSIONE (che fallisce il gate) solo se
        // colpisce un file che QUESTO run ha TOCCATO. Gli errori in file non
        // toccati sono debito preesistente del progetto: non devono impedire la
        // chiusura di un task che non li ha introdotti (es. un fix di login
        // bocciato da errori di tipo preesistenti in BookingPage.tsx). Segnali
        // STRUTTURATI (regola M): exit_code, set dei file-con-errori (localizzazione
        // tsc/rustc), lista dei file toccati (dai tool_use mutator, non dalla
        // prosa). Fail-CLOSED di default: senza localizzazione degli errori
        // (formato non coperto -> set vuoto) o senza file toccati dichiarati si
        // ricade sul criterio ASSOLUTO (exit 0 + zero errori), identico a prima.
        let touched: Vec<&str> = spec
            .get("touched_files")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let error_files = nexus_agent_graph::build_error_files(&raw);
        let regressed_files: Vec<String> = error_files
            .iter()
            .filter(|ef| {
                touched
                    .iter()
                    .any(|tf| nexus_agent_graph::error_file_matches_touched(ef, tf))
            })
            .cloned()
            .collect();
        let delta_applicable = !error_files.is_empty() && !touched.is_empty();
        let regression = !regressed_files.is_empty();
        let preexisting_files = error_files.len().saturating_sub(regressed_files.len());
        // ── Delta-aware sui CRITERI (baseline pre-lavoro, regola H/M) ────────
        // Un criterio che fallisce ORA con lo STESSO exit code non-zero
        // misurato sull'albero PRE-lavoro (baseline all'innesto del profilo) e
        // SENZA file d'errore localizzati e' un fallimento di BOOTSTRAP
        // pre-esistente dell'ambiente (es. `npx eslint` exit 2 per config
        // assente: incidente run 695794af, pnpm build verde ma gate bocciato
        // per un criterio impossibile in QUALUNQUE stato del lavoro). Non e'
        // una regressione del run: non boccia, viene dichiarato nel verdict.
        // Condizioni STRETTE (solo segnali strutturati): exit non-zero
        // IDENTICO alla baseline + zero error_files (se il comando localizza
        // errori in file, decide il ramo delta sui touched_files). Se il run
        // avesse ROTTO lui il bootstrap (es. config cancellata), la baseline
        // sarebbe 0 e il confronto NON scatterebbe. `None` = fail-closed.
        let baseline_exit = spec.get("baseline_exit_code").and_then(Value::as_i64);
        // I due fatti viaggiano SEPARATI fino al verdetto: comporli qui
        // significava concludere «ambiente gia' rotto» senza avere sotto gli
        // occhi `build_errors`, cioe' la prova che invece un errore c'e'.
        let nessuna_localizzazione = error_files.is_empty();
        let stesso_esito_della_baseline =
            !exit_ok && baseline_exit.is_some_and(|b| b != 0 && actual_exit == Some(b));
        let outcome = verdetto_del_comando(FattiDelComando {
            delta_applicable,
            regression,
            nessuna_localizzazione,
            stesso_esito_della_baseline,
            build_errors,
            esito_misurato,
            exit_ok,
        });

        // max_output_chars (parita' Python: default 600, floor 200). Il criterio
        // BUILD del nodo Rust lo valorizza (build_output_max_chars).
        let max_chars = spec
            .get("max_output_chars")
            .and_then(Value::as_i64)
            .map(|v| v.max(200) as usize)
            .unwrap_or(DEFAULT_MAX_OUTPUT_CHARS);
        let excerpt = truncate_chars(&raw, max_chars);
        let total_chars = raw.chars().count();
        let mut ev = json!({
            "command": cmd,
            "exit_code": actual_exit,
            "expected_exit": expected_exit,
            "output_excerpt": excerpt,
            "output_truncated": total_chars > max_chars,
            "output_total_chars": total_chars,
            "build_errors": build_errors,
            "error_files": error_files.len(),
            "delta_applied": delta_applicable,
            "preexisting_error_files": preexisting_files,
            "regressed_files": regressed_files.clone(),
            "baseline_exit_code": baseline_exit,
            "preexisting_bootstrap": matches!(outcome, CriterionOutcome::Passed)
                && stesso_esito_della_baseline
                && nessuna_localizzazione,
        });
        if outcome.is_inconclusive() {
            ev["verdict"] = json!(format!(
                "Comando '{cmd}' terminato SENZA exit code: l'esito non e' stato misurato (il processo non ha prodotto uno stato d'uscita). Non e' una prova di difetto ne' di correttezza: il criterio non conta nel gate e la chiusura resta NON verificata."
            ));
        } else if matches!(outcome, CriterionOutcome::Passed)
            && stesso_esito_della_baseline
            && nessuna_localizzazione
        {
            ev["verdict"] = json!(format!(
                "Criterio fallito con esito IDENTICO alla baseline pre-lavoro (exit {} gia' misurato all'innesto del profilo): fallimento PRE-ESISTENTE dell'ambiente (es. config del tool assente), non una regressione di questo run. Debito del progetto, non blocca la chiusura.",
                baseline_exit.unwrap_or_default()
            ));
        } else if regression {
            // Il task ha lasciato errori nei file che ha modificato: blocca e dillo.
            ev["verdict"] = json!(format!(
                "Verifica delta: errori nei file modificati da questo task ({}). Correggi TUTTI gli errori in questi file prima di chiudere; il debito preesistente in altri file non e' richiesto.",
                regressed_files.join(", ")
            ));
        } else if delta_applicable && (build_errors > 0 || !exit_ok) {
            // Passa nonostante gli errori: sono debito preesistente in file non toccati.
            ev["verdict"] = json!(format!(
                "Verifica delta superata: {preexisting_files} file con errori PREESISTENTI non modificati da questo task (debito del progetto, non una regressione introdotta qui). Nessun errore nei file toccati dal task."
            ));
        } else if exit_ok && build_errors > 0 {
            // Fallback (delta non applicabile): exit-code bugiardo, come prima.
            ev["verdict"] = json!(format!(
                "Build uscito con exit ok ma l'output contiene {build_errors} errore/i di build (es. import non risolti): il bundle NON e' valido. Correggi gli errori sopra e riverifica."
            ));
        }
        (outcome, ev)
    }

    // ── service_logs_clean: run_command + match pattern sui log ───────────────

    async fn check_service_logs_clean(
        &self,
        spec: &Value,
        _timeout_s: f64,
    ) -> (CriterionOutcome, Value) {
        let cmd = spec.get("command").and_then(Value::as_str).unwrap_or("");
        let patterns: Vec<String> = spec
            .get("patterns")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if cmd.is_empty() || patterns.is_empty() {
            // NON APPLICABILE, che e' diverso da NON MISURABILE: non c'e' nulla
            // da misurare, quindi il criterio non impone nulla e non toglie
            // nulla alla verifica. Resta un pass, come da sempre.
            return (
                CriterionOutcome::Passed,
                json!({ "skipped": "service_logs_clean: command/patterns mancanti (N/A)" }),
            );
        }
        let raw = match self
            .run_tool("run_command", json!({ "command": cmd }))
            .await
        {
            Ok(o) => outcome_text(&o),
            // I log non si sono potuti leggere: non blocchiamo la chiusura (non
            // e' una prova di difetto) ma nemmeno la dichiariamo pulita.
            Err(e) => {
                return (
                    CriterionOutcome::Inconclusive,
                    json!({ "skipped_reason": format!("run_command log fallito: {e}") }),
                )
            }
        };
        let lows: Vec<String> = patterns.iter().map(|p| p.to_lowercase()).collect();
        let mut hits: Vec<String> = Vec::new();
        for line in raw.lines() {
            let ll = line.to_lowercase();
            if lows.iter().any(|p| ll.contains(p)) {
                hits.push(truncate_chars(line.trim(), 200));
                if hits.len() >= 8 {
                    break;
                }
            }
        }
        let passed = hits.is_empty();
        let mut evidence = json!({
            "command": truncate_chars(cmd, 120),
            "error_lines": hits.len(),
            "matched": hits.clone(),
        });
        if !passed {
            evidence["verdict"] = json!("errori runtime nei log dei servizi");
            evidence["output_excerpt"] = json!(format!(
                "ERRORI RUNTIME nei log dei servizi: il codice e' stato scritto ma il flusso reale FALLISCE.\n{}\nAGISCI: trova e correggi la causa (es. applica le migrazioni / crea le tabelle mancanti, sistema la rotta), poi RIESERCITA il flusso reale e verifica che gli errori siano spariti.",
                hits.join("\n")
            ));
        }
        (CriterionOutcome::measured(passed), evidence)
    }

    // ── http: chiamata REALE via reqwest (parita' httpx Python) ───────────────

    async fn check_http(
        &self,
        spec: &Value,
        expected: &Value,
        timeout_s: f64,
    ) -> (bool, Value) {
        let url = spec.get("url").and_then(Value::as_str).unwrap_or("");
        if url.is_empty() {
            return (false, json!({ "error": "spec.url obbligatorio" }));
        }
        let method = spec
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_uppercase();
        let expected_statuses = expected_statuses_from(expected.get("status"));
        let Ok(m) = reqwest::Method::from_bytes(method.as_bytes()) else {
            return (
                false,
                json!({ "error": format!("metodo HTTP invalido: {method}"), "url": url }),
            );
        };
        let rb = with_body_and_headers(
            self.http_client
                .request(m, url)
                .timeout(Duration::from_secs_f64(timeout_s)),
            spec,
        );
        let ricevuta = match risposta_ricevuta(rb).await {
            Ok(r) => r,
            // Servizio spento o irraggiungibile: e' un fallimento della prova,
            // non un guasto del runner (parita' col try/except Python).
            Err(e) => return (false, json!({ "error": format!("http call: {e}"), "url": url })),
        };
        esito_http(
            &method,
            url,
            &expected_statuses,
            expected.get("body_contains").and_then(Value::as_str),
            expected
                .get("reject_html")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            ricevuta.status,
            ricevuta.content_type.as_deref(),
            &ricevuta.text,
        )
    }

    // ── file_exists: interrogazione diretta del filesystem ────────────────────

    /// `file_exists`: il file dichiarato dal criterio c'e' sull'albero del run?
    ///
    /// Chiede al FILESYSTEM. Prima chiedeva a `list_files` — cioe' a un elenco
    /// composto per un lettore umano — e ne deduceva due cose leggendo il testo:
    /// se il tool fosse fallito (quattro parole cercate nei primi 80 caratteri,
    /// una delle quali, `"non trovato"`, il produttore non ha mai scritto: lui
    /// scriveva `non trovata`) e se il file ci fosse (match del basename come
    /// token isolato su una riga di elenco). La seconda deduzione era cieca ai
    /// dotfile, che quell'elenco NON mostra: un `.env` scritto dal run risultava
    /// ASSENTE, il criterio falliva, il gate bocciava e apriva un ciclo di
    /// correzione su un lavoro gia' fatto.
    ///
    /// L'esito e' a tre stati ([`EsistenzaFile`]) perche' "non ho potuto
    /// guardare" e' una risposta diversa da "non c'e'", e confonderle e' proprio
    /// il modo in cui questo criterio bocciava run corretti.
    async fn check_file_exists(
        &self,
        spec: &Value,
        expected: &Value,
        _timeout_s: f64,
    ) -> (CriterionOutcome, Value) {
        let path = spec.get("path").and_then(Value::as_str).unwrap_or("");
        if path.is_empty() {
            // file_exists senza path: N/A (pass), parita' Python. Non e' un
            // "non misurabile": non c'e' nessun file di cui rispondere.
            return (
                CriterionOutcome::Passed,
                json!({ "skipped": "file_exists senza path: criterio non applicabile (N/A)" }),
            );
        }
        let expected_exists = expected
            .get("exists")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let esito = interroga_esistenza(self.run_root.as_deref(), path).await;
        match esito {
            EsistenzaFile::NonInterrogabile { motivo } => (
                // Non misurabile, e ora lo DICE il tipo: prima era un `true`, e
                // per il gate un `true` valeva quanto una misura riuscita —
                // rispondere "esiste" senza aver guardato ha la stessa forma di
                // una prova e nessuna delle sue garanzie.
                CriterionOutcome::Inconclusive,
                json!({
                    "path": path,
                    "expected_exists": expected_exists,
                    "method": "filesystem",
                    "skipped_reason": motivo,
                }),
            ),
            EsistenzaFile::Esiste | EsistenzaFile::NonEsiste => {
                let exists = esito == EsistenzaFile::Esiste;
                (
                    CriterionOutcome::measured(exists == expected_exists),
                    json!({
                        "path": path,
                        "exists": exists,
                        "expected_exists": expected_exists,
                        "method": "filesystem",
                    }),
                )
            }
        }
    }

    // ── outputs_exist: agent_steps (tool mutativi) + file_exists ──────────────

    async fn check_outputs_exist(
        &self,
        spec: &Value,
        timeout_s: f64,
    ) -> Result<(CriterionOutcome, Value), PortError> {
        let run_id = spec
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if run_id.is_empty() {
            return Ok((
                CriterionOutcome::Passed,
                json!({ "skipped": "outputs_exist senza run_id: N/A" }),
            ));
        }
        let run_uuid = match Uuid::parse_str(run_id) {
            Ok(u) => u,
            Err(_) => {
                return Ok((
                    CriterionOutcome::Passed,
                    json!({ "skipped": "outputs_exist run_id non-UUID: N/A" }),
                ))
            }
        };
        // Tool mutativi file del run da agent_steps (status completed), ordinati.
        let mutator_names: Vec<&str> = OUTPUT_PATH_KEYS.iter().map(|(t, _)| *t).collect();
        let rows: Vec<(String, Value)> = sqlx::query_as(
            "SELECT tool_name, tool_input FROM agent_steps \
             WHERE run_id = $1 AND status = 'completed' AND tool_name = ANY($2) \
             ORDER BY step_index ASC",
        )
        .bind(run_uuid)
        .bind(&mutator_names)
        .fetch_all(&self.db)
        .await
        .map_err(|e| PortError::Tool(format!("outputs_exist lettura agent_steps: {e}").into()))?;

        let mut paths: Vec<String> = Vec::new();
        for (tool_name, tool_input) in &rows {
            let Some((_, key)) = OUTPUT_PATH_KEYS.iter().find(|(t, _)| *t == tool_name) else {
                continue;
            };
            // tool_input puo' essere JSONB object o stringa JSON.
            let obj = match tool_input {
                Value::Object(_) => tool_input.clone(),
                Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
                _ => Value::Null,
            };
            if let Some(p) = obj.get(*key).and_then(Value::as_str) {
                let p = p.trim().to_string();
                if !p.is_empty() && !paths.contains(&p) {
                    paths.push(p);
                }
            }
        }
        if paths.is_empty() {
            return Ok((
                CriterionOutcome::Passed,
                json!({ "skipped": "nessuno step mutativo file nel run: N/A" }),
            ));
        }
        let mut missing: Vec<String> = Vec::new();
        let mut checked: Vec<String> = Vec::new();
        // Path per cui `check_file_exists` non ha potuto guardare (radice del
        // run ignota, percorso non risolvibile): NON sono "assenti" (altrimenti
        // il gate boccerebbe un run che ha scritto l'output, solo perche' il
        // criterio non sapeva dove cercarlo) ma nemmeno "verificati" — l'evidence
        // deve portarne traccia, o un lettore del report legge "checked" e crede
        // che ogni path sia stato davvero confermato sul disco.
        let mut inconclusive: Vec<String> = Vec::new();
        // Cap difensivo (parita' Python: 20 output).
        for p in paths.iter().take(20) {
            // L'esito del sotto-criterio arriva TIPIZZATO: prima si rileggeva un
            // flag dall'evidence JSON che il produttore poteva smettere di
            // scrivere senza che nulla se ne accorgesse (regola M).
            let (esito, _ev) = self
                .check_file_exists(&json!({ "path": p }), &json!({}), timeout_s)
                .await;
            checked.push(p.clone());
            match esito {
                CriterionOutcome::Inconclusive => inconclusive.push(p.clone()),
                CriterionOutcome::Failed => missing.push(p.clone()),
                CriterionOutcome::Passed => {}
            }
        }
        if !missing.is_empty() {
            // Un file dichiarato e ASSENTE e' una misura, anche se altri path non
            // si sono potuti guardare: la prova del difetto c'e' gia'.
            return Ok((
                CriterionOutcome::Failed,
                json!({
                    "missing": missing,
                    "checked": checked,
                    "inconclusive": inconclusive,
                    "verdict": "output dichiarati dagli step assenti sul filesystem a fine run",
                }),
            ));
        }
        let mut evidence = json!({ "checked": checked, "inconclusive": inconclusive });
        if !inconclusive.is_empty() {
            evidence["verdict"] = json!(
                "output presenti dove verificabili; alcuni non erano verificabili (radice del run non risolta) -- NON e' una conferma piena"
            );
            // L'evidence lo diceva gia' a parole; l'esito diceva "passato". Ora
            // le due cose coincidono: verifica incompleta = non misurata.
            return Ok((CriterionOutcome::Inconclusive, evidence));
        }
        Ok((CriterionOutcome::Passed, evidence))
    }
}

#[async_trait]
impl CriteriaRunner for FinalGateCriteriaRunnerAdapter {
    async fn run(
        &self,
        criteria: Vec<CriterionSpec>,
    ) -> Result<Vec<CriterionResult>, PortError> {
        let mut out = Vec::with_capacity(criteria.len());
        for c in &criteria {
            out.push(self.run_one(c).await?);
        }
        Ok(out)
    }
}

/// Promuove l'esito di un criterio che ha DAVVERO misurato (`bool`) alla forma
/// a tre stati. Da usare solo dove il "non misurabile" non e' rappresentabile:
/// i criteri puri (fatti gia' nella spec) e quelli la cui prova o riesce o
/// fallisce (una risposta HTTP e' sempre una misura, anche un 500).
fn misurato((passed, evidence): (bool, Value)) -> (CriterionOutcome, Value) {
    (CriterionOutcome::measured(passed), evidence)
}

/// Status HTTP attesi da un criterio: intero singolo o lista (parita' Python).
/// Assente o non interpretabile -> `200`, il default storico del criterio.
fn expected_statuses_from(status: Option<&Value>) -> Vec<u16> {
    const OK: u16 = 200;
    match status {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(Value::as_i64)
            .map(|v| v as u16)
            .collect(),
        Some(v) => vec![v.as_i64().map(|n| n as u16).unwrap_or(OK)],
        None => vec![OK],
    }
}

/// Aggiunge alla richiesta il corpo e gli header dichiarati nella `spec` del
/// criterio. Un corpo oggetto/array parte come JSON, una stringa non vuota come
/// corpo grezzo; gli altri tipi non aggiungono nulla.
fn with_body_and_headers(
    mut rb: reqwest::RequestBuilder,
    spec: &Value,
) -> reqwest::RequestBuilder {
    if let Some(body) = spec.get("body") {
        match body {
            Value::Object(_) | Value::Array(_) => rb = rb.json(body),
            Value::String(s) if !s.is_empty() => rb = rb.body(s.clone()),
            _ => {}
        }
    }
    if let Some(Value::Object(headers)) = spec.get("headers") {
        for (k, v) in headers {
            if let Some(vs) = v.as_str() {
                rb = rb.header(k.as_str(), vs);
            }
        }
    }
    rb
}

/// Cio' che si e' potuto osservare di una risposta HTTP.
///
/// Esiste come tipo perche' i tre campi si leggono in un ORDINE obbligato: il
/// `Content-Type` sta negli header e va preso PRIMA che `text()` consumi la
/// risposta. Tenerli insieme rende quell'ordine una proprieta' della
/// costruzione invece di una regola da ricordare al call site.
struct RispostaRicevuta {
    status: u16,
    /// Header standard, quindi segnale strutturato: leggerlo non e' dedurre lo
    /// stato dal testo (regola M), e' chiedere al protocollo che FORMA abbia la
    /// risposta. `None` se il server non lo manda.
    content_type: Option<String>,
    text: String,
}

/// Esegue la richiesta e raccoglie cio' che serve a giudicarla.
///
/// Separata da `check_http` perche' sono due responsabilita': qui si tocca la
/// rete, li' si decide. La decisione e' poi tutta in `esito_http`, che e' pura.
async fn risposta_ricevuta(rb: reqwest::RequestBuilder) -> Result<RispostaRicevuta, reqwest::Error> {
    let resp = rb.send().await?;
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let text = resp.text().await.unwrap_or_default();
    Ok(RispostaRicevuta {
        status,
        content_type,
        text,
    })
}

/// Esito di una prova HTTP data la risposta ricevuta. PURA (nessuna rete): si
/// esercita senza server, e per lo stesso motivo il test che ne verifica il
/// contenuto puo' partire dal produttore vero invece di fabbricare l'evidence.
///
/// La DECISIONE e' lo STATUS (segnale strutturato, regola M). Il corpo entra
/// nella decisione SOLO se il criterio dichiara `body_contains` — cosa che fanno
/// i criteri configurati a mano, mai quelli derivati da una dichiarazione
/// dell'agente; per tutti gli altri e' materiale diagnostico.
///
/// `reject_html` e' la sola eccezione, e non e' una lettura del corpo: guarda il
/// `Content-Type`, header standard. Serve a una domanda che lo status da solo non
/// puo' distinguere — «questa risposta viene dal backend, o e' la pagina del
/// frontend?». Vedi [`risposta_e_html`].
#[allow(clippy::too_many_arguments)]
fn esito_http(
    method: &str,
    url: &str,
    expected_statuses: &[u16],
    body_contains: Option<&str>,
    reject_html: bool,
    actual: u16,
    content_type: Option<&str>,
    text: &str,
) -> (bool, Value) {
    let body_excerpt = truncate_chars(text, HTTP_BODY_EXCERPT_CHARS);
    let mut passed = expected_statuses.contains(&actual);
    if let Some(needle) = body_contains {
        passed = passed && text.contains(needle);
    }
    let html_indebito = reject_html && risposta_e_html(content_type, text);
    if html_indebito {
        passed = false;
    }
    let mut verdict = http_verdict(method, url, actual, expected_statuses, &body_excerpt);
    if html_indebito {
        verdict = format!(
            "{verdict} — RISPOSTA HTML: il frontend ha servito la propria pagina invece \
             dei dati del backend. Il proxy non raggiunge l'API (tipicamente un `rewrite` \
             che toglie il prefisso su cui il backend espone), e il fallback della SPA \
             maschera il 404 con un 200."
        );
    }
    (
        passed,
        json!({
            "url": url,
            "method": method,
            "status": actual,
            "expected_status": expected_statuses,
            "content_type": content_type,
            "body_excerpt": body_excerpt,
            "verdict": verdict,
        }),
    )
}

/// La risposta e' una pagina HTML invece del dato atteso da un endpoint di API?
///
/// ROOT CAUSE, misurata il 04/08/2026 su biblioteca-scolastica. Il criterio
/// d'integrazione (`endpoint_probes::criteri_integrazione_frontend`) prova gli
/// endpoint ANCHE attraverso l'origine del frontend, e decideva sul solo status:
///
///     35954/api/books -> HTTP 200, Content-Type: text/html        <- la SPA
///     35976/api/books -> HTTP 200, Content-Type: application/json <- il backend
///     35976/books     -> HTTP 404
///
/// `vite.config.ts` aveva `rewrite: p => p.replace(/^\/api/, '')`, che toglie il
/// prefisso su cui il backend espone: il proxy inoltrava a `/books`, il backend
/// rispondeva 404, e Vite ripiegava su `index.html` con **status 200**. Il gate
/// vedeva 200 e approvava un'applicazione le cui due meta' non si parlavano.
///
/// Era il limite DICHIARATO nel commento di quel criterio quando fu scritto
/// ("cattura il proxy assente o mal indirizzato, non il fallback silenzioso");
/// qui viene chiuso, e la chiusura non passa dal corpo ma dall'header.
///
/// Il `Content-Type` e' la fonte: e' cio' che il server DICHIARA di aver
/// mandato. Il corpo interviene solo quando l'header manca — un `<!DOCTYPE html`
/// in testa e' sintassi, non prosa, e senza header non c'e' altro da chiedere.
fn risposta_e_html(content_type: Option<&str>, text: &str) -> bool {
    if let Some(ct) = content_type {
        let ct = ct.to_ascii_lowercase();
        // `text/html`, `text/html; charset=utf-8`, `application/xhtml+xml`.
        return ct.contains("text/html") || ct.contains("xhtml");
    }
    let inizio = text.trim_start().to_ascii_lowercase();
    inizio.starts_with("<!doctype html") || inizio.starts_with("<html")
}

/// Caratteri di corpo della risposta conservati nell'evidence di una prova HTTP.
const HTTP_BODY_EXCERPT_CHARS: usize = 400;

/// Riga DIAGNOSTICA di una prova HTTP (display), letta da
/// `final_gate::render_failed_block` — che salta i criteri falliti senza testo.
/// Senza di essa il gate bocciava una POST 500 e rimandava all'agente un blocco
/// che non nominava ne' l'URL ne' lo status: un re-loop cieco.
///
/// PURA, e non ri-decide nulla: riceve l'esito gia' deciso dallo status (regola
/// M). Il corpo entra solo come coda diagnostica.
fn http_verdict(
    method: &str,
    url: &str,
    actual: u16,
    expected_statuses: &[u16],
    body_excerpt: &str,
) -> String {
    let atteso: Vec<String> = expected_statuses.iter().map(u16::to_string).collect();
    let coda = if body_excerpt.is_empty() {
        String::new()
    } else {
        format!("\n{body_excerpt}")
    };
    format!("{method} {url} -> {actual} (atteso {}){coda}", atteso.join("/"))
}

/// Taglia una stringa a `max` CARATTERI (non byte: evita di spezzare UTF-8).
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// Testo del risultato di un tool: il `content` normalizzato a stringa.
///
/// I criteri che ISPEZIONANO l'output (errori di build, righe di log) leggono
/// da qui; quelli che ne valutano l'ESITO leggono i campi strutturati del
/// [`ToolOutcome`] e non passano di qua.
fn outcome_text(outcome: &ToolOutcome) -> String {
    match &outcome.content {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Risposta alla domanda "questo file c'e'?", con lo stato che mancava.
///
/// `NonInterrogabile` non e' un dettaglio di cortesia: e' cio' che tiene
/// separato "ho guardato e non c'e'" da "non ho potuto guardare". Fonderli
/// significa dichiarare assente un file che nessuno ha cercato, ed e' un
/// verdetto che il gate prende sul serio quanto una misura.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EsistenzaFile {
    /// Il filesystem lo conferma presente sotto la radice del run.
    Esiste,
    /// Il filesystem lo conferma assente: il percorso e' stato risolto e
    /// interrogato, e non c'e' nulla li'.
    NonEsiste,
    /// Non e' stato possibile guardare (radice del run ignota, percorso non
    /// risolvibile sotto la radice, errore di I/O). Il motivo viaggia con
    /// l'esito: un inconcludente senza causa e' indistinguibile da una svista.
    NonInterrogabile { motivo: String },
}

/// Verdetto testuale del criterio-suite: la dichiarazione del punto unico, piu'
/// la nota sul fallimento pre-esistente quando e' quello a farlo passare (senza,
/// l'evidence direbbe "fallita" accanto a un criterio superato).
fn verdetto_suite(v: &crate::suite_verification::SuiteVerification, preesistente: bool) -> String {
    if preesistente {
        format!(
            "{} La suite falliva GIA' con lo stesso exit code sull'albero pre-lavoro: \
             debito del progetto, non una regressione di questo run.",
            v.dichiarazione()
        )
    } else {
        v.dichiarazione()
    }
}

/// Interroga il FILESYSTEM sull'esistenza di `path`, relativo alla radice del run.
///
/// Il percorso si risolve con [`nexus_types::workspace_paths::resolve_workspace_target`]
/// — lo STESSO punto unico che usano i tool di SCRITTURA (`figma_tools`,
/// `image_tools`, `video_tools`, `audio_tools`, gli handler HTTP di
/// `project_files`) per de-duplicare la root e bloccare il traversal `..`,
/// SENZA richiedere che il target esista gia'. E' la scelta obbligata: il
/// gemello `nexus_agent_tools::paths::resolve_relative_path` — quello dei tool
/// di LETTURA — canonicalizza il path INTERO, che fallisce con "non trovato"
/// per costruzione su un file che non c'e' ancora. Usarlo qui avrebbe reso
/// ogni file assente `NonInterrogabile` (quindi PASS per il gate) invece di
/// `NonEsiste`: la stessa premessa sbagliata che questo fix doveva togliere,
/// solo spostata di una riga (regola O: lo strumento deve poter guardare
/// esattamente cio' che gli si chiede, non un sottoinsieme che gia' esiste).
/// Risolverlo qui a modo proprio (un `root.join(path)`) darebbe al gate
/// un'idea di "dentro il progetto" diversa da quella con cui il run scrive.
async fn interroga_esistenza(run_root: Option<&Path>, path: &str) -> EsistenzaFile {
    let Some(root) = run_root else {
        return EsistenzaFile::NonInterrogabile {
            motivo: "radice del run non risolta (sessione senza progetto): esistenza non verificabile".to_string(),
        };
    };
    let assoluto = match nexus_types::workspace_paths::resolve_workspace_target(root, path) {
        Ok((_, assoluto)) => assoluto,
        Err(e) => {
            return EsistenzaFile::NonInterrogabile {
                motivo: format!("percorso non risolvibile sotto la radice del run: {}", e.message()),
            }
        }
    };
    match tokio::fs::try_exists(&assoluto).await {
        Ok(true) => EsistenzaFile::Esiste,
        Ok(false) => EsistenzaFile::NonEsiste,
        // Permessi, percorso troppo lungo, volume smontato: il file potrebbe
        // esserci eccome. `try_exists` distingue questo caso da `Ok(false)`
        // proprio perche' non vanno confusi (a differenza di `Path::exists`,
        // che li appiattisce entrambi su `false`).
        Err(e) => EsistenzaFile::NonInterrogabile {
            motivo: format!("filesystem non interrogabile: {e}"),
        },
    }
}

/// Cio' che si e' potuto osservare di UN'esecuzione di comando, gia' ridotto a
/// fatti. E' l'input di [`verdetto_del_comando`]: la raccolta dei fatti tocca il
/// mondo (tool, filesystem, DB), il verdetto no, e tenerli insieme rendeva la
/// parte che vale la pena interrogare raggiungibile solo attraverso quella che
/// vale la pena isolare.
struct FattiDelComando {
    /// L'output ha localizzato errori IN FILE e il run dichiara file toccati:
    /// si puo' rispondere "questo run ha rotto qualcosa?" invece della domanda
    /// piu' grossolana "il progetto ha errori?".
    delta_applicable: bool,
    /// Almeno un file d'errore e' fra quelli che questo run ha toccato.
    regression: bool,
    /// L'output NON ha attribuito nessun errore a un file: o il progetto e'
    /// pulito, o il formato non e' fra quelli che `build_error_files` sa
    /// leggere. I due casi qui NON si distinguono, ed e' esattamente il motivo
    /// per cui questo campo e' un FATTO GREZZO e non l'ipotesi gia' conclusa:
    /// chi la conclude deve vedere anche `build_errors`, che invece PROVA gli
    /// errori (vedi `verdetto_del_comando`).
    nessuna_localizzazione: bool,
    /// Exit non-zero IDENTICO alla baseline misurata prima del lavoro:
    /// l'ambiente rispondeva cosi' anche prima che il run cominciasse.
    stesso_esito_della_baseline: bool,
    /// Errori di build PROVATI dall'output (rete di sicurezza sull'exit bugiardo).
    build_errors: usize,
    /// C'e' stata una misura: exit code presente, oppure il tool ha DICHIARATO
    /// il fallimento (invocazione rifiutata).
    esito_misurato: bool,
    /// L'exit code misurato e' quello atteso.
    exit_ok: bool,
}

/// Il verdetto su un criterio comando, dai soli fatti. Puro per costruzione:
/// nessun I/O, nessun testo analizzato (regola M), quindi interrogabile caso per
/// caso senza montare un esecutore.
///
/// L'ORDINE dei rami e' il contratto, non uno stile: il delta sui file toccati
/// precede tutto perche' e' la misura piu' fine disponibile; gli errori PROVATI
/// vengono prima dell'ipotesi «ambiente gia' rotto», perche' quell'ipotesi si
/// regge su un'ASSENZA (nessun file localizzato) e un'assenza non batte una
/// prova; "non misurato" viene per ULTIMO fra i casi negativi, cosi' non puo'
/// assorbire un caso che qualcuno aveva gia' misurato.
///
/// ROOT CAUSE dell'ordine attuale: `preexisting_bootstrap` arrivava qui GIA'
/// composto dal chiamante, che lo costruiva con `error_files.is_empty()` —
/// mentre il doc di `build_error_files` (final_gate.rs) dichiara: «un set VUOTO
/// significa "nessuna localizzazione ricavabile" (formato non coperto o output
/// pulito) — il chiamante NON deve dedurne "nessun errore"... ma ricadere sul
/// criterio assoluto (fail-closed)». Il ramo assolveva PRIMA di guardare
/// `build_errors`, quindi un criterio con 3 errori di build provati usciva
/// `Passed` — comportamento che un test fissava perfino come atteso.
///
/// Ora i due fatti arrivano separati e l'ipotesi si compone QUI, dove la prova
/// e' visibile: un output che non attribuisce errori a un file puo' significare
/// «pulito» o «non so leggerlo», e nel dubbio non si assolve niente che il
/// conteggio degli errori abbia gia' provato.
fn verdetto_del_comando(f: FattiDelComando) -> CriterionOutcome {
    if f.delta_applicable {
        // Chiude se il task non ha lasciato errori nei file che ha toccato,
        // anche se il progetto ha debito preesistente altrove. E' una MISURA
        // anche senza exit code: l'output ha localizzato gli errori.
        return CriterionOutcome::measured(!f.regression);
    }
    if f.build_errors > 0 {
        // L'output PROVA errori di build: misura valida a prescindere
        // dall'exit code, e a prescindere da come stava l'ambiente prima.
        return CriterionOutcome::Failed;
    }
    // Ipotesi «l'ambiente era gia' cosi'»: si compone solo ORA, cioe' dopo aver
    // escluso gli errori provati. Pretende ENTRAMBI i fatti — stesso esito della
    // baseline E nessuna localizzazione — perche' il secondo da solo non
    // distingue un progetto pulito da un formato che non sappiamo leggere.
    if f.stesso_esito_della_baseline && f.nessuna_localizzazione {
        return CriterionOutcome::Passed;
    }
    if !f.esito_misurato {
        // Nessun exit code, nessuna dichiarazione di fallimento, nessun errore
        // nell'output: non si e' misurato NULLA. Non e' una prova di difetto (il
        // gate non boccia) e non e' una prova di correttezza (la chiusura non e'
        // verificata).
        //
        // Il delta sulla baseline, che altrove distingue il preesistente
        // dall'introdotto, qui non e' ponibile: `baseline_exit_code` assente
        // significa insieme "mai misurata" e "misurata senza exit code"
        // (`measure_command_exit` collassa i due casi in `None`), quindi
        // confrontarla sarebbe una risposta inventata su un segnale ambiguo.
        return CriterionOutcome::Inconclusive;
    }
    // Fail-closed: criterio assoluto.
    CriterionOutcome::measured(f.exit_ok)
}

#[cfg(test)]
mod verdetto_del_comando_tests {
    use super::*;

    fn fatti() -> FattiDelComando {
        FattiDelComando {
            delta_applicable: false,
            regression: false,
            nessuna_localizzazione: false,
            stesso_esito_della_baseline: false,
            build_errors: 0,
            esito_misurato: true,
            exit_ok: true,
        }
    }

    /// L'ordine dei rami e' il contratto: ogni caso qui fissa una PRECEDENZA,
    /// non solo un esito. Sono le combinazioni in cui due rami sarebbero
    /// entrambi applicabili, cioe' le uniche in cui l'ordine si vede.
    ///
    /// MUTAZIONE: spostando il ramo `!esito_misurato` sopra `build_errors > 0`,
    /// il terzo caso torna `Inconclusive` e rosseggia — sarebbe il fail-open che
    /// assolve un build rotto solo perche' il processo non ha reso un exit code.
    #[test]
    fn la_precedenza_fra_i_rami_e_il_contratto() {
        // Il delta pretende errori LOCALIZZATI, il bootstrap pretende che non
        // ce ne siano: la vecchia fixture li accendeva insieme, fissando una
        // precedenza fra due rami che il produttore non puo' emettere insieme
        // (regola O). Qui la precedenza si misura su cio' che e' costruibile:
        // delta applicabile e stesso esito della baseline.
        let delta_vince = FattiDelComando {
            delta_applicable: true,
            regression: false,
            stesso_esito_della_baseline: true,
            exit_ok: false,
            ..fatti()
        };
        assert_eq!(
            verdetto_del_comando(delta_vince),
            CriterionOutcome::Passed,
            "il delta sui file toccati e' la misura piu' fine: precede il bootstrap"
        );

        // LA PRECEDENZA CORRETTA, che prima era fissata al contrario: una PROVA
        // (build_errors) batte un'IPOTESI che si regge su un'assenza (nessuna
        // localizzazione). Il vecchio test pretendeva `Passed` con 3 errori di
        // build provati, cioe' fissava il fail-open come contratto.
        //
        // MUTAZIONE: rimettere il ramo bootstrap sopra `build_errors > 0` fa
        // tornare `Passed` e rosseggiare — col valore del difetto reale.
        let prova_batte_ipotesi = FattiDelComando {
            stesso_esito_della_baseline: true,
            nessuna_localizzazione: true,
            build_errors: 3,
            exit_ok: false,
            ..fatti()
        };
        assert_eq!(
            verdetto_del_comando(prova_batte_ipotesi),
            CriterionOutcome::Failed,
            "3 errori di build PROVATI non si assolvono perche' l'output non li ha attribuiti a un file"
        );

        // Senza errori provati, l'ipotesi regge: e' il caso legittimo per cui il
        // ramo esiste (config del tool assente da prima del run).
        let bootstrap_legittimo = FattiDelComando {
            stesso_esito_della_baseline: true,
            nessuna_localizzazione: true,
            exit_ok: false,
            ..fatti()
        };
        assert_eq!(
            verdetto_del_comando(bootstrap_legittimo),
            CriterionOutcome::Passed,
            "un ambiente gia' rotto prima del run, senza errori provati, non diventa colpa del run"
        );

        // E i due fatti servono ENTRAMBI: la sola assenza di localizzazione non
        // assolve, o un formato che non sappiamo leggere diventerebbe un
        // lasciapassare.
        let solo_assenza = FattiDelComando {
            nessuna_localizzazione: true,
            exit_ok: false,
            ..fatti()
        };
        assert_eq!(
            verdetto_del_comando(solo_assenza),
            CriterionOutcome::Failed,
            "senza la baseline che lo confermi, un output non localizzato non assolve"
        );

        let build_batte_il_non_misurato = FattiDelComando {
            build_errors: 2,
            esito_misurato: false,
            exit_ok: false,
            ..fatti()
        };
        assert_eq!(
            verdetto_del_comando(build_batte_il_non_misurato),
            CriterionOutcome::Failed,
            "l'output PROVA gli errori: l'exit code assente non li cancella"
        );

        let niente_da_misurare = FattiDelComando {
            esito_misurato: false,
            exit_ok: false,
            ..fatti()
        };
        assert_eq!(
            verdetto_del_comando(niente_da_misurare),
            CriterionOutcome::Inconclusive,
            "senza alcun segnale non si boccia e non si assolve"
        );

        let assoluto = FattiDelComando {
            exit_ok: false,
            ..fatti()
        };
        assert_eq!(
            verdetto_del_comando(assoluto),
            CriterionOutcome::Failed,
            "fail-closed: misurato e diverso dall'atteso"
        );
    }
}

#[cfg(test)]
mod verdetto_suite_tests {
    use super::*;
    use crate::suite_verification::{
        OrigineEsito, SuiteOutcome, SuiteStats, SuiteVerification,
    };

    fn verifica(outcome: SuiteOutcome, exit: Option<i32>) -> SuiteVerification {
        SuiteVerification {
            outcome,
            origine: OrigineEsito::Eseguita,
            stats: SuiteStats {
                passed: 19,
                failed: 2,
                ..Default::default()
            },
            test_instabili: vec!["e2e/home.spec.ts:5:3".to_string()],
            motivo_non_classificato: None,
            exit_code: exit,
            testo: "output".to_string(),
            job_id: None,
            state_key: Some("stato-A".to_string()),
            suite_key: "app|playwright test".to_string(),
        }
    }

    /// `flaky` PASSA il criterio: e' la decisione centrale del presidio. Se
    /// bocciasse, il ciclo di correzione ripartirebbe su codice sano — il
    /// difetto misurato il 31/07/2026.
    #[test]
    fn flaky_non_boccia_il_criterio_e_lo_dichiara() {
        let (esito, ev) = FinalGateCriteriaRunnerAdapter::evidenza_suite(
            "npx playwright test",
            Ok(verifica(SuiteOutcome::Flaky, Some(1))),
            None,
        );
        assert_eq!(esito, CriterionOutcome::Passed);
        assert_eq!(ev["suite_outcome"], json!("flaky"));
        assert_eq!(ev["flaky_tests"][0], json!("e2e/home.spec.ts:5:3"));
    }

    /// Un fallimento riprodotto boccia, come prima.
    #[test]
    fn tests_failed_boccia_il_criterio() {
        let (esito, ev) = FinalGateCriteriaRunnerAdapter::evidenza_suite(
            "npx playwright test",
            Ok(verifica(SuiteOutcome::TestsFailed, Some(1))),
            None,
        );
        assert_eq!(esito, CriterionOutcome::Failed);
        assert_eq!(ev["suite_outcome"], json!("tests_failed"));
    }

    /// Delta-aware: una suite che falliva GIA' con lo stesso exit code
    /// sull'albero pre-lavoro e' debito del progetto, non una regressione di
    /// questo run. Senza questo ramo la delega al punto unico avrebbe
    /// introdotto proprio la bocciatura che il gate delta-aware evita.
    #[test]
    fn fallimento_identico_alla_baseline_non_boccia() {
        let (esito, ev) = FinalGateCriteriaRunnerAdapter::evidenza_suite(
            "npx playwright test",
            Ok(verifica(SuiteOutcome::TestsFailed, Some(1))),
            Some(1),
        );
        assert_eq!(esito, CriterionOutcome::Passed);
        assert_eq!(ev["preexisting_bootstrap"], json!(true));
        assert!(ev["verdict"].as_str().unwrap().contains("pre-lavoro"));
    }

    /// Baseline VERDE: il run ha rotto la suite, e va bocciato.
    #[test]
    fn baseline_verde_e_suite_rossa_boccia() {
        let (esito, _) = FinalGateCriteriaRunnerAdapter::evidenza_suite(
            "npx playwright test",
            Ok(verifica(SuiteOutcome::TestsFailed, Some(1))),
            Some(0),
        );
        assert_eq!(esito, CriterionOutcome::Failed);
    }

    /// Una verifica che non ha prodotto esito (timeout, runner morto, DB del
    /// progetto assente, radice non risolvibile) NON boccia il criterio: e' un
    /// guasto dell'ESECUZIONE, non una misura del codice.
    ///
    /// Prima usciva `Failed`, e il gate bocciava codice che nessuno aveva
    /// provato — il difetto non stava nel giudizio ma nella firma: `evidenza_suite`
    /// ritornava `(bool, Value)`, e in un bool il «non lo so» non ha un posto
    /// dove stare. Il ramo gemello `check_run_command`, venticinque righe piu'
    /// sotto, gestiva gia' lo stesso caso come `Inconclusive`.
    ///
    /// MUTAZIONE: rimettere `CriterionOutcome::measured(false)` sul ramo `Err`
    /// fa tornare `Failed` e rosseggiare — col valore del difetto reale.
    #[test]
    fn il_guasto_della_verifica_non_boccia_il_codice() {
        let (esito, ev) = FinalGateCriteriaRunnerAdapter::evidenza_suite(
            "npx playwright test",
            Err("Timeout dopo 600s".to_string()),
            None,
        );
        assert_eq!(
            esito,
            CriterionOutcome::Inconclusive,
            "un runner morto non e' una prova di difetto del codice"
        );
        assert!(ev["error"].as_str().unwrap().contains("Timeout"));
        // Il motivo resta leggibile: inconcludente non vuol dire muto.
        assert!(ev["verdict"].as_str().unwrap().contains("guasto dell'esecuzione"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Chiama `esito_http` come lo chiama la produzione, leggendo `reject_html`
    /// dal criterio invece di passarlo a mano: cosi' il test attraversa anche la
    /// dichiarazione, non solo il calcolo (regola O).
    fn prova_http(
        expected: &Value,
        status: u16,
        content_type: Option<&str>,
        corpo: &str,
    ) -> (bool, Value) {
        esito_http(
            "GET",
            "http://127.0.0.1:35954/api/books",
            &expected_statuses_from(expected.get("status")),
            expected.get("body_contains").and_then(Value::as_str),
            expected
                .get("reject_html")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            status,
            content_type,
            corpo,
        )
    }

    /// IL CASO MISURATO il 04/08/2026 su biblioteca-scolastica: attraverso il
    /// frontend l'endpoint risponde 200 con `text/html` — e' la pagina della
    /// SPA, non i dati del backend.
    ///
    /// MUTAZIONE: togliere `if html_indebito { passed = false; }` da
    /// `esito_http` fa rosseggiare la prima asserzione, cioe' il gate torna ad
    /// approvare un'applicazione le cui due meta' non si parlano.
    #[test]
    fn attraverso_il_frontend_una_pagina_html_non_e_una_risposta_del_backend() {
        // Il criterio lo costruisce il PRODUTTORE, non il test.
        let declared = json!({
            "outcome": "done",
            "endpoints": nexus_agent_graph::decisions::endpoint_probes::normalize_endpoints(
                Some(&json!([{ "method": "GET", "url": "http://127.0.0.1:35976/api/books" }])),
            ),
        });
        let criteri = nexus_agent_graph::decisions::endpoint_probes::criteri_integrazione_frontend(
            Some(&declared),
            Some("http://127.0.0.1:35954"),
            15.0,
        );
        assert_eq!(criteri.len(), 1, "il produttore deve emettere la prova: {criteri:?}");
        let expected = &criteri[0].expected;

        // Il fallback SPA: status 200, ma la forma e' una pagina.
        let (passed, ev) = prova_http(
            expected,
            200,
            Some("text/html"),
            "<!DOCTYPE html>\n<html lang=\"en\">",
        );
        assert!(!passed, "200 con text/html attraverso il frontend NON e' una risposta valida");
        let verdetto = ev["verdict"].as_str().unwrap_or_default();
        assert!(
            verdetto.contains("RISPOSTA HTML"),
            "il verdetto deve dire COSA e' successo, non solo che e' fallito: {verdetto}"
        );
        assert_eq!(ev["content_type"], json!("text/html"), "l'evidenza porta l'header");

        // Il backend vero, attraverso lo stesso proxy: passa.
        let (passed, _) = prova_http(
            expected,
            200,
            Some("application/json; charset=utf-8"),
            "[{\"id\":2,\"title\":\"Harry Potter\"}]",
        );
        assert!(passed, "il JSON del backend attraverso il proxy e' l'esito atteso");
    }

    /// Senza `reject_html` (criteri diretti al backend, o configurati a mano) una
    /// risposta HTML resta legittima: una GET sulla home di un servizio web
    /// risponde HTML per definizione.
    #[test]
    fn sul_backend_una_risposta_html_resta_legittima() {
        let expected = json!({ "status": [200] });
        let (passed, _) = prova_http(&expected, 200, Some("text/html"), "<!DOCTYPE html>");
        assert!(passed, "senza reject_html l'HTML non e' un difetto");
    }

    /// Header assente: si guarda l'inizio del corpo, che e' sintassi e non prosa.
    /// Un JSON che PARLA di html non e' una pagina.
    #[test]
    fn senza_header_decide_la_sintassi_non_una_parola_nel_corpo() {
        assert!(risposta_e_html(None, "  <!DOCTYPE html><html>"));
        assert!(risposta_e_html(None, "<html><body>x</body></html>"));
        assert!(!risposta_e_html(None, "{\"tipo\":\"text/html\",\"nota\":\"<html> nel dato\"}"));
        // L'header, quando c'e', ha la precedenza sul corpo.
        assert!(!risposta_e_html(Some("application/json"), "<!DOCTYPE html>"));
        assert!(risposta_e_html(Some("text/html; charset=utf-8"), "{}"));
    }

    /// ToolExecutor fittizio: ritorna risultati pre-programmati per nome, e
    /// registra le chiamate ricevute (per asserire gli args).
    struct FakeToolExecutor {
        /// risultati per tool_name (in coda: la N-esima chiamata pop dell'indice N).
        results: std::collections::HashMap<String, Vec<String>>,
        /// log dei nomi tool chiamati per le asserzioni.
        calls: StdMutex<Vec<String>>,
    }

    impl FakeToolExecutor {
        fn with(results: &[(&str, &[&str])]) -> Arc<Self> {
            let mut map = std::collections::HashMap::new();
            for (name, outs) in results {
                map.insert(
                    name.to_string(),
                    outs.iter().map(|s| s.to_string()).collect(),
                );
            }
            Arc::new(Self {
                results: map,
                calls: StdMutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl ToolExecutor for FakeToolExecutor {
        async fn execute(
            &self,
            call: ToolCall,
        ) -> Result<nexus_agent_graph::runtime::ports::ToolOutcome, PortError> {
            self.calls.lock().unwrap().push(call.name.clone());
            let idx = self
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter(|n| *n == &call.name)
                .count()
                - 1;
            let content = self
                .results
                .get(&call.name)
                .and_then(|v| v.get(idx))
                .cloned()
                .unwrap_or_default();
            // Deriva is_error/exit_code con lo STESSO punto unico della
            // produzione (`ToolRunnerExecutorAdapter::execute`, regola O): un
            // fake che li lasciasse a `Default` (sempre `None`/`false`)
            // mentirebbe su cio' che un ToolExecutor vero produce da un testo
            // con `EXIT CODE: N`, e i criteri che ora leggono quei campi
            // strutturati (non piu' il testo) misurerebbero un doppio che non
            // si comporta come l'originale.
            Ok(crate::agent_graph_adapter::tool_executor::map_result_to_outcome(
                &call.id,
                nexus_types::tool_outcome::RispostaTool::da_testo_legacy(content),
            ))
        }
    }

    fn spec(criterion_type: &str, spec: Value, expected: Value) -> CriterionSpec {
        CriterionSpec {
            criterion_type: criterion_type.to_string(),
            spec,
            expected,
            timeout_s: None,
        }
    }

    // sqlx::test fornisce un pool; per i criteri che non toccano il DB usiamo
    // comunque il pool (db richiesto dal costruttore) ma non lo interroghiamo.

    #[sqlx::test]
    async fn run_command_build_passa_su_exit_zero(pool: PgPool) {
        let exec = FakeToolExecutor::with(&[("run_command", &["compilato\nEXIT CODE: 0\nok"])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec.clone(), pool, None);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "cargo check" }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert_eq!(res.len(), 1);
        assert!(res[0].passed(), "exit 0 == expected 0 -> passed");
        assert_eq!(res[0].evidence["exit_code"], json!(0));
        assert!(res[0].evidence["output_total_chars"].is_number());
        assert_eq!(res[0].evidence["type"], json!("run_command"));
    }

    /// REGRESSIONE run 48793fde (Beaty-Book): `pnpm build` (vite) esce 0 e RIESCE,
    /// ma stampa il warning del reporter `[plugin:vite:reporter]` (import misto
    /// dinamico/statico + chunk > 500 kB). Prima il pattern `[plugin:` di
    /// count_build_errors lo contava come errore -> il criterio build bocciava un
    /// build OGGETTIVAMENTE verde (falso negativo del final_gate, gate 2/2). Ora
    /// passa: exit 0 + zero errori reali.
    #[sqlx::test]
    async fn run_command_build_passa_con_warning_reporter_vite(pool: PgPool) {
        let raw = "EXIT CODE: 0\nSTDOUT:\n\
            vite v5.4.21 building for production...\n\
            2334 modules transformed.\n\
            [plugin:vite:reporter] [plugin vite:reporter]\n\
            (!) src/app/services/bookingService.ts is dynamically imported by \
            src/app/components/admin/AppointmentsTab.tsx but also statically imported by \
            src/app/components/admin/AdminLogin.tsx, dynamic import will not move module \
            into another chunk.\n\
            (!) Some chunks are larger than 500 kB after minification.\n\
            built in 7.67s\nSTDERR:\n";
        let exec = FakeToolExecutor::with(&[("run_command", &[raw])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "pnpm build" }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert!(
            res[0].passed(),
            "build vite exit 0 con soli warning deve PASSARE, evidence: {}",
            res[0].evidence
        );
        assert_eq!(res[0].evidence["exit_code"], json!(0));
        assert_eq!(
            res[0].evidence["build_errors"],
            json!(0),
            "il warning [plugin:vite:reporter] non e' un errore"
        );
    }

    #[sqlx::test]
    async fn run_command_build_fallisce_su_exit_non_zero(pool: PgPool) {
        let exec =
            FakeToolExecutor::with(&[("run_command", &["error[E0432]\nEXIT CODE: 101\nboom"])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "cargo check" }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert!(res[0].failed(), "exit 101 != 0 -> build fallito");
        assert_eq!(res[0].evidence["exit_code"], json!(101));
        // output_excerpt presente (render_failed_block ramo build lo legge).
        assert!(res[0].evidence["output_excerpt"].is_string());
    }

    #[sqlx::test]
    async fn run_command_bootstrap_identico_alla_baseline_non_boccia(pool: PgPool) {
        // REGRESSIONE run 695794af: `npx eslint src` exit 2 per config assente
        // (fallimento di bootstrap PRE-ESISTENTE, zero file localizzati) con
        // baseline pre-lavoro identica NON deve bocciare il gate.
        let exec = FakeToolExecutor::with(&[(
            "run_command",
            &["Oops! Something went wrong!\nESLint couldn't find a config file.\nEXIT CODE: 2"],
        )]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "npx eslint src", "baseline_exit_code": 2, "touched_files": ["src/app/x.ts"] }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert!(
            res[0].passed(),
            "fallimento identico alla baseline pre-lavoro = debito ambiente, non regressione"
        );
        assert_eq!(res[0].evidence["preexisting_bootstrap"], json!(true));
        let verdict = res[0].evidence["verdict"].as_str().unwrap_or_default();
        assert!(
            verdict.contains("PRE-ESISTENTE"),
            "verdict dichiara il debito: {verdict}"
        );
    }

    #[sqlx::test]
    async fn run_command_fallimento_diverso_dalla_baseline_boccia(pool: PgPool) {
        // Exit diverso dalla baseline = comportamento NUOVO (possibile rottura
        // introdotta dal run, es. config cancellata): resta bloccante.
        let exec = FakeToolExecutor::with(&[("run_command", &["boom di bootstrap\nEXIT CODE: 2"])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "npx eslint src", "baseline_exit_code": 1 }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert!(
            res[0].failed(),
            "exit 2 con baseline 1: non identico -> boccia"
        );
        assert_eq!(res[0].evidence["preexisting_bootstrap"], json!(false));
    }

    #[sqlx::test]
    async fn run_command_baseline_con_errori_localizzati_resta_delta_su_file(pool: PgPool) {
        // Se l'output LOCALIZZA errori in file, decide il ramo delta sui
        // touched_files (qui: errore in file TOCCATO -> regressione, boccia),
        // MAI il ramo bootstrap anche con baseline identica.
        let exec = FakeToolExecutor::with(&[(
            "run_command",
            &["src/app/x.ts(3,5): error TS2304: Cannot find name 'y'.\nEXIT CODE: 2"],
        )]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "npx tsc --noEmit", "baseline_exit_code": 2, "touched_files": ["src/app/x.ts"] }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert!(
            res[0].failed(),
            "errore localizzato in file toccato = regressione: la baseline non salva"
        );
        assert_eq!(res[0].evidence["preexisting_bootstrap"], json!(false));
    }

    // ── Exit code ASSENTE: non misurato, non fallito ─────────────────────────

    #[sqlx::test]
    async fn run_command_senza_exit_code_non_e_misurato(pool: PgPool) {
        // Il confronto `actual == Some(expected)` faceva di un exit code
        // ASSENTE lo stesso `false` di uno SBAGLIATO: il gate bocciava il
        // codice per un guasto dell'esecuzione (misura sui run reali: 94
        // bocciature run_command, 11 senza exit code). L'output arriva dal
        // produttore vero (`map_result_to_outcome`), che senza la riga
        // "EXIT CODE: N" non estrae nulla — esattamente la produzione.
        let exec = FakeToolExecutor::with(&[("run_command", &["nessuno stato di uscita"])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(vec![spec(
                "run_command",
                json!({ "command": "pnpm build" }),
                json!({ "exit_code": 0 }),
            )])
            .await
            .expect("ok");
        assert!(
            res[0].inconclusive(),
            "senza exit code non c'e' misura, evidence: {}",
            res[0].evidence
        );
        assert!(!res[0].failed(), "non e' una prova di difetto");
        assert!(!res[0].passed(), "e nemmeno una prova di correttezza");
        assert_eq!(res[0].evidence["exit_code"], json!(null));
        assert_eq!(res[0].evidence["outcome"], json!("inconclusive"));
    }

    /// ToolExecutor che fallisce alla PORTA: il tool non parte affatto (runner
    /// giu', gRPC irraggiungibile). Distinto dal fake che ritorna un testo di
    /// rifiuto, dove il tool E' partito e ha detto di no.
    struct ExecutorGuasto;

    #[async_trait]
    impl ToolExecutor for ExecutorGuasto {
        async fn execute(
            &self,
            _call: ToolCall,
        ) -> Result<nexus_agent_graph::runtime::ports::ToolOutcome, PortError> {
            Err(PortError::Tool("tool runner irraggiungibile".to_string().into()))
        }
    }

    #[sqlx::test]
    async fn guasto_della_porta_non_e_una_misura_del_codice(pool: PgPool) {
        // Terzo caso, distinto dai due sopra: il tool non e' MAI partito perche'
        // e' l'infrastruttura di esecuzione a essere giu'. Non c'e' nulla da
        // rieseguire correggendo il comando e non c'e' nulla da correggere nel
        // codice: bocciare qui rimanderebbe in correzione un lavoro che nessuno
        // ha provato. E' l'unico dei tre in cui `Inconclusive` e' la risposta
        // giusta per costruzione, e va tenuto separato dal rifiuto sopra —
        // altrimenti la stessa parola coprirebbe "guasto mio" e "comando tuo".
        //
        // MUTAZIONE: facendo tornare il ramo `Err` a `CriterionOutcome::Failed`
        // l'assert su `inconclusive()` rosseggia.
        let runner = FinalGateCriteriaRunnerAdapter::new(Arc::new(ExecutorGuasto), pool, None);
        let res = runner
            .run(vec![spec(
                "run_command",
                json!({ "command": "pnpm build" }),
                json!({ "exit_code": 0 }),
            )])
            .await
            .expect("il guasto di UN criterio non fa fallire la batteria");
        assert!(
            res[0].inconclusive(),
            "guasto dell'esecutore: non e' una misura del codice, evidence: {}",
            res[0].evidence
        );
        assert!(
            !res[0].passed(),
            "e nemmeno una prova di correttezza: la chiusura resta non verificata"
        );
        assert!(res[0].evidence["error"].is_string());
    }

    #[sqlx::test]
    async fn run_command_rifiutato_prima_dell_esecuzione_boccia(pool: PgPool) {
        // "Nessun exit code" ha DUE cause che non vanno confuse: il comando e'
        // partito e non ha prodotto stato d'uscita (non misurato: il gate non
        // boccia), oppure l'invocazione e' stata RIFIUTATA da una guardia e il
        // comando non e' mai partito. Il secondo caso e' un criterio da
        // rieseguire corretto, non da assolvere: e' anche quello DOMINANTE,
        // misurato il 01/08/2026 sui criteri falliti di gestione-spese (309 su
        // 329 run_command senza exit code, di cui 237 rifiuti per working_dir
        // duplicato). Il rifiuto si riconosce dal marker di fallimento in testa
        // — segnale strutturato (regola M), non dal testo del messaggio — e il
        // fake lo traduce in `is_error` passando dal punto unico della
        // produzione, come farebbe il dispatch vero.
        //
        // MUTAZIONE: togliendo `|| outcome.is_error` da `esito_misurato` il
        // criterio torna `inconclusive`, il gate CHIUDE e il run diventa
        // `CompletedUnverified`, che `is_success()` dichiara riuscito: il
        // rifiuto verrebbe assolto invece che corretto.
        let rifiuto = nexus_types::tool_outcome::tool_failure(
            "[working_dir gia' applicato] Il comando gira GIA' dentro 'frontend'. \
             Correggi il comando: togli 'cd frontend'.",
        );
        let exec = FakeToolExecutor::with(&[("run_command", &[rifiuto.as_str()])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(vec![spec(
                "run_command",
                json!({ "command": "cd frontend && pnpm build" }),
                json!({ "exit_code": 0 }),
            )])
            .await
            .expect("ok");
        assert!(
            res[0].failed(),
            "un'invocazione rifiutata e' un criterio da rieseguire corretto, \
             non un criterio non misurabile, evidence: {}",
            res[0].evidence
        );
        assert!(
            !res[0].inconclusive(),
            "assolverlo renderebbe muto il canale con cui il gate insegna \
             all'agente come correggere il comando"
        );
        assert_eq!(res[0].evidence["exit_code"], json!(null));
    }

    #[sqlx::test]
    async fn run_command_senza_exit_code_ma_con_errori_di_build_boccia(pool: PgPool) {
        // NON e' un lasciapassare: se l'output PROVA errori di build la misura
        // c'e' lo stesso, e la rete di sicurezza sull'exit-code bugiardo resta.
        let exec = FakeToolExecutor::with(&[(
            "run_command",
            &["error: Could not resolve \"./mancante\" from src/main.ts"],
        )]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(vec![spec(
                "run_command",
                json!({ "command": "pnpm build" }),
                json!({ "exit_code": 0 }),
            )])
            .await
            .expect("ok");
        assert!(
            res[0].failed(),
            "errori di build nell'output = misura valida, evidence: {}",
            res[0].evidence
        );
    }

    // ── Gate DELTA-aware (regola H): regressione vs debito preesistente ──────────

    /// NO fail-open: un errore in un file che il task ha TOCCATO e' una
    /// regressione e fallisce il gate, anche se il resto e' preesistente.
    #[sqlx::test]
    async fn run_command_delta_regressione_in_file_toccato_fallisce(pool: PgPool) {
        let out = "src/app/pages/BookingPage.tsx(156,7): error TS2554: Expected 2 args\n\
                   src/app/pages/LoginPage.tsx(5,10): error TS2305: no member\n\
                   Found 2 errors.\nEXIT CODE: 2";
        let exec = FakeToolExecutor::with(&[("run_command", &[out])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "npx tsc --noEmit", "touched_files": ["src/app/pages/LoginPage.tsx"] }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert!(
            res[0].failed(),
            "errore in un file toccato = regressione -> fallisce (no fail-open)"
        );
        assert_eq!(res[0].evidence["delta_applied"], json!(true));
        let regressed = res[0].evidence["regressed_files"].as_array().unwrap();
        assert!(regressed
            .iter()
            .any(|v| v.as_str() == Some("src/app/pages/LoginPage.tsx")));
    }

    /// Debito preesistente: errori SOLO in file non toccati -> non bloccano la
    /// chiusura, anche con exit != 0 (il task non li ha introdotti).
    #[sqlx::test]
    async fn run_command_delta_debito_preesistente_non_blocca(pool: PgPool) {
        let out = "src/app/pages/BookingPage.tsx(156,7): error TS2554: Expected 2 args\n\
                   src/app/pages/ConfirmationPage.tsx(17,42): error TS2304: Cannot find name\n\
                   Found 2 errors.\nEXIT CODE: 2";
        let exec = FakeToolExecutor::with(&[("run_command", &[out])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "npx tsc --noEmit", "touched_files": ["src/app/services/authService.ts"] }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert!(
            res[0].passed(),
            "errori solo in file NON toccati (debito preesistente) -> non blocca"
        );
        assert_eq!(res[0].evidence["delta_applied"], json!(true));
        assert_eq!(res[0].evidence["preexisting_error_files"], json!(2));
        assert!(res[0].evidence["regressed_files"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    /// Fail-CLOSED di default: errori localizzati ma il run non dichiara file
    /// toccati -> criterio ASSOLUTO come prima (exit != 0 -> fallisce).
    #[sqlx::test]
    async fn run_command_delta_senza_file_toccati_e_failclosed(pool: PgPool) {
        let out =
            "src/app/pages/BookingPage.tsx(156,7): error TS2554: Expected 2 args\nEXIT CODE: 2";
        let exec = FakeToolExecutor::with(&[("run_command", &[out])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(
                vec![spec(
                    "run_command",
                    json!({ "command": "npx tsc --noEmit" }),
                    json!({ "exit_code": 0 }),
                )],
            )
            .await
            .expect("ok");
        assert!(
            res[0].failed(),
            "senza file toccati -> fail-closed (criterio assoluto)"
        );
        assert_eq!(res[0].evidence["delta_applied"], json!(false));
    }

    #[sqlx::test]
    async fn run_command_delega_al_tool_executor(pool: PgPool) {
        let exec = FakeToolExecutor::with(&[("run_command", &["EXIT CODE: 0"])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec.clone(), pool, None);
        runner
            .run(vec![spec(
                "run_command",
                json!({ "command": "x" }),
                json!({ "exit_code": 0 }),
            )])
            .await
            .expect("ok");
        // La chiamata e' arrivata al ToolExecutor (punto unico dell'esecuzione).
        let calls = exec.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], "run_command".to_string());
    }

    /// Status della prova: la famiglia di successo attesa viene dal PUNTO UNICO
    /// (`endpoint_probes`), non ricopiata qui — una copia divergerebbe dal
    /// criterio che il gate costruisce davvero.
    const SUCCESSI_2XX: &[i64] = nexus_agent_graph::decisions::endpoint_probes::DEFAULT_SUCCESS_STATUSES;
    const STATUS_500: &str = "500 Internal Server Error";
    const STATUS_201: &str = "201 Created";
    const CODICE_500: u16 = 500;
    /// Byte di richiesta letti prima di rispondere: basta la request-line.
    const RICHIESTA_MAX_BYTES: usize = 2048;

    /// Server HTTP minimale di test: risponde a ogni richiesta con lo status dato
    /// e chiude. Ritorna l'URL su cui e' in ascolto. Serve a esercitare il
    /// PRODUTTORE reale dell'evidence (`check_http`) invece di fabbricarla nel
    /// test (regola O): e' l'unico modo per sapere se il gate, quando boccia una
    /// chiamata, sa anche DIRE quale.
    async fn server_che_risponde(status_line: &'static str, corpo: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind effimero");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; RICHIESTA_MAX_BYTES];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{corpo}",
                    corpo.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        format!("http://{addr}/api/expenses")
    }

    /// Il caso reale (gestione-spese, 2026-07-28): la POST di creazione risponde
    /// 500. Il criterio deve FALLIRE e la sua evidence deve nominare metodo, URL
    /// e status — `final_gate::render_failed_block` salta i criteri falliti senza
    /// testo, quindi un'evidence muta significa rimandare l'agente a correggere
    /// senza dirgli cosa.
    #[sqlx::test]
    async fn http_post_500_fallisce_e_dice_cosa(pool: PgPool) {
        let url = server_che_risponde(STATUS_500, r#"{"error":"no such column: date"}"#).await;
        let exec = FakeToolExecutor::with(&[]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(vec![spec(
                "http",
                json!({ "url": url, "method": "POST", "body": {"amount": 12.5} }),
                json!({ "status": SUCCESSI_2XX }),
            )])
            .await
            .expect("ok");
        assert!(res[0].failed(), "500 fuori dagli status attesi -> criterio fallito");
        assert_eq!(res[0].evidence["status"], json!(CODICE_500));
        let verdict = res[0].evidence["verdict"]
            .as_str()
            .expect("evidence.verdict: e' il testo che il gate mostra all'agente");
        assert!(verdict.contains("POST"), "verdict senza metodo: {verdict}");
        assert!(verdict.contains("/api/expenses"), "verdict senza URL: {verdict}");
        assert!(
            verdict.contains(&CODICE_500.to_string()),
            "verdict senza status: {verdict}"
        );
        // Il corpo entra nell'evidence per la diagnosi, MAI nella decisione
        // (regola M): l'esito l'ha deciso lo status.
        assert!(res[0].evidence["body_excerpt"]
            .as_str()
            .unwrap_or("")
            .contains("no such column"));
    }

    /// Contro-prova: 201 e' nella famiglia 2xx attesa per una creazione.
    #[sqlx::test]
    async fn http_post_201_passa(pool: PgPool) {
        let url = server_che_risponde(STATUS_201, r#"{"id":1}"#).await;
        let exec = FakeToolExecutor::with(&[]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(vec![spec(
                "http",
                json!({ "url": url, "method": "POST", "body": {"amount": 12.5} }),
                json!({ "status": SUCCESSI_2XX }),
            )])
            .await
            .expect("ok");
        assert!(res[0].passed(), "201 e' un successo di creazione");
    }

    #[sqlx::test]
    async fn service_logs_clean_passa_senza_pattern_hit(pool: PgPool) {
        let exec =
            FakeToolExecutor::with(&[("run_command", &["INFO server avviato\nINFO richiesta ok"])]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(
                vec![spec(
                    "service_logs_clean",
                    json!({ "command": "docker logs app", "patterns": ["does not exist", "ERROR"] }),
                    json!({}),
                )],
            )
            .await
            .expect("ok");
        assert!(res[0].passed(), "nessun pattern -> log puliti");
    }

    #[sqlx::test]
    async fn service_logs_clean_fallisce_su_pattern_hit(pool: PgPool) {
        let exec = FakeToolExecutor::with(&[(
            "run_command",
            &["relation \"users\" does not exist\nINFO altro"],
        )]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(
                vec![spec(
                    "service_logs_clean",
                    json!({ "command": "docker logs app", "patterns": ["does not exist"] }),
                    json!({}),
                )],
            )
            .await
            .expect("ok");
        assert!(res[0].failed(), "pattern presente -> errori runtime");
        assert_eq!(res[0].evidence["error_lines"], json!(1));
        assert!(res[0].evidence["verdict"].is_string());
    }

    #[sqlx::test]
    async fn no_orphan_imported_e_inconclusive_e_non_blocca(pool: PgPool) {
        let exec = FakeToolExecutor::with(&[]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(
                vec![spec(
                    "no_orphan_imported",
                    json!({ "staging_dir": "figma_export" }),
                    json!({ "mounted": true }),
                )],
            )
            .await
            .expect("ok");
        // Non misurabile: il gate non lo conteggia ne' fra i pass ne' fra i
        // fail. Prima era `passed`, cioe' assolveva il codice per una lacuna
        // del verificatore.
        assert!(res[0].inconclusive());
        assert!(!res[0].passed(), "un criterio mai eseguito non e' un pass");
        assert_eq!(res[0].evidence["outcome"], json!("inconclusive"));
    }

    #[sqlx::test]
    async fn tool_capability_passa_se_history_ha_tool_call(pool: PgPool) {
        // Regressione: il final_gate puo' vedere tools_json vuoto/assente su
        // resume/fan-in, ma la history porta tool_use gia' osservati. Quel segnale
        // strutturato dimostra che il catalogo era disponibile nel run.
        let exec = FakeToolExecutor::with(&[]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(
                vec![spec(
                    "tool_capability",
                    json!({ "tools_count": 0, "has_tool_calls": true }),
                    json!({ "capable": true }),
                )],
            )
            .await
            .expect("ok");
        assert!(res[0].passed(), "history tool_use conferma capacita' tool");
        assert_eq!(res[0].evidence["tools_count"], json!(0));
        assert_eq!(res[0].evidence["has_tool_calls"], json!(true));
    }

    #[sqlx::test]
    async fn tool_capability_fallisce_senza_catalogo_ne_history(pool: PgPool) {
        let exec = FakeToolExecutor::with(&[]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(
                vec![spec(
                    "tool_capability",
                    json!({ "tools_count": 0, "has_tool_calls": false }),
                    json!({ "capable": true }),
                )],
            )
            .await
            .expect("ok");
        assert!(
            res[0].failed(),
            "zero catalogo e zero tool_use resta misconfigurazione"
        );
    }


    // ── file_exists: interroga il filesystem VERO, mai un listing fabbricato ──
    //
    // Il vecchio test costruiva a mano una riga di listing ("- README.md\n...")
    // che nessun produttore emette in quella forma (regola O): fissava
    // l'assunto — "list_files torna righe con prefisso `- `" — che il criterio
    // doveva verificare. Questi test scrivono file VERI su un tempdir e
    // interrogano quello, come fa `check_file_exists` in produzione.

    #[sqlx::test]
    async fn file_exists_trova_un_file_scritto_sul_disco(pool: PgPool) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("variables.txt"), "x").expect("scrittura");
        let exec = FakeToolExecutor::with(&[]);
        let runner =
            FinalGateCriteriaRunnerAdapter::new(exec, pool, Some(dir.path().to_path_buf()));
        let res = runner
            .run(vec![spec(
                "file_exists",
                json!({ "path": "variables.txt" }),
                json!({}),
            )])
            .await
            .expect("ok");
        assert!(res[0].passed(), "file presente sul disco -> esiste");
        assert_eq!(res[0].evidence["exists"], json!(true));
        assert_eq!(res[0].evidence["method"], json!("filesystem"));
    }

    #[sqlx::test]
    async fn file_exists_trova_un_dotfile_che_il_listing_nascondeva(pool: PgPool) {
        // Il vecchio criterio, basato su `list_files`, saltava le voci che
        // iniziano per punto: un `.env` scritto dal run risultava ASSENTE
        // (regola M). Sul filesystem non c'e' questa distinzione.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".env"), "SECRET=1").expect("scrittura");
        let exec = FakeToolExecutor::with(&[]);
        let runner =
            FinalGateCriteriaRunnerAdapter::new(exec, pool, Some(dir.path().to_path_buf()));
        let res = runner
            .run(vec![spec("file_exists", json!({ "path": ".env" }), json!({}))])
            .await
            .expect("ok");
        assert!(res[0].passed(), ".env scritto sul disco -> esiste, anche se un listing lo nasconde");
    }

    #[sqlx::test]
    async fn file_exists_fallisce_se_il_file_non_ce(pool: PgPool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let exec = FakeToolExecutor::with(&[]);
        let runner =
            FinalGateCriteriaRunnerAdapter::new(exec, pool, Some(dir.path().to_path_buf()));
        let res = runner
            .run(vec![spec(
                "file_exists",
                json!({ "path": "assente.txt" }),
                json!({}),
            )])
            .await
            .expect("ok");
        assert!(res[0].failed(), "file assente -> il criterio fallisce");
        assert_eq!(res[0].evidence["exists"], json!(false));
    }

    #[sqlx::test]
    async fn file_exists_e_inconclusivo_senza_radice_del_run(pool: PgPool) {
        // Sessione non mappata a un progetto: il criterio non sa DOVE guardare.
        // "non ho potuto guardare" e "non c'e'" sono risposte diverse — la prima
        // non deve far fallire il gate (regola M: mai spacciare un'assenza di
        // misura per una misura).
        let exec = FakeToolExecutor::with(&[]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(vec![spec(
                "file_exists",
                json!({ "path": "qualsiasi.txt" }),
                json!({}),
            )])
            .await
            .expect("ok");
        assert!(res[0].inconclusive(), "non misurabile, non un pass");
        assert!(!res[0].failed(), "inconclusivo -> non blocca il gate");
        assert_eq!(res[0].evidence["outcome"], json!("inconclusive"));
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn outputs_exist_na_senza_step(pool: PgPool) {
        let run = crate::test_support::seed_agent_run(&pool).await;
        let exec = FakeToolExecutor::with(&[]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(
                vec![spec(
                    "outputs_exist",
                    json!({ "run_id": run.to_string() }),
                    json!({}),
                )],
            )
            .await
            .expect("ok");
        // Nessuno step mutativo -> N/A (pass).
        assert!(res[0].passed());
        assert!(res[0].evidence["skipped"].is_string());
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn outputs_exist_fallisce_se_output_assente(pool: PgPool) {
        let run = crate::test_support::seed_agent_run(&pool).await;
        // Step write_file su "nuovo.rs": il file NON esiste sul disco -> missing.
        sqlx::query(
            "INSERT INTO agent_steps (id, run_id, step_index, tool_name, tool_input, status) \
             VALUES (gen_random_uuid(), $1, 1000, 'write_file', $2, 'completed')",
        )
        .bind(run)
        .bind(json!({ "path": "nuovo.rs" }))
        .execute(&pool)
        .await
        .expect("insert step");

        // Albero VERO del run: contiene altri file, non "nuovo.rs".
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("vecchio.rs"), "x").expect("scrittura");
        std::fs::write(dir.path().join("README.md"), "x").expect("scrittura");
        let exec = FakeToolExecutor::with(&[]);
        let runner =
            FinalGateCriteriaRunnerAdapter::new(exec, pool, Some(dir.path().to_path_buf()));
        let res = runner
            .run(
                vec![spec(
                    "outputs_exist",
                    json!({ "run_id": run.to_string() }),
                    json!({}),
                )],
            )
            .await
            .expect("ok");
        assert!(res[0].failed(), "output dichiarato assente -> fallisce");
        assert_eq!(res[0].evidence["missing"], json!(["nuovo.rs"]));
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn outputs_exist_passa_se_output_presente_sul_disco(pool: PgPool) {
        let run = crate::test_support::seed_agent_run(&pool).await;
        sqlx::query(
            "INSERT INTO agent_steps (id, run_id, step_index, tool_name, tool_input, status) \
             VALUES (gen_random_uuid(), $1, 1000, 'write_file', $2, 'completed')",
        )
        .bind(run)
        .bind(json!({ "path": "nuovo.rs" }))
        .execute(&pool)
        .await
        .expect("insert step");

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("nuovo.rs"), "x").expect("scrittura");
        let exec = FakeToolExecutor::with(&[]);
        let runner =
            FinalGateCriteriaRunnerAdapter::new(exec, pool, Some(dir.path().to_path_buf()));
        let res = runner
            .run(
                vec![spec(
                    "outputs_exist",
                    json!({ "run_id": run.to_string() }),
                    json!({}),
                )],
            )
            .await
            .expect("ok");
        assert!(res[0].passed(), "output presente sul disco -> passa");
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn outputs_exist_senza_radice_passa_ma_dichiara_inconcludente(pool: PgPool) {
        // Un output dichiarato, ma nessuna radice del run risolvibile: il
        // criterio non puo' GUARDARE, quindi non deve bocciare (fail-open, come
        // il singolo file_exists) -- ma l'evidence deve portare traccia che
        // quel path non e' stato verificato, non solo "checked". Prima di
        // questo fix `check_outputs_exist` scartava l'evidence del check
        // interno (`let (ok, _ev) = ...`): `ok=true` per "esiste" e per "non
        // verificabile" erano indistinguibili nel report.
        let run = crate::test_support::seed_agent_run(&pool).await;
        sqlx::query(
            "INSERT INTO agent_steps (id, run_id, step_index, tool_name, tool_input, status) \
             VALUES (gen_random_uuid(), $1, 1000, 'write_file', $2, 'completed')",
        )
        .bind(run)
        .bind(json!({ "path": "nuovo.rs" }))
        .execute(&pool)
        .await
        .expect("insert step");

        let exec = FakeToolExecutor::with(&[]);
        // run_root=None: nessuna radice, come una sessione senza progetto.
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(vec![spec(
                "outputs_exist",
                json!({ "run_id": run.to_string() }),
                json!({}),
            )])
            .await
            .expect("ok");
        assert!(
            !res[0].failed(),
            "non verificabile != assente: non deve bocciare il gate"
        );
        assert!(
            res[0].inconclusive(),
            "verifica incompleta: l'evidence lo diceva gia' a parole, ora lo dice l'esito"
        );
        assert_eq!(
            res[0].evidence["inconclusive"],
            json!(["nuovo.rs"]),
            "l'evidence deve dichiarare quale output NON e' stato verificato: {:?}",
            res[0].evidence
        );
    }

    #[sqlx::test]
    async fn tipo_sconosciuto_fallisce_con_error(pool: PgPool) {
        let exec = FakeToolExecutor::with(&[]);
        let runner = FinalGateCriteriaRunnerAdapter::new(exec, pool, None);
        let res = runner
            .run(
                vec![spec("inventato", json!({}), json!({}))],
            )
            .await
            .expect("ok");
        assert!(!res[0].passed());
        assert!(res[0].evidence["error"].is_string());
    }


    // ── helper puri ───────────────────────────────────────────────────────────

    #[test]
    fn truncate_chars_non_spezza_utf8() {
        assert_eq!(truncate_chars("abc", 10), "abc");
        assert_eq!(truncate_chars("abcdef", 3), "abc");
        // caratteri multibyte: taglio per char, non per byte.
        assert_eq!(truncate_chars("àèìòù", 2), "àè");
    }

    // ── criteri strutturali (ADR 0018 leva 3): tabella pass/fail ─────────────

    #[test]
    fn check_action_requested_tabella() {
        use serde_json::json;
        // (action_oriented, has_productive_action) -> passed
        let cases = [
            (false, false, true), // nessuna azione richiesta -> passa
            (false, true, true),
            (true, true, true),   // azione richiesta ED eseguita -> passa
            (true, false, false), // azione richiesta MAI eseguita -> fallisce
        ];
        for (ao, acted, want) in cases {
            let (passed, ev) = FinalGateCriteriaRunnerAdapter::check_action_requested(&json!({
                "action_oriented": ao, "has_productive_action": acted,
            }));
            assert_eq!(passed, want, "ao={ao} acted={acted}");
            assert!(ev.get("verdict").is_some());
        }
        // Fatti assenti -> default conservativo (non action_oriented): passa.
        let (passed, _) = FinalGateCriteriaRunnerAdapter::check_action_requested(&json!({}));
        assert!(passed);
    }

    #[test]
    fn check_tool_capability_tabella() {
        use serde_json::json;
        let (ok, _) =
            FinalGateCriteriaRunnerAdapter::check_tool_capability(&json!({"tools_count": 12}));
        assert!(ok);
        let (ko, ev) =
            FinalGateCriteriaRunnerAdapter::check_tool_capability(&json!({"tools_count": 0}));
        assert!(!ko);
        assert!(ev["verdict"]
            .as_str()
            .unwrap()
            .contains("misconfigurazione"));
        // Fatto assente -> 0 -> fallisce (fail-closed: e' il gate a decidere).
        let (ko2, _) = FinalGateCriteriaRunnerAdapter::check_tool_capability(&json!({}));
        assert!(!ko2);
    }

    #[test]
    fn check_completion_confirmed_tabella() {
        use serde_json::json;
        // Qualunque outcome DICHIARATO passa (anche blocked: e' onesto).
        for o in ["done", "blocked", "partial", "needs_input"] {
            let (ok, _) = FinalGateCriteriaRunnerAdapter::check_completion_confirmed(
                &json!({"declared_outcome": o}),
            );
            assert!(ok, "outcome {o} dichiarato deve passare");
        }
        // Assente o vuoto -> fallisce con invito a task_complete.
        for spec in [
            json!({}),
            json!({"declared_outcome": null}),
            json!({"declared_outcome": "  "}),
        ] {
            let (ko, ev) = FinalGateCriteriaRunnerAdapter::check_completion_confirmed(&spec);
            assert!(!ko, "spec {spec} deve fallire");
            assert!(ev["verdict"].as_str().unwrap().contains("task_complete"));
        }
        // Delega: il PADRE non dichiara ma un sub-agente e' arrivato a chiusura
        // -> passa (la dichiarazione onesta del run e' quella del figlio).
        let (ok, ev) = FinalGateCriteriaRunnerAdapter::check_completion_confirmed(
            &json!({"declared_outcome": null, "subagent_completed": true}),
        );
        assert!(ok, "completamento subagente deve confermare la chiusura");
        assert!(ev["verdict"].as_str().unwrap().contains("sub-agente"));
        // subagent_completed=false senza dichiarazione -> continua a fallire.
        let (ko, _) = FinalGateCriteriaRunnerAdapter::check_completion_confirmed(
            &json!({"declared_outcome": null, "subagent_completed": false}),
        );
        assert!(
            !ko,
            "nessuna dichiarazione e nessun subagente completato -> fallisce"
        );
        // La dichiarazione del padre prevale sul verdict del subagente.
        let (ok, ev) = FinalGateCriteriaRunnerAdapter::check_completion_confirmed(
            &json!({"declared_outcome": "done", "subagent_completed": true}),
        );
        assert!(ok);
        assert!(ev["verdict"]
            .as_str()
            .unwrap()
            .contains("esito dichiarato: done"));
    }
}

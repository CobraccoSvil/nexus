//! Creazione ATOMICA di una FIGURA (kind di sub-agente) dal pannello admin.
//!
//! Route esposte (sotto `/api/admin`, middleware `require_admin` del main.rs):
//!   * POST   /orchestrator/figures            — crea una figura completa
//!   * DELETE /orchestrator/figures/:kind      — disabilita la figura + la toglie dal CSV
//!   * POST   /orchestrator/subagents/whitelist — muta il CSV dei kind convocabili
//!
//! ## Perche' una transazione e non quattro chiamate
//!
//! Un kind di sub-agente e' VIVO solo se esistono QUATTRO pezzi coerenti:
//!   1. il prompt (`nexus_prompt_templates`, chiave `subagent.<kind>.base`);
//!   2. la selezione del modello (`nexus_purpose_model`, purpose `subagent_<kind>`);
//!   3. la definizione (`nexus_subagent_definitions`: tool, purpose, limiti);
//!   4. il kind nel CSV `settings['orchestrator.subagent_kinds_whitelist']`
//!      (Guard 1 del dispatcher: fuori dal CSV `convocable_kinds` non lo elenca e
//!      il dispatch lo rifiuta come "non in whitelist").
//!
//! Finora l'admin poteva creare UN pezzo alla volta da editor diversi, e la
//! figura restava muta senza dire perche': una definition senza prompt non
//! parte, una senza purpose non ha modello, una fuori dal CSV non e' nemmeno
//! convocabile. Qui i quattro pezzi nascono o falliscono INSIEME (`begin`/
//! `commit`): un fallimento a meta' lascerebbe esattamente la figura monca che
//! questo endpoint esiste per eliminare.
//!
//! ## Perche' NON delega al PUT purpose-model di mcp-core
//!
//! La riga di purpose e' TIER-ONLY: `provider = ''`, `model_id = ''`, e il
//! modello concreto lo sceglie sempre `best_model_for_tier` dal catalog
//! (capability + cooldown aware). L'endpoint `PUT /purpose-model/:purpose`
//! (mcp-core, `admin/routing.rs`) RIFIUTA con 400 provider/model_id vuoti: e'
//! costruito per le righe statiche storiche, non per il tier-only. Delegargli
//! questo pezzo significherebbe scrivere un modello a nome — cioe' la cosa che
//! la regola G vieta. La riga la scrive quindi questa transazione.
//!
//! ## Errori
//!
//! Ogni rifiuto e' STRUTTURATO (regola M): `{error, code, field}` + eventuali
//! dettagli macchina (`missing_sections`, `offending_tools`). `error` resta la
//! stringa umana che `fetchJson` (lib/api/_shared.ts) legge per il messaggio;
//! `code` e' l'identificatore canonico su cui il wizard puo' ramificare senza
//! parsare la prosa.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, Transaction};
use std::sync::OnceLock;

use nexus_types::{tiers::is_performance_tier, ApiError, ApiResult};

use crate::AppState;

// ── Contratto canonico (regola N: gli identificatori li deriva il server) ────

/// Setting CSV dei kind convocabili — Guard 1 del dispatcher
/// (`convocable_kinds`, mcp-core/agent_tools/subagent_native.rs).
const KINDS_WHITELIST_KEY: &str = "orchestrator.subagent_kinds_whitelist";

/// Setting CSV dei tool che MUTANO filesystem/progetto (mig 0394). E' il punto
/// unico dei DATI "tool mutativo": lo stesso che alimenta
/// `nexus_agent_graph::decisions::hitl::is_mutator_tool_name` (gate HITL) e la
/// tool_result_cache. admin-service non puo' importare il crate del grafo (si
/// tirerebbe dietro l'intero motore agentico per una lista di stringhe), quindi
/// legge la LISTA dalla stessa chiave e applica lo stesso identico test
/// (appartenenza esatta al nome). La lista NON e' duplicata: sta nel DB.
const MUTATOR_TOOLS_KEY: &str = "agent.tools.result_cache_mutators";

/// Tool di chiusura strutturata di una figura advisory (gemello di
/// `review_verdict`/`debate_position`): senza, la figura parlerebbe in prosa e
/// nessun aggregatore potrebbe contarne il verdetto.
use nexus_types::figure_advisory::ADVISORY_VERDICT_TOOL;

/// Categoria dei prompt di sub-agente in `nexus_prompt_templates`
/// (CHECK mig 0035: system|quality|automation). Le figure seedate (0546/0554/
/// 0605) stanno tutte in `automation`.
const PROMPT_CATEGORY: &str = "automation";

/// Provenienza della riga di prompt creata dal wizard.
const UPDATED_BY: &str = "admin_figure_wizard";

/// Sezioni obbligatorie dello schema XML dei prompt di figura (CLAUDE.md sez. D,
/// come le figure seedate in mig 0546/0605). Un prompt senza `<lente>` produce
/// una figura che guarda la richiesta come tutte le altre: la lente E' la figura.
pub const REQUIRED_PROMPT_SECTIONS: [&str; 7] = [
    "role",
    "contesto",
    "lente",
    "autonomia",
    "principi_nexus",
    "anti_loop",
    "output_format",
];

/// Estremi ammessi per `max_iterations` (colonna `nexus_subagent_definitions`).
const MAX_ITERATIONS_RANGE: (i32, i32) = (1, 100);
/// Estremi ammessi per `timeout_s` (colonna `nexus_subagent_definitions`).
const TIMEOUT_S_RANGE: (i32, i32) = (30, 3600);

/// Chiave del prompt della figura: derivata, MAI digitata dall'utente (regola N).
pub fn prompt_key_for(kind: &str) -> String {
    format!("subagent.{kind}.base")
}

/// Purpose di selezione modello della figura: derivato, MAI digitato (regola N).
/// NB: le figure SEEDATE hanno purpose storici diversi (`council_*`,
/// `debate_advocate`): sono l'eccezione documentata delle migrazioni, non una
/// seconda convenzione da imitare.
pub fn purpose_for(kind: &str) -> String {
    format!("subagent_{kind}")
}

/// `^[a-z][a-z0-9_]{1,63}$`: il kind e' un identificatore canonico (regola N) e
/// finisce in una chiave di prompt, in un purpose e in un CSV separato da
/// virgole — spazi, maiuscole e virgole lo romperebbero in silenzio.
fn kind_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z][a-z0-9_]{1,63}$").expect("regex kind valida"))
}

// ── Errore strutturato ──────────────────────────────────────────────────────

/// Rifiuto STRUTTURATO (regola M): il chiamante decide su `code`, non sul testo.
#[derive(Debug, Clone, PartialEq)]
pub struct FigureError {
    pub status: StatusCode,
    /// Identificatore canonico e stabile del rifiuto (regola N).
    pub code: &'static str,
    /// Campo del payload responsabile, se ce n'e' uno.
    pub field: Option<&'static str>,
    /// Messaggio umano: SOLO per display (`fetchJson` legge `payload.error`).
    pub message: String,
    /// Dettagli macchina (es. i tag mancanti, i tool offendenti). `Box` perche'
    /// un `Value` inline gonfierebbe ogni `Result` dei validatori (clippy
    /// `result_large_err`): i dettagli sono l'eccezione, non il caso comune.
    pub details: Option<Box<Value>>,
}

impl FigureError {
    fn new(
        status: StatusCode,
        code: &'static str,
        field: Option<&'static str>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status,
            code,
            field,
            message: message.into(),
            details: None,
        }
    }

    fn bad_request(code: &'static str, field: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, Some(field), message)
    }

    fn conflict(code: &'static str, field: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, Some(field), message)
    }

    fn with_details(mut self, details: Value) -> Self {
        self.details = Some(Box::new(details));
        self
    }

    /// Payload JSON: `error` (umano) + `code`/`field` (macchina) + dettagli.
    pub fn into_api(self) -> ApiError {
        let mut payload = json!({
            "error": self.message,
            "code": self.code,
            "field": self.field,
        });
        if let (Some(details), Some(obj)) = (self.details, payload.as_object_mut()) {
            if let Value::Object(extra) = *details {
                for (k, v) in extra {
                    obj.insert(k, v);
                }
            }
        }
        (self.status, Json(payload))
    }
}

/// Errore su una INSERT della figura: una violazione di UNICITA' e' un
/// CONFLITTO (409), non un guasto (500).
///
/// Serve alla corsa fra due creazioni dello stesso kind: i pre-check leggono in
/// READ COMMITTED, quindi due transazioni concorrenti li superano entrambe e a
/// fermare la seconda e' il vincolo, non il controllo. Senza questa mappatura
/// il secondo admin vedrebbe un 500 "operazione DB fallita" al posto del "esiste
/// gia'" che descrive davvero cos'e' successo.
///
/// Il segnale e' STRUTTURATO (`is_unique_violation`, regola M): mai il parsing
/// del messaggio Postgres, che cambia con la locale e con la versione.
fn insert_error(
    err: sqlx::Error,
    what: &str,
    code: &'static str,
    message: impl Into<String>,
) -> ApiError {
    if let sqlx::Error::Database(ref db) = err {
        if db.is_unique_violation() {
            return FigureError::conflict(code, "kind", message).into_api();
        }
    }
    db_error(err, what)
}

/// Errore DB: propagato (regola H), mai ingoiato. Il messaggio sqlx resta nel
/// log; al client va un codice stabile.
fn db_error(err: sqlx::Error, what: &str) -> ApiError {
    tracing::error!(error = %err, operation = what, "figures: operazione DB fallita");
    FigureError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "db_error",
        None,
        format!("Operazione DB fallita ({what})"),
    )
    .into_api()
}

// ── Validatori PURI ─────────────────────────────────────────────────────────

/// Richiesta NORMALIZZATA (trim, lowercase del tier, tool deduplicati): ogni
/// validatore e il SQL vedono gli stessi identici byte.
#[derive(Debug, Clone, PartialEq)]
pub struct FigureRequest {
    pub kind: String,
    pub description: String,
    pub advisory: bool,
    pub tier: String,
    pub prompt_content: String,
    pub prompt_title: String,
    pub tool_whitelist: Vec<String>,
    pub max_iterations: Option<i32>,
    pub timeout_s: Option<i32>,
}

/// Spezza un CSV di `settings` (whitelist kind, lista mutatori): trim + scarto
/// dei vuoti, come `apply_subagent_setting` in mcp-core.
pub fn split_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Tag di sezione obbligatori ASSENTI dal prompt. Una sezione e' presente solo
/// se ha apertura E chiusura: `<lente>` citato nella prosa non e' una lente.
/// Confronto case-insensitive.
pub fn missing_prompt_sections(content: &str) -> Vec<&'static str> {
    let lower = content.to_ascii_lowercase();
    REQUIRED_PROMPT_SECTIONS
        .iter()
        .copied()
        .filter(|tag| {
            !(lower.contains(&format!("<{tag}>")) && lower.contains(&format!("</{tag}>")))
        })
        .collect()
}

/// Tool della whitelist che MUTANO stato, dato l'elenco autoritativo letto dal
/// setting `agent.tools.result_cache_mutators`. Stesso test di appartenenza di
/// `hitl::is_mutator_tool_name`; la LISTA e' la stessa riga di DB.
///
/// Re-export dal punto unico (regola L): la stessa domanda la pone anche la
/// CONVOCAZIONE delle figure del Consiglio, e due copie divergerebbero — e' gia'
/// successo, con l'esito descritto in [`nexus_types::figure_advisory`].
pub use nexus_types::figure_advisory::mutator_tools_in;

/// Tutti i rifiuti che NON richiedono il DB, in ordine deterministico.
/// `mutator_tools` arriva dal chiamante (regola G: la config e' un parametro,
/// cosi' la funzione resta pura e testabile).
/// Contratto della figura ADVISORY: dichiara il verdetto sul proprio canale e
/// non muta lo stato. Sono due facce della stessa promessa ("analizza, non
/// esegue"), percio' condividono il codice d'errore.
/// I due rifiuti restano separati per dire al wizard COSA manca, ma il criterio
/// e' quello del punto unico: `is_advisory_kind` risponde si' esattamente quando
/// nessuno dei due rami qui sotto scatta.
fn validate_advisory_readonly(
    req: &FigureRequest,
    mutator_tools: &[String],
) -> Result<(), FigureError> {
    if !req
        .tool_whitelist
        .iter()
        .any(|t| t == ADVISORY_VERDICT_TOOL)
    {
        return Err(FigureError::bad_request(
            "advisory_not_readonly",
            "tool_whitelist",
            format!(
                "Una figura advisory deve avere '{ADVISORY_VERDICT_TOOL}' in whitelist: \
                 e' il canale strutturato con cui il suo verdetto viene contato."
            ),
        )
        .with_details(json!({ "missing_tools": [ADVISORY_VERDICT_TOOL] })));
    }
    let offending = mutator_tools_in(&req.tool_whitelist, mutator_tools);
    if !offending.is_empty() {
        return Err(FigureError::bad_request(
            "advisory_not_readonly",
            "tool_whitelist",
            format!(
                "Una figura advisory analizza e non muta lo stato, ma questi tool \
                 scrivono: {}.",
                offending.join(", ")
            ),
        )
        .with_details(json!({ "offending_tools": offending })));
    }
    Ok(())
}

/// Range dei parametri di esecuzione: fuori scala non sono "valori strani", sono
/// figure che non concluderebbero mai o che morirebbero prima di parlare.
fn validate_ranges(req: &FigureRequest) -> Result<(), FigureError> {
    if let Some(v) = req.max_iterations {
        let (min, max) = MAX_ITERATIONS_RANGE;
        if v < min || v > max {
            return Err(FigureError::bad_request(
                "out_of_range",
                "max_iterations",
                format!("max_iterations deve stare tra {min} e {max} (ricevuto {v})."),
            ));
        }
    }
    if let Some(v) = req.timeout_s {
        let (min, max) = TIMEOUT_S_RANGE;
        if v < min || v > max {
            return Err(FigureError::bad_request(
                "out_of_range",
                "timeout_s",
                format!("timeout_s deve stare tra {min} e {max} (ricevuto {v})."),
            ));
        }
    }
    Ok(())
}

pub fn validate_figure(req: &FigureRequest, mutator_tools: &[String]) -> Result<(), FigureError> {
    if !kind_regex().is_match(&req.kind) {
        return Err(FigureError::bad_request(
            "invalid_kind",
            "kind",
            format!(
                "Il kind '{}' non e' un identificatore valido: minuscole, cifre e underscore, \
                 iniziale alfabetica, da 2 a 64 caratteri (es. 'data_engineer').",
                req.kind
            ),
        ));
    }

    if req.description.is_empty() {
        return Err(FigureError::bad_request(
            "empty_description",
            "description",
            "La descrizione e' obbligatoria: e' cio' che il coordinatore legge per \
             decidere quando convocare la figura.",
        ));
    }

    if !is_performance_tier(&req.tier) {
        return Err(FigureError::bad_request(
            "invalid_tier",
            "tier",
            format!(
                "Tier '{}' non valido: usa uno di light|medium|high|heavy|frontier.",
                req.tier
            ),
        ));
    }

    let missing = missing_prompt_sections(&req.prompt_content);
    if !missing.is_empty() {
        return Err(FigureError::bad_request(
            "prompt_missing_sections",
            "prompt_content",
            format!(
                "Il prompt non ha le sezioni obbligatorie dello schema XML: {}.",
                missing
                    .iter()
                    .map(|t| format!("<{t}>"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
        .with_details(json!({ "missing_sections": missing })));
    }

    if req.tool_whitelist.is_empty() {
        return Err(FigureError::bad_request(
            "empty_tool_whitelist",
            "tool_whitelist",
            "La figura deve avere almeno un tool: senza whitelist non puo' fare nulla.",
        ));
    }

    if req.advisory {
        validate_advisory_readonly(req, mutator_tools)?;
    }
    validate_ranges(req)?;
    Ok(())
}

// ── Payload ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateFigureBody {
    pub kind: String,
    #[serde(default)]
    pub description: String,
    /// `true` = figura di ANALISI read-only (chiude con `advisory_verdict`);
    /// `false` = figura esecutiva (puo' avere tool di scrittura).
    #[serde(default)]
    pub advisory: bool,
    /// Tier di capacita': l'UNICA leva di selezione del modello (regola G).
    pub tier: String,
    pub prompt_content: String,
    #[serde(default)]
    pub prompt_title: Option<String>,
    #[serde(default)]
    pub tool_whitelist: Vec<String>,
    #[serde(default)]
    pub max_iterations: Option<i32>,
    #[serde(default)]
    pub timeout_s: Option<i32>,
}

/// Normalizzazione: trim ovunque, tier in minuscolo (la CHECK di
/// `nexus_purpose_model.tier`, mig 0547, vuole il letterale esatto), tool
/// deduplicati preservando l'ordine. Titolo assente -> derivato dal kind.
pub fn normalize(body: CreateFigureBody) -> FigureRequest {
    let kind = body.kind.trim().to_string();
    let mut tool_whitelist: Vec<String> = Vec::new();
    for t in body.tool_whitelist {
        let t = t.trim().to_string();
        if !t.is_empty() && !tool_whitelist.contains(&t) {
            tool_whitelist.push(t);
        }
    }
    let prompt_title = body
        .prompt_title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| format!("Figura: {kind}"));
    FigureRequest {
        kind,
        description: body.description.trim().to_string(),
        advisory: body.advisory,
        tier: body.tier.trim().to_ascii_lowercase(),
        prompt_content: body.prompt_content.trim().to_string(),
        prompt_title,
        tool_whitelist,
        max_iterations: body.max_iterations,
        timeout_s: body.timeout_s,
    }
}

// ── Lettura config ──────────────────────────────────────────────────────────

/// Elenco autoritativo dei tool mutativi. Assente/illeggibile -> errore VISIBILE
/// (regola G: niente lista di ripiego nel codice — con un default inventato il
/// contratto read-only delle figure advisory sarebbe verificato contro la lista
/// sbagliata, cioe' non verificato affatto).
async fn read_mutator_tools(db: &PgPool) -> Result<Vec<String>, ApiError> {
    let raw = nexus_auth::get_setting_nonempty(db, MUTATOR_TOOLS_KEY)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "figures: lettura {MUTATOR_TOOLS_KEY} fallita");
            FigureError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "settings_read_failed",
                None,
                format!("Lettura del setting '{MUTATOR_TOOLS_KEY}' fallita."),
            )
            .into_api()
        })?;
    let Some(raw) = raw else {
        return Err(FigureError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "mutator_tools_setting_missing",
            None,
            format!(
                "Il setting '{MUTATOR_TOOLS_KEY}' e' assente o vuoto: senza l'elenco dei \
                 tool mutativi il contratto read-only delle figure advisory non e' \
                 verificabile. Applica la migrazione 0394."
            ),
        )
        .into_api());
    };
    Ok(split_csv(&raw))
}

// ── Punto unico del CSV della whitelist ─────────────────────────────────────

/// UNICA sede che sa mutare `settings['orchestrator.subagent_kinds_whitelist']`
/// (regola L): aggiunge `add`, toglie `remove`, ritorna il CSV risultante.
///
/// Idempotente per costruzione — e' il pattern delle migrazioni 0546/0605:
/// split del CSV, append, `DISTINCT`, `string_agg ... ORDER BY`. Riaggiungere un
/// kind presente non lo duplica; toglierne uno assente non e' un errore; l'ordine
/// e' deterministico. Tutto in UN `UPDATE` (nessun read-modify-write lato Rust:
/// due admin che aggiungono due kind insieme si sovrascriverebbero a vicenda).
///
/// La riga di setting DEVE esistere (mig 0153): se manca, il chiamante riceve un
/// errore esplicito invece di una figura silenziosamente non convocabile.
pub async fn mutate_kinds_whitelist(
    tx: &mut Transaction<'_, Postgres>,
    add: &[String],
    remove: &[String],
) -> Result<Vec<String>, ApiError> {
    let updated: Option<String> = sqlx::query_scalar(
        r#"UPDATE settings
              SET value = COALESCE((
                      SELECT string_agg(k, ',' ORDER BY k)
                        FROM (
                            SELECT DISTINCT trim(x) AS k
                              FROM unnest(
                                  string_to_array(COALESCE(value, ''), ',') || $1::text[]
                              ) AS x
                             WHERE trim(x) <> ''
                               AND NOT (trim(x) = ANY($2::text[]))
                        ) t
                  ), ''),
                  updated_at = NOW()
            WHERE key = $3
        RETURNING value"#,
    )
    .bind(add)
    .bind(remove)
    .bind(KINDS_WHITELIST_KEY)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| db_error(e, "mutate_kinds_whitelist"))?;

    let Some(csv) = updated else {
        return Err(FigureError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "whitelist_setting_missing",
            None,
            format!(
                "Il setting '{KINDS_WHITELIST_KEY}' non esiste: senza, nessun kind e' \
                 convocabile dal dispatcher. Applica la migrazione 0153."
            ),
        )
        .into_api());
    };
    Ok(split_csv(&csv))
}

// ── POST /orchestrator/figures ──────────────────────────────────────────────

/// Pre-check dei conflitti sui 3 pezzi referenziati, DENTRO la transazione: il
/// wizard CREA, non sovrascrive — un upsert silenzioso riscriverebbe il prompt
/// di una figura viva credendo di crearne una nuova. L'uscita anticipata droppa
/// la `tx` = rollback, zero righe scritte.
///
/// NB: fra il check e l'INSERT c'e' una finestra (READ COMMITTED): a fermare due
/// creazioni simultanee dello stesso kind e' il VINCOLO di unicita', mappato a
/// 409 dal segnale strutturato di sqlx. Questi check servono a dare un errore
/// COMPRENSIBILE nel caso normale, non a garantire l'unicita'.
async fn ensure_no_conflicts(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    kind: &str,
    prompt_key: &str,
    purpose: &str,
) -> Result<(), (StatusCode, Json<Value>)> {
    let existing_kind: Option<bool> =
        sqlx::query_scalar("SELECT is_enabled FROM nexus_subagent_definitions WHERE kind = $1")
            .bind(kind)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| db_error(e, "select definition"))?;
    if let Some(is_enabled) = existing_kind {
        let message = if is_enabled {
            format!("La figura '{kind}' esiste gia'.")
        } else {
            format!(
                "La figura '{kind}' esiste gia' ma e' disabilitata: riabilitala dall'editor \
                 delle definizioni invece di ricrearla (prompt e cronologia sono ancora li')."
            )
        };
        return Err(FigureError::conflict("kind_exists", "kind", message)
            .with_details(json!({ "is_enabled": is_enabled }))
            .into_api());
    }

    let prompt_exists: Option<i32> =
        sqlx::query_scalar("SELECT 1 FROM nexus_prompt_templates WHERE key = $1")
            .bind(prompt_key)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| db_error(e, "select prompt"))?;
    if prompt_exists.is_some() {
        return Err(FigureError::conflict(
            "prompt_key_exists",
            "kind",
            format!(
                "Esiste gia' un prompt con chiave '{prompt_key}' (residuo di una figura \
                 rimossa?): rinomina il kind oppure elimina il prompt dall'editor."
            ),
        )
        .with_details(json!({ "prompt_key": prompt_key }))
        .into_api());
    }

    let purpose_exists: Option<i32> =
        sqlx::query_scalar("SELECT 1 FROM nexus_purpose_model WHERE purpose = $1")
            .bind(purpose)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| db_error(e, "select purpose"))?;
    if purpose_exists.is_some() {
        return Err(FigureError::conflict(
            "purpose_exists",
            "kind",
            format!(
                "Esiste gia' un purpose '{purpose}' (residuo di una figura rimossa?): \
                 rinomina il kind oppure rimuovi la riga dal registry dei purpose."
            ),
        )
        .with_details(json!({ "purpose": purpose }))
        .into_api());
    }
    Ok(())
}

pub async fn create_figure(
    State(state): State<AppState>,
    Json(body): Json<CreateFigureBody>,
) -> ApiResult {
    let mutator_tools = read_mutator_tools(&state.db).await?;
    let req = normalize(body);
    validate_figure(&req, &mutator_tools).map_err(FigureError::into_api)?;

    let prompt_key = prompt_key_for(&req.kind);
    let purpose = purpose_for(&req.kind);

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| db_error(e, "begin transaction"))?;

    // (0) Conflitti: il wizard CREA, non sovrascrive (vedi `ensure_no_conflicts`).
    ensure_no_conflicts(&mut tx, &req.kind, &prompt_key, &purpose).await?;

    // (1) Prompt della figura.
    sqlx::query(
        r#"INSERT INTO nexus_prompt_templates
               (key, category, title, content, is_active, version, updated_by, updated_at)
           VALUES ($1, $2, $3, $4, true, 1, $5, NOW())"#,
    )
    .bind(&prompt_key)
    .bind(PROMPT_CATEGORY)
    .bind(&req.prompt_title)
    .bind(&req.prompt_content)
    .bind(UPDATED_BY)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        insert_error(
            e,
            "insert prompt",
            "prompt_key_exists",
            format!("Esiste gia' un prompt con chiave '{prompt_key}'."),
        )
    })?;

    // (2) Selezione modello TIER-ONLY: provider/model_id VUOTI di proposito
    //     (regola G + direttiva utente). Il modello concreto lo sceglie
    //     `best_model_for_tier` dal catalog a ogni convocazione, cosi' la figura
    //     segue il catalog invece di fossilizzare il nome del modello di oggi.
    //     `requires_tool_use = true` non e' un default inventato: e' derivato
    //     dalla whitelist non vuota gia' validata (un modello senza tool_use non
    //     potrebbe eseguire un solo tool della figura). `required_capability`
    //     resta NULL: il payload non la dichiara e imporre 'reasoning' qui
    //     restringerebbe il catalog di nascosto.
    sqlx::query(
        r#"INSERT INTO nexus_purpose_model
               (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
           VALUES ($1, '', '', $2, NULL, true, $3)"#,
    )
    .bind(&purpose)
    .bind(&req.tier)
    .bind(format!(
        "Figura '{}' creata dal pannello admin. Tier-only: nessun modello statico.",
        req.kind
    ))
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        insert_error(
            e,
            "insert purpose",
            "purpose_exists",
            format!("Esiste gia' un purpose '{purpose}'."),
        )
    })?;

    // (3) Definizione. max_iterations/timeout_s omessi -> restano i DEFAULT
    //     della colonna (mig 0151): il default vive nello schema e non viene
    //     ri-dichiarato qui (una seconda copia divergerebbe al primo ALTER).
    sqlx::query(
        r#"INSERT INTO nexus_subagent_definitions
               (kind, description, prompt_key, tool_whitelist, model_purpose,
                is_background, is_enabled, updated_at)
           VALUES ($1, $2, $3, $4, $5, false, true, NOW())"#,
    )
    .bind(&req.kind)
    .bind(&req.description)
    .bind(&prompt_key)
    .bind(&req.tool_whitelist)
    .bind(&purpose)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        insert_error(
            e,
            "insert definition",
            "kind_exists",
            format!("La figura '{}' esiste gia'.", req.kind),
        )
    })?;

    if req.max_iterations.is_some() || req.timeout_s.is_some() {
        sqlx::query(
            r#"UPDATE nexus_subagent_definitions
                  SET max_iterations = COALESCE($2, max_iterations),
                      timeout_s      = COALESCE($3, timeout_s)
                WHERE kind = $1"#,
        )
        .bind(&req.kind)
        .bind(req.max_iterations)
        .bind(req.timeout_s)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_error(e, "update definition limits"))?;
    }

    // (4) Guard 1: senza il kind nel CSV la figura esiste e resta muta.
    let whitelist = mutate_kinds_whitelist(&mut tx, std::slice::from_ref(&req.kind), &[]).await?;

    tx.commit().await.map_err(|e| db_error(e, "commit"))?;

    tracing::info!(
        kind = %req.kind,
        tier = %req.tier,
        advisory = req.advisory,
        "figures: figura creata (prompt + purpose tier-only + definition + whitelist)"
    );

    Ok(Json(json!({
        "ok": true,
        "kind": req.kind,
        "promptKey": prompt_key,
        "purpose": purpose,
        "tier": req.tier,
        "whitelist": whitelist,
    })))
}

// ── DELETE /orchestrator/figures/:kind ──────────────────────────────────────

/// Ritiro di una figura: soft-delete della definition (`is_enabled = false`,
/// coerente con la DELETE storica del pannello) + rimozione dal CSV, in UNA
/// transazione. Prompt e purpose RESTANO: sono storici e innocui senza una
/// definition abilitata, e conservarli rende reversibile il ritiro.
pub async fn delete_figure(State(state): State<AppState>, Path(kind): Path<String>) -> ApiResult {
    let kind = kind.trim().to_string();
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| db_error(e, "begin transaction"))?;

    let disabled: Option<String> = sqlx::query_scalar(
        "UPDATE nexus_subagent_definitions SET is_enabled = false, updated_at = NOW() \
         WHERE kind = $1 RETURNING kind",
    )
    .bind(&kind)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| db_error(e, "disable definition"))?;

    if disabled.is_none() {
        return Err(FigureError::new(
            StatusCode::NOT_FOUND,
            "kind_not_found",
            Some("kind"),
            format!("Nessuna figura '{kind}'."),
        )
        .into_api());
    }

    let whitelist = mutate_kinds_whitelist(&mut tx, &[], std::slice::from_ref(&kind)).await?;

    tx.commit().await.map_err(|e| db_error(e, "commit"))?;

    tracing::info!(kind = %kind, "figures: figura ritirata (soft-delete + fuori whitelist)");

    Ok(Json(json!({
        "ok": true,
        "kind": kind,
        "softDeleted": true,
        "whitelist": whitelist,
    })))
}

// ── POST /orchestrator/subagents/whitelist ──────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct WhitelistBody {
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
}

/// Riparazione del CSV dei kind convocabili: serve alla UI quando una figura
/// esiste ma e' fuori whitelist (il badge del pannello consiglio). Delega allo
/// stesso `mutate_kinds_whitelist` della creazione: una sola sede sa mutare il CSV.
///
/// `add` accetta solo kind canonici (un identificatore malformato nel CSV
/// sarebbe spazzatura invisibile); `remove` accetta QUALUNQUE stringa, perche'
/// deve poter ripulire proprio le voci malformate gia' presenti.
pub async fn update_kinds_whitelist(
    State(state): State<AppState>,
    Json(body): Json<WhitelistBody>,
) -> ApiResult {
    let add: Vec<String> = body.add.iter().map(|k| k.trim().to_string()).collect();
    let remove: Vec<String> = body.remove.iter().map(|k| k.trim().to_string()).collect();

    if add.is_empty() && remove.is_empty() {
        return Err(FigureError::bad_request(
            "empty_mutation",
            "add",
            "Nessuna modifica richiesta: valorizza 'add' oppure 'remove'.",
        )
        .into_api());
    }
    for k in &add {
        if !kind_regex().is_match(k) {
            return Err(FigureError::bad_request(
                "invalid_kind",
                "add",
                format!("Il kind '{k}' non e' un identificatore valido."),
            )
            .into_api());
        }
    }

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| db_error(e, "begin transaction"))?;
    let whitelist = mutate_kinds_whitelist(&mut tx, &add, &remove).await?;
    tx.commit().await.map_err(|e| db_error(e, "commit"))?;

    tracing::info!(
        added = add.len(),
        removed = remove.len(),
        "figures: whitelist kind aggiornata"
    );

    Ok(Json(json!({ "ok": true, "whitelist": whitelist })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mutators() -> Vec<String> {
        // Sottoinsieme reale del setting 0394 (qui e' un PARAMETRO, non una
        // copia della lista: in produzione arriva dal DB).
        ["write_file", "edit_file", "run_command"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn prompt_completo() -> String {
        REQUIRED_PROMPT_SECTIONS
            .iter()
            .map(|t| format!("<{t}>contenuto</{t}>"))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn richiesta_valida() -> FigureRequest {
        FigureRequest {
            kind: "data_engineer".into(),
            description: "Figura di analisi dei dati.".into(),
            advisory: true,
            tier: "heavy".into(),
            prompt_content: prompt_completo(),
            prompt_title: "Figura: data_engineer".into(),
            tool_whitelist: vec!["read_file".into(), "advisory_verdict".into()],
            max_iterations: Some(12),
            timeout_s: Some(240),
        }
    }

    #[test]
    fn derivazioni_canoniche_dal_kind() {
        assert_eq!(
            prompt_key_for("data_engineer"),
            "subagent.data_engineer.base"
        );
        assert_eq!(purpose_for("data_engineer"), "subagent_data_engineer");
    }

    #[test]
    fn richiesta_valida_passa() {
        assert_eq!(validate_figure(&richiesta_valida(), &mutators()), Ok(()));
    }

    // ── kind ────────────────────────────────────────────────────────────────

    #[test]
    fn kind_accetta_solo_identificatori_canonici() {
        for buono in ["ab", "data_engineer", "x1", "a_9_b", &"a".repeat(64)] {
            let mut r = richiesta_valida();
            r.kind = buono.to_string();
            assert_eq!(
                validate_figure(&r, &mutators()),
                Ok(()),
                "kind '{buono}' doveva passare"
            );
        }
        // Maiuscole, spazi, trattini, virgole (romperebbero il CSV), iniziale
        // non alfabetica, troppo corto, troppo lungo.
        for cattivo in [
            "",
            "a",
            "Data_Engineer",
            "data engineer",
            "data-engineer",
            "data,engineer",
            "1data",
            "_data",
            "data.engineer",
            &"a".repeat(65),
        ] {
            let mut r = richiesta_valida();
            r.kind = cattivo.to_string();
            let err = validate_figure(&r, &mutators()).expect_err("doveva fallire");
            assert_eq!(err.code, "invalid_kind", "kind '{cattivo}'");
            assert_eq!(err.status, StatusCode::BAD_REQUEST);
            assert_eq!(err.field, Some("kind"));
        }
    }

    // ── sezioni del prompt ──────────────────────────────────────────────────

    #[test]
    fn prompt_completo_non_ha_sezioni_mancanti() {
        assert!(missing_prompt_sections(&prompt_completo()).is_empty());
    }

    #[test]
    fn prompt_senza_lente_elenca_la_sezione_mancante() {
        let content = prompt_completo().replace("<lente>contenuto</lente>", "");
        let mut r = richiesta_valida();
        r.prompt_content = content;
        let err = validate_figure(&r, &mutators()).expect_err("prompt senza lente rifiutato");
        assert_eq!(err.code, "prompt_missing_sections");
        assert_eq!(err.field, Some("prompt_content"));
        assert_eq!(
            err.details.as_deref(),
            Some(&json!({ "missing_sections": ["lente"] })),
            "il chiamante deve sapere QUALE sezione manca, non solo che manca"
        );
    }

    #[test]
    fn prompt_vuoto_elenca_tutte_le_sezioni() {
        let missing = missing_prompt_sections("");
        assert_eq!(missing.len(), REQUIRED_PROMPT_SECTIONS.len());
    }

    #[test]
    fn sezione_senza_chiusura_non_conta() {
        // `<lente>` citato nella prosa (o aperto e mai chiuso) non e' una sezione.
        let content = prompt_completo().replace("</lente>", "");
        assert_eq!(missing_prompt_sections(&content), vec!["lente"]);
    }

    #[test]
    fn tag_case_insensitive() {
        let content = prompt_completo()
            .replace("<lente>", "<LENTE>")
            .replace("</lente>", "</Lente>");
        assert!(missing_prompt_sections(&content).is_empty());
    }

    // ── tier ────────────────────────────────────────────────────────────────

    #[test]
    fn tier_valido_solo_sul_vocabolario_unico() {
        for t in nexus_types::tiers::PERFORMANCE_TIERS {
            let mut r = richiesta_valida();
            r.tier = t.to_string();
            assert_eq!(validate_figure(&r, &mutators()), Ok(()), "tier '{t}'");
        }
        // 'low' e' una classe di complessita', non un performance_tier; 'static'
        // e' il marcatore della selezione statica; entrambi vanno rifiutati.
        for t in ["", "low", "static", "ultra", "fast"] {
            let mut r = richiesta_valida();
            r.tier = t.to_string();
            let Err(err) = validate_figure(&r, &mutators()) else {
                panic!("il tier '{t}' doveva essere rifiutato");
            };
            assert_eq!(err.code, "invalid_tier");
            assert_eq!(err.field, Some("tier"));
        }
    }

    // ── whitelist tool / contratto advisory ─────────────────────────────────

    #[test]
    fn whitelist_vuota_rifiutata() {
        let mut r = richiesta_valida();
        r.tool_whitelist = vec![];
        let err = validate_figure(&r, &mutators()).expect_err("whitelist vuota rifiutata");
        assert_eq!(err.code, "empty_tool_whitelist");
    }

    #[test]
    fn advisory_senza_verdetto_strutturato_rifiutata() {
        let mut r = richiesta_valida();
        r.tool_whitelist = vec!["read_file".into()];
        let err = validate_figure(&r, &mutators()).expect_err("advisory senza verdict rifiutata");
        assert_eq!(err.code, "advisory_not_readonly");
        assert_eq!(err.field, Some("tool_whitelist"));
    }

    #[test]
    fn advisory_con_tool_di_scrittura_elenca_gli_offendenti() {
        let mut r = richiesta_valida();
        r.tool_whitelist = vec![
            "read_file".into(),
            "write_file".into(),
            "advisory_verdict".into(),
            "run_command".into(),
        ];
        let err = validate_figure(&r, &mutators()).expect_err("advisory mutativa rifiutata");
        assert_eq!(err.code, "advisory_not_readonly");
        assert_eq!(
            err.details.as_deref(),
            Some(&json!({ "offending_tools": ["write_file", "run_command"] }))
        );
    }

    #[test]
    fn figura_esecutiva_puo_scrivere() {
        // Il contratto read-only vale SOLO per le advisory: una figura esecutiva
        // senza write_file non servirebbe a niente.
        let mut r = richiesta_valida();
        r.advisory = false;
        r.tool_whitelist = vec!["read_file".into(), "write_file".into()];
        assert_eq!(validate_figure(&r, &mutators()), Ok(()));
    }

    #[test]
    fn mutator_tools_in_e_appartenenza_esatta() {
        let tools = vec![
            "read_file".into(),
            "write_file_x".into(),
            "edit_file".into(),
        ];
        // Nessun match per prefisso/sottostringa: 'write_file_x' non e' 'write_file'.
        assert_eq!(mutator_tools_in(&tools, &mutators()), vec!["edit_file"]);
    }

    // ── range ───────────────────────────────────────────────────────────────

    #[test]
    fn range_iterazioni_e_timeout() {
        let casi: [(Option<i32>, Option<i32>, Option<&str>); 8] = [
            (Some(1), Some(30), None),
            (Some(100), Some(3600), None),
            (None, None, None),
            (Some(0), Some(240), Some("max_iterations")),
            (Some(101), Some(240), Some("max_iterations")),
            (Some(-1), Some(240), Some("max_iterations")),
            (Some(12), Some(29), Some("timeout_s")),
            (Some(12), Some(3601), Some("timeout_s")),
        ];
        for (max_iterations, timeout_s, campo_atteso) in casi {
            let mut r = richiesta_valida();
            r.max_iterations = max_iterations;
            r.timeout_s = timeout_s;
            match campo_atteso {
                None => assert_eq!(
                    validate_figure(&r, &mutators()),
                    Ok(()),
                    "{max_iterations:?}/{timeout_s:?} doveva passare"
                ),
                Some(field) => {
                    let err = validate_figure(&r, &mutators()).expect_err("fuori range");
                    assert_eq!(err.code, "out_of_range");
                    assert_eq!(err.field, Some(field));
                }
            }
        }
    }

    // ── normalizzazione ─────────────────────────────────────────────────────

    #[test]
    fn normalizza_trim_tier_e_dedup_tool() {
        let body = CreateFigureBody {
            kind: "  data_engineer  ".into(),
            description: "  analisi  ".into(),
            advisory: true,
            tier: "  HEAVY ".into(),
            prompt_content: format!("  {}  ", prompt_completo()),
            prompt_title: Some("   ".into()),
            tool_whitelist: vec![
                " read_file ".into(),
                "read_file".into(),
                "".into(),
                "advisory_verdict".into(),
            ],
            max_iterations: None,
            timeout_s: None,
        };
        let req = normalize(body);
        assert_eq!(req.kind, "data_engineer");
        assert_eq!(req.description, "analisi");
        // Il tier finisce in una colonna con CHECK sul letterale minuscolo.
        assert_eq!(req.tier, "heavy");
        // Titolo vuoto -> derivato, la colonna e' NOT NULL.
        assert_eq!(req.prompt_title, "Figura: data_engineer");
        assert_eq!(req.tool_whitelist, vec!["read_file", "advisory_verdict"]);
        assert_eq!(validate_figure(&req, &mutators()), Ok(()));
    }

    #[test]
    fn split_csv_ignora_spazi_e_vuoti() {
        assert_eq!(split_csv(" a , b ,, c "), vec!["a", "b", "c"]);
        assert_eq!(split_csv(""), Vec::<String>::new());
        assert_eq!(split_csv(" , "), Vec::<String>::new());
    }

    // ── payload d'errore ────────────────────────────────────────────────────

    #[test]
    fn payload_errore_ha_error_code_field_e_dettagli() {
        let (status, Json(payload)) = FigureError::bad_request("invalid_kind", "kind", "messaggio")
            .with_details(json!({ "extra": 1 }))
            .into_api();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        // `error` stringa: e' quella che fetchJson (lib/api/_shared.ts) legge.
        assert_eq!(payload["error"], json!("messaggio"));
        assert_eq!(payload["code"], json!("invalid_kind"));
        assert_eq!(payload["field"], json!("kind"));
        assert_eq!(payload["extra"], json!(1));
    }
}

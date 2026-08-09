//! Tool agente di analisi qualita codice: scan singolo/progetto e batch.
//!
//! Estratto da mod.rs (refactor god-file).
//!
//! MIGRATI al contratto d'ingresso e a `RispostaTool` (regola Q): l'esito sta
//! nel campo `esito`, la natura del fallimento nel campo `natura`, e il testo
//! torna a essere solo testo.
//!
//! I due handler avevano rami di FALLIMENTO che uscivano come prosa nuda —
//! percorso non risolvibile, file illeggibile, DB irraggiungibile — cioe' che il
//! dispatch leggeva come SUCCESSI. Il piu' grave era la lettura del DB: `match
//! rows { Ok(non vuoto) => ..., _ => "Nessun dato disponibile" }` collassava
//! «lo scan non ha trovato nulla» (successo) e «la query e' fallita» (guasto)
//! nella stessa frase, quindi un DB giu' invitava l'agente a rifare una
//! scansione dal pannello Ottimizzazione.

use serde_json::Value;
use sqlx::postgres::PgRow;
use sqlx::Row;

use nexus_types::routing_client::resolve_purpose_via_http;
use nexus_types::tool_outcome::{NaturaFallimento, RispostaTool};

use super::gateway_client::{
    gateway_batch_status, gateway_batch_submit, GwBatchRequest, GwBatchResult,
};
use super::ToolContextCore;
use crate::input_contract::InputTool;
use crate::tool_inputs::{
    BatchAnalyzeCodeFile, BatchAnalyzeCodeInput, ScanCodeQualityInput, SeverityFilter, Task,
};

/// Purpose del routing per il batch-analyze (regola G: modello dal DB, non
/// hardcoded). Tier-only in `nexus_purpose_model` (mig 0102/0136).
const BATCH_PURPOSE: &str = "anthropic_batch";

/// `max_tokens` di generazione per ogni richiesta del batch. Non e' un nome di
/// modello (regola G): e' il tetto di output, allineato al default del gateway.
const BATCH_MAX_TOKENS: u32 = 4096;

/// Quanti file al massimo entrano in un solo batch. Costante e non letterale
/// perche' il messaggio che rimanda l'agente a spezzare la lista deve dire lo
/// STESSO numero che il controllo applica.
const MAX_FILE_BATCH: usize = 20;

/// Tetto in BYTE del contenuto di un file dentro il prompt di una richiesta.
const PROMPT_MAX_BYTES: usize = 32_000;

/// Quanto si aspetta che il batch termini prima di dichiarare la resa.
const BATCH_DEADLINE_SECS: u64 = 600;

// ──────────────────────────────────────────────────────────────────────────
// scan_code_quality
// ──────────────────────────────────────────────────────────────────────────

/// `scan_code_quality(file_path?, severity_filter?)`.
pub async fn tool_scan_code_quality(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    let params = match ScanCodeQualityInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    // Il catalogo dichiara "Default: all": campo assente e `all` sono lo stesso
    // caso. DIVERGENZA CHIUSA: `severity_filter` e' un ENUM nel catalogo e
    // l'handler lo leggeva come stringa libera con un ramo `_ => true`, quindi
    // un valore mai promesso (es. "low", "critical") non veniva rifiutato ma
    // degradava in silenzio a "nessun filtro" — l'agente otteneva l'opposto di
    // cio' che aveva chiesto senza saperlo.
    let filtro = params.severity_filter.unwrap_or(SeverityFilter::All);
    // Una stringa vuota vale come l'assenza del campo: entrambe significano
    // "l'intero progetto" (stessa lettura di `tool_list_files` per `directory`).
    // Prima finiva nel ramo del file singolo e moriva su un errore di lettura.
    match params.file_path.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(rel_path) => analizza_file(ctx, rel_path, filtro).await,
        None => findings_del_progetto(ctx).await,
    }
}

/// `true` se la severita' passa il filtro richiesto.
///
/// PUNTO UNICO del criterio per le DUE analisi (regola L). Prima il filtro
/// valeva solo per i sorgenti e i file `.sql` lo ignoravano del tutto, pur
/// essendo dichiarato dal catalogo per il tool intero: un parametro promesso e
/// mai letto su meta' dei percorsi.
fn passa_filtro(severity: &str, filtro: SeverityFilter) -> bool {
    match filtro {
        SeverityFilter::All => true,
        SeverityFilter::High => severity == "high",
        SeverityFilter::Medium => severity == "high" || severity == "medium",
    }
}

/// Analizza UN file: risolve il percorso, lo legge, e sceglie la lente in base
/// all'estensione.
async fn analizza_file(
    ctx: &ToolContextCore,
    rel_path: &str,
    filtro: SeverityFilter,
) -> RispostaTool {
    // Punto unico (regola L): de-duplica la root e blocca "..".
    let full_path =
        match nexus_types::workspace_paths::normalize_into_root(&ctx.root_path, rel_path) {
            Ok(clean) => ctx.root_path.join(&clean),
            // RIMEDIABILE, e il messaggio dice come: il percorso lo ha scritto
            // l'agente e la correzione e' riscriverlo relativo alla root.
            Err(e) => {
                return RispostaTool::fallito_rimediabile(format!(
                    "Errore risoluzione path '{rel_path}': {}. Usa un percorso relativo alla \
                     root del progetto, senza '..'.",
                    e.message()
                ))
            }
        };
    let content = match tokio::fs::read_to_string(&full_path).await {
        Ok(c) => c,
        // La natura NON si sceglie a mano: viene dal `ErrorKind` (regola M), che
        // distingue "non esiste" (rimediabile) da "permesso negato" (del
        // sistema) senza leggere un messaggio localizzato.
        Err(e) => {
            return RispostaTool::fallito(format!("Errore lettura file '{rel_path}': {e}"))
                .con_natura(NaturaFallimento::da_errore_io(&e))
        }
    };

    if rel_path.ends_with(".sql") {
        analisi_sql(rel_path, &content, filtro)
    } else {
        analisi_sorgente(rel_path, &content, filtro)
    }
}

/// Lente SQL. Un'analisi che non trova nulla e' RIUSCITA: il vuoto e' il suo
/// risultato, non un fallimento.
fn analisi_sql(rel_path: &str, content: &str, filtro: SeverityFilter) -> RispostaTool {
    let report = mcp_db::analyze_query(content);
    let righe: Vec<String> = report
        .findings
        .iter()
        .filter(|f| passa_filtro(&f.severity, filtro))
        .map(|f| {
            format!(
                "[{}][{}] {} -- {}",
                f.severity.to_uppercase(),
                f.category,
                f.title,
                f.detail
            )
        })
        .collect();
    if righe.is_empty() {
        return RispostaTool::riuscito(format!(
            "Nessun problema trovato in `{rel_path}` (filtro: {})",
            filtro.come_stringa()
        ));
    }
    RispostaTool::riuscito(format!("Analisi SQL `{rel_path}`:\n{}", righe.join("\n")))
}

/// Lente sorgente (complessita', smells, duplicati) piu' le metriche del file.
fn analisi_sorgente(rel_path: &str, content: &str, filtro: SeverityFilter) -> RispostaTool {
    let report = mcp_quality::analyze_source(rel_path, content);
    let righe: Vec<String> = report
        .findings
        .iter()
        .filter(|f| passa_filtro(&f.severity, filtro))
        .map(|f| {
            let loc = f.line.map(|l| format!(":{l}")).unwrap_or_default();
            format!(
                "[{}][{}] {rel_path}{loc} -- {}",
                f.severity.to_uppercase(),
                f.category,
                f.title
            )
        })
        .collect();
    if righe.is_empty() {
        return RispostaTool::riuscito(format!(
            "Nessun problema trovato in `{rel_path}` (filtro: {})",
            filtro.come_stringa()
        ));
    }
    let m = &report.metrics;
    RispostaTool::riuscito(format!(
        "Analisi `{rel_path}`:\n{}\n\nMetriche: {} righe totali, complessità max: {}, \
         lunghezza media funzioni: {:.0}",
        righe.join("\n"),
        m.total_lines,
        m.max_complexity,
        m.avg_function_length
    ))
}

/// Una riga di finding dal DB. I `unwrap_or_default` restano: una colonna
/// illeggibile di una riga gia' letta impoverisce la RESA, non l'esito dello
/// scan, e far fallire l'intero elenco per un titolo NULL sarebbe peggio del
/// difetto. L'errore che contava — la query fallita — ora ha il suo ramo.
fn riga_finding(r: &PgRow) -> String {
    let fp: String = r.try_get("file_path").unwrap_or_default();
    let cat: String = r.try_get("category").unwrap_or_default();
    let sev: String = r.try_get("severity").unwrap_or_default();
    let title: String = r.try_get("title").unwrap_or_default();
    let line: Option<i32> = r.try_get("line_number").ok().flatten();
    let loc = line.map(|l| format!(":{l}")).unwrap_or_default();
    format!("[{}][{}] {fp}{loc} -- {title}", sev.to_uppercase(), cat)
}

/// I top findings gia' registrati per il progetto (ultimo scan completo).
async fn findings_del_progetto(ctx: &ToolContextCore) -> RispostaTool {
    let rows = sqlx::query(
        "SELECT file_path, category, severity, title, line_number \
         FROM project_quality_findings WHERE project_id = $1 AND fixed_at IS NULL \
         ORDER BY CASE severity WHEN 'high' THEN 1 WHEN 'medium' THEN 2 ELSE 3 END \
         LIMIT 30",
    )
    .bind(ctx.project_id)
    .fetch_all(&*ctx.db)
    .await;

    let rows = match rows {
        Ok(r) => r,
        // RAMO NUDO CHIUSO: una query fallita usciva con lo stesso testo di uno
        // scan senza risultati. DEL SISTEMA — un DB irraggiungibile non lo
        // rimedia l'agente, e ripetere la chiamata rifallira'.
        Err(e) => {
            return RispostaTool::fallito_di_sistema(format!(
                "Lettura dei findings di progetto fallita: {e}. Analizza intanto un file \
                 singolo passando 'file_path', che non passa dal database."
            ))
        }
    };
    if rows.is_empty() {
        // Elenco VUOTO da una query RIUSCITA: e' un successo. Il progetto non ha
        // findings registrati, e il testo dice come produrli.
        return RispostaTool::riuscito(
            "Nessun dato di qualità disponibile. Esegui prima una scansione completa dal \
             pannello Ottimizzazione, oppure specifica un file_path per analizzare un file \
             singolo.",
        );
    }
    let righe: Vec<String> = rows.iter().map(riga_finding).collect();
    RispostaTool::riuscito(format!(
        "Top findings del progetto (da ultimo scan):\n{}\n\nUsa scan_code_quality(file_path) \
         per analizzare un file specifico.",
        righe.join("\n")
    ))
}

// ──────────────────────────────────────────────────────────────────────────
// batch_analyze_code
// ──────────────────────────────────────────────────────────────────────────

/// Ruolo di sistema per batch_analyze_code dal DB (mig 0445) con fallback
/// hardcoded. Query diretta: questo crate e' a monte di mcp-core e non puo'
/// usare get_template_or_default.
///
/// Prende il `Task` e non una stringa: col tipo il match e' esaustivo, mentre il
/// ramo `_` catturava insieme "analyze" e qualunque valore fuori catalogo.
async fn batch_role_prompt(db: &sqlx::PgPool, task: Task) -> String {
    let (key, fallback) = match task {
        Task::Document => (
            "system.batch_document_role",
            "Sei un esperto di documentazione tecnica. Analizza il codice e genera commenti/docstring chiari e concisi in italiano. Concentrati sul WHY, non sul WHAT.",
        ),
        Task::Optimize => (
            "system.batch_optimize_role",
            "Sei un esperto di ottimizzazione del codice. Identifica problemi di performance, complessità eccessiva, codice duplicato e suggerisci refactoring concreti.",
        ),
        Task::Analyze => (
            "system.batch_review_role",
            "Sei un esperto di revisione del codice. Identifica bug potenziali, problemi di sicurezza, violazioni di pattern architetturali e punti di miglioramento.",
        ),
    };
    let letto = sqlx::query_scalar::<_, String>(
        "SELECT content FROM nexus_prompt_templates WHERE key = $1 AND is_active = true",
    )
    .bind(key)
    .fetch_optional(db)
    .await;
    match letto {
        Ok(Some(t)) if !t.trim().is_empty() => t,
        Ok(_) => fallback.to_string(),
        // Il ripiego resta — e' il comportamento DICHIARATO di questo helper —
        // ma un DB irraggiungibile smette di essere indistinguibile da una riga
        // assente: prima il ruolo cambiava in silenzio.
        Err(e) => {
            tracing::warn!("batch_role_prompt: lettura template '{key}' fallita: {e}");
            fallback.to_string()
        }
    }
}

/// `batch_analyze_code(files, task)`.
pub async fn tool_batch_analyze_code(ctx: &ToolContextCore, input: &Value) -> RispostaTool {
    let params = match BatchAnalyzeCodeInput::leggi(input) {
        Ok(p) => p,
        Err(risposta) => return risposta,
    };
    if params.files.is_empty() {
        return RispostaTool::fallito_rimediabile(
            "[batch_analyze_code] Nessun file specificato: 'files' deve contenere almeno una \
             voce con il campo 'path'.",
        );
    }
    if params.files.len() > MAX_FILE_BATCH {
        return RispostaTool::fallito_rimediabile(format!(
            "[batch_analyze_code] {} file richiesti, il massimo per batch e' {MAX_FILE_BATCH}: \
             spezza la lista in piu' chiamate.",
            params.files.len()
        ));
    }

    let system_prompt = batch_role_prompt(&ctx.db, params.task).await;
    let (requests, scartati) =
        costruisci_richieste(ctx, &params.files, &system_prompt, params.task).await;
    if requests.is_empty() {
        return RispostaTool::fallito_rimediabile(format!(
            "[batch_analyze_code] Nessun file leggibile: {}. I percorsi devono essere relativi \
             alla root del progetto e non uscirne con '..'.",
            scartati.join("; ")
        ));
    }

    // Provider/modello dal purpose (regola G: niente modello hardcoded). Il batch
    // del gateway oggi supporta solo Anthropic: se il purpose risolve un altro
    // provider, il gateway risponde 400/501 e l'errore risale onestamente al
    // modello (niente fallback inventato, regola H).
    let (provider, model) = match resolve_purpose_via_http(&ctx.db, BATCH_PURPOSE).await {
        Ok(pm) => pm,
        // Configurazione mancante: fuori dalla portata dell'agente.
        Err(e) => {
            return RispostaTool::fallito_di_sistema(format!(
                "[batch_analyze_code] modello batch non risolvibile (purpose '{BATCH_PURPOSE}'): \
                 {e}. Verifica nexus_purpose_model.{BATCH_PURPOSE} (mig 0102/0136)."
            ))
        }
    };
    let batch_id =
        match gateway_batch_submit(&ctx.db, &provider, &model, &requests, BATCH_MAX_TOKENS).await {
            Ok(id) => id,
            // La causa vera (gateway giu', oppure purpose su un provider senza
            // batch) arriva qui gia' appiattita in una String: il codice
            // strutturato non c'e' piu' e indovinarlo dal testo sarebbe la
            // regola M al contrario. DEL SISTEMA e' il default dichiarato quando
            // non si sa: manda a cercare un'altra strada invece di far ripetere
            // una sottomissione che rifallira'.
            Err(e) => {
                return RispostaTool::fallito_di_sistema(format!(
                    "[batch_analyze_code] Errore sottomissione batch: {e}. Analizza i file uno \
                     per uno con scan_code_quality(file_path)."
                ))
            }
        };

    let results = match attendi_risultati(ctx, &provider, &batch_id).await {
        Ok(r) => r,
        Err(risposta) => return risposta,
    };
    componi_esito(&params.files, &results, params.task, &batch_id, &scartati)
}

/// Tronca a `max` BYTE senza spezzare un carattere.
///
/// `&testo[..max]` PANICA se l'indice cade dentro una sequenza UTF-8
/// multi-byte, e un sorgente con accenti, simboli o emoji lo rende raggiungibile
/// a ogni file oltre il tetto: era un panic dentro l'handler, cioe' un tool che
/// non risponde affatto invece di un tool che dichiara un fallimento.
fn taglia_ai_byte(testo: &str, max: usize) -> &str {
    if testo.len() <= max {
        return testo;
    }
    let mut fine = max;
    while fine > 0 && !testo.is_char_boundary(fine) {
        fine -= 1;
    }
    &testo[..fine]
}

/// Il contenuto da analizzare: quello fornito dalla chiamata, oppure il file
/// letto dalla root del progetto.
///
/// `Err(motivo)` per cio' che non si e' potuto leggere. Prima quel caso
/// diventava una richiesta al provider il cui "contenuto del file" era il
/// messaggio d'errore, con in testa il marker di fallimento — cioe' una chiamata
/// pagata per analizzare un errore, dentro un payload dove quel marker non e' il
/// canale di nessuno, e senza che chi aveva chiesto il batch lo sapesse.
async fn contenuto_del_file(
    ctx: &ToolContextCore,
    file: &BatchAnalyzeCodeFile,
) -> Result<String, String> {
    if let Some(c) = file.content.as_deref().filter(|s| !s.is_empty()) {
        return Ok(c.to_string());
    }
    // Punto unico (regola L): de-duplica la root e blocca "..".
    let abs_path = nexus_types::workspace_paths::normalize_into_root(&ctx.root_path, &file.path)
        .map(|clean| ctx.root_path.join(&clean))
        .map_err(|e| format!("{}: percorso non valido ({})", file.path, e.message()))?;
    tokio::fs::read_to_string(&abs_path)
        .await
        .map_err(|e| format!("{}: lettura fallita ({e})", file.path))
}

/// Costruisce una richiesta per file, e l'elenco di cio' che ha dovuto scartare.
///
/// L'indice di `custom_id` resta quello della lista ORIGINALE: chi ricompone
/// l'esito ritrova il file dal suo posto, e uno scartato semplicemente non ha
/// risultato.
async fn costruisci_richieste(
    ctx: &ToolContextCore,
    files: &[BatchAnalyzeCodeFile],
    system_prompt: &str,
    task: Task,
) -> (Vec<GwBatchRequest>, Vec<String>) {
    let mut requests: Vec<GwBatchRequest> = Vec::new();
    let mut scartati: Vec<String> = Vec::new();
    for (i, file) in files.iter().enumerate() {
        let content = match contenuto_del_file(ctx, file).await {
            Ok(c) => c,
            Err(motivo) => {
                tracing::warn!("batch_analyze_code: {motivo}");
                scartati.push(motivo);
                continue;
            }
        };
        let prompt = format!(
            "File: {}\n\n```\n{}\n```\n\nEsegui il task '{}' su questo file.",
            file.path,
            taglia_ai_byte(&content, PROMPT_MAX_BYTES),
            task.come_stringa()
        );
        requests.push(GwBatchRequest {
            custom_id: format!("file-{i}"),
            system: Some(system_prompt.to_string()),
            prompt,
        });
    }
    (requests, scartati)
}

/// Poll con backoff esponenziale su `GET /v1/batch/{provider}/{id}` fino alla
/// scadenza. `Err(risposta)` porta gia' il fallimento dichiarato.
async fn attendi_risultati(
    ctx: &ToolContextCore,
    provider: &str,
    batch_id: &str,
) -> Result<Vec<GwBatchResult>, RispostaTool> {
    let mut wait_secs = 2u64;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(BATCH_DEADLINE_SECS);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(wait_secs)).await;
        wait_secs = (wait_secs * 2).min(60);

        let snapshot = match gateway_batch_status(&ctx.db, provider, batch_id).await {
            Ok(s) => s,
            // Il batch E' stato sottomesso: ripetere il tool ne creerebbe un
            // secondo, pagato, per la stessa domanda. Non c'e' nulla che
            // l'agente possa correggere nella propria chiamata.
            Err(e) => {
                return Err(RispostaTool::fallito_di_sistema(format!(
                    "[batch_analyze_code] Errore polling status del batch {batch_id}: {e}"
                )))
            }
        };
        if snapshot.is_ended() {
            return Ok(snapshot.results);
        }
        if tokio::time::Instant::now() >= deadline {
            // Non e' transitorio nel senso utile del termine: non esiste un tool
            // per riprendere il poll di questo batch, quindi "ritenta identico"
            // significherebbe sottometterne un altro da capo.
            return Err(RispostaTool::fallito_di_sistema(format!(
                "[batch_analyze_code] Timeout: il batch {batch_id} non ha terminato in {} \
                 minuti. Analizza i file uno per uno con scan_code_quality(file_path).",
                BATCH_DEADLINE_SECS / 60
            )));
        }
    }
}

/// Ricompone l'esito per file, preservando la forma storica dell'output.
fn componi_esito(
    files: &[BatchAnalyzeCodeFile],
    results: &[GwBatchResult],
    task: Task,
    batch_id: &str,
    scartati: &[String],
) -> RispostaTool {
    let mut parti: Vec<String> = Vec::new();
    let mut falliti = 0usize;
    for (i, file) in files.iter().enumerate() {
        let custom_id = format!("file-{i}");
        let Some(result) = results.iter().find(|r| r.custom_id == custom_id) else {
            continue;
        };
        if let Some(err) = &result.error {
            falliti += 1;
            parti.push(format!("### {}\n\n[Errore: {err}]", file.path));
        } else if !result.content.is_empty() {
            parti.push(format!("### {}\n\n{}", file.path, result.content));
        }
    }

    if parti.is_empty() {
        return RispostaTool::fallito_di_sistema(format!(
            "[batch_analyze_code] Nessun risultato per il batch {batch_id}."
        ));
    }
    if falliti == parti.len() {
        // RAMO NUDO CHIUSO: un batch terminato con TUTTE le richieste in errore
        // usciva come successo, perche' il testo non era vuoto — l'agente
        // riceveva un elenco di "[Errore: ...]" annunciato come "## Analisi
        // batch". Nessuna analisi e' stata prodotta: e' un fallimento.
        return RispostaTool::fallito_di_sistema(format!(
            "[batch_analyze_code] Il batch {batch_id} e' terminato ma tutte le {falliti} \
             richieste sono fallite:\n\n{}",
            parti.join("\n\n---\n\n")
        ));
    }

    let mut testo = format!(
        "## Analisi batch ({}) — {} file\n\n{}",
        task.come_stringa(),
        parti.len(),
        parti.join("\n\n---\n\n")
    );
    if !scartati.is_empty() {
        testo.push_str(&format!(
            "\n\n---\n\nFile NON analizzati ({}): {}",
            scartati.len(),
            scartati.join("; ")
        ));
    }
    RispostaTool::riuscito(testo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use uuid::Uuid;

    /// Contesto reale (la struct di produzione), pool lazy mai contattato: i
    /// rami qui esercitati rifiutano l'input PRIMA di toccare il DB. Stessa
    /// forma di `attachments::tests::ctx_di_prova` (il crate non ha un helper
    /// condiviso per i test; quando nascera', questi due convergono li').
    fn ctx_di_prova() -> ToolContextCore {
        use crate::context_core::{NoopEmbedder, NoopMutationHooks};
        let db =
            sqlx::PgPool::connect_lazy("postgres://test:test@127.0.0.1:1/test").expect("pool lazy");
        ToolContextCore {
            root_path: std::env::temp_dir(),
            user_id: Uuid::nil(),
            is_git_repo: false,
            can_write: true,
            project_id: Uuid::nil(),
            session_id: None,
            db: Arc::new(db.clone()),
            run_db: Arc::new(db),
            parent_run_id: None,
            run_id: None,
            long_running_patterns: Vec::new(),
            user_role: "admin".to_string(),
            is_nexus_operator: true,
            project_channels: Arc::new(dashmap::DashMap::new()),
            monitor_registry: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
            hooks: Arc::new(NoopMutationHooks),
            embedder: Arc::new(NoopEmbedder),
            isolated_subrun: false,
            write_scope: Vec::new(),
        }
    }

    /// Un input rifiutato e' un FALLIMENTO, e ora lo dichiara nel CAMPO invece
    /// che con un marker in testa al testo. La natura e' `Rimediabile` perche'
    /// il messaggio dice cosa correggere: quale campo manca, o come spezzare la
    /// lista troppo lunga.
    ///
    /// MUTAZIONE: riportando uno dei tre rami a `RispostaTool::riuscito` (o alla
    /// prosa nuda di prima) l'asserzione corrispondente rosseggia — il valore
    /// del difetto reale, che era un rifiuto letto dal dispatch come successo.
    #[tokio::test]
    async fn l_input_rifiutato_dichiara_il_fallimento() {
        let ctx = ctx_di_prova();
        let molti: Vec<Value> = (0..=MAX_FILE_BATCH)
            .map(|i| json!({"path": format!("f{i}.rs")}))
            .collect();
        let casi = [
            json!({"task": "analyze"}),             // files mancante (contratto)
            json!({"files": [], "task": "analyze"}), // vuoto
            json!({"files": molti, "task": "analyze"}), // oltre il cap
        ];
        for input in casi {
            let out = tool_batch_analyze_code(&ctx, &input).await;
            assert!(
                out.esito.e_fallito(),
                "il rifiuto deve dichiararsi fallimento nel campo: {out:?}"
            );
            assert_eq!(
                out.natura,
                Some(NaturaFallimento::Rimediabile),
                "un input sbagliato lo corregge l'agente: {out:?}"
            );
        }
    }

    /// `task` e' OBBLIGATORIO nel catalogo (`required: ["files","task"]`) e
    /// l'handler lo accettava assente ripiegando su "analyze": il modello poteva
    /// ottenere una revisione dove aveva chiesto documentazione senza che nulla
    /// glielo dicesse. Il contratto ora lo pretende, e un valore fuori enum non
    /// arriva all'handler.
    #[tokio::test]
    async fn il_task_e_obbligatorio_e_vincolato() {
        let ctx = ctx_di_prova();
        let senza = json!({"files": [{"path": "a.rs"}]});
        let out = tool_batch_analyze_code(&ctx, &senza).await;
        assert!(out.esito.e_fallito(), "task assente: {out:?}");
        assert!(out.testo.contains("task"), "l'errore nomina il campo: {out:?}");

        let fuori = json!({"files": [{"path": "a.rs"}], "task": "refactor"});
        let out = tool_batch_analyze_code(&ctx, &fuori).await;
        assert!(out.esito.e_fallito(), "valore fuori enum: {out:?}");
        for ammesso in Task::valori() {
            assert!(
                out.testo.contains(ammesso),
                "l'errore elenca i valori ammessi, manca '{ammesso}': {out:?}"
            );
        }
    }

    /// DIVERGENZA CHIUSA: `severity_filter` era dichiarato dal catalogo per il
    /// tool intero e la lente SQL lo ignorava.
    ///
    /// MUTAZIONE: togliendo il `.filter(...)` da `analisi_sql` (com'era prima),
    /// il caso `High` torna a mostrare il finding MEDIUM e il test rosseggia.
    #[test]
    fn il_filtro_di_severita_vale_anche_per_il_sql() {
        let sql = "DELETE FROM users; SELECT * FROM ordini;";
        let tutto = analisi_sql("q.sql", sql, SeverityFilter::All);
        assert!(!tutto.esito.e_fallito(), "un'analisi eseguita e' riuscita: {tutto:?}");
        assert!(tutto.testo.contains("[HIGH]"), "{tutto:?}");
        assert!(tutto.testo.contains("[MEDIUM]"), "{tutto:?}");

        let solo_alte = analisi_sql("q.sql", sql, SeverityFilter::High);
        assert!(solo_alte.testo.contains("[HIGH]"), "{solo_alte:?}");
        assert!(
            !solo_alte.testo.contains("[MEDIUM]"),
            "il filtro deve tagliare le medie: {solo_alte:?}"
        );
    }

    /// Un'analisi senza findings da mostrare e' RIUSCITA: il vuoto e' il
    /// risultato, non un fallimento (stesso criterio di una directory vuota in
    /// `list_files`). Vale anche quando a svuotare l'elenco e' il FILTRO: la
    /// domanda posta ha avuto risposta, ed e' "niente a questa severita'".
    #[test]
    fn nessun_problema_trovato_e_un_successo() {
        let sql = "SELECT id FROM t WHERE id = 1;";
        let out = analisi_sql("q.sql", sql, SeverityFilter::High);
        assert!(!out.esito.e_fallito(), "{out:?}");
        assert!(out.testo.contains("Nessun problema"), "{out:?}");
        assert!(out.testo.contains("high"), "il testo dice quale filtro: {out:?}");
    }

    /// Il taglio del prompt avviene su un confine di carattere: `&s[..max]` su
    /// un indice interno a una sequenza UTF-8 e' un panic, cioe' un tool che non
    /// risponde affatto.
    ///
    /// MUTAZIONE: sostituendo il corpo con `&testo[..max]` questo test panica
    /// invece di fallire — ed e' esattamente cio' che accadeva a ogni file con
    /// accenti oltre i 32 KB.
    #[test]
    fn il_taglio_non_spezza_un_carattere() {
        let testo = "aàaàaà".repeat(10);
        for max in 0..testo.len() {
            let tagliato = taglia_ai_byte(&testo, max);
            assert!(tagliato.len() <= max, "non supera il tetto");
        }
        assert_eq!(taglia_ai_byte("abc", 10), "abc", "sotto il tetto resta intero");
    }
}

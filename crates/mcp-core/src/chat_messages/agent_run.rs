use super::*;

/// Parametri condivisi per avviare un agent run (usato da send e resend).
pub(crate) struct SpawnAgentParams {
    pub(crate) user_id: Uuid,
    pub(crate) session_id: Uuid,
    pub(crate) project_id: Uuid,
    pub(crate) user_message_id: Uuid,
    pub(crate) content: String,
    pub(crate) automation_mode: AutomationMode,
    pub(crate) supervisor_mode: SupervisorMode,
    pub(crate) profile_prompt_block: String,
    pub(crate) system_context: String,
    pub(crate) provider_override: Option<String>,
    pub(crate) model_override: Option<String>,
    pub(crate) profile_provider: Option<String>,
    pub(crate) profile_model: Option<String>,
    pub(crate) attachments: Vec<ChatAttachment>,
    /// Agent type hint dal client (bypassa Q-Learning se presente).
    /// Quando valorizzato attiva `agent_type_forced` in `spawn_agent_run`, che
    /// e' il punto unico (regola L) di bypass del gate di disambiguazione: i
    /// workflow d'azione (error-fix in-chat, auto-debug service_observer) passano
    /// l'AgentType e quindi non vengono mai bloccati da una domanda all'utente.
    pub(crate) nexus_agent_type_hint: Option<String>,
}
/// Risultato di spawn_agent_run: (run_id, provider, model)
pub(crate) struct SpawnAgentResult {
    pub(crate) run_id: Uuid,
    pub(crate) provider: String,
    pub(crate) model: String,
}

/// Esito di `spawn_agent_run`. Distingue i due casi che prima collassavano
/// entrambi su `None` (regola H: la causa radice del bug "disambiguazione +
/// run_turn doppio" era proprio l'indistinguibilita' semantica):
///
/// - `Started`: l'agent run e' stato avviato (caso nominale).
/// - `Disambiguation`: l'intent era ambiguo, il messaggio di chiarimento A/B e'
///   gia' stato inserito; il turno DEVE fermarsi in attesa della risposta utente
///   (mai cadere su `run_turn`). Il payload e' il message-view JSON gia' pronto
///   per il frontend.
/// - `NotStarted`: fallback (es. progetto non caricabile); il chiamante puo'
///   ripiegare su `run_turn`. Equivale all'ex `None` di fallback.
pub(crate) enum SpawnOutcome {
    Started(SpawnAgentResult),
    Disambiguation(serde_json::Value),
    NotStarted,
}
/// Troncamento per caratteri (mai per byte: evita di spezzare sequenze UTF-8).
pub(crate) fn trunc_chars(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        s
    } else {
        s.chars().take(max).collect()
    }
}

/// Resoconto deterministico delle azioni eseguite dall'agente (ADR 0025).
///
/// Usato come risposta finale quando il modello chiude il turno senza body
/// (hollow / completamento vuoto) MA ha comunque eseguito tool: invece di un
/// generico "nessuna risposta", l'utente vede cosa e' stato fatto. Nessuna
/// chiamata LLM: rete di sicurezza garantita, indipendente da provider/cooldown.
/// Ritorna `None` se non c'e' alcuna azione concreta (l'agente non ha fatto
/// nulla) — in quel caso il chiamante usa il placeholder generico.
/// Raccoglie, dagli step COMPLETATI, le righe-azione leggibili (deduplicate) e
/// l'insieme dei file creati/modificati. Punto unico (regola L) condiviso da
/// `build_action_recap` (caso hollow: final_answer vuoto) e `action_recap_footer`
/// (caso final_answer non-conclusivo).
fn collect_actions(steps: &[AgentStep]) -> (Vec<String>, std::collections::BTreeSet<String>) {
    // Punto unico (regola L): l'estrazione dei fatti dagli step vive in
    // session_worklog::collect_step_facts (che la condivide con l'ingest del
    // worklog di sessione, mig 0411). Qui resta solo l'adattamento alla firma
    // storica del recap: righe-azione + set dei path toccati.
    let facts = crate::session_worklog::collect_step_facts(
        steps,
        crate::session_worklog::DEFAULT_ERROR_EXCERPT_CHARS,
    );
    (
        facts.action_lines,
        facts.files_touched.into_keys().collect(),
    )
}

fn build_action_recap(steps: &[AgentStep]) -> Option<String> {
    let (lines, files_touched) = collect_actions(steps);
    if lines.is_empty() {
        return None;
    }
    let mut out = String::from("Task completato. Azioni eseguite dall'agente:\n");
    out.push_str(&lines.join("\n"));
    if !files_touched.is_empty() {
        let files: Vec<String> = files_touched.iter().map(|f| format!("`{f}`")).collect();
        out.push_str(&format!("\n\nFile creati/modificati: {}", files.join(", ")));
    }
    out.push_str(
        "\n\n_(Riepilogo generato automaticamente: l'agente ha eseguito le azioni \
         sopra ma non ha prodotto un messaggio finale. Verifica i risultati.)_",
    );
    Some(out)
}

/// Riepilogo conciso SEMPRE in coda alla risposta: conteggi REALI dagli step
/// (file modificati, azioni eseguite), indipendenti dalla narrativa dell'agente.
/// Cosi' l'utente capisce a colpo d'occhio cosa e' stato fatto anche quando la
/// risposta e' interlocutoria/confusa o troncata. Lo stato del turno
/// (completato/fallito) e' gia' nel badge; qui i fatti che lo completano. `None`
/// se non c'e' alcuna azione concreta (es. turno conversazionale).
fn outcome_summary(steps: &[AgentStep]) -> Option<String> {
    let (lines, files) = collect_actions(steps);
    if lines.is_empty() {
        return None;
    }
    Some(format!(
        "\n\n---\n_Riepilogo esecuzione: {} file modificati, {} azioni eseguite._",
        files.len(),
        lines.len()
    ))
}

/// Footer da appendere a un `final_answer` NON conclusivo (es. frase
/// interlocutoria "Ora elenco i file...") che NON riflette il lavoro svolto:
/// l'agente ha modificato file ma la risposta non li menziona, lasciando
/// l'utente con l'impressione che "non sia stato prodotto nulla di valido"
/// (incidente run gemini-2.5-pro f1db9550). Ritorna `None` se non ci sono state
/// scritture o se la risposta menziona gia' almeno un file modificato (allora e'
/// gia' informativa e non serve appendere nulla).
fn action_recap_footer(answer: &str, steps: &[AgentStep]) -> Option<String> {
    let (lines, files_touched) = collect_actions(steps);
    if files_touched.is_empty() {
        return None;
    }
    let lo = answer.to_lowercase();
    let mentions = files_touched.iter().any(|f| {
        let base = f.rsplit('/').next().unwrap_or(f).to_lowercase();
        !base.is_empty() && lo.contains(&base)
    });
    if mentions {
        return None;
    }
    let mut out = String::from(
        "\n\n---\n_Riepilogo automatico delle azioni eseguite in questo turno \
         (la risposta sopra non le riflette):_\n",
    );
    out.push_str(&lines.join("\n"));
    let files: Vec<String> = files_touched.iter().map(|f| format!("`{f}`")).collect();
    out.push_str(&format!("\n\nFile creati/modificati: {}", files.join(", ")));
    Some(out)
}

// ── Punto unico di finalizzazione del turno (regola L) ──────────────────────
// Estratto perche' DUE call-site finalizzano un run: lo spawn principale
// (spawn_agent_run) e il resume di conferma (handlers.rs). Prima divergevano: il
// resume inseriva il messaggio SOLO se final_answer era presente (niente recap
// per i run hollow) e usava lo status grezzo (bypassando il declassamento
// hollow->failed_diagnosed). Queste tre funzioni pure rendono l'esito IDENTICO
// e CERTO su entrambi i percorsi.

/// True se il run e' hollow E l'intent NON e' conversazionale (per la chat pura
/// il completamento "vuoto" e' atteso). Fonte intent: nexus_task_type (router del
/// brain), come il calcolo `report_hollow` dello spawn principale.
fn is_report_hollow(result: &crate::agent_types::AgentRunResult) -> bool {
    let intent = result.nexus_task_type.as_deref().unwrap_or("");
    result.hollow_completion && intent != "chat"
}

/// Status canonico del run: declassa l'hollow SENZA LAVORO (0 step + risposta
/// vuota, oppure rinuncia esplicita) a `FailedDiagnosed` — mai un "completed"
/// muto (esito certo). L'hollow CON step completati resta lo status originale.
pub(crate) fn canonical_run_status(
    result: &crate::agent_types::AgentRunResult,
) -> AgentRunStatus {
    if is_report_hollow(result) {
        let no_work = (result.steps.is_empty()
            && result.hollow_completion_kind.contains("EMPTY_ANSWER"))
            || result.hollow_completion_kind == "RESIGNED";
        if no_work {
            return AgentRunStatus::FailedDiagnosed;
        }
    }
    result.status.clone()
}

/// Nota di chiusura ONESTA quando un run hollow coincide con provider AI in
/// cooldown. Causa-radice del messaggio fuorviante "completamento vuoto" (regola
/// H): quando i provider buoni (anthropic/openai/google) sono in cooldown per
/// quota/credito esaurito, l'agente non puo' produrre output, ma il sistema
/// mostrava un generico "completamento vuoto, cambia modello" invece della causa
/// reale. Qui si legge la fonte autoritativa `provider_cooldown::cooldown_snapshot()`
/// (regola L, stessa usata dal frontend e dal pre-check) e si dice all'utente cosa
/// fare (ricaricare i crediti). `None` se nessun provider e' in cooldown: vale il
/// placeholder generico.
fn cooldown_exhaustion_note() -> Option<String> {
    cooldown_note_from_snapshot(&crate::provider_cooldown::cooldown_snapshot())
}

/// Logica pura (testabile, regola F): compone la nota dato lo snapshot dei
/// cooldown. Separata dalla fonte globale per non dipendere da stato condiviso.
fn cooldown_note_from_snapshot(snap: &[(String, u64, Option<String>)]) -> Option<String> {
    if snap.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    let mut any_billing = false;
    for (name, secs, reason) in snap {
        let r = reason.as_deref().unwrap_or("");
        let rl = r.to_lowercase();
        if rl.contains("credit")
            || rl.contains("quota")
            || rl.contains("billing")
            || rl.contains("balance")
        {
            any_billing = true;
        }
        if r.is_empty() {
            let mins = secs.div_ceil(60);
            parts.push(format!("{} (~{} min)", name, mins));
        } else {
            parts.push(format!("{} ({})", name, r));
        }
    }
    let list = parts.join(", ");
    let msg = if any_billing {
        format!(
            "_(Il turno non ha prodotto una risposta perche' i provider AI principali sono \
             in cooldown per quota/credito esaurito: {}. Ricarica i crediti (o attendi il \
             reset) e riprova.)_",
            list
        )
    } else {
        format!(
            "_(Il turno non ha prodotto una risposta: i provider AI sono temporaneamente non \
             disponibili ({}). Attendi qualche istante e riprova.)_",
            list
        )
    };
    Some(msg)
}

/// Detection STRUTTURALE di una tool call "colata nel testo" usata come
/// risposta finale (incidente run 5ec12cad: domanda "quante tabelle nel db",
/// dati corretti raccolti dai tool, ma final_answer = "read_file\n{\"path\":
/// \"src/services/bookingService.ts\"}" — il modello ha scritto la chiamata
/// come testo invece di emetterla nel canale strutturato). Criterio di FORMATO,
/// zero semantica: prima riga = identifier snake_case breve, resto = oggetto
/// JSON. Una risposta cosi' non e' una risposta: si passa al recap deterministico.
fn looks_like_textual_tool_call(s: &str) -> bool {
    let trimmed = s.trim();
    let mut lines = trimmed.splitn(2, '\n');
    let first = lines.next().unwrap_or("").trim();
    let rest = lines.next().unwrap_or("").trim();
    if first.is_empty() || first.len() > 64 {
        return false;
    }
    let is_identifier = first
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && first.chars().next().is_some_and(|c| c.is_ascii_lowercase());
    if !is_identifier {
        return false;
    }
    if rest.is_empty() {
        return false;
    }
    matches!(
        serde_json::from_str::<serde_json::Value>(rest),
        Ok(serde_json::Value::Object(_))
    )
}

/// True se il testo e' composto SOLO da blocchi `[Error: ...]` concatenati
/// (formato fisso nostro: i provider impacchettano cosi' gli errori). Incidente
/// run 2c6e41fb: final_answer = "[Error: An assistant message with][Error:
/// Unexpected tool call id ...]..." — gli errori della cascade erano diventati
/// la "risposta" di un run completed. Strutturale, zero semantica.
fn is_only_provider_errors(s: &str) -> bool {
    let mut rest = s.trim();
    if !rest.starts_with("[Error:") {
        return false;
    }
    while rest.starts_with("[Error:") {
        match rest.find(']') {
            Some(end) => rest = rest[end + 1..].trim_start(),
            None => return false,
        }
    }
    rest.trim().is_empty()
}

/// Messaggio finale GARANTITO (ADR 0025): la risposta reale + footer di recap se
/// non menziona i file toccati; oppure, per un run hollow, il recap deterministico
/// delle azioni o un placeholder esplicito. `None` solo se non c'e' nulla da dire
/// (run non-hollow senza risposta, es. intent chat che chiude legittimamente).
pub(crate) fn compose_turn_answer(
    result: &crate::agent_types::AgentRunResult,
) -> Option<String> {
    match result.final_answer.as_deref() {
        // Tool call colata nel testo o soli errori provider come "risposta":
        // non e' una risposta. Recap deterministico (o nota cooldown/placeholder).
        Some(s) if looks_like_textual_tool_call(s) || is_only_provider_errors(s) => {
            build_action_recap(&result.steps)
                .or_else(cooldown_exhaustion_note)
                .or_else(|| {
                    Some(format!(
                        "_(Il modello {} / {} non ha prodotto una risposta valida \
                         (output malformato o soli errori provider). Riformula la \
                         richiesta.)_",
                        result.provider, result.model
                    ))
                })
        }
        Some(s) if !s.trim().is_empty() => {
            let mut a = s.to_string();
            if let Some(footer) = action_recap_footer(&a, &result.steps) {
                a.push_str(&footer);
            }
            Some(a)
        }
        _ if is_report_hollow(result) => build_action_recap(&result.steps)
            .or_else(cooldown_exhaustion_note)
            .or_else(|| {
                Some(format!(
                    "_(Nessuna risposta utile prodotta dall'agente — {} / {} ha chiuso \
                     il turno con un completamento vuoto dopo aver esaurito i tentativi \
                     di fallback. Riformula la richiesta o cambia provider/modello manualmente.)_",
                    result.provider, result.model
                ))
            }),
        _ => None,
    }
}

/// Recap NARRATIVO opzionale (mig 0415, Fase D del flusso chat leggibile). Se il
/// gate `agent.chat.narrative_recap_enabled` e' attivo e il run e' hollow con
/// azioni concrete, chiede a un LLM leggero (purpose `turn_recap`) di
/// trasformare il recap deterministico `base` in una breve narrativa. E' il
/// PUNTO UNICO della logica narrativa (regola L): i due call-site — finalize
/// dello spawn e resume — la invocano. Fallback al `base` su qualunque
/// condizione non soddisfatta o errore (regola H): a gate spento, purpose non
/// configurato o LLM fallito, il comportamento resta il recap deterministico.
pub(crate) async fn narrative_or(
    state: &AppState,
    result: &crate::agent_types::AgentRunResult,
    base: Option<String>,
) -> Option<String> {
    let Some(base_text) = base.as_ref().map(ToOwned::to_owned) else {
        return base;
    };
    let enabled = nexus_auth::get_setting(&state.db, "agent.chat.narrative_recap_enabled")
        .await
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    if !enabled || !is_report_hollow(result) || result.steps.is_empty() {
        return base;
    }

    use crate::internal_routing::{resolve_purpose_model, PurposeResolution};
    let (provider, model) = match resolve_purpose_model(state, "turn_recap").await {
        PurposeResolution::Resolved { provider, model, .. } => (provider, model),
        _ => return base,
    };

    // Riassunto dei fatti deterministici per il prompt (azioni + errori).
    let facts = crate::session_worklog::collect_step_facts(&result.steps, 200);
    let mut actions = String::new();
    for line in facts.action_lines.iter().take(20) {
        actions.push_str(line);
        actions.push('\n');
    }
    for e in facts.errors.iter().take(10) {
        actions.push_str(&format!(
            "- errore [{}] {}: {}\n",
            e.tool,
            e.detail,
            e.excerpt.chars().take(120).collect::<String>()
        ));
    }

    let template = nexus_types::get_template_or_default(
        &state.db,
        &state.template_cache,
        "system.turn_recap_narrative",
    )
    .await;
    if template.trim().is_empty() {
        return base;
    }
    let prompt = template
        .replace("{{recap}}", &base_text)
        .replace("{{actions}}", actions.trim());
    let messages_json =
        serde_json::to_string(&json!([{ "role": "user", "content": prompt }])).unwrap_or_default();

    match state
        .orchestrator
        .neural
        .generate_agent_turn(&provider, &model, &messages_json, "[]", 600, "")
        .await
    {
        Ok(v) => {
            let narr = v
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            // Niente payload nei log (regola F): solo l'esito.
            if narr.chars().count() >= 20 {
                Some(narr)
            } else {
                base
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "turn_recap: narrativa fallita, uso recap deterministico");
            base
        }
    }
}

/// Costruisce il messaggio iniziale per il brain arricchendolo con il contenuto
/// reale degli allegati (pre-extraction nel prompt — ADR 0010/0011/0012).
///
/// ROOT CAUSE storico: senza questo arricchimento, quando l'utente allega un
/// file (es. un `.make` Figma con la specifica di un'app) il modello riceve solo
/// il testo "crea l'app descritta nel file" SENZA il contenuto del file, e finisce
/// per allucinare o generare un Hello World. Qui pre-estraiamo il contenuto
/// autoritativo (Figma/PDF/DOCX/testo) e lo iniettiamo in un blocco `<allegati>`.
///
/// Politica "mai troncare-e-buttare" (mig 0216): il contenuto completo degli
/// allegati e' indicizzato in RAG (vedi `rag::index_attachment`) e recuperato
/// semanticamente qui (`rag::search_semantic`), senza budget arbitrario che
/// tagli dati. Gli estratti iniettati nel prompt sono i chunk piu' rilevanti
/// (cap difensivo per-chunk `CHUNK_INJECT_CAP`, non un budget di sessione); il
/// resto resta accessibile via `nexus_search_semantic`. Estrazioni fallite
/// degradano con metadata + nota, mai panic.
/// Blocco metadata degli allegati di SESSIONE per i turni senza allegati nuovi.
///
/// Complementare a `build_initial_msg_with_attachments` (stesso punto unico,
/// regola L): quando il messaggio corrente non allega nulla ma la sessione ha
/// allegati su messaggi precedenti, il modello deve sapere che esistono e come
/// raggiungerli (nexus_inspect_attachment / estrattori), altrimenti li cerca
/// nel filesystem del progetto. Metadata-only: niente pre-extraction, cap 10
/// allegati piu' recenti.
async fn build_session_attachments_block(
    db: &PgPool,
    content: &str,
    session_id: Uuid,
    current_message_id: Uuid,
) -> String {
    let rows = match sqlx::query(
        r#"SELECT a.id, a.file_name, a.mime_type, a.size_bytes, a.kind
           FROM chat_message_attachments a
           JOIN chat_messages m ON m.id = a.message_id
          WHERE m.session_id = $1 AND a.message_id <> $2
          ORDER BY a.created_at DESC
          LIMIT 10"#,
    )
    .bind(session_id)
    .bind(current_message_id)
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "initial_msg: lettura allegati di sessione fallita — prompt senza blocco"
            );
            return content.to_string();
        }
    };
    if rows.is_empty() {
        return content.to_string();
    }

    let mut b = String::new();
    b.push_str("<allegati_sessione>\n");
    b.push_str(&format!(
        "Questa sessione ha {} allegato/i caricati in messaggi PRECEDENTI. \
         NON sono file nel filesystem del progetto: si leggono SOLO con i tool \
         allegati.\n\n## Allegati di sessione:\n",
        rows.len()
    ));
    for r in &rows {
        let id = r
            .try_get::<Uuid, _>("id")
            .map(|u| u.to_string())
            .unwrap_or_default();
        let name: String = r.try_get("file_name").unwrap_or_default();
        let mime: String = r.try_get("mime_type").unwrap_or_default();
        let size: i64 = r.try_get("size_bytes").unwrap_or(0);
        let kind: String = r.try_get("kind").unwrap_or_default();
        b.push_str(&format!(
            "- {} ({}, {} byte, kind={}) [ID: {}]\n",
            name, mime, size, kind, id
        ));
    }
    b.push_str(
        "\nISTRUZIONE: se il task riguarda uno di questi allegati, chiama \
         nexus_inspect_attachment(attachment_id) e poi il tool di estrazione \
         consigliato in next_action_recommended (es. nexus_extract_figma_code, \
         nexus_extract_pdf_text, nexus_read_attachment). Non cercare questi \
         file con list_files/read_file: non esistono sul filesystem.\n\
         </allegati_sessione>\n\n",
    );
    b.push_str(content);
    b
}

pub(crate) async fn build_initial_msg_with_attachments(
    db: &PgPool,
    content: &str,
    attachments: &[crate::orchestrator::ChatAttachment],
    user_message_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
) -> String {
    if attachments.is_empty() {
        // ROOT CAUSE (2026-06-10, "ha perso il riferimento al file allegato"):
        // il blocco <allegati> copriva SOLO gli allegati del messaggio corrente.
        // Su un turno successivo senza allegati ("riprendi") il run non aveva
        // alcuna traccia degli allegati caricati nei messaggi precedenti della
        // sessione (il blocco non viene persistito in history) e l'agente
        // andava a cercare il file nel filesystem del progetto, concludendo
        // che non esiste. Qui si inietta un blocco METADATA-ONLY con gli
        // allegati di sessione (niente pre-extraction: l'investigazione passa
        // dai tool, ADR 0010/0012).
        return build_session_attachments_block(db, content, session_id, user_message_id).await;
    }

    let n = attachments.len();

    // Cap difensivo per singolo chunk iniettato: i chunk RAG sono gia' limitati
    // da chunk_size, ma evitiamo che un chunk patologico gonfi il prompt.
    const CHUNK_INJECT_CAP: usize = 8_000;

    // Risolvo path fisici e stato di indicizzazione leggendo direttamente
    // chat_message_attachments: serve file_path (per index sincrono di
    // fallback), mime/kind e chunk_count.
    struct AttRow {
        id: String,
        file_name: String,
        file_path: String,
        mime_type: String,
        chunk_count: i64,
    }
    let saved_rows: Vec<AttRow> = match sqlx::query(
        r#"SELECT id, file_name, file_path, mime_type, chunk_count
           FROM chat_message_attachments WHERE message_id = $1 ORDER BY created_at ASC"#,
    )
    .bind(user_message_id)
    .fetch_all(db)
    .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|r| AttRow {
                id: r
                    .try_get::<Uuid, _>("id")
                    .map(|u| u.to_string())
                    .unwrap_or_default(),
                file_name: r.try_get("file_name").unwrap_or_default(),
                file_path: r.try_get("file_path").unwrap_or_default(),
                mime_type: r.try_get("mime_type").unwrap_or_default(),
                chunk_count: r.try_get("chunk_count").unwrap_or(0),
            })
            .collect(),
        Err(e) => {
            tracing::warn!(
                user_message_id = %user_message_id,
                error = %e,
                "initial_msg: lettura chat_message_attachments fallita, fallback metadata"
            );
            Vec::new()
        }
    };

    // Blocco di fallback con soli metadata + istruzione tool, usato quando il
    // RAG e' disabilitato o non produce hit. Mai contenuto inventato.
    let metadata_block = |reason: &str| -> String {
        let mut b = String::new();
        b.push_str("<allegati>\n");
        b.push_str(&format!(
            "L'utente ha allegato {} file. Il contenuto integrale non e' inline qui ({}). \
             DEVI investigarlo prima di rispondere.\n\n## File allegati:\n",
            n, reason
        ));
        for att in attachments.iter() {
            let id_label = att.id.map(|u| format!(" [ID: {}]", u)).unwrap_or_default();
            b.push_str(&format!(
                "- {} ({}, {} byte){}\n",
                att.name, att.mime_type, att.size_bytes, id_label
            ));
        }
        b.push_str(
            "\nISTRUZIONE: per ogni allegato chiama nexus_inspect_attachment(id) e poi il tool \
             di estrazione consigliato (nexus_extract_pdf_text / nexus_extract_docx_text / \
             nexus_extract_figma_structure / nexus_read_attachment), oppure \
             nexus_search_semantic(query, filter_attachment_id) sul contenuto vettorializzato. \
             NON generare un placeholder, NON un Hello World, NON inventare un dominio diverso.\n",
        );
        b.push_str("</allegati>");
        b
    };

    // RAG abilitato? Se no o config non disponibile, fallback metadata.
    let cfg = match crate::rag::current_config(db).await {
        Ok(c) if c.enabled => c,
        Ok(_) => {
            tracing::info!("initial_msg: RAG disabilitato, fallback metadata + tool");
            return format!("{}\n\n{}", content, metadata_block("RAG disabilitato"));
        }
        Err(e) => {
            tracing::warn!("initial_msg: config RAG non disponibile ({e}), fallback metadata");
            return format!(
                "{}\n\n{}",
                content,
                metadata_block("configurazione RAG non disponibile")
            );
        }
    };

    // Index sincrono di fallback: l'auto-index al persist e' fire-and-forget,
    // quindi i chunk potrebbero non essere pronti. Per ogni allegato non ancora
    // indicizzato (chunk_count=0) indicizziamo ORA, sincrono, prima della search.
    let mut current_ids: Vec<String> = Vec::with_capacity(n);
    for att in attachments.iter() {
        let row = att
            .id
            .map(|id| id.to_string())
            .and_then(|id_str| saved_rows.iter().find(|r| r.id == id_str))
            .or_else(|| saved_rows.iter().find(|r| r.file_name == att.name));
        let Some(row) = row else {
            continue;
        };
        current_ids.push(row.id.clone());

        if row.chunk_count <= 0 {
            let Ok(att_uuid) = Uuid::parse_str(&row.id) else {
                continue;
            };
            match crate::rag::index_attachment(
                db,
                att_uuid,
                std::path::PathBuf::from(&row.file_path),
                row.mime_type.clone(),
                row.file_name.clone(),
                Some(project_id),
                Some(session_id),
            )
            .await
            {
                Ok(nc) => tracing::info!(
                    "initial_msg: index sincrono allegato {} -> {} chunks",
                    row.id,
                    nc
                ),
                Err(e) => tracing::warn!(
                    "initial_msg: index sincrono allegato {} fallito: {}",
                    row.id,
                    e
                ),
            }
        }
    }

    // RAG retrieval: cerca i chunk piu' rilevanti tra i soli allegati.
    let hits = match crate::rag::search_semantic(
        db,
        content,
        vec![crate::rag::SourceKind::Attachment],
        Some(project_id),
        Some(session_id),
        Some(cfg.top_k_default),
        Vec::new(),
    )
    .await
    {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("initial_msg RAG: search fallita ({e}), fallback metadata");
            return format!(
                "{}\n\n{}",
                content,
                metadata_block("recupero semantico non disponibile")
            );
        }
    };

    // Tieni solo gli hit appartenenti agli allegati di QUESTO messaggio.
    let relevant: Vec<_> = hits
        .into_iter()
        .filter(|h| current_ids.iter().any(|id| id == &h.source_id))
        .collect();

    if relevant.is_empty() {
        tracing::info!("initial_msg RAG: 0 hit rilevanti, fallback metadata + tool");
        return format!(
            "{}\n\n{}",
            content,
            metadata_block("nessun estratto rilevante recuperato dal contenuto vettorializzato")
        );
    }

    let name_for = |source_id: &str| -> String {
        attachments
            .iter()
            .find(|a| a.id.map(|u| u.to_string()).as_deref() == Some(source_id))
            .map(|a| a.name.clone())
            .or_else(|| {
                saved_rows
                    .iter()
                    .find(|r| r.id == source_id)
                    .map(|r| r.file_name.clone())
            })
            .unwrap_or_else(|| source_id.to_string())
    };

    let mut block = String::new();
    block.push_str("<allegati>\n");
    block.push_str(&format!(
        "L'utente ha allegato {} file. Sotto trovi gli estratti piu' rilevanti rispetto alla tua \
         richiesta, recuperati semanticamente dal contenuto completo dei file (vettorializzato). \
         Il contenuto completo e' disponibile via il tool nexus_search_semantic(query, \
         filter_attachment_id) per approfondire qualsiasi aspetto.\n\n## File allegati:\n",
        n
    ));
    for att in attachments.iter() {
        let id_label = att.id.map(|u| format!(" [ID: {}]", u)).unwrap_or_default();
        block.push_str(&format!(
            "- {} ({}, {} byte){}\n",
            att.name, att.mime_type, att.size_bytes, id_label
        ));
    }

    block.push_str("\n## Estratti rilevanti:\n");
    let n_hits = relevant.len();
    for h in relevant.iter() {
        let chunk = trunc_chars(h.chunk_text.clone(), CHUNK_INJECT_CAP);
        block.push_str(&format!(
            "\n[score {:.2}, da {}]\n{}\n",
            h.score,
            name_for(&h.source_id),
            chunk
        ));
    }

    block.push_str(
        "\nISTRUZIONE: il contenuto sopra e' la specifica reale fornita dall'utente. Implementa \
         ESATTAMENTE quanto descritto, con le funzionalita' specifiche indicate. Se ti serve piu' \
         contesto chiama nexus_search_semantic(query=\"...\", filter_attachment_id=\"<id>\"). NON \
         generare un placeholder, NON un Hello World, NON inventare un dominio diverso da quello \
         descritto.\n",
    );
    block.push_str("</allegati>");

    tracing::info!(
        attachments = n,
        chunks_retrieved = n_hits,
        block_chars = block.len(),
        "initial_msg RAG: {} allegati, {} chunk recuperati, blocco {} chars",
        n,
        n_hits,
        block.len()
    );

    format!("{}\n\n{}", content, block)
}
/// Punto unico (regola L) dell'invariante "al piu' UN run agentico attivo per
/// session_id". Applica il last-wins: marca 'cancelled' + `cancellation_requested`
/// TUTTI i run ancora attivi della sessione (il nuovo richiedente supera i
/// precedenti) ed emette `is_final` sui loro broadcast channel cosi' la UI chiude
/// subito gli SSE. Il flag `cancellation_requested` e' il segnale di stop
/// COOPERATIVO che il brain controlla tra le iterazioni del grafo
/// (`route_after_executor` -> `_check_superseded`) per terminare DAVVERO il loop
/// in memoria del run superato (marcare il DB da solo non lo fermerebbe).
///
/// Tutti i call site che creano o annullano run delegano qui (spawn_agent_run,
/// resume-handler, cancel_agent_run): nessuna re-implementazione della query
/// "cancella gli attivi della sessione". Ritorna gli id cancellati.
pub(crate) async fn supersede_active_runs(
    state: &AppState,
    session_id: Uuid,
    reason: &str,
) -> Vec<Uuid> {
    // Messaggio finale leggibile per la UI (il cancellation_reason resta il
    // valore macchina). COALESCE: non sovrascrive un final_answer gia' presente.
    let final_msg = if reason == "user_cancel" {
        "Operazione annullata."
    } else {
        "Superato da un nuovo run."
    };
    let cancelled_ids: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE agent_runs \
         SET status='cancelled', completed_at=NOW(), \
             cancellation_requested=NOW(), cancellation_reason=$2, \
             final_answer=COALESCE(final_answer, $3) \
         WHERE session_id = $1 \
           AND status IN ('running', 'awaiting_confirmation') \
         RETURNING id",
    )
    .bind(session_id)
    .bind(reason)
    .bind(final_msg)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    for cid in &cancelled_ids {
        if let Some(ch) = state.agent_channels.get(cid) {
            let _ = ch.send(AgentStepEvent {
                run_id: cid.to_string(),
                step: None,
                trace: None,
                is_final: true,
                token_delta: None,
                thinking_delta: None,
                meta_step: None,
            });
        }
    }
    if !cancelled_ids.is_empty() {
        tracing::info!(
            "supersede_active_runs: cancellati {} run attivi sulla session {} (reason={})",
            cancelled_ids.len(),
            session_id,
            reason
        );
        // Worklog di sessione (mig 0411): ingest SINCRONO del lavoro gia'
        // svolto dai run superati, dagli agent_steps gia' persistiti
        // incrementalmente dal brain (M68). Sincrono by-design: il chiamante
        // (spawn_agent_run) compone il contesto del NUOVO run subito dopo, e
        // deve trovare il digest aggiornato — altrimenti il nuovo run ripete
        // le azioni del run interrotto. Best-effort sul singolo run.
        let label = if reason == "user_cancel" {
            "annullato dall'utente"
        } else {
            "superseduto (interrotto da un nuovo messaggio)"
        };
        for cid in &cancelled_ids {
            if let Err(e) =
                crate::session_worklog::ingest_from_db_steps(&state.db, *cid, label).await
            {
                tracing::warn!(error = %e, run_id = %cid, "session_worklog: ingest al supersede fallito");
            }
        }
    }
    cancelled_ids
}

/// Punto unico (regola L) della domanda "c'e' un run agentico attivo su questa
/// sessione?". Usa gli STESSI stati che `supersede_active_runs` (sopra)
/// considera attivi (`running`/`awaiting_confirmation`): coerenza garantita,
/// nessuna re-implementazione del predicato sparsa nei call site.
///
/// Fail-safe: in caso di errore DB assume run attivo (`true`) per NON rischiare
/// di interrompere un run in corso. Il chiamante critico e' `process_resume`,
/// che usa questa funzione per decidere se RISVEGLIARE l'agente (a riposo) o
/// RIMANDARE il resume (run ancora attivo) invece di superarlo via last-wins.
pub(crate) async fn session_has_active_run(db: &sqlx::PgPool, session_id: Uuid) -> bool {
    match sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS( \
             SELECT 1 FROM agent_runs \
              WHERE session_id = $1 \
                AND status IN ('running', 'awaiting_confirmation') \
         )",
    )
    .bind(session_id)
    .fetch_one(db)
    .await
    {
        Ok(active) => active,
        Err(e) => {
            tracing::warn!(
                "session_has_active_run: query fallita su session {session_id} ({e}); \
                 assumo run attivo (skip per sicurezza)"
            );
            true
        }
    }
}

/// Risolve la RISPOSTA dell'utente a una richiesta di disambiguazione.
///
/// Quando l'ultimo messaggio assistant della sessione e' una
/// `disambiguation_request` (metadata.kind) e il nuovo messaggio utente e' la
/// scelta secca di un'opzione ("A", "b.", "C)"), l'intent va risolto dal
/// candidato corrispondente gia' salvato nel metadata della domanda — NON
/// ri-classificato: la ri-classificazione della lettera singola risultava di
/// nuovo ambigua e il gate ri-emetteva la stessa domanda in loop (bug di
/// continuita' osservato in UI: l'utente rispondeva "A" e riceveva un'altra
/// volta la domanda A/B). Un testo piu' lungo della sola lettera e' una nuova
/// descrizione e segue la classify normale, come da istruzioni nella domanda.
/// Ritorna l'intent scelto, o None se non e' una risposta di disambiguazione.
async fn resolve_disambiguation_reply(
    db: &sqlx::PgPool,
    session_id: Uuid,
    content: &str,
) -> Option<String> {
    let t = content.trim();
    if t.is_empty() || t.len() > 2 {
        return None;
    }
    let mut chars = t.chars();
    let letter = chars.next()?.to_ascii_uppercase();
    if !('A'..='C').contains(&letter) {
        return None;
    }
    if let Some(p) = chars.next() {
        if p != '.' && p != ')' {
            return None;
        }
    }
    let idx = (letter as u8 - b'A') as usize;

    let meta = sqlx::query_scalar::<_, serde_json::Value>(
        r#"SELECT metadata FROM chat_messages
            WHERE session_id = $1 AND role = 'assistant'
            ORDER BY created_at DESC LIMIT 1"#,
    )
    .bind(session_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()?;

    if meta.get("kind").and_then(|v| v.as_str()) != Some("disambiguation_request") {
        return None;
    }
    Some(
        meta.get("candidates")?
            .get(idx)?
            .get("intent")?
            .as_str()?
            .to_string(),
    )
}

/// Logica condivisa: carica progetto, costruisce contesto, avvia AgentLoop in background.
/// Ritorna `SpawnOutcome::NotStarted` se il progetto non è caricabile (fallback al
/// singolo turn) e `SpawnOutcome::Disambiguation` se l'intent e' ambiguo (turno
/// fermato in attesa della risposta utente).
pub(crate) async fn spawn_agent_run(
    state: &AppState,
    mut params: SpawnAgentParams,
) -> SpawnOutcome {
    let project_ctx = load_project_context(&state.db, params.project_id, params.user_id).await;
    let proj = match project_ctx {
        Ok(p) => p,
        Err(_) => return SpawnOutcome::NotStarted,
    };

    let run_id = Uuid::new_v4();
    let (tx, _rx) = broadcast::channel::<AgentStepEvent>(256);
    state.agent_channels.insert(run_id, tx.clone());

    // Forcing esplicito dell'AgentType via hint (chiude il campo prima inerte):
    // se il chiamante specifica `nexus_agent_type_hint` (es. "debugger" da
    // "Risolvi con Nexus" o dall'auto-debug del service_observer), antepone il
    // system prompt specializzato di quel ruolo al contesto di sistema, cosi'
    // l'agente assume davvero quel comportamento. Riusa get_agent_system_prompt.
    // Traccia se il chiamante ha gia' DECISO il ruolo/intent (hint forzato): in
    // tal caso la disambiguazione piu' sotto e' incoerente (l'intent NON e'
    // ambiguo per costruzione) e va saltata. Critico per i trigger AUTOMATICI
    // fuori-chat (auto-debug del service_observer): nessun umano risponde al
    // chiarimento, quindi senza questo bypass il loop di remediation si blocca.
    let mut agent_type_forced = false;
    if let Some(hint) = params.nexus_agent_type_hint.as_deref() {
        if !hint.trim().is_empty() {
            let pascal = crate::internal_learning::snake_to_pascal(hint);
            let agent_type = nexus_orchestrator::AgentType::from_name(&pascal);
            let agent_prompt = crate::nexus_routing::get_agent_system_prompt(&agent_type);
            if !agent_prompt.is_empty() {
                params.system_context = if params.system_context.trim().is_empty() {
                    agent_prompt
                } else {
                    format!("{}\n\n{}", agent_prompt, params.system_context)
                };
                agent_type_forced = true;
                tracing::info!("spawn_agent_run: AgentType forzato via hint = {}", pascal);
            }
        }
    }

    // ─────────────────────────────────────────────────────────────────
    // Disambiguation step (best practice NLU)
    // ─────────────────────────────────────────────────────────────────
    // Se il classifier marca il task come ambiguo (top confidence < 0.70
    // oppure margine < 0.15 sul secondo candidato) E l'utente NON e' in
    // modalita' "automatic", inseriamo un messaggio assistant che chiede
    // chiarimenti invece di indovinare. Riferimento: Rasa/Dialogflow/LUIS.
    //
    // Modalita' automatic salta la disambiguazione: l'utente vuole che il
    // sistema agisca anche con incertezza moderata (top candidato vince).
    // Anche un AgentType FORZATO via hint la salta: il ruolo/intent e' gia'
    // deciso dal chiamante, chiedere chiarimenti sarebbe ridondante e (per i
    // trigger automatici fuori-chat) bloccherebbe il flusso senza un umano.
    // Arricchiamo il messaggio passato al classifier con un breve contesto
    // dei turni recenti: il classifier LLM NON ha accesso autonomo alla
    // cronologia, quindi senza questo prefisso messaggi tipo "riepiloga
    // animali" o "applica tutte le ultime" verrebbero marcati ambigui
    // ("messaggio troppo generico"). L'originale resta invariato per il
    // resto del flusso (`params.content`).
    let classifier_input = build_message_with_recent_context_for_classifier(
        &state.db,
        params.session_id,
        &params.content,
    )
    .await;
    let mut classified = state
        .orchestrator
        .classify_intent_full(&classifier_input)
        .await;
    // Continuita' della disambiguazione: se questo messaggio E' la risposta
    // ("A"/"B"/"C") alla domanda appena posta, l'intent e' quello del candidato
    // scelto — risolto dal metadata della domanda, non ri-classificato (la
    // lettera secca verrebbe ri-marcata ambigua e la stessa domanda ri-emessa
    // in loop). intent_str_to_static e' il punto unico di interning (regola L).
    // Quando il resolver scatta, l'intent va propagato anche al brain come
    // intent_hint: il router_node ri-classificherebbe la lettera secca come
    // 'chat' (prompt_len=1), vanificando la scelta dell'utente.
    let mut resolved_intent_hint: Option<String> = None;
    if let Some(chosen) =
        resolve_disambiguation_reply(&state.db, params.session_id, &params.content).await
    {
        if let Some(static_intent) = crate::orchestrator::intent_str_to_static(&chosen) {
            tracing::info!(
                "spawn_agent_run: risposta di disambiguazione '{}' -> intent '{}' (gate saltato)",
                params.content.trim(),
                static_intent,
            );
            classified.intent = static_intent;
            classified.is_ambiguous = false;
            classified.confidence = 1.0;
            resolved_intent_hint = Some(static_intent.to_string());
        } else {
            tracing::warn!(
                "spawn_agent_run: risposta di disambiguazione con intent sconosciuto '{}' — \
                 procedo con la classify normale",
                chosen,
            );
        }
    }
    if classified.is_ambiguous
        && !matches!(params.automation_mode, AutomationMode::Automatic)
        && !agent_type_forced
    {
        tracing::info!(
            "spawn_agent_run: intent ambiguo (conf={:.2}, candidati={}), chiedo disambiguazione",
            classified.confidence,
            classified.candidates.len(),
        );
        let disambig_msg = build_disambiguation_message(&classified);
        let meta = json!({
            "kind": "disambiguation_request",
            "intent": classified.intent,
            "confidence": classified.confidence,
            "candidates": classified.candidates,
            // Metriche a 0: la disambiguazione non consuma token. Il frontend legge
            // totalTokens/totalCost dal metadata (to_message_view, persistence.rs)
            // e li formatta con toFixed; senza questi campi il view avrebbe null
            // -> crash JS nel processing della risposta live del send.
            "promptTokens": 0,
            "completionTokens": 0,
            "totalTokens": 0,
            "totalCost": 0.0,
        });
        // Inseriamo il messaggio di chiarimento e ne recuperiamo l'id: serve sia
        // per costruire il message-view da restituire al frontend, sia per
        // emettere l'evento SSE (la disambiguazione prima NON ne emetteva nessuno,
        // quindi i client che ascoltano lo stream non vedevano il messaggio finche'
        // non ricaricavano). Stesso pattern di run.rs (insert -> emit -> view).
        let msg_id = match insert_message(
            &state.db,
            params.session_id,
            params.project_id,
            "assistant",
            &disambig_msg,
            meta,
            Some(params.user_message_id),
        )
        .await
        {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(
                    "spawn_agent_run: insert messaggio disambiguazione fallito: {}",
                    e.1["error"].as_str().unwrap_or("errore sconosciuto")
                );
                state.agent_channels.remove(&run_id);
                return SpawnOutcome::NotStarted;
            }
        };
        nexus_events::dispatcher::emit(
            &state.project_channels,
            params.project_id,
            nexus_events::ProjectEvent::ChatMessageAdded {
                session_id: params.session_id,
                message_id: msg_id,
                role: "assistant".into(),
                total_tokens: None,
                total_cost_usd: None,
            },
        );
        // Rimuoviamo il canale broadcast: non avviamo l'agent run.
        state.agent_channels.remove(&run_id);
        let view = match load_message_by_id(&state.db, msg_id).await {
            Ok(row) => match to_message_view(&row) {
                Ok(v) => match serde_json::to_value(v) {
                    Ok(json_view) => json_view,
                    Err(e) => {
                        tracing::warn!(
                            "spawn_agent_run: serializzazione view disambiguazione fallita: {}",
                            e
                        );
                        return SpawnOutcome::NotStarted;
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        "spawn_agent_run: to_message_view disambiguazione fallita: {}",
                        e.1["error"].as_str().unwrap_or("errore sconosciuto")
                    );
                    return SpawnOutcome::NotStarted;
                }
            },
            Err(e) => {
                tracing::warn!(
                    "spawn_agent_run: load_message_by_id disambiguazione fallito: {}",
                    e.1["error"].as_str().unwrap_or("errore sconosciuto")
                );
                return SpawnOutcome::NotStarted;
            }
        };
        return SpawnOutcome::Disambiguation(view);
    }

    // ─────────────────────────────────────────────────────────────────
    // Routing slot-based (Livello 4 NLU, mig 0133)
    // ─────────────────────────────────────────────────────────────────
    // Prima del routing classico (intent, behavior_mode), proviamo la
    // matrice slots: e' piu' precisa perche' indicizzata su 4 slot
    // canonici (action_verb, target_type, framework, scope) estratti
    // dal classifier LLM. Se nessun match O slots incompleti, cadiamo
    // sul routing classico testato. Soglia confidence: 0.60.
    //
    // Safety-net: se il classifier LLM non ha estratto slot (es. JSON
    // parse fail con Gemini Flash) ma il messaggio chiaramente descrive
    // una "test failure resolution" via keyword detection, ricostruiamo
    // slots minimi euristicamente per non perdere il routing capable.
    let effective_slots = if classified.slots.is_complete() {
        classified.slots.clone()
    } else {
        crate::routing_slots::infer_slots_heuristic(&params.content)
    };
    let slot_routing_hit = if params.provider_override.is_none() && params.model_override.is_none()
    {
        state
            .orchestrator
            .route_by_slots(&state.db, &effective_slots, 0.60)
            .await
    } else {
        None
    };

    // Routing intelligente: Neural Core classifica l'intent e sceglie il provider ottimale
    // (es. "fix" → anthropic, "chat" → openai, ecc.) invece di usare sempre il primo in lista.
    // Il profile_provider ha priorità sul routing automatico, ma non sul provider_override utente.
    let effective_override = if let Some((slot_provider, _slot_model, _src)) = &slot_routing_hit {
        // Slot routing ha vinto: forziamo il provider scelto come override
        // (il modello viene applicato sotto, dopo il routing classico, sovrascrivendolo).
        Some(slot_provider.clone())
    } else {
        params
            .provider_override
            .filter(|v| !v.trim().is_empty())
            .or_else(|| params.profile_provider.filter(|v| !v.trim().is_empty()))
    };
    let effective_model_override =
        if let Some((_slot_provider, slot_model, _src)) = &slot_routing_hit {
            Some(slot_model.clone())
        } else {
            params
                .model_override
                .filter(|v| !v.trim().is_empty())
                .or_else(|| params.profile_model.filter(|v| !v.trim().is_empty()))
        };
    if let Some((p, m, src)) = &slot_routing_hit {
        tracing::info!("spawn_agent_run: routing slot-based {} → {}/{}", src, p, m);
    }

    // Conta i messaggi esistenti nella sessione per calibrare il routing:
    // sessioni con molti messaggi indicano task lunghi (es. "continua") che
    // richiedono modelli più capaci anche se il messaggio è breve.
    let context_message_count =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM chat_messages WHERE session_id = $1")
            .bind(params.session_id)
            .fetch_one(&state.db)
            .await
            .unwrap_or(0) as usize;

    // Versione "detailed": ritorna anche `no_capable_provider` e
    // `providers_in_cooldown`. Se nessun provider e' utilizzabile fermiamo
    // il run prima di chiamare il brain — emettiamo invece un evento SSE
    // `provider_unavailable` che la UI consuma per mostrare un banner.
    let routing_result = state
        .orchestrator
        .resolve_agent_provider_detailed(
            &state.db,
            &params.project_id.to_string(),
            "",
            &params.content,
            effective_override.as_deref(),
            effective_model_override.as_deref(),
            context_message_count,
            None, // behavior_mode_session: nessun override per il pre-check routing
            None, // intent_hint: il pre-check classifica (non e' nel loop, no timeout client)
        )
        .await;

    let provider = routing_result.provider.clone();
    let model_str = routing_result.model.clone();

    // Stop & alert se nessun provider capable: niente run, niente brain call.
    if routing_result.no_capable_provider {
        let providers_list = routing_result.providers_in_cooldown.join(", ");
        let alert_msg = if providers_list.is_empty() {
            "Nessun provider AI configurato disponibile. Verifica le API key in admin.".to_string()
        } else {
            format!(
                "Tutti i provider AI configurati sono in cooldown (quota/credito esaurito): {}. \
                 Aggiungi credito o aspetta il reset, poi riprova.",
                providers_list,
            )
        };
        tracing::warn!(
            "spawn_agent_run: no_capable_provider per session={} → STOP + alert. {}",
            params.session_id,
            alert_msg,
        );
        // Persist run come "failed" con errore strutturato.
        let _ = sqlx::query(
            r#"INSERT INTO agent_runs
               (id, session_id, project_id, user_id, run_message_id, status,
                automation_mode, provider, model, supervisor_mode, iteration_count, error, created_at)
               VALUES ($1,$2,$3,$4,$5,'failed',$6,$7,$8,$9,0,$10,NOW())"#,
        )
        .bind(run_id)
        .bind(params.session_id)
        .bind(params.project_id)
        .bind(params.user_id)
        .bind(params.user_message_id)
        .bind(params.automation_mode.as_str())
        .bind(&provider)
        .bind(&model_str)
        .bind(params.supervisor_mode.as_str())
        .bind(&alert_msg)
        .execute(&state.db)
        .await;
        // Emit evento SSE con status `provider_unavailable`. La UI lo intercetta
        // (vedi chat-panel.tsx) per mostrare il banner rosso.
        let alert_step = AgentStep {
            run_id: run_id.to_string(),
            step_index: 0,
            tool_name: String::new(),
            tool_input: serde_json::json!({
                "providers_in_cooldown": routing_result.providers_in_cooldown,
                "rationale": routing_result.rationale,
            }),
            tool_result: Some(alert_msg.clone()),
            status: AgentStepStatus::ProviderUnavailable,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        let _ = tx.send(AgentStepEvent {
            run_id: run_id.to_string(),
            step: Some(alert_step),
            trace: None,
            is_final: true,
            token_delta: None,
            thinking_delta: None,
            meta_step: None,
        });
        return SpawnOutcome::Started(SpawnAgentResult {
            run_id,
            provider,
            model: model_str,
        });
    }

    // ── AUTO-COMPACT a soglia, in BACKGROUND (regola H: fix strutturale) ─────
    // Valuta il rapporto token sessione / context window e, se supera la soglia
    // (DB-driven, regola G), compatta la sessione riusando compact_session_core.
    //
    // ESEGUITO IN BACKGROUND (fire-and-forget): la compattazione chiama un LLM e
    // su sessioni grandi puo' durare oltre 20s. Eseguirla sincrona qui bloccava
    // la risposta "running" della POST /chat/.../messages oltre il timeout del
    // proxy Next.js: la chat riceveva HTTP 500 (socket hang up) ad OGNI invio su
    // una sessione vicina alla soglia, perche' il compact riscattava ogni volta.
    // Il turno corrente NON dipende dalla compattazione: la history dell'agente
    // usa una finestra limitata (ultimi 4 raw + top-6 semantici), non l'intera
    // sessione. Quindi snellire la sessione puo' avvenire async, a beneficio dei
    // turni futuri. Best-effort: ogni fallimento e' loggato WARN dentro il task.
    {
        let state_bg = state.clone();
        let session_id = params.session_id;
        let project_id = params.project_id;
        let provider_bg = provider.clone();
        let model_bg = model_str.clone();
        tokio::spawn(async move {
            maybe_auto_compact(&state_bg, session_id, project_id, &provider_bg, &model_bg).await;
        });
    }

    // Last-wins (punto unico, regola L): questo nuovo run supera OGNI run ancora
    // attivo sulla stessa sessione, fermandoli cooperativamente. Vale per tutti i
    // call site (chat, resume, process_resume, service_observer) perche' passano
    // tutti da spawn_agent_run -> l'invariante "max 1 run attivo per sessione" e'
    // applicata in un solo posto e nessun nuovo call site puo' dimenticarla.
    let superseded_runs =
        supersede_active_runs(state, params.session_id, "superseded_by_new_run").await;

    // Persist initial run in DB
    let _ = sqlx::query(
        r#"INSERT INTO agent_runs
           (id, session_id, project_id, user_id, run_message_id, status,
            automation_mode, provider, model, supervisor_mode, iteration_count, created_at)
           VALUES ($1,$2,$3,$4,$5,'running',$6,$7,$8,$9,0,NOW())"#,
    )
    .bind(run_id)
    .bind(params.session_id)
    .bind(params.project_id)
    .bind(params.user_id)
    .bind(params.user_message_id)
    .bind(params.automation_mode.as_str())
    .bind(&provider)
    .bind(&model_str)
    .bind(params.supervisor_mode.as_str())
    .execute(&state.db)
    .await;

    // Il loop agente gira integralmente nel brain LangGraph (Python): qui
    // serve solo il Sender broadcast per ri-emettere gli eventi SSE.
    let tx_for_brain = tx.clone();
    // Consumato dal tokio::spawn sotto per non lasciare dangling clone.
    drop(tx);

    // History ibrida: ultimi 4 raw (2 turni completi) + top-6 semanticamente
    // rilevanti da Qdrant. L'embedding di ricerca include l'ULTIMO turno
    // user+assistant insieme al messaggio corrente, cosi' la ricerca
    // semantica si aggancia al tema della conversazione e non al solo testo
    // letterale del turno corrente. I semantici che ricadono nella finestra
    // recente vengono filtrati per evitare duplicazione.
    // Se Qdrant/embedding non disponibile, fallback a ultimi 8 raw.
    let vec_deps_ok = state
        .dependency_status
        .qdrant
        .load(std::sync::atomic::Ordering::Relaxed)
        && state
            .dependency_status
            .embedder
            .load(std::sync::atomic::Ordering::Relaxed);
    let recent_history = if vec_deps_ok {
        build_vectorized_conversation_history(
            &state.db,
            &state.orchestrator.neural,
            params.session_id,
            &params.content,
            4, // ultimi 4 messaggi raw = 2 turni completi user+assistant
            6, // top-6 semantici dalla storia piu' vecchia (soglia 0.40)
        )
        .await
    } else {
        // Dipendenze vettoriali down: usa solo gli ultimi messaggi raw
        build_recent_conversation_history(&state.db, params.session_id, 8).await
    };
    // Versione testuale compatta solo per logging
    let recent_context = build_recent_conversation_context(&state.db, params.session_id, 4).await;
    // Legge analysis_json + custom_instructions in un'unica query
    let (analysis_json_opt, custom_instructions_opt): (Option<serde_json::Value>, Option<String>) =
        sqlx::query_as::<_, (Option<serde_json::Value>, Option<String>)>(
            "SELECT analysis_json, custom_instructions FROM projects WHERE id = $1",
        )
        .bind(params.project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or((None, None));

    let analysis_summary: Option<String> =
        analysis_json_opt.and_then(|analysis: serde_json::Value| {
            let langs = analysis
                .get("languages")
                .and_then(|l| l.as_array())
                .map(|arr| {
                    arr.iter()
                        .take(5)
                        .filter_map(|e| e.get("language").and_then(|v| v.as_str()))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let frameworks = analysis
                .get("frameworks")
                .and_then(|f| f.as_array())
                .map(|arr| {
                    arr.iter()
                        .take(6)
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let scripts = analysis
                .get("dependencies")
                .and_then(|d| d.get("node"))
                .and_then(|n| n.get("scripts"))
                .and_then(|s| s.as_object())
                .map(|scripts_map| {
                    scripts_map
                        .iter()
                        .take(8)
                        .map(|(k, v)| format!("  {} → {}", k, v.as_str().unwrap_or("")))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if langs.is_empty() && frameworks.is_empty() {
                None
            } else {
                let mut summary = format!("Linguaggi: {}\nFramework/stack: {}", langs, frameworks);
                if !scripts.is_empty() {
                    summary.push_str(&format!("\nScript disponibili:\n{}", scripts));
                }
                Some(summary)
            }
        });

    let db_connections_block = {
        let rows = sqlx::query(
            "SELECT name, engine, ENCODE(connection_secret, 'escape') AS connection_secret, is_primary \
             FROM project_database_config \
             WHERE project_id = $1 \
             ORDER BY is_primary DESC, LOWER(name)"
        )
        .bind(params.project_id)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        if rows.is_empty() {
            String::new()
        } else {
            let mut block = format!(
                "\n{}\n",
                crate::prompt_templates::get_template_or_default(
                    &state.db,
                    &state.template_cache,
                    "system.db_connections_directive",
                )
                .await
            );
            for r in &rows {
                let name: String = r.try_get("name").unwrap_or_default();
                let engine: Option<String> = r.try_get("engine").unwrap_or(None);
                let dsn: Option<String> = r.try_get("connection_secret").unwrap_or(None);
                let primary: bool = r.try_get("is_primary").unwrap_or(false);
                let label = if primary { " [PRIMARY]" } else { "" };
                if let Some(ref dsn_val) = dsn {
                    block.push_str(&format!(
                        "  - {}{}: engine={} connection_string=\"{}\"\n",
                        name,
                        label,
                        engine.as_deref().unwrap_or("unknown"),
                        dsn_val
                    ));
                } else {
                    block.push_str(&format!(
                        "  - {}{}: engine={} (nessuna connection string configurata)\n",
                        name,
                        label,
                        engine.as_deref().unwrap_or("unknown")
                    ));
                }
            }
            block
        }
    };

    // Header contesto progetto dal DB (mig 0446): i DATI restano interpolati qui,
    // solo le frasi-cornice vivono nel template. Il codice aggiunge il \n\n finale.
    let _git_label = if proj.is_git_repo { "si" } else { "no" };
    let _root_str = proj.repository_root_path.display().to_string();
    let project_header = if let Some(ref summary) = analysis_summary {
        format!(
            "{}\n\n",
            crate::prompt_templates::get_template_or_default(
                &state.db,
                &state.template_cache,
                "system.project_context_header_with_summary",
            )
            .await
            .replace("{{project_name}}", &proj.details.name)
            .replace("{{project_root}}", &_root_str)
            .replace("{{is_git_repo}}", _git_label)
            .replace("{{db_connections_block}}", &db_connections_block)
            .replace("{{analysis_summary}}", summary)
        )
    } else {
        format!(
            "{}\n\n",
            crate::prompt_templates::get_template_or_default(
                &state.db,
                &state.template_cache,
                "system.project_context_header_no_summary",
            )
            .await
            .replace("{{project_name}}", &proj.details.name)
            .replace("{{project_root}}", &_root_str)
            .replace("{{is_git_repo}}", _git_label)
            .replace("{{db_connections_block}}", &db_connections_block)
        )
    };

    // Blocco RISORSE PROGETTO (punto unico resource_resolver, regola L):
    // stato runtime reale dei servizi del progetto (porte allocate + porte
    // realmente in ascolto + orfani del bucket), riconciliato dalle fonti
    // SEMPRE disponibili in WSL (DB + LISTEN, systemd best-effort). Iniettato a
    // valle di project_header e prima di automation_instructions, NON tocca il
    // template DB (non re-triggera l'offload del system). Stringa vuota se non
    // c'e' alcuna risorsa: niente blocco rumoroso. Corregge il loop request_port
    // (l'agente vedeva 0 risorse e variava la label per riallocare).
    let risorse_block = crate::project_workspace::resource_resolver::render_prompt_block(
        &state.db,
        &state.port_registry,
        params.project_id,
    )
    .await;

    // Istruzioni specifiche per modalità automazione: punto unico nel DB
    // (regola L). Stesse chiavi del path orchestrator (automation.mode_*_
    // instruction); prima erano DUPLICATE hardcoded qui, ignorando le rifiniture
    // DB e non modificabili a caldo. Il gate (quale modo) resta strutturale,
    // solo il TESTO viene dal DB via il punto unico get_template_or_default.
    let automation_instructions = {
        let key = params.automation_mode.prompt_instruction_template_key();
        let body =
            crate::prompt_templates::get_template_or_default(&state.db, &state.template_cache, key)
                .await;
        if body.trim().is_empty() {
            String::new()
        } else {
            format!("\n{}\n", body)
        }
    };

    // Istruzioni TDD per cicli test-fix-test iterativi
    let test_instructions = {
        let l = params.content.to_lowercase();
        let is_test_intent = l.contains("test")
            || l.contains("testa")
            || l.contains("verifica che funzion")
            || l.contains("tdd")
            || l.contains("fai passare");
        if is_test_intent {
            // Direttiva test-fix-test dal DB (regola G/L). Il gate (intent di
            // test) resta strutturale; solo il testo vive in system.test_fix_test_directive.
            let body = crate::prompt_templates::get_template_or_default(
                &state.db,
                &state.template_cache,
                "system.test_fix_test_directive",
            )
            .await;
            format!("\n{body}\n")
        } else {
            String::new()
        }
    };

    // Istruzioni specifiche per-progetto (auto-generate da analyze_project o modificate manualmente)
    let project_custom_instructions = custom_instructions_opt
        .filter(|s| !s.trim().is_empty())
        .map(|s| format!("\n{}\n", s))
        .unwrap_or_default();

    // Iniezione istruzione "precedente significativo": quando l'utente fa
    // una domanda meta auto-referenziale ("qual era l'ultima richiesta?",
    // "ripeti l'ultimo"), il LLM rischia di interpretare letteralmente
    // l'ultimo messaggio (la domanda stessa) invece di scalare al precedente
    // messaggio utente significativo. L'hint e' auto-aggiornato: include un
    // few-shot example tratto dalla cronologia reale di questa sessione.
    let self_ref_hint = build_self_referential_hint(&state.db, params.session_id, &params.content)
        .await
        .unwrap_or_default();

    // Istruzioni specifiche per modelli o-series (o1/o3/o4-mini): forzano
    // l'uso esplicito dei tool instead of narrare le azioni come testo.
    let o_series_instructions = if crate::brain_agent_client::is_o_series_model_pub(&model_str) {
        // Direttiva tool per modelli reasoning dal DB (regola G/L). Il gate
        // (modello o-series) resta strutturale; il testo vive in
        // system.reasoning_model_tool_directive.
        let body = crate::prompt_templates::get_template_or_default(
            &state.db,
            &state.template_cache,
            "system.reasoning_model_tool_directive",
        )
        .await;
        format!("\n{body}\n")
    } else {
        String::new()
    };

    let system_text = format!(
        "{}{}{}{}{}{}{}{}{}",
        project_header,
        risorse_block,
        project_custom_instructions,
        automation_instructions,
        o_series_instructions,
        test_instructions,
        params.profile_prompt_block,
        params.system_context,
        self_ref_hint
    );
    // Costruzione del messaggio iniziale arricchito con il contenuto reale
    // degli allegati (ADR 0010/0011/0012 — pre-extraction nel prompt).
    //
    // ROOT CAUSE storico: senza questo blocco, quando l'utente allega un file
    // (es. un .make Figma con la specifica di un'app) il modello riceve solo il
    // testo "crea l'app descritta nel file" SENZA il contenuto del file, e
    // finisce per allucinare o generare un Hello World. Qui pre-estraiamo il
    // contenuto autoritativo e lo iniettiamo in un blocco <allegati>.
    //
    // La history recente viene passata come turns strutturati via resume_history.
    // La costruzione del blocco <allegati> e' estratta in build_initial_msg_with_attachments
    // (funzione dedicata) per non gonfiare ulteriormente spawn_agent_run, che e'
    // gia' enorme: una closure complessa inline qui faceva degenerare il typeck
    // del compilatore (ICE).
    let initial_msg = build_initial_msg_with_attachments(
        &state.db,
        &params.content,
        &params.attachments,
        params.user_message_id,
        params.project_id,
        params.session_id,
    )
    .await;

    // Se questo run ha SUPERATO run attivi (last-wins), il modello vedra' nella
    // history della sessione il task precedente ancora "aperto" e tende a
    // continuarlo ignorando il nuovo messaggio (incidente reale: l'istruzione
    // error-fix M44 ha prodotto un run che ha proseguito i formatters del task
    // precedente). Nota OPERATIVA strutturale anteposta al prompt: fatto reale
    // (run interrotto), non euristica. Solo quando il supersede e' avvenuto.
    let initial_msg = if superseded_runs.is_empty() {
        initial_msg
    } else {
        // Nota arricchita dal worklog (mig 0411): sintesi del lavoro gia'
        // svolto dal run interrotto + puntatore al blocco <session_worklog>.
        // L'ingest e' gia' avvenuto SINCRONO dentro supersede_active_runs.
        let worklog_note = crate::session_worklog::supersede_summary(
            &state.db,
            params.session_id,
            &superseded_runs,
        )
        .await;
        format!(
            "[NOTA OPERATIVA — generata dal sistema]\nIl run precedente su questa \
             sessione e' stato INTERROTTO e sostituito da questo messaggio. Il task \
             corrente e' SOLO quello del messaggio qui sotto: non riprendere ne' \
             continuare il lavoro del run precedente se non e' esplicitamente \
             richiesto dal nuovo messaggio.{worklog_note}\n\n{initial_msg}"
        )
    };

    tracing::warn!(
        "TOKEN_OPT: system_text_len={} initial_msg_len={} recent_ctx_len={} history_turns={}",
        system_text.len(),
        initial_msg.len(),
        recent_context.len(),
        recent_history.len(),
    );

    let db_clone = state.db.clone();
    let channels_clone = state.agent_channels.clone();
    let session_id_cp = params.session_id;
    let project_id_cp = params.project_id;
    let user_message_id = params.user_message_id;
    // Cloni per i monitor automatici del pannello Monitor (regola H: il run si
    // auto-documenta senza dipendere dal fatto che il modello chiami
    // `dispatcher_update_monitor`). Usati dal task ascoltatore sotto e dal
    // monitor finale (completato/errore) emesso a fine run.
    let monitor_registry_for_run = state.monitor_registry.clone();
    let project_channels_for_run = state.project_channels.clone();
    // Cattura il provider che era stato impostato come preferenza di sessione.
    // Se al termine del run il gateway ha usato un provider locale diverso (vllm),
    // significa che è avvenuto un re-routing privacy → azzeriamo la preferenza.
    let requested_provider_clone = provider.clone();
    let had_session_override = effective_override.is_some();

    // Fase 4 del refactor Nexus: il loop agente gira sempre nel brain
    // LangGraph (Python). Non c'e' piu' un path AgentLoop locale.
    let provider_clone = provider.clone();
    let model_clone = model_str.clone();
    let initial_msg_clone = initial_msg.clone();
    let system_text_clone = system_text.clone();
    // Clono la routing matrix cache per il loop di fallback dentro lo spawn
    // (non posso catturare `state: &AppState` con lifetime locale dentro
    // `tokio::spawn(async move {...})` che richiede 'static).
    let routing_matrix_for_loop = state.orchestrator.routing_matrix.clone();
    let neural_for_embed = state.orchestrator.neural.clone();
    let recent_history_for_brain = recent_history;
    // L'intent classificato pilota la decisione di retry "hollow completion":
    // per chat/docs e' normale che il modello risponda senza tool, NON e' un
    // bug del modello e non giustifica un fallback su un modello piu' costoso.
    // `classified.intent` e' `&'static str` quindi e' Copy: nessun clone serve.
    let classified_intent_for_loop: &'static str = classified.intent;
    // Modalita' automazione propagata al brain (per clarify_or_expand skip).
    let automation_mode_for_brain: String = params.automation_mode.as_str().to_string();
    // Intent risolto dalla risposta di disambiguazione, propagato al brain
    // (None per i run normali: il router_node classifica come sempre).
    let intent_hint_for_brain: Option<String> = resolved_intent_hint.clone();

    // Calcola il payload tools dinamico (discovery mode vs inline) prima dello spawn.
    // Il filtering per automation_mode avviene dentro build_tools_json_for_agent:
    // in `study` esporta solo tool read-only (gating difensivo), in `confirm` e
    // `automatic` esporta la lista completa.
    let tools_json_for_brain = crate::brain_agent_client::build_tools_json_for_agent(
        &state.db,
        params.user_id,
        params.project_id,
        &params.automation_mode,
        &provider,
        &model_str,
    )
    .await;

    // Lettura della soglia SSE silence da settings (mig 0132). Cache 60s
    // tramite RoutingThresholdsCache: la doppia chiamata e' gratis.
    // Fallback al default tecnico (120s) se DB non disponibile.
    let sse_max_silence_secs: u64 =
        match state.orchestrator.routing_thresholds.current_async().await {
            Ok(t) => t.sse_heartbeat_max_silence_secs,
            Err(_) => 120,
        };

    // Cloni dedicati al panic-handler: se il corpo principale del tokio::spawn
    // panica, dobbiamo comunque emettere is_final e marcare il run come failed
    // nel DB. Senza questi cloni esterni, il `move` cattura tutto e il branch
    // di recovery non avrebbe accesso ai canali/DB. (Garanzia anti-blocco UI.)
    let panic_tx = tx_for_brain.clone();
    let panic_db = db_clone.clone();
    let panic_channels = channels_clone.clone();
    let panic_run_id = run_id;
    let panic_session_id = session_id_cp;
    let panic_project_id = project_id_cp;
    let panic_user_msg_id = user_message_id;

    // ── Monitor automatici del run (regola H, indipendenti dall'LLM) ─────────
    // Il pannello Monitor si popola guardando lo STREAM degli step del run
    // (eventi gia' prodotti dal parsing SSE del brain), non da chiamate del
    // modello a `dispatcher_update_monitor`. Un task dedicato si sottoscrive al
    // broadcast del run e traduce gli step in poche card chiave:
    //   - `agent_run`  -> stato del run ("in corso", poi "completato"/"errore")
    //   - `agent_tool` -> nome dell'ultimo tool eseguito (+ target file in label)
    //   - `agent_files`-> contatore file toccati (write_file/edit_file)
    // Sottoscriviamo PRIMA di spawnare il run cosi' nessun evento si perde.
    {
        let mut step_rx = tx_for_brain.subscribe();
        let mon_reg = monitor_registry_for_run.clone();
        let mon_ch = project_channels_for_run.clone();
        let mon_project = project_id_cp;
        // Stato iniziale immediato: il pannello mostra "in corso" appena parte.
        crate::agent_tools::monitor::set_monitor(
            &mon_reg,
            &mon_ch,
            mon_project,
            "agent_run",
            serde_json::Value::String("in corso".to_string()),
            Some("avvio run agente".to_string()),
        );
        tokio::spawn(async move {
            let mut files_touched: u64 = 0;
            loop {
                let ev = match step_rx.recv().await {
                    Ok(ev) => ev,
                    // Lagged: alcuni eventi persi (buffer pieno). Continuiamo:
                    // i monitor sono best-effort, non un log esaustivo.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    // Tutti i sender chiusi: il run e' finito. Lo stato finale
                    // (completato/errore) lo emette il corpo del run con
                    // result.status (qui non lo conosciamo). Usciamo.
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                if ev.is_final {
                    break;
                }
                let Some(step) = ev.step else { continue };
                // Aggiorna `agent_tool` quando un tool inizia (status Running):
                // value = nome tool, label = target file se ricavabile dall'input.
                if step.status == AgentStepStatus::Running && !step.tool_name.is_empty() {
                    let target = step
                        .tool_input
                        .get("file_path")
                        .or_else(|| step.tool_input.get("path"))
                        .or_else(|| step.tool_input.get("file"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string);
                    let label = target
                        .clone()
                        .map(|t| format!("step {} · {}", step.step_index, t))
                        .unwrap_or_else(|| format!("step {}", step.step_index));
                    crate::agent_tools::monitor::set_monitor(
                        &mon_reg,
                        &mon_ch,
                        mon_project,
                        "agent_tool",
                        serde_json::Value::String(step.tool_name.clone()),
                        Some(label),
                    );
                }
                // Contatore file toccati: incrementa quando write_file/edit_file
                // si completa con successo.
                if step.status == AgentStepStatus::Completed
                    && matches!(step.tool_name.as_str(), "write_file" | "edit_file")
                {
                    files_touched = files_touched.saturating_add(1);
                    crate::agent_tools::monitor::set_monitor(
                        &mon_reg,
                        &mon_ch,
                        mon_project,
                        "agent_files",
                        serde_json::Value::from(files_touched),
                        Some("file modificati".to_string()),
                    );
                }
            }
        });
    }

    // Clone di AppState per il finalize dentro il task 'static: serve a
    // narrative_or (mig 0415) per risolvere il purpose e chiamare il neural.
    // Cheap: i campi di AppState sono Arc/pool condivisi.
    let state_for_finalize = state.clone();

    tokio::spawn(async move {
        use futures::FutureExt;

        tracing::info!(
            "spawn_agent_run: delega al brain LangGraph run_id={}",
            run_id
        );

        let agent_body = std::panic::AssertUnwindSafe(async move {
            // ── Confine di selezione del motore (strangler-fig, Fase 0) ──────────
            // Punto unico select_engine: legge nexus_orchestrator_engine (regola
            // G). In Fase 0 ritorna SEMPRE Python -> si prosegue col flusso
            // run_via_brain esistente, comportamento INVARIATO. Il record
            // agent_runs.engine viene popolato per il recovery (sa su quale
            // motore girava un run interrotto). Il ramo nativo e' uno stub
            // (run_via_native) mai raggiunto finche' la tabella non abilita
            // 'rust'/'shadow'; se per errore venisse selezionato, si logga e si
            // cade in modo sicuro sul motore Python (nessun crash).
            let engine = select_engine(&db_clone, &session_id_cp.to_string()).await;
            let _ = sqlx::query("UPDATE agent_runs SET engine = $2 WHERE id = $1")
                .bind(run_id)
                .bind(match engine {
                    Engine::Python => "python",
                    Engine::Rust => "rust",
                    Engine::Shadow => "shadow",
                })
                .execute(&db_clone)
                .await;
            if engine != Engine::Python {
                // Mai in Fase 0. run_via_native e' uno stub che ritorna errore:
                // si logga e si prosegue sul motore Python (fail-safe).
                if let Err(e) = run_via_native(run_id).await {
                    tracing::warn!(
                        run_id = %run_id,
                        error = %e,
                        "motore nativo selezionato ma non implementato (Fase 0): fallback al motore Python"
                    );
                }
            }

            // ── Loop di retry con fallback automatico tra provider ───────────────
            // Se il run fallisce per "credit too low" / "quota exceeded", il provider
            // viene messo in cooldown lungo (in brain_agent_client). Qui rileviamo
            // il fallimento e ritentiamo con il prossimo provider della gerarchia
            // ammin (escludendo quelli in cooldown).
            //
            // Limite dinamico: tante iterazioni quanti sono i provider con almeno
            // un modello idoneo nel catalog (is_enabled + supports_tool_use +
            // consecutive_failures=0). Il +1 copre il tentativo iniziale. Floor=2
            // per garantire almeno un fallback se il catalog e' parziale.
            let max_provider_fallbacks: usize = {
                let n: i64 = sqlx::query_scalar(
                    "SELECT COUNT(DISTINCT provider)
                   FROM ai_price_catalog
                  WHERE is_enabled = true
                    AND supports_tool_use = true
                    AND agentic_thinking_policy <> 'exclude'
                    AND consecutive_failures = 0",
                )
                .fetch_one(&db_clone)
                .await
                .unwrap_or(4);
                std::cmp::max(2, (n as usize).saturating_add(1))
            };
            let provider_hierarchy: Vec<String> = {
                let row: Option<String> = sqlx::query_scalar(
                    "SELECT value FROM settings WHERE key = 'provider_hierarchy' LIMIT 1",
                )
                .fetch_optional(&db_clone)
                .await
                .ok()
                .flatten();
                row.map(|s| {
                    s.split(',')
                        .map(|t| t.trim().to_lowercase())
                        .filter(|t| !t.is_empty())
                        .collect()
                })
                .unwrap_or_else(|| {
                    vec![
                        "anthropic".into(),
                        "openai".into(),
                        "google".into(),
                        "deepseek".into(),
                        "mistral".into(),
                    ]
                })
            };

            let mut current_provider = provider_clone.clone();
            let mut current_model = model_clone.clone();
            let mut tried: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut result;
            let mut fallback_attempt: usize = 0;

            // ── Fix B+C: stima tokens richiesti e scelta context-aware ──────────
            // Approssimazione (1 token = ~4 caratteri): system prompt + msg utente
            // + storia conversazione + descrizioni tool. Usata per:
            //   B) troncare history se eccede 70% ctx del modello selezionato
            //   C) pre-filtrare il routing escludendo modelli con ctx insufficiente
            let estimated_input_chars: usize = {
                let history_chars: usize = recent_history_for_brain
                    .iter()
                    .map(|m| {
                        m.get("content")
                            .and_then(|c| c.as_str())
                            .map(|s| s.len())
                            .unwrap_or(0)
                    })
                    .sum();
                let tools_chars: usize = serde_json::to_string(&tools_json_for_brain)
                    .map(|s| s.len())
                    .unwrap_or(0);
                system_text_clone.len() + initial_msg_clone.len() + history_chars + tools_chars
            };
            let estimated_input_tokens: i64 = (estimated_input_chars / 4) as i64;
            tracing::info!(
                "agent_run {}: input stimato {} tokens (~{} chars)",
                run_id,
                estimated_input_tokens,
                estimated_input_chars
            );
            // Se il modello primario non ha context_window sufficiente (con margine
            // 30% per output), cerca subito un modello idoneo per ctx.
            let ctx_needed: i64 = (estimated_input_tokens as f64 * 1.3) as i64;
            // Idoneita' del primario: context_window sufficiente E eleggibilita'
            // agentica (supports_tool_use AND policy<>'exclude'), lette in un'unica
            // query. Il routing a monte (routing matrix) puo' aver scelto un modello
            // NON tool-capable (es. mistral-small-latest, supports_tool_use=false):
            // in un run agentico fallirebbe sistematicamente (422/MALFORMED/hollow).
            // Va sostituito SUBITO con un modello eleggibile, non solo quando il
            // context e' insufficiente. Cosi' il gate di capability vale anche per
            // il PRIMARIO, non solo per i fallback (prima era bypassato).
            // context_window e' INT4 in Postgres: il cast ::bigint evita il
            // type-mismatch i64/INT4 che faceva fallire la decodifica sqlx. Prima
            // l'errore veniva ingoiato da .ok() -> fallback (8192,false) ->
            // re-route SEMPRE attivo -> degrado a un modello piccolo anche quando
            // il routing aveva scelto un modello capace (regola G: niente fallback
            // magico che nasconde errori). Ora l'errore reale viene loggato.
            let (primary_ctx, primary_tool_ok): (i64, bool) = match sqlx::query_as(
                "SELECT context_window::bigint,
                        (supports_tool_use AND agentic_thinking_policy <> 'exclude')
                   FROM ai_price_catalog WHERE provider=$1 AND model=$2 LIMIT 1",
            )
            .bind(&current_provider)
            .bind(&current_model)
            .fetch_optional(&db_clone)
            .await
            {
                Ok(Some(row)) => row,
                Ok(None) => {
                    tracing::warn!(
                        "agent_run {}: {}/{} assente dal catalog per il check idoneita', \
                         fallback conservativo (8192, non-tool)",
                        run_id,
                        current_provider,
                        current_model
                    );
                    (8192, false)
                }
                Err(e) => {
                    tracing::error!(
                        "agent_run {}: query idoneita' context fallita per {}/{}: {e} \
                         (fallback conservativo)",
                        run_id,
                        current_provider,
                        current_model
                    );
                    (8192, false)
                }
            };
            if primary_ctx < ctx_needed || !primary_tool_ok {
                tracing::warn!(
                    "agent_run {}: primario {}/{} non idoneo (ctx {} < {} oppure non tool-capable: {}), re-route agentico",
                    run_id, current_provider, current_model, primary_ctx, ctx_needed, !primary_tool_ok
                );
                // Re-routing AGENTICO. PUNTO UNICO di selezione (regola L):
                // l'eleggibilita' agentica (tool_use, policy<>'exclude',
                // consecutive_failures, cooldown) e' definita una sola volta in
                // select_agentic_model. Vincolo extra: context_window >= ctx_needed.
                let alt = crate::orchestrator::select_agentic_model(
                    &db_clone,
                    &[],
                    None,
                    ctx_needed,
                    &[],
                    "input_cost_per_million_tokens ASC NULLS LAST",
                )
                .await;
                if let Some((p, m)) = alt {
                    tracing::info!(
                        "agent_run {}: re-route agentico: {} -> {}/{}",
                        run_id,
                        current_model,
                        p,
                        m
                    );
                    current_provider = p;
                    current_model = m;
                }
            }

            // ADR 0023 (Fix 3a): se il re-routing context-aware ha cambiato il
            // modello rispetto a quello registrato a spawn (provider_clone/
            // model_clone), allinea il record agent_runs al modello EFFETTIVO
            // con cui il run partira'. Cosi' header e badge dei meta-step (che
            // leggono agentRun.provider/model) convergono sul modello reale.
            // Best-effort: un fallimento qui non deve bloccare il run.
            if current_provider != provider_clone || current_model != model_clone {
                let _ =
                    sqlx::query("UPDATE agent_runs SET provider = $1, model = $2 WHERE id = $3")
                        .bind(&current_provider)
                        .bind(&current_model)
                        .bind(run_id)
                        .execute(&db_clone)
                        .await;
                tracing::info!(
                    "agent_run {}: agent_runs.provider/model aggiornato al modello effettivo {}/{} (era {}/{})",
                    run_id,
                    current_provider,
                    current_model,
                    provider_clone,
                    model_clone
                );
            }

            loop {
                tried.insert(current_provider.to_lowercase());
                tracing::info!(
                    "agent_run {}: tentativo {}/{} con provider={} model={} (ctx_needed={})",
                    run_id,
                    fallback_attempt + 1,
                    max_provider_fallbacks,
                    current_provider,
                    current_model,
                    ctx_needed
                );
                result = crate::brain_agent_client::run_via_brain(
                    run_id,
                    session_id_cp,
                    current_provider.clone(),
                    current_model.clone(),
                    system_text_clone.clone(),
                    initial_msg_clone.clone(),
                    tx_for_brain.clone(),
                    recent_history_for_brain.clone(),
                    tools_json_for_brain.clone(),
                    sse_max_silence_secs,
                    false, // emit_final_event: emesso manualmente dopo il break del retry loop
                    automation_mode_for_brain.clone(),
                    intent_hint_for_brain.clone(),
                    db_clone.clone(),
                )
                .await;

                // ── Detection errore infrastrutturale ───────────────────────────
                // Il ToolRunner/sandbox down NON e' colpa del modello: non
                // incrementare consecutive_failures e terminare senza scalare (gli
                // altri provider hanno lo stesso ToolRunner).
                // WAVE 2.2: fonte PRIMARIA = error_class STRUTTURATO "infrastructure"
                // emesso dal brain (tool_runner_client su gRPC UNAVAILABLE). Il
                // contains testuale su final_answer resta SOLO come fallback quando
                // il brain non ha propagato la classe (run vecchio), loggato.
                let is_infrastructure_error = if result.error_class.as_deref()
                    == Some("infrastructure")
                {
                    true
                } else {
                    let hit = result
                        .final_answer
                        .as_ref()
                        .map(|s| {
                            let lower = s.to_lowercase();
                            lower.contains("sandbox")
                                && (lower.contains("gr pc")
                                    || lower.contains("grpc")
                                    || lower.contains("connession")
                                    || lower.contains("non e' raggiungibile")
                                    || lower.contains("non raggiungibile"))
                                || lower.contains("50500")
                                || lower.contains("tool_runner")
                                || lower.contains("toolrunner")
                                || lower.contains("tcp handshaker")
                        })
                        .unwrap_or(false);
                    if hit {
                        tracing::info!(
                            "lexical_fallback_used: is_infrastructure_error (contains su final_answer)"
                        );
                    }
                    hit
                };
                if is_infrastructure_error {
                    tracing::warn!(
                    "agent_run {}: errore INFRASTRUTTURALE rilevato (ToolRunner/sandbox down) — \
                     non incremento consecutive_failures per {}/{}, termino senza fallback (altri \
                     provider hanno lo stesso ToolRunner)",
                    run_id, result.provider, result.model
                );
                    break;
                }

                // ── Counter hollow per modello (auto-disable) ────────────────────
                // Se il run e' hollow_completion REALE in produzione, incrementa
                // il counter consecutive_failures su ai_price_catalog. Questo e'
                // piu' affidabile del model_health_probe perche' rileva il problema
                // su workload reali (prompt lunghi, max_tokens reali) — non con
                // "ping" che a volte passa anche su modelli broken (es. gemini-3.5-flash
                // risponde a "ping" in 5s ma da hollow su prompt agente).
                //
                // Soglia 3 fallimenti consecutivi → is_enabled=false. Reset a 0 al
                // primo successo (status=Completed e final_answer NON vuoto).
                let intent_uses_tools = classified_intent_for_loop != "chat";
                if result.status.is_success() && intent_uses_tools {
                    let success_now = !result.hollow_completion
                        && result
                            .final_answer
                            .as_ref()
                            .map(|s| !s.trim().is_empty())
                            .unwrap_or(false);

                    // ── B: tool-failure model-specific (MALFORMED / output-vuoto su tool) ──
                    // `hollow_no_tools` = il modello aveva tool esposti ma non ne ha
                    // invocato nessuno al primo turno: e' il segnale runtime di
                    // finish_reason=MALFORMED_FUNCTION_CALL / output vuoto sul
                    // tool-forcing (es. gemini-2.5-pro sui task agentici). Questo NON
                    // significa che il modello sia rotto in assoluto: funziona per i
                    // task chat. Quindi NON tocchiamo is_enabled (che lo escluderebbe
                    // ANCHE dai task chat) ma incrementiamo un contatore DEDICATO
                    // (consecutive_tool_failures) e a soglia marchiamo
                    // supports_tool_use=false. L'auto-promoter, che per gli intent con
                    // requires_tool_use filtra su supports_tool_use, lo escludera' dai
                    // soli intent agentici lasciandolo per chat; il cleanup pass (A)
                    // disattivera' poi la riga matrix agentica gia' presente.
                    if result.hollow_no_tools {
                        let tool_threshold: i32 = crate::settings::get_setting(
                            &db_clone,
                            "agent.model_tool_failure_threshold",
                        )
                        .await
                        .ok()
                        .flatten()
                        .and_then(|v| v.trim().parse::<i32>().ok())
                        .filter(|n| *n > 0)
                        .unwrap_or(3);

                        // PUNTO UNICO (regola L): counter + degrado a soglia con
                        // guard capability_source='auto' vivono in tool_capability.
                        // Le righe curate a mano (manual) non vengono mai degradate
                        // dal runtime (incidente deepseek-v4, 2026-06-10).
                        let rec = crate::tool_capability::record_tool_failure(
                            &db_clone,
                            &result.provider,
                            &result.model,
                            tool_threshold,
                            crate::tool_capability::REASON_MALFORMED_TOOL_CALLS,
                        )
                        .await;
                        if let crate::tool_capability::ToolFailureRecord::Counted { failures }
                        | crate::tool_capability::ToolFailureRecord::MarkedNonToolCapable {
                            failures,
                        } = rec
                        {
                            tracing::warn!(
                            "agent_run {}: tool-failure (MALFORMED/empty su tool) su {}/{} — tool_counter={}/{}",
                            run_id, result.provider, result.model, failures, tool_threshold
                        );
                        }
                    } else if result.hollow_completion {
                        // Hollow generico NON dovuto al tool-forcing (empty answer /
                        // resigned con content): mantiene la semantica storica sul
                        // contatore consecutive_failures -> is_enabled=false a soglia 3.
                        let new_count: Option<i32> = sqlx::query_scalar(
                            "UPDATE ai_price_catalog
                            SET consecutive_failures = consecutive_failures + 1,
                                updated_at = NOW()
                          WHERE provider = $1 AND model = $2
                        RETURNING consecutive_failures",
                        )
                        .bind(&result.provider)
                        .bind(&result.model)
                        .fetch_optional(&db_clone)
                        .await
                        .ok()
                        .flatten();
                        if let Some(n) = new_count {
                            tracing::warn!(
                                "agent_run {}: hollow run reale su {}/{} — counter={}/3",
                                run_id,
                                result.provider,
                                result.model,
                                n
                            );
                            if n >= 3 {
                                let _ = sqlx::query(
                                    "UPDATE ai_price_catalog
                                    SET is_enabled = false,
                                        auto_disabled_at = NOW(),
                                        auto_disabled_reason = 'hollow_completion_runtime',
                                        updated_at = NOW()
                                  WHERE provider = $1 AND model = $2
                                    AND is_enabled = true",
                                )
                                .bind(&result.provider)
                                .bind(&result.model)
                                .execute(&db_clone)
                                .await;
                                tracing::warn!(
                                    "AUTO-DISABLE runtime {}/{} dopo {} hollow consecutivi",
                                    result.provider,
                                    result.model,
                                    n
                                );
                            }
                        }
                    } else if success_now {
                        // Turno-con-tool andato a buon fine: reset di ENTRAMBI i
                        // contatori (generico e tool-specific) e riabilita la
                        // tool-capability se il degrado era automatico, da
                        // QUALUNQUE fonte (runtime O tool-probe) — punto unico.
                        crate::tool_capability::reset_tool_failures_on_success(
                            &db_clone,
                            &result.provider,
                            &result.model,
                            true,
                        )
                        .await;
                    }
                }

                // Decide se ritentare: nuova logica basata su error_class strutturato
                // propagato dal brain via SSE, oltre allo stato cooldown del provider.
                // Casi che giustificano un retry su altro provider:
                //   - provider in cooldown (lungo o breve, gia' marcato dal brain_agent_client)
                //   - error_class in {billing_error, rate_limit, provider_error}
                //   - il run e' fallito con stop_reason=error (anche senza classify, ritenta una volta)
                //   - hollow_completion: il modello ha risposto senza usare tool (0 step)
                let failed_retry = matches!(result.status, AgentRunStatus::Failed) && {
                    let in_cooldown =
                        crate::provider_cooldown::is_provider_in_cooldown(&current_provider);
                    let retriable_class = matches!(
                        result.error_class.as_deref(),
                        Some("billing_error") | Some("rate_limit") | Some("provider_error")
                    );
                    in_cooldown || retriable_class
                };
                // Hollow completion: il modello ha risposto senza usare tool.
                // Per intent `chat` (chiacchierata, domande conversazionali,
                // meta-domande) la risposta senza tool e' attesa e corretta —
                // disabilitiamo il retry. Per altri intent (anche `docs` quando
                // l'utente chiede di scrivere/leggere documentazione) il retry
                // serve perche' il modello dovrebbe usare tool.
                //
                // Intent AUTORITATIVO: quello del router del brain propagato in
                // nexus_task_type (segnale del classifier LLM). WAVE 4
                // (de-lessicalizzazione): se il brain ha fornito l'intent, e' LUI
                // a decidere se il run d'azione hollow va ritentato (intent !=
                // "chat") — niente piu' OR con le keyword di detect_action_request
                // sull'initial_msg, che introducevano falsi positivi (una chat con
                // la parola "crea" forzava un retry inutile). Il keyword resta SOLO
                // come fallback quando il brain NON ha propagato l'intent (caso
                // degradato), loggato come lexical_fallback_used.
                let action_intent = match result.nexus_task_type.as_deref() {
                    Some(intent) => intent != "chat",
                    None => {
                        let kw = crate::agent_types::detect_action_request(&initial_msg_clone);
                        if kw {
                            tracing::info!(
                                "lexical_fallback_used: hollow_retry detect_action_request (brain_intent assente)"
                            );
                        }
                        kw || classified_intent_for_loop != "chat"
                    }
                };
                let hollow_retry = result.hollow_completion && action_intent;
                let should_retry = failed_retry || hollow_retry;

                if !should_retry || fallback_attempt + 1 >= max_provider_fallbacks {
                    break;
                }

                if hollow_retry {
                    tracing::warn!(
                        "agent_run {}: hollow completion da {}/{} — il modello ha risposto \
                     senza usare tool, ritento con un modello piu capace",
                        run_id,
                        current_provider,
                        current_model
                    );
                }

                // ── ESCALATION su hollow ricorrente ─────────────────────────────
                // Se gia' 1 hollow nel run (questo e' il 2o tentativo dopo hollow),
                // smetti di girare in tondo sui modelli small e scala al primo
                // modello "di ordine superiore" disponibile nel catalog:
                // performance_tier='heavy' AND is_enabled, ordinato per qualita'
                // (costo input desc = proxy di capacita'). Provider-agnostic:
                // sceglie qualunque heavy disponibile non gia' tried/in-cooldown.
                //
                // Esempi attesi (sort cost desc):
                //   anthropic/claude-opus-4-7 > openai/gpt-5 > anthropic/claude-sonnet-4-6
                //   > mistral/mistral-large-latest > google/gemini-2.5-pro > deepseek/deepseek-reasoner
                //
                // Conta come "hollow precedente" se hollow_retry == true ora E
                // questo e' fallback_attempt >= 1 (cioe' siamo gia' al 2o turno).
                let escalate_on_hollow = hollow_retry && fallback_attempt >= 1;
                let next_pair: Option<(String, String)> = if escalate_on_hollow {
                    let tried_models: Vec<String> = tried.iter().cloned().collect();
                    // Escalation su hollow ricorrente: PUNTO UNICO di selezione
                    // (regola L). Eleggibilita' agentica + cooldown definiti una
                    // sola volta in select_agentic_model. Esclude i provider gia'
                    // provati; preferisce i piu' "potenti" (tier desc, costo desc) e
                    // con context_window sufficiente.
                    crate::orchestrator::select_agentic_model(
                        &db_clone,
                        &[],
                        None,
                        ctx_needed,
                        &tried_models,
                        "CASE performance_tier WHEN 'heavy' THEN 2 WHEN 'medium' THEN 1 ELSE 0 END DESC, \
                         input_cost_per_million_tokens DESC NULLS LAST, \
                         output_cost_per_million_tokens DESC NULLS LAST",
                    )
                    .await
                    .map(|(p, m)| {
                        tracing::warn!(
                            "agent_run {}: ESCALATION hollow ricorrente — salto a {}/{} (selettore unico)",
                            run_id, p, m
                        );
                        (p, m)
                    })
                } else {
                    None
                };

                let (chosen_provider, chosen_model) = if let Some(pair) = next_pair {
                    pair
                } else {
                    // Cerca il prossimo provider nella gerarchia che sia:
                    //   - non gia' provato in questo run
                    //   - non in cooldown billing/quota
                    //   - dotato di un default model in nexus_provider_default_model
                    //   - con coppia (provider, model) coerente (guard-rail anti-mismatch)
                    //
                    // INVARIANTE: provider e model devono SEMPRE appartenere allo
                    // stesso provider. Un provider senza default model viene SKIPPATO
                    // nel fallback, mai accoppiato al model del provider precedente.
                    // Fonte di verita: nexus_provider_default_model (regola G); i
                    // prefix in model_belongs_to_provider sono detection. Vedi ADR 0016.
                    //
                    // Se la routing_matrix non e disponibile non si puo decidere un
                    // model coerente -> break (manteniamo il result corrente).
                    let matrix_arc = match routing_matrix_for_loop.current_async().await {
                        Ok(m) => m,
                        Err(e) => {
                            tracing::error!(
                            "agent_run {}: routing_matrix non disponibile ({}), interrompo fallback e mantengo risultato",
                            run_id, e
                        );
                            break;
                        }
                    };
                    let mut chosen: Option<(String, String)> = None;
                    for candidate in provider_hierarchy.iter() {
                        if tried.contains(candidate)
                            || crate::provider_cooldown::is_provider_in_cooldown(candidate)
                        {
                            continue;
                        }
                        let Some(candidate_model) = matrix_arc.default_model(candidate) else {
                            tracing::warn!(
                            "agent_run {}: provider '{}' senza default model in nexus_provider_default_model, skip nel fallback",
                            run_id, candidate
                        );
                            continue;
                        };
                        // Guard-rail: la coppia (provider, model) deve essere coerente.
                        // Previene QUALSIASI mismatch: se il default model non
                        // appartiene al provider, NON tentiamo la chiamata (404).
                        if !model_belongs_to_provider(candidate, &candidate_model) {
                            tracing::error!(
                            "agent_run {}: coppia incoerente provider='{}' model='{}' in nexus_provider_default_model, skip nel fallback",
                            run_id, candidate, candidate_model
                        );
                            continue;
                        }
                        chosen = Some((candidate.clone(), candidate_model));
                        break;
                    }
                    let Some(pair) = chosen else {
                        tracing::warn!(
                        "agent_run {}: nessun provider alternativo con default model coerente disponibile, mantengo risultato",
                        run_id
                    );
                        break;
                    };
                    pair
                };
                // Invariante difensiva finale: anche i candidati da escalation
                // hollow (next_pair) passano per il guard-rail. Una coppia
                // incoerente non deve mai diventare current_provider/model.
                if !model_belongs_to_provider(&chosen_provider, &chosen_model) {
                    tracing::error!(
                    "agent_run {}: coppia incoerente scelta provider='{}' model='{}', interrompo fallback (guard-rail)",
                    run_id, chosen_provider, chosen_model
                );
                    break;
                }
                current_provider = chosen_provider;
                current_model = chosen_model;
                fallback_attempt += 1;
                tracing::warn!(
                    "agent_run {}: fallback automatico a {}/{} (motivo: {})",
                    run_id,
                    current_provider,
                    current_model,
                    if hollow_retry {
                        "hollow completion"
                    } else {
                        "provider error/cooldown"
                    }
                );
                // Meta-step `fallback` pubblicato in chat per trasparenza:
                // utente vede in tempo reale che il sistema ha cambiato
                // provider/modello (es. anthropic -> openai per quota_exceeded).
                let reason = if hollow_retry {
                    "hollow_completion"
                } else {
                    "provider_error_or_cooldown"
                };
                let _ = tx_for_brain.send(AgentStepEvent {
                    run_id: run_id.to_string(),
                    step: None,
                    trace: None,
                    is_final: false,
                    token_delta: None,
                    thinking_delta: None,
                    meta_step: Some(crate::agent_types::AgentMetaStep {
                        kind: "fallback".to_string(),
                        title: format!("Fallback su {}/{}", current_provider, current_model),
                        payload: serde_json::json!({
                            "to_provider": current_provider,
                            "to_model": current_model,
                            "reason": reason,
                            "attempt": fallback_attempt,
                        }),
                        correlation_id: None,
                        created_at: chrono::Utc::now().to_rfc3339(),
                    }),
                });
            }
            // Emetti is_final solo DOPO la fine del retry loop, cosi' il
            // frontend non chiude lo stream SSE dopo il primo tentativo fallito
            // perdendo i successivi tentativi di fallback.
            let _ = tx_for_brain.send(AgentStepEvent {
                run_id: run_id.to_string(),
                step: None,
                trace: None,
                is_final: true,
                token_delta: None,
                thinking_delta: None,
                meta_step: None,
            });
            channels_clone.remove(&run_id);

            // Se il gateway ha re-instradato su provider locale per privacy
            // (il provider finale differ da quello richiesto ed è "vllm" o altro locale),
            // azzeriamo la preferenza di sessione → al prossimo messaggio torna il routing automatico.
            let privacy_rerouted = had_session_override
                && result.provider != requested_provider_clone
                && matches!(result.provider.as_str(), "vllm" | "local" | "ollama");
            if privacy_rerouted {
                clear_session_preferred_provider_after_privacy(&db_clone, session_id_cp).await;
            }

            // ── Hollow completion: il modello ha dichiarato di aver completato
            // senza invocare alcun tool. Per intent `chat` questo e' atteso (il
            // brain azzera i tool: chat diretta) e NON va segnalato come avviso.
            // Intent AUTORITATIVO dal router del brain (nexus_task_type), non la
            // pre-classificazione locale di mcp-core: quest'ultima divergeva e,
            // combinata con had_tools=true (mcp-core passa i tool, il brain li
            // azzera), produceva l'avviso "0 tool / risposta generica" fuorviante
            // sulle chat dirette. Fallback al locale se il task_type non c'e'.
            let effective_intent = result
                .nexus_task_type
                .as_deref()
                .unwrap_or(classified_intent_for_loop);
            let conversational_intent = effective_intent == "chat";
            let report_hollow = result.hollow_completion && !conversational_intent;
            if report_hollow {
                tracing::warn!(
                    "agent_run {}: hollow completion rilevato — il modello {}/{} \
                 non ha eseguito alcun tool. La risposta potrebbe essere incompleta.",
                    run_id,
                    result.provider,
                    result.model
                );

                // ── QW2: diagnostica persistente in nexus_provider_empty_responses ──
                // Toggle via setting agent.diagnostics.empty_response_log_enabled.
                let diag_enabled: bool = sqlx::query_scalar::<_, String>(
                    "SELECT value FROM settings WHERE key = 'agent.diagnostics.empty_response_log_enabled'",
                )
                .fetch_optional(&db_clone)
                .await
                .ok()
                .flatten()
                .map(|v| v.trim().eq_ignore_ascii_case("true"))
                .unwrap_or(true);

                if diag_enabled {
                    let max_bytes: usize = sqlx::query_scalar::<_, String>(
                        "SELECT value FROM settings WHERE key = 'agent.diagnostics.empty_response_excerpt_max_bytes'",
                    )
                    .fetch_optional(&db_clone)
                    .await
                    .ok()
                    .flatten()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(8192usize);

                    let raw = result.final_answer.as_deref().unwrap_or("");
                    let excerpt: String = if raw.len() > max_bytes {
                        let mut end = max_bytes;
                        while !raw.is_char_boundary(end) && end > 0 {
                            end -= 1;
                        }
                        format!("{}\n[...truncated at {} bytes...]", &raw[..end], max_bytes)
                    } else {
                        raw.to_string()
                    };

                    let suspected = match result.hollow_completion_kind.as_str() {
                        "EMPTY_ANSWER" | "EMPTY_ANSWER+NO_TOOLS" => {
                            if raw.trim().is_empty() {
                                "empty_completion_unknown"
                            } else {
                                "empty_after_text"
                            }
                        }
                        "RESIGNED" => "resigned_after_few_steps",
                        "NO_TOOLS" => "no_tool_calls",
                        _ => "unknown",
                    };

                    let _ = sqlx::query(
                        r#"
                        INSERT INTO nexus_provider_empty_responses
                            (agent_run_id, chat_session_id, project_id, provider, model,
                             intent, kind, iteration, steps_count, final_answer_chars,
                             est_input_tokens, est_output_tokens,
                             raw_response_excerpt, suspected_cause)
                        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
                        "#,
                    )
                    .bind(run_id)
                    .bind(session_id_cp)
                    .bind(project_id_cp)
                    .bind(&result.provider)
                    .bind(&result.model)
                    .bind(classified_intent_for_loop as &str)
                    .bind(if result.hollow_completion_kind.is_empty() {
                        "UNKNOWN"
                    } else {
                        result.hollow_completion_kind.as_ref()
                    })
                    .bind(result.iteration_count as i32)
                    .bind(result.steps.len() as i32)
                    .bind(raw.len() as i32)
                    .bind(result.prompt_tokens as i32)
                    .bind(result.completion_tokens as i32)
                    .bind(&excerpt)
                    .bind(suspected)
                    .execute(&db_clone)
                    .await
                    .inspect_err(|e| {
                        tracing::debug!(
                            "diagnostica empty_response: INSERT best-effort fallita: {e}"
                        );
                    });
                }
            }

            // Save final answer as assistant message.
            // Se l'agente ha completato ma final_answer e' None o whitespace-only
            // (caso hollow EMPTY_ANSWER, es. deepseek-coder che chiude il turno
            // senza emettere body), generiamo comunque un messaggio chiaro per
            // l'utente — altrimenti la UI mostra solo lo status "completed"
            // senza alcun contenuto, lasciando l'utente con l'impressione che il
            // sistema abbia "fatto qualcosa" che in realta' non e' avvenuto.
            let answer_owned: Option<String> = match result.final_answer.as_ref() {
                Some(s) if !s.trim().is_empty() => {
                    // Fix "riepilogo finale garantito": se l'agente ha modificato
                    // file ma la risposta (es. frase interlocutoria non conclusiva)
                    // non li menziona, allega il riepilogo deterministico delle
                    // azioni cosi' l'utente vede sempre cosa e' stato fatto.
                    let mut a = s.clone();
                    if let Some(footer) = action_recap_footer(&a, &result.steps) {
                        a.push_str(&footer);
                    }
                    Some(a)
                }
                // Final answer mancante o vuoto: siamo qui DOPO il retry loop
                // (hollow_completion confermato e tentativi esauriti). Se l'agente
                // ha comunque ESEGUITO azioni concrete (tool completati), produci
                // un recap deterministico (ADR 0025) cosi' l'utente vede cosa e'
                // stato fatto invece di un generico "nessuna risposta". Solo se
                // non c'e' alcuna azione si usa il placeholder generico.
                _ if report_hollow => build_action_recap(&result.steps)
                    .or_else(cooldown_exhaustion_note)
                    .or_else(|| {
                        Some(format!(
                            "_(Nessuna risposta utile prodotta dall'agente — {} / {} ha chiuso \
                     il turno con un completamento vuoto dopo aver esaurito i tentativi \
                     di fallback. Riformula la richiesta o cambia provider/modello manualmente.)_",
                            result.provider, result.model
                        ))
                    }),
                _ => None,
            };

            // Recap NARRATIVO opzionale (mig 0415, gate off di default): a gate
            // attivo, i run hollow ricevono una sintesi LLM al posto del recap
            // secco. Punto unico narrative_or; fallback al deterministico.
            let answer_owned = narrative_or(&state_for_finalize, &result, answer_owned).await;

            if let Some(ref answer) = answer_owned {
                // Annota la risposta solo se l'intent richiedeva tool e l'agente
                // ha prodotto un body (per evitare doppia annotazione sul placeholder).
                let had_real_body = result
                    .final_answer
                    .as_ref()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false);
                let effective_answer = if report_hollow && had_real_body {
                    format!(
                        "{answer}\n\n---\n*Avviso: l'agente ({}/{}) ha risposto senza \
                     eseguire alcun tool (0 step). La risposta potrebbe essere \
                     incompleta o generica. Riprova con un modello piu' capace \
                     o riformula la richiesta.*",
                        result.provider, result.model
                    )
                } else {
                    answer.clone()
                };
                // Riepilogo esecuzione SEMPRE in coda (numeri reali dagli step):
                // l'utente vede cosa e' stato fatto anche se la risposta e'
                // interlocutoria/confusa. None per i turni senza azioni.
                let effective_answer = match outcome_summary(&result.steps) {
                    Some(s) => format!("{effective_answer}{s}"),
                    None => effective_answer,
                };
                let meta = json!({
                    "provider": &result.provider,
                    "model": &result.model,
                    "agentRunId": run_id.to_string(),
                    "iterationCount": result.iteration_count,
                    "automationMode": "agent",
                    "privacyRerouted": privacy_rerouted,
                    "hollowCompletion": result.hollow_completion,
                    // Usage tracking: senza questi campi il TokenUsageBar resta
                    // invisibile (la query in billing::get_session_usage somma
                    // metadata->>'totalTokens'). I valori sono gia' calcolati e
                    // scritti su agent_runs subito sotto.
                    "promptTokens": result.prompt_tokens,
                    "completionTokens": result.completion_tokens,
                    "totalTokens": result.total_tokens,
                    "totalCost": result.total_cost,
                    "currency": "USD",
                });
                let _ = sqlx::query(
                r#"INSERT INTO chat_messages
                   (id, session_id, project_id, role, content, metadata, request_message_id, created_at)
                   VALUES (gen_random_uuid(),$1,$2,'assistant',$3,$4,$5,NOW())"#,
            )
            .bind(session_id_cp)
            .bind(project_id_cp)
            .bind(&effective_answer)
            .bind(meta)
            .bind(user_message_id)
            .execute(&db_clone)
            .await;

                spawn_embed_conversation_turn(
                    neural_for_embed.clone(),
                    db_clone.clone(),
                    session_id_cp,
                    Uuid::new_v4(),
                    "assistant".to_string(),
                    effective_answer.clone(),
                );
            }

            // Update run status in DB
            //
            // Esito CERTO (mai "completed" vuoto): un run hollow confermato dopo
            // l'esaurimento dei retry, che NON ha eseguito alcuna azione e non ha
            // prodotto risposta (EMPTY_ANSWER con steps vuoti) oppure ha rinunciato
            // esplicitamente (RESIGNED), non e' un successo: marcarlo 'completed'
            // mostrava all'utente un esito ambiguo ("completato" + nessun
            // contenuto). Lo si declassa all'esito canonico failed_diagnosed: la
            // diagnosi e' il placeholder/recap gia' composto sopra (answer_owned).
            // Un hollow con step completati resta invece Completed (il lavoro c'e',
            // manca solo la conferma testuale del modello).
            let hollow_no_work = report_hollow
                && ((result.steps.is_empty()
                    && result.hollow_completion_kind.contains("EMPTY_ANSWER"))
                    || result.hollow_completion_kind == "RESIGNED");
            let status_canonical = if hollow_no_work {
                tracing::warn!(
                    "agent_run {}: hollow senza lavoro ({}) — status declassato \
                     completed -> failed_diagnosed (esito certo)",
                    run_id,
                    result.hollow_completion_kind
                );
                crate::agent_types::AgentRunStatus::FailedDiagnosed
            } else {
                result.status.clone()
            };
            let status_str = status_canonical.as_str();
            // final_answer nel run record: per i run vuoti persiste il
            // placeholder/diagnosi (answer_owned) cosi' run e messaggio chat
            // raccontano lo stesso esito; per i run normali resta la risposta.
            let final_answer_db: Option<String> = match result.final_answer.as_deref() {
                Some(s) if !s.trim().is_empty() => Some(s.to_string()),
                _ => answer_owned.clone(),
            };
            // ADR 0023 (Fix 3a): aggiorna anche provider/model col valore
            // EFFETTIVO usato dal run (result.provider/result.model). Cattura i
            // cascade fallback avvenuti dentro il loop (es. primario -> openai
            // su billing_error), che il blocco context-aware pre-loop non vede.
            // L'header, leggendo agentRun dopo getAgentRun(), mostra il modello
            // reale dell'esecuzione, non quello registrato a spawn.
            let _ = sqlx::query(
                "UPDATE agent_runs SET status=$2, final_answer=$3, iteration_count=$4, \
             prompt_tokens=$5, completion_tokens=$6, total_tokens=$7, total_cost=$8, \
             nexus_override_applied=$9, nexus_agent_type=$10, nexus_task_type=$11, \
             provider=$12, model=$13, \
             completed_at=NOW() WHERE id=$1",
            )
            .bind(run_id)
            .bind(status_str)
            .bind(final_answer_db.as_deref())
            .bind(result.iteration_count as i32)
            .bind(result.prompt_tokens as i32)
            .bind(result.completion_tokens as i32)
            .bind(result.total_tokens as i32)
            .bind(result.total_cost)
            .bind(result.nexus_override_applied)
            .bind(result.nexus_agent_type.as_deref())
            .bind(result.nexus_task_type.as_deref())
            .bind(&result.provider)
            .bind(&result.model)
            .execute(&db_clone)
            .await;

            // ── Monitor finale del run (regola H, indipendente dall'LLM) ───────
            // Porta la card `agent_run` allo stato terminale. Non cancelliamo i
            // monitor: restano visibili come ultimo stato del run.
            let (run_state, run_label): (&str, String) = match status_canonical {
                AgentRunStatus::Completed | AgentRunStatus::CompletedVerified => (
                    "completato",
                    format!(
                        "{} step · {} iter",
                        result.steps.len(),
                        result.iteration_count
                    ),
                ),
                AgentRunStatus::AwaitingConfirmation => (
                    "in attesa conferma",
                    "conferma utente richiesta".to_string(),
                ),
                AgentRunStatus::FailedDiagnosed => (
                    "non completato",
                    "diagnosi e prossimo passo disponibili".to_string(),
                ),
                AgentRunStatus::BlockedNeedsInput => {
                    ("bloccato", "richiede input o conferma".to_string())
                }
                _ => (
                    "errore",
                    result
                        .error_class
                        .clone()
                        .unwrap_or_else(|| status_str.to_string()),
                ),
            };
            crate::agent_tools::monitor::set_monitor(
                &monitor_registry_for_run,
                &project_channels_for_run,
                project_id_cp,
                "agent_run",
                serde_json::Value::String(run_state.to_string()),
                Some(run_label),
            );

            // ── G4: memorizza startup_command dopo avvio servizio riuscito ─────
            // Se il run è completato con successo e ha eseguito un `docker compose up`,
            // salva il comando in memory_entries → al turno successivo l'agente lo
            // trova in "Memoria di progetto" e sa già cosa eseguire.
            if status_canonical.is_success() {
                crate::agent_types::save_startup_command_if_needed(
                    &db_clone,
                    project_id_cp,
                    &result.steps,
                )
                .await;
            }

            // ── Budget tracking ──────────────────────────────────────────────
            // Incrementa il `spent_current_period_usd` per il provider del run.
            // Strategia comune a tutti i 5 provider visto che la maggior parte
            // (anthropic/openai/google/mistral) non espone balance via API: il
            // budget va stimato sommando il cost dei run reali.
            //
            // Calcolo del cost:
            //   - Se brain ha propagato result.total_cost > 0 -> usalo.
            //   - Altrimenti: calcolo da prompt_tokens/completion_tokens × prezzi
            //     dal catalog (caso comune: brain non emette total_cost nelle
            //     SSE events, ma propaga i token usage che sono affidabili).
            let cost_to_charge: f64 =
                if result.total_cost > 0.0 {
                    result.total_cost
                } else if result.prompt_tokens > 0 || result.completion_tokens > 0 {
                    // Look up prezzi dal catalog. Costo per milione di token.
                    #[derive(sqlx::FromRow)]
                    struct PriceRow {
                        input_cost: f64,
                        output_cost: f64,
                    }
                    let prices: Option<PriceRow> = sqlx::query_as::<_, PriceRow>(
                        "SELECT input_cost_per_million_tokens::float8 AS input_cost,
                        output_cost_per_million_tokens::float8 AS output_cost
                   FROM ai_price_catalog
                  WHERE provider = $1 AND model = $2 AND is_enabled = true
                  ORDER BY effective_from DESC LIMIT 1",
                    )
                    .bind(&result.provider)
                    .bind(&result.model)
                    .fetch_optional(&db_clone)
                    .await
                    .ok()
                    .flatten();
                    if let Some(p) = prices {
                        let input_cost = (result.prompt_tokens as f64) * p.input_cost / 1_000_000.0;
                        let output_cost =
                            (result.completion_tokens as f64) * p.output_cost / 1_000_000.0;
                        let total = input_cost + output_cost;
                        if total > 0.0 {
                            // Aggiorna anche agent_runs.total_cost per coerenza UI.
                            let _ = sqlx::query(
                        "UPDATE agent_runs SET total_cost = $2 WHERE id = $1 AND total_cost = 0",
                    )
                    .bind(run_id)
                    .bind(total)
                    .execute(&db_clone)
                    .await;
                            tracing::debug!(
                        "budget: cost calcolato da Rust per {}/{} = ${:.6} (prompt={} comp={})",
                        result.provider, result.model, total,
                        result.prompt_tokens, result.completion_tokens
                    );
                        }
                        total
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };
            if cost_to_charge > 0.0 {
                let _ = sqlx::query(
                "INSERT INTO provider_budget_status (provider, spent_current_period_usd)
                   VALUES ($1, $2)
                 ON CONFLICT (provider) DO UPDATE
                   SET spent_current_period_usd = provider_budget_status.spent_current_period_usd + EXCLUDED.spent_current_period_usd,
                       updated_at = NOW()",
            )
            .bind(&result.provider)
            .bind(cost_to_charge)
            .execute(&db_clone)
            .await;
            }

            // Persisti gli step del run su agent_steps (fix bug: la tabella veniva letta
            // da chat_agent.rs:121,195 ma non scritta — dashboard "AI Workspace" mostrava
            // sempre storia vuota, reflection non poteva correlare step con outcome).
            // Gli step sono gia' raccolti in-memory dal brain_agent_client durante il loop SSE.
            if !result.steps.is_empty() {
                for step in &result.steps {
                    let _ = sqlx::query(
                    "INSERT INTO agent_steps \
                     (id, run_id, step_index, tool_name, tool_input, tool_result, status, created_at) \
                     VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6, NOW())",
                )
                .bind(run_id)
                .bind(step.step_index as i32)
                .bind(&step.tool_name)
                .bind(&step.tool_input)
                .bind(step.tool_result.as_deref())
                .bind(step.status.as_str())
                .execute(&db_clone)
                .await;
                }
                tracing::debug!(
                    "agent_run {}: {} step persistiti in agent_steps",
                    run_id,
                    result.steps.len()
                );

                // Worklog di sessione (mig 0411): deriva gli eventi operativi
                // strutturati dagli step e rinfresca il digest provider-neutro
                // che il brain inietta nei run successivi della sessione.
                // Best-effort: un errore qui non tocca l'esito del run.
                if let Err(e) = crate::session_worklog::ingest_steps_for_run(
                    &db_clone,
                    session_id_cp,
                    Some(project_id_cp),
                    run_id,
                    status_str,
                    &result.steps,
                )
                .await
                {
                    tracing::warn!(error = %e, "session_worklog: ingest a fine run fallito");
                }
            }
        }); // chiude AssertUnwindSafe(async move { ... })

        // Cattura panic dell'intero body: senza questo, un panic dentro lo
        // spawn lascia il run con status='running' per sempre e l'UI bloccata
        // (il canale broadcast non riceve mai is_final).
        if let Err(panic_payload) = agent_body.catch_unwind().await {
            let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "panic non-stringificato".to_string()
            };
            tracing::error!(
                "agent_run {}: PANIC catturato nel tokio::spawn — emetto is_final di fallback. Payload: {}",
                panic_run_id, panic_msg
            );

            // 1. Emetti is_final per sbloccare l'UI
            let _ = panic_tx.send(AgentStepEvent {
                run_id: panic_run_id.to_string(),
                step: None,
                trace: None,
                is_final: true,
                token_delta: None,
                thinking_delta: None,
                meta_step: None,
            });
            panic_channels.remove(&panic_run_id);

            // 2. Aggiorna agent_runs come failed
            let _ = sqlx::query(
                "UPDATE agent_runs SET status='failed', completed_at=NOW(), \
                 final_answer=$2 WHERE id=$1",
            )
            .bind(panic_run_id)
            .bind(format!(
                "Errore interno: il task agente e' terminato in modo imprevisto ({}). Riprova.",
                panic_msg
            ))
            .execute(&panic_db)
            .await;

            // 3. Inserisci un messaggio assistant per far vedere l'errore in chat
            let _ = sqlx::query(
                r#"INSERT INTO chat_messages
                   (id, session_id, project_id, role, content, metadata, request_message_id, created_at)
                   VALUES (gen_random_uuid(),$1,$2,'assistant',$3,$4,$5,NOW())"#,
            )
            .bind(panic_session_id)
            .bind(panic_project_id)
            .bind(format!(
                "⚠ Errore interno: il task agente e' terminato in modo imprevisto.\n\n```\n{}\n```\n\nPuoi riprovare la richiesta.",
                panic_msg
            ))
            .bind(json!({"errorClass": "internal_panic", "agentRunId": panic_run_id.to_string()}))
            .bind(panic_user_msg_id)
            .execute(&panic_db)
            .await;
        }
    });

    SpawnOutcome::Started(SpawnAgentResult {
        run_id,
        provider,
        model: model_str,
    })
}

// ===========================================================================
// Confine di selezione del motore di orchestrazione (strangler-fig, Fase 0).
//
// Punto UNICO (regola L) di decisione "quale motore esegue questo run":
// `select_engine`. Il flusso esistente `run_via_brain` (motore Python) NON e'
// toccato; si affianca solo il confine + lo stub `run_via_native`, mai
// raggiunto in Fase 0 (la fonte DB ritorna sempre 'python'). Rischio di
// regressione NULLO.
// ===========================================================================

/// Motore di orchestrazione con cui un run viene eseguito.
///
/// Persistito su `agent_runs.engine` (per il recovery) e deciso da
/// `select_engine` leggendo `nexus_orchestrator_engine` (regola G: la fonte e'
/// il DB, niente env var ne' default hardcoded di emergenza).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Engine {
    /// Motore corrente: orchestrazione nel brain Python/LangGraph (`run_via_brain`).
    Python,
    /// Motore nativo Rust (`nexus-agent-graph`). Non ancora raggiungibile in Fase 0.
    Rust,
    /// Doppia esecuzione di confronto (parita'): primario Python + ombra Rust
    /// senza side-effect. Non ancora raggiungibile in Fase 0.
    Shadow,
}

impl Engine {
    /// Parsing dal valore TEXT del DB. Sconosciuto -> `None` (il chiamante
    /// decide: in Fase 0 cade sul default sicuro 'python' loggando un warn,
    /// senza mascherare il dato malformato).
    fn from_db(value: &str) -> Option<Engine> {
        match value.trim().to_ascii_lowercase().as_str() {
            "python" => Some(Engine::Python),
            "rust" => Some(Engine::Rust),
            "shadow" => Some(Engine::Shadow),
            _ => None,
        }
    }
}

/// Chiave jolly (default globale) in `nexus_orchestrator_engine`.
const ENGINE_GLOBAL_SCOPE: &str = "*";

static ENGINE_CACHE: std::sync::OnceLock<nexus_cache::TtlCache<String, Engine>> =
    std::sync::OnceLock::new();

/// Cache TTL 60s del routing motore (stesso pattern di model_selection.rs,
/// punto unico cache regola L). La chiave e' lo `scope_key` risolto.
fn engine_cache() -> &'static nexus_cache::TtlCache<String, Engine> {
    ENGINE_CACHE.get_or_init(|| nexus_cache::TtlCache::new(std::time::Duration::from_secs(60)))
}

/// Decide il motore di orchestrazione per il run corrente (PUNTO UNICO).
///
/// Legge `nexus_orchestrator_engine` (riga jolly '*' come default globale, mig
/// 0451) con cache 60s. Regola G: nessun fallback hardcoded di emergenza — la
/// configurazione vive nel DB. In FASE 0 la riga '*' vale 'python', quindi la
/// funzione ritorna SEMPRE `Engine::Python` e il path Rust non viene mai
/// imboccato; la firma e' gia' pronta per il routing per-run (Fase 5).
///
/// Comportamento difensivo coerente con il resto del sistema: se il DB e'
/// irraggiungibile o la riga manca, ritorna `Engine::Python` (il motore
/// stabile) loggando un warn. NON e' un "magic fallback" sul comportamento
/// configurabile (il motore di default e' una decisione di safety, non un
/// valore di business mascherato): mantiene il sistema sull'unico motore
/// validato finche' la tabella non abilita esplicitamente quello nativo.
pub(crate) async fn select_engine(db: &PgPool, _scope_key: &str) -> Engine {
    // In Fase 0 il routing e' solo globale: si legge sempre la riga jolly '*'.
    // La firma accetta gia' lo scope per non rompere i call site quando il
    // routing per-sessione/progetto verra' attivato (Fase 5).
    let cache = engine_cache();
    if let Some(hit) = cache.get(ENGINE_GLOBAL_SCOPE) {
        return hit;
    }

    let row = sqlx::query("SELECT engine FROM nexus_orchestrator_engine WHERE scope_key = $1")
        .bind(ENGINE_GLOBAL_SCOPE)
        .fetch_optional(db)
        .await;

    let engine = match row {
        Ok(Some(r)) => {
            let raw: String = r.get("engine");
            match Engine::from_db(&raw) {
                Some(e) => e,
                None => {
                    tracing::warn!(
                        engine_raw = %raw,
                        "select_engine: valore engine non riconosciuto in nexus_orchestrator_engine, uso il motore stabile (python)"
                    );
                    Engine::Python
                }
            }
        }
        Ok(None) => {
            tracing::warn!(
                "select_engine: riga jolly '*' assente in nexus_orchestrator_engine (mig 0451 applicata?), uso il motore stabile (python)"
            );
            Engine::Python
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "select_engine: query nexus_orchestrator_engine fallita, uso il motore stabile (python)"
            );
            Engine::Python
        }
    };

    cache.insert(ENGINE_GLOBAL_SCOPE.to_string(), engine);
    engine
}

/// Avvio di un run sul motore nativo Rust (`nexus-agent-graph`).
///
/// STUB di Fase 0: mai raggiunto (`select_engine` ritorna sempre `Python`). La
/// firma esiste per stabilire il confine; l'implementazione (build del grafo,
/// `run_until_interrupt`, transport SSE) arriva nelle fasi successive del
/// porting. Ritorna un errore esplicito invece di `unimplemented!()` per non
/// far panicare il processo se un futuro refactoring lo invocasse per errore
/// prima del completamento.
pub(crate) async fn run_via_native(run_id: Uuid) -> anyhow::Result<()> {
    anyhow::bail!(
        "run_via_native non e' ancora implementato (Fase 0 scaffold): il motore nativo Rust \
         non e' raggiungibile finche' nexus_orchestrator_engine non abilita 'rust'/'shadow'. \
         run_id={run_id}"
    )
}

#[cfg(test)]
mod tests_select_engine {
    use super::*;

    /// Crea la tabella minimale di routing motore nel DB di test.
    async fn create_engine_table(pool: &sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE nexus_orchestrator_engine ( \
                 scope_key  TEXT PRIMARY KEY, \
                 scope_kind TEXT NOT NULL DEFAULT 'global', \
                 engine     TEXT NOT NULL DEFAULT 'python', \
                 percent    INT NOT NULL DEFAULT 100, \
                 updated_at TIMESTAMPTZ NOT NULL DEFAULT now() \
             )",
        )
        .execute(pool)
        .await
        .expect("create table nexus_orchestrator_engine");
    }

    #[test]
    fn engine_from_db_parsing() {
        assert_eq!(Engine::from_db("python"), Some(Engine::Python));
        assert_eq!(Engine::from_db("RUST"), Some(Engine::Rust));
        assert_eq!(Engine::from_db(" shadow "), Some(Engine::Shadow));
        assert_eq!(Engine::from_db("boh"), None);
    }

    #[sqlx::test]
    async fn select_engine_ritorna_python_con_default_globale(pool: sqlx::PgPool) {
        create_engine_table(&pool).await;
        sqlx::query(
            "INSERT INTO nexus_orchestrator_engine (scope_key, scope_kind, engine, percent) \
             VALUES ('*', 'global', 'python', 100)",
        )
        .execute(&pool)
        .await
        .expect("insert default");

        let engine = select_engine(&pool, "qualsiasi-scope").await;
        assert_eq!(engine, Engine::Python, "il default globale Fase 0 e' python");
    }

    #[sqlx::test]
    async fn select_engine_cade_su_python_se_riga_assente(pool: sqlx::PgPool) {
        // Tabella vuota: nessuna riga jolly. Comportamento difensivo: python.
        create_engine_table(&pool).await;
        let engine = select_engine(&pool, "*").await;
        assert_eq!(engine, Engine::Python);
    }

    #[tokio::test]
    async fn run_via_native_e_uno_stub_che_fallisce() {
        let err = run_via_native(Uuid::new_v4())
            .await
            .expect_err("lo stub di Fase 0 deve fallire, mai eseguire");
        assert!(err.to_string().contains("non e' ancora implementato"));
    }
}

#[cfg(test)]
mod tests_session_active {
    use super::*;

    async fn create_agent_runs_table(pool: &sqlx::PgPool) {
        sqlx::query(
            "CREATE TABLE agent_runs ( \
                 id UUID NOT NULL DEFAULT gen_random_uuid(), \
                 session_id UUID NOT NULL, \
                 status TEXT NOT NULL \
             )",
        )
        .execute(pool)
        .await
        .expect("create table agent_runs");
    }

    #[sqlx::test]
    async fn vero_su_running(pool: sqlx::PgPool) {
        create_agent_runs_table(&pool).await;
        let sess = Uuid::new_v4();
        sqlx::query("INSERT INTO agent_runs (session_id, status) VALUES ($1, 'running')")
            .bind(sess)
            .execute(&pool)
            .await
            .expect("insert running");
        assert!(session_has_active_run(&pool, sess).await);
    }

    #[sqlx::test]
    async fn vero_su_awaiting_confirmation(pool: sqlx::PgPool) {
        create_agent_runs_table(&pool).await;
        let sess = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_runs (session_id, status) VALUES ($1, 'awaiting_confirmation')",
        )
        .bind(sess)
        .execute(&pool)
        .await
        .expect("insert awaiting");
        assert!(session_has_active_run(&pool, sess).await);
    }

    #[sqlx::test]
    async fn falso_se_solo_run_conclusi(pool: sqlx::PgPool) {
        create_agent_runs_table(&pool).await;
        let sess = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent_runs (session_id, status) \
             VALUES ($1, 'completed'), ($1, 'cancelled'), ($1, 'failed')",
        )
        .bind(sess)
        .execute(&pool)
        .await
        .expect("insert conclusi");
        assert!(!session_has_active_run(&pool, sess).await);
    }

    #[sqlx::test]
    async fn isolamento_per_sessione(pool: sqlx::PgPool) {
        create_agent_runs_table(&pool).await;
        let sess_a = Uuid::new_v4();
        let sess_b = Uuid::new_v4();
        // Solo sess_b ha un run attivo.
        sqlx::query("INSERT INTO agent_runs (session_id, status) VALUES ($1, 'running')")
            .bind(sess_b)
            .execute(&pool)
            .await
            .expect("insert running sess_b");
        assert!(!session_has_active_run(&pool, sess_a).await);
        assert!(session_has_active_run(&pool, sess_b).await);
    }
}

#[cfg(test)]
mod tests_finalize_turn {
    use super::*;
    use crate::agent_types::{AgentRunResult, AgentRunStatus, AgentStep};

    fn mk_result(
        status: AgentRunStatus,
        steps: Vec<AgentStep>,
        final_answer: Option<&str>,
        hollow_completion: bool,
        hollow_kind: &str,
        task_type: Option<&str>,
    ) -> AgentRunResult {
        AgentRunResult {
            run_id: "r".into(),
            status,
            steps,
            pending_actions: vec![],
            final_answer: final_answer.map(str::to_string),
            provider: "mistral".into(),
            model: "mistral-large".into(),
            iteration_count: 1,
            nexus_override_applied: false,
            nexus_agent_type: None,
            nexus_q_value: None,
            provider_privacy_notice: None,
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            total_cost: 0.0,
            last_prompt_tokens: None,
            error_class: None,
            stop_reason: None,
            nexus_task_type: task_type.map(str::to_string),
            hollow_completion,
            hollow_no_tools: false,
            hollow_completion_kind: hollow_kind.into(),
        }
    }

    fn write_step() -> AgentStep {
        AgentStep {
            run_id: "r".into(),
            step_index: 1,
            tool_name: "write_file".into(),
            tool_input: serde_json::json!({"path": "src/a.ts"}),
            tool_result: Some("ok".into()),
            status: AgentStepStatus::Completed,
            created_at: String::new(),
        }
    }

    fn rename_step() -> AgentStep {
        AgentStep {
            run_id: "r".into(),
            step_index: 2,
            tool_name: "rename_file".into(),
            tool_input: serde_json::json!({"from": "figma_export/src/app", "to": "src/app"}),
            tool_result: Some("ok".into()),
            status: AgentStepStatus::Completed,
            created_at: String::new(),
        }
    }

    #[test]
    fn collect_actions_include_rename_to() {
        // Incidente Beauty-Book: run di soli rename aveva files_touched VUOTO,
        // quindi il recap non poteva contraddire un resoconto che dichiarava
        // path ormai svuotati dai rename stessi. La destinazione del rename E'
        // un file toccato.
        let (lines, files) = collect_actions(&[rename_step()]);
        assert!(!lines.is_empty());
        assert!(
            files.contains("src/app"),
            "files_touched deve includere il 'to' del rename: {files:?}"
        );
    }

    #[test]
    fn hollow_senza_lavoro_declassato_a_failed_diagnosed() {
        // 0 step + EMPTY_ANSWER -> failed_diagnosed (esito certo, mai completed muto).
        let r = mk_result(
            AgentRunStatus::Completed, vec![], None, true,
            "EMPTY_ANSWER", Some("fix"),
        );
        assert_eq!(canonical_run_status(&r), AgentRunStatus::FailedDiagnosed);
        // RESIGNED -> failed_diagnosed anche con risposta.
        let r2 = mk_result(
            AgentRunStatus::Completed, vec![], Some("non posso"), true,
            "RESIGNED", Some("fix"),
        );
        assert_eq!(canonical_run_status(&r2), AgentRunStatus::FailedDiagnosed);
    }

    #[test]
    fn hollow_con_lavoro_resta_completed() {
        // Hollow ma con step produttivi -> NON declassato (il lavoro c'e').
        let r = mk_result(
            AgentRunStatus::Completed, vec![write_step()], None, true,
            "EMPTY_ANSWER", Some("fix"),
        );
        assert_eq!(canonical_run_status(&r), AgentRunStatus::Completed);
    }

    #[test]
    fn chat_hollow_non_e_report_hollow() {
        // intent chat: completamento vuoto atteso, non declassato.
        let r = mk_result(
            AgentRunStatus::Completed, vec![], None, true,
            "EMPTY_ANSWER", Some("chat"),
        );
        assert_eq!(canonical_run_status(&r), AgentRunStatus::Completed);
    }

    #[test]
    fn compose_garantisce_messaggio_su_hollow() {
        // Hollow con step -> recap delle azioni (mai messaggio assente).
        let r = mk_result(
            AgentRunStatus::Completed, vec![write_step()], None, true,
            "EMPTY_ANSWER", Some("fix"),
        );
        let msg = compose_turn_answer(&r).expect("recap atteso");
        assert!(msg.contains("a.ts"), "il recap deve elencare il file toccato");
        // Hollow senza step -> placeholder esplicito.
        let r2 = mk_result(
            AgentRunStatus::Completed, vec![], None, true,
            "EMPTY_ANSWER", Some("fix"),
        );
        let msg2 = compose_turn_answer(&r2).expect("placeholder atteso");
        assert!(msg2.contains("Nessuna risposta utile"));
    }

    #[test]
    fn compose_usa_risposta_reale_quando_presente() {
        let r = mk_result(
            AgentRunStatus::Completed, vec![], Some("Ecco il risultato."), false,
            "", Some("fix"),
        );
        assert_eq!(compose_turn_answer(&r).unwrap(), "Ecco il risultato.");
    }

    #[test]
    fn compose_none_se_non_hollow_e_senza_risposta() {
        // Run non-hollow senza final_answer (es. chat che chiude): nessun messaggio.
        let r = mk_result(
            AgentRunStatus::Completed, vec![], None, false, "", Some("chat"),
        );
        assert!(compose_turn_answer(&r).is_none());
    }

    #[test]
    fn textual_tool_call_rilevata_e_sostituita_dal_recap() {
        // Caso reale run 5ec12cad: final_answer = tool call colata nel testo.
        let raw = "read_file\n{\"path\": \"src/services/bookingService.ts\"}";
        assert!(super::looks_like_textual_tool_call(raw));
        // Risposte legittime NON matchano.
        assert!(!super::looks_like_textual_tool_call("Il DB ha 6 tabelle."));
        assert!(!super::looks_like_textual_tool_call(
            "read_file e' un tool che legge i file."
        ));
        assert!(!super::looks_like_textual_tool_call("{\"ok\": true}"));
        // compose: con step produttivi -> recap, non la spazzatura.
        let r = mk_result(
            AgentRunStatus::Completed, vec![write_step()], Some(raw), false,
            "", Some("fix"),
        );
        let msg = compose_turn_answer(&r).expect("recap atteso");
        assert!(msg.contains("a.ts"), "deve usare il recap deterministico: {msg}");
        assert!(!msg.contains("bookingService"), "niente tool call nel testo: {msg}");
    }

    #[test]
    fn soli_errori_provider_rilevati_e_sostituiti() {
        // Caso reale run 2c6e41fb: final_answer = concatenazione di 422 Mistral.
        let raw = "[Error: An assistant message with][Error: Unexpected tool call id FbW0bLZsv in tool results][Error: An assistant message with]";
        assert!(super::is_only_provider_errors(raw));
        // Risposte legittime (anche se citano errori) NON matchano.
        assert!(!super::is_only_provider_errors("Il build fallisce con [Error: x]"));
        assert!(!super::is_only_provider_errors("Il DB ha 6 tabelle."));
        assert!(!super::is_only_provider_errors(""));
        // compose: con step -> recap, non la concatenazione di errori.
        let r = mk_result(
            AgentRunStatus::Completed, vec![write_step()], Some(raw), false,
            "", Some("fix"),
        );
        let msg = compose_turn_answer(&r).expect("recap atteso");
        assert!(msg.contains("a.ts"), "recap deterministico atteso: {msg}");
        assert!(!msg.contains("FbW0bLZsv"), "niente errori grezzi: {msg}");
    }

    #[test]
    fn cooldown_note_vuoto_se_nessun_cooldown() {
        // Nessun provider in cooldown -> None: vale il placeholder generico.
        assert!(super::cooldown_note_from_snapshot(&[]).is_none());
    }

    #[test]
    fn cooldown_note_billing_dice_ricarica() {
        // Provider buoni in cooldown per credito/quota -> messaggio ONESTO che
        // indica la causa reale e l'azione, non "completamento vuoto".
        let snap = vec![
            ("anthropic".to_string(), 300u64, Some("credit balance too low".to_string())),
            ("openai".to_string(), 250u64, Some("you exceeded your current quota".to_string())),
        ];
        let msg = super::cooldown_note_from_snapshot(&snap).expect("nota attesa");
        assert!(msg.contains("quota/credito esaurito"), "deve indicare la causa billing: {msg}");
        assert!(msg.contains("Ricarica"), "deve suggerire la ricarica: {msg}");
        assert!(msg.contains("anthropic") && msg.contains("openai"), "deve elencare i provider: {msg}");
    }

    #[test]
    fn cooldown_note_non_billing_dice_attendi() {
        // Cooldown transitorio (rate-limit) -> "attendi", NON "ricarica".
        let snap = vec![("mistral".to_string(), 60u64, Some("rate limit".to_string()))];
        let msg = super::cooldown_note_from_snapshot(&snap).expect("nota attesa");
        assert!(msg.contains("temporaneamente non disponibili"), "non-billing -> attendi: {msg}");
        assert!(!msg.contains("Ricarica"), "non deve dire ricarica per rate-limit: {msg}");
    }
}

use super::*;

/// Parametri condivisi per avviare un agent run (usato da send e resend).
#[derive(Clone)]
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
    // Concatenazione (Rust `[T]::join`, non SQL JOIN) estratta su riga a se':
    // evita il falso positivo del detector line-based su push_str + join.
    let corpo = lines.join("\n");
    out.push_str(&corpo);
    if !files_touched.is_empty() {
        let files: Vec<String> = files_touched.iter().map(|f| format!("`{f}`")).collect();
        let files_list = files.join(", ");
        out.push_str(&format!("\n\nFile creati/modificati: {files_list}"));
    }
    out.push_str(
        "\n\n_(Riepilogo generato automaticamente: l'agente ha eseguito le azioni \
         sopra ma non ha prodotto un messaggio finale. Verifica i risultati.)_",
    );
    Some(out)
}

/// Abbrevia un path file per la visualizzazione nel recap: normalizza i
/// backslash Windows in slash, e se ci sono piu' di 2 segmenti mostra solo gli
/// ultimi due preceduti da `.../`. Parita' 1:1 con la mappa di `buildSemanticDetail`
/// (run-summary.ts righe 106-110): cosi' il recap persistito e quello live LIVE
/// coincidono dopo un refresh.
fn short_file_label(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').collect();
    if parts.len() > 2 {
        // Concatenazione dei segmenti (Rust `[T]::join`, non SQL JOIN): su riga a
        // se' per non far scattare il detector line-based (join + format!).
        let tail = parts[parts.len() - 2..].join("/");
        format!(".../{tail}")
    } else {
        path.to_string()
    }
}

/// Riepilogo RICCO SEMPRE in coda alla risposta: conteggi e dettagli REALI dagli
/// step (file modificati con nome breve, comandi eseguiti, file analizzati, esito
/// step/errori), indipendenti dalla narrativa dell'agente. Cosi' l'utente capisce
/// a colpo d'occhio cosa e' stato fatto anche quando la risposta e'
/// interlocutoria/confusa o troncata.
///
/// PUNTO UNICO DEL RECAP (regola L / regola H): produce lo STESSO blocco oggi
/// composto LIVE dal frontend in `apps/web-ide/lib/use-chat/run-summary.ts`
/// `buildSemanticDetail` (righe 66-128). Prima divergeva: questo backend
/// persisteva una riga secca ("N file modificati, M azioni eseguite") mentre il
/// frontend live mostrava il blocco ricco -> dopo un refresh (che ricostruisce
/// dal DB) la chat cambiava aspetto. Replicando il formato esatto qui, il
/// `content` persistito in `chat_messages` coincide col recap mostrato LIVE,
/// eliminando la divergenza F5.
///
/// `None` se non c'e' alcuna azione concreta (ne' file modificati, ne' comandi,
/// ne' file analizzati): turno conversazionale, stessa decisione di
/// `buildSemanticDetail` (return "" -> qui `None`).
/// Conteggi e collezioni deduplicate estratti dagli step per il recap ricco.
/// Popolato una volta da [`tally_steps`] e consumato dal compositore
/// `outcome_summary` (punto unico della classificazione step->recap, regola L).
struct StepTally {
    /// Path dei file creati/modificati, deduplicati, in ordine di prima comparsa.
    modified_files: Vec<String>,
    /// Comandi eseguiti, troncati e deduplicati (parita' run-summary.ts).
    commands: Vec<String>,
    /// Numero di step di lettura/ricerca (file "analizzati").
    analysis_count: usize,
    /// Step falliti.
    error_count: usize,
    /// Step completati.
    completed_count: usize,
}

/// Path del file toccato da un tool di scrittura (chiavi alternative in ordine
/// di preferenza: path/file_path/filename). Pura, nessun side-effect.
fn extract_write_path(step: &AgentStep) -> Option<&str> {
    step.tool_input
        .get("path")
        .or_else(|| step.tool_input.get("file_path"))
        .or_else(|| step.tool_input.get("filename"))
        .and_then(|v| v.as_str())
        .filter(|p| !p.is_empty())
}

/// Comando eseguito da un tool di terminale (chiavi command/cmd/text), gia'
/// troncato a 77 char + "..." per parita' con run-summary.ts (80 char). Pura.
fn extract_command_short(step: &AgentStep) -> Option<String> {
    let c = step
        .tool_input
        .get("command")
        .or_else(|| step.tool_input.get("cmd"))
        .or_else(|| step.tool_input.get("text"))
        .and_then(|v| v.as_str())
        .filter(|c| !c.is_empty())?;
    let short = if c.chars().count() > 80 {
        format!("{}...", c.chars().take(77).collect::<String>())
    } else {
        c.to_string()
    };
    Some(short)
}

/// Classifica gli step del run nei conteggi/collezioni del recap. Incapsula
/// TUTTO il loop di match tool-group + estrazione path/comando + dedup +
/// troncamento, cosi' il compositore `outcome_summary` resta lineare (esce da
/// long-fn e complexity-high). Comportamento 1:1 con la versione inline
/// precedente (dedup per prima comparsa, troncamento comandi a 77 char + "...").
fn tally_steps(steps: &[AgentStep]) -> StepTally {
    const WRITE_TOOLS: &[&str] = &["write_file", "edit_file", "create_file", "patch_file"];
    const CMD_TOOLS: &[&str] = &["run_in_terminal", "run_command"];
    const READ_TOOLS: &[&str] = &["read_file", "search_in_files", "search_files"];
    const IGNORE_TOOLS: &[&str] = &["supervisor_check"];

    let mut tally = StepTally {
        modified_files: Vec::new(),
        commands: Vec::new(),
        analysis_count: 0,
        error_count: 0,
        completed_count: 0,
    };

    for step in steps {
        let tool = step.tool_name.as_str();
        if IGNORE_TOOLS.contains(&tool) {
            continue;
        }
        if step.status == AgentStepStatus::Failed {
            tally.error_count += 1;
        }
        if step.status == AgentStepStatus::Completed {
            tally.completed_count += 1;
        }

        if WRITE_TOOLS.contains(&tool) {
            if let Some(p) = extract_write_path(step) {
                if !tally.modified_files.iter().any(|f| f == p) {
                    tally.modified_files.push(p.to_string());
                }
            }
        } else if CMD_TOOLS.contains(&tool) {
            if let Some(short) = extract_command_short(step) {
                if !tally.commands.iter().any(|x| x == &short) {
                    tally.commands.push(short);
                }
            }
        } else if READ_TOOLS.contains(&tool) {
            tally.analysis_count += 1;
        }
    }

    tally
}

/// Riga "- Modificati N file: ..." del recap. `None` se nessun file modificato.
/// Mostra al massimo i primi 5 con nome breve, poi "e altri K file".
fn render_modified_files_line(files: &[String]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    const MAX_FILES: usize = 5;
    let shown: Vec<String> = files
        .iter()
        .take(MAX_FILES)
        .map(|f| format!("`{}`", short_file_label(f)))
        .collect();
    let extra = if files.len() > MAX_FILES {
        format!(" e altri {} file", files.len() - MAX_FILES)
    } else {
        String::new()
    };
    // Concatenazione (Rust `[T]::join`, non SQL JOIN) su riga a se': evita il
    // falso positivo del detector line-based su format! + join.
    let shown_list = shown.join(", ");
    Some(format!(
        "- Modificati {} file: {shown_list}{extra}",
        files.len()
    ))
}

/// Riga "- Eseguiti N comandi: ..." del recap. `None` se nessun comando.
/// Mostra al massimo i primi 3, poi "e altri K".
fn render_commands_line(commands: &[String]) -> Option<String> {
    if commands.is_empty() {
        return None;
    }
    const MAX_CMDS: usize = 3;
    let shown: Vec<String> = commands
        .iter()
        .take(MAX_CMDS)
        .map(|c| format!("`{c}`"))
        .collect();
    let extra = if commands.len() > MAX_CMDS {
        format!(" e altri {}", commands.len() - MAX_CMDS)
    } else {
        String::new()
    };
    // Concatenazione (Rust `[T]::join`, non SQL JOIN) su riga a se': evita il
    // falso positivo del detector line-based su format! + join.
    let shown_list = shown.join(", ");
    Some(format!(
        "- Eseguiti {} comandi: {shown_list}{extra}",
        commands.len()
    ))
}

fn outcome_summary(steps: &[AgentStep]) -> Option<String> {
    let tally = tally_steps(steps);

    // Turno conversazionale (nessuna azione significativa): nessun recap.
    if tally.modified_files.is_empty() && tally.commands.is_empty() && tally.analysis_count == 0 {
        return None;
    }

    let mut lines: Vec<String> = Vec::new();
    if let Some(line) = render_modified_files_line(&tally.modified_files) {
        lines.push(line);
    }
    if let Some(line) = render_commands_line(&tally.commands) {
        lines.push(line);
    }
    if tally.analysis_count > 0 {
        lines.push(format!("- Analizzati {} file", tally.analysis_count));
    }
    let errors_suffix = if tally.error_count > 0 {
        format!(", {} errori", tally.error_count)
    } else {
        String::new()
    };
    lines.push(format!(
        "- Risultato: {} step completati{errors_suffix}",
        tally.completed_count
    ));

    // Concatenazione (Rust `[T]::join`, non SQL JOIN) su riga a se': evita il
    // falso positivo del detector line-based su format! + join.
    let joined = lines.join("\n");
    Some(format!("\n\n**Riepilogo:**\n{joined}"))
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
    // Concatenazioni (Rust `[T]::join`, non SQL JOIN) estratte su righe a se':
    // evita il falso positivo del detector line-based su push_str + join.
    let corpo = lines.join("\n");
    out.push_str(&corpo);
    let files: Vec<String> = files_touched.iter().map(|f| format!("`{f}`")).collect();
    let files_list = files.join(", ");
    out.push_str(&format!("\n\nFile creati/modificati: {files_list}"));
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
/// vuota) a `FailedDiagnosed` — mai un "completed" muto (esito certo).
/// L'hollow CON step completati resta lo status originale. La rinuncia
/// esplicita non passa piu' da qui: e' dichiarata via task_complete
/// (refusal/blocked, ADR 0034 — la detection lessicale RESIGNED e' stata
/// rimossa, ADR 0018 fase 3).
pub(crate) fn canonical_run_status(result: &crate::agent_types::AgentRunResult) -> AgentRunStatus {
    if is_report_hollow(result) {
        let no_work =
            result.steps.is_empty() && result.hollow_completion_kind.contains("EMPTY_ANSWER");
        if no_work {
            return AgentRunStatus::FailedDiagnosed;
        }
    }
    // Esito certo (errore provider): un run chiuso `completed` la cui sola
    // risposta e' il messaggio di errore provider (final_answer "[Errore
    // provider ...]") senza completion_tokens e' un fallimento infrastrutturale,
    // non un successo. Punto unico: is_provider_error_completion.
    if is_provider_error_completion(result) {
        return AgentRunStatus::Failed;
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
///
/// GATE (regola M, BUG 3 failover): la nota viene prodotta SOLO quando la causa
/// STRUTTURATA del turno corrente e' attribuibile all'indisponibilita' dei provider
/// COINVOLTI nel turno, non da uno snapshot di cooldown globale ortogonale. Senza
/// questo gate, un turno fallito per una causa diversa (es. `empty_completion` di
/// google, che ha risposto 200 vuoto e NON e' in cooldown) veniva attribuito a
/// openai/anthropic solo perche' quei due erano in cooldown billing per motivi
/// propri, mostrando "ricarica i crediti" e nominando i provider sbagliati.
fn cooldown_exhaustion_note(result: &crate::agent_types::AgentRunResult) -> Option<String> {
    cooldown_note_for_turn(&crate::provider_cooldown::cooldown_snapshot(), result)
}

/// True se la classe errore STRUTTURATA del turno indica che il provider coinvolto
/// e' finito in cooldown (billing o throttle transiente), non un completamento
/// vuoto o altra causa ortogonale. Segnale macchina (regola M): nessun parsing di
/// prosa. Le classi sono quelle emesse dal brain
/// (`agent_turn_setup::classify_provider_error`).
fn error_class_indicates_cooldown(error_class: Option<&str>) -> bool {
    matches!(
        error_class,
        Some(
            "billing_error"
                | "insufficient_quota"
                | "rate_limit"
                | "overloaded"
                | "service_unavailable"
                | "bad_gateway"
                | "provider_error"
        )
    )
}

/// Logica pura (testabile, regola F) del gate BUG 3: dato lo snapshot dei cooldown
/// e il `result` del turno, decide se la nota cooldown/ricarica e' pertinente e su
/// quali provider. Discriminazione sui SEGNALI STRUTTURATI del turno (regola M),
/// mai dallo stato globale ortogonale:
///   - `status == ProviderUnavailable`: il routing non ha trovato ALCUN provider
///     disponibile (tutti in cooldown). Il turno e' fallito PROPRIO per questo:
///     l'intero snapshot e' la causa legittima -> nota su tutti i provider.
///   - `error_class` cooldown-related: il provider DEL TURNO ha colpito un errore
///     billing/throttle. Lo snapshot va filtrato al provider coinvolto
///     (`result.provider`): la nota nomina solo lui, non provider ortogonali.
///   - altrimenti (es. `empty_completion` di google mentre openai/anthropic sono
///     in cooldown per motivi propri): `None`, vale il placeholder a valle che
///     nomina il `result.provider` reale.
fn cooldown_note_for_turn(
    snap: &[(String, u64, Option<String>)],
    result: &crate::agent_types::AgentRunResult,
) -> Option<String> {
    // Caso 1: il turno e' fallito perche' NESSUN provider era disponibile.
    // Segnale strutturato dal routing (no_capable_provider). L'intero snapshot
    // e' la causa legittima del turno.
    if result.status == crate::agent_types::AgentRunStatus::ProviderUnavailable {
        return cooldown_note_from_snapshot(snap);
    }
    // Caso 2: il provider DEL TURNO ha fallito per una causa cooldown/billing
    // strutturata. Solo allora lo snapshot e' pertinente, e va filtrato ai
    // provider effettivamente coinvolti nel turno (result.provider). Se il turno
    // e' fallito per altra causa (empty_completion, output malformato, ...) lo
    // snapshot globale e' ortogonale: `None`.
    if !error_class_indicates_cooldown(result.error_class.as_deref()) {
        return None;
    }
    let involved = result.provider.to_lowercase();
    let filtered: Vec<(String, u64, Option<String>)> = snap
        .iter()
        .filter(|(name, _, _)| name.to_lowercase() == involved)
        .cloned()
        .collect();
    cooldown_note_from_snapshot(&filtered)
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
    // Formato tool-call "template" di deepseek colato nel content invece di essere
    // parsato come tool strutturato: <｜｜DSML｜｜tool_calls> ... <｜｜DSML｜｜invoke
    // name=...> (barre fullwidth U+FF5C). Run 8d429b03 (Beauty-Book):
    // deepseek-v4-pro sotto G1 nudge emette la tool-call come testo -> finiva
    // grezza nel final_answer. Trattala come tool-call testuale: il finalizzatore
    // usa il recap deterministico invece di mostrare il markup all'utente.
    if trimmed.contains("\u{ff5c}\u{ff5c}DSML") {
        return true;
    }
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
pub(crate) fn compose_turn_answer(result: &crate::agent_types::AgentRunResult) -> Option<String> {
    match result.final_answer.as_deref() {
        // Tool call colata nel testo o soli errori provider come "risposta":
        // non e' una risposta. Recap deterministico (o nota cooldown/placeholder).
        Some(s) if looks_like_textual_tool_call(s) || is_only_provider_errors(s) => {
            build_action_recap(&result.steps)
                .or_else(|| cooldown_exhaustion_note(result))
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
        // Anche un run NON-hollow con step eseguiti (es. troncato dal final_gate a
        // max_cycles o da un loop G1) deve mostrare il recap: ha modificato file ma
        // ha chiuso senza body -> mai lasciare la chat muta. Vedi finalize dello spawn.
        _ if is_report_hollow(result) || !result.steps.is_empty() => {
            build_action_recap(&result.steps)
                .or_else(|| cooldown_exhaustion_note(result))
                .or_else(|| {
                    Some(format!(
                        "_(Nessuna risposta utile prodotta dall'agente — {} / {} ha chiuso \
                         il turno con un completamento vuoto dopo aver esaurito i tentativi \
                         di fallback. Riformula la richiesta o cambia provider/modello manualmente.)_",
                        result.provider, result.model
                    ))
                })
        }
        _ => None,
    }
}

/// Appende a `answer` il recap RICCO delle azioni del run (`outcome_summary`),
/// se presente. PUNTO UNICO (regola L / regola H) dell'append del recap al
/// content del messaggio assistant: prima lo spawn lo appendeva inline e il
/// RESUME no, producendo due content divergenti (recap presente live/spawn,
/// assente dopo un resume di conferma). Centralizzandolo qui i due percorsi
/// producono lo stesso testo. `outcome_summary` e' a sua volta 1:1 con il
/// `buildSemanticDetail` del frontend (FIX D3), cosi' live e refresh coincidono.
pub(crate) fn append_outcome_summary(answer: String, steps: &[AgentStep]) -> String {
    match outcome_summary(steps) {
        Some(s) => format!("{answer}{s}"),
        None => answer,
    }
}

/// Assembla il prompt della narrativa dai fatti deterministici e dal template.
/// PURA (nessun DB, nessun LLM): estratta da `narrative_or` (regola L) cosi' che
/// l'orchestrazione async resti snella e la costruzione del prompt sia testabile
/// in isolamento. Comportamento 1:1: max 20 righe-azione + max 10 errori (excerpt
/// a 120 char), poi replace dei placeholder `{{recap}}`/`{{actions}}`.
fn build_recap_prompt(
    base: &str,
    facts: &crate::session_worklog::StepFacts,
    template: &str,
) -> String {
    let mut actions = String::new();
    for line in facts.action_lines.iter().take(20) {
        actions.push_str(line);
        actions.push('\n');
    }
    for e in facts.errors.iter().take(10) {
        let excerpt = e.excerpt.chars().take(120).collect::<String>();
        actions.push_str(&format!("- errore [{}] {}: {excerpt}\n", e.tool, e.detail));
    }
    template
        .replace("{{recap}}", base)
        .replace("{{actions}}", actions.trim())
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
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    if !enabled || !is_report_hollow(result) || result.steps.is_empty() {
        return base;
    }

    use crate::internal_routing::{resolve_purpose_model, PurposeResolution};
    let (provider, model) = match resolve_purpose_model(state, "turn_recap").await {
        PurposeResolution::Resolved {
            provider, model, ..
        } => (provider, model),
        _ => return base,
    };

    // Riassunto dei fatti deterministici per il prompt (azioni + errori).
    let facts = crate::session_worklog::collect_step_facts(&result.steps, 200);

    let template = nexus_types::get_template_or_default(
        &state.db,
        &state.template_cache,
        "system.turn_recap_narrative",
    )
    .await;
    if template.trim().is_empty() {
        return base;
    }
    let prompt = build_recap_prompt(&base_text, &facts, &template);
    run_narrative_llm(state, &provider, &model, &prompt, base).await
}

/// Chiama l'LLM leggero (`turn_recap`) col prompt gia' assemblato e ne estrae la
/// narrativa. Estratta da `narrative_or` (regola L): isola l'unica I/O verso il
/// gateway e la sua politica di fallback. Su risposta troppo corta (<20 char) o
/// errore ricade su `base` (regola H). Niente payload nei log (regola F).
async fn run_narrative_llm(
    state: &AppState,
    provider: &str,
    model: &str,
    prompt: &str,
    base: Option<String>,
) -> Option<String> {
    let messages_json =
        serde_json::to_string(&json!([{ "role": "user", "content": prompt }])).unwrap_or_default();

    match state
        .orchestrator
        .neural
        .generate_agent_turn(provider, model, &messages_json, "[]", 600, "")
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
    // Pool del DB PER-PROGETTO (chat_message_attachments e' migrata): risolto dal
    // chiamante via project_data_pool_by_session_from.
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
    // Pool del DB PER-PROGETTO (chat_message_attachments e' migrata): risolto dal
    // chiamante via project_data_pool_by_session_from.
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
    // Pool del progetto risolto dalla sessione (separazione DB): tabella
    // agent_runs migrata -> instrada la UPDATE sul DB del progetto. Riusato
    // sotto per l'ingest del worklog dei run superati. DB non disponibile ->
    // niente da superare LI' (i run vivono su quel DB): WARN e lista vuota.
    let wpool = match crate::project_db_routes::project_data_pool_by_session_from(
        &state.db, session_id,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "supersede_active_runs: DB progetto non disponibile, nessun run superato"
            );
            return Vec::new();
        }
    };
    let cancelled_ids: Vec<Uuid> = sqlx::query_scalar(
        // Cancella TUTTI i run attivi/sospesi-vivi della sessione (punto unico
        // ACTIVE_RUN_STATUSES): il force-stop deve liberare anche un padre sospeso
        // su awaiting_subagents, non solo running/awaiting_confirmation.
        &format!(
            "UPDATE agent_runs \
         SET status='cancelled', completed_at=NOW(), \
             cancellation_requested=NOW(), cancellation_reason=$2, \
             final_answer=COALESCE(final_answer, $3) \
         WHERE session_id = $1 \
           AND status IN ({}) \
         RETURNING id",
            crate::agent_types::ACTIVE_RUN_STATUS_SQL
        ),
    )
    .bind(session_id)
    .bind(reason)
    .bind(final_msg)
    .fetch_all(&wpool)
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
        cancel_orphan_subagent_runs(&wpool, session_id, &cancelled_ids).await;
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
        // Worklog nel DB del progetto (separazione DB): riuso il pool per-progetto
        // gia' risolto sopra dalla sessione.
        for cid in &cancelled_ids {
            if let Err(e) =
                crate::session_worklog::ingest_from_db_steps(&state.db, &wpool, *cid, label).await
            {
                tracing::warn!(error = %e, run_id = %cid, "session_worklog: ingest al supersede fallito");
            }
        }
    }
    cancelled_ids
}

/// Chiude i sub-run ancora `running` ancorati alla sessione (legacy pre-fix) o ai
/// run primari appena superati: depth/cost chain non devono inquinare il run nuovo.
async fn cancel_orphan_subagent_runs(
    wpool: &sqlx::PgPool,
    session_id: Uuid,
    cancelled_parent_ids: &[Uuid],
) {
    let mut anchors: Vec<Uuid> = Vec::with_capacity(1 + cancelled_parent_ids.len());
    anchors.push(session_id);
    anchors.extend(cancelled_parent_ids.iter().copied());
    let cancelled = sqlx::query_scalar::<_, Uuid>(
        "UPDATE nexus_subagent_runs SET status='cancelled', completed_at=NOW(), \
         final_summary=COALESCE(final_summary, 'superseded') \
         WHERE parent_run_id = ANY($1) AND status = 'running' \
         RETURNING id",
    )
    .bind(&anchors)
    .fetch_all(wpool)
    .await
    .unwrap_or_default();
    if cancelled.is_empty() {
        return;
    }
    let _ = sqlx::query(
        "UPDATE agent_runs SET status='cancelled', completed_at=NOW(), \
         cancellation_requested=NOW(), cancellation_reason='superseded', \
         final_answer=COALESCE(final_answer, 'Superato da un nuovo run.') \
         WHERE id = ANY($1) AND status IN ('running', 'awaiting_confirmation', 'awaiting_subagents')",
    )
    .bind(&cancelled)
    .execute(wpool)
    .await;
    tracing::info!(
        session_id = %session_id,
        count = cancelled.len(),
        "supersede_active_runs: chiusi sub-run orfani ancora running"
    );
}

/// Punto unico (regola L) della domanda "c'e' un run agentico attivo su questa
/// sessione?". Usa la lista autoritativa `ACTIVE_RUN_STATUSES` (running /
/// awaiting_confirmation / awaiting_subagents), la STESSA di `supersede_active_runs`
/// e di ogni altro gate "run attivo": nessuna re-implementazione del predicato
/// sparsa nei call site.
///
/// Fail-safe: in caso di errore DB assume run attivo (`true`) per NON rischiare
/// di interrompere un run in corso. Il chiamante critico e' `process_resume`,
/// che usa questa funzione per decidere se RISVEGLIARE l'agente (a riposo) o
/// RIMANDARE il resume (run ancora attivo) invece di superarlo via last-wins.
pub(crate) async fn session_has_active_run(db: &sqlx::PgPool, session_id: Uuid) -> bool {
    match sqlx::query_scalar::<_, bool>(&format!(
        "SELECT EXISTS( \
             SELECT 1 FROM agent_runs \
              WHERE session_id = $1 \
                AND status IN ({}) \
         )",
        crate::agent_types::ACTIVE_RUN_STATUS_SQL
    ))
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

/// Riepilogo del run "dispatcher" di todo-isolation, composto DAI DATI dei todo
/// completati (regola M), non da prosa generata.
///
/// Serve perche' il run principale che delega ai sub-run non attraversa mai un
/// turno di sintesi (`route_after_todo_runner` -> FinalGate/Learner, mai
/// Executor) e quindi non produce un `final_answer` proprio: senza questo recap
/// resterebbe MUTO in chat ora che un piano concluso non viene piu' declassato a
/// `failed_diagnosed`. Comporlo dai dati -- invece di chiedere un riassunto al
/// modello -- evita sia una chiamata LLM in piu' sia il rischio di ritrovarsi con
/// una seconda risposta vuota.
fn compose_todo_isolation_recap(
    todos: &[nexus_agent_graph::decisions::dag_scheduler::Todo],
) -> String {
    use nexus_agent_graph::decisions::dag_scheduler::TodoStatus;
    let completati: Vec<&str> = todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Completed))
        .filter_map(|t| t.content.as_deref())
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .collect();
    let n = todos
        .iter()
        .filter(|t| matches!(t.status, TodoStatus::Completed))
        .count();
    let mut out = format!("Piano completato: {n} attivita' eseguite in sub-run isolati.");
    for c in completati {
        out.push_str("\n- ");
        out.push_str(c);
    }
    out
}

/// Mappa l'esito del motore nativo Rust ([`crate::native_engine::NativeRunOutcome`])
/// nello STESSO [`AgentRunResult`] prodotto da `run_via_brain`, cosi' il primario
/// nativo converge sul finalizzatore unico (regola L): NESSUNA seconda forma di
/// finalize, NESSUN ramo `if engine` nel persistente.
///
/// Gli step sono RICOSTRUITI da `agent_steps` (il grafo Rust li ha gia' persistiti
/// per-superstep via `PgAgentStepStore`, `ExecMode::Real`): il chiamante marca
/// `native_steps_persisted=true` cosi' il finalizzatore NON li re-inserisce
/// (eviterebbe doppioni; gli step_index del grafo sono `iteration*1000+idx`, non
/// idempotenti con quelli del path Python). Il worklog usa comunque questi step.
///
/// Mappatura status (parita' con `derive_status` del path Python):
/// - HITL (`completed=false`, `resume_at` valorizzato) -> `AwaitingConfirmation`;
/// - forced-close anti-loop (`loop_*`/`g1_*`) -> `FailedDiagnosed`;
/// - altrimenti -> `Completed` (il finalizzatore declassa poi l'hollow-senza-lavoro
///   a `FailedDiagnosed`, identico al path Python).
pub(crate) async fn native_outcome_to_run_result(
    db: &PgPool,
    run_id: Uuid,
    outcome: crate::native_engine::NativeRunOutcome,
) -> crate::agent_types::AgentRunResult {
    use nexus_agent_graph::StopReason;

    // Step gia' persistiti dal grafo: rileggili in ordine stabile (step_index).
    // Best-effort: un errore di lettura -> nessuno step (il run resta valido).
    #[derive(sqlx::FromRow)]
    struct StepRow {
        step_index: i32,
        tool_name: String,
        tool_input: Value,
        tool_result: Option<String>,
        status: String,
        created_at: chrono::DateTime<chrono::Utc>,
    }
    let rows: Vec<StepRow> = sqlx::query_as::<_, StepRow>(
        "SELECT step_index, tool_name, tool_input, tool_result, status, created_at \
         FROM agent_steps WHERE run_id = $1 ORDER BY step_index ASC",
    )
    .bind(run_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    let steps: Vec<AgentStep> = rows
        .into_iter()
        .map(|r| AgentStep {
            run_id: run_id.to_string(),
            step_index: r.step_index.max(0) as u32,
            tool_name: r.tool_name,
            tool_input: r.tool_input,
            tool_result: r.tool_result,
            status: match r.status.as_str() {
                "failed" => AgentStepStatus::Failed,
                "running" => AgentStepStatus::Running,
                "skipped" => AgentStepStatus::Skipped,
                _ => AgentStepStatus::Completed,
            },
            created_at: r.created_at.to_rfc3339(),
        })
        .collect();

    // Il summary DICHIARATO via task_complete (ADR 0034) e' testo umano di
    // display: fa da risposta quando il modello ha chiuso senza produrre testo.
    let declared_summary = outcome
        .declared_outcome
        .as_ref()
        .and_then(|v| v.get("summary"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    // Status canonico dal PUNTO UNICO della classificazione (regola L/M):
    // `NativeRunOutcome::classify_status` — stessa funzione usata dal ponte
    // esito del SUB-agente (`structured_verdict`), cosi' il verdetto che un
    // coordinatore legge da un sub-run coincide con quello del run padre.
    let status = outcome.classify_status();

    let pending_actions: Vec<crate::agent_types::AgentPendingAction> = outcome
        .pending_actions
        .iter()
        .filter_map(|v| {
            Some(crate::agent_types::AgentPendingAction {
                index: v.get("index")?.as_u64()? as usize,
                tool_name: v.get("toolName")?.as_str()?.to_string(),
                tool_input: v.get("toolInput").cloned().unwrap_or_else(|| json!({})),
                description: v
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect();

    // stop_reason in forma snake_case (serde dell'enum) per la colonna agent_runs
    // / la telemetria: stesso vocabolario del path Python.
    let stop_reason: Option<String> =
        outcome
            .stop_reason
            .and_then(|r| match serde_json::to_value(r) {
                Ok(Value::String(s)) => Some(s),
                _ => None,
            });

    let provider = outcome.provider_used.clone().unwrap_or_default();
    let model = outcome.model_used.clone().unwrap_or_default();

    // Il summary DICHIARATO fa da risposta quando il modello ha chiuso con
    // task_complete senza produrre testo (parita' WAVE 3.2): mai un
    // "completed" muto se il modello ha comunque dichiarato l'esito.
    let final_answer = outcome
        .final_answer
        .filter(|s| !s.trim().is_empty())
        .or(declared_summary);

    // Annotazione di esito ONESTO (regola M): se la verifica oggettiva
    // pre-chiusura NON e' passata (final_gate al cap/forced), il resoconto e' la
    // AUTO-VALUTAZIONE ottimista del modello e NON riflette il verdetto del gate.
    // Riconciliamo i due qui, al punto unico dell'esito, appendendo l'avviso al
    // testo mostrato/persistito. Senza questo l'utente vedeva un resoconto
    // "completato" mentre lo status era failed_diagnosed (run e91d4892). Non si
    // applica alle dichiarazioni oneste (blocked/needs_input/partial: il modello
    // stesso ha gia' descritto l'incompletezza nel summary).
    // Review adversariale bocciata (ReviewGate): il titolo onesto viene PRIMA
    // dei rami del final_gate -- l'utente leggeva "TASK COMPLETATO" con la nota
    // della review appesa sotto, cioe' un successo, su un run failed_diagnosed.
    let final_answer = if outcome.review_panel_rejected {
        final_answer.map(|ans| {
            let nota = outcome
                .review_panel_last
                .as_ref()
                .map(render_review_panel_note)
                .unwrap_or_else(|| {
                    "**Review adversariale automatica: NON superata.**".to_string()
                });
            compose_unconfirmed_report(
                &nota,
                "NON confermata dalla review",
                &provider,
                &model,
                &ans,
            )
        })
    } else {
        final_answer
    };
    let final_answer = match (
        final_answer,
        outcome.final_gate_passed,
        outcome.final_gate_unverified,
    ) {
        (Some(ans), Some(false), _) => Some(compose_unconfirmed_report(
            NOTE_GATE_NON_SUPERATA,
            "non confermata dalla verifica",
            &provider,
            &model,
            &ans,
        )),
        // Bocciatura del gate in sospeso (run morto prima della ri-verifica, es.
        // provider esauriti): l'ultima verifica ESEGUITA era fallita e le
        // correzioni successive non sono mai state ri-verificate (run a5db0985).
        (Some(ans), _, _) if outcome.final_gate_failed_pending => Some(compose_unconfirmed_report(
            NOTE_GATE_FALLITA_PENDING,
            "non confermata dalla verifica",
            &provider,
            &model,
            &ans,
        )),
        // Lavoro svolto ma verifica tecnica NON eseguita (profilo di verifica
        // dell'ambiente assente): annotazione onesta (regola M), non un fallimento.
        // Forma DIVERSA (nota DOPO, nessun degrado ad auto-valutazione): il lavoro
        // e' riuscito, manca solo la conferma tecnica. Resta fuori dal punto unico.
        (Some(ans), _, Some(true)) => Some(format!(
            "{ans}\n\n---\n\n**Verifica tecnica non eseguita**: per questo progetto \
             non e' disponibile un profilo di verifica (comandi di build/test), quindi \
             l'esito NON e' stato confermato da un comando reale. Definisci i comandi \
             di verifica del progetto oppure ricontrolla manualmente prima di \
             considerarlo concluso."
        )),
        (other, _, _) => other,
    };

    // Segnale STRUTTURATO del lavoro svolto per DELEGA (regola M). Sotto
    // todo-isolation (`supervisor_mode=continuous`) i todo del piano sono
    // eseguiti come SUB-RUN isolati: il run PRINCIPALE e' solo un dispatcher e
    // `route_after_todo_runner`, a todo esauriti, lo instrada a FinalGate/Learner
    // e MAI all'Executor. Non scrive quindi `agent_steps` sul proprio run_id e
    // non produce un `final_answer` (non passa da alcun turno di sintesi): la
    // detection hollow qui sotto vedrebbe "0 step + risposta vuota" e
    // declasserebbe a `failed_diagnosed` un run che ha fatto TUTTO il lavoro
    // (incidente run 79d2d6eb: 7/7 todo completed e file creati, mostrati
    // all'utente come "fallito").
    //
    // FONTE AUTORITATIVA: la tabella `nexus_agent_todos` letta FRESCA dal punto
    // unico `TodoStore::list_todos` (regola L: la query NON si riscrive qui).
    // NON si usa lo snapshot in-memory `state.current_todos`: il TodoRunner lo
    // aggiorna solo nel ramo "c'e' un todo successivo", non in quello terminale
    // (EndTurn), quindi a piano concluso e' STANTIO e direbbe "non completato".
    // Best-effort come la lettura degli step: errore -> nessun todo -> il
    // predicato e' falso -> detection hollow invariata (nessuna regressione sul
    // gap 0-step b07c7e78).
    let plan_todos = {
        use nexus_agent_graph::runtime::ports::TodoStore as _;
        // `meta` = `db`: `list_todos` non legge `settings`, il pool meta non
        // viene mai toccato da questa chiamata.
        crate::agent_graph_adapter::todo_store::PgTodoStore::new(db.clone(), db.clone())
            .list_todos(&run_id.to_string())
            .await
            .unwrap_or_default()
    };
    let plan_concluso_con_lavoro =
        nexus_agent_graph::decisions::dag_scheduler::plan_todos_all_completed(&plan_todos);

    // Recap del dispatcher (regola M): ora che un piano concluso non viene piu'
    // declassato, il run resterebbe MUTO in chat (prima il placeholder di
    // diagnosi copriva il buco). Il riepilogo e' composto DAI DATI dei todo
    // completati -- non da prosa generata: nessuna chiamata LLM in piu' e
    // nessun rischio di produrre una seconda risposta vuota.
    let final_answer = match final_answer {
        Some(ans) if !ans.trim().is_empty() => Some(ans),
        _ if plan_concluso_con_lavoro => Some(compose_todo_isolation_recap(&plan_todos)),
        other => other,
    };

    // Hollow sul path NATIVO (prima: false hardcoded, "detection del client
    // SSE" che il grafo non ha): un run TERMINATO senza risposta ne' step
    // eseguiti restava MUTO in chat con status 'completed' (incidente run
    // b07c7e78 / gap 0-step). Perimetro stretto: solo run conclusi
    // (`completed`, mai AwaitingConfirmation) e mai su StopReason::Error (i
    // Failed hanno il proprio percorso retry/diagnosi). Tassonomia kind
    // identica al path brain ("EMPTY_ANSWER+NO_TOOLS"): e' quella che
    // is_report_hollow/canonical_run_status e la diagnostica gia' consumano.
    // `hollow_no_tools` resta false: il segnale tool-forcing (MALFORMED)
    // e' una detection del client SSE che qui non esiste; il contatore
    // generico consecutive_failures e' la semantica corretta per l'empty
    // answer.
    //
    // GATE (regola M): un piano concluso con lavoro reale NON e' mai hollow. Il
    // gate sta sul PRODUTTORE del flag, cosi' entrambi i consumatori --
    // `hollow_no_work` del finalizzatore e il gemello puro
    // `is_report_hollow`/`canonical_run_status` del resume -- lo ereditano senza
    // duplicare la condizione (regola L).
    let hollow_completion = outcome.completed
        && !matches!(outcome.stop_reason, Some(StopReason::Error))
        && final_answer.is_none()
        && steps.is_empty()
        && !plan_concluso_con_lavoro;

    crate::agent_types::AgentRunResult {
        run_id: run_id.to_string(),
        status,
        steps,
        pending_actions,
        final_answer,
        provider,
        model,
        iteration_count: outcome.iterations.max(0) as u32,
        nexus_override_applied: false,
        nexus_agent_type: None,
        nexus_q_value: None,
        provider_privacy_notice: None,
        prompt_tokens: outcome.prompt_tokens.max(0) as u32,
        completion_tokens: outcome.completion_tokens.max(0) as u32,
        total_tokens: outcome.total_tokens.max(0) as u32,
        total_cost: outcome.total_cost,
        // Lo stato del grafo nativo e' last-write per-turno: outcome.prompt_tokens
        // E' il prompt dell'ultima iterazione (riempimento contesto corrente).
        // Va catturato qui, PRIMA che reconcile_run_cost_from_ledger sovrascriva
        // prompt_tokens col cumulativo di billing del ledger.
        last_prompt_tokens: (outcome.prompt_tokens > 0).then_some(outcome.prompt_tokens as u32),
        // Classe d'errore STRUTTURATA dal grafo (extra.error_class, es.
        // context_overflow — ADR 0016 D2): segnale macchina, mai dal testo.
        error_class: outcome.error_class,
        stop_reason,
        // Intent del turno: pilota la decisione hollow/conversational del
        // finalizzatore (parita' col nexus_task_type del path Python).
        nexus_task_type: outcome.user_intent,
        hollow_completion,
        hollow_no_tools: false,
        hollow_completion_kind: if hollow_completion {
            "EMPTY_ANSWER+NO_TOOLS".to_string()
        } else {
            String::new()
        },
        // FIX D4: reasoning accumulato dal grafo nativo (reasoning_acc dello stato).
        reasoning: outcome.reasoning,
        // Conversazione finale del grafo per agent_runs.messages_json (resume +
        // trace panel): prima il run nativo lasciava la colonna NULL.
        messages_json: outcome.messages_json,
    }
}

/// Nota di esito del final_gate NON superato al cap. Contratto riconosciuto a
/// valle (regola M): il prefisso `**Verifica automatica non superata**` e'
/// matchato da `is_report_hollow`/canonical_run_status e da un test. Invariato.
const NOTE_GATE_NON_SUPERATA: &str = "**Verifica automatica non superata** \
(limite tentativi raggiunto): i criteri di verifica del progetto non sono \
passati; il task NON e' confermato completo. Controlla i criteri falliti nella \
timeline \"Decisioni del turno\" e riverifica il flusso reale prima di \
considerarlo concluso.";

/// Nota di esito del gate bocciato e mai ri-verificato (run chiuso prima della
/// ri-verifica). Contratto (regola M): prefisso invariato.
const NOTE_GATE_FALLITA_PENDING: &str = "**Verifica automatica fallita e non \
ripetuta**: l'ultima verifica dei criteri del progetto era FALLITA e il run si \
e' chiuso prima di poter ri-verificare le correzioni. Il task NON risulta \
verificato e puo' contenere regressioni: controlla i criteri falliti nella \
timeline \"Decisioni del turno\" e riesegui la verifica prima di considerarlo \
concluso.";

/// Punto unico (regola L) del resoconto "auto-valutazione NON confermata": i tre
/// casi (review bocciata / gate non superato / gate fallito-non-ripetuto)
/// condividono la STESSA forma e differiscono solo per `note` e `qualifica`.
/// Compone: nota di esito, header col PROVENIENZA (provider/model che ha
/// generato il testo) e corpo, con spaziatura markdown ariosa. Le note passate
/// sono un contratto a valle (regola M): non vengono alterate.
fn compose_unconfirmed_report(
    note: &str,
    qualifica: &str,
    provider: &str,
    model: &str,
    ans: &str,
) -> String {
    let prov = if !provider.is_empty() && !model.is_empty() {
        format!("{provider}/{model} · ")
    } else {
        String::new()
    };
    format!(
        "{note}\n\n---\n\n_Resoconto dell'agente ({prov}auto-valutazione, \
         {qualifica}):_\n\n{ans}"
    )
}

/// Riconosce il messaggio di errore provider sintetizzato dall'executor del
/// grafo nativo (nexus-agent-graph, ramo "agent_turn fallita"): la
/// `final_answer` inizia col marker `[Errore provider`. Punto unico di
/// detection (regola L) per l'esito-certo: il marker e' emesso in un solo posto
/// (executor.rs) e qui lo riconosciamo per declassare a Failed un run che
/// altrimenti risulterebbe `completed` pur essendo fallito perche' il provider
/// non era disponibile (cooldown / gateway irraggiungibile).
pub(crate) fn is_provider_error_answer(answer: &str) -> bool {
    answer.trim_start().starts_with("[Errore provider")
}

/// True se il run, pur risultando `Completed`, e' in realta' un fallimento per
/// provider non disponibile: nessun token di completion prodotto E `final_answer`
/// = messaggio di errore provider sintetizzato dall'executor. Punto unico (regola
/// L) della regola "esito certo: errore provider -> Failed", invocato sia dal
/// finalizzatore dello spawn principale sia da `canonical_run_status` (path resume).
pub(crate) fn is_provider_error_completion(result: &crate::agent_types::AgentRunResult) -> bool {
    matches!(result.status, crate::agent_types::AgentRunStatus::Completed)
        && result.completion_tokens == 0
        && result
            .final_answer
            .as_deref()
            .is_some_and(is_provider_error_answer)
}

/// Totali aggregati dal ledger di billing (`ai_usage_ledger`) per un singolo
/// run. Fonte autoritativa del costo del turno: il gateway vi scrive una riga
/// per ogni chiamata LLM applicando i prezzi corretti del catalog (regola L).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct LedgerTotals {
    pub total_cost: f64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    /// Numero di righe FINALIZZATE del ledger per il run. E' il segnale
    /// STRUTTURATO di "il gateway ha contabilizzato questo run" (regola M):
    /// distingue "nessuna riga" da "righe a costo zero" — due casi che il solo
    /// `total_cost` confonde.
    pub rows: i64,
}

impl LedgerTotals {
    /// True se il gateway ha contabilizzato almeno una chiamata per il run.
    ///
    /// Deliberatamente NON guarda l'importo: un run i cui modelli hanno prezzo
    /// ignoto produce righe a costo 0, e quelle righe sono comunque la
    /// contabilita' autoritativa (i token sono reali). Dedurre "il ledger non
    /// c'e'" da "il costo e' 0" e' inferire lo stato da una grandezza invece che
    /// dal segnale strutturato — regola M.
    pub(crate) fn has_rows(&self) -> bool {
        self.rows > 0
    }

    /// Token totali coerenti del run. Alcune righe del ledger possono avere
    /// `total_tokens = 0` pur avendo prompt/completion validi: in quel caso il
    /// totale va ricostruito, per non pubblicare "0 token" a fronte di chiamate
    /// realmente avvenute. Punto unico del quirk (regola L): lo usano sia il run
    /// di chat sia i sub-run.
    pub(crate) fn coherent_total_tokens(&self) -> i64 {
        let tt = self.total_tokens.max(0);
        if tt > 0 {
            tt
        } else {
            self.prompt_tokens
                .max(0)
                .saturating_add(self.completion_tokens.max(0))
        }
    }
}

/// Aggrega costo e token dal ledger per il `run_id` dato. Best-effort: se il DB
/// e' irraggiungibile o non esistono righe, ritorna [`LedgerTotals::default`]
/// (tutti 0, `rows = 0`) e il chiamante ricade sul fallback catalog. Il `run_id`
/// del ledger coincide con `agent_runs.id` (il gateway lo popola dal
/// `request_id` del turno).
///
/// Conta e somma SOLO le righe `status = 'finalized'`: e' l'unico stato che
/// rappresenta una chiamata effettivamente contabilizzata (gli altri ammessi dal
/// CHECK — `reserved`, `rejected`, `failed`, `released` — non lo sono). Filtrare
/// sullo stato strutturato invece che sull'importo e' la regola M.
///
/// NB: NON usare `finalized_at IS NOT NULL` come predicato di finalizzazione —
/// e' NULL sulla quasi totalita' delle righe realmente finalizzate.
pub(crate) async fn fetch_ledger_totals(db: &PgPool, run_id: Uuid) -> LedgerTotals {
    #[derive(sqlx::FromRow)]
    struct Row {
        total_cost: f64,
        prompt_tokens: i64,
        completion_tokens: i64,
        total_tokens: i64,
        rows: i64,
    }
    let row: Option<Row> = sqlx::query_as::<_, Row>(
        "SELECT COALESCE(SUM(total_cost), 0)::float8        AS total_cost,
                COALESCE(SUM(prompt_tokens), 0)::int8       AS prompt_tokens,
                COALESCE(SUM(completion_tokens), 0)::int8   AS completion_tokens,
                COALESCE(SUM(total_tokens), 0)::int8        AS total_tokens,
                COUNT(*)::int8                              AS rows
           FROM ai_usage_ledger
          WHERE run_id = $1 AND status = 'finalized'",
    )
    .bind(run_id)
    .fetch_optional(db)
    .await
    .inspect_err(|e| {
        tracing::warn!(error = %e, "billing: aggregazione ledger del run fallita (best-effort)");
    })
    .ok()
    .flatten();
    match row {
        Some(r) => LedgerTotals {
            total_cost: r.total_cost,
            prompt_tokens: r.prompt_tokens,
            completion_tokens: r.completion_tokens,
            total_tokens: r.total_tokens,
            rows: r.rows,
        },
        None => LedgerTotals::default(),
    }
}

/// Riconcilia costo e token di `result` con i totali del ledger (punto unico,
/// regola L). Il ledger e' la fonte autoritativa: se il gateway ha
/// contabilizzato il run (almeno una riga finalizzata), i suoi totali
/// sovrascrivono `total_cost`/`prompt_tokens`/`completion_tokens`/`total_tokens`
/// di `result`, cosi' TUTTI i consumer a valle (metadata del messaggio
/// assistant, agent_runs, budget provider, cap di spesa dei subagenti) vedono il
/// costo reale del RUN INTERO.
///
/// Perche' il ledger vince SEMPRE quando ha righe: `result.total_cost` arriva da
/// `state.total_cost_usd`, che il grafo aggiorna con un reducer di tipo
/// *overwrite* — vale quindi l'ULTIMO TURNO, non il totale del run
/// (vedi `native_engine.rs`, campo `total_cost` di `NativeRunOutcome`).
/// La guardia precedente (`result.total_cost > 0.0` -> non riconciliare) si
/// fondava sulla premessa "il path nativo lascia total_cost = 0, solo il brain
/// Python lo valorizza": premessa FALSA (il path nativo lo valorizza col costo
/// del turno) e per giunta riferita a un brain che non esiste piu'. L'effetto era
/// che la riconciliazione si auto-disabilitava proprio sui run che doveva
/// correggere, pubblicando il costo di una singola iterazione come totale.
///
/// Funzione pura/isolata (testabile senza DB): ritorna `true` se ha applicato i
/// valori del ledger, `false` se lo ha lasciato invariato perche' il gateway non
/// ha contabilizzato nulla per questo run (`rows == 0`) — in quel caso il
/// chiamante mantiene il fallback al calcolo-da-catalog.
pub(crate) fn reconcile_run_cost_from_ledger(
    result: &mut crate::agent_types::AgentRunResult,
    ledger: &LedgerTotals,
) -> bool {
    if !ledger.has_rows() {
        return false;
    }
    result.total_cost = ledger.total_cost;
    result.prompt_tokens = ledger.prompt_tokens.max(0) as u32;
    result.completion_tokens = ledger.completion_tokens.max(0) as u32;
    result.total_tokens = ledger.coherent_total_tokens() as u32;
    true
}

/// PUNTO UNICO (regola L) della riconciliazione STOP -> stato finale del run.
///
/// Un run per cui e' stata richiesta la cancellazione (`cancellation_requested IS
/// NOT NULL`, scritto dallo Stop utente `user_cancel` o dal supersede last-wins)
/// NON deve chiudersi 'completed'. Il gate di TESTA dell'executor legge la
/// cancellazione solo a INIZIO iterazione (`executor.rs`, `head_gate` ->
/// `StopReason::Superseded`), MAI durante la chiamata LLM: se lo Stop arriva
/// mentre l'ultima chiamata e' in volo e il modello poi conclude (task_complete/
/// end_turn), il run finalizza 'completed' senza ripassare dal gate. Questa
/// riconciliazione chiude la finestra dal lato della persistenza: l'UPDATE e'
/// ATOMICO (la condizione `cancellation_requested IS NOT NULL` e' valutata dal DB
/// al momento della scrittura), quindi coglie anche una cancellazione arrivata
/// DOPO la finalizzazione ma prima di questa chiamata (nessuna race read-modify).
///
/// Tocca SOLO il ramo 'completed': un esito 'failed'/'cancelled' resta invariato
/// (un fallimento tecnico e' informativo, non va mascherato da 'cancelled', e un
/// 'cancelled' e' gia' corretto). Best-effort: un guasto DB non deve far fallire
/// la chiusura del run (l'esito 'completed' resta, degradazione onesta).
async fn enforce_user_cancellation_status(pool: &PgPool, run_id: Uuid) {
    let res = sqlx::query(
        "UPDATE agent_runs SET status = 'cancelled' \
         WHERE id = $1 AND cancellation_requested IS NOT NULL AND status = 'completed'",
    )
    .bind(run_id)
    .execute(pool)
    .await;
    if let Err(e) = res {
        tracing::warn!(
            run_id = %run_id,
            error = %e,
            "enforce_user_cancellation_status fallito (best-effort): lo Stop \
             potrebbe non riflettersi nello stato finale"
        );
    }
}

/// Costruisce un [`AgentRunResult`] FAILED ONESTO per il fallimento del motore
/// nativo PRIMARIO (regola H: nessun fallback mascherato al brain). Converge sullo
/// stesso finalizzatore del path normale (regola L) impostando `native_result`:
/// status `Failed`, `final_answer` = messaggio diagnostico gia' sanificato (regola
/// F: niente stack trace), `stop_reason = "error"`. `error_class = None` (non e'
/// un errore provider classificabile: e' un fallimento di esecuzione del grafo) ->
/// il loop di retry NON lo ritenta su altri provider (vedi `failed_retry`).
pub(crate) fn native_engine_failure_result(
    run_id: Uuid,
    provider: &str,
    model: &str,
    final_answer: String,
) -> crate::agent_types::AgentRunResult {
    crate::agent_types::AgentRunResult {
        run_id: run_id.to_string(),
        status: AgentRunStatus::Failed,
        steps: Vec::new(),
        pending_actions: Vec::new(),
        final_answer: Some(final_answer),
        provider: provider.to_string(),
        model: model.to_string(),
        iteration_count: 0,
        nexus_override_applied: false,
        nexus_agent_type: None,
        nexus_q_value: None,
        nexus_task_type: None,
        provider_privacy_notice: None,
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        total_cost: 0.0,
        last_prompt_tokens: None,
        error_class: None,
        stop_reason: Some("error".to_string()),
        hollow_completion: false,
        hollow_no_tools: false,
        hollow_completion_kind: String::new(),
        // Fallimento del motore nativo: nessun reasoning utile (FIX D4).
        reasoning: None,
        // Fallimento prima di produrre una conversazione: nessun messages_json.
        messages_json: None,
    }
}

// =============================================================================
// Punto UNICO (regola L) dispatch post-INSERT agent run.
//
// Tutto cio' che supera il budget HTTP del proxy Next.js (compact, monitor,
// history, consiglio, multi-provider, motore) passa da UN solo `tokio::spawn`
// avviato da `spawn_agent_run` subito dopo INSERT `agent_runs`. I call site
// HTTP (handlers send/resend) attendono solo `SpawnOutcome::Started`.
//
// Compact e monitor sono helper interni (task figli), non secondi punti di
// controllo architetturali.
// =============================================================================

/// Unico `tokio::spawn` per il lifecycle post-INSERT di un agent run.
fn dispatch_agent_run_post_insert<F>(work: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(work);
}

/// Auto-compact best-effort in parallelo al turno (non blocca consiglio/run).
fn spawn_auto_compact_for_run(
    state: AppState,
    session_id: Uuid,
    project_id: Uuid,
    provider: String,
    model: String,
) {
    tokio::spawn(async move {
        maybe_auto_compact(&state, session_id, project_id, &provider, &model).await;
    });
}

/// Ascoltatore monitor pannello: si sottoscrive al broadcast prima del corpo run.
fn spawn_agent_run_step_monitor(
    step_tx: broadcast::Sender<AgentStepEvent>,
    monitor_registry: crate::agent_tools::monitor::MonitorRegistry,
    project_channels: nexus_events::ProjectChannels,
    project_id: Uuid,
) {
    let mut step_rx = step_tx.subscribe();
    crate::agent_tools::monitor::set_monitor(
        &monitor_registry,
        &project_channels,
        project_id,
        "agent_run",
        serde_json::Value::String("in corso".to_string()),
        Some("avvio run agente".to_string()),
    );
    tokio::spawn(async move {
        let mut files_touched: u64 = 0;
        loop {
            let ev = match step_rx.recv().await {
                Ok(ev) => ev,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            if ev.is_final {
                break;
            }
            let Some(step) = ev.step else { continue };
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
                    &monitor_registry,
                    &project_channels,
                    project_id,
                    "agent_tool",
                    serde_json::Value::String(step.tool_name.clone()),
                    Some(label),
                );
            }
            if step.status == AgentStepStatus::Completed
                && matches!(step.tool_name.as_str(), "write_file" | "edit_file")
            {
                files_touched = files_touched.saturating_add(1);
                crate::agent_tools::monitor::set_monitor(
                    &monitor_registry,
                    &project_channels,
                    project_id,
                    "agent_files",
                    serde_json::Value::from(files_touched),
                    Some("file modificati".to_string()),
                );
            }
        }
    });
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
        .classify_intent_full(&state.db, &classifier_input)
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
    // Separazione DB: chat_messages e' migrata -> pool del progetto risolto UNA
    // volta e riusato per TUTTE le letture/scritture per-progetto di questo
    // spawn (disambiguazione, history, INSERT run, allegati, worklog). DB non
    // disponibile -> NotStarted onesto (regola M), mai il meta: sul meta le
    // letture rispondono vuoto e le scritture producono run/messaggi fantasma.
    let msgs_pool = match crate::project_db_routes::project_data_pool_by_session_from(
        &state.db,
        params.session_id,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                session_id = %params.session_id,
                error = %e,
                "spawn_agent_run: DB progetto non disponibile, run non avviato"
            );
            state.agent_channels.remove(&run_id);
            return SpawnOutcome::NotStarted;
        }
    };
    if let Some(chosen) =
        resolve_disambiguation_reply(&msgs_pool, params.session_id, &params.content).await
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
        let view = match load_message_by_id(&state.db, params.project_id, msg_id).await {
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
            .as_ref()
            .filter(|v| !v.trim().is_empty())
            .cloned()
            .or_else(|| {
                params
                    .profile_provider
                    .as_ref()
                    .filter(|v| !v.trim().is_empty())
                    .cloned()
            })
    };
    let effective_model_override =
        if let Some((_slot_provider, slot_model, _src)) = &slot_routing_hit {
            Some(slot_model.clone())
        } else {
            params
                .model_override
                .as_ref()
                .filter(|v| !v.trim().is_empty())
                .cloned()
                .or_else(|| {
                    params
                        .profile_model
                        .as_ref()
                        .filter(|v| !v.trim().is_empty())
                        .cloned()
                })
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
            .fetch_one(&msgs_pool)
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
            // turn_has_image: RIPRISTINO regressione Python->Rust (CLAUDE.md sez. I,
            // "Smart routing vision"). Se il messaggio corrente allega un'immagine
            // il routing forza un modello con supports_vision=TRUE. Segnale
            // strutturato dai MIME del turno (punto unico turn_has_image_attachment),
            // non dal testo del prompt.
            crate::orchestrator::turn_has_image_attachment(&params.attachments),
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
        // Pool del progetto (separazione DB): tabella agent_runs migrata ->
        // riusa msgs_pool (stesso DB <slug>_nexus, risolto una volta sopra).
        let run_pool = msgs_pool.clone();
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
        .execute(&run_pool)
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

    // Last-wins (punto unico, regola L): questo nuovo run supera OGNI run ancora
    // attivo sulla stessa sessione, fermandoli cooperativamente. Vale per tutti i
    // call site (chat, resume, process_resume, service_observer) perche' passano
    // tutti da spawn_agent_run -> l'invariante "max 1 run attivo per sessione" e'
    // applicata in un solo posto e nessun nuovo call site puo' dimenticarla.
    let superseded_runs =
        supersede_active_runs(state, params.session_id, "superseded_by_new_run").await;

    // Persist initial run in DB
    // Pool del progetto (separazione DB): tabella agent_runs migrata -> riusa
    // msgs_pool (stesso DB <slug>_nexus, risolto una volta a inizio spawn).
    let run_pool = msgs_pool.clone();
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
    .execute(&run_pool)
    .await;

    let tx_for_brain = tx.clone();
    drop(tx);

    let provider_for_response = provider.clone();
    let model_for_response = model_str.clone();

    let state_bg = state.clone();
    let params_bg = params;
    let proj_bg = proj;
    let classified_bg = classified;
    let resolved_intent_hint_bg = resolved_intent_hint;
    let classifier_input_bg = classifier_input;
    let effective_override_bg = effective_override;
    let superseded_runs_bg = superseded_runs;
    let provider_bg = provider;
    let model_str_bg = model_str;
    let msgs_pool_bg = msgs_pool;
    let tx_bg = tx_for_brain.clone();

    dispatch_agent_run_post_insert(async move {
        let state = state_bg;
        let params = params_bg;
        let proj = proj_bg;
        let classified = classified_bg;
        let resolved_intent_hint = resolved_intent_hint_bg;
        let classifier_input = classifier_input_bg;
        let effective_override = effective_override_bg;
        let superseded_runs = superseded_runs_bg;
        let provider = provider_bg;
        let model_str = model_str_bg;
        let msgs_pool = msgs_pool_bg;
        let tx_for_brain = tx_bg;

        spawn_auto_compact_for_run(
            state.clone(),
            params.session_id,
            params.project_id,
            provider.clone(),
            model_str.clone(),
        );
        spawn_agent_run_step_monitor(
            tx_for_brain.clone(),
            state.monitor_registry.clone(),
            state.project_channels.clone(),
            params.project_id,
        );

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
            &msgs_pool, // pool progetto: chat_messages (raw fallback + finestra recente)
            &state.db,  // meta-DB: setting globali qdrant_url/collection (regola G/L)
            &state.orchestrator.neural,
            params.session_id,
            &params.content,
            4, // ultimi 4 messaggi raw = 2 turni completi user+assistant
            6, // top-6 semantici dalla storia piu' vecchia (soglia 0.40)
        )
        .await
    } else {
        // Dipendenze vettoriali down: usa solo gli ultimi messaggi raw
        build_recent_conversation_history(&msgs_pool, params.session_id, 8).await
    };
    // Versione testuale compatta solo per logging
    let recent_context = build_recent_conversation_context(&msgs_pool, params.session_id, 4).await;
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
    let self_ref_hint = build_self_referential_hint(&msgs_pool, params.session_id, &params.content)
        .await
        .unwrap_or_default();

    // Istruzioni specifiche per modelli o-series (o1/o3/o4-mini): forzano
    // l'uso esplicito dei tool instead of narrare le azioni come testo.
    let o_series_instructions = if crate::agent_turn_setup::is_o_series_model_pub(&model_str) {
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

    // `mut`: nel ramo classico la direttiva <consiglio_analisi> viene tolta qui
    // sotto se il consiglio ha gia' parlato (il suo parere e' nel prompt, la
    // direttiva sarebbe rumore). Nel ramo overlap resta: il consiglio non ha
    // ancora deliberato.
    let mut system_text = format!(
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
    // chat_message_attachments vive nel DB del progetto (separazione DB): riusa
    // msgs_pool (stesso DB, risolto una volta a inizio spawn), coerente con il
    // worklog piu' sotto.
    let initial_msg = build_initial_msg_with_attachments(
        &msgs_pool,
        &params.content,
        &params.attachments,
        params.user_message_id,
        params.project_id,
        params.session_id,
    )
    .await;
    // Decisione AGENTICA su consiglio + multi-provider (regola M): a decidere se
    // il task merita la deliberazione a monte e' il GIUDIZIO del classificatore
    // LLM (`complexity`), non un keyword-match sul testo (fragile, monolingua,
    // falsi positivi tipo "migra il testo del bottone"). Solo se il classificatore
    // NON e' disponibile (fallback: LLM down/timeout/JSON invalido) si degrada al
    // gate keyword `council_triggered_for` (comportamento storico, fail-safe).
    let deliberate = if !classified.classifier_resolved {
        crate::prompt_templates::council_triggered_for(&state.db, &params.content).await
    } else {
        classified.complexity != "low"
    };
    // Dimensionamento dell'orchestrazione (punto unico orchestration_sizing):
    // classe del classificatore + profilo admin della classe + budget residuo
    // -> target dei panel. `None` = sizing spento o segnali non risolti -> cap
    // storici invariati (bit-identico a flag OFF).
    let sizing_complexity = if classified.classifier_resolved {
        nexus_agent_graph::decisions::orchestration_sizing::TaskComplexity::try_parse(
            &classified.complexity,
        )
    } else {
        None
    };
    let sizing_scope_system_wide = classified.slots.scope.trim() == "system_wide";
    let orchestration_plan = if deliberate {
        let time_remaining = crate::agent_tools::subagent_native::run_time_remaining_s(
            &state.db, &run_pool, run_id,
        )
        .await;
        resolve_orchestration_plan_for(
            &state.db,
            sizing_complexity,
            sizing_scope_system_wide,
            false,
            0.0,
            time_remaining,
        )
        .await
    } else {
        None
    };
    if let Some(plan) = &orchestration_plan {
        emit_orchestration_plan_meta_step(&run_pool, &tx_for_brain, run_id, plan, "pre_run").await;
    }
    // I due panel a monte girano in PARALLELO (fase 2 del paradigma): non
    // condividono dati — le sintesi vengono riconciliate solo a valle da
    // select_pre_run_advisory — quindi la serializzazione era solo
    // implementativa (fino a ~300+300s di pre-step nel caso peggiore). La
    // pressione sub-run e' governata dal semaforo di processo del fan-out
    // (FanoutGovernor, mig 0603). Le emissioni meta-step restano sequenziali
    // DOPO la join: ordine deterministico degli step.
    let upstream_inputs = UpstreamInputs {
        state: state.clone(),
        run_pool: run_pool.clone(),
        tx: tx_for_brain.clone(),
        session_id: params.session_id,
        run_id,
        user_text: params.content.clone(),
        deliberate,
        plan: orchestration_plan,
        complexity: sizing_complexity,
        scope_system_wide: sizing_scope_system_wide,
    };
    // OVERLAP (mig 0606): il run parte SUBITO e i panel deliberano in parallelo,
    // oppure — ramo classico, flag OFF — si attende il loro verdetto qui.
    // I due rami condividono `run_upstream_panels` (regola L): cambia SOLO chi
    // aspetta chi.
    let overlap = nexus_auth::get_bool_setting(&state.db, "orchestrator.advisory_overlap_enabled")
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
        && deliberate;
    let (initial_msg, pre_run_advisory_synthesis, pre_run_advisory_source, advisory_gate) =
        if overlap {
            // Il modello NON vede i blocchi nel prompt iniziale (non esistono
            // ancora): li ricevera' come promemoria alla release della barriera,
            // prima di poter scrivere. Il system prompt tiene la direttiva
            // <consiglio_analisi>: qui il consiglio non ha ancora parlato.
            let (gate_tx, gate_rx) = tokio::sync::watch::channel(
                nexus_agent_graph::nodes::AdvisoryGateState::Pending,
            );
            spawn_advisory_gate_task(upstream_inputs, gate_tx);
            (initial_msg, None, None, Some(gate_rx))
        } else {
            let panels = run_upstream_panels(&upstream_inputs).await;
            let msg = if panels.blocks.is_empty() {
                initial_msg
            } else {
                format!("{}\n\n{initial_msg}", panels.blocks.join("\n\n"))
            };
            if panels.council_present {
                system_text = crate::prompt_templates::strip_council_directive(&system_text);
            }
            (msg, panels.synthesis, panels.source, None)
        };

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
        // Worklog nel DB del progetto (separazione DB): riusa msgs_pool.
        let worklog_note = crate::session_worklog::supersede_summary(
            &state.db,
            &msgs_pool,
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
    // Messaggio passato al classifier (con contesto recente) catturato per i rami
    // nativi: serve a ricostruire i dati COMPLETI del classifier del turno
    // (Tappa 1b punto B) via `resolve_classifier_fields`. Usato nei rami
    // Engine::Rust (primario instradato globalmente) ed Engine::Shadow
    // (attivabile solo per-sessione).
    let classifier_input_for_shadow: String = classifier_input.clone();
    let pre_run_advisory_synthesis_clone = pre_run_advisory_synthesis.clone();
    let pre_run_advisory_source_clone = pre_run_advisory_source;
    // Barriera advisory (overlap, mig 0606): il receiver entra nel ctx del grafo
    // e arma il gate del ToolDispatchNode. `None` nel ramo classico.
    let advisory_gate_for_run = advisory_gate;

    // Calcola il payload tools dinamico (discovery mode vs inline) prima dello spawn.
    // Il filtering per automation_mode avviene dentro build_tools_json_for_agent:
    // in `study` esporta solo tool read-only (gating difensivo), in `confirm` e
    // `automatic` esporta la lista completa.
    let tools_json_for_brain = crate::agent_turn_setup::build_tools_json_for_agent(
        &state.db,
        params.user_id,
        params.project_id,
        &params.automation_mode,
        &provider,
        &model_str,
    )
    .await;


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

    // Clone di AppState per il finalize dentro il task 'static: serve a
    // narrative_or (mig 0415) per risolvere il purpose e chiamare il neural.
    // Cheap: i campi di AppState sono Arc/pool condivisi.
    let state_for_finalize = state.clone();

        use futures::FutureExt;

        tracing::info!(
            "spawn_agent_run: avvio run agentico (motore da nexus_orchestrator_engine) run_id={}",
            run_id
        );

        let agent_body = std::panic::AssertUnwindSafe(async move {
            // La strangler-fig e' finita: il motore e' UNO, quello nativo.
            //
            // Qui `select_engine` leggeva `nexus_orchestrator_engine` e sceglieva
            // fra Rust, Python (`run_via_brain`) e Shadow (il cui PRIMARIO era
            // Python). Il brain e' stato rimosso (mig 0462/0532): quei due rami
            // non instradavano piu' i run su un altro motore, li instradavano nel
            // vuoto — e restavano armabili con un UPDATE di "rollback" all'aria di
            // innocuo. `agent_runs.engine` resta valorizzato per il recovery, ora
            // con l'unico valore possibile.
            match crate::project_db_routes::project_data_pool_from(&db_clone, project_id_cp).await
            {
                Ok(run_pool) => {
                    let _ = sqlx::query("UPDATE agent_runs SET engine = $2 WHERE id = $1")
                        .bind(run_id)
                        .bind(ENGINE_NATIVE)
                        .execute(&run_pool)
                        .await;
                }
                Err(e) => {
                    tracing::warn!(
                        run_id = %run_id,
                        error = %e,
                        "run task: DB progetto non disponibile, colonna engine non aggiornata"
                    );
                }
            }
            // `native_steps_persisted` evita la re-INSERT degli step (il grafo li
            // ha gia' persistiti) nel finalizzatore, che resta uno solo (regola L).
            // Nessun inizializzatore: ogni ramo del blocco sotto li assegna, e il
            // compilatore lo verifica. Un default qui sarebbe un valore che nessuno
            // legge — cioe' un esito finto in attesa di essere scambiato per vero.
            let native_result: Option<crate::agent_types::AgentRunResult>;
            let native_steps_persisted: bool;
            {
                // Su Err NON si cade su un altro motore (regola H): un fallback
                // automatico mascherava il fallimento del grafo dietro un secondo
                // run (esito disonesto, doppio costo). Su Err si finalizza il run
                // come FAILED diagnosticato (`native_engine_failure_result`).
                // ── action_oriented ────────────────────────────────────────────────
                // Il primario nativo (RunRole::Primary) deve derivare action_oriented/
                // report_only ESATTAMENTE come il primario Python (che riclassifica nel
                // router_node col SUO classifier). Riusiamo il PUNTO UNICO condiviso
                // con lo shadow (`resolve_classifier_fields`, regola L): classifica il
                // turno col porting 1:1 + legge la soglia DB. `build_initial_state`
                // ramo Primary deriva poi i flag fedeli (read->false: niente G1 spurio;
                // azione->true: tool). Senza gateway o su fallback del classifier i
                // campi restano neutri e build_initial_state lascia None (RouterNode
                // decide, comportamento INVARIATO). Attivo per engine='rust', il
                // motore primario instradato globalmente.
                let primary_classifier = crate::native_engine::resolve_classifier_fields(
                    &db_clone,
                    &state_for_finalize.orchestrator.nexus_gateway,
                    &classifier_input_for_shadow,
                )
                .await;
                let native_input = crate::native_engine::NativeRunInput {
                    run_id,
                    session_id: session_id_cp,
                    provider: provider_clone.clone(),
                    model: model_clone.clone(),
                    system_text: system_text_clone.clone(),
                    initial_msg: initial_msg_clone.clone(),
                    conversation_history: recent_history_for_brain.clone(),
                    tools_json: tools_json_for_brain.clone(),
                    intent_hint: intent_hint_for_brain.clone(),
                    // Dati del classifier del turno (helper condiviso con lo shadow):
                    // il PRIMARIO RUST deriva action_oriented/report_only fedeli.
                    requires_tools: primary_classifier.requires_tools,
                    agentic_score: primary_classifier.agentic_score,
                    authorizes_changes: primary_classifier.authorizes_changes,
                    classifier_resolved: primary_classifier.classifier_resolved,
                    action_oriented_min_score: primary_classifier.action_oriented_min_score,
                    automation_mode: automation_mode_for_brain.clone(),
                    supervisor_mode: crate::native_engine::graph_supervisor_mode(
                        params.supervisor_mode,
                    ),
                    step_tx: tx_for_brain.clone(),
                    // Run PRINCIPALE (non sub-agente): nessun parent/depth. Solo
                    // `dispatch_subagent` (subagent_native) popola questi campi.
                    parent_run_id: None,
                    subagent_depth: None,
                    sizing_complexity,
                    sizing_scope_system_wide,
                    classifier_intent: Some(classified_intent_for_loop.to_string()),
                    run_time_budget_s: None,
                    // Run principale sulla root del progetto: nessun isolamento
                    // (l'override worktree e' riservato ai sub-run isolati, PR4).
                    working_root: None,
                    pre_run_advisory_synthesis: pre_run_advisory_synthesis_clone.clone(),
                    pre_run_advisory_source: pre_run_advisory_source_clone,
                    advisory_gate: advisory_gate_for_run.clone(),
                };
                match run_via_native(&state_for_finalize, &native_input).await {
                    Ok(outcome) => {
                        // Niente leak: si logga la LUNGHEZZA della risposta, non il
                        // contenuto (regola F). provider/model EFFETTIVI post cascade.
                        tracing::info!(
                            run_id = %run_id,
                            completed = outcome.completed,
                            stop_reason = ?outcome.stop_reason,
                            final_answer_len = outcome.final_answer.as_deref().map(str::len).unwrap_or(0),
                            provider_used = outcome.provider_used.as_deref().unwrap_or("-"),
                            model_used = outcome.model_used.as_deref().unwrap_or("-"),
                            resume_at = outcome.resume_at.as_deref().unwrap_or("-"),
                            "motore nativo: run primario eseguito (path primario, zero-Python)"
                        );
                        // Mappa l'esito sullo stesso AgentRunResult del path Python:
                        // gli step sono gia' su agent_steps (PgAgentStepStore) ->
                        // native_steps_persisted=true. Il run termina QUI (no loop
                        // Python): il finalizzatore sotto opera su questo result.
                        // Separazione DB: agent_steps vive nel DB del progetto
                        // (PgAgentStepStore scrive sul run_db) -> la rilettura per
                        // costruire result.steps DEVE usare lo stesso pool, mai il
                        // meta (tabella vuota: il worklog perderebbe i fatti del run).
                        // La review adversariale programmatica ora vive DENTRO il
                        // grafo (nodo ReviewGate, prima della chiusura): su
                        // bocciatura rimanda in correzione invece di annotare un
                        // run gia' morto. Qui resta solo il mapping dell'esito
                        // (native_outcome_to_run_result legge review_panel_rejected
                        // e extra.review_panel_last dal medesimo stato). Se il DB
                        // del progetto non e' disponibile al finalize, l'esito e'
                        // un FAILED diagnosticato (regola M), mai il meta.
                        native_result = match crate::project_db_routes::project_data_pool_by_session_from(
                            &db_clone,
                            session_id_cp,
                        )
                        .await
                        {
                            Ok(steps_pool) => Some(
                                native_outcome_to_run_result(&steps_pool, run_id, outcome).await,
                            ),
                            Err(e) => {
                                tracing::error!(
                                    run_id = %run_id,
                                    error = %e,
                                    "motore nativo: DB progetto non disponibile al finalize"
                                );
                                Some(native_engine_failure_result(
                                    run_id,
                                    &provider_clone,
                                    &model_clone,
                                    format!(
                                        "DB del progetto non disponibile al finalize del run: {e}"
                                    ),
                                ))
                            }
                        };
                        native_steps_persisted = true;
                    }
                    Err(e) => {
                        // Regola H (fix definitivo, non toppa): un errore del motore
                        // nativo PRIMARIO e' un fallimento del run, NON un motivo per
                        // cambiare motore. Il vecchio fallback automatico a
                        // `run_via_brain` (Python) mascherava il problema del grafo
                        // nativo dietro un secondo run su un altro motore (esito
                        // disonesto, doppio costo, e — verso zero-Python — su un brain
                        // che sparira'). Si finalizza come FAILED diagnosticato: il
                        // run converge sullo STESSO finalizzatore (regola L) via
                        // `native_result`, senza scendere nel loop Python.
                        tracing::error!(
                            run_id = %run_id,
                            "motore nativo: esecuzione fallita — run finalizzato come failed (nessun fallback al brain, regola H)"
                        );
                        let msg = format!(
                            "Il motore nativo non e' riuscito a completare il run ({}). \
                             Il run e' stato chiuso come non riuscito.",
                            crate::agent_turn_setup::sanitize_error_for_user(&e.to_string())
                        );
                        native_result = Some(native_engine_failure_result(
                            run_id,
                            &provider_clone,
                            &model_clone,
                            msg,
                        ));
                        // Gli step prodotti prima dell'errore sono gia' su agent_steps
                        // (PgAgentStepStore): non re-inserirli (idempotenza).
                        native_steps_persisted = true;
                    }
                }
            }

            // L'esito del run (`result`) viene dal PRIMARIO nativo (se ha girato e
            // concluso) oppure dal loop di retry Python. Un blocco etichettato lascia
            // il path Python INVARIATO (nessuna re-indentazione): il primario nativo
            // esce subito con `break 'compute`, evitando il doppio-run (DEBITO 2).
            // L'esito del run viene dal motore nativo: e' l'unico motore.
            //
            // Qui c'era un blocco etichettato 'compute con dentro 691 righe di
            // loop di retry Python: il primario nativo usciva subito con un
            // `break 'compute` e tutto il resto serviva solo a `run_via_brain`.
            // Il brain e' stato rimosso, e il failover cross-provider che quel
            // loop implementava vive gia' nel motore nativo
            // (`nexus-agent-graph`, nodes/executor.rs: "provider caduto ->
            // FAILOVER cross-provider via routing"): non era una rete di
            // sicurezza in piu', era una copia irraggiungibile.
            let mut result: crate::agent_types::AgentRunResult = match native_result {
                Some(r) => r,
                // Non raggiungibile per costruzione: ogni ramo del blocco nativo
                // valorizza `native_result` (esito reale o failure diagnosticato).
                // Se mai accadesse, il run si chiude come FALLITO dichiarandolo:
                // un run senza esito non resta appeso ne' viene spacciato per ok.
                None => native_engine_failure_result(
                    run_id,
                    &provider_clone,
                    &model_clone,
                    "Il motore nativo non ha prodotto alcun esito per questo run.".to_string(),
                ),
            };

            // ── Riconciliazione costo/token del run dal ledger (regola L) ───────
            // Punto unico: il ledger (`ai_usage_ledger`) e' la fonte AUTORITATIVA
            // del costo del run. Il gateway scrive una riga per OGNI chiamata LLM
            // del turno applicando i prezzi corretti del catalog (e gestendo
            // escalation / modelli multipli nello stesso run, cosa che il calcolo
            // single-price dal catalog NON fa bene). Il grafo nativo, invece,
            // tiene `total_cost`/`prompt_tokens` con un reducer di tipo overwrite:
            // valgono l'ULTIMO TURNO, non il run. Senza questa riconciliazione
            // TUTTI i consumer a valle (metadata del messaggio assistant per la
            // UI, agent_runs, budget provider) pubblicano il costo di una singola
            // iterazione spacciandolo per il totale del run.
            //
            // Una sola aggregazione, riusata da messaggio + agent_runs + budget.
            // Autoritativa ogni volta che il gateway ha contabilizzato il run
            // (almeno una riga finalizzata); resta il fallback al calcolo-da-catalog
            // piu' sotto solo per il caso "nessuna riga di ledger" (provider che non
            // scrive ledger, o META irraggiungibile).
            let ledger_totals = fetch_ledger_totals(&db_clone, run_id).await;
            // ESITO STRUTTURATO della riconciliazione (regola M): `true` = il
            // gateway ha contabilizzato il run e i suoi totali sono stati adottati.
            // Va tenuto: piu' sotto il budget provider deve sapere SE il costo e'
            // autoritativo, e non puo' dedurlo da "total_cost > 0" — un run
            // riconciliato con costo 0 (prezzo ignoto) e' contabilita' valida, non
            // un dato mancante.
            let cost_reconciled = reconcile_run_cost_from_ledger(&mut result, &ledger_totals);
            if cost_reconciled {
                tracing::debug!(
                    run_id = %run_id,
                    cost = result.total_cost,
                    total_tokens = result.total_tokens,
                    "billing: costo/token del run riconciliati dal ledger (fonte autoritativa)"
                );
            }
            // NOTA (FIX ordine is_final): l'emissione di `is_final=true` + la
            // rimozione del canale broadcast NON avviene piu' qui. Erano emessi
            // PRIMA dell'INSERT chat_messages e dell'UPDATE agent_runs, creando una
            // race: il frontend, ricevuto `is_final`, rileggeva il record dal DB
            // (retry) prima che fosse persistito e poteva forzare status=failed.
            // Ora il terminatore unico dello stream e' emesso DOPO le scritture DB
            // (vedi sotto, dopo l'UPDATE agent_runs), cosi' quando il frontend
            // riceve `is_final` il record (chat_messages + agent_runs) e' gia'
            // persistito. Vale per ENTRAMBI i path (loop Python e primario nativo):
            // questo blocco e' condiviso a valle del blocco 'compute. Il `Done`/
            // is_final del grafo nativo non e' emesso dai nodi: il terminatore e'
            // unico ed e' del finalizzatore (1:1 con la chiusura del path Python),
            // cosi' il frontend non chiude lo stream SSE dopo il primo tentativo
            // fallito perdendo i fallback.

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
                // Un run con lavoro reale (step eseguiti) non puo' restare muto
                // anche se NON e' hollow: il loop G1 troncato dal final_gate o la
                // chiusura a max_cycles lasciano final_answer vuoto pur avendo gia'
                // modificato file. Produci SEMPRE il recap deterministico.
                _ if report_hollow || !result.steps.is_empty() => build_action_recap(&result.steps)
                    .or_else(|| cooldown_exhaustion_note(&result))
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
                // interlocutoria/confusa. None per i turni senza azioni. Punto
                // unico append_outcome_summary, condiviso col resume (regola L).
                let effective_answer = append_outcome_summary(effective_answer, &result.steps);
                let mut meta = json!({
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
                // Context ratio UI: prompt dell'ULTIMA chiamata LLM del run.
                // `promptTokens` qui sopra e' il CUMULATIVO di tutte le
                // iterazioni (billing): usato come riempimento contesto
                // produceva percentuali assurde (es. 5046% ctx). Aggiunto SOLO
                // se presente: senza il campo la UI nasconde la percentuale.
                if let Some(last_pt) = result.last_prompt_tokens.filter(|v| *v > 0) {
                    if let Some(obj) = meta.as_object_mut() {
                        obj.insert("lastPromptTokens".to_string(), json!(last_pt));
                    }
                }
                // FIX D4: persisti il ragionamento (thinking) accumulato del run.
                // LIVE viaggiava solo come evento SSE `agent_thinking` (volatile):
                // al refresh il blocco "Ragionamento" spariva. Salvandolo qui nel
                // metadata.reasoning il frontend lo ricostruisce dal DB. Aggiunto
                // SOLO se presente (niente campo/null per i run senza thinking).
                if let Some(reasoning) = result
                    .reasoning
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    if let Some(obj) = meta.as_object_mut() {
                        obj.insert("reasoning".to_string(), json!(reasoning));
                    }
                }
                match crate::project_db_routes::project_data_pool_from(&db_clone, project_id_cp)
                    .await
                {
                    Ok(msg_pool) => {
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
                        .execute(&msg_pool)
                        .await;
                    }
                    Err(e) => {
                        tracing::error!(
                            run_id = %run_id,
                            error = %e,
                            "finalize: DB progetto non disponibile, messaggio assistant non persistito"
                        );
                    }
                }

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
            // prodotto risposta (EMPTY_ANSWER con steps vuoti), non e' un successo:
            // marcarlo 'completed' mostrava all'utente un esito ambiguo
            // ("completato" + nessun contenuto). Lo si declassa all'esito canonico
            // failed_diagnosed: la diagnosi e' il placeholder/recap gia' composto
            // sopra (answer_owned). Un hollow con step completati resta invece
            // Completed. La rinuncia esplicita e' gestita dal segnale strutturato
            // task_complete (ADR 0034), non piu' dalla detection lessicale RESIGNED
            // (rimossa, ADR 0018 fase 3).
            let hollow_no_work = report_hollow
                && result.steps.is_empty()
                && result.hollow_completion_kind.contains("EMPTY_ANSWER");
            // Esito CERTO (errore provider): un run che si chiuderebbe `completed`
            // ma la cui unica risposta e' il messaggio di errore provider
            // sintetizzato dall'executor SENZA token di completion e' un
            // fallimento infrastrutturale (provider in cooldown / gateway
            // irraggiungibile). Il routing post-errore puo' richiudere il grafo
            // con uno stop_reason non-Error (cap iterazioni -> EndTurn), perdendo a
            // monte il segnale; lo recuperiamo dal punto unico. Failed (non
            // FailedDiagnosed): e' infrastrutturale, non una chiusura diagnostica.
            let provider_error_close = is_provider_error_completion(&result);
            let status_canonical = if hollow_no_work {
                tracing::warn!(
                    "agent_run {}: hollow senza lavoro ({}) — status declassato \
                     completed -> failed_diagnosed (esito certo)",
                    run_id,
                    result.hollow_completion_kind
                );
                crate::agent_types::AgentRunStatus::FailedDiagnosed
            } else if provider_error_close {
                tracing::warn!(
                    "agent_run {}: chiusura con errore provider e 0 completion_tokens \
                     — status declassato completed -> failed (esito certo)",
                    run_id
                );
                crate::agent_types::AgentRunStatus::Failed
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
            // messages_json: conversazione finale del grafo nativo (None sul path
            // Python). Persistito solo se valorizzato (COALESCE: non azzera un
            // valore eventualmente scritto a monte), cosi' resume e trace panel
            // la trovano (prima il run nativo lasciava la colonna NULL/vuota).
            let pending_actions_json = if result.pending_actions.is_empty() {
                None
            } else {
                Some(serde_json::to_value(&result.pending_actions).unwrap_or(json!([])))
            };
            let _ = sqlx::query(
                "UPDATE agent_runs SET status=$2, final_answer=$3, iteration_count=$4, \
             prompt_tokens=$5, completion_tokens=$6, total_tokens=$7, total_cost=$8, \
             nexus_override_applied=$9, nexus_agent_type=$10, nexus_task_type=$11, \
             provider=$12, model=$13, messages_json=COALESCE($14, messages_json), \
             pending_actions_json=COALESCE($15, pending_actions_json), \
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
            .bind(result.messages_json.as_deref())
            .bind(pending_actions_json)
            // Pool del progetto (separazione DB): agent_runs migrata.
            .execute(&run_pool)
            .await;
            // Stop utente arrivato durante l'ultima chiamata LLM: riconcilia
            // 'completed' -> 'cancelled' (punto unico, regola L).
            enforce_user_cancellation_status(&run_pool, run_id).await;

            // IL LAVORO NON FATTO NON SVANISCE COL RUN. I todo rimasti non
            // terminali (`pending` = mai iniziati, `blocked` = falliti) vengono
            // marcati `carry_over`: il backlog del progetto li conserva e un run
            // successivo puo' RIPRENDERLI invece di ripartire da un'analisi da
            // zero. Senza questa marcatura la colonna `carry_over` (mig 0244,
            // con tanto di indice dedicato) e l'endpoint che la legge restavano
            // lettera morta: nessuno la scriveva MAI, quindi un run che chiudeva
            // con 12 todo indietro li perdeva del tutto.
            //
            // Gli `skipped` sono ESCLUSI di proposito: sono discendenti di un
            // todo fallito (cascade), quindi vanno ri-pianificati a valle della
            // causa, non riproposti tali e quali.
            //
            // `origin_run_id` conserva la provenienza (solo se non gia' scritto:
            // un todo ereditato piu' volte deve ricordare da DOVE nasce).
            // Best-effort: un errore qui non deve impedire la chiusura del run.
            let carried = sqlx::query(
                "UPDATE nexus_agent_todos \
                 SET carry_over = true, \
                     origin_run_id = COALESCE(origin_run_id, run_id) \
                 WHERE run_id = $1 AND status IN ('pending', 'blocked')",
            )
            .bind(run_id)
            .execute(&run_pool)
            .await;
            match carried {
                Ok(r) if r.rows_affected() > 0 => tracing::info!(
                    run_id = %run_id,
                    todo = r.rows_affected(),
                    "todo non completati riportati nel backlog del progetto (carry_over)"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(
                    run_id = %run_id,
                    error = %e,
                    "carry_over dei todo non riuscito (best-effort)"
                ),
            }

            // ── Terminatore dello stream: SOLO per gli stati TERMINALI ─────────
            // Emesso DOPO l'INSERT chat_messages e l'UPDATE agent_runs: quando il
            // frontend riceve `is_final` e rilegge il run dal DB, il record e' gia'
            // persistito (status canonico + final_answer), eliminando la race che
            // poteva forzare status=failed sul retry DB del client. Vale per
            // entrambi i path (loop Python e primario nativo): il blocco e'
            // condiviso. `is_final` e' idempotente lato UI.
            //
            // SOSPENSIONE (regola L, gemello di AwaitingConfirmation/HITL): se lo
            // status e' NON-terminale (punto unico `is_terminal`: `AwaitingSubagents`
            // per il fan-in background, `AwaitingConfirmation` per l'HITL quando il
            // nodo che imposta il flag sara' portato al nativo), il run NON e' finito:
            // e' SOSPESO e riprendera' (worker fan-in / conferma utente). Chiudere lo
            // stream qui (a) farebbe trattare il run come FINITO al frontend (tasto
            // invio riabilitato) e (b) rimuovendo il canale spegnerebbe la narrazione
            // live dei figli background (agganciata al `step_tx` del padre). Quindi
            // per la sospensione emettiamo un meta step (`is_final=false`) e TENIAMO
            // il canale aperto — esattamente come l'HITL tiene "in attesa conferma".
            if status_canonical.is_terminal() {
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
            } else if status_canonical == AgentRunStatus::AwaitingSubagents {
                // Fan-in (Fase D): sospeso in attesa dei figli background. Emette il
                // meta step con il COUNT dei figli non-terminali; canale PRESERVATO
                // (la narrazione dei figli continua, il worker fan-in riprendera' il
                // run). Punto unico `emit_awaiting_subagents_meta`.
                let pending = emit_awaiting_subagents_meta(&run_pool, &tx_for_brain, run_id).await;
                tracing::info!(
                    target: "mcp_core::fanin",
                    run_id = %run_id,
                    pending_background_children = pending,
                    "fan-in: run padre SOSPESO in awaiting_subagents (stream mantenuto, meta step emesso)"
                );
            } else if status_canonical == AgentRunStatus::AwaitingConfirmation {
                let action_count =
                    emit_awaiting_confirmation_meta(&tx_for_brain, run_id, &result.pending_actions)
                        .await;
                tracing::info!(
                    target: "mcp_core::agent_run",
                    run_id = %run_id,
                    pending_actions = action_count,
                    "HITL: run padre SOSPESO in awaiting_confirmation (stream mantenuto)"
                );
            } else {
                // Altro stato non-terminale: canale PRESERVATO, nessun is_final.
                tracing::info!(
                    target: "mcp_core::agent_run",
                    run_id = %run_id,
                    status = status_str,
                    "run padre SOSPESO in stato non-terminale (stream mantenuto)"
                );
            }

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
                AgentRunStatus::CompletedUnverified => (
                    "completato (non verificato)",
                    format!(
                        "{} step · {} iter · verifica non eseguita",
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
            // Calcolo del cost (gerarchia: ledger -> catalog -> 0):
            //   - riconciliato dal ledger -> usa quel costo, SEMPRE. Il gateway ha
            //     applicato i prezzi corretti a ogni chiamata del run; se il totale
            //     e' 0 perche' il prezzo del modello e' ignoto, 0 e' la risposta
            //     onesta e va addebitata come tale.
            //   - Altrimenti (nessuna riga di ledger per il run, es. provider che
            //     non scrive ledger): fallback al calcolo da prompt/completion_tokens
            //     × prezzi del catalog.
            //
            // La condizione e' il SEGNALE `cost_reconciled`, non `total_cost > 0`
            // (regola M). Con la vecchia soglia, i run riconciliati a costo 0 —
            // misurate 875 righe di ledger con token > 0 e costo 0 — cadevano nel
            // fallback, che STIMAVA dal catalog un costo che il gateway si era
            // deliberatamente rifiutato di attribuire, e lo addebitava al budget
            // del provider. Una stima inventata sopra un dato dichiarato ignoto.
            let cost_to_charge: f64 = if cost_reconciled {
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
                        // Aggiorna anche agent_runs.total_cost E total_tokens
                        // per coerenza UI (l'UPDATE principale sopra ha scritto
                        // result.total_tokens, che nel path nativo senza ledger
                        // puo' essere 0 pur avendo prompt/completion validi:
                        // ricostruiscilo da prompt+completion). Idempotente:
                        // tocca solo i run rimasti a 0.
                        let total_tokens_fallback = (result.total_tokens.max(
                            result
                                .prompt_tokens
                                .saturating_add(result.completion_tokens),
                        )) as i32;
                        let _ = sqlx::query(
                            "UPDATE agent_runs SET total_cost = $2, total_tokens = $3 \
                         WHERE id = $1 AND total_cost = 0",
                        )
                        .bind(run_id)
                        .bind(total)
                        .bind(total_tokens_fallback)
                        // Pool del progetto (separazione DB): agent_runs migrata.
                        .execute(&run_pool)
                        .await;
                        tracing::debug!(
                        "budget: cost calcolato da catalog (ledger vuoto) per {}/{} = ${:.6} (prompt={} comp={})",
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
            // Gli step sono gia' raccolti in-memory dal agent_turn_setup durante il loop SSE.
            // DEBITO 3: il PRIMARIO nativo li ha gia' persistiti per-superstep
            // (PgAgentStepStore) -> `native_steps_persisted` salta la re-INSERT (gli
            // step_index del grafo non sono idempotenti con quelli del path Python:
            // re-inserirli creerebbe doppioni). Il worklog ingest sotto resta SEMPRE
            // attivo (legge `result.steps`, ricostruiti da DB nel path nativo).
            if !result.steps.is_empty() {
                if !native_steps_persisted {
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
                // Pool del progetto (separazione DB): agent_steps migrata.
                .execute(&run_pool)
                .await;
                    }
                    tracing::debug!(
                        "agent_run {}: {} step persistiti in agent_steps",
                        run_id,
                        result.steps.len()
                    );
                }

                // Worklog di sessione (mig 0411): deriva gli eventi operativi
                // strutturati dagli step e rinfresca il digest provider-neutro
                // che il brain inietta nei run successivi della sessione.
                // Best-effort: un errore qui non tocca l'esito del run.
                // Worklog nel DB del progetto (separazione DB): riuso run_pool,
                // gia' risolto in cima al task per lo stesso project_id_cp.
                if let Err(e) = crate::session_worklog::ingest_steps_for_run(
                    &db_clone,
                    &run_pool,
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
            // Pool del progetto (separazione DB): agent_runs (e chat_messages sotto)
            // migrate -> instrada le scritture del panic-handler sul DB del progetto.
            // Risolto una volta dal clone del meta (panic_db) + panic_project_id
            // catturati; riusato per l'INSERT chat_messages sotto. DB non
            // disponibile -> il panic-handler NON deve panicare a sua volta:
            // ERROR e uscita (il run_reaper chiudera' il run stale).
            let panic_pool = match crate::project_db_routes::project_data_pool_from(
                &panic_db,
                panic_project_id,
            )
            .await
            {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(
                        run_id = %panic_run_id,
                        error = %e,
                        "panic-handler: DB progetto non disponibile, run non marcato failed"
                    );
                    return;
                }
            };
            let _ = sqlx::query(
                "UPDATE agent_runs SET status='failed', completed_at=NOW(), \
                 final_answer=$2 WHERE id=$1",
            )
            .bind(panic_run_id)
            .bind(format!(
                "Errore interno: il task agente e' terminato in modo imprevisto ({}). Riprova.",
                panic_msg
            ))
            .execute(&panic_pool)
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
            // Pool del progetto (separazione DB): riuso panic_pool gia' risolto sopra.
            .execute(&panic_pool)
            .await;
        }
    });

    SpawnOutcome::Started(SpawnAgentResult {
        run_id,
        provider: provider_for_response,
        model: model_for_response,
    })
}

// ===========================================================================
// Motore di orchestrazione: uno solo, quello nativo.
//
// Qui viveva la strangler-fig: `select_engine` leggeva `nexus_orchestrator_engine`
// e sceglieva fra Engine::{Rust, Python, Shadow}. Il cutover e' finito e il brain
// Python e' stato rimosso (mig 0462/0532): l'enum, la cache TTL e la risoluzione
// scope-specifico/jolly sono spariti con lui. Restava un solo valore vivo.
// ===========================================================================

/// Valore di `agent_runs.engine` per ogni run: serve al recovery, che deve
/// sapere con che motore girava un run interrotto.
pub(crate) const ENGINE_NATIVE: &str = "rust";

/// Avvio di un run sul motore nativo Rust (`nexus-agent-graph`).
///
/// FASE 3 (aggancio reale): costruisce ed esegue il grafo Rust con le 14 impl
/// concrete di [`crate::agent_graph_adapter`] tramite il PUNTO UNICO
/// [`crate::native_engine::run_native`] (regola L: la costruzione/esecuzione
/// vive in un solo modulo). Raccoglie da `AppState` le dipendenze infra
/// (`ToolRunnerDeps` + client gateway) e le passa con gli input del run gia'
/// risolti a monte (regola L: prompt/tools/history NON ricostruiti qui).
///
/// Su errore il chiamante NON fa piu' fallback al brain (regola H, verso
/// zero-Python): un Err finalizza il run come FAILED diagnosticato
/// (`native_engine_failure_result`), non maschera il fallimento del grafo dietro
/// un secondo run su un altro motore.
pub(crate) async fn run_via_native(
    state: &AppState,
    input: &crate::native_engine::NativeRunInput,
) -> anyhow::Result<crate::native_engine::NativeRunOutcome> {
    let deps = build_native_deps(state).await;
    crate::native_engine::run_native(&deps, input).await
}

/// RESUME di un run nativo (Engine::Rust) sospeso su `awaiting_confirmation`.
///
/// PUNTO UNICO (regola L) del resume nativo lato call site: riusa
/// `build_native_deps` (stesso cablaggio infra di `run_via_native`) e delega al
/// motore via [`crate::native_engine::resume_native`], che riparte dal checkpoint
/// Postgres iniettando il `resume_message` di approvazione. Il GRAFO riprende dal
/// nodo salvato nel checkpoint: l'`input` serve solo a popolare ctx + porte I/O
/// (provider/model/session/canale SSE), NON a ricostruire lo stato (gia' nel
/// checkpoint).
pub(crate) async fn resume_via_native(
    state: &AppState,
    input: &crate::native_engine::NativeRunInput,
    resume_message: &str,
) -> anyhow::Result<crate::native_engine::NativeRunOutcome> {
    let deps = build_native_deps(state).await;
    crate::native_engine::resume_native(&deps, input, resume_message).await
}

/// RESUME completo di un run nativo (Engine::Rust) in `awaiting_confirmation`.
///
/// PUNTO UNICO (regola L) del resume HITL lato call site: e' l'analogo nativo del
/// `POST /agent/approve` del brain (`resume_run`), ma l'esecuzione e' IN-PROCESS,
/// quindi qui mcp-core deve anche FINALIZZARE l'esito (il brain lo fa da se' via
/// stream). Riprende il grafo dal checkpoint Postgres (`resume_via_native`),
/// mappa l'esito con `native_outcome_to_run_result` (mapping unico) e persiste lo
/// stato terminale + emette `is_final` sul canale SSE del run.
///
/// `provider`/`model`/`session_id` sono i valori del run originale (dal record
/// `agent_runs`): popolano ctx + porte I/O; il GRAFO riparte comunque dal nodo
/// salvato nel checkpoint (prompt/tools/history NON sono ricostruiti, vivono nel
/// checkpoint). Su un nuovo interrupt il run resta `awaiting_confirmation`
/// (l'utente potra' riapprovare). Su errore del motore -> `failed` ONESTO (regola
/// H: nessun fallback al brain qui, l'esecuzione nativa che fallisce e' un
/// fallimento del run, non un motivo per cambiare motore).
///
/// NB: il grafo nativo imposta `awaiting_confirmation` nel `ToolDispatchNode`
/// quando `automation_mode=confirm` e ci sono tool mutativi pendenti (gate HITL
/// in `nexus-agent-graph::decisions::hitl`). Il resume avviene via
/// [`confirm_native_run`] + checkpoint Postgres.
pub(crate) async fn confirm_native_run(
    state: &AppState,
    run_id: Uuid,
    session_id: Uuid,
    provider: String,
    model: String,
    automation_mode: String,
    resume_message: &str,
) -> Result<crate::agent_types::AgentRunStatus, String> {
    // Canale SSE del run: riusa quello esistente (i client sono gia' agganciati);
    // se assente (es. dopo un restart), creane uno nuovo registrato sotto run_id
    // cosi' l'is_final finale sblocca eventuali reattach.
    let tx = match state.agent_channels.get(&run_id) {
        Some(ch) => ch.clone(),
        None => {
            let (tx, _rx) = broadcast::channel::<AgentStepEvent>(256);
            state.agent_channels.insert(run_id, tx.clone());
            tx
        }
    };

    // Input MINIMO per il resume: i campi di costruzione dello stato (prompt/
    // tools/history/classifier) NON servono — il grafo riparte dal checkpoint.
    // Restano provider/model/session per ctx + porte e il canale SSE.
    let input = crate::native_engine::NativeRunInput {
        run_id,
        session_id,
        provider: provider.clone(),
        model: model.clone(),
        system_text: String::new(),
        initial_msg: String::new(),
        conversation_history: Vec::new(),
        tools_json: serde_json::json!([]),
        intent_hint: None,
        requires_tools: None,
        agentic_score: None,
        authorizes_changes: None,
        classifier_resolved: false,
        action_oriented_min_score: crate::intent_classifier::DEFAULT_ACTION_ORIENTED_MIN_SCORE,
        automation_mode,
        supervisor_mode: nexus_agent_graph::SupervisorMode::None,
        step_tx: tx.clone(),
        parent_run_id: None,
        subagent_depth: None,
        sizing_complexity: None,
        sizing_scope_system_wide: false,
        classifier_intent: None,
        run_time_budget_s: None,
        // Resume del run principale sulla root del progetto: nessun isolamento.
        working_root: None,
        pre_run_advisory_synthesis: None,
        pre_run_advisory_source: None,
        // RESUME da checkpoint: i panel a monte hanno gia' deliberato nel primo
        // tratto del run e il loro esito e' nello stato checkpointato (chiave
        // `advisory_gate`); ri-armare la barriera farebbe attendere un verdetto
        // che e' gia' arrivato.
        advisory_gate: None,
    };

    let outcome = resume_via_native(state, &input, resume_message).await;

    // Pool del progetto risolto dalla sessione (separazione DB): agent_runs
    // migrata -> instrada le UPDATE di finalize (esito Ok/Err) sul DB del
    // progetto. Risolto una volta, riusato in entrambi i rami.
    let cn_pool = crate::project_db_routes::project_data_pool_by_session_from(&state.db, session_id)
        .await
        .map_err(|e| e.to_string())?;
    let status = match outcome {
        Ok(outcome) => {
            // Mapping unico esito->AgentRunResult (regola L), poi finalize essenziale.
            // Separazione DB: gli step del run vivono nel DB del progetto ->
            // rilettura sul cn_pool risolto sopra, non su state.db (meta).
            let mut result = native_outcome_to_run_result(&cn_pool, run_id, outcome).await;
            // Riconciliazione costo/token dal ledger (stesso punto unico dello
            // spawn principale): il path nativo non aggrega costo nel grafo, senza
            // questo l'UPDATE azzererebbe agent_runs.total_cost/total_tokens a 0
            // anche dopo un resume con chiamate LLM a pagamento.
            let ledger_totals = fetch_ledger_totals(&state.db, run_id).await;
            let _ = reconcile_run_cost_from_ledger(&mut result, &ledger_totals);
            let status_str = result.status.as_str();
            // messages_json: conversazione finale del resume nativo (COALESCE: non
            // azzera la history del run originale se il resume non ne produce una
            // nuova). Resume e trace panel la trovano valorizzata.
            let pending_json = if result.pending_actions.is_empty() {
                None
            } else {
                Some(serde_json::to_value(&result.pending_actions).unwrap_or(json!([])))
            };
            let _ = sqlx::query(
                "UPDATE agent_runs SET status=$2, final_answer=$3, iteration_count=$4, \
                 prompt_tokens=$5, completion_tokens=$6, total_tokens=$7, total_cost=$8, \
                 nexus_task_type=$9, provider=$10, model=$11, \
                 messages_json=COALESCE($12, messages_json), \
                 pending_actions_json=COALESCE($13, pending_actions_json), \
                 completed_at=NOW() \
                 WHERE id=$1",
            )
            .bind(run_id)
            .bind(status_str)
            .bind(result.final_answer.as_deref())
            .bind(result.iteration_count as i32)
            .bind(result.prompt_tokens as i32)
            .bind(result.completion_tokens as i32)
            .bind(result.total_tokens as i32)
            .bind(result.total_cost)
            .bind(result.nexus_task_type.as_deref())
            .bind(&result.provider)
            .bind(&result.model)
            .bind(result.messages_json.as_deref())
            .bind(pending_json)
            // Pool del progetto (separazione DB): agent_runs migrata.
            .execute(&cn_pool)
            .await;
            enforce_user_cancellation_status(&cn_pool, run_id).await;
            result.status
        }
        Err(e) => {
            // Regola H: errore del motore nativo -> failed ONESTO, niente fallback
            // al brain. Il contenuto dell'errore non finisce nei log (regola F):
            // si logga solo che il resume e' fallito.
            tracing::error!(run_id = %run_id, "confirm_native_run: resume nativo fallito");
            let msg = format!(
                "Il resume nativo del run e' fallito ({}). Il run e' stato chiuso come non riuscito.",
                crate::agent_turn_setup::sanitize_error_for_user(&e.to_string())
            );
            let _ = sqlx::query(
                "UPDATE agent_runs SET status='failed', final_answer=$2, completed_at=NOW() \
                 WHERE id=$1",
            )
            .bind(run_id)
            .bind(&msg)
            // Pool del progetto (separazione DB): agent_runs migrata.
            .execute(&cn_pool)
            .await;
            crate::agent_types::AgentRunStatus::Failed
        }
    };

    // is_final sul canale SSE: sblocca i client agganciati (run terminale o nuovo
    // awaiting_confirmation). Best-effort: nessun subscriber -> ignorato.
    let _ = tx.send(AgentStepEvent {
        run_id: run_id.to_string(),
        step: None,
        trace: None,
        is_final: true,
        token_delta: None,
        thinking_delta: None,
        meta_step: None,
    });
    state.agent_channels.remove(&run_id);

    Ok(status)
}

/// Conta i figli BACKGROUND DIRETTI di `run_id` ancora NON-terminali
/// (`running`/`paused`) sul pool del progetto (segnale strutturato `status`,
/// regola M — mai prosa). Correla per `dispatcher_run_id = run_id` (mig project
/// 0010), come il backstop fan-in e `claim_subagent_fanin_results`: isola i figli
/// DIRETTI dai nipoti annidati. Best-effort: su errore infra ritorna 0 (il meta
/// step viene comunque emesso col count noto, non blocca la sospensione).
async fn count_pending_background_children(proj_pool: &PgPool, run_id: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM nexus_subagent_runs \
         WHERE dispatcher_run_id = $1 AND is_background = true \
           AND status IN ('running', 'paused')",
    )
    .bind(run_id)
    .fetch_one(proj_pool)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(
            target: "mcp_core::fanin",
            run_id = %run_id,
            error = %e,
            "fan-in: COUNT figli background pendenti fallita (uso 0 nel meta step)"
        );
        0
    })
}

/// PUNTO UNICO (regola L) dell'evento SSE di SOSPENSIONE di un run non-terminale
/// (`AwaitingSubagents` fan-in, e per parita' `AwaitingConfirmation` HITL quando
/// il nodo che imposta il flag sara' portato al nativo). GEMELLO del terminatore
/// `is_final=true`, ma per la sospensione: NON chiude lo stream e NON rimuove il
/// canale broadcast (a differenza del finalize terminale), cosi' (a) il frontend
/// tiene aperto lo stream e mostra "sospeso in attesa di N sub-agent" invece di
/// riabilitare il tasto invio come su un run FINITO; (b) la narrazione live dei
/// figli background — gia' collegata via il `step_tx` del padre — continua ad
/// arrivare. Emette un `AgentMetaStep` con `is_final=false`.
///
/// CONTRATTO SSE (consumato dal frontend, NON cambiare senza aggiornare il web-ide):
///   kind = "awaiting_subagents"
///   title = "In attesa di N sub-agent"
///   payload = { "status": "awaiting_subagents", "pending_count": <i64>,
///               "run_id": "<uuid>" }
/// `pending_count` = numero di figli background DIRETTI ancora non-terminali
/// (`running`/`paused`), segnale strutturato (regola M), mai dedotto da prosa.
/// Ritorna il `pending_count` emesso (per log/test).
async fn emit_awaiting_subagents_meta(
    proj_pool: &PgPool,
    tx: &tokio::sync::broadcast::Sender<AgentStepEvent>,
    run_id: Uuid,
) -> i64 {
    let pending = count_pending_background_children(proj_pool, run_id).await;
    let _ = tx.send(AgentStepEvent {
        run_id: run_id.to_string(),
        step: None,
        trace: None,
        // NON terminale: lo stream resta aperto (gemello di AwaitingConfirmation,
        // non del finalize). Il canale NON viene rimosso dal chiamante.
        is_final: false,
        token_delta: None,
        thinking_delta: None,
        meta_step: Some(crate::agent_types::AgentMetaStep {
            kind: "awaiting_subagents".to_string(),
            title: format!("In attesa di {pending} sub-agent"),
            payload: serde_json::json!({
                "status": "awaiting_subagents",
                "pending_count": pending,
                "run_id": run_id.to_string(),
            }),
            correlation_id: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        }),
    });
    pending
}

/// Meta step SSE per sospensione HITL (`awaiting_confirmation`).
///
/// CONTRATTO SSE (frontend `use-chat.ts`):
///   kind = "awaiting_confirmation"
///   payload = { "status": "awaiting_confirmation", "pending_actions": [...], "run_id": "..." }
async fn emit_awaiting_confirmation_meta(
    tx: &tokio::sync::broadcast::Sender<AgentStepEvent>,
    run_id: Uuid,
    pending_actions: &[crate::agent_types::AgentPendingAction],
) -> usize {
    let count = pending_actions.len();
    let actions_json = serde_json::to_value(pending_actions).unwrap_or_else(|_| json!([]));
    let _ = tx.send(AgentStepEvent {
        run_id: run_id.to_string(),
        step: None,
        trace: None,
        is_final: false,
        token_delta: None,
        thinking_delta: None,
        meta_step: Some(crate::agent_types::AgentMetaStep {
            kind: "awaiting_confirmation".to_string(),
            title: if count == 1 {
                "Attesa conferma (1 azione)".to_string()
            } else {
                format!("Attesa conferma ({count} azioni)")
            },
            payload: json!({
                "status": "awaiting_confirmation",
                "pending_actions": actions_json,
                "run_id": run_id.to_string(),
            }),
            correlation_id: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        }),
    });
    count
}

/// Risultati strutturati dei figli BACKGROUND di un parent, nella STESSA forma
/// del tool_result di `nexus_subagent_poll` (regola L): un `Value` per figlio
/// con `{subagent_run_id, kind, status, summary, outcome}`. `outcome` e' il
/// verdetto STRUTTURATO persistito (`nexus_subagent_runs.verdict`, regola M),
/// mai prosa. Iniettati nello stato al resume fan-in (`build_resume_delta_
/// subagents`) cosi' il padre compone i verdetti dei figli deterministicamente.
///
/// CLAIM, non lettura pura: marca `fanin_consumed_at` sui figli ritornati, cosi'
/// un'ondata successiva non li re-inietta (ALTA-2). Query sul PROJECT pool (dove
/// vive `nexus_subagent_runs`), ordinata per `created_at` (ordine stabile di
/// dispatch). Best-effort: su errore infra logga un WARN e ritorna `vec![]` (il
/// resume prosegue con esiti vuoti, meglio che bloccare).
///
/// Correla via `dispatcher_run_id = dispatcher_run_id` (mig project 0010): i figli
/// DIRETTI del run che si e' sospeso (`dispatcher_run_id` = `ctx.core.run_id` di
/// quel run, cioe' il `parent_run_id` della coda di resume). NON via
/// `parent_run_id = COALESCE(...)` = anchor (che degenera in session_id e
/// inietterebbe anche i NIPOTI annidati dispatchati da un altro figlio ancora vivo:
/// ALTA 1). Il `parent_run_id` della coda E' gia' il dispatcher (accodato da
/// `fanin_enqueue_if_last` sul run corrente), quindi nessuna derivazione d'anchor.
async fn claim_subagent_fanin_results(proj_pool: &PgPool, dispatcher_run_id: Uuid) -> Vec<Value> {
    // UPDATE ... RETURNING dentro una CTE (non piu' SELECT): marca ATOMICAMENTE
    // `fanin_consumed_at = NOW()` sui figli NON ancora consumati e li ritorna
    // ordinati. Discrimina l'ONDATA (ALTA-2, mig project 0011): `dispatcher_run_id`
    // e' COSTANTE tra le ondate (stesso run che si ri-sospende), quindi senza il
    // filtro `fanin_consumed_at IS NULL` la 2a ondata rifetcherebbe anche la 1a e il
    // modello la rivedrebbe come nuova (doppia iniezione). Marcando al fetch, ogni
    // figlio e' iniettato ESATTAMENTE una volta. La CTE preserva l'ordine di
    // dispatch (`created_at`), non garantito da UPDATE ... RETURNING nudo.
    let rows = sqlx::query(
        "WITH claimed AS ( \
             UPDATE nexus_subagent_runs \
             SET fanin_consumed_at = NOW() \
             WHERE dispatcher_run_id = $1 AND is_background = true \
               AND fanin_consumed_at IS NULL \
             RETURNING id, kind, status, final_summary, verdict, created_at \
         ) \
         SELECT id::text AS sub_id, kind, status, final_summary, verdict \
         FROM claimed ORDER BY created_at ASC",
    )
    .bind(dispatcher_run_id)
    .fetch_all(proj_pool)
    .await
    .unwrap_or_else(|e| {
        tracing::warn!(
            target: "mcp_core::fanin",
            dispatcher_run_id = %dispatcher_run_id,
            error = %e,
            "fan-in: claim dei risultati figli fallito (infra) -> resume con esiti vuoti"
        );
        Vec::new()
    });
    rows.into_iter()
        .map(|r| {
            use sqlx::Row;
            serde_json::json!({
                "subagent_run_id": r.try_get::<String, _>("sub_id").unwrap_or_default(),
                "kind": r.try_get::<String, _>("kind").unwrap_or_default(),
                "status": r.try_get::<String, _>("status").unwrap_or_default(),
                "summary": r.try_get::<Option<String>, _>("final_summary").unwrap_or(None),
                // Verdetto ESITO strutturato (mig project/0009): il padre legge
                // success/verdict qui, mai dalla prosa di `summary` (regola M).
                "outcome": r.try_get::<Option<Value>, _>("verdict").unwrap_or(None),
            })
        })
        .collect()
}

/// RESUME FAN-IN completo di un run nativo (Engine::Rust) sospeso su
/// `awaiting_subagents` (Fase D Slice 3). Gemello di [`confirm_native_run`]
/// (regola L), ma innescato dal WORKER fan-in (non dall'utente) quando l'ultimo
/// figlio background del parent completa. Differenze dall'HITL:
/// - provider/model/session sono LETTI da `agent_runs` (il worker ha solo gli id
///   della coda), non passati come parametri;
/// - il delta iniettato porta i `subagent_results` (via `resume_native_fanin`),
///   non un messaggio umano di approvazione;
/// - il flag di interrupt azzerato e' `awaiting_subagents`, non
///   `awaiting_confirmation`.
///
/// Il CAS che garantisce UN solo resume (`awaiting_subagents -> running`) e' del
/// worker chiamante: questa funzione presuppone di aver vinto il CAS. Riprende il
/// grafo dal checkpoint Postgres (`resume_native_fanin`), mappa l'esito col
/// mapping unico (`native_outcome_to_run_result`), persiste lo stato terminale +
/// emette `is_final` sul canale SSE. Su un nuovo interrupt fan-in il run torna
/// `awaiting_subagents` (un ulteriore giro di figli background) e sara' ri-accodato
/// dai loro finalize. Su errore del motore -> `failed` ONESTO (regola H: nessun
/// fallback al brain).
pub(crate) async fn resume_fanin(
    state: &AppState,
    parent_run_id: Uuid,
    project_id: Uuid,
    session_id: Uuid,
) -> Result<crate::agent_types::AgentRunStatus, String> {
    // Pool del progetto (separazione DB): agent_runs / nexus_subagent_runs sono
    // migrate. Risolto UNA volta, riusato per lettura provider/model, risultati
    // dei figli e UPDATE di finalize.
    let cn_pool = crate::project_db_routes::project_data_pool_from(&state.db, project_id)
        .await
        .map_err(|e| e.to_string())?;

    // provider/model del run originale dal DB (il worker ha solo gli id): servono
    // a popolare ctx + porte I/O del resume (il grafo riparte comunque dal
    // checkpoint). Se il run non esiste piu' (stale), esce senza resume.
    let row = sqlx::query("SELECT provider, model FROM agent_runs WHERE id = $1")
        .bind(parent_run_id)
        .fetch_optional(&cn_pool)
        .await
        .map_err(|e| format!("lookup run padre fallito: {e}"))?;
    let (provider, model) = match row {
        Some(r) => {
            use sqlx::Row;
            (
                r.try_get::<Option<String>, _>("provider")
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
                r.try_get::<Option<String>, _>("model")
                    .ok()
                    .flatten()
                    .unwrap_or_default(),
            )
        }
        None => return Err(format!("run padre {parent_run_id} non trovato (stale)")),
    };

    // Risultati strutturati dei figli background da iniettare nello stato. I figli
    // DIRETTI di questo run sono registrati con `dispatcher_run_id = parent_run_id`
    // (= ctx.core.run_id del run che li ha dispatchati, cioe' proprio QUESTO run
    // sospeso e ripreso ora; mig project 0010). NON via `parent_run_id = anchor`
    // (COALESCE che degenera in session_id e inietterebbe anche i NIPOTI annidati
    // dispatchati da un altro figlio ancora vivo: ALTA 1). La coda porta gia' il
    // dispatcher (il run corrente), quindi il fetch usa direttamente il suo id.
    let subagent_results = claim_subagent_fanin_results(&cn_pool, parent_run_id).await;

    // Canale SSE del run: riusa quello esistente (client agganciati) o creane uno
    // nuovo (dopo un restart) cosi' l'is_final finale sblocca i reattach.
    let tx = match state.agent_channels.get(&parent_run_id) {
        Some(ch) => ch.clone(),
        None => {
            let (tx, _rx) = broadcast::channel::<AgentStepEvent>(256);
            state.agent_channels.insert(parent_run_id, tx.clone());
            tx
        }
    };

    // Input MINIMO per il resume (come confirm_native_run): il grafo riparte dal
    // checkpoint, quindi prompt/tools/history NON servono. Restano provider/model/
    // session per ctx + porte e il canale SSE.
    let input = crate::native_engine::NativeRunInput {
        run_id: parent_run_id,
        session_id,
        provider,
        model,
        system_text: String::new(),
        initial_msg: String::new(),
        conversation_history: Vec::new(),
        tools_json: serde_json::json!([]),
        intent_hint: None,
        requires_tools: None,
        agentic_score: None,
        authorizes_changes: None,
        classifier_resolved: false,
        action_oriented_min_score: crate::intent_classifier::DEFAULT_ACTION_ORIENTED_MIN_SCORE,
        // Il resume fan-in del run principale eredita l'automazione del run
        // originale via checkpoint; qui basta un valore coerente per il ctx.
        automation_mode: "automatic".to_string(),
        supervisor_mode: nexus_agent_graph::SupervisorMode::None,
        step_tx: tx.clone(),
        parent_run_id: None,
        subagent_depth: None,
        sizing_complexity: None,
        sizing_scope_system_wide: false,
        classifier_intent: None,
        run_time_budget_s: None,
        // Resume del run principale sulla root del progetto: nessun isolamento.
        working_root: None,
        pre_run_advisory_synthesis: None,
        pre_run_advisory_source: None,
        // RESUME da checkpoint: i panel a monte hanno gia' deliberato nel primo
        // tratto del run e il loro esito e' nello stato checkpointato (chiave
        // `advisory_gate`); ri-armare la barriera farebbe attendere un verdetto
        // che e' gia' arrivato.
        advisory_gate: None,
    };

    // Costruisce le NativeDeps da AppState (PUNTO UNICO build_native_deps, regola
    // L: stesso cablaggio infra di run_via_native / confirm_native_run) e delega
    // al motore la meccanica di resume col delta fan-in.
    let deps = build_native_deps(state).await;
    let outcome = crate::native_engine::resume_native_fanin(&deps, &input, subagent_results).await;

    let status = match outcome {
        Ok(outcome) => {
            let mut result = native_outcome_to_run_result(&cn_pool, parent_run_id, outcome).await;
            // Riconciliazione costo/token dal ledger (stesso punto unico dello
            // spawn/confirm): senza, l'UPDATE azzererebbe total_cost/total_tokens.
            let ledger_totals = fetch_ledger_totals(&state.db, parent_run_id).await;
            let _ = reconcile_run_cost_from_ledger(&mut result, &ledger_totals);
            let status_str = result.status.as_str();
            let _ = sqlx::query(
                "UPDATE agent_runs SET status=$2, final_answer=$3, iteration_count=$4, \
                 prompt_tokens=$5, completion_tokens=$6, total_tokens=$7, total_cost=$8, \
                 nexus_task_type=$9, provider=$10, model=$11, \
                 messages_json=COALESCE($12, messages_json), completed_at=NOW() \
                 WHERE id=$1",
            )
            .bind(parent_run_id)
            .bind(status_str)
            .bind(result.final_answer.as_deref())
            .bind(result.iteration_count as i32)
            .bind(result.prompt_tokens as i32)
            .bind(result.completion_tokens as i32)
            .bind(result.total_tokens as i32)
            .bind(result.total_cost)
            .bind(result.nexus_task_type.as_deref())
            .bind(&result.provider)
            .bind(&result.model)
            .bind(result.messages_json.as_deref())
            .execute(&cn_pool)
            .await;
            enforce_user_cancellation_status(&cn_pool, parent_run_id).await;
            result.status
        }
        Err(e) => {
            tracing::error!(run_id = %parent_run_id, "resume_fanin: resume nativo fallito");
            let msg = format!(
                "Il resume fan-in del run e' fallito ({}). Il run e' stato chiuso come non riuscito.",
                crate::agent_turn_setup::sanitize_error_for_user(&e.to_string())
            );
            let _ = sqlx::query(
                "UPDATE agent_runs SET status='failed', final_answer=$2, completed_at=NOW() \
                 WHERE id=$1",
            )
            .bind(parent_run_id)
            .bind(&msg)
            .execute(&cn_pool)
            .await;
            crate::agent_types::AgentRunStatus::Failed
        }
    };

    // SOSPENSIONE vs TERMINE (regola L, stesso trattamento del finalize del run
    // principale): il resume fan-in puo' RI-sospendere il padre su una 2a ondata di
    // figli background (`AwaitingSubagents`). In quel caso il run NON e' finito:
    // NON emettere `is_final=true` e NON rimuovere il canale (spegnerebbe la
    // narrazione della 2a ondata e farebbe trattare il run come concluso), ma
    // emettere il meta step di sospensione col nuovo COUNT (punto unico
    // `emit_awaiting_subagents_meta`). I figli della 2a ondata accoderanno una riga
    // fresca al loro completamento -> il worker riprende di nuovo. Solo sugli stati
    // terminali si chiude lo stream.
    if status.is_terminal() {
        let _ = tx.send(AgentStepEvent {
            run_id: parent_run_id.to_string(),
            step: None,
            trace: None,
            is_final: true,
            token_delta: None,
            thinking_delta: None,
            meta_step: None,
        });
        state.agent_channels.remove(&parent_run_id);
    } else if status == AgentRunStatus::AwaitingSubagents {
        let pending = emit_awaiting_subagents_meta(&cn_pool, &tx, parent_run_id).await;
        tracing::info!(
            target: "mcp_core::fanin",
            run_id = %parent_run_id,
            pending_background_children = pending,
            "fan-in: run padre RI-SOSPESO in awaiting_subagents dopo resume (2a ondata, stream mantenuto)"
        );
    } else {
        tracing::info!(
            target: "mcp_core::agent_run",
            run_id = %parent_run_id,
            status = status.as_str(),
            "resume_fanin: run padre SOSPESO in stato non-terminale (stream mantenuto)"
        );
    }

    Ok(status)
}

/// Assembla le `NativeDeps` (ToolRunner in-process + client gateway) da
/// `AppState`. PUNTO UNICO (regola L): sia il run nativo primario
/// (`run_via_native`) sia lo shadow (`run_shadow_for_state`) lo riusano, niente
/// duplicazione del cablaggio infra.
/// Assemblaggio delle `ToolRunnerDeps` da `AppState` (PUNTO UNICO, regola L): stessa
/// slice sottile usata dal server gRPC (`main.rs`) per l'esecuzione IN-PROCESS
/// (mcp-core E' il ToolRunner). Riusato da `build_native_deps` e dal pre-step del
/// consiglio (`spawn_agent_run`), cosi' la costruzione del ctx tool ha una sola sede.
fn build_tool_runner_deps(state: &AppState) -> crate::tool_runner_server::ToolRunnerDeps {
    crate::tool_runner_server::ToolRunnerDeps {
        db: state.db.clone(),
        neural: state.orchestrator.neural.clone(),
        playwright_channels: state.playwright_channels.clone(),
        dependency_status: state.dependency_status.clone(),
        project_channels: state.project_channels.clone(),
        monitor_registry: state.monitor_registry.clone(),
        port_registry: state.port_registry.clone(),
    }
}

/// Pre-step del CONSIGLIO a monte (regola L/M): quando acceso e il messaggio tocca
/// ambiti sensibili, e' il MOTORE (non il modello) a convocare alcune figure
/// sub-agente read-only PRIMA che il run primario agisca; ritorna il blocco
/// `<consiglio_sintesi>` da anteporre al primo messaggio del run. `None` quando non
/// c'e' consiglio: kill-switch OFF, ambito non sensibile, nessuna figura selezionata,
/// sessione non risolvibile o convocazione senza parere valido. In tutti i casi il
/// run primario prosegue INVARIATO — best-effort, mai bloccante, nessun errore risale.
///
/// Non ricorsivo: le figure girano come sub-run via `run_single_subagent` (grafo
/// nativo), NON ripassano da `spawn_agent_run`, quindi il pre-step scatta solo per il
/// run primario della chat.
/// Sceglie quale sintesi (consiglio vs multi-provider) seedare come
/// `pre_run_advisory_synthesis` per l'enforcement al tool_dispatch: vince il
/// verdetto PIU' RESTRITTIVO (block/inconclusive > proceed_with_changes >
/// proceed/none). Cosi' un `block` di UNO QUALSIASI dei due panel ferma
/// l'esecuzione (asimmetria chiusa). Su parita' di rango preferisce il consiglio
/// (panel di dominio, requisiti piu' ricchi). Il `source` seleziona il messaggio
/// di enforcement (regola M: solo "multi_provider_synthesis" e' speciale).
fn select_pre_run_advisory(
    council: Option<serde_json::Value>,
    multi_provider: Option<serde_json::Value>,
) -> (Option<serde_json::Value>, Option<&'static str>) {
    fn rank(v: &serde_json::Value) -> u8 {
        match v.get("verdict").and_then(serde_json::Value::as_str) {
            Some("block") | Some("inconclusive") => 3,
            Some("proceed_with_changes") => 1,
            _ => 0,
        }
    }
    match (council, multi_provider) {
        (Some(c), Some(m)) => {
            if rank(&m) > rank(&c) {
                (Some(m), Some("multi_provider_synthesis"))
            } else {
                (Some(c), Some("council_synthesis"))
            }
        }
        (Some(c), None) => (Some(c), Some("council_synthesis")),
        (None, Some(m)) => (Some(m), Some("multi_provider_synthesis")),
        (None, None) => (None, None),
    }
}

/// Risolve il piano di orchestrazione dimensionato (regola L: punto unico
/// `orchestration_sizing` in nexus-agent-graph; qui SOLO il caricamento degli
/// input). Riusato pre-run (`cost_spent_usd=0`) e post-run (budget residui
/// reali dalla review). `None` = sizing spento o segnali non risolti ->
/// comportamento legacy coi cap storici (fail-safe, mai un piano dimensionato
/// su un fallback del classificatore).
pub(crate) async fn resolve_orchestration_plan_for(
    db: &PgPool,
    complexity: Option<nexus_agent_graph::decisions::orchestration_sizing::TaskComplexity>,
    scope_system_wide: bool,
    decision_detected: bool,
    cost_spent_usd: f64,
    time_remaining_s: Option<i64>,
) -> Option<nexus_agent_graph::decisions::orchestration_sizing::OrchestrationPlan> {
    use nexus_agent_graph::decisions::orchestration_sizing as sizing;
    let cfg =
        crate::agent_tools::subagent_native::read_orchestration_sizing_config(db).await;
    if !cfg.enabled {
        return None;
    }
    let complexity = complexity?;
    let demand =
        crate::agent_tools::subagent_native::read_sizing_profile(db, complexity).await?;
    let backstops =
        crate::agent_tools::subagent_native::read_orchestration_backstops(db).await;
    let unit = crate::agent_tools::subagent_native::read_panel_unit_estimate(db).await;
    // Budget di costo del run: la STESSA chiave anti-runaway dell'executor
    // (`agent.run_cost_budget_usd`, 0 = off). Nessuna seconda fonte di verita'.
    let cost_budget = nexus_auth::get_setting(db, "agent.run_cost_budget_usd")
        .await
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|b| *b > 0.0);
    let budgets = sizing::OrchestrationBudgets {
        cost_remaining_usd: cost_budget.map(|b| (b - cost_spent_usd).max(0.0)),
        // Deadline di run (fase 3, mig 0604): residuo calcolato dal chiamante
        // col punto unico `run_time_remaining_s`. None = deadline disattivata.
        time_remaining_s,
    };
    Some(sizing::resolve_orchestration_plan(
        Some(complexity),
        scope_system_wide,
        decision_detected,
        &budgets,
        &unit,
        &demand,
        &backstops,
        &cfg,
    ))
}

/// Meta-step strutturato `orchestration_plan` (regola M): i numeri del piano e
/// QUALE vincolo li ha decisi (`sized_by`), osservabili in UI e nei log.
async fn emit_orchestration_plan_meta_step(
    run_pool: &PgPool,
    tx: &broadcast::Sender<AgentStepEvent>,
    run_id: Uuid,
    plan: &nexus_agent_graph::decisions::orchestration_sizing::OrchestrationPlan,
    phase: &str,
) {
    let created_at_dt = Utc::now();
    let created_at = created_at_dt.to_rfc3339();
    let title = format!(
        "Orchestrazione dimensionata: vincolo {}",
        plan.sized_by.as_str()
    );
    let mut payload = plan.to_value();
    payload["phase"] = json!(phase);
    if let Err(e) = sqlx::query(
        "INSERT INTO nexus_agent_meta_steps (run_id, kind, title, payload, created_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(run_id)
    .bind("orchestration_plan")
    .bind(&title)
    .bind(&payload)
    .bind(created_at_dt)
    .execute(run_pool)
    .await
    {
        tracing::warn!(
            run_id = %run_id,
            error = %e,
            "orchestration plan: meta_step persistito fallito (best-effort)"
        );
    }
    let _ = tx.send(AgentStepEvent {
        run_id: run_id.to_string(),
        step: None,
        trace: None,
        is_final: false,
        token_delta: None,
        thinking_delta: None,
        meta_step: Some(crate::agent_types::AgentMetaStep {
            kind: "orchestration_plan".to_string(),
            title,
            payload,
            correlation_id: None,
            created_at,
        }),
    });
}

/// Ingredienti dei panel A MONTE, identici nei due rami (bloccante e overlap).
/// Raggrupparli evita di passare 8 argomenti a ogni funzione e di clonarli a
/// mano nel task dell'overlap.
struct UpstreamInputs {
    state: AppState,
    run_pool: PgPool,
    tx: broadcast::Sender<AgentStepEvent>,
    session_id: Uuid,
    run_id: Uuid,
    user_text: String,
    deliberate: bool,
    plan: Option<nexus_agent_graph::decisions::orchestration_sizing::OrchestrationPlan>,
    complexity: Option<nexus_agent_graph::decisions::orchestration_sizing::TaskComplexity>,
    scope_system_wide: bool,
}

/// Esito dei panel a monte: i blocchi testuali per il prompt + il verdetto
/// strutturato per l'enforcement (regola M).
struct UpstreamPanels {
    /// Blocchi gia' renderizzati, nell'ordine consiglio -> dibattito ->
    /// multi-provider (dal generale al particolare), vuoti scartati.
    blocks: Vec<String>,
    synthesis: Option<serde_json::Value>,
    source: Option<&'static str>,
    /// `true` se il consiglio ha prodotto un blocco: la direttiva
    /// `<consiglio_analisi>` va tolta dal system prompt (l'ha gia' fatto).
    council_present: bool,
}

/// Dibattito, se il consiglio ha dichiarato una decisione contesa: consuma il
/// suo segnale strutturato, quindi puo' girare solo DOPO di lui. `None` = niente
/// decisione contesa, o budget che non finanzia il contraddittorio.
async fn run_debate_if_contested(
    inp: &UpstreamInputs,
    council_outcome: Option<&crate::agent_tools::subagent_native::CouncilConveneOutcome>,
) -> Option<String> {
    let advocates = debate_advocate_target(
        &inp.state,
        &inp.run_pool,
        inp.run_id,
        council_outcome,
        inp.complexity,
        inp.scope_system_wide,
    )
    .await?;
    let outcome = maybe_convene_debate(
        &inp.state,
        &inp.run_pool,
        &inp.tx,
        inp.session_id,
        inp.run_id,
        &inp.user_text,
        council_outcome,
        advocates,
    )
    .await?;
    Some(crate::agent_tools::subagent_native::render_debate_synthesis(
        &outcome,
    ))
}

/// Esegue TUTTI i panel a monte: consiglio ∥ multi-provider, poi il dibattito
/// se il consiglio dichiara una decisione contesa.
///
/// PUNTO UNICO (regola L) dei due rami: bloccante (il run parte dopo) e overlap
/// (il run e' gia' partito e questa gira in un task). Senza, l'ordine dei panel
/// e le loro condizioni sarebbero scritti due volte e divergerebbero al primo
/// panel nuovo.
async fn run_upstream_panels(inp: &UpstreamInputs) -> UpstreamPanels {
    // I due panel a monte non condividono dati (le sintesi si riconciliano solo
    // a valle): la serializzazione era implementativa, non necessaria.
    let (council_outcome, multi_provider) = tokio::join!(
        maybe_convene_council(
            &inp.state,
            &inp.run_pool,
            &inp.tx,
            inp.session_id,
            inp.run_id,
            &inp.user_text,
            inp.deliberate,
            inp.plan.as_ref().map(|p| p.council_figures),
        ),
        maybe_convene_multi_provider_panel(
            &inp.state,
            inp.session_id,
            &inp.user_text,
            inp.deliberate,
            inp.plan.as_ref().map(|p| p.multi_provider_providers),
        )
    );
    if let Some(outcome) = &council_outcome {
        emit_council_of_competencies_meta_step(&inp.run_pool, &inp.tx, inp.run_id, outcome).await;
    }
    if let Some(outcome) = &multi_provider {
        emit_multi_provider_panel_meta_step(&inp.run_pool, &inp.tx, inp.run_id, outcome).await;
    }
    let council_block = council_outcome
        .as_ref()
        .map(crate::agent_tools::subagent_native::CouncilConveneOutcome::render_block)
        .filter(|b| !b.is_empty());
    let debate_block = run_debate_if_contested(inp, council_outcome.as_ref()).await;

    let (synthesis, source) = select_pre_run_advisory(
        council_outcome
            .as_ref()
            .and_then(|o| o.advisory_synthesis_value()),
        multi_provider
            .as_ref()
            .and_then(|o| o.advisory_synthesis_value()),
    );
    UpstreamPanels {
        council_present: council_block.is_some(),
        blocks: [
            council_block,
            debate_block,
            multi_provider.as_ref().map(|o| o.render_block()),
        ]
        .into_iter()
        .flatten()
        .filter(|b| !b.trim().is_empty())
        .collect(),
        synthesis,
        source,
    }
}

/// Spawna il task dei panel a monte in OVERLAP col run (mig 0606) e scioglie la
/// barriera di scrittura col loro esito.
///
/// Invariante: la barriera si scioglie SEMPRE, su ogni percorso. Un panel che
/// muore, un panic o un errore devono produrre `Unavailable` — mai il silenzio,
/// che al gate diventerebbe un'attesa fino al timeout (il run resterebbe fermo
/// per nulla). Il `Drop` del sender chiuderebbe comunque il canale e il gate lo
/// leggerebbe come `advisory_channel_closed`: la rete c'e' comunque, ma dire il
/// motivo vero e' meglio che dedurlo da un'assenza (regola M).
fn spawn_advisory_gate_task(
    inp: UpstreamInputs,
    gate_tx: tokio::sync::watch::Sender<nexus_agent_graph::nodes::AdvisoryGateState>,
) {
    let run_id = inp.run_id;
    tokio::spawn(async move {
        let panels = run_upstream_panels(&inp).await;
        let state = gate_state_from_panels(&panels);
        tracing::info!(
            run_id = %run_id,
            gate = ?state,
            "barriera advisory: panel a monte conclusi, barriera sciolta"
        );
        // Il receiver puo' essere gia' caduto (run finito prima che i panel
        // deliberassero: succede sui task brevi che non scrivono nulla). Non e'
        // un errore: il verdetto semplicemente non serve piu'.
        let _ = gate_tx.send(state);
    });
}

/// Traduce l'esito dei panel nello stato della barriera. Nessuna sintesi = i
/// panel non hanno deliberato (roster morto, gate non passato): il run prosegue
/// SENZA approvazione e il modello deve saperlo (regola M).
fn gate_state_from_panels(
    panels: &UpstreamPanels,
) -> nexus_agent_graph::nodes::AdvisoryGateState {
    use nexus_agent_graph::nodes::AdvisoryGateState;
    let Some(synthesis) = panels.synthesis.as_ref() else {
        return AdvisoryGateState::Unavailable {
            reason_code: "advisory_synthesis_unavailable".to_string(),
        };
    };
    // Enforcement col PUNTO UNICO gia' usato dal ramo classico (native_engine):
    // stessa forma, stessa semantica del veto.
    let source = panels.source.unwrap_or("advisory_synthesis");
    match nexus_agent_graph::nodes::panel_enforcement_from_advisory_synthesis(synthesis, source) {
        Some(enforcement) => {
            let terminal = enforcement
                .get("terminal")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if terminal {
                AdvisoryGateState::Vetoed { enforcement }
            } else {
                AdvisoryGateState::Released {
                    enforcement: Some(enforcement),
                }
            }
        }
        // Sintesi senza enforcement = via libera piena: niente vincoli da
        // ricordare.
        None => AdvisoryGateState::Released { enforcement: None },
    }
}

/// Estrae `(topic, options)` dalla decisione contesa dichiarata dal CONSIGLIO.
/// Il segnale arriva dalla sua sintesi: e' il consiglio che sa riconoscere una
/// decisione architetturale aperta (il classificatore ha un contratto 1:1
/// congelato e gira su ogni turno, anche di chat pura). `None` = nessuna
/// decisione contesa dichiarata, o dichiarazione senza opzioni utilizzabili.
fn contested_decision_from(
    council_outcome: Option<&crate::agent_tools::subagent_native::CouncilConveneOutcome>,
) -> Option<(String, Vec<String>)> {
    let synthesis = council_outcome?.advisory_synthesis_value()?;
    let contested = synthesis.get("contested_decision")?;
    let topic = contested.get("topic")?.as_str()?.trim().to_string();
    let options: Vec<String> = contested
        .get("options")?
        .as_array()?
        .iter()
        .filter_map(|o| o.as_str().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect();
    (!topic.is_empty()).then_some((topic, options))
}

/// Quanti avvocati convocare, RI-risolvendo il piano ora che il segnale
/// `contested_decision` del consiglio esiste (pre-run vale sempre 0: la
/// decisione contesa la dichiara il consiglio, non il classificatore).
///
/// PUNTO UNICO (regola L): stesso `resolve_orchestration_plan_for` del pre-run,
/// con `decision_detected=true` e i budget aggiornati — il consiglio ha gia'
/// speso tempo, e il resolver deve vederlo (un dibattito non si finanzia col
/// budget che il consiglio ha appena consumato).
///
/// `None` = nessun dibattito: niente decisione contesa dichiarata, sizing
/// spento, o budget che non regge il floor di 2 avvocati (il resolver lo ha
/// gia' azzerato). Emette il meta-step `orchestration_plan` del secondo giro
/// cosi' la scelta e' osservabile (regola M).
async fn debate_advocate_target(
    state: &AppState,
    run_pool: &PgPool,
    run_id: Uuid,
    council_outcome: Option<&crate::agent_tools::subagent_native::CouncilConveneOutcome>,
    complexity: Option<nexus_agent_graph::decisions::orchestration_sizing::TaskComplexity>,
    scope_system_wide: bool,
) -> Option<usize> {
    // Gate a costo zero: senza decisione contesa non si ri-risolve nulla.
    let synthesis = council_outcome?.advisory_synthesis_value()?;
    synthesis.get("contested_decision").filter(|v| !v.is_null())?;
    let time_remaining =
        crate::agent_tools::subagent_native::run_time_remaining_s(&state.db, run_pool, run_id)
            .await;
    // Costo gia' speso dai panel a monte: il run principale non e' partito, ma
    // consiglio e multi-provider hanno consumato. Punto unico `cumulative_cost`
    // (regola L, lo stesso del cost-cap del prepare): i sub-run hanno run_id
    // propri nel ledger, quindi un'aggregazione per-run del padre li perderebbe.
    let spent = crate::agent_tools::subagent_native::cumulative_cost(run_pool, run_id).await;
    let plan = resolve_orchestration_plan_for(
        &state.db,
        complexity,
        scope_system_wide,
        true, // decision_detected: il consiglio l'ha dichiarata
        spent,
        time_remaining,
    )
    .await?;
    if plan.debate_advocates < 2 {
        tracing::info!(
            run_id = %run_id,
            sized_by = plan.sized_by.as_str(),
            advocates = plan.debate_advocates,
            "dibattito: decisione contesa dichiarata ma il piano non finanzia il contraddittorio"
        );
        return None;
    }
    Some(plan.debate_advocates)
}

/// Convoca il DIBATTITO a tesi contrapposte quando il consiglio ha dichiarato
/// una decisione architetturale CONTESA (`contested_decision`, segnale
/// strutturato — regola M: mai dedotto dalla prosa dei pareri).
///
/// Innescato dal COORDINATORE e mai da dentro un sub-run del consiglio: un
/// sub-run che convoca sub-run consuma il budget di profondita'
/// (`orchestrator.subagent_max_depth`, guard in prepare) e richiederebbe
/// `dispatch_subagents` nella whitelist delle figure — superficie inutile.
///
/// `None` (nessun dibattito) se: nessuna decisione contesa dichiarata, dibattito
/// spento (`orchestrator.debate_enabled`), piano senza avvocati (budget stretto:
/// il resolver ha gia' deciso), o nessun avvocato produce una posizione valida.
async fn maybe_convene_debate(
    state: &AppState,
    run_pool: &PgPool,
    tx: &broadcast::Sender<AgentStepEvent>,
    session_id: Uuid,
    run_id: Uuid,
    user_text: &str,
    council_outcome: Option<&crate::agent_tools::subagent_native::CouncilConveneOutcome>,
    advocate_target: usize,
) -> Option<crate::agent_tools::subagent_native::DebatePanelOutcome> {
    if advocate_target < 2 {
        return None;
    }
    let (topic, options) = contested_decision_from(council_outcome)?;
    let cfg = crate::agent_tools::subagent_native::read_debate_config(&state.db).await?;
    let deps = build_tool_runner_deps(state);
    let svc = crate::tool_runner_server::ToolRunnerService::new(deps);
    let ctx = match svc.build_ctx_for_primary_run(session_id, run_id).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(run_id = %run_id, "dibattito: build_ctx fallita: {e}");
            return None;
        }
    };
    let (quorum, quorum_pct) = debate_quorum_policy(&ctx).await;
    tracing::info!(
        run_id = %run_id,
        topic = %topic,
        options = options.len(),
        advocates = advocate_target,
        "dibattito: convocazione degli avvocati su decisione contesa"
    );
    let outcome = crate::agent_tools::subagent_native::convene_debate_panel(
        &ctx,
        &cfg,
        &topic,
        &options,
        advocate_target,
        user_text,
        &quorum,
        quorum_pct,
    )
    .await?;
    emit_debate_meta_step(run_pool, tx, run_id, &outcome).await;
    Some(outcome)
}

/// Policy di quorum del dibattito, nel vocabolario GENERICO di `panel_quorum`.
///
/// Il dibattito CONDIVIDE le chiavi del quorum dei panel a monte
/// (`orchestrator.council_advisory_*`): e' la stessa domanda — "quanti voti
/// servono perche' il panel abbia deliberato" — quindi nessuna chiave nuova da
/// tenere allineata a mano (regola L). Ritorna anche `quorum_pct`, che nel tipo
/// generico non esiste (li' la soglia relativa e' un parametro di
/// `required_valid`, non un campo della policy).
async fn debate_quorum_policy(
    ctx: &crate::agent_tools::AgentToolContext,
) -> (nexus_agent_graph::decisions::panel_quorum::QuorumPolicy, u8) {
    let advisory = crate::agent_tools::subagent_native::read_advisory_policy(ctx).await;
    (
        nexus_agent_graph::decisions::panel_quorum::QuorumPolicy {
            min_valid: advisory.min_valid_advisories,
            veto_on_high_severity: advisory.block_on_high_severity,
        },
        advisory.quorum_pct,
    )
}

/// Payload del meta-step `debate_panel`: la sintesi strutturata + chi difendeva
/// cosa (regola M: l'assegnazione e' il fatto che rende leggibile il tally).
fn debate_meta_payload(
    outcome: &crate::agent_tools::subagent_native::DebatePanelOutcome,
) -> serde_json::Value {
    let mut payload = outcome.synthesis.to_value();
    payload["product_name"] = json!("Tesi contrapposte");
    payload["topic"] = json!(outcome.topic);
    payload["assignments"] = json!(outcome
        .assignments
        .iter()
        .map(|a| json!({
            "advocate_index": a.advocate_index,
            "assigned_position": a.assigned_position,
        }))
        .collect::<Vec<_>>());
    payload
}

/// Meta-step strutturato `debate_panel` (regola M): esito, tally per opzione e
/// base dei voti, osservabili in UI senza rileggere la prosa.
async fn emit_debate_meta_step(
    run_pool: &PgPool,
    tx: &broadcast::Sender<AgentStepEvent>,
    run_id: Uuid,
    outcome: &crate::agent_tools::subagent_native::DebatePanelOutcome,
) {
    let created_at_dt = Utc::now();
    let created_at = created_at_dt.to_rfc3339();
    let title = format!(
        "Tesi contrapposte: {} ({})",
        outcome.topic,
        outcome.synthesis.verdict.as_str()
    );
    let payload = debate_meta_payload(outcome);
    if let Err(e) = sqlx::query(
        "INSERT INTO nexus_agent_meta_steps (run_id, kind, title, payload, created_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(run_id)
    .bind("debate_panel")
    .bind(&title)
    .bind(&payload)
    .bind(created_at_dt)
    .execute(run_pool)
    .await
    {
        tracing::warn!(
            run_id = %run_id,
            error = %e,
            "dibattito: meta_step persistito fallito (best-effort)"
        );
    }
    let _ = tx.send(AgentStepEvent {
        run_id: run_id.to_string(),
        step: None,
        trace: None,
        is_final: false,
        token_delta: None,
        thinking_delta: None,
        meta_step: Some(crate::agent_types::AgentMetaStep {
            kind: "debate_panel".to_string(),
            title,
            payload,
            correlation_id: None,
            created_at,
        }),
    });
}

async fn maybe_convene_council(
    state: &AppState,
    run_pool: &PgPool,
    tx: &broadcast::Sender<AgentStepEvent>,
    session_id: Uuid,
    run_id: Uuid,
    user_text: &str,
    deliberate: bool,
    figure_target: Option<usize>,
) -> Option<crate::agent_tools::subagent_native::CouncilConveneOutcome> {
    use crate::agent_tools::subagent_native::{
        CouncilConveneOutcome, CouncilDegradeReason,
    };
    // Kill-switch DB-driven (regola G), default OFF: nessun fallback hardcoded, la
    // feature va accesa esplicitamente in `settings`.
    let enabled = nexus_auth::get_bool_setting(&state.db, "orchestrator.council_enabled")
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    if !enabled {
        return None;
    }
    // Decisione AGENTICA (regola M): `deliberate` viene dal giudizio del
    // classificatore LLM (o dal fallback keyword se il classificatore e' down),
    // deciso a monte dal chiamante. Qui si RISPETTA quella decisione: un task che
    // non la merita non convoca il consiglio.
    if !deliberate {
        return None;
    }
    // Selezione figure (DB-driven + routing per ambito): lista vuota -> niente.
    let cfg = crate::agent_tools::subagent_native::read_council_config(&state.db).await;
    let mut figures = crate::agent_tools::subagent_native::select_council_figures(user_text, &cfg);
    // Target del piano di orchestrazione (punto unico orchestration_sizing): la
    // DECISIONE del numero e' del resolver, qui si applica. 0 = panel azzerato
    // dal budget: non si convoca (il meta-step `orchestration_plan` lo documenta).
    if let Some(target) = figure_target {
        if target == 0 {
            return None;
        }
        figures.truncate(target);
    }
    if figures.is_empty() {
        return None;
    }
    // Kill-switch globale sub-agent (regola G): senza sub-agent il consiglio non
    // puo' convocare le figure, ma il gate e' passato -> degrado strutturato + UI.
    let subagents_enabled =
        nexus_auth::get_bool_setting(&state.db, "orchestrator.subagents_enabled")
            .await
            .ok()
            .flatten()
            .unwrap_or(false);
    if !subagents_enabled {
        tracing::warn!(
            session_id = %session_id,
            figure = ?figures,
            "consiglio a monte: sub-agents disabilitati (orchestrator.subagents_enabled=false)"
        );
        return Some(CouncilConveneOutcome::Degraded {
            reason: CouncilDegradeReason::SubagentsDisabled,
            figures,
            figure_reports: Vec::new(),
        });
    }
    // Costruzione del ctx tool ancorato al run primario (PUNTO UNICO
    // ToolRunnerService::build_ctx_for_primary_run, regola L): depth/cost isolati
    // per run, non per sessione.
    let deps = build_tool_runner_deps(state);
    let svc = crate::tool_runner_server::ToolRunnerService::new(deps);
    let ctx = match svc.build_ctx_for_primary_run(session_id, run_id).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                run_id = %run_id,
                "consiglio a monte: build_ctx fallita (session={session_id}): {e}"
            );
            return Some(CouncilConveneOutcome::Degraded {
                reason: CouncilDegradeReason::BuildCtxFailed,
                figures,
                figure_reports: Vec::new(),
            });
        }
    };
    let policy = crate::agent_tools::subagent_native::read_advisory_policy(&ctx).await;
    tracing::info!(
        session_id = %session_id,
        run_id = %run_id,
        figure = ?figures,
        "consiglio a monte: convocazione programmatica delle figure"
    );

    let total = figures.len();
    emit_council_convening_meta_step(run_pool, tx, run_id, &figures, &[], 0, total).await;

    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel(total.max(1));
    let ctx_bg = ctx.clone();
    let user_text_bg = user_text.to_string();
    let figures_bg = figures.clone();
    let policy_bg = policy;
    let convene_handle = tokio::spawn(async move {
        let kinds_bg: Vec<&str> = figures_bg.iter().map(String::as_str).collect();
        crate::agent_tools::subagent_native::convene_council(
            &ctx_bg,
            &user_text_bg,
            &kinds_bg,
            &policy_bg,
            Some(progress_tx),
        )
        .await
    });

    let mut completed_reports: Vec<crate::agent_tools::subagent_native::FigureAdvisoryReport> =
        Vec::with_capacity(total);
    while let Some(report) = progress_rx.recv().await {
        completed_reports.push(report);
        emit_council_convening_meta_step(
            run_pool,
            tx,
            run_id,
            &figures,
            &completed_reports,
            completed_reports.len(),
            total,
        )
        .await;
    }

    let convoke = match convene_handle.await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                run_id = %run_id,
                error = %e,
                "consiglio a monte: task convocazione join fallito"
            );
            return Some(CouncilConveneOutcome::Degraded {
                reason: CouncilDegradeReason::SynthesisUnavailable,
                figures,
                figure_reports: completed_reports,
            });
        }
    };
    for report in &convoke.figure_reports {
        if report.status != crate::agent_tools::subagent_native::FigureAdvisoryStatus::AdvisoryOk
        {
            tracing::warn!(
                session_id = %session_id,
                run_id = %run_id,
                kind = %report.kind,
                status = ?report.status,
                detail_code = %report.detail_code,
                "consiglio a monte: figura senza parere valido"
            );
        }
    }
    let synthesis = match convoke.synthesis {
        Some(s) => s,
        None => {
            tracing::warn!(
                session_id = %session_id,
                run_id = %run_id,
                figure = ?figures,
                "consiglio a monte: nessuna sintesi valida dalle figure"
            );
            return Some(CouncilConveneOutcome::Degraded {
                reason: CouncilDegradeReason::SynthesisUnavailable,
                figures,
                figure_reports: convoke.figure_reports,
            });
        }
    };
    tracing::info!(
        session_id = %session_id,
        verdict = %synthesis.verdict.as_str(),
        pareri_validi = synthesis.valid,
        figure_convocate = synthesis.convened,
        quorum_minimo = synthesis.required_valid,
        requisiti = synthesis.requirements.len(),
        rischi = synthesis.risks.len(),
        "consiglio a monte: sintesi composta, iniezione nel primo messaggio"
    );
    Some(CouncilConveneOutcome::Active {
        synthesis: Box::new(synthesis),
        figures,
        figure_reports: convoke.figure_reports,
    })
}

/// `true` se il path e' un file di CODICE (una modifica a codice merita review;
/// config/markdown/asset no). Estensione case-insensitive.
pub(crate) fn is_code_file(path: &str) -> bool {
    const CODE_EXT: &[&str] = &[
        "ts", "tsx", "js", "jsx", "mjs", "cjs", "rs", "py", "go", "java", "rb", "php", "cs", "cpp",
        "cc", "c", "h", "hpp", "vue", "svelte", "sql", "kt", "swift",
    ];
    path.rsplit('.')
        .next()
        .map(|e| CODE_EXT.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Segnali strutturati dagli step del run (regola M, MAI dalla prosa): file di
/// CODICE modificati + se un panel di review e' GIA' stato prodotto. Il tool reale
/// e' annidato in `agent_steps.tool_input` (`{tool_name, tool_input:{path}}`); la
/// presenza di un panel si legge dal `panel_verdict` nel tool_result del fan-in.
pub(crate) async fn review_gate_signals(pool: &PgPool, run_id: Uuid) -> (Vec<String>, bool) {
    use sqlx::Row;
    const WRITE_TOOLS: &[&str] = &["write_file", "edit_file", "create_file", "patch_file"];
    let rows = sqlx::query(
        "SELECT tool_input->>'tool_name' AS tname, \
                tool_input->'tool_input'->>'path' AS fpath, \
                tool_result \
         FROM agent_steps WHERE run_id = $1 ORDER BY step_index",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let mut modified: Vec<String> = Vec::new();
    let mut reviewed = false;
    for r in rows {
        let tname: String = r.try_get::<Option<String>, _>("tname").ok().flatten().unwrap_or_default();
        if WRITE_TOOLS.contains(&tname.as_str()) {
            if let Some(p) = r.try_get::<Option<String>, _>("fpath").ok().flatten() {
                if is_code_file(&p) && !modified.contains(&p) {
                    modified.push(p);
                }
            }
        }
        if !reviewed {
            if let Some(res) = r.try_get::<Option<String>, _>("tool_result").ok().flatten() {
                if res.contains("panel_verdict") {
                    reviewed = true;
                }
            }
        }
    }
    (modified, reviewed)
}

/// Nota onesta (regola M) da anteporre al resoconto quando la review NON
/// approva. Il `Value` e' `PanelOutcome::to_value` trasportato dallo stato del
/// grafo (`extra.review_panel_last`): stessi campi strutturati, vocabolario di
/// `PanelVerdict::as_str`. Findings limitati per non gonfiare.
fn render_review_panel_note(panel: &serde_json::Value) -> String {
    let label = match panel.get("verdict").and_then(|v| v.as_str()).unwrap_or("") {
        "fail" => "NON superata (difetti bloccanti)",
        "needs_changes" => "richiede modifiche",
        "inconclusive" => "non conclusiva (quorum non raggiunto)",
        "pass" => "superata",
        _ => "esito non disponibile",
    };
    let valid = panel.get("valid").and_then(|v| v.as_u64()).unwrap_or(0);
    let total = panel
        .get("total_reviews")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let mut s = format!(
        "**Review adversariale automatica: {label}** ({valid}/{total} voti validi). Un panel di \
         revisori indipendenti ha esaminato le modifiche di questo run."
    );
    let vuoto = Vec::new();
    let findings = panel
        .get("findings")
        .and_then(|v| v.as_array())
        .unwrap_or(&vuoto);
    let mut shown = 0;
    for f in findings {
        if shown >= 8 {
            break;
        }
        let desc = f.get("description").and_then(|v| v.as_str()).unwrap_or("");
        if desc.is_empty() {
            continue;
        }
        let sev = f.get("severity").and_then(|v| v.as_str()).unwrap_or("");
        let file = f.get("file").and_then(|v| v.as_str()).unwrap_or("");
        s.push_str(&format!("\n- [{sev}] {file}: {desc}"));
        shown += 1;
    }
    s.push_str("\n\nControlla e correggi i punti sopra prima di considerare il task concluso.");
    s
}

async fn maybe_convene_multi_provider_panel(
    state: &AppState,
    session_id: Uuid,
    user_text: &str,
    deliberate: bool,
    provider_target: Option<usize>,
) -> Option<crate::agent_tools::subagent_native::MultiProviderPanelOutcome> {
    // Decisione AGENTICA (regola M): stesso segnale del consiglio (giudizio del
    // classificatore, fallback keyword se down). Il multi-provider e' un panel di
    // deliberazione a monte come il consiglio: condivide la soglia di attivazione.
    if !deliberate {
        return None;
    }
    let mut cfg = crate::agent_tools::subagent_native::read_multi_provider_config(&state.db).await?;
    // Target del piano di orchestrazione (punto unico orchestration_sizing):
    // 0 = panel azzerato dal budget, non si convoca; altrimenti il target lima
    // max_providers (il floor di quorum e' gia' garantito dal resolver).
    if let Some(target) = provider_target {
        if target == 0 {
            return None;
        }
        cfg.max_providers = cfg.max_providers.min(target);
        cfg.min_providers = cfg.min_providers.min(cfg.max_providers);
    }
    let deps = build_tool_runner_deps(state);
    let svc = crate::tool_runner_server::ToolRunnerService::new(deps);
    let ctx = match svc.build_ctx(session_id).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("multi-provider panel: build_ctx fallita (session={session_id}): {e}");
            return None;
        }
    };
    let policy = crate::agent_tools::subagent_native::read_advisory_policy(&ctx).await;
    tracing::info!(
        session_id = %session_id,
        purpose = %cfg.purpose,
        max_providers = cfg.max_providers,
        "multi-provider panel: convocazione programmatica"
    );
    let outcome = crate::agent_tools::subagent_native::convene_multi_provider_panel(
        &ctx,
        user_text,
        &cfg,
        &policy,
    )
    .await?;
    match &outcome {
        crate::agent_tools::subagent_native::MultiProviderPanelOutcome::Active { synthesis, .. } => {
            tracing::info!(
                session_id = %session_id,
                verdict = %synthesis.verdict.as_str(),
                valid = synthesis.valid,
                total = synthesis.total_advisories,
                "multi-provider panel: sintesi composta, iniezione nel primo messaggio"
            );
        }
        crate::agent_tools::subagent_native::MultiProviderPanelOutcome::Degraded {
            reason,
            got,
            min,
        } => {
            tracing::warn!(
                session_id = %session_id,
                reason = ?reason,
                got = got,
                min = min,
                "multi-provider panel: degrado strutturato, nessuna sintesi iniettata"
            );
        }
    }
    Some(outcome)
}

/// Progresso live del Consiglio: lista figure con stato running/done/failed mentre
/// la convocazione parallela e' in corso. Emesso a inizio (0/N) e ad ogni figura
/// che termina, cosi' la chat non resta muta per minuti.
async fn emit_council_convening_meta_step(
    run_pool: &PgPool,
    tx: &broadcast::Sender<AgentStepEvent>,
    run_id: Uuid,
    figures: &[String],
    completed_reports: &[crate::agent_tools::subagent_native::FigureAdvisoryReport],
    completed_count: usize,
    total: usize,
) {
    let created_at_dt = Utc::now();
    let created_at = created_at_dt.to_rfc3339();
    let figure_tasks = crate::agent_tools::subagent_native::council_figure_tasks(
        figures,
        completed_reports,
    );
    let figure_tasks_json: Vec<serde_json::Value> = figure_tasks
        .iter()
        .map(|t| serde_json::to_value(t).unwrap_or_else(|_| json!({})))
        .collect();
    let figure_reports_json: Vec<serde_json::Value> = completed_reports
        .iter()
        .map(|r| serde_json::to_value(r).unwrap_or_else(|_| json!({})))
        .collect();
    let title = format!("Consiglio in corso ({completed_count}/{total})");
    let payload = json!({
        "product_name": "Consiglio delle Competenze",
        "activation_source": "agentic_deterministic_complexity_scope_analysis",
        "signal": "council_convening",
        "phase": "convening",
        "activated": false,
        "degraded": false,
        "figure_count": total,
        "completed_count": completed_count,
        "figure_tasks": figure_tasks_json,
        "figure_reports": figure_reports_json,
    });
    if let Err(e) = sqlx::query(
        "INSERT INTO nexus_agent_meta_steps (run_id, kind, title, payload, created_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(run_id)
    .bind("council_of_competencies")
    .bind(&title)
    .bind(&payload)
    .bind(created_at_dt)
    .execute(run_pool)
    .await
    {
        tracing::warn!(
            run_id = %run_id,
            error = %e,
            "consiglio competenze: meta_step progresso persistito fallito (best-effort)"
        );
    }
    let _ = tx.send(AgentStepEvent {
        run_id: run_id.to_string(),
        step: None,
        trace: None,
        is_final: false,
        token_delta: None,
        thinking_delta: None,
        meta_step: Some(crate::agent_types::AgentMetaStep {
            kind: "council_of_competencies".to_string(),
            title,
            payload,
            correlation_id: None,
            created_at,
        }),
    });
}

/// Segnale strutturato per la UI chat: il Consiglio delle Competenze e' stato
/// attivato dal gate agentico/deterministico. Emesso anche in degrado (es.
/// sub-agents off o sintesi assente) cosi' l'utente vede il tentativo di
/// attivazione, non solo l'esito positivo.
async fn emit_council_of_competencies_meta_step(
    run_pool: &PgPool,
    tx: &broadcast::Sender<AgentStepEvent>,
    run_id: Uuid,
    outcome: &crate::agent_tools::subagent_native::CouncilConveneOutcome,
) {
    let created_at_dt = Utc::now();
    let created_at = created_at_dt.to_rfc3339();
    let figure_count = outcome.figures().len();
    let figure_reports_json: Vec<serde_json::Value> = outcome
        .figure_reports()
        .iter()
        .map(|r| serde_json::to_value(r).unwrap_or_else(|_| json!({})))
        .collect();
    let (title, payload) = match outcome {
        crate::agent_tools::subagent_native::CouncilConveneOutcome::Active { synthesis, .. } => (
            "Consiglio delle Competenze attivo".to_string(),
            json!({
                "product_name": "Consiglio delle Competenze",
                "activation_source": "agentic_deterministic_complexity_scope_analysis",
                "signal": "council_synthesis_present",
                "activated": true,
                "degraded": false,
                "figure_count": figure_count,
                "figure_reports": figure_reports_json,
                "advisory_verdict": synthesis.verdict.as_str(),
                "advisory_valid": synthesis.valid,
                "advisory_convened": synthesis.convened,
                "advisory_required_valid": synthesis.required_valid,
                "requirements_count": synthesis.requirements.len(),
                "risks_count": synthesis.risks.len(),
            }),
        ),
        crate::agent_tools::subagent_native::CouncilConveneOutcome::Degraded { reason, .. } => (
            format!("Consiglio delle Competenze degradato ({figure_count} figure)"),
            json!({
                "product_name": "Consiglio delle Competenze",
                "activation_source": "agentic_deterministic_complexity_scope_analysis",
                "signal": "council_degraded",
                "activated": false,
                "degraded": true,
                "figure_count": figure_count,
                "figure_reports": figure_reports_json,
                "degradation_reason": outcome.degradation_reason_code(),
                "degradation_detail": match reason {
                    crate::agent_tools::subagent_native::CouncilDegradeReason::SubagentsDisabled =>
                        "Sub-agents disabilitati (orchestrator.subagents_enabled=false): impossibile convocare le figure.",
                    crate::agent_tools::subagent_native::CouncilDegradeReason::BuildCtxFailed =>
                        "Contesto tool non costruibile per la sessione: convocazione annullata.",
                    crate::agent_tools::subagent_native::CouncilDegradeReason::SynthesisUnavailable =>
                        "Nessuna figura ha prodotto un parere advisory valido.",
                },
            }),
        ),
    };
    if let Err(e) = sqlx::query(
        "INSERT INTO nexus_agent_meta_steps (run_id, kind, title, payload, created_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(run_id)
    .bind("council_of_competencies")
    .bind(&title)
    .bind(&payload)
    .bind(created_at_dt)
    .execute(run_pool)
    .await
    {
        tracing::warn!(
            run_id = %run_id,
            error = %e,
            "consiglio competenze: meta_step persistito fallito (best-effort)"
        );
    }
    let _ = tx.send(AgentStepEvent {
        run_id: run_id.to_string(),
        step: None,
        trace: None,
        is_final: false,
        token_delta: None,
        thinking_delta: None,
        meta_step: Some(crate::agent_types::AgentMetaStep {
            kind: "council_of_competencies".to_string(),
            title,
            payload,
            correlation_id: None,
            created_at,
        }),
    });
}

async fn emit_multi_provider_panel_meta_step(
    run_pool: &PgPool,
    tx: &broadcast::Sender<AgentStepEvent>,
    run_id: Uuid,
    outcome: &crate::agent_tools::subagent_native::MultiProviderPanelOutcome,
) {
    let created_at_dt = Utc::now();
    let created_at = created_at_dt.to_rfc3339();
    let (title, payload) = match outcome {
        crate::agent_tools::subagent_native::MultiProviderPanelOutcome::Active {
            provider_count,
            synthesis,
            panel_providers,
            provider_reports,
        } => (
            format!("Panel multi-provider attivo ({provider_count})"),
            json!({
                "product_name": "Multi-provider advisory",
                "activation_source": "agentic_deterministic_multi_provider_panel",
                "signal": "multi_provider_synthesis_present",
                "activated": true,
                "degraded": false,
                "provider_count": provider_count,
                "panel_providers": panel_providers,
                // Pareri INDIVIDUALI per provider (stessa shape di figure_reports):
                // la UI li rende espandibili per mostrare la differenza tra provider.
                "provider_reports": provider_reports,
                "advisory_verdict": synthesis.verdict.as_str(),
                "requirements_count": synthesis.requirements.len(),
                "risks_count": synthesis.risks.len(),
                "dissent": synthesis.dissent,
            }),
        ),
        crate::agent_tools::subagent_native::MultiProviderPanelOutcome::Degraded {
            reason,
            got,
            min,
        } => (
            format!("Panel multi-provider degradato ({got}/{min})"),
            json!({
                "product_name": "Multi-provider advisory",
                "activation_source": "agentic_deterministic_multi_provider_panel",
                "signal": "multi_provider_degraded",
                "activated": false,
                "degraded": true,
                "degradation_reason": outcome.degradation_reason_code(),
                "degradation_detail": match reason {
                    crate::agent_tools::subagent_native::MultiProviderDegradeReason::PurposeUnavailable =>
                        "Purpose multi-provider non risolvibile dal catalog.",
                    crate::agent_tools::subagent_native::MultiProviderDegradeReason::InsufficientProviderDiversity =>
                        "Provider distinti insufficienti per il quorum configurato.",
                },
                "provider_count_got": got,
                "provider_count_min": min,
            }),
        ),
    };
    if let Err(e) = sqlx::query(
        "INSERT INTO nexus_agent_meta_steps (run_id, kind, title, payload, created_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(run_id)
    .bind("multi_provider_panel")
    .bind(&title)
    .bind(&payload)
    .bind(created_at_dt)
    .execute(run_pool)
    .await
    {
        tracing::warn!(
            run_id = %run_id,
            error = %e,
            "multi-provider panel: meta_step persistito fallito (best-effort)"
        );
    }
    let _ = tx.send(AgentStepEvent {
        run_id: run_id.to_string(),
        step: None,
        trace: None,
        is_final: false,
        token_delta: None,
        thinking_delta: None,
        meta_step: Some(crate::agent_types::AgentMetaStep {
            kind: "multi_provider_panel".to_string(),
            title,
            payload,
            correlation_id: None,
            created_at,
        }),
    });
}

async fn build_native_deps(state: &AppState) -> crate::native_engine::NativeDeps {
    // Dipendenze del ToolRunner concreto: stesso assemblaggio del server gRPC
    // (main.rs), ma per l'esecuzione IN-PROCESS (mcp-core E' il ToolRunner).
    let tool_runner_deps = build_tool_runner_deps(state);

    // Client gateway dalla porta nel DB (regola G/L: punto unico del cablaggio).
    let gateway = crate::nexus_gateway::NexusGatewayClient::from_db(&state.db).await;

    crate::native_engine::NativeDeps {
        db: state.db.clone(),
        tool_runner_deps,
        gateway,
    }
}

/// Avvio del run SHADOW (Engine::Shadow): ri-esegue il grafo Rust in modalita'
/// shadow (read-only) confrontando lo stato finale col primario gia'
/// concluso (`primary_run_id`). Riusa il PUNTO UNICO `build_native_deps` +
/// `native_engine::run_shadow` (regola L). Non prodotto dal routing globale (che
/// e' rust); attivabile solo per-sessione con `engine='shadow'`. Lo shadow non
/// impatta MAI il primario: su errore il chiamante logga WARN.
pub(crate) async fn run_shadow_for_state(
    state: &AppState,
    input: &crate::native_engine::NativeRunInput,
    primary_run_id: Uuid,
) -> anyhow::Result<()> {
    let deps = build_native_deps(state).await;
    crate::native_engine::run_shadow(&deps, input, primary_run_id).await
}

#[cfg(test)]
mod tests_compose_report {
    use super::{compose_unconfirmed_report, NOTE_GATE_NON_SUPERATA};

    #[test]
    fn provenienza_inclusa_e_nota_preservata() {
        let out = compose_unconfirmed_report(
            NOTE_GATE_NON_SUPERATA,
            "non confermata dalla verifica",
            "mistral",
            "mistral-small-latest",
            "Ho fatto X.",
        );
        // Contratto a valle (regola M): il prefisso nota resta il primo carattere.
        assert!(out.starts_with("**Verifica automatica non superata**"), "{out}");
        // Provenienza inclusa nell'header del resoconto.
        assert!(out.contains("mistral/mistral-small-latest · auto-valutazione"), "{out}");
        // Corpo presente, separato dall'header.
        assert!(out.contains("\n\nHo fatto X."), "{out}");
        assert!(out.contains("Resoconto dell'agente"), "{out}");
    }

    #[test]
    fn provenienza_omessa_se_provider_o_model_vuoti() {
        // Senza provider/model noti: header senza provenienza (niente "·" spurio).
        let out = compose_unconfirmed_report("**Nota.**", "non confermata", "", "", "corpo");
        assert!(out.contains("_Resoconto dell'agente (auto-valutazione,"), "{out}");
        assert!(!out.contains(" · "), "nessun separatore provenienza vuoto: {out}");
    }
}

#[cfg(test)]
mod tests_review_gate {
    use super::{is_code_file, select_pre_run_advisory};
    use serde_json::json;

    fn synth(verdict: &str) -> serde_json::Value {
        json!({ "verdict": verdict })
    }

    #[test]
    fn select_pre_run_advisory_vince_il_piu_restrittivo() {
        // block del CONSIGLIO vince su proceed del multi-provider (asimmetria chiusa).
        let (v, src) =
            select_pre_run_advisory(Some(synth("block")), Some(synth("proceed")));
        assert_eq!(v.unwrap()["verdict"], "block");
        assert_eq!(src, Some("council_synthesis"));

        // block del MULTI-PROVIDER vince su proceed_with_changes del consiglio.
        let (v, src) = select_pre_run_advisory(
            Some(synth("proceed_with_changes")),
            Some(synth("block")),
        );
        assert_eq!(v.unwrap()["verdict"], "block");
        assert_eq!(src, Some("multi_provider_synthesis"));

        // parita' -> preferisce il consiglio.
        let (_, src) = select_pre_run_advisory(Some(synth("proceed")), Some(synth("proceed")));
        assert_eq!(src, Some("council_synthesis"));

        // solo uno presente / nessuno.
        assert_eq!(
            select_pre_run_advisory(None, Some(synth("block"))).1,
            Some("multi_provider_synthesis")
        );
        assert_eq!(select_pre_run_advisory(None, None), (None, None));
    }

    #[test]
    fn is_code_file_riconosce_codice_e_scarta_il_resto() {
        for ok in [
            "src/utils/utils.ts",
            "app/Login.tsx",
            "crates/x/src/lib.rs",
            "backend/server.js",
            "db/migrations/0001.sql",
            "MODULE.PY",
        ] {
            assert!(is_code_file(ok), "atteso codice: {ok}");
        }
        for no in [
            "README.md",
            "package.json",
            "styles.css",
            "logo.png",
            "config.yaml",
            "senza_estensione",
        ] {
            assert!(!is_code_file(no), "atteso NON codice: {no}");
        }
    }
}


#[cfg(test)]
mod tests_session_active {
    use super::*;

    /// Sessione con un run nello stato voluto. Lo schema reale vincola
    /// `agent_runs.session_id` a `chat_sessions(id)`: la sessione va seminata,
    /// non inventata (la vecchia fixture a mano, senza FK, la accettava).
    async fn sessione_con_run(pool: &sqlx::PgPool, status: &str) -> Uuid {
        let project = Uuid::new_v4();
        let sess = crate::test_support::seed_chat_session(pool, project).await;
        crate::test_support::insert_agent_run(pool, sess, project, status).await;
        sess
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn vero_su_running(pool: sqlx::PgPool) {
        let sess = sessione_con_run(&pool, "running").await;
        assert!(session_has_active_run(&pool, sess).await);
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn vero_su_awaiting_confirmation(pool: sqlx::PgPool) {
        let sess = sessione_con_run(&pool, "awaiting_confirmation").await;
        assert!(session_has_active_run(&pool, sess).await);
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn vero_su_awaiting_subagents(pool: sqlx::PgPool) {
        // Fase D: un padre sospeso su awaiting_subagents e' un run ATTIVO/sospeso-vivo
        // sulla sessione (punto unico ACTIVE_RUN_STATUSES) -> session_has_active_run
        // deve vederlo, altrimenti process_resume/service_observer avvierebbero un 2o
        // run parallelo (S2 della re-review).
        let sess = sessione_con_run(&pool, "awaiting_subagents").await;
        assert!(session_has_active_run(&pool, sess).await);
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn falso_se_solo_run_conclusi(pool: sqlx::PgPool) {
        let project = Uuid::new_v4();
        let sess = crate::test_support::seed_chat_session(&pool, project).await;
        for status in ["completed", "cancelled", "failed"] {
            crate::test_support::insert_agent_run(&pool, sess, project, status).await;
        }
        assert!(!session_has_active_run(&pool, sess).await);
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn isolamento_per_sessione(pool: sqlx::PgPool) {
        let sess_a = crate::test_support::seed_chat_session(&pool, Uuid::new_v4()).await;
        // Solo sess_b ha un run attivo.
        let sess_b = sessione_con_run(&pool, "running").await;
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
            reasoning: None,
            messages_json: None,
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
            AgentRunStatus::Completed,
            vec![],
            None,
            true,
            "EMPTY_ANSWER",
            Some("fix"),
        );
        assert_eq!(canonical_run_status(&r), AgentRunStatus::FailedDiagnosed);
        // La rinuncia esplicita non e' piu' un kind lessicale (RESIGNED rimosso,
        // ADR 0018 fase 3): un completed con risposta e kind vuoto resta tale
        // (la rinuncia dichiarata passa da task_complete refusal/blocked).
        let r2 = mk_result(
            AgentRunStatus::Completed,
            vec![],
            Some("non posso"),
            true,
            "",
            Some("fix"),
        );
        assert_eq!(canonical_run_status(&r2), AgentRunStatus::Completed);
    }

    #[test]
    fn hollow_con_lavoro_resta_completed() {
        // Hollow ma con step produttivi -> NON declassato (il lavoro c'e').
        let r = mk_result(
            AgentRunStatus::Completed,
            vec![write_step()],
            None,
            true,
            "EMPTY_ANSWER",
            Some("fix"),
        );
        assert_eq!(canonical_run_status(&r), AgentRunStatus::Completed);
    }

    #[test]
    fn is_provider_error_answer_riconosce_marker() {
        assert!(is_provider_error_answer(
            "[Errore provider deepseek: gateway giu']"
        ));
        assert!(is_provider_error_answer("  [Errore provider openai: x]"));
        assert!(!is_provider_error_answer("Ecco il risultato del task."));
        assert!(!is_provider_error_answer("[INFO] qualcosa"));
    }

    #[test]
    fn errore_provider_zero_token_declassato_a_failed() {
        // Incidente Beauty-Book (run 8025e4e3): tutti i provider in cooldown,
        // l'executor sintetizza "[Errore provider ...]" ma il routing post-errore
        // richiude il grafo con stop_reason non-Error -> il run finiva `completed`.
        // Con final_answer di errore + 0 completion_tokens deve essere Failed.
        let r = mk_result(
            AgentRunStatus::Completed,
            vec![write_step()],
            Some(
                "[Errore provider deepseek: gateway LLM non raggiungibile]\n\n\
                 Interrotto dopo 10 iterazioni. Lavoro svolto finora: 15 azioni.",
            ),
            false,
            "",
            Some("agentic_default"),
        );
        assert!(is_provider_error_completion(&r));
        assert_eq!(canonical_run_status(&r), AgentRunStatus::Failed);
    }

    #[test]
    fn errore_provider_con_token_non_declassato() {
        // Falso positivo da evitare: il modello ha prodotto output (completion>0),
        // non e' il caso "0 output per provider giu'": resta Completed.
        let mut r = mk_result(
            AgentRunStatus::Completed,
            vec![write_step()],
            Some("[Errore provider x: transiente, ma poi ho risposto]"),
            false,
            "",
            Some("agentic_default"),
        );
        r.completion_tokens = 10;
        assert!(!is_provider_error_completion(&r));
        assert_eq!(canonical_run_status(&r), AgentRunStatus::Completed);
    }

    #[test]
    fn risposta_normale_resta_completed() {
        let mut r = mk_result(
            AgentRunStatus::Completed,
            vec![write_step()],
            Some("Ho completato il task con successo."),
            false,
            "",
            Some("agentic_default"),
        );
        r.completion_tokens = 42;
        assert!(!is_provider_error_completion(&r));
        assert_eq!(canonical_run_status(&r), AgentRunStatus::Completed);
    }

    #[test]
    fn chat_hollow_non_e_report_hollow() {
        // intent chat: completamento vuoto atteso, non declassato.
        let r = mk_result(
            AgentRunStatus::Completed,
            vec![],
            None,
            true,
            "EMPTY_ANSWER",
            Some("chat"),
        );
        assert_eq!(canonical_run_status(&r), AgentRunStatus::Completed);
    }

    #[test]
    fn compose_garantisce_messaggio_su_hollow() {
        // Hollow con step -> recap delle azioni (mai messaggio assente).
        let r = mk_result(
            AgentRunStatus::Completed,
            vec![write_step()],
            None,
            true,
            "EMPTY_ANSWER",
            Some("fix"),
        );
        let msg = compose_turn_answer(&r).expect("recap atteso");
        assert!(
            msg.contains("a.ts"),
            "il recap deve elencare il file toccato"
        );
        // Hollow senza step -> placeholder esplicito.
        let r2 = mk_result(
            AgentRunStatus::Completed,
            vec![],
            None,
            true,
            "EMPTY_ANSWER",
            Some("fix"),
        );
        let msg2 = compose_turn_answer(&r2).expect("placeholder atteso");
        assert!(msg2.contains("Nessuna risposta utile"));
    }

    #[test]
    fn compose_usa_risposta_reale_quando_presente() {
        let r = mk_result(
            AgentRunStatus::Completed,
            vec![],
            Some("Ecco il risultato."),
            false,
            "",
            Some("fix"),
        );
        assert_eq!(compose_turn_answer(&r).unwrap(), "Ecco il risultato.");
    }

    #[test]
    fn compose_none_se_non_hollow_e_senza_risposta() {
        // Run non-hollow senza final_answer NE step (es. chat che chiude): nessun
        // messaggio. Con 0 step la chat non ha nulla di concreto da riportare.
        let r = mk_result(
            AgentRunStatus::Completed,
            vec![],
            None,
            false,
            "",
            Some("chat"),
        );
        assert!(compose_turn_answer(&r).is_none());
    }

    #[test]
    fn compose_recap_su_run_non_hollow_con_step_e_risposta_vuota() {
        // Regressione run 04a2b2c6 (Beauty-Book): l'agente ripara l'app (step
        // produttivi) ma gemini-2.5-pro entra in loop G1 e il final_gate chiude a
        // max_cycles con final_answer vuoto. Il run NON e' hollow (c'e' lavoro
        // reale) ma la chat restava MUTA (final_answer NULL nel DB). Ora un run
        // con step eseguiti produce SEMPRE il recap: mai un completed senza nulla
        // a schermo.
        let r = mk_result(
            AgentRunStatus::Completed,
            vec![write_step()],
            None,
            false,
            "",
            Some("fix"),
        );
        let msg = compose_turn_answer(&r).expect("recap atteso anche se non-hollow");
        assert!(
            msg.contains("a.ts"),
            "il recap deve elencare il file toccato: {msg}"
        );
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
        // Formato DSML di deepseek (tool-call colata nel content) -> rilevata.
        assert!(super::looks_like_textual_tool_call(
            "<\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c}tool_calls>\n\
             <\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c}invoke name=\"read_file\">"
        ));
        // compose: con step produttivi -> recap, non la spazzatura.
        let r = mk_result(
            AgentRunStatus::Completed,
            vec![write_step()],
            Some(raw),
            false,
            "",
            Some("fix"),
        );
        let msg = compose_turn_answer(&r).expect("recap atteso");
        assert!(
            msg.contains("a.ts"),
            "deve usare il recap deterministico: {msg}"
        );
        assert!(
            !msg.contains("bookingService"),
            "niente tool call nel testo: {msg}"
        );
    }

    #[test]
    fn soli_errori_provider_rilevati_e_sostituiti() {
        // Caso reale run 2c6e41fb: final_answer = concatenazione di 422 Mistral.
        let raw = "[Error: An assistant message with][Error: Unexpected tool call id FbW0bLZsv in tool results][Error: An assistant message with]";
        assert!(super::is_only_provider_errors(raw));
        // Risposte legittime (anche se citano errori) NON matchano.
        assert!(!super::is_only_provider_errors(
            "Il build fallisce con [Error: x]"
        ));
        assert!(!super::is_only_provider_errors("Il DB ha 6 tabelle."));
        assert!(!super::is_only_provider_errors(""));
        // compose: con step -> recap, non la concatenazione di errori.
        let r = mk_result(
            AgentRunStatus::Completed,
            vec![write_step()],
            Some(raw),
            false,
            "",
            Some("fix"),
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
            (
                "anthropic".to_string(),
                300u64,
                Some("credit balance too low".to_string()),
            ),
            (
                "openai".to_string(),
                250u64,
                Some("you exceeded your current quota".to_string()),
            ),
        ];
        let msg = super::cooldown_note_from_snapshot(&snap).expect("nota attesa");
        assert!(
            msg.contains("quota/credito esaurito"),
            "deve indicare la causa billing: {msg}"
        );
        assert!(
            msg.contains("Ricarica"),
            "deve suggerire la ricarica: {msg}"
        );
        assert!(
            msg.contains("anthropic") && msg.contains("openai"),
            "deve elencare i provider: {msg}"
        );
    }

    #[test]
    fn cooldown_note_non_billing_dice_attendi() {
        // Cooldown transitorio (rate-limit) -> "attendi", NON "ricarica".
        let snap = vec![("mistral".to_string(), 60u64, Some("rate limit".to_string()))];
        let msg = super::cooldown_note_from_snapshot(&snap).expect("nota attesa");
        assert!(
            msg.contains("temporaneamente non disponibili"),
            "non-billing -> attendi: {msg}"
        );
        assert!(
            !msg.contains("Ricarica"),
            "non deve dire ricarica per rate-limit: {msg}"
        );
    }

    #[test]
    fn cooldown_note_gate_non_incolpa_provider_ortogonali() {
        // BUG 3 failover (regola M): il turno e' fallito per un completamento
        // VUOTO di google (200 vuoto, thought_signature) — google NON e' in
        // cooldown. openai/anthropic sono in cooldown billing per motivi PROPRI,
        // ortogonali a questo turno. La nota NON deve attribuire il fallimento a
        // openai/anthropic ne' suggerire "ricarica crediti": deve tacere e lasciar
        // parlare il placeholder a valle, che nomina google (result.provider).
        let mut r = mk_result(
            AgentRunStatus::Completed,
            vec![],
            None,
            true,
            "EMPTY_ANSWER",
            Some("fix"),
        );
        r.provider = "google".into();
        r.model = "gemini-2.5-flash".into();
        r.error_class = None; // 200 vuoto: nessuna classe errore strutturata
        let snap = vec![
            (
                "openai".to_string(),
                250u64,
                Some("you exceeded your current quota".to_string()),
            ),
            (
                "anthropic".to_string(),
                300u64,
                Some("credit balance too low".to_string()),
            ),
        ];
        assert!(
            super::cooldown_note_for_turn(&snap, &r).is_none(),
            "il turno fallito per empty_completion di google NON deve produrre la \
             nota cooldown che incolpa openai/anthropic"
        );
    }

    #[test]
    fn cooldown_note_gate_provider_unavailable_usa_snapshot() {
        // Il routing non ha trovato ALCUN provider disponibile (tutti in cooldown):
        // segnale strutturato ProviderUnavailable. In questo caso lo snapshot
        // globale E' la causa legittima del turno -> nota su tutti i provider.
        let r = mk_result(
            AgentRunStatus::ProviderUnavailable,
            vec![],
            None,
            false,
            "",
            None,
        );
        let snap = vec![
            (
                "openai".to_string(),
                250u64,
                Some("you exceeded your current quota".to_string()),
            ),
            (
                "anthropic".to_string(),
                300u64,
                Some("credit balance too low".to_string()),
            ),
        ];
        let msg = super::cooldown_note_for_turn(&snap, &r).expect("nota attesa su ProviderUnavailable");
        assert!(msg.contains("Ricarica"), "billing -> ricarica: {msg}");
        assert!(
            msg.contains("openai") && msg.contains("anthropic"),
            "deve elencare tutti i provider indisponibili: {msg}"
        );
    }

    #[test]
    fn cooldown_note_gate_billing_del_turno_filtra_al_provider() {
        // Il provider DEL TURNO (openai) ha colpito un billing_error strutturato:
        // la nota e' pertinente ma va FILTRATA a openai, non deve trascinare
        // anthropic (in cooldown per motivi propri, ortogonali).
        let mut r = mk_result(AgentRunStatus::Failed, vec![], None, false, "", None);
        r.provider = "openai".into();
        r.error_class = Some("billing_error".into());
        let snap = vec![
            (
                "openai".to_string(),
                250u64,
                Some("you exceeded your current quota".to_string()),
            ),
            (
                "anthropic".to_string(),
                300u64,
                Some("credit balance too low".to_string()),
            ),
        ];
        let msg = super::cooldown_note_for_turn(&snap, &r).expect("nota attesa per billing del turno");
        assert!(msg.contains("openai"), "deve nominare il provider del turno: {msg}");
        assert!(
            !msg.contains("anthropic"),
            "NON deve nominare provider ortogonali al turno: {msg}"
        );
    }

    #[test]
    fn ledger_riconcilia_costo_quando_result_a_zero() {
        // BUG produzione "19.4K token - $0.00": il path NATIVO non aggrega costo
        // nel grafo (result.total_cost/prompt_tokens/... = 0) ma il ledger ha le
        // righe reali per il run. La riconciliazione deve adottare i valori del
        // ledger cosi' che messaggio assistant + agent_runs mostrino il costo vero.
        let mut r = mk_result(
            AgentRunStatus::Completed,
            vec![write_step()],
            Some("fatto"),
            false,
            "",
            Some("fix"),
        );
        assert_eq!(r.total_cost, 0.0);
        let ledger = super::LedgerTotals {
            total_cost: 0.063,
            prompt_tokens: 18_000,
            completion_tokens: 1_400,
            total_tokens: 19_400,
            rows: 3,
        };
        let applied = super::reconcile_run_cost_from_ledger(&mut r, &ledger);
        assert!(
            applied,
            "deve adottare il ledger quando result e' a 0 e ledger ha costo"
        );
        assert!((r.total_cost - 0.063).abs() < 1e-9, "costo dal ledger");
        assert_eq!(r.prompt_tokens, 18_000);
        assert_eq!(r.completion_tokens, 1_400);
        assert_eq!(r.total_tokens, 19_400);
    }

    #[test]
    fn ledger_sovrascrive_il_costo_dell_ultimo_turno() {
        // REGRESSIONE (misurata in produzione, sessione chat 25): result.total_cost
        // NON e' il totale del run ma il costo dell'ULTIMO TURNO (reducer overwrite
        // nel grafo). Il ledger ha le righe di TUTTE le chiamate: quando ne ha,
        // vince, altrimenti il run pubblica l'ultima iterazione come totale
        // (misurato: 0.0338 invece di 0.1510 -> sottostima 4.5x).
        //
        // Il test precedente asseriva il CONTRARIO ("result con costo > 0 non va
        // sovrascritto"), sancendo il difetto sul razionale "il brain Python e'
        // autoritativo": premessa falsa e brain non piu' esistente.
        let mut r = mk_result(
            AgentRunStatus::Completed,
            vec![write_step()],
            Some("fatto"),
            false,
            "",
            Some("fix"),
        );
        r.total_cost = 0.033842; // costo del solo ultimo turno
        r.prompt_tokens = 13_780;
        r.completion_tokens = 1_047;
        r.total_tokens = 14_827;
        let ledger = super::LedgerTotals {
            total_cost: 0.150972, // totale reale del run (8 chiamate)
            prompt_tokens: 63_177,
            completion_tokens: 4_103,
            total_tokens: 67_280,
            rows: 8,
        };
        let applied = super::reconcile_run_cost_from_ledger(&mut r, &ledger);
        assert!(
            applied,
            "il ledger con righe e' autoritativo anche se result ha gia' un costo > 0"
        );
        assert!((r.total_cost - 0.150972).abs() < 1e-9, "costo del RUN, non del turno");
        assert_eq!(r.prompt_tokens, 63_177);
        assert_eq!(r.total_tokens, 67_280);
    }

    #[test]
    fn ledger_con_righe_a_costo_zero_e_comunque_autoritativo() {
        // Prezzo del modello ignoto (pricing_state='unknown') -> il gateway scrive
        // righe con token reali e costo 0. Sono contabilita' valida: i token vanno
        // riconciliati lo stesso. Il vecchio predicato `total_cost > 0` le
        // scambiava per "ledger assente" (regola M: lo stato non si deduce da una
        // grandezza).
        let mut r = mk_result(
            AgentRunStatus::Completed,
            vec![write_step()],
            Some("fatto"),
            false,
            "",
            Some("fix"),
        );
        r.total_cost = 0.02;
        r.prompt_tokens = 10;
        let ledger = super::LedgerTotals {
            total_cost: 0.0,
            prompt_tokens: 15_162_322,
            completion_tokens: 140_049,
            total_tokens: 15_302_371,
            rows: 747,
        };
        let applied = super::reconcile_run_cost_from_ledger(&mut r, &ledger);
        assert!(applied, "righe presenti -> ledger autoritativo anche a costo 0");
        assert_eq!(r.prompt_tokens, 15_162_322, "i token sono reali e vanno adottati");
        assert!(r.total_cost.abs() < 1e-9, "costo onestamente 0: il prezzo e' ignoto, non stimato");
    }

    #[test]
    fn ledger_vuoto_lascia_result_invariato_per_fallback_catalog() {
        // Ledger SENZA RIGHE per il run (provider che non scrive ledger): nessuna
        // riconciliazione -> il chiamante ricade sul fallback calcolo-da-catalog.
        let mut r = mk_result(
            AgentRunStatus::Completed,
            vec![write_step()],
            Some("fatto"),
            false,
            "",
            Some("fix"),
        );
        r.prompt_tokens = 500;
        r.completion_tokens = 100;
        let before = (
            r.total_cost,
            r.prompt_tokens,
            r.completion_tokens,
            r.total_tokens,
        );
        let applied =
            super::reconcile_run_cost_from_ledger(&mut r, &super::LedgerTotals::default());
        assert!(!applied, "ledger senza righe: niente da riconciliare");
        assert_eq!(
            (
                r.total_cost,
                r.prompt_tokens,
                r.completion_tokens,
                r.total_tokens
            ),
            before,
            "result invariato quando il ledger e' vuoto"
        );
    }

    #[test]
    fn ledger_preserva_last_prompt_tokens_per_context_ratio() {
        // Regressione badge "5046% ctx": last_prompt_tokens (prompt dell'ULTIMA
        // iterazione, numeratore del context ratio della UI) NON deve essere
        // toccato dalla riconciliazione ledger, che sovrascrive prompt_tokens
        // col CUMULATIVO di billing di tutte le iterazioni del run. Se i due
        // campi tornassero a coincidere, il ratio esploderebbe di nuovo sui run
        // multi-iterazione.
        let mut r = mk_result(
            AgentRunStatus::Completed,
            vec![write_step()],
            Some("fatto"),
            false,
            "",
            Some("fix"),
        );
        r.last_prompt_tokens = Some(42_000);
        let ledger = super::LedgerTotals {
            total_cost: 0.5,
            prompt_tokens: 1_650_000, // cumulativo multi-iterazione
            completion_tokens: 40_000,
            total_tokens: 1_690_000,
            rows: 12,
        };
        let applied = super::reconcile_run_cost_from_ledger(&mut r, &ledger);
        assert!(
            applied,
            "ledger con righe e' autoritativo per il billing"
        );
        assert_eq!(
            r.prompt_tokens, 1_650_000,
            "billing dal ledger (cumulativo)"
        );
        assert_eq!(
            r.last_prompt_tokens,
            Some(42_000),
            "il riempimento contesto resta quello dell'ultima iterazione"
        );
    }

    #[test]
    fn ledger_total_tokens_zero_ricostruito_da_prompt_e_completion() {
        // Robustezza: alcune righe ledger possono avere total_tokens=0 pur avendo
        // prompt+completion validi. Non mostrare 0 token a fronte di un costo > 0.
        let mut r = mk_result(
            AgentRunStatus::Completed,
            vec![write_step()],
            Some("fatto"),
            false,
            "",
            Some("fix"),
        );
        let ledger = super::LedgerTotals {
            total_cost: 0.01,
            prompt_tokens: 800,
            completion_tokens: 150,
            total_tokens: 0,
            rows: 2,
        };
        assert!(super::reconcile_run_cost_from_ledger(&mut r, &ledger));
        assert_eq!(
            r.total_tokens, 950,
            "total_tokens ricostruito da prompt+completion"
        );
    }

    // ── D3: outcome_summary == buildSemanticDetail (recap unico ricco) ─────────
    fn step(tool: &str, input: serde_json::Value, status: AgentStepStatus) -> AgentStep {
        AgentStep {
            run_id: "r".into(),
            step_index: 0,
            tool_name: tool.into(),
            tool_input: input,
            tool_result: None,
            status,
            created_at: String::new(),
        }
    }

    #[test]
    fn outcome_summary_turno_conversazionale_e_none() {
        // Nessuna azione concreta (solo un tool ignorato) -> None, parita' con
        // buildSemanticDetail che ritorna "" (turno conversazionale).
        let steps = vec![step(
            "supervisor_check",
            serde_json::json!({}),
            AgentStepStatus::Completed,
        )];
        assert!(outcome_summary(&steps).is_none());
        assert!(outcome_summary(&[]).is_none());
    }

    #[test]
    fn outcome_summary_formato_ricco_completo() {
        // Replica esatta del blocco di buildSemanticDetail (run-summary.ts).
        let steps = vec![
            step(
                "write_file",
                serde_json::json!({"path": "src/components/Header.tsx"}),
                AgentStepStatus::Completed,
            ),
            step(
                "edit_file",
                serde_json::json!({"file_path": "a.ts"}),
                AgentStepStatus::Completed,
            ),
            step(
                "run_command",
                serde_json::json!({"command": "pnpm build"}),
                AgentStepStatus::Completed,
            ),
            step(
                "read_file",
                serde_json::json!({"path": "x"}),
                AgentStepStatus::Completed,
            ),
        ];
        let out = outcome_summary(&steps).expect("recap atteso");
        assert!(
            out.starts_with("\n\n**Riepilogo:**\n"),
            "header esatto: {out}"
        );
        // Path con >2 segmenti -> nome breve ".../ultimi2"; path <=2 segmenti intero.
        assert!(
            out.contains("- Modificati 2 file: `.../components/Header.tsx`, `a.ts`"),
            "lista file con nome breve: {out}"
        );
        assert!(
            out.contains("- Eseguiti 1 comandi: `pnpm build`"),
            "comando in backtick: {out}"
        );
        assert!(
            out.contains("- Analizzati 1 file"),
            "conteggio analisi: {out}"
        );
        assert!(
            out.contains("- Risultato: 4 step completati"),
            "esito completati: {out}"
        );
        assert!(
            !out.contains("errori"),
            "nessun errore -> niente suffisso: {out}"
        );
    }

    #[test]
    fn outcome_summary_tronca_file_e_comandi_e_conta_errori() {
        // >5 file -> " e altri N file"; >3 comandi -> " e altri N"; comando lungo
        // troncato a 77 + "..."; step failed conteggiati come errori.
        let mut steps = Vec::new();
        for i in 0..7 {
            steps.push(step(
                "write_file",
                serde_json::json!({ "path": format!("dir/file{i}.ts") }),
                AgentStepStatus::Completed,
            ));
        }
        let long_cmd = "x".repeat(120);
        steps.push(step(
            "run_command",
            serde_json::json!({ "command": long_cmd }),
            AgentStepStatus::Failed,
        ));
        let out = outcome_summary(&steps).expect("recap atteso");
        assert!(
            out.contains("- Modificati 7 file:"),
            "conteggio totale 7: {out}"
        );
        assert!(out.contains(" e altri 2 file"), "extra file: {out}");
        let expected_short = format!("`{}...`", "x".repeat(77));
        assert!(
            out.contains(&expected_short),
            "comando troncato a 77+...: {out}"
        );
        assert!(
            out.contains("- Risultato: 7 step completati, 1 errori"),
            "errori conteggiati: {out}"
        );
    }

    #[test]
    fn outcome_summary_normalizza_backslash_windows() {
        // Path Windows: "\\" -> "/" prima della troncatura nome breve.
        let steps = vec![step(
            "create_file",
            serde_json::json!({"filename": "src\\lib\\util.ts"}),
            AgentStepStatus::Completed,
        )];
        let out = outcome_summary(&steps).expect("recap atteso");
        assert!(
            out.contains("`.../lib/util.ts`"),
            "backslash normalizzati: {out}"
        );
    }

    #[test]
    fn short_file_label_due_segmenti_intero() {
        // <=2 segmenti -> path intero (parita' col ternario di run-summary.ts).
        assert_eq!(short_file_label("a/b"), "a/b");
        assert_eq!(short_file_label("solo.ts"), "solo.ts");
        assert_eq!(short_file_label("a/b/c/d.ts"), ".../c/d.ts");
    }

    /// Sub-run di background nello stato voluto, con i NOT NULL dello schema
    /// reale (`project_id`, `kind`, `task_description`) che la vecchia fixture a
    /// mano non aveva: il DB di produzione non accetta un sub-run senza.
    async fn insert_sub_run(
        pool: &sqlx::PgPool,
        anchor: Uuid,
        dispatcher: Uuid,
        status: &str,
    ) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO nexus_subagent_runs \
             (id, parent_run_id, dispatcher_run_id, project_id, kind, task_description, \
              status, is_background) \
             VALUES ($1, $2, $3, $4, 'coder', 'task di test', $5, true)",
        )
        .bind(id)
        .bind(anchor)
        .bind(dispatcher)
        .bind(Uuid::new_v4())
        .bind(status)
        .execute(pool)
        .await
        .expect("insert sub-run");
        id
    }

    /// ALTA 1 (isolamento per-run del FETCH): con l'annidamento tutti i sub-run
    /// condividono `parent_run_id = session_id` (anchor), ma il fetch del padre P
    /// deve iniettare SOLO i suoi figli DIRETTI (dispatcher_run_id = P), MAI i
    /// nipoti Cs1 dispatchati da un figlio Cp1 (dispatcher_run_id = Cp1). Il fetch
    /// filtra su `dispatcher_run_id` (mig project 0010): riproduce il bug perche'
    /// col vecchio filtro `parent_run_id = anchor` la lista di P conterrebbe ANCHE
    /// Cs1 (nipote mai dispatchato da P).
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn fetch_fanin_isola_per_dispatcher_non_anchor(pool: sqlx::PgPool) {
        // Annidamento: tutti i sub-run hanno parent_run_id = session (anchor
        // degenere), ma dispatcher distinto. P dispatcha Cp1,Cp2; Cp1 dispatcha Cs1.
        let session = Uuid::new_v4();
        let p_run = Uuid::new_v4();
        let cp1_run = Uuid::new_v4();
        let insert = |anchor: Uuid, dispatcher: Uuid, status: &'static str| {
            let pool = pool.clone();
            async move {
                insert_sub_run(&pool, anchor, dispatcher, status).await;
            }
        };
        // Cp1, Cp2: figli DIRETTI di P (dispatcher = P), terminali.
        insert(session, p_run, "completed").await;
        insert(session, p_run, "completed").await;
        // Cs1: NIPOTE, figlio di Cp1 (dispatcher = Cp1), ancora running.
        insert(session, cp1_run, "running").await;

        // Fetch di P: deve vedere SOLO i suoi 2 figli diretti, NON il nipote Cs1.
        let results_p = claim_subagent_fanin_results(&pool, p_run).await;
        assert_eq!(
            results_p.len(),
            2,
            "il fetch di P deve iniettare SOLO Cp1,Cp2 (dispatcher=P), non il nipote Cs1"
        );
        assert!(
            results_p
                .iter()
                .all(|r| r["status"] == serde_json::json!("completed")),
            "i figli diretti di P sono entrambi completed"
        );

        // Fetch di Cp1: deve vedere SOLO il suo figlio diretto Cs1.
        let results_cp1 = claim_subagent_fanin_results(&pool, cp1_run).await;
        assert_eq!(
            results_cp1.len(),
            1,
            "il fetch di Cp1 deve iniettare SOLO Cs1 (dispatcher=Cp1)"
        );
        assert_eq!(results_cp1[0]["status"], serde_json::json!("running"));
    }

    /// ALTA-2 (discriminazione d'ondata): `dispatcher_run_id` e' COSTANTE tra le
    /// ondate (lo stesso run che si ri-sospende). Senza il consumo, la 2a fetch
    /// dello stesso dispatcher rifetcherebbe anche i figli della 1a ondata (doppia
    /// iniezione nel modello). Con `fanin_consumed_at` (mig project 0011) la 1a
    /// fetch li marca e la 2a NON li rivede; solo i figli NUOVI vengono iniettati.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn fetch_fanin_consuma_e_non_re_inietta_seconda_ondata(pool: sqlx::PgPool) {
        let session = Uuid::new_v4();
        let dispatcher = Uuid::new_v4();
        let insert = |status: &'static str| {
            let pool = pool.clone();
            async move {
                insert_sub_run(&pool, session, dispatcher, status).await;
            }
        };
        // 1a ondata: 2 figli diretti terminali.
        insert("completed").await;
        insert("completed").await;

        // 1a fetch: vede e CONSUMA i 2 figli.
        let wave1 = claim_subagent_fanin_results(&pool, dispatcher).await;
        assert_eq!(wave1.len(), 2, "la 1a ondata inietta i 2 figli");

        // 2a fetch SENZA nuovi figli: 0 (gia' consumati) -> nessuna ri-iniezione.
        let repeat = claim_subagent_fanin_results(&pool, dispatcher).await;
        assert_eq!(repeat.len(), 0, "figli gia' consumati: mai ri-iniettati");

        // 2a ondata: un NUOVO figlio dello stesso dispatcher.
        insert("failed").await;
        let wave2 = claim_subagent_fanin_results(&pool, dispatcher).await;
        assert_eq!(wave2.len(), 1, "la 2a ondata inietta SOLO il figlio nuovo");
        assert_eq!(wave2[0]["status"], serde_json::json!("failed"));
    }
}

/// Test del mapping esito nativo -> `AgentRunResult`. Vivevano in un modulo
/// chiamato `tests_select_engine` insieme ai test del selettore di motore: il
/// selettore non c'e' piu' (un motore solo), questi restano.
#[cfg(test)]
mod tests_native_mapping {
    use super::*;






    // `run_via_native` (FASE 3) richiede un `AppState` reale (ToolRunnerDeps +
    // gateway), non costruibile in unit test: il suo corpo e' un assemblaggio di
    // clone da AppState + delega al PUNTO UNICO `native_engine::run_native`. La
    // logica testabile (costruzione initial_state dal prompt, mapping esito) e'
    // coperta dai test di `crate::native_engine`; il grafo end-to-end con gateway
    // scriptato e' coperto da `nexus_agent_graph::graph` (stessi tipi e builder).
    // I due `select_engine_*` sotto coprono il confine di routing: default
    // difensivo 'python' a tabella vuota e scoping per-sessione (override sul
    // default globale, che in produzione e' '*'=rust, il primario instradato).

    // ── DEBITO 3: mapping NativeRunOutcome -> AgentRunResult (finalize unico) ─────

    use nexus_agent_graph::StopReason;

    /// Preambolo comune dei test di mapping: un run reale (sessione + riga
    /// `agent_runs` con i suoi NOT NULL) sullo schema del DB-progetto. Le tabelle
    /// non si creano piu' qui: le porta il migrator del set `db/migrations/project`.
    async fn setup_mapping_run(pool: &sqlx::PgPool) -> Uuid {
        crate::test_support::seed_agent_run(pool).await
    }

    // Chiavi/valori fixture ricorrenti degli outcome dichiarati.
    const K_BLOCKER: &str = "blocker";
    const RESUME_EXECUTOR: &str = "executor";

    fn outcome_base() -> crate::native_engine::NativeRunOutcome {
        crate::native_engine::NativeRunOutcome {
            completed: true,
            awaiting_subagents: false,
            final_answer: Some("fatto".to_string()),
            stop_reason: Some(StopReason::EndTurn),
            provider_used: Some("anthropic".to_string()),
            model_used: Some("claude-x".to_string()),
            resume_at: None,
            iterations: 2,
            prompt_tokens: 100,
            completion_tokens: 40,
            total_tokens: 140,
            total_cost: 0.0,
            user_intent: Some("code".to_string()),
            reasoning: None,
            messages_json: Some(r#"[{"role":"user","content":"ciao"}]"#.to_string()),
            declared_outcome: None,
            review_verdict: None,
            advisory_verdict: None,
            debate_position: None,
            error_class: None,
            forced_close_unverified: false,
            final_gate_passed: None,
            final_gate_unverified: None,
            final_gate_failed_pending: false,
            review_panel_rejected: false,
            review_panel_last: None,
            pending_actions: Vec::new(),
        }
    }

    /// Todo del piano del run. Semina prima il piano (`nexus_agent_todos.run_id`
    /// e' vincolato da una FK verso `nexus_agent_plans(run_id)`) e ne eredita il
    /// `project_id`, NOT NULL nello schema reale.
    ///
    /// I test che NON chiamano questa fn lavorano su una tabella VUOTA - lo
    /// scenario reale "run senza piano" - e non piu' su una tabella ASSENTE, che
    /// in produzione non capita mai: la detection hollow resta invariata in
    /// entrambi i casi, ma ora il ramo esercitato e' quello che esiste davvero.
    async fn insert_todo(pool: &sqlx::PgPool, run: Uuid, seq: i64, status: &str, content: &str) {
        crate::test_support::seed_plan(pool, run, Uuid::new_v4()).await;
        sqlx::query(
            "INSERT INTO nexus_agent_todos (run_id, project_id, seq, status, content) \
             SELECT $1, p.project_id, $2, $3, $4 \
             FROM nexus_agent_plans p WHERE p.run_id = $1",
        )
        .bind(run)
        .bind(seq)
        .bind(status)
        .bind(content)
        .execute(pool)
        .await
        .expect("insert todo");
    }

    /// Il run DISPATCHER della todo-isolation non e' hollow: 0 step e risposta
    /// vuota sono la sua forma NORMALE (delega ai sub-run), non un fallimento.
    /// Riproduce l'incidente 79d2d6eb (7/7 todo completed mostrati "falliti").
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn dispatcher_todo_isolation_non_e_hollow(pool: sqlx::PgPool) {
        let run = setup_mapping_run(&pool).await;
        insert_todo(&pool, run, 1, "completed", "Scrivi index.html").await;
        insert_todo(&pool, run, 2, "completed", "Scrivi script.js").await;
        insert_todo(&pool, run, 3, "skipped", "Passo opzionale").await;

        // Dispatcher: NESSUNO step sul proprio run_id, NESSUNA risposta propria.
        let mut outcome = outcome_base();
        outcome.final_answer = None;

        let r = native_outcome_to_run_result(&pool, run, outcome).await;
        assert!(
            !r.hollow_completion,
            "un piano concluso con lavoro non e' hollow: il declassamento a \
             failed_diagnosed marcherebbe 'fallito' un run che ha fatto tutto"
        );
        assert!(r.hollow_completion_kind.is_empty());
        // Recap dai dati: il run non resta muto in chat.
        let ans = r.final_answer.expect("recap del dispatcher");
        assert!(ans.contains("Piano completato"), "recap inatteso: {ans}");
        assert!(ans.contains("Scrivi index.html"), "recap senza todo: {ans}");
    }

    /// Contro-prova: un dispatch PARZIALMENTE FALLITO (todo `blocked`) resta
    /// hollow. Il gate non deve inghiottire i fallimenti veri.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn dispatcher_con_todo_blocked_resta_hollow(pool: sqlx::PgPool) {
        let run = setup_mapping_run(&pool).await;
        insert_todo(&pool, run, 1, "completed", "Fatto").await;
        insert_todo(&pool, run, 2, "blocked", "Rimasto bloccato").await;

        let mut outcome = outcome_base();
        outcome.final_answer = None;

        let r = native_outcome_to_run_result(&pool, run, outcome).await;
        assert!(
            r.hollow_completion,
            "piano fermo su un todo blocked: il fallimento deve restare visibile"
        );
        assert!(r.final_answer.is_none(), "nessun recap su piano non concluso");
    }

    /// Regressione gap 0-step (incidente b07c7e78): un run SENZA piano che
    /// chiude muto resta hollow come prima del gate.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn run_senza_piano_resta_hollow(pool: sqlx::PgPool) {
        let run = setup_mapping_run(&pool).await;

        let mut outcome = outcome_base();
        outcome.final_answer = None;

        let r = native_outcome_to_run_result(&pool, run, outcome).await;
        assert!(r.hollow_completion, "run senza piano: detection invariata");
        assert!(r.hollow_completion_kind.contains("EMPTY_ANSWER"));
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn native_mapping_completed_legge_step_e_usage(pool: sqlx::PgPool) {
        let run = setup_mapping_run(&pool).await;
        // Step gia' persistiti dal grafo (step_index = iteration*1000+idx).
        for (si, name, st) in [
            (1000, "read_file", "completed"),
            (2000, "write_file", "failed"),
        ] {
            // `tool_input` e' NOT NULL SENZA default nello schema reale: la vecchia
            // fixture gli aveva inventato un `DEFAULT '{}'` che permetteva di
            // ometterlo, cosa che in produzione il DB rifiuta.
            sqlx::query(
                "INSERT INTO agent_steps (run_id, step_index, tool_name, tool_input, status) \
                 VALUES ($1,$2,$3,'{}'::jsonb,$4)",
            )
            .bind(run)
            .bind(si)
            .bind(name)
            .bind(st)
            .execute(&pool)
            .await
            .expect("insert step");
        }

        let r = native_outcome_to_run_result(&pool, run, outcome_base()).await;
        assert_eq!(r.status, AgentRunStatus::Completed);
        assert_eq!(r.final_answer.as_deref(), Some("fatto"));
        assert_eq!(r.provider, "anthropic");
        assert_eq!(r.model, "claude-x");
        assert_eq!(r.iteration_count, 2);
        assert_eq!(r.prompt_tokens, 100);
        assert_eq!(r.total_tokens, 140);
        // Intent del turno -> nexus_task_type (parita' col path Python).
        assert_eq!(r.nexus_task_type.as_deref(), Some("code"));
        assert_eq!(r.stop_reason.as_deref(), Some("end_turn"));
        // Step ricostruiti da DB in ordine di step_index, con status mappato.
        assert_eq!(r.steps.len(), 2);
        assert_eq!(r.steps[0].tool_name, "read_file");
        assert_eq!(r.steps[0].status, AgentStepStatus::Completed);
        assert_eq!(r.steps[1].tool_name, "write_file");
        assert_eq!(r.steps[1].status, AgentStepStatus::Failed);
        // La conversazione finale del grafo e' propagata per agent_runs.messages_json.
        assert_eq!(
            r.messages_json.as_deref(),
            Some(r#"[{"role":"user","content":"ciao"}]"#)
        );
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn native_mapping_declared_outcome_stati_canonici(pool: sqlx::PgPool) {
        // ADR 0034: l'esito DICHIARATO via task_complete e' un segnale MACCHINA
        // che decide lo status canonico (mai la prosa, regola M).
        let run = setup_mapping_run(&pool).await;

        // blocked -> BlockedNeedsInput; senza testo, il summary fa da risposta.
        let mut o = outcome_base();
        o.final_answer = None;
        o.declared_outcome = Some(serde_json::json!({
            "outcome": "blocked",
            "summary": "Serve la API key.",
            K_BLOCKER: "credential"
        }));
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(r.status, AgentRunStatus::BlockedNeedsInput);
        assert_eq!(r.final_answer.as_deref(), Some("Serve la API key."));

        // partial -> FailedDiagnosed (dichiarazione onesta di incompletezza,
        // mai "completed" su un lavoro dichiarato parziale).
        let mut o = outcome_base();
        o.declared_outcome = Some(serde_json::json!({"outcome": "partial", "summary": "meta'"}));
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(r.status, AgentRunStatus::FailedDiagnosed);

        // refusal=true -> BlockedNeedsInput anche con outcome=done dichiarato.
        let mut o = outcome_base();
        o.declared_outcome = Some(serde_json::json!({
            "outcome": "done", "summary": "no", "refusal": true
        }));
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(r.status, AgentRunStatus::BlockedNeedsInput);

        // Il declared ha precedenza sul forced_close (dichiarazione onesta
        // post-abort piu' specifica del segnale generico di chiusura).
        let mut o = outcome_base();
        o.stop_reason = Some(StopReason::LoopAbort);
        o.declared_outcome = Some(serde_json::json!({
            "outcome": "blocked", "summary": "fermo", K_BLOCKER: "service"
        }));
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(r.status, AgentRunStatus::BlockedNeedsInput);

        // done senza refusal: Completed (poi final_gate/hollow a valle).
        let mut o = outcome_base();
        o.declared_outcome = Some(serde_json::json!({"outcome": "done", "summary": "fatto"}));
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(r.status, AgentRunStatus::Completed);
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn native_mapping_final_gate_non_superato(pool: sqlx::PgPool) {
        // Verifica oggettiva pre-chiusura NON superata (final_gate al cap/forced):
        // il verdetto strutturato final_gate_passed=false (regola M) prevale su una
        // dichiarazione "done" ottimista e annota il resoconto (run e91d4892).
        let run = setup_mapping_run(&pool).await;

        // done dichiarato + gate NON passato -> FailedDiagnosed, mai Completed.
        let mut o = outcome_base();
        o.final_answer = Some("Task completato con successo.".to_string());
        o.declared_outcome = Some(serde_json::json!({"outcome": "done", "summary": "fatto"}));
        o.final_gate_passed = Some(false);
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(r.status, AgentRunStatus::FailedDiagnosed);
        let ans = r.final_answer.expect("resoconto presente");
        // Il verdetto oggettivo GUIDA (regola M): la prosa ottimista del modello e'
        // retrocessa a resoconto non confermato, sotto l'annotazione del gate.
        assert!(
            ans.starts_with("**Verifica automatica non superata**"),
            "il verdetto del gate deve guidare il resoconto: {ans}"
        );
        assert!(
            ans.contains("Task completato con successo."),
            "la prosa del modello resta presente (subordinata): {ans}"
        );
        assert!(ans.contains("Resoconto dell'agente"));
        // Provenienza: il resoconto dice QUALE provider/modello l'ha generato.
        assert!(
            ans.contains("anthropic/claude-x"),
            "il resoconto deve riportare provider/modello: {ans}"
        );

        // gate PASSATO -> Completed, nessuna annotazione.
        let mut o = outcome_base();
        o.final_gate_passed = Some(true);
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(r.status, AgentRunStatus::Completed);
        assert_eq!(r.final_answer.as_deref(), Some("fatto"));

        // gate NON eseguito (None) -> comportamento invariato.
        let mut o = outcome_base();
        o.final_gate_passed = None;
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(r.status, AgentRunStatus::Completed);
        assert_eq!(r.final_answer.as_deref(), Some("fatto"));
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn native_mapping_final_gate_bocciato_mai_riverificato(pool: sqlx::PgPool) {
        // REGRESSIONE run a5db0985: final_gate 1/2 FALLITO -> correzione in volo
        // (che introduce una regressione) -> provider esauriti bruciano i turni
        // fino al cap iterazioni -> chiusura SENZA gate 2/2. L'ultimo verdetto
        // oggettivo e' una bocciatura: mai 'completed' (esito bugiardo), sempre
        // FailedDiagnosed dal segnale strutturato (regola M).
        let run = setup_mapping_run(&pool).await;

        // Chiusura muta (la nota cooldown viene composta a valle): lo status
        // deve comunque essere onesto.
        let mut o = outcome_base();
        o.final_answer = None;
        o.final_gate_passed = None; // gate 2/2 mai eseguito
        o.final_gate_failed_pending = true;
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(r.status, AgentRunStatus::FailedDiagnosed);

        // Con resoconto presente: status onesto + annotazione della bocciatura.
        let mut o = outcome_base();
        o.final_answer = Some("Correzioni applicate.".to_string());
        o.final_gate_failed_pending = true;
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(r.status, AgentRunStatus::FailedDiagnosed);
        let ans = r.final_answer.expect("resoconto presente");
        assert!(
            ans.starts_with("**Verifica automatica fallita e non ripetuta**"),
            "il verdetto del gate deve guidare il resoconto: {ans}"
        );
        assert!(
            ans.contains("Correzioni applicate."),
            "la prosa del modello resta presente (subordinata): {ans}"
        );

        // Contro-prova: senza bocciatura pendente il comportamento e' invariato.
        let mut o = outcome_base();
        o.final_gate_failed_pending = false;
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(r.status, AgentRunStatus::Completed);
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn native_mapping_final_gate_bocciato_precedenze_di_stato(pool: sqlx::PgPool) {
        // Ordine della catena col segnale `final_gate_failed_pending` attivo:
        // gli stati piu' specifici mantengono la precedenza sulla bocciatura.
        let run = setup_mapping_run(&pool).await;

        // La dichiarazione onesta `blocked` resta piu' specifica del segnale
        // di bocciatura pendente (stessa precedenza del forced_close).
        let mut o = outcome_base();
        o.final_gate_failed_pending = true;
        o.declared_outcome = Some(serde_json::json!({
            "outcome": "blocked", "summary": "fermo", K_BLOCKER: "credential"
        }));
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(r.status, AgentRunStatus::BlockedNeedsInput);

        // PRECEDENZA vs StopReason::Error: un errore infrastrutturale sopraggiunto
        // DOPO la bocciatura del gate deve dare Failed (percorso retry/diagnosi),
        // mai FailedDiagnosed. Blinda l'ordine della catena col campo nuovo attivo.
        let mut o = outcome_base();
        o.final_gate_failed_pending = true;
        o.stop_reason = Some(StopReason::Error);
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(r.status, AgentRunStatus::Failed);

        // PRECEDENZA vs AwaitingConfirmation: un run sospeso per HITL con una
        // bocciatura pendente resta AwaitingConfirmation (riprendera'), mai
        // declassato a FailedDiagnosed.
        let mut o = outcome_base();
        o.final_gate_failed_pending = true;
        o.completed = false;
        o.resume_at = Some(RESUME_EXECUTOR.to_string());
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(r.status, AgentRunStatus::AwaitingConfirmation);
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn native_mapping_hitl_e_awaiting_confirmation(pool: sqlx::PgPool) {
        let run = setup_mapping_run(&pool).await;
        let mut o = outcome_base();
        o.completed = false;
        o.resume_at = Some(RESUME_EXECUTOR.to_string());
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(
            r.status,
            AgentRunStatus::AwaitingConfirmation,
            "interrupt HITL -> awaiting_confirmation"
        );
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn native_mapping_forced_close_failed_diagnosed(pool: sqlx::PgPool) {
        let run = setup_mapping_run(&pool).await;
        let mut o = outcome_base();
        o.stop_reason = Some(StopReason::LoopAbort);
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(
            r.status,
            AgentRunStatus::FailedDiagnosed,
            "abort anti-loop -> failed_diagnosed (esito certo)"
        );
        assert_eq!(r.stop_reason.as_deref(), Some("loop_abort"));
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn native_mapping_risposta_vuota_zero_step_hollow_e_recap(pool: sqlx::PgPool) {
        // REGRESSIONE incidente run b07c7e78 (gap 2): run nativo TERMINATO con
        // risposta vuota e ZERO step non deve restare un 'completed' MUTO. Il
        // mapping calcola hollow (prima: false hardcoded), il canonico declassa
        // a FailedDiagnosed e il compositore del messaggio produce comunque un
        // testo per la chat (placeholder/recap deterministico).
        let run = setup_mapping_run(&pool).await;
        let mut o = outcome_base();
        o.final_answer = Some("   ".to_string()); // whitespace-only = vuota
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert!(r.final_answer.is_none());
        assert!(r.hollow_completion, "risposta vuota + 0 step -> hollow");
        assert_eq!(r.hollow_completion_kind, "EMPTY_ANSWER+NO_TOOLS");
        assert_eq!(
            canonical_run_status(&r),
            AgentRunStatus::FailedDiagnosed,
            "mai 'completed' su un run muto senza lavoro"
        );
        assert!(
            compose_turn_answer(&r).is_some(),
            "la chat non deve restare muta: placeholder/recap garantito"
        );
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn native_mapping_hollow_non_scatta_su_risposta_o_step_presenti(pool: sqlx::PgPool) {
        // Contro-prova del calcolo hollow nativo: risposta presente (o run
        // HITL non concluso) -> hollow false, status invariato.
        let run = setup_mapping_run(&pool).await;
        // Risposta presente -> non hollow.
        let r = native_outcome_to_run_result(&pool, run, outcome_base()).await;
        assert!(!r.hollow_completion);
        assert_eq!(r.hollow_completion_kind, "");
        assert_eq!(canonical_run_status(&r), AgentRunStatus::Completed);
        // Interrupt HITL (final_answer assente ma run NON concluso) -> non hollow.
        let mut o = outcome_base();
        o.completed = false;
        o.resume_at = Some(RESUME_EXECUTOR.to_string());
        o.final_answer = None;
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert!(!r.hollow_completion, "un interrupt HITL non e' un run muto");
        // Errore del grafo -> non hollow (percorso Failed dedicato).
        let mut o = outcome_base();
        o.stop_reason = Some(StopReason::Error);
        o.final_answer = None;
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert!(!r.hollow_completion);
        assert_eq!(r.status, AgentRunStatus::Failed);
    }

    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn native_mapping_forced_close_risposta_vuota_mai_completed(pool: sqlx::PgPool) {
        // REGRESSIONE incidente run b07c7e78 (gap 1, lato mapping): risposta
        // VUOTA al turno forzato -> l'executor marca forced_close_unverified;
        // il mapping non deve MAI produrre 'completed' e la chat non resta muta.
        let run = setup_mapping_run(&pool).await;
        let mut o = outcome_base();
        o.final_answer = None;
        o.forced_close_unverified = true; // segnale autoritativo dell'executor
        let r = native_outcome_to_run_result(&pool, run, o).await;
        assert_eq!(
            r.status,
            AgentRunStatus::FailedDiagnosed,
            "esito non verificato: mai 'completed' con risposta vuota"
        );
        assert!(
            compose_turn_answer(&r).is_some(),
            "recap/placeholder garantito anche a 0 step"
        );
    }

    #[test]
    fn native_failure_result_e_failed_onesto_senza_retry_class() {
        // Regola H: un Err del motore nativo -> FAILED diagnosticato, NIENTE
        // error_class (cosi' il loop di retry non lo ritenta su altri provider) e
        // stop_reason "error". Nessun fallback al brain mascherato.
        let run = Uuid::new_v4();
        let r = native_engine_failure_result(
            run,
            "anthropic",
            "claude-x",
            "Il motore nativo non e' riuscito a completare il run.".to_string(),
        );
        assert_eq!(r.status, AgentRunStatus::Failed);
        assert!(
            r.error_class.is_none(),
            "niente error_class: il fallimento del grafo non e' un errore provider ritentabile"
        );
        assert_eq!(r.stop_reason.as_deref(), Some("error"));
        assert_eq!(r.provider, "anthropic");
        assert_eq!(r.model, "claude-x");
        assert!(r.steps.is_empty());
        assert!(r
            .final_answer
            .as_deref()
            .unwrap_or_default()
            .contains("non e' riuscito"));
    }
}

#[cfg(test)]
mod tests_enforce_cancellation {
    use super::*;

    /// Run nello stato voluto, eventualmente con lo Stop utente registrato.
    /// Sessione e NOT NULL reali vengono dal seeder condiviso: la tabella la
    /// porta il migrator del set project.
    async fn insert(pool: &sqlx::PgPool, status: &str, cancelled: bool) -> Uuid {
        let project = Uuid::new_v4();
        let session = crate::test_support::seed_chat_session(pool, project).await;
        let id = crate::test_support::insert_agent_run(pool, session, project, status).await;
        if cancelled {
            sqlx::query("UPDATE agent_runs SET cancellation_requested = NOW() WHERE id = $1")
                .bind(id)
                .execute(pool)
                .await
                .expect("registra lo Stop utente");
        }
        id
    }

    async fn status_of(pool: &sqlx::PgPool, id: Uuid) -> String {
        sqlx::query_scalar::<_, String>("SELECT status FROM agent_runs WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("status")
    }

    /// Il caso di campo (run 53dac032, 18/07): Stop premuto durante l'ultima
    /// chiamata LLM, il modello conclude -> finalizzato 'completed' con
    /// `cancellation_requested` valorizzato. La riconciliazione lo porta a
    /// 'cancelled', cosi' lo Stop utente si riflette nello stato finale.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn completed_con_cancellazione_diventa_cancelled(pool: sqlx::PgPool) {
        let id = insert(&pool, "completed", true).await;
        enforce_user_cancellation_status(&pool, id).await;
        assert_eq!(status_of(&pool, id).await, "cancelled");
    }

    /// Senza cancellazione un 'completed' resta 'completed': nessun falso Stop su
    /// un run terminato normalmente.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn completed_senza_cancellazione_resta_completed(pool: sqlx::PgPool) {
        let id = insert(&pool, "completed", false).await;
        enforce_user_cancellation_status(&pool, id).await;
        assert_eq!(status_of(&pool, id).await, "completed");
    }

    /// Un 'failed' con cancellazione NON viene mascherato da 'cancelled': il
    /// fallimento tecnico resta informativo (la riconciliazione tocca solo
    /// 'completed').
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn failed_con_cancellazione_resta_failed(pool: sqlx::PgPool) {
        let id = insert(&pool, "failed", true).await;
        enforce_user_cancellation_status(&pool, id).await;
        assert_eq!(status_of(&pool, id).await, "failed");
    }
}

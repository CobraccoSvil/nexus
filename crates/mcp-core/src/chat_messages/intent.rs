use super::*;

pub(crate) fn parse_automation_mode(value: Option<&str>) -> AutomationMode {
    match value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("confirm")
        .to_lowercase()
        .as_str()
    {
        "study" | "studio" => AutomationMode::Study,
        "automatic" | "automatico" | "auto" => AutomationMode::Automatic,
        _ => AutomationMode::Confirm,
    }
}
#[allow(dead_code)]
pub(crate) fn parse_provider_hierarchy(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.starts_with('[') {
        if let Ok(items) = serde_json::from_str::<Vec<String>>(trimmed) {
            return items
                .into_iter()
                .map(|item| item.trim().to_lowercase())
                .filter(|item| !item.is_empty())
                .collect();
        }
    }
    trimmed
        .split(',')
        .map(|item| item.trim().to_lowercase())
        .filter(|item| !item.is_empty())
        .collect()
}
/// Guard-rail anti-mismatch: verifica che model appartenga a provider.
///
/// La fonte di verita della coppia provider/model resta
/// nexus_provider_default_model + nexus_routing_matrix (regola G CLAUDE.md):
/// questi prefix sono solo detection difensiva per impedire una coppia
/// impossibile (es. anthropic + gemini-2.5-pro) che fallirebbe con 404. ADR 0016.
pub(crate) fn model_belongs_to_provider(provider: &str, model: &str) -> bool {
    let p = provider.trim().to_lowercase();
    let m = model.trim().to_lowercase();
    match p.as_str() {
        "anthropic" => m.starts_with("claude"),
        "google" => m.starts_with("gemini"),
        "openai" => {
            m.starts_with("gpt")
                || m.starts_with("o1")
                || m.starts_with("o3")
                || m.starts_with("o4")
                || m.strip_prefix('o')
                    .and_then(|rest| rest.chars().next())
                    .map(|c| c.is_ascii_digit())
                    .unwrap_or(false)
        }
        "deepseek" => m.starts_with("deepseek"),
        "mistral" => {
            m.starts_with("mistral")
                || m.starts_with("codestral")
                || m.starts_with("ministral")
                || m.starts_with("pixtral")
        }
        _ => true,
    }
}

// default_model_for_provider e load_agent_provider_defaults rimossi dopo refactor 0101.
// Erano duplicati di logica in orchestrator.rs e marcati #[allow(dead_code)].
// Per leggere il default per provider usare:
//   crate::orchestrator::default_model_for_provider(matrix, provider)
// con matrix ottenuta da state.orchestrator.routing_matrix.current().
pub(crate) fn humanize_ai_error(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("429")
        || lower.contains("529")
        || lower.contains("rate_limit")
        || lower.contains("rate limit")
        || lower.contains("overloaded")
        || lower.contains("quota")
        || lower.contains("resource_exhausted")
        || lower.contains("service unavailable")
        || lower.contains("503")
    {
        return "Il provider AI è temporaneamente sovraccarico (overloaded). Sto ritentando automaticamente con backoff; riprova tra poco se persiste.".to_string();
    }
    if lower.contains("timeout") {
        return "La richiesta AI e' scaduta per timeout. Riprova con un prompt piu' corto o tra qualche secondo.".to_string();
    }

    let first_line = raw
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("Richiesta AI non completata");
    let trimmed = first_line.trim();
    if trimmed.chars().count() > 220 {
        format!("{}...", trimmed.chars().take(220).collect::<String>())
    } else {
        trimmed.to_string()
    }
}
/// Azzera la preferenza di provider della sessione e segna l'evento privacy.
/// Va chiamato ogni volta che il gateway ha re-instradato su un provider locale per privacy.
/// Al messaggio successivo il sistema userà il routing automatico invece della preferenza precedente.
pub(crate) async fn clear_session_preferred_provider_after_privacy(
    db: &sqlx::PgPool,
    session_id: uuid::Uuid,
) {
    let _ = sqlx::query(
        "UPDATE chat_sessions \
         SET preferred_provider = NULL, preferred_model = NULL, privacy_rerouted_at = NOW() \
         WHERE id = $1",
    )
    .bind(session_id)
    .execute(db)
    .await;
}
/// Rileva se il messaggio è un comando di reset al routing automatico.
pub(crate) fn detect_model_reset(content: &str) -> bool {
    let lower = content.trim().to_lowercase();
    if lower.chars().count() > 80 {
        return false;
    }
    lower == "routing automatico"
        || lower == "modello automatico"
        || lower == "reimposta modello"
        || lower == "reset modello"
        || lower == "reset routing"
        || lower.contains("routing auto")
        || lower.contains("modello auto")
        || lower.contains("torna al default")
        || lower.contains("torna al routing")
        || lower.contains("modello di default")
        || lower.contains("provider di default")
}
/// Rileva se il messaggio dell'utente è un comando esplicito di cambio provider/modello.
///
/// Restituisce `Some((provider, modello_specifico))` se rilevato, `None` altrimenti.
/// Considera solo messaggi brevi (< 100 caratteri) per evitare falsi positivi.
///
/// I PROVIDER ID (mistral, anthropic, openai, google, deepseek) sono identificatori
/// stabili Nexus, quindi keyword-based. I MODELLI invece sono letti dal DB
/// (`ai_price_catalog`) — cosi' aggiungere claude-opus-5 al DB lo rende
/// automaticamente riconoscibile in chat senza modifiche al codice.
pub(crate) async fn detect_model_switch(
    db: &sqlx::PgPool,
    content: &str,
) -> Option<(String, Option<String>)> {
    let lower = content.trim().to_lowercase();
    // Ignora messaggi lunghi: quasi certamente non è un puro comando di switch
    if lower.chars().count() > 100 {
        return None;
    }

    // Identifica il provider richiesto in base a keyword nel messaggio.
    // I 5 provider id sono identificatori stabili (slug Nexus) — non cambiano.
    let provider: &'static str =
        if lower.contains("mistral") || lower.contains("codestral") || lower.contains("mixtral") {
            "mistral"
        } else if lower.contains("claude")
            || lower.contains("anthropic")
            || lower.contains("sonnet")
            || lower.contains("opus")
            || lower.contains("haiku")
        {
            "anthropic"
        } else if lower.contains("openai")
            || lower.contains("gpt")
            || lower.contains("chatgpt")
            || lower.contains("o1")
            || lower.contains("o3")
        {
            "openai"
        } else if lower.contains("gemini") || lower.contains("google") || lower.contains("bard") {
            "google"
        } else if lower.contains("deepseek") {
            "deepseek"
        } else {
            return None;
        };

    // Verifica che sia presente un verbo d'azione (switch, usa, cambia, ecc.)
    let has_action = lower.starts_with("usa ")
        || lower.starts_with("use ")
        || lower == "usa mistral"
        || lower == "usa claude"
        || lower == "usa openai"
        || lower == "usa gemini"
        || lower == "usa deepseek"
        || lower.contains("cambia")
        || lower.contains("passa a")
        || lower.contains("passa su")
        || lower.contains("switch to")
        || lower.contains("switch su")
        || lower.contains("rispondi con")
        || lower.contains("utilizza ")
        || lower.contains("voglio usare")
        || lower.contains("voglio ")
        || lower.contains("usa il modello")
        || lower.contains("use the model")
        || lower.contains("imposta ")
        || lower.contains("setta ");

    if !has_action {
        return None;
    }

    // Modello specifico: query DB per i modelli enabled del provider scelto.
    // Match: il messaggio contiene il model_id intero, o l'ultima componente
    // (es. "sonnet" matcha "claude-sonnet-4-6") quando il modello ha trattini.
    // Ordinato per is_featured DESC + costo ASC: se ci sono ambiguita' (es.
    // "claude" matcha sia haiku che sonnet), vince il piu' "in evidenza".
    let candidates: Vec<String> = sqlx::query_scalar(
        "SELECT model FROM ai_price_catalog \
         WHERE provider = $1 AND is_enabled = TRUE \
         ORDER BY is_featured DESC, input_cost_per_million_tokens ASC",
    )
    .bind(provider)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    let specific_model: Option<String> = candidates.into_iter().find(|m| {
        let m_lower = m.to_lowercase();
        // Match diretto: lower contiene l'intero model_id
        if lower.contains(&m_lower) {
            return true;
        }
        // Match per "famiglia": ogni componente split-trattino del modello
        // (es. "claude-opus-4-6" -> ["claude", "opus", "4", "6"]) — se nel
        // messaggio c'e' una componente "opus" (>=4 char per evitare match
        // di numeri o suffissi tipo "4"), considera match.
        m_lower
            .split('-')
            .any(|part| part.len() >= 4 && lower.contains(part))
    });

    Some((provider.to_string(), specific_model))
}
/// Tipo di query meta auto-referenziale rilevato. Determina quale
/// messaggio precedente significativo va citato nell'hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelfRefIntent {
    /// Riferito al messaggio piu' recente (precedente alla query stessa).
    Last,
    /// Riferito al primo messaggio significativo della sessione.
    First,
}
/// Rileva se il messaggio utente e' una domanda meta auto-referenziale
/// e ritorna il tipo (Last/First). Esempi:
/// - "qual era l'ultima richiesta?", "ripeti l'ultimo", "e l'ultima" → Last
/// - "qual era la prima richiesta?", "e la prima" → First
///
/// In questi casi il LLM deve sapere che il messaggio corrente NON conta
/// come "ultima/prima richiesta" e va riferito al precedente messaggio
/// significativo nella cronologia.
pub(crate) fn detect_self_referential_intent(message: &str) -> Option<SelfRefIntent> {
    let m = message.trim().to_lowercase();
    if m.is_empty() {
        return None;
    }

    // Token "richiesta-target": il messaggio sembra parlare DI un'altra
    // richiesta (vs. essere una richiesta concreta). Sia "richiesta" che
    // "domanda" che "messaggio" qualificano.
    let target_tokens = [
        "richiesta",
        "domanda",
        "messaggio",
        "cosa ho chiesto",
        "cosa ti ho chiesto",
        "cosa avevo chiesto",
        "cosa ti avevo chiesto",
    ];
    let has_target = target_tokens.iter().any(|t| m.contains(t));

    // Token "precedente": frasi che esprimono precedenza temporale.
    let prev_tokens = [
        "ripeti",
        "precedente",
        "prima di",
        "appena chiesto",
        "appena fatto",
    ];
    let has_prev = prev_tokens.iter().any(|t| m.contains(t));

    // Match per "First": qualunque combinazione di "prima" o "iniziale"
    // con target o riferimento alla chat.
    let first_phrases = [
        "prima richiesta",
        "prima domanda",
        "primo messaggio",
        "prima cosa",
        "all'inizio",
        "all'avvio",
        "iniziale",
        "inizio della chat",
        "inizio conversazione",
        "inizio della conversazione",
    ];
    if first_phrases.iter().any(|p| m.contains(p)) {
        return Some(SelfRefIntent::First);
    }
    // Forme abbreviate tipo "e la prima", "qual era la prima"
    if (m.contains("la prima") || m.starts_with("prima ") || m == "la prima" || m == "prima")
        && (has_target || m.len() < 30)
    {
        return Some(SelfRefIntent::First);
    }

    // Match per "Last": pattern espliciti
    let last_phrases = [
        "ultima richiesta",
        "ultima domanda",
        "ultimo messaggio",
        "ultima cosa",
        "qual era la richiesta",
        "qual era la domanda",
        "ripeti l'ultim",
        "ripeti ultim",
        "ripeti la richiesta",
        "ripeti la domanda",
    ];
    if last_phrases.iter().any(|p| m.contains(p)) {
        return Some(SelfRefIntent::Last);
    }
    // Forme abbreviate tipo "e l'ultima", "l'ultima", "qual era l'ultima"
    if m.contains("l'ultima")
        || m.contains("l ultima")
        || (m == "ultima" || m.starts_with("ultima "))
    {
        return Some(SelfRefIntent::Last);
    }
    // "richiesta/domanda/messaggio precedente"
    if has_target && has_prev {
        return Some(SelfRefIntent::Last);
    }

    None
}
/// Backward-compatible wrapper: ritorna true se il messaggio e' una qualsiasi
/// variante di query meta auto-referenziale.
pub(crate) fn detect_self_referential_query(message: &str) -> bool {
    detect_self_referential_intent(message).is_some()
}
/// Cerca un messaggio utente significativo nella sessione, scartando saluti,
/// conferme brevi e meta-domande auto-referenziali. La direzione di scansione
/// dipende dall'intent:
/// - `Last`: dal piu' recente al piu' vecchio (precedente alla query corrente)
/// - `First`: dal piu' vecchio al piu' recente (primo messaggio della chat)
pub(crate) async fn find_target_user_message(
    db: &PgPool,
    session_id: Uuid,
    intent: SelfRefIntent,
) -> Option<String> {
    let order = match intent {
        SelfRefIntent::Last => "DESC",
        SelfRefIntent::First => "ASC",
    };
    let query = format!(
        r#"
        SELECT content FROM chat_messages
        WHERE session_id = $1 AND deleted_at IS NULL AND role = 'user'
        ORDER BY created_at {order}
        LIMIT 20
        "#,
        order = order,
    );
    let rows = sqlx::query(&query)
        .bind(session_id)
        .fetch_all(db)
        .await
        .ok()?;

    let trivial_patterns: &[&str] = &["ok", "si", "sì", "no", "grazie", "ciao", "ok grazie"];
    for row in rows.iter() {
        let content: String = row.try_get("content").ok()?;
        let trimmed = content.trim();
        let lower = trimmed.to_lowercase();
        if trimmed.is_empty() || trimmed.len() < 5 {
            continue;
        }
        if trivial_patterns.iter().any(|p| lower == *p) {
            continue;
        }
        if detect_self_referential_query(trimmed) {
            continue;
        }
        let compact = trimmed
            .replace('\n', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if compact.chars().count() > 120 {
            return Some(format!(
                "{}...",
                compact.chars().take(120).collect::<String>()
            ));
        }
        return Some(compact);
    }
    None
}
/// Costruisce l'istruzione contestuale "precedente significativo" con un
/// few-shot example *reale* tratto dalla cronologia attuale. Differenzia
/// tra richiesta "Last" (precedente) e "First" (prima della chat). Se la
/// cronologia non contiene messaggi significativi (o non c'e' match),
/// ritorna `None`: il prompt non subisce iniezione spuria.
pub(crate) async fn build_self_referential_hint(
    db: &PgPool,
    session_id: Uuid,
    current_message: &str,
) -> Option<String> {
    let intent = detect_self_referential_intent(current_message)?;
    let target = find_target_user_message(db, session_id, intent).await;
    let (label_role, label_example) = match intent {
        SelfRefIntent::Last => ("precedente", "La tua precedente richiesta era"),
        SelfRefIntent::First => ("prima", "La tua prima richiesta in questa chat e' stata"),
    };
    let core_rule = format!(
        "Istruzione contestuale: l'utente sta chiedendo informazioni sulla sua \
         richiesta {label_role}. Il messaggio attuale e' la domanda meta — \
         NON considerarlo come 'ultima/prima richiesta'. Riferisciti al \
         corretto messaggio utente significativo nella cronologia (saltando \
         saluti, conferme brevi tipo 'ok'/'si'/'no'/'grazie' e altre \
         meta-domande auto-referenziali ricorsive)."
    );
    let current_short: String = current_message
        .replace('\n', " ")
        .chars()
        .take(80)
        .collect();
    match target {
        Some(example) => Some(format!(
            "\n\n{core_rule}\n\nEsempio tratto da questa conversazione: \
             il messaggio utente da citare e' \"{example}\". \
             Per una domanda come '{current_short}' la risposta corretta inizia con: \
             \"{label_example}: ...\" citando quel testo, NON il messaggio \
             attuale.",
        )),
        None => Some(format!("\n\n{core_rule}")),
    }
}
pub(crate) fn summarize_title(content: &str) -> String {
    let normalized = content.replace('\n', " ").trim().to_string();
    if normalized.is_empty() {
        return "Nuova sessione".to_string();
    }
    if normalized.chars().count() <= 64 {
        return normalized;
    }
    normalized.chars().take(61).collect::<String>() + "..."
}
/// Mappa intent canonico → descrizione human-readable in italiano per
/// il messaggio di disambiguazione mostrato all'utente. Ritorna `None` per
/// intent sconosciuti (il caller usa il nome canonico come fallback).
pub(crate) fn intent_human_description(intent: &str) -> Option<&'static str> {
    Some(match intent {
        "chat" => "rispondere con una spiegazione testuale",
        "debug" => "analizzare l'errore o il fallimento per trovare la causa radice",
        "fix" => "applicare una correzione mirata a un bug noto",
        "refactor" => "riorganizzare il codice senza cambiarne il comportamento",
        "test" => "scrivere o migliorare test (nuovi casi di test)",
        "docs" => "scrivere/aggiornare documentazione",
        "code_read" => "leggere ed esaminare file di codice esistenti",
        "architecture" => "fare design o pianificare una migrazione",
        "file_ops" => "creare/eliminare/spostare file",
        "system_admin" => "configurare servizi, utenti o deploy",
        _ => return None,
    })
}
/// Costruisce il messaggio di disambiguazione mostrato all'utente quando
/// il classifier non e' sicuro tra 2+ intent plausibili. Lista le opzioni
/// con etichetta (A/B/C) per facilitare la risposta.
pub(crate) fn build_disambiguation_message(c: &crate::orchestrator::ClassifiedIntent) -> String {
    let mut s = String::from(
        "Per dare la risposta giusta ho bisogno di un chiarimento — la tua richiesta puo' \
         essere interpretata in piu' modi. Quale di queste opzioni descrive meglio cosa vuoi?\n\n",
    );
    let labels = ["A", "B", "C"];
    for (idx, cand) in c.candidates.iter().take(3).enumerate() {
        let desc = intent_human_description(&cand.intent).unwrap_or(cand.intent.as_str());
        s.push_str(&format!(
            "**{}.** {} (intent: `{}`, confidence {:.0}%)\n",
            labels[idx],
            desc,
            cand.intent,
            cand.confidence * 100.0,
        ));
    }
    s.push_str(
        "\nRispondi indicando la lettera (es. \"A\") oppure descrivi piu' precisamente \
         cosa vuoi che faccia. Se preferisci che proceda senza chiedere, imposta la \
         modalita' di automazione su \"Automatico\".",
    );
    s
}

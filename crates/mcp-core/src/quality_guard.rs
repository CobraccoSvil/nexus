//! Quality guard: rileva risposte di bassa qualità e suggerisce reiniezione contesto.
//!
//! Classifier leggero basato su regex. Restituisce Some(reinject_msg) se la risposta
//! è considerata di bassa qualità e vale la pena riprovare con più contesto;
//! None se la risposta è accettabile come risposta finale.
//!
//! Limiti:
//! - Non usa LLM judge per evitare latenza (aggiungibile in futuro con campionamento 1/3)
//! - Le reinjection vengono conteggiate su planning_no_tool_count dal chiamante
//! - Gate: attivo solo se reinject_count < MAX_QUALITY_REINJECTIONS

pub const MAX_QUALITY_REINJECTIONS: u32 = 2;

/// Label di qualità assegnata alla risposta
#[derive(Debug, PartialEq)]
pub enum QualityLabel {
    Ok,
    Vague,
    ClarifyingQuestion,
    OffTopic,
}

/// Valuta la qualità della risposta e restituisce il messaggio di reiniezione
/// se la risposta non è accettabile, None se è ok.
///
/// `reinject_count` = numero di reinjection già effettuate in questo run.
pub async fn check_response_quality(
    response: &str,
    original_task: &str,
    reinject_count: u8,
) -> Option<String> {
    if reinject_count >= MAX_QUALITY_REINJECTIONS as u8 {
        return None;
    }

    let label = classify_response(response, original_task);

    match label {
        QualityLabel::Ok => None,
        QualityLabel::Vague => {
            tracing::info!(
                "quality_guard: risposta vaga rilevata (len={}, reinject_count={reinject_count})",
                response.len()
            );
            Some(
                "La tua risposta precedente era troppo generica o incompleta. \
                 Fornisci dettagli concreti: comandi eseguiti, output ottenuti, file modificati, \
                 o errori rilevati. Se non hai abbastanza contesto, usa i tool per ottenerlo prima \
                 di rispondere."
                    .to_string(),
            )
        }
        QualityLabel::ClarifyingQuestion => {
            tracing::info!(
                "quality_guard: risposta con domanda chiarificatrice (reinject_count={reinject_count})"
            );
            Some(
                "Non fare domande: sei un agente autonomo. \
                 Usa i tool disponibili (list_files, read_file, run_command) per ottenere \
                 il contesto che ti manca, poi agisci direttamente."
                    .to_string(),
            )
        }
        QualityLabel::OffTopic => {
            tracing::info!(
                "quality_guard: risposta off-topic rilevata (reinject_count={reinject_count})"
            );
            Some(format!(
                "La tua risposta non riguarda il task originale. \
                 Ricorda: il task è: \"{task_preview}\". \
                 Concentrati su questo obiettivo e usa i tool necessari per completarlo.",
                task_preview = original_task.chars().take(200).collect::<String>()
            ))
        }
    }
}

fn classify_response(response: &str, original_task: &str) -> QualityLabel {
    let resp_lower = response.to_lowercase();
    let resp_len = response.trim().len();

    // Risposta troppo corta (<80 char) e senza tool eseguiti → probabilmente vaga
    if resp_len < 80 && !response.contains("```") {
        // Eccetto se è una conferma di completamento ("fatto", "completato", ecc.)
        let is_completion = [
            "completato",
            "fatto",
            "risolto",
            "terminato",
            "ok",
            "pronto",
        ]
        .iter()
        .any(|w| resp_lower.contains(w));
        if !is_completion {
            return QualityLabel::Vague;
        }
    }

    // Risposta che contiene domande chiarificatrici all'utente
    let clarifying_patterns = [
        "puoi dirmi",
        "potresti dirmi",
        "hai bisogno che",
        "vuoi che io",
        "dovrei procedere",
        "come preferisci",
        "quale preferisci",
        "mi confermi",
        "puoi confermare",
        "hai un preferenza",
    ];
    if clarifying_patterns.iter().any(|p| resp_lower.contains(p)) && response.contains('?') {
        return QualityLabel::ClarifyingQuestion;
    }

    // Risposta off-topic: non menziona nessuna parola chiave dal task originale
    // (solo per task con >= 20 char, per evitare falsi positivi su task brevi)
    if original_task.len() >= 20 {
        let task_keywords: Vec<&str> = original_task
            .split_whitespace()
            .filter(|w| w.len() >= 5)
            .take(6)
            .collect();
        let mentions_any_keyword = task_keywords
            .iter()
            .any(|kw| resp_lower.contains(&kw.to_lowercase()));
        // Off-topic solo se risposta >200 char (per evitare falsi positivi su risposte brevi valide)
        if !mentions_any_keyword && resp_len > 200 {
            return QualityLabel::OffTopic;
        }
    }

    QualityLabel::Ok
}

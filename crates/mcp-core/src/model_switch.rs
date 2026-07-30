//! Cambio di provider/modello dentro la chat: punto unico (regola L) di
//! VALIDAZIONE del segnale che il classificatore dichiara.
//!
//! `intent_classifier::classify` legge l'intero turno e produce, fra gli altri
//! campi, `model_switch`: e' un segnale SEMANTICO ("l'utente sta chiedendo di
//! cambiare provider", giudicato da chi legge intento e contesto), non un
//! verdetto. Il verdetto — la coppia provider/modello che la sessione USERA'
//! davvero — lo da' questo modulo, verificando il segnale contro
//! `ai_price_catalog` (regola G: il listino e' l'unica fonte di verita' su cosa
//! esiste ed e' abilitato).
//!
//! PRIMA questa decisione viveva tutta in
//! `chat_messages::intent::detect_model_switch`: 5 liste di keyword per
//! riconoscere il provider e 18 pattern di verbo per decidere se il messaggio
//! fosse un COMANDO. Il listino non era il problema — i modelli erano gia' letti
//! dal DB — il problema era il TESTO: "voglio capire perche' gemini risponde
//! male" contiene sia "gemini" sia "voglio", quindi veniva letto come switch e il
//! turno non arrivava mai all'agente, la richiesta reale spariva senza errore.
//! Il fix sposta il giudizio "e' un comando o e' lavoro" al classificatore (che
//! vede l'intento, non sottostringhe) e lascia a questo modulo SOLO la domanda
//! "questo provider/modello esiste ed e' abilitato".

use sqlx::PgPool;

use crate::intent_classifier::ModelSwitchSignal;

/// Verdetto di uno switch: provider validato contro il listino + modello (se
/// nominato e risolvibile). Chi lo riceve puo' persistere la preferenza di
/// sessione senza ulteriori controlli — il provider ha almeno un modello
/// abilitato, il modello (se presente) e' un `model_id` reale del catalog.
pub(crate) struct ModelSwitchVerdict {
    pub provider: String,
    pub model: Option<String>,
}

/// Valida `signal` contro `ai_price_catalog`. `None` quando il segnale e'
/// assente (non era uno switch) O quando punta a un provider senza NESSUN
/// modello abilitato: un comando verso un fornitore inesistente o disattivato
/// non e' uno switch valido, e' un no-op che merita un warning, non un ack.
pub(crate) async fn resolve_switch_verdict(
    db: &PgPool,
    signal: Option<&ModelSwitchSignal>,
) -> Option<ModelSwitchVerdict> {
    let signal = signal?;
    // Ordinato per is_featured DESC + costo ASC: se il modello nominato e'
    // ambiguo (es. "claude" matcha sia haiku che sonnet), vince il piu' "in
    // evidenza" — stesso criterio del punto unico precedente.
    let candidates: Vec<String> = sqlx::query_scalar(
        "SELECT model FROM ai_price_catalog \
         WHERE provider = $1 AND is_enabled = TRUE \
         ORDER BY is_featured DESC, input_cost_per_million_tokens ASC",
    )
    .bind(&signal.provider)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    if candidates.is_empty() {
        tracing::warn!(
            provider = %signal.provider,
            "model_switch: nessun modello abilitato per il provider dichiarato dal classificatore, switch scartato"
        );
        return None;
    }
    let model = signal.model.as_deref().and_then(|wanted| {
        let wanted_lower = wanted.trim().to_lowercase();
        if wanted_lower.is_empty() {
            return None;
        }
        candidates
            .iter()
            .find(|m| model_matches(m, &wanted_lower))
            .cloned()
    });
    Some(ModelSwitchVerdict {
        provider: signal.provider.clone(),
        model,
    })
}

/// Un candidato del catalog matcha il nome nominato dal classificatore: match
/// esatto sull'intero `model_id`, oppure il nominato (lungo ALMENO 4 caratteri,
/// per non far matchare un bare "4" o "6") e' contenuto per intero nel
/// `model_id` (nickname tipo "sonnet" dentro "claude-sonnet-4-6"), oppure una
/// componente split-trattino del `model_id` (es. "opus" in "claude-opus-4-6") e'
/// nominata per intero ed e' lunga abbastanza da non essere un numero/suffisso
/// ambiguo.
fn model_matches(catalog_model: &str, wanted_lower: &str) -> bool {
    let m_lower = catalog_model.to_lowercase();
    if m_lower == wanted_lower {
        return true;
    }
    if wanted_lower.len() >= 4 && m_lower.contains(wanted_lower) {
        return true;
    }
    m_lower
        .split('-')
        .any(|part| part.len() >= 4 && wanted_lower.contains(part))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_matches_id_esatto() {
        assert!(model_matches("claude-sonnet-4-6", "claude-sonnet-4-6"));
    }

    #[test]
    fn model_matches_nickname_contenuto_nel_model_id() {
        assert!(model_matches("claude-sonnet-4-6", "sonnet"));
    }

    #[test]
    fn model_matches_per_componente_famiglia() {
        assert!(model_matches("claude-opus-4-6", "voglio opus per favore"));
    }

    #[test]
    fn model_matches_falso_su_estranei() {
        assert!(!model_matches("claude-sonnet-4-6", "gpt-4o"));
        // Componenti corte (numeri/suffissi) non bastano da sole.
        assert!(!model_matches("claude-sonnet-4-6", "4"));
    }
}

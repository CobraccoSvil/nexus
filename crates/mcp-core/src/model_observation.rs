//! Osservazione dei TURNI LLM reali (fase 2 del design "gate di
//! qualificazione modelli", ANELLO 5): traduce l'esito STRUTTURATO di ogni
//! turno del motore agentico — chat, figure del consiglio, sub-run, worker —
//! in scritture sui contatori del catalog.
//!
//! Prima di questo modulo il degrado runtime viveva SOLO in
//! `chat_messages::agent_run` (livello RUN, gated su `status.is_success()` e
//! sul solo run primario di chat): una figura del consiglio morta a
//! iterazione 0 su un modello degenere non lasciava alcun segnale nel catalog
//! (incidente 2026-07-14: le richieste di produzione facevano da probe e il
//! fallimento non veniva mai contato).
//!
//! PUNTO UNICO (regola L) della decisione turno->degrado; le scritture
//! delegano al punto unico esistente
//! [`crate::model_health_probe::record_model_specific_failure`]. Il PROBE non
//! passa di qui (ha il suo ciclo in `model_health_probe`): l'incisione e'
//! nell'adapter del grafo ([`crate::agent_graph_adapter::llm_gateway`]), non
//! in `neural_client`, proprio per non contare doppio i probe.
//!
//! Regola M: si decide sul `primary_cause` STRUTTURATO del gateway
//! (`GatewayHttpError.details`), mai sul testo dell'errore.

use sqlx::PgPool;

/// Setting DB della soglia di fallimenti consecutivi (condivisa col probe:
/// un solo knob per il degrado modello-specifico, regola G).
const FAILURE_THRESHOLD_SETTING: &str = "model_health_probe_failure_threshold";

/// Classe di degrado MODELLO-SPECIFICO derivabile da un turno reale fallito.
/// PURA e testabile: decide dal solo `primary_cause` strutturato del gateway.
/// `None` = nessun degrado: le cause provider-wide o di richiesta (cooldown,
/// billing, transient, context_too_long, client_error) non sono colpa del
/// modello e punirlo produrrebbe falsi positivi (stessa prudenza del probe:
/// transient MAI punitivo).
pub(crate) fn degrade_kind_for_turn(primary_cause: Option<&str>) -> Option<&'static str> {
    match primary_cause {
        // 200 degenere (content vuoto, zero tool-call, finish non terminale):
        // il provider e' sano ma il MODELLO non produce turni utili sul
        // workload reale. E' esattamente la firma dell'incidente glm/gemini-3.
        Some("empty_completion") => Some("empty_completion"),
        _ => None,
    }
}

/// Registra il fallimento MODELLO-SPECIFICO di un turno reale (fire-and-forget:
/// il hot path del turno non attende la scrittura). Chiamare SOLO quando il
/// provider/model del turno e' quello richiesto (pin a monte): su una cascata
/// multi-provider l'attribuzione al singolo modello non sarebbe affidabile.
pub(crate) fn observe_turn_failure(
    db: PgPool,
    provider: String,
    model: String,
    primary_cause: Option<String>,
) {
    let Some(kind) = degrade_kind_for_turn(primary_cause.as_deref()) else {
        return;
    };
    tokio::spawn(async move {
        let threshold: i32 = crate::settings::get_setting(&db, FAILURE_THRESHOLD_SETTING)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.trim().parse::<i32>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(3);
        let prior: i32 = sqlx::query_scalar(
            "SELECT consecutive_failures FROM ai_price_catalog
              WHERE provider = $1 AND model = $2",
        )
        .bind(&provider)
        .bind(&model)
        .fetch_optional(&db)
        .await
        .ok()
        .flatten()
        .unwrap_or(0);
        tracing::warn!(
            provider = %provider,
            model = %model,
            kind = %kind,
            prior_failures = prior,
            "model_observation: turno reale degenere -> conteggio modello-specifico"
        );
        let _ = crate::model_health_probe::record_model_specific_failure(
            &db, &provider, &model, kind, prior, threshold,
        )
        .await;
    });
}

/// Azzera il contatore dei fallimenti consecutivi dopo un turno reale
/// PRODUTTIVO (il gateway trasforma gia' le risposte degeneri in errore
/// `empty_completion`, quindi un `Ok` qui e' un turno utile). Simmetrico
/// all'incremento: senza reset per turno la parola "consecutivi" mentirebbe e
/// un modello sano al 95% marcerebbe comunque verso l'auto-disable. Guardato
/// (`consecutive_failures > 0`): nessuna scrittura nel caso comune.
pub(crate) fn observe_turn_success(db: PgPool, provider: String, model: String) {
    tokio::spawn(async move {
        let _ = sqlx::query(
            "UPDATE ai_price_catalog
                SET consecutive_failures = 0, updated_at = NOW()
              WHERE provider = $1 AND model = $2 AND consecutive_failures > 0",
        )
        .bind(&provider)
        .bind(&model)
        .execute(&db)
        .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_completion_degrada_il_modello() {
        assert_eq!(
            degrade_kind_for_turn(Some("empty_completion")),
            Some("empty_completion")
        );
    }

    #[test]
    fn cause_provider_wide_o_di_richiesta_non_degradano() {
        // Cooldown/billing/transient sono del PROVIDER; context_too_long e
        // client_error sono della RICHIESTA: mai contarli contro il modello.
        for cause in [
            "cooldown",
            "transient",
            "billing",
            "cooldown_billing",
            "client_error",
            "context_too_long",
            "sconosciuto",
        ] {
            assert_eq!(degrade_kind_for_turn(Some(cause)), None, "causa: {cause}");
        }
        assert_eq!(degrade_kind_for_turn(None), None);
    }
}

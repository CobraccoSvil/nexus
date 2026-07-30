//! `turn_task`: PUNTO UNICO (regola L) della domanda "qual e' la richiesta
//! dell'utente per QUESTO turno".
//!
//! La risposta NON si deriva dalla cronologia: si legge dove il turno l'ha
//! FISSATA all'origine, `extra[ORIGINAL_TASK_KEY]`, scritta da
//! `native_engine::build_initial_state` col messaggio con cui il run e' partito.
//!
//! Perche' un punto unico. In un run agentico `state.messages` NON contiene solo
//! quello che l'utente ha scritto: `tool_dispatch` consegna i risultati dei tool
//! come `Message::Human` a blocchi, la barriera advisory e il promemoria dei todo
//! vi appendono blocchi `<system-reminder>`, i nudge anti-stallo dell'executor
//! iniettano altri `Message::Human` di testo, e il resume HITL ne aggiunge uno di
//! conferma. Il ruolo `user` sul canale interno significa "questo lo legge il
//! modello come input", non "questo lo ha scritto l'utente": chiunque provi a
//! ricavare la richiesta scorrendo i messaggi sta indovinando (regola M), e ogni
//! consumatore indovina in modo diverso.
//!
//! Due consumatori sono passati di qui dopo essersi rotti, ciascuno con la
//! propria euristica:
//! - il supervisore prendeva il PRIMO `Human` del run: in una sessione
//!   multi-turno era il task del turno precedente (incidente Chat 11 Beaty-Book:
//!   60 iterazioni sul crash frontend invece del task di sicurezza auth);
//! - il focus del turno ([`super::turn_focus`]) prendeva l'ULTIMO: dal secondo
//!   turno in poi era un tool_result o un `<system-reminder>`, dichiarato al
//!   modello come "la richiesta da portare a termine ADESSO".
//!
//! Sono la STESSA domanda con due risposte diverse: qui ce n'e' una sola.

use serde_json::Value;

use crate::state::{AgentState, Message};

/// Chiave in `extra` dove `native_engine::build_initial_state` fissa il task del
/// TURNO CORRENTE all'avvio del run.
///
/// Sopravvive a tutto il run: ogni nodo che scrive `extra` parte da
/// `state.extra.clone()` / [`crate::state::put_extra`] (overwrite dell'intera
/// mappa, vedi doc di `state::delta`), quindi la chiave non viene persa a meta'
/// strada, e il checkpoint la serializza con lo stato.
pub const ORIGINAL_TASK_KEY: &str = "original_task";

/// La richiesta dell'utente per il turno corrente, se il run l'ha fissata
/// all'origine. `None` quando la chiave manca (stati costruiti fuori da
/// `build_initial_state`: run legacy, test, integrazioni future).
///
/// NON ha fallback sulla cronologia: e' il punto in cui si risponde "non lo so"
/// invece di rispondere con un'euristica. Chi vuole comunque una stringa sceglie
/// esplicitamente il proprio ripiego (vedi [`extract_original_task`]).
pub fn current_turn_task(state: &AgentState) -> Option<&str> {
    state
        .extra
        .get(ORIGINAL_TASK_KEY)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Task del turno corrente per il SUPERVISORE, che ne vuole comunque uno da
/// mettere nel prompt: [`current_turn_task`], altrimenti l'euristica storica
/// (primo `Message::Human` del run), altrimenti un segnaposto.
///
/// Il ripiego resta perche' il prompt del supervisore ha una riga "task" da
/// riempire in ogni caso; e' esplicito e locale a QUESTO consumatore, non una
/// regola del sistema. Il focus del turno fa la scelta opposta (nessuna
/// directive senza il dato): li' la stringa non riempie un campo, AFFERMA al
/// modello quale sia la richiesta.
pub fn extract_original_task(state: &AgentState) -> String {
    if let Some(task) = current_turn_task(state) {
        return task.to_string();
    }
    state
        .messages
        .iter()
        .find_map(|m| match m {
            Message::Human { content } => Some(content.flatten_text()),
            _ => None,
        })
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Task non disponibile".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::MessageContent;

    #[test]
    fn original_task_dalla_chiave_extra_vince_sul_primo_human() {
        // Regressione Chat 11: la cronologia multi-turno ha come PRIMO Human il task
        // del turno precedente (auto-debug crash); il task corrente e' fissato in
        // extra[ORIGINAL_TASK_KEY]. Il supervisore deve seguire quello, non il primo.
        let mut extra = serde_json::Map::new();
        extra.insert(
            ORIGINAL_TASK_KEY.to_string(),
            Value::String("Analizza la sicurezza dell'autenticazione".into()),
        );
        let state = AgentState {
            messages: vec![
                Message::Human {
                    content: MessageContent::text("Crash rilevato nel servizio frontend"),
                },
                Message::Human {
                    content: MessageContent::text("Analizza la sicurezza dell'autenticazione"),
                },
            ],
            extra,
            ..Default::default()
        };
        assert_eq!(
            extract_original_task(&state),
            "Analizza la sicurezza dell'autenticazione"
        );
        assert_eq!(
            current_turn_task(&state),
            Some("Analizza la sicurezza dell'autenticazione")
        );
    }

    #[test]
    fn original_task_fallback_primo_human_senza_chiave() {
        // Senza la chiave (run resumati/legacy) il comportamento del SUPERVISORE
        // resta l'euristica storica: primo Message::Human del run.
        let state = AgentState {
            messages: vec![
                Message::Human {
                    content: MessageContent::text("primo task"),
                },
                Message::Human {
                    content: MessageContent::text("secondo messaggio"),
                },
            ],
            ..Default::default()
        };
        assert_eq!(extract_original_task(&state), "primo task");
        // La primitiva, invece, dichiara di non saperlo: nessuna euristica.
        assert_eq!(current_turn_task(&state), None);
    }

    #[test]
    fn chiave_vuota_o_di_soli_spazi_non_e_un_task() {
        for grezzo in ["", "   ", "\n\t"] {
            let mut extra = serde_json::Map::new();
            extra.insert(ORIGINAL_TASK_KEY.to_string(), Value::String(grezzo.into()));
            let state = AgentState {
                extra,
                ..Default::default()
            };
            assert_eq!(current_turn_task(&state), None, "grezzo={grezzo:?}");
        }
    }
}

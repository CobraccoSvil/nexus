//! Playbook matcher runtime (punto unico, regola L).
//!
//! Legge `nexus_task_playbooks` e, per il task corrente, seleziona il playbook che
//! ne soddisfa il trigger, restituendone i passi deterministici. I passi vengono
//! poi messi in `AgentState::playbook_steps`; il planner li trasforma in todo
//! (`planner::playbook_fallback_block`) e il final_gate puo' verificarli
//! (es. `design_verify` dopo `nexus_visual_compare`).
//!
//! CONTESTO (anello mancante del porting): il matcher viveva nel brain Python
//! (`task_playbook.py`), rimosso con lo zero-python. Senza questo modulo nessun
//! codice Rust leggeva `nexus_task_playbooks` -> `playbook_steps` restava SEMPRE
//! vuoto a runtime -> i playbook nel DB (0366/0395/0468/0469) erano DATI MORTI e
//! l'agente non pianificava mai `nexus_visual_compare`. Questo modulo chiude il
//! buco (regola H: causa radice, non la sola migrazione-toppa).
//!
//! Niente hardcoded (regola G): trigger, keyword, soglie e passi vivono nel DB.

use std::path::Path;

use serde_json::Value;
use sqlx::{PgPool, Row};

/// Playbook selezionato per il task: passi deterministici + chiave + guida.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybookMatch {
    pub key: String,
    pub steps: Vec<String>,
    pub guidance: String,
}

/// Seleziona il PRIMO playbook abilitato (per `priority DESC`) il cui trigger
/// matcha il task. Ritorna `None` se nessuno matcha o se il DB e' irraggiungibile
/// (comportamento neutro: `playbook_steps` resta vuoto, nessun panic).
///
/// `intent`: intent risolto del turno (o `intent_hint`); puo' essere `None`.
/// `prompt_text`: messaggio utente del turno (per il match su keyword).
/// `project_root`: root del progetto, per i trigger con `project_markers`.
pub async fn match_playbook(
    db: &PgPool,
    intent: Option<&str>,
    prompt_text: &str,
    project_root: Option<&Path>,
) -> Option<PlaybookMatch> {
    let rows = sqlx::query(
        "SELECT key, trigger_json, steps_json, guidance_text \
         FROM nexus_task_playbooks \
         WHERE enabled = true \
         ORDER BY priority DESC, id ASC",
    )
    .fetch_all(db)
    .await
    .ok()?;

    let prompt_lc = prompt_text.to_lowercase();
    let intent_lc = intent.map(str::to_lowercase);

    for row in rows {
        let key: String = row.try_get("key").ok()?;
        let trigger: Value = row.try_get("trigger_json").unwrap_or(Value::Null);
        let steps_json: Value = row.try_get("steps_json").unwrap_or(Value::Null);
        let guidance: String = row.try_get("guidance_text").unwrap_or_default();

        if !trigger_matches(&trigger, intent_lc.as_deref(), &prompt_lc, project_root) {
            continue;
        }
        let steps = steps_from_json(&steps_json);
        if steps.is_empty() {
            continue;
        }
        return Some(PlaybookMatch { key, steps, guidance });
    }
    None
}

/// Estrae i passi (`steps_json` = array di stringhe) in `Vec<String>`.
fn steps_from_json(steps_json: &Value) -> Vec<String> {
    steps_json
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Valuta un singolo `trigger_json`. Criteri in AND:
///   1. `intent` del task in `trigger.intent` OPPURE una `trigger.keyword` nel prompt;
///   2. se `trigger.attachment_kind` presente: il prompt deve citarlo (best-effort);
///   3. se `trigger.project_markers` presente: almeno un marker esiste nella root.
/// PURA salvo il check FS dei markers.
fn trigger_matches(
    trigger: &Value,
    intent_lc: Option<&str>,
    prompt_lc: &str,
    project_root: Option<&Path>,
) -> bool {
    let intents = str_array_lc(trigger.get("intent"));
    let keywords = str_array_lc(trigger.get("keywords"));

    let intent_ok = match intent_lc {
        Some(i) => intents.iter().any(|t| t == i),
        None => false,
    };
    let keyword_ok = keywords
        .iter()
        .any(|k| !k.is_empty() && prompt_lc.contains(k.as_str()));
    if !(intent_ok || keyword_ok) {
        return false;
    }

    // attachment_kind: se richiesto, il prompt deve citarlo. Niente parsing degli
    // allegati qui (best-effort): conservativo, se non determinabile -> no match.
    if let Some(ak) = trigger.get("attachment_kind").and_then(Value::as_str) {
        let ak_lc = ak.to_lowercase();
        let cited = prompt_lc.contains(&ak_lc)
            || (ak_lc == "figma_make" && (prompt_lc.contains(".make") || prompt_lc.contains("figma")));
        if !cited {
            return false;
        }
    }

    // project_markers: almeno un marker deve esistere nella root del progetto.
    if let Some(markers) = trigger.get("project_markers").and_then(Value::as_array) {
        let needed: Vec<&str> = markers.iter().filter_map(Value::as_str).collect();
        if !needed.is_empty() {
            match project_root {
                Some(root) => {
                    if !needed.iter().any(|m| root.join(m).exists()) {
                        return false;
                    }
                }
                None => return false,
            }
        }
    }
    true
}

/// Array JSON di stringhe -> `Vec<String>` lowercase (vuoto se assente/non-array).
fn str_array_lc(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_lowercase()))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn design_trigger() -> Value {
        json!({
            "intent": ["fix", "frontend"],
            "keywords": ["conforme al design", "design figma"]
        })
    }

    #[test]
    fn keyword_match_basta() {
        let t = design_trigger();
        assert!(trigger_matches(&t, None, "il layout non e' conforme al design", None));
    }

    #[test]
    fn intent_match_basta() {
        let t = design_trigger();
        assert!(trigger_matches(&t, Some("frontend"), "testo senza keyword", None));
    }

    #[test]
    fn nessun_match_se_ne_intent_ne_keyword() {
        let t = design_trigger();
        assert!(!trigger_matches(&t, Some("chat"), "ciao come stai", None));
    }

    #[test]
    fn attachment_kind_richiesto_blocca_senza_indizio() {
        // implement.figma_make: senza .make/figma nel prompt non matcha.
        let t = json!({
            "intent": ["implement"],
            "keywords": ["realizza l'app"],
            "attachment_kind": "figma_make"
        });
        assert!(!trigger_matches(&t, Some("implement"), "realizza l'app gestionale", None));
        // Con "figma" nel prompt l'indizio c'e'.
        assert!(trigger_matches(&t, Some("implement"), "realizza l'app dal figma", None));
    }

    #[test]
    fn project_markers_richiede_root_e_marker() {
        let t = json!({
            "intent": ["fix"],
            "keywords": ["allinea"],
            "project_markers": ["figma_export"]
        });
        // Senza root -> no match (marker non verificabile).
        assert!(!trigger_matches(&t, Some("fix"), "allinea il layout", None));
    }

    #[test]
    fn steps_da_json_filtra_non_stringhe() {
        let v = json!(["passo 1", 42, "passo 2", null]);
        assert_eq!(steps_from_json(&v), vec!["passo 1".to_string(), "passo 2".to_string()]);
    }
}

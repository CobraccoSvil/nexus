//! Adapter del trait [`nexus_agent_graph::runtime::ports::NextActionsDeriver`].
//!
//! IMPLEMENTA (FASE 2b) `NextActionsDeriver::derive` derivando le scelte di
//! proseguimento dal testo dell'assistente, 1:1 con `next_actions.derive`
//! (`brain/agents/next_actions.py:451-484`), nello stesso ordine:
//!   1. PRIMARIO — parse del blocco machine-readable `<suggested_actions>`
//!      (`extract_block`, py:262-295). Il testo arriva GIA' pulito dal punto unico
//!      deterministico ([`nexus_agent_graph::decisions::end_turn::strip_suggested_actions`]),
//!      ma se il blocco fosse ancora presente (o passato grezzo) lo estraiamo.
//!   2. FALLBACK DETERMINISTICO — liste "Prossimi passi" -> choices
//!      (`extract_next_steps`, py:89-114). Robusto quando il provider del fallback
//!      LLM e' in cooldown o lo stop e' forzato (G1).
//!   3. FALLBACK LLM — purpose `choices_extractor` (mig 0330) risolto dalla routing
//!      matrix (regola G, `internal_routing::resolve_purpose_model_db`) + chiamata
//!      TESTUALE al gateway (`extract_via_llm`, py:376-436). Gata dall'euristica
//!      lessicale `looks_like_choices` (py:298-330).
//!
//! BEST-EFFORT (parita' col try/except py): la derivazione NON deve mai rompere il
//! turno. Su QUALUNQUE errore (router giu', provider in cooldown, JSON malformato,
//! gateway HTTP error) -> `Ok(vec![])` (nessuna scelta -> nessun meta_step), MAI un
//! `PortError`. Il blocco e' gia' rimosso dal nodo a monte, quindi il testo visibile
//! resta pulito anche se la derivazione fallisce. SOLA LETTURA: nessun gate `mode`.
//!
//! Regola F: niente prompt/response in chiaro nei log (solo lunghezze/hash).

use async_trait::async_trait;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use nexus_agent_graph::runtime::ports::{NextActionChoice, NextActionsDeriver, PortError};

use crate::internal_routing::{resolve_purpose_model_db, PurposeResolution};
use crate::nexus_gateway::{GwMessage, GwMetadata, GwRequest, NexusGatewayClient};

// Cap difensivi (parita' py:48-50).
const MAX_CHOICES: usize = 6;
const MAX_LABEL_CHARS: usize = 60;
const MAX_PROMPT_CHARS: usize = 2000;

/// Sentinelle del router: NON sono provider reali (regola G) -> fallback saltato.
const SENTINELS: [&str; 2] = ["__router_unavailable__", "__no_capable_provider__"];

// Regex 1:1 col Python (`next_actions.py`). `(?is)` = IGNORECASE + DOTALL.
static BLOCK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?is)<suggested_actions>\s*(.*?)\s*</suggested_actions>").expect("BLOCK_RE")
});
// Header "Prossimi passi" e simili (py:82-85). `(?im)` = IGNORECASE + MULTILINE.
static NEXT_STEPS_HEADER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?im)^\s*#{0,4}\s*\**\s*(?:prossimi passi|prossime azioni|next steps|soluzione immediata|azioni suggerite|come proseguire|cosa fare ora)\s*\**\s*:?\s*$",
    )
    .expect("NEXT_STEPS_HEADER_RE")
});
// Voce di lista dopo l'header (py:86).
static NEXT_STEP_ITEM_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:\d+[.)]|[-*•])\s+(.+?)\s*$").expect("NEXT_STEP_ITEM_RE"));
// Rete lessicale del gate fallback LLM (py:61-66).
static CHOICE_HINT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(vuoi|vorresti|preferisci|preferiresti|ti interessa|posso|procediamo con|scegli|scegliere|sceglier\w*|scelta|opzion\w*|alternativ\w*|tra cui|quale preferisci|come preferisci|fammi sapere)\b",
    )
    .expect("CHOICE_HINT_RE")
});
// Voce di elenco generica (py:70).
static LIST_ITEM_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:\d+[.)]|[-*•])\s+\S").expect("LIST_ITEM_RE"));
// Fence markdown attorno al JSON (py:281-282 / 423-424).
static FENCE_OPEN_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)^```(?:json)?\s*").expect("FENCE_OPEN_RE"));
static FENCE_CLOSE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s*```$").expect("FENCE_CLOSE_RE"));

/// Una scelta grezza nel JSON `{label, prompt}` (parse del blocco / output LLM).
#[derive(Debug, Deserialize)]
struct RawChoice {
    label: Option<String>,
    prompt: Option<String>,
}

/// Adapter [`NextActionsDeriver`] -> parse blocco + fallback deterministico +
/// fallback LLM (`choices_extractor`).
pub struct NextActionsDeriverAdapter {
    /// Pool Postgres per risolvere il purpose `choices_extractor` (regola G) e
    /// leggere il template del prompt + la porta del gateway.
    db: PgPool,
}

impl NextActionsDeriverAdapter {
    /// Costruisce l'adapter sul pool Postgres condiviso.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

/// Normalizza una lista grezza di scelte nel formato contrattuale (1:1 con
/// `_coerce_choices` py:231-259): accetta solo entry con `label`+`prompt` non
/// vuoti, tronca ai cap difensivi, limita il numero.
fn coerce_choices(raw: Vec<RawChoice>) -> Vec<NextActionChoice> {
    let mut out = Vec::new();
    for item in raw {
        let (Some(label), Some(prompt)) = (item.label, item.prompt) else {
            continue;
        };
        let label = label.trim();
        let prompt = prompt.trim();
        if label.is_empty() || prompt.is_empty() {
            continue;
        }
        out.push(NextActionChoice {
            label: truncate_chars(label, MAX_LABEL_CHARS),
            prompt: truncate_chars(prompt, MAX_PROMPT_CHARS),
        });
        if out.len() >= MAX_CHOICES {
            break;
        }
    }
    out
}

/// Tronca a `max` caratteri (non byte: parita' con lo slicing Python sui char).
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Rimuove eventuali fence markdown attorno a un JSON (py:281-282 / 423-424).
fn strip_fences(inner: &str) -> String {
    let no_open = FENCE_OPEN_RE.replace(inner.trim(), "");
    FENCE_CLOSE_RE.replace(&no_open, "").trim().to_string()
}

/// PRIMARIO: estrae e parsa il blocco `<suggested_actions>` (1:1 `extract_block`).
/// Ritorna le scelte (vuoto se il blocco manca o e' malformato — non solleva).
fn extract_block(text: &str) -> Vec<NextActionChoice> {
    let Some(cap) = BLOCK_RE.captures(text) else {
        return Vec::new();
    };
    let Some(inner) = cap.get(1) else {
        return Vec::new();
    };
    let inner = strip_fences(inner.as_str());
    match serde_json::from_str::<Vec<RawChoice>>(&inner) {
        Ok(parsed) => coerce_choices(parsed),
        Err(_) => Vec::new(),
    }
}

/// FALLBACK DETERMINISTICO (no LLM): se il testo ha un header "Prossimi passi"
/// seguito da una lista, genera una choice per voce (1:1 `extract_next_steps`).
fn extract_next_steps(text: &str) -> Vec<NextActionChoice> {
    if text.is_empty() {
        return Vec::new();
    }
    let Some(m) = NEXT_STEPS_HEADER_RE.find(text) else {
        return Vec::new();
    };
    let tail = &text[m.end()..];
    let mut out = Vec::new();
    for cap in NEXT_STEP_ITEM_RE.captures_iter(tail) {
        let Some(raw) = cap.get(1) else { continue };
        // Toglie ** e backtick, rstrip del punto finale (py:103).
        let label_full = raw
            .as_str()
            .replace("**", "")
            .replace('`', "")
            .trim()
            .trim_end_matches('.')
            .to_string();
        if label_full.is_empty() || label_full.chars().count() < 4 {
            continue;
        }
        // label = prima del ':' / '.' (py:106).
        let label_src = label_full
            .split(':')
            .next()
            .unwrap_or(&label_full)
            .split('.')
            .next()
            .unwrap_or(&label_full)
            .trim();
        let label = truncate_chars(label_src, MAX_LABEL_CHARS);
        // prompt: istruzione a ESEGUIRE il passo (py:107-110).
        let prompt = format!(
            "Esegui questo passo, modificando i file del progetto: {label_full}. \
             Al termine verifica che funzioni davvero end-to-end."
        );
        out.push(NextActionChoice {
            label,
            prompt: truncate_chars(&prompt, MAX_PROMPT_CHARS),
        });
        if out.len() >= MAX_CHOICES {
            break;
        }
    }
    out
}

/// Conteggio voci di elenco (py:73-74).
fn list_item_count(text: &str) -> usize {
    LIST_ITEM_RE.find_iter(text).count()
}

/// Gate del fallback LLM: rete lessicale `_regex_looks_like_choices` (py:298-313).
/// (Il detector semantico a embedding del Python e' un complemento opzionale: in
/// sua assenza Python ricade su questa rete lessicale, che qui usiamo come gate.)
fn looks_like_choices(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    if text.matches('?').count() >= 2 {
        return true;
    }
    let hints = CHOICE_HINT_RE.find_iter(text).count();
    if hints >= 2 {
        return true;
    }
    hints >= 1 && list_item_count(text) >= 2
}

impl NextActionsDeriverAdapter {
    /// FALLBACK LLM: risolve il purpose `choices_extractor` (regola G) e chiama il
    /// gateway in modalita' TESTUALE (nessun tool_choice: e' estrazione di testo).
    /// Best-effort: ogni errore -> `Vec::new()` (mai propagato).
    async fn extract_via_llm(&self, assistant_text: &str) -> Vec<NextActionChoice> {
        // 1. Risoluzione modello (tier-only, regola G).
        let (provider, model) =
            match resolve_purpose_model_db(&self.db, "choices_extractor").await {
                PurposeResolution::Resolved {
                    provider, model, ..
                } => (provider, model),
                _ => return Vec::new(),
            };
        if SENTINELS.contains(&provider.as_str()) || SENTINELS.contains(&model.as_str()) {
            return Vec::new();
        }

        // 2. Client gateway lazy dalla porta nel DB (regola G: niente hardcoded).
        let gw = match self.gateway_client().await {
            Some(gw) => gw,
            None => return Vec::new(),
        };

        // 3. Prompt extractor (template DB o fallback hardcoded come Python).
        let prompt = self.build_extractor_prompt(assistant_text).await;

        let req = GwRequest {
            model,
            messages: vec![GwMessage {
                role: "user".to_string(),
                content: prompt,
            }],
            max_tokens: Some(1024),
            temperature: Some(0.0),
            tools: None,
            metadata: GwMetadata {
                tenant_id: "internal".to_string(),
                user_id: "system".to_string(),
                request_id: Uuid::new_v4().to_string(),
                sensitivity_tier: 0,
                feature: "choices_extractor".to_string(),
            },
        };

        let resp = match tokio::time::timeout(std::time::Duration::from_secs(20), gw.complete(req))
            .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "next_actions: estrazione LLM fallita (gateway)");
                return Vec::new();
            }
            Err(_) => {
                tracing::warn!("next_actions: estrazione LLM in timeout");
                return Vec::new();
            }
        };

        // 4. Parse output: ci aspettiamo una LISTA JSON top-level (tollerante ai fence).
        let cleaned = strip_fences(resp.content.trim());
        match serde_json::from_str::<Vec<RawChoice>>(&cleaned) {
            Ok(parsed) => {
                let choices = coerce_choices(parsed);
                if !choices.is_empty() {
                    tracing::info!(
                        n = choices.len(),
                        "next_actions: fallback LLM ha estratto scelte"
                    );
                }
                choices
            }
            Err(_) => {
                tracing::debug!(
                    out_len = resp.content.len(),
                    "next_actions: output extractor non-JSON"
                );
                Vec::new()
            }
        }
    }

    /// Costruisce il client gateway dalla porta nel DB (regola G: niente env/porta
    /// hardcoded). `None` se la lettura della porta fallisce. Riusa il token di
    /// servizio condiviso (env per il solo segreto, come `main.rs`).
    async fn gateway_client(&self) -> Option<NexusGatewayClient> {
        let port = nexus_auth::resolve_port(&self.db, "nexus_gateway_port").await;
        if port == 0 {
            return None;
        }
        let url = format!("http://127.0.0.1:{port}");
        let token = std::env::var("NEXUS_GATEWAY_SERVICE_TOKEN")
            .unwrap_or_else(|_| "dev-internal-token".to_string());
        Some(NexusGatewayClient::new(url, token))
    }

    /// Prompt per il modello estrattore: template DB `system.choices_extractor` se
    /// presente (placeholder `{{assistant_text}}`), altrimenti fallback hardcoded
    /// (graceful degradation, parita' py:333-373).
    async fn build_extractor_prompt(&self, assistant_text: &str) -> String {
        let tpl = sqlx::query_scalar::<_, String>(
            "SELECT content FROM nexus_prompt_templates \
             WHERE key = 'system.choices_extractor' AND is_active = TRUE LIMIT 1",
        )
        .fetch_optional(&self.db)
        .await
        .ok()
        .flatten();

        if let Some(tpl) = tpl.filter(|t| !t.trim().is_empty()) {
            return tpl.replace("{{assistant_text}}", assistant_text);
        }

        format!(
            "Sei un estrattore. Ti viene data la risposta di un assistente AI.\n\
             Se la risposta propone all'utente delle SCELTE su come proseguire \
             (opzioni, varianti, prossimi passi suggeriti), estraile.\n\n\
             Restituisci ESCLUSIVAMENTE un array JSON, senza testo aggiuntivo, nel formato:\n\
             [{{\"label\":\"<testo breve del pulsante, max 40 caratteri>\",\
             \"prompt\":\"<istruzione completa e non ambigua, pronta da inviare come \
             messaggio utente per proseguire con quella scelta>\"}}]\n\n\
             - label: conciso, orientato all'azione, in italiano (max 40 caratteri).\n\
             - Se la risposta NON propone scelte, restituisci esattamente: []\n\
             - Massimo 6 scelte.\n\n\
             RISPOSTA DELL'ASSISTENTE:\n<<<\n{assistant_text}\n>>>"
        )
    }
}

#[async_trait]
impl NextActionsDeriver for NextActionsDeriverAdapter {
    /// Deriva le scelte da `cleaned_text` (gia' privo del blocco a monte). Ordine
    /// 1:1 con `next_actions.derive`: blocco -> deterministico -> LLM gated.
    /// Best-effort: ogni errore -> `Ok(vec![])`, mai `PortError`.
    async fn derive(
        &self,
        cleaned_text: &str,
    ) -> Result<Vec<NextActionChoice>, PortError> {
        if cleaned_text.trim().is_empty() {
            return Ok(Vec::new());
        }

        // 1. PRIMARIO: blocco machine-readable (se ancora presente nel testo).
        let block = extract_block(cleaned_text);
        if !block.is_empty() {
            return Ok(block);
        }

        // 2. FALLBACK DETERMINISTICO: liste "Prossimi passi".
        let det = extract_next_steps(cleaned_text);
        if !det.is_empty() {
            return Ok(det);
        }

        // 3. FALLBACK LLM: solo se l'euristica lessicale lo giustifica.
        if looks_like_choices(cleaned_text) {
            return Ok(self.extract_via_llm(cleaned_text).await);
        }

        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_estrae_e_normalizza() {
        let text = "Ecco le opzioni.\n<suggested_actions>\n\
            [{\"label\":\"Aggiungi immagini\",\"prompt\":\"Aggiungi immagini reali nella home.\"}]\n\
            </suggested_actions>";
        let choices = extract_block(text);
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].label, "Aggiungi immagini");
        assert!(choices[0].prompt.contains("immagini reali"));
    }

    #[test]
    fn block_tollera_fence_markdown() {
        let text = "<suggested_actions>\n```json\n\
            [{\"label\":\"X\",\"prompt\":\"Esegui X.\"}]\n```\n</suggested_actions>";
        let choices = extract_block(text);
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].label, "X");
    }

    #[test]
    fn block_malformato_ritorna_vuoto() {
        let text = "<suggested_actions>non-json</suggested_actions>";
        assert!(extract_block(text).is_empty());
    }

    #[test]
    fn block_entry_senza_prompt_scartata() {
        let text = "<suggested_actions>[{\"label\":\"solo label\"}]</suggested_actions>";
        assert!(extract_block(text).is_empty(), "entry senza prompt scartata");
    }

    #[test]
    fn next_steps_genera_choice_per_voce() {
        let text = "Ho analizzato il problema.\n\n## Prossimi passi\n\
            1. Correggere il bug nel router\n\
            2. Aggiungere un test di regressione\n";
        let choices = extract_next_steps(text);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].label, "Correggere il bug nel router");
        assert!(choices[0].prompt.contains("Esegui questo passo"));
        assert!(choices[1].label.starts_with("Aggiungere un test"));
    }

    #[test]
    fn next_steps_senza_header_vuoto() {
        let text = "Solo testo, nessun header.\n1. una voce\n2. due voci\n";
        assert!(extract_next_steps(text).is_empty());
    }

    #[test]
    fn next_steps_label_taglia_su_due_punti() {
        let text = "Prossimi passi:\n- Refactor router: estrarre il punto unico\n";
        let choices = extract_next_steps(text);
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].label, "Refactor router");
        // Il prompt mantiene la riga completa.
        assert!(choices[0].prompt.contains("estrarre il punto unico"));
    }

    #[test]
    fn looks_like_choices_due_domande() {
        assert!(looks_like_choices("Vuoi A? Oppure preferisci B?"));
    }

    #[test]
    fn looks_like_choices_hint_piu_lista() {
        let text = "Posso procedere con queste opzioni:\n- prima\n- seconda\n";
        assert!(looks_like_choices(text));
    }

    #[test]
    fn looks_like_choices_negativo() {
        assert!(!looks_like_choices(
            "Ho completato la modifica del file come richiesto."
        ));
    }

    #[test]
    fn coerce_tronca_e_limita() {
        let raw: Vec<RawChoice> = (0..10)
            .map(|i| RawChoice {
                label: Some(format!("label-{i}")),
                prompt: Some(format!("prompt-{i}")),
            })
            .collect();
        let choices = coerce_choices(raw);
        assert_eq!(choices.len(), MAX_CHOICES, "max 6 scelte");
    }

    /// Best-effort end-to-end: testo vuoto -> nessuna scelta, mai PortError.
    #[sqlx::test]
    async fn derive_testo_vuoto_e_ok_vuoto(pool: PgPool) {
        let port = NextActionsDeriverAdapter::new(pool.clone());
        let r = port.derive("   ").await.expect("mai PortError");
        assert!(r.is_empty());
    }

    /// `derive` usa il blocco primario senza toccare il DB/gateway.
    #[sqlx::test]
    async fn derive_usa_il_blocco_primario(pool: PgPool) {
        let port = NextActionsDeriverAdapter::new(pool.clone());
        let text = "Risposta.\n<suggested_actions>\
            [{\"label\":\"Vai\",\"prompt\":\"Procedi col passo successivo.\"}]\
            </suggested_actions>";
        let r = port.derive(text).await.expect("mai PortError");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].label, "Vai");
    }

    /// `derive` cade sul fallback deterministico "Prossimi passi".
    #[sqlx::test]
    async fn derive_fallback_deterministico(pool: PgPool) {
        let port = NextActionsDeriverAdapter::new(pool.clone());
        let text = "Analisi fatta.\n\nProssimi passi:\n\
            - Implementare il modulo di cache\n\
            - Aggiungere i test\n";
        let r = port.derive(text).await.expect("mai PortError");
        assert_eq!(r.len(), 2);
    }
}

//! `escalation`: SELEZIONE AGENTICA del modello di auto-escalation dell'orchestratore.
//!
//! Punto unico (regola L) della domanda "dato il modello corrente che sta fallendo
//! e l'insieme dei candidati di escalation ammissibili (con tier + telemetria), qual
//! e' il PROSSIMO modello migliore?". La funzione [`pick_escalation_model`] e' PURA:
//! l'insieme dei candidati (catena intra-provider + candidati cross-provider, gia'
//! filtrati per capability/cooldown e arricchiti di tier + telemetria) arriva gia'
//! risolto dal chiamante via [`crate::runtime::ports::EscalationPort`] (nessun IO,
//! nessuna lettura DB qui — regola G).
//!
//! ## Selezione AGENTICA (niente catena fissa)
//!
//! Non c'e' piu' una catena POSIZIONALE (indice per numero di escalation) ne' uno
//! split fisso Tier1-intra / Tier2-cross: c'e' UN solo insieme di candidati, ordinato
//! agenticamente ad ogni escalation da segnali strutturati (regola M):
//!   1. **salute** — i candidati "recently_failed" (cooldown / error-rate / fallimenti
//!      consecutivi, punto unico [`crate::decisions::governance::is_recently_failed`])
//!      sono RETROCESSI: si prende un modello sano se esiste (fail-safe: mai a mani
//!      vuote);
//!   2. **forza di tier** — a parita' di salute si preferisce il tier PIU' FORTE
//!      (heavy > medium > light): l'escalation serve a ottenere piu' capacita' quando
//!      il modello debole non converge (risolve il "restava su flash");
//!   3. **likelihood** — a parita' di tier, punteggio di probabilita' di successo
//!      ([`crate::decisions::governance::likelihood_score`], telemetria);
//!   4. **ordine d'ingresso** — tie-breaker finale (`sort_by` STABILE): con telemetria
//!      e tier uniformi l'ordine d'ingresso (preferenza del routing) e' preservato.
//!
//! Il modello corrente (quello che sta fallendo) e' sempre ESCLUSO. La scelta e'
//! DETERMINISTICA e replay-stabile (nessun LLM, solo il ranking puro sui segnali).

use serde::{Deserialize, Serialize};

use crate::decisions::governance::{
    is_recently_failed, likelihood_score, GovernancePolicy, ModelTelemetry,
};

/// Rank numerico del performance-tier per l'ordinamento di escalation (piu' alto =
/// piu' capace). Sconosciuto/assente -> `medium` (neutro): non penalizza ne' premia.
fn tier_rank(tier: Option<&str>) -> u8 {
    match tier.map(|t| t.trim().to_ascii_lowercase()).as_deref() {
        Some("light") => 1,
        Some("medium") => 2,
        Some("heavy") => 3,
        _ => 2,
    }
}

/// Una voce della catena di escalation intra-provider
/// (`nexus_model_escalation_chain`) risolta dall'impl della porta. Tipo di
/// TRASPORTO usato dalla porta per COSTRUIRE i candidati unificati; la SELEZIONE
/// non lo usa direttamente (usa [`EscalationCandidate`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainEntry {
    /// Modello di destinazione dell'escalation (`escalation_model`).
    pub escalation_model: String,
    /// Performance tier del modello di destinazione (dal catalog). `None` se non
    /// risolto (fail-open: trattato come `medium` neutro nel ranking).
    #[serde(default)]
    pub tier: Option<String>,
}

/// Candidato cross-provider (`loop_fallback_default`) risolto dal router. Tipo di
/// TRASPORTO come [`ChainEntry`]: la porta lo fonde nei candidati unificati.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossProviderCandidate {
    /// Provider del candidato cross-provider.
    pub provider: String,
    /// Modello del candidato cross-provider.
    pub model: String,
    /// Performance tier del modello cross-provider (dal catalog). `None` = `medium`.
    #[serde(default)]
    pub tier: Option<String>,
}

/// Candidato di escalation UNIFICATO (intra o cross): l'unita' su cui lavora la
/// selezione agentica. Provider+model+tier dal routing/catalog, telemetria dal
/// `model_health_probe`+catalog (regola M). Costruito dall'impl della porta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EscalationCandidate {
    /// Provider del candidato.
    pub provider: String,
    /// Modello del candidato.
    pub model: String,
    /// Performance tier (`light`/`medium`/`heavy`), `None` = `medium` neutro.
    #[serde(default)]
    pub tier: Option<String>,
    /// Telemetria strutturata del candidato (salute/likelihood). `default` = sano
    /// (nessun segnale = nessuna penalita').
    #[serde(default)]
    pub telemetry: ModelTelemetry,
}

/// Modello promosso dall'escalation: provider + model con cui RI-ESEGUIRE il turno
/// (signature-loop) o da rendere sticky (cap G1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscalationPick {
    /// Provider del modello promosso.
    pub provider: String,
    /// Modello promosso.
    pub model: String,
    /// `true` se il modello promosso e' dello STESSO provider corrente (eco
    /// diagnostica: il chiamante non cambia comportamento in base a questo).
    pub from_chain: bool,
    /// Performance tier del modello promosso, propagato dal candidato scelto SENZA
    /// lookup extra. Il chiamante lo scrive in `StateDelta::current_tier`. `None` se
    /// il tier non era risolto.
    #[serde(default)]
    pub tier: Option<String>,
}

/// PUNTO UNICO (regola L) della selezione AGENTICA del prossimo modello di
/// escalation. PURA. Dato l'insieme dei candidati ammissibili (gia' con tier +
/// telemetria) e la coppia corrente, ritorna il modello migliore o `None`.
///
/// Ordinamento (stabile): salute (non-recently_failed prima) -> tier DESC (piu'
/// capace prima) -> likelihood DESC -> ordine d'ingresso. Il modello CORRENTE e'
/// escluso. `None` solo se non resta alcun candidato (insieme vuoto o solo il
/// corrente): il chiamante chiude secco.
///
/// - `candidates`: insieme unificato (intra + cross) dei target di escalation, gia'
///   filtrati per capability/cooldown dall'impl della porta.
/// - `current_provider` / `current_model`: la coppia del turno che sta fallendo
///   (esclusa dalla selezione).
/// - `policy`: soglie governance DB-driven (regola G) per salute/likelihood.
pub fn pick_escalation_model(
    candidates: &[EscalationCandidate],
    current_provider: Option<&str>,
    current_model: Option<&str>,
    policy: &GovernancePolicy,
) -> Option<EscalationPick> {
    let cur_p = current_provider.map(|p| p.trim().to_ascii_lowercase());
    let cur_m = current_model.map(|m| m.trim().to_string());
    let is_current = |c: &EscalationCandidate| {
        cur_p.as_deref() == Some(c.provider.trim().to_ascii_lowercase().as_str())
            && cur_m.as_deref() == Some(c.model.trim())
    };

    // Arricchisco i candidati (esclusi il corrente e i vuoti) con bucket-salute,
    // rank-tier e punteggio. `sort_by` STABILE preserva l'ordine d'ingresso a
    // parita' di chiave -> ultimo tie-breaker implicito.
    let mut ranked: Vec<(&EscalationCandidate, u8, u8, f64)> = candidates
        .iter()
        .filter(|c| !c.provider.trim().is_empty() && !c.model.trim().is_empty())
        .filter(|c| !is_current(c))
        .map(|c| {
            let bucket = u8::from(is_recently_failed(&c.telemetry, policy));
            let trank = tier_rank(c.tier.as_deref());
            let score = likelihood_score(&c.telemetry, policy);
            (c, bucket, trank, score)
        })
        .collect();

    ranked.sort_by(|a, b| {
        a.1.cmp(&b.1) // salute: bucket ASC (sani prima)
            .then(b.2.cmp(&a.2)) // tier DESC (piu' capace prima)
            .then(b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal)) // likelihood DESC
    });

    ranked.first().map(|(c, _, _, _)| EscalationPick {
        provider: c.provider.clone(),
        model: c.model.clone(),
        from_chain: cur_p.as_deref() == Some(c.provider.trim().to_ascii_lowercase().as_str()),
        tier: c.tier.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(provider: &str, model: &str, tier: Option<&str>) -> EscalationCandidate {
        EscalationCandidate {
            provider: provider.to_string(),
            model: model.to_string(),
            tier: tier.map(str::to_string),
            telemetry: ModelTelemetry::default(),
        }
    }

    /// Telemetria che marca un candidato come "recently_failed" (cooldown).
    fn cand_failed(provider: &str, model: &str, tier: Option<&str>) -> EscalationCandidate {
        let mut c = cand(provider, model, tier);
        c.telemetry.provider_in_cooldown = true;
        c
    }

    #[test]
    fn preferisce_il_tier_piu_forte_a_parita_di_salute() {
        // Il cuore del fix "restava su flash": tra due candidati sani, vince heavy.
        let p = GovernancePolicy::default();
        let cands = vec![
            cand("deepseek", "deepseek-v4-flash-alt", Some("medium")),
            cand("deepseek", "deepseek-v4-pro", Some("heavy")),
        ];
        let r = pick_escalation_model(&cands, Some("deepseek"), Some("deepseek-v4-flash"), &p)
            .expect("pick");
        assert_eq!(r.model, "deepseek-v4-pro");
        assert_eq!(r.tier.as_deref(), Some("heavy"));
    }

    #[test]
    fn cross_provider_heavy_vince_su_intra_medium() {
        // Non c'e' preferenza intra-provider: se il cross e' piu' forte, ci si va.
        let p = GovernancePolicy::default();
        let cands = vec![
            cand("deepseek", "deepseek-v4-pro-medium", Some("medium")),
            cand("google", "gemini-2.5-pro", Some("heavy")),
        ];
        let r = pick_escalation_model(&cands, Some("deepseek"), Some("deepseek-v4-flash"), &p)
            .expect("pick");
        assert_eq!(r.provider, "google");
        assert_eq!(r.model, "gemini-2.5-pro");
        assert!(!r.from_chain);
    }

    #[test]
    fn un_sano_batte_un_piu_forte_ma_malato() {
        // Salute prima del tier: un medium sano batte un heavy in cooldown.
        let p = GovernancePolicy::default();
        let cands = vec![
            cand_failed("google", "gemini-2.5-pro", Some("heavy")),
            cand("deepseek", "deepseek-v4-pro", Some("medium")),
        ];
        let r = pick_escalation_model(&cands, Some("deepseek"), Some("deepseek-v4-flash"), &p)
            .expect("pick");
        assert_eq!(r.model, "deepseek-v4-pro"); // sano, anche se medium
    }

    #[test]
    fn esclude_il_modello_corrente() {
        let p = GovernancePolicy::default();
        let cands = vec![
            cand("deepseek", "deepseek-v4-flash", Some("medium")), // == corrente
            cand("deepseek", "deepseek-v4-pro", Some("heavy")),
        ];
        let r = pick_escalation_model(&cands, Some("deepseek"), Some("deepseek-v4-flash"), &p)
            .expect("pick");
        assert_eq!(r.model, "deepseek-v4-pro");
    }

    #[test]
    fn esclusione_corrente_case_insensitive_sul_provider() {
        let p = GovernancePolicy::default();
        let cands = vec![cand("DeepSeek", "deepseek-v4-flash", Some("medium"))];
        // L'unico candidato e' il corrente (provider case-diverso) -> None.
        let r = pick_escalation_model(&cands, Some("deepseek"), Some("deepseek-v4-flash"), &p);
        assert_eq!(r, None);
    }

    #[test]
    fn insieme_vuoto_e_none() {
        let p = GovernancePolicy::default();
        let r = pick_escalation_model(&[], Some("deepseek"), Some("deepseek-v4-flash"), &p);
        assert_eq!(r, None);
    }

    #[test]
    fn tutti_malati_ne_ritorna_comunque_uno_fail_safe() {
        // Fail-safe: se tutti "recently_failed", non resta a mani vuote (retrocede,
        // non cancella). Sceglie il piu' forte tra i malati.
        let p = GovernancePolicy::default();
        let cands = vec![
            cand_failed("mistral", "mistral-large", Some("medium")),
            cand_failed("google", "gemini-2.5-pro", Some("heavy")),
        ];
        let r = pick_escalation_model(&cands, Some("deepseek"), Some("deepseek-v4-flash"), &p)
            .expect("fail-safe: un candidato deve uscire");
        assert_eq!(r.model, "gemini-2.5-pro"); // heavy tra i malati
    }

    #[test]
    fn a_parita_totale_preserva_ordine_ingresso() {
        // Stessa salute, stesso tier, stessa likelihood -> ordine d'ingresso.
        let p = GovernancePolicy::default();
        let cands = vec![
            cand("google", "gemini-2.5-pro", Some("heavy")),
            cand("anthropic", "claude-opus", Some("heavy")),
        ];
        let r = pick_escalation_model(&cands, Some("deepseek"), Some("deepseek-v4-flash"), &p)
            .expect("pick");
        assert_eq!(r.provider, "google"); // primo in ingresso
    }

    #[test]
    fn tier_assente_trattato_come_medium_neutro() {
        // Un candidato senza tier (None) non e' penalizzato ne' premiato: rank medium.
        let p = GovernancePolicy::default();
        let cands = vec![
            cand("google", "gemini", None),        // -> medium
            cand("deepseek", "pro", Some("heavy")), // heavy vince
        ];
        let r = pick_escalation_model(&cands, Some("x"), Some("y"), &p).expect("pick");
        assert_eq!(r.model, "pro");
    }

    #[test]
    fn from_chain_true_se_stesso_provider() {
        let p = GovernancePolicy::default();
        let cands = vec![cand("deepseek", "deepseek-v4-pro", Some("heavy"))];
        let r = pick_escalation_model(&cands, Some("deepseek"), Some("deepseek-v4-flash"), &p)
            .expect("pick");
        assert!(r.from_chain);
    }
}

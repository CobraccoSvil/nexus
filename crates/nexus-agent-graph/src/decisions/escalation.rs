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
/// piu' capace). DELEGA al PUNTO UNICO del vocabolario tier
/// ([`super::tiers::tier_rank`], scala a 5 livelli light<medium<high<heavy<frontier;
/// sconosciuto/assente -> `medium` neutro). Wrapper sottile che adatta l'`Option`
/// del chiamante (assente == "" == medium neutro).
fn tier_rank(tier: Option<&str>) -> u8 {
    super::tiers::tier_rank(tier.unwrap_or(""))
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

/// Limita i candidati di escalation a una salita di UN SOLO gradino di tier
/// rispetto a `current_tier` (piu' i pari-tier e inferiori, che
/// [`pick_escalation_model`] arbitra con l'ordine salute -> tier -> likelihood).
/// Evita il SALTO diretto al tier MASSIMO disponibile su una non-convergenza:
/// si sale gradualmente (es. medium -> high, non medium -> frontier), coerente
/// con `clamp_one_step` dello scale-controller e con l'obiettivo costi (un
/// gradino costa molto meno del massimo, e spesso basta). FALLBACK graceful: se
/// NESSUN candidato e' entro `current+1` (l'unico piu' capace e' 2+ gradini
/// sopra), ritorna TUTTI i candidati — meglio un salto grande che restare bloccati
/// su un modello che non converge. `current_tier` `None` = medium neutro
/// (coerente con `tier_rank`).
pub fn cap_candidates_one_step(
    candidates: &[EscalationCandidate],
    current_tier: Option<&str>,
) -> Vec<EscalationCandidate> {
    let ceil = tier_rank(current_tier).saturating_add(1);
    let capped: Vec<EscalationCandidate> = candidates
        .iter()
        .filter(|c| tier_rank(c.tier.as_deref()) <= ceil)
        .cloned()
        .collect();
    if capped.is_empty() {
        candidates.to_vec()
    } else {
        capped
    }
}

/// PUNTO UNICO (regola L) della selezione AGENTICA del SOSTITUTO su provider
/// caduto (failover cross-provider). PURA. Dato l'insieme dei candidati
/// ammissibili (tutti i tier, gia' filtrati per eleggibilita'/cooldown/exclude
/// dall'impl della porta) sceglie il modello con la migliore probabilita' di
/// SOSTITUIRE quello che sta fallendo.
///
/// Differenza deliberata da [`pick_escalation_model`]: il tier NON e' un criterio
/// di ordinamento assoluto ne' un filtro — e' un'INDICAZIONE. Il punteggio e'
/// `likelihood * affinita'`, dove l'affinita' penalizza dolcemente ogni livello
/// di tier SOTTO quello corrente ([`GovernancePolicy::failover_downgrade_penalty`]
/// per livello) e non premia i livelli sopra. Un candidato piu' debole ma con
/// telemetria nettamente migliore puo' quindi superare l'indicazione; un
/// downgrade resta sempre ammesso se e' l'unica opzione sana.
///
/// Ordinamento (stabile): salute (non-recently_failed prima) -> punteggio DESC
/// (`likelihood * affinita'`) -> distanza di tier dal corrente ASC (il sostituto
/// piu' vicino) -> ordine d'ingresso (preferenza del routing: featured/economico).
///
/// - `candidates`: insieme unificato cross-provider/cross-tier dei sostituti.
/// - `current_tier`: tier del modello che sta fallendo (indicazione; `None` =
///   medium neutro, coerente con `tier_rank`).
/// - `policy`: soglie governance DB-driven (regola G).
pub fn pick_failover_model(
    candidates: &[EscalationCandidate],
    current_tier: Option<&str>,
    policy: &GovernancePolicy,
) -> Option<CrossProviderCandidate> {
    let cur_rank = tier_rank(current_tier);
    // Penalty fuori range (0, 1] -> indicazione disattivata (1.0), mai un boost.
    let penalty = if policy.failover_downgrade_penalty > 0.0
        && policy.failover_downgrade_penalty <= 1.0
    {
        policy.failover_downgrade_penalty
    } else {
        1.0
    };

    let mut ranked: Vec<(&EscalationCandidate, u8, f64, u8)> = candidates
        .iter()
        .filter(|c| !c.provider.trim().is_empty() && !c.model.trim().is_empty())
        .map(|c| {
            let bucket = u8::from(is_recently_failed(&c.telemetry, policy));
            let trank = tier_rank(c.tier.as_deref());
            let affinity = if trank < cur_rank {
                penalty.powi(i32::from(cur_rank - trank))
            } else {
                1.0
            };
            let score = likelihood_score(&c.telemetry, policy) * affinity;
            let distance = cur_rank.abs_diff(trank);
            (c, bucket, score, distance)
        })
        .collect();

    ranked.sort_by(|a, b| {
        a.1.cmp(&b.1) // salute: bucket ASC (sani prima)
            .then(b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal)) // punteggio DESC
            .then(a.3.cmp(&b.3)) // distanza di tier ASC (sostituto piu' vicino)
    });

    ranked.first().map(|(c, _, _, _)| CrossProviderCandidate {
        provider: c.provider.clone(),
        model: c.model.clone(),
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

    // ---- pick_failover_model (sostituto agentico, tier come indicazione) ----

    #[test]
    fn failover_preferisce_il_sostituto_piu_vicino_al_tier_corrente() {
        // L'incidente reale: cade un heavy, il vecchio failover ripiegava al
        // pavimento medium (v4-flash) ignorando il high sano (v4-pro). Con
        // l'affinita' vince il sostituto PIU' VICINO al corrente.
        let p = GovernancePolicy::default();
        let cands = vec![
            cand("deepseek", "deepseek-v4-flash", Some("medium")),
            cand("deepseek", "deepseek-v4-pro", Some("high")),
        ];
        let r = pick_failover_model(&cands, Some("heavy"), &p).expect("pick");
        assert_eq!(r.model, "deepseek-v4-pro");
        assert_eq!(r.tier.as_deref(), Some("high"));
    }

    #[test]
    fn failover_salute_prima_di_tutto() {
        // Un pari-tier malato perde contro un tier inferiore sano: la salute
        // (segnale strutturato, regola M) domina l'indicazione di tier.
        let p = GovernancePolicy::default();
        let cands = vec![
            cand_failed("google", "gemini-heavy", Some("heavy")),
            cand("mistral", "codestral-latest", Some("medium")),
        ];
        let r = pick_failover_model(&cands, Some("heavy"), &p).expect("pick");
        assert_eq!(r.model, "codestral-latest");
    }

    #[test]
    fn failover_likelihood_supera_l_indicazione_di_tier() {
        // Il tier NON e' imposto: un candidato piu' lontano dal corrente ma con
        // telemetria molto migliore vince su uno piu' vicino ma degradato.
        let p = GovernancePolicy::default();
        let mut vicino_degradato = cand("deepseek", "quasi-pari", Some("high"));
        // 1 fallimento consecutivo: non "recently_failed" (soglia 2) ma
        // likelihood *= 1/(1+0.5) = 0.667 -> 0.667*0.85 = 0.567.
        vicino_degradato.telemetry.consecutive_failures = 1;
        let lontano_sano = cand("mistral", "large-sano", Some("medium"));
        // sano: 1.0 * 0.85^2 = 0.7225 > 0.567.
        let cands = vec![vicino_degradato, lontano_sano];
        let r = pick_failover_model(&cands, Some("heavy"), &p).expect("pick");
        assert_eq!(r.model, "large-sano");
    }

    #[test]
    fn failover_downgrade_ammesso_se_unica_opzione_sana() {
        // Nessun filtro di tier: se resta solo un light sano, si usa il light
        // (meglio un sostituto debole che una chiusura secca).
        let p = GovernancePolicy::default();
        let cands = vec![
            cand_failed("openai", "gpt-frontier", Some("frontier")),
            cand("google", "gemini-flash-lite", Some("light")),
        ];
        let r = pick_failover_model(&cands, Some("frontier"), &p).expect("pick");
        assert_eq!(r.model, "gemini-flash-lite");
    }

    #[test]
    fn failover_a_parita_di_punteggio_vince_il_piu_vicino_sopra() {
        // Sopra il corrente l'affinita' non discrimina (nessun boost): a parita'
        // di punteggio decide la distanza (il sostituto sobrio, non il massimo).
        let p = GovernancePolicy::default();
        let cands = vec![
            cand("anthropic", "opus-frontier", Some("frontier")), // dist 3
            cand("deepseek", "v4-pro", Some("high")),             // dist 1
        ];
        let r = pick_failover_model(&cands, Some("medium"), &p).expect("pick");
        assert_eq!(r.model, "v4-pro");
    }

    #[test]
    fn failover_tier_corrente_assente_neutro_e_ordine_preservato() {
        // current_tier None -> medium neutro; a parita' totale vince l'ordine
        // d'ingresso (preferenza del routing: featured/economico).
        let p = GovernancePolicy::default();
        let cands = vec![
            cand("mistral", "primo", Some("medium")),
            cand("deepseek", "secondo", Some("medium")),
        ];
        let r = pick_failover_model(&cands, None, &p).expect("pick");
        assert_eq!(r.model, "primo");
    }

    #[test]
    fn failover_penalty_fuori_range_disattiva_l_indicazione() {
        // penalty invalida (0.0) -> affinita' 1.0 per tutti: nessuna penalita'
        // di downgrade, decide la distanza a parita' di likelihood.
        let p = GovernancePolicy {
            failover_downgrade_penalty: 0.0,
            ..Default::default()
        };
        let cands = vec![
            cand("a", "light-sano", Some("light")),
            cand("b", "high-sano", Some("high")),
        ];
        let r = pick_failover_model(&cands, Some("heavy"), &p).expect("pick");
        assert_eq!(r.model, "high-sano"); // dist 1 < dist 3
    }

    #[test]
    fn failover_insieme_vuoto_ritorna_none() {
        let p = GovernancePolicy::default();
        assert!(pick_failover_model(&[], Some("heavy"), &p).is_none());
    }

    // ---- cap_candidates_one_step (salita di un gradino) ----

    #[test]
    fn cap_un_gradino_scarta_i_tier_troppo_alti() {
        // Corrente medium (rank 2): ammessi fino a high (rank 3). heavy/frontier fuori.
        let cands = vec![
            cand("a", "med", Some("medium")),
            cand("b", "high", Some("high")),
            cand("c", "heavy", Some("heavy")),
            cand("d", "frontier", Some("frontier")),
        ];
        let capped = cap_candidates_one_step(&cands, Some("medium"));
        let models: Vec<&str> = capped.iter().map(|c| c.model.as_str()).collect();
        assert_eq!(models, vec!["med", "high"], "solo <= corrente+1 (high)");
    }

    #[test]
    fn cap_un_gradino_fallback_se_nessuno_entro_il_gradino() {
        // Corrente medium ma l'unico piu' capace e' frontier (2+ gradini): niente
        // candidato entro high -> fallback a TUTTI (meglio saltare che bloccarsi).
        let cands = vec![cand("z", "frontier", Some("frontier"))];
        let capped = cap_candidates_one_step(&cands, Some("medium"));
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].model, "frontier");
    }

    #[test]
    fn cap_un_gradino_pari_tier_altro_provider_ammesso() {
        // Corrente high: pari-tier (high, altro provider) e high+1 (heavy) ammessi.
        let cands = vec![
            cand("p1", "high-alt", Some("high")),
            cand("p2", "heavy", Some("heavy")),
            cand("p3", "frontier", Some("frontier")),
        ];
        let capped = cap_candidates_one_step(&cands, Some("high"));
        let models: Vec<&str> = capped.iter().map(|c| c.model.as_str()).collect();
        assert_eq!(models, vec!["high-alt", "heavy"]);
    }
}

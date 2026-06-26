//! `escalation`: SELEZIONE deterministica del modello di auto-escalation
//! dell'orchestratore. Porting 1:1 di `_pick_escalation_model`
//! (`brain/agents/nodes/helpers.py:1702-1760`), usato sia dalla loop-detection
//! per signature (`brain/agents/nodes/__init__.py:3159-3284`) sia dal cap G1
//! (`__init__.py:1962-1993`): in entrambi i casi, prima di chiudere secco,
//! l'orchestratore PROMUOVE il turno a un modello piu' capace.
//!
//! Punto unico (regola L) della domanda "dato (provider, model) corrente, le
//! escalation gia' fatte, la catena DB e lo stato cooldown, qual e' il prossimo
//! modello eleggibile?". La funzione [`pick_escalation_model`] e' PURA: la catena
//! (`nexus_model_escalation_chain`, mig 0128), l'insieme dei provider in cooldown
//! (gate ADR 0020) e il candidato cross-provider (`loop_fallback_default`)
//! arrivano gia' risolti dal chiamante via [`crate::runtime::ports::EscalationPort`]
//! (nessun IO, nessuna lettura DB qui — regola G).
//!
//! ## Selezione 1:1 col Python (due Tier, in ordine)
//!
//! - **Tier 1 — catena intra-provider** (stesso provider, tier superiore): solo se
//!   `current_provider` e' valorizzato e NON e' in cooldown billing/quota
//!   (escalare sullo stesso provider morto sprecherebbe un turno, incidente
//!   reale). La catena e' gia' filtrata per `(provider, base_model)` e ordinata
//!   per `escalation_position` ASC. Si prende l'elemento all'indice `escalations`
//!   (= numero di escalation gia' fatte, parita' con `LIMIT escalations+1` +
//!   `_rows[escalations]` del Python). Se esiste ed e' DIVERSO dal modello
//!   corrente, e' il risultato.
//! - **Tier 2 — purpose cross-provider** (`loop_fallback_default`): se Tier 1 non
//!   produce, si usa il candidato cross-provider risolto a monte dal router
//!   (gia' filtrato dal gate, sentinelle escluse dal chiamante). Eleggibile se
//!   non e' la coppia `(current_provider, current_model)` corrente.
//! - Altrimenti `None` (catena esaurita / tutto in cooldown / nessun candidato):
//!   il chiamante chiude secco (`loop_detected` / cap G1).

use serde::{Deserialize, Serialize};

/// Una voce della catena di escalation intra-provider
/// (`nexus_model_escalation_chain`, mig 0128) gia' filtrata per `(provider,
/// base_model)` e ordinata per `escalation_position` ASC. Forma MINIMALE: al
/// nodo serve solo il modello di destinazione (provider e base_model sono il
/// contesto del lookup, gia' applicato dall'impl della porta).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainEntry {
    /// Modello di destinazione dell'escalation (`escalation_model`).
    pub escalation_model: String,
}

/// Candidato cross-provider (`loop_fallback_default`) risolto a monte dal router
/// (regola G). `None` quando il purpose non e' configurato o il gate ADR 0020
/// non ha un capable provider (sentinelle gia' escluse dall'impl della porta:
/// `__router_unavailable__` / `__no_capable_provider__` NON arrivano qui).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossProviderCandidate {
    /// Provider del candidato cross-provider.
    pub provider: String,
    /// Modello del candidato cross-provider.
    pub model: String,
}

/// Modello promosso dall'escalation: provider + model con cui RI-ESEGUIRE il
/// turno (signature-loop) o da rendere sticky (cap G1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscalationPick {
    /// Provider del modello promosso.
    pub provider: String,
    /// Modello promosso.
    pub model: String,
    /// `true` se la scelta viene dalla catena intra-provider (Tier 1), `false`
    /// se dal candidato cross-provider (Tier 2). Eco diagnostica (il chiamante
    /// non cambia comportamento in base a questo): utile a log/golden.
    pub from_chain: bool,
}

/// Punto unico (regola L): data la fotografia (catena + corrente + escalation gia'
/// fatte + cooldown + candidato cross-provider), ritorna il prossimo modello
/// eleggibile o `None`. PURA. Replica 1:1 `_pick_escalation_model`.
///
/// - `chain`: catena intra-provider per `(current_provider, current_model)`, gia'
///   ordinata per posizione ASC (la porta applica il filtro + l'ordine + il
///   `is_active`). Vuota se non c'e' catena per la coppia corrente.
/// - `current_provider` / `current_model`: la coppia del turno corrente. `None`
///   (provider) salta il Tier 1 (parita' col `if provider and model` Python).
/// - `escalations`: numero di escalation gia' fatte nel run (`auto_escalations`):
///   indice nella catena (Tier 1) come nel Python (`_rows[escalations]`).
/// - `provider_in_cooldown`: `true` se `current_provider` e' in cooldown billing/
///   quota (gate ADR 0020). Se `true`, Tier 1 e' SALTATO (escalare sullo stesso
///   provider morto sprecherebbe un turno).
/// - `cross_provider`: candidato `loop_fallback_default` (Tier 2), o `None`.
pub fn pick_escalation_model(
    chain: &[ChainEntry],
    current_provider: Option<&str>,
    current_model: Option<&str>,
    escalations: i64,
    provider_in_cooldown: bool,
    cross_provider: Option<&CrossProviderCandidate>,
) -> Option<EscalationPick> {
    // === Tier 1: catena intra-provider (stesso provider, tier superiore) ===
    // Solo se provider+model valorizzati e provider NON in cooldown (parita' col
    // gate Python: `provider.strip().lower() not in cooldown_set`).
    if let (Some(provider), Some(model)) = (
        current_provider.filter(|s| !s.is_empty()),
        current_model.filter(|s| !s.is_empty()),
    ) {
        if !provider_in_cooldown {
            // `_rows[escalations]` con `LIMIT escalations+1`: l'elemento all'indice
            // `escalations` (numero di escalation gia' fatte). `escalations` < 0 e'
            // impossibile in pratica (contatore >= 0); il cast a usize lo tratta
            // come fuori-catena (nessun candidato), coerente col Python.
            if let Ok(idx) = usize::try_from(escalations) {
                if let Some(entry) = chain.get(idx) {
                    if !entry.escalation_model.is_empty() && entry.escalation_model != model {
                        return Some(EscalationPick {
                            provider: provider.to_string(),
                            model: entry.escalation_model.clone(),
                            from_chain: true,
                        });
                    }
                }
            }
        }
    }

    // === Tier 2: purpose model cross-provider dal router (loop_fallback_default) ===
    // Eleggibile se non e' la coppia corrente (parita' col Python: `not (provider
    // == current and model == current)`). Le sentinelle sono gia' escluse dalla
    // porta (non arrivano come `cross_provider`).
    if let Some(cand) = cross_provider {
        let same_as_current = current_provider.map(|p| p == cand.provider).unwrap_or(false)
            && current_model.map(|m| m == cand.model).unwrap_or(false);
        if !same_as_current {
            return Some(EscalationPick {
                provider: cand.provider.clone(),
                model: cand.model.clone(),
                from_chain: false,
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(models: &[&str]) -> Vec<ChainEntry> {
        models
            .iter()
            .map(|m| ChainEntry {
                escalation_model: (*m).to_string(),
            })
            .collect()
    }

    fn cross(p: &str, m: &str) -> CrossProviderCandidate {
        CrossProviderCandidate {
            provider: p.to_string(),
            model: m.to_string(),
        }
    }

    #[test]
    fn tier1_prima_posizione() {
        let c = chain(&["claude-sonnet-4-6", "claude-opus-4-6"]);
        let r = pick_escalation_model(&c, Some("anthropic"), Some("claude-haiku-4-5"), 0, false, None);
        assert_eq!(
            r,
            Some(EscalationPick {
                provider: "anthropic".into(),
                model: "claude-sonnet-4-6".into(),
                from_chain: true,
            })
        );
    }

    #[test]
    fn tier1_indice_segue_escalations() {
        let c = chain(&["claude-sonnet-4-6", "claude-opus-4-6"]);
        // escalations=1 -> seconda posizione.
        let r = pick_escalation_model(&c, Some("anthropic"), Some("claude-haiku-4-5"), 1, false, None);
        assert_eq!(r.unwrap().model, "claude-opus-4-6");
    }

    #[test]
    fn tier1_catena_esaurita_senza_cross_e_none() {
        let c = chain(&["claude-sonnet-4-6"]);
        // escalations=1 -> oltre la catena (len 1), nessun cross -> None.
        let r = pick_escalation_model(&c, Some("anthropic"), Some("claude-haiku-4-5"), 1, false, None);
        assert_eq!(r, None);
    }

    #[test]
    fn tier1_candidato_uguale_al_corrente_salta() {
        // La catena ritorna lo stesso modello corrente -> Tier 1 NON eleggibile,
        // cade al cross-provider.
        let c = chain(&["claude-haiku-4-5"]);
        let cand = cross("google", "gemini-2.5-pro");
        let r = pick_escalation_model(
            &c,
            Some("anthropic"),
            Some("claude-haiku-4-5"),
            0,
            false,
            Some(&cand),
        );
        assert_eq!(
            r,
            Some(EscalationPick {
                provider: "google".into(),
                model: "gemini-2.5-pro".into(),
                from_chain: false,
            })
        );
    }

    #[test]
    fn provider_in_cooldown_salta_tier1_va_a_cross() {
        let c = chain(&["claude-sonnet-4-6"]);
        let cand = cross("openai", "gpt-4.1");
        // provider_in_cooldown=true -> Tier 1 saltato anche se la catena avrebbe
        // un candidato; si usa il cross-provider.
        let r = pick_escalation_model(
            &c,
            Some("anthropic"),
            Some("claude-haiku-4-5"),
            0,
            true,
            Some(&cand),
        );
        assert_eq!(r.unwrap().provider, "openai");
    }

    #[test]
    fn provider_in_cooldown_senza_cross_e_none() {
        let c = chain(&["claude-sonnet-4-6"]);
        let r = pick_escalation_model(&c, Some("anthropic"), Some("claude-haiku-4-5"), 0, true, None);
        assert_eq!(r, None);
    }

    #[test]
    fn nessun_provider_corrente_salta_tier1() {
        // provider None -> Tier 1 saltato (parita' `if provider and model`),
        // si usa il cross-provider.
        let c = chain(&["claude-sonnet-4-6"]);
        let cand = cross("mistral", "mistral-large-2411");
        let r = pick_escalation_model(&c, None, None, 0, false, Some(&cand));
        assert_eq!(r.unwrap().provider, "mistral");
    }

    #[test]
    fn cross_uguale_al_corrente_e_none() {
        // Il cross-provider coincide con la coppia corrente -> non eleggibile.
        let cand = cross("anthropic", "claude-haiku-4-5");
        let r = pick_escalation_model(
            &[],
            Some("anthropic"),
            Some("claude-haiku-4-5"),
            0,
            false,
            Some(&cand),
        );
        assert_eq!(r, None);
    }

    #[test]
    fn catena_vuota_usa_cross() {
        let cand = cross("google", "gemini-2.5-flash");
        let r = pick_escalation_model(&[], Some("openai"), Some("gpt-4o-mini"), 0, false, Some(&cand));
        assert_eq!(r.unwrap().model, "gemini-2.5-flash");
    }

    #[test]
    fn tutto_assente_e_none() {
        let r = pick_escalation_model(&[], Some("openai"), Some("gpt-4o-mini"), 0, false, None);
        assert_eq!(r, None);
    }
}

/// Golden di parita' 1:1 vs Python per `pick_escalation_model`. Carica
/// `/tmp/golden_escalation.json` (vedi `gen_golden_escalation.py`).
#[cfg(test)]
mod golden {
    use super::*;
    use serde::Deserialize;
    use serde_json::{json, Value};

    #[derive(Debug, Deserialize)]
    struct In {
        chain: Vec<String>,
        current_provider: Option<String>,
        current_model: Option<String>,
        escalations: i64,
        provider_in_cooldown: bool,
        cross_provider: Option<[String; 2]>,
    }

    #[derive(Debug, Deserialize)]
    struct GoldenCase {
        group: String,
        case_id: String,
        input: In,
        output: Value,
    }

    #[test]
    #[ignore = "richiede /tmp/golden_escalation.json generato da gen_golden_escalation.py"]
    fn golden_escalation() {
        let Some(raw) =
            crate::golden_util::load_golden("golden_escalation.json", "gen_golden_escalation.py")
        else {
            return;
        };
        let cases: Vec<GoldenCase> = serde_json::from_str(&raw).expect("golden JSON malformato");
        assert!(cases.len() >= 10, "attesi >= 10 casi, trovati {}", cases.len());
        for c in &cases {
            assert_eq!(c.group, "pick_escalation_model");
            let chain: Vec<ChainEntry> = c
                .input
                .chain
                .iter()
                .map(|m| ChainEntry {
                    escalation_model: m.clone(),
                })
                .collect();
            let cand = c.input.cross_provider.as_ref().map(|pm| CrossProviderCandidate {
                provider: pm[0].clone(),
                model: pm[1].clone(),
            });
            let r = pick_escalation_model(
                &chain,
                c.input.current_provider.as_deref(),
                c.input.current_model.as_deref(),
                c.input.escalations,
                c.input.provider_in_cooldown,
                cand.as_ref(),
            );
            // Output Python: [provider, model] o null (from_chain non e' osservabile
            // nel ritorno Python (tuple[str,str]|None), quindi NON entra nel golden).
            let got = match &r {
                Some(p) => json!([p.provider, p.model]),
                None => Value::Null,
            };
            assert_eq!(
                got, c.output,
                "PARITA' FALLITA pick_escalation_model / {}:\n  rust   = {}\n  python = {}",
                c.case_id, got, c.output
            );
        }
        println!("golden escalation: {} casi verificati, tutti verdi", cases.len());
    }
}

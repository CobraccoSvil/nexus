//! `tiers`: PUNTO UNICO (regola L) del VOCABOLARIO dei performance-tier dei modelli.
//!
//! Scala di CAPACITA' a 5 livelli, dal meno al piu' capace:
//! `light < medium < high < heavy < frontier` — allineata a
//! `ai_price_catalog.performance_tier` (mig 0528) e ai CHECK vivi (mig 0547).
//!
//! Prima di questo modulo il vocabolario era re-elencato in piu' punti Rust (due
//! `tier_rank` con scale diverse, i validatori `agentic_min_tier` e
//! `normalize_tier`): un 6o livello avrebbe richiesto di toccarli tutti (odore
//! regola L). Ora l'ORDINAMENTO e la VALIDAZIONE del vocabolario vivono qui; i
//! call site DELEGANO. Un nuovo livello si aggiunge in UN solo posto.
//!
//! NB: le CHECK SQL (mig 0547) e le opzioni del frontend sono SPECCHI di
//! [`PERFORMANCE_TIERS`] (non condivisibili col Rust): vanno tenute allineate a
//! mano quando la scala cambia.
//!
//! NON e' un `speed_tier` (fast/medium/slow) ne' una `ContextPressure`/complexity
//! (low/medium/high): quelli sono concetti diversi con un vocabolario proprio.

/// Vocabolario canonico dei performance-tier, ordinato dal meno al piu' capace.
/// L'indice nell'array e' il rank 0-based (`light`=0 ... `frontier`=4).
pub const PERFORMANCE_TIERS: [&str; 5] = ["light", "medium", "high", "heavy", "frontier"];

/// Rank di capacita' `1..=5` (piu' alto = piu' capace). Case-insensitive e
/// tollerante agli spazi. Un valore sconosciuto/assente -> `2` (`medium` neutro:
/// non penalizza ne' premia), preservando il comportamento storico dei due
/// `tier_rank` che ora delegano qui.
pub fn tier_rank(tier: &str) -> u8 {
    match tier.trim().to_ascii_lowercase().as_str() {
        "light" => 1,
        "medium" => 2,
        "high" => 3,
        "heavy" => 4,
        "frontier" => 5,
        _ => 2,
    }
}

/// `true` se `tier` (case-insensitive, spazi tollerati) e' uno dei 5 livelli
/// canonici. Punto unico per la VALIDAZIONE del vocabolario (usato dai validatori
/// admin e dal pavimento agentico in mcp-core).
pub fn is_performance_tier(tier: &str) -> bool {
    let t = tier.trim().to_ascii_lowercase();
    PERFORMANCE_TIERS.contains(&t.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_monotono_sulla_scala_a_5() {
        let ranks: Vec<u8> = PERFORMANCE_TIERS.iter().map(|t| tier_rank(t)).collect();
        assert_eq!(ranks, vec![1, 2, 3, 4, 5], "ordine light<medium<high<heavy<frontier");
        // Strettamente crescente.
        assert!(ranks.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn rank_sconosciuto_e_medium_neutro() {
        assert_eq!(tier_rank(""), 2);
        assert_eq!(tier_rank("ultra"), 2);
        assert_eq!(tier_rank("chat"), 2);
    }

    #[test]
    fn rank_case_insensitive_e_trim() {
        assert_eq!(tier_rank("  HEAVY "), tier_rank("heavy"));
        assert_eq!(tier_rank("Frontier"), 5);
    }

    #[test]
    fn is_performance_tier_riconosce_i_5_e_rifiuta_gli_altri() {
        for t in PERFORMANCE_TIERS {
            assert!(is_performance_tier(t));
            assert!(is_performance_tier(&t.to_ascii_uppercase()));
        }
        assert!(is_performance_tier("  medium "));
        assert!(!is_performance_tier(""));
        assert!(!is_performance_tier("ultra"));
        assert!(!is_performance_tier("static"));
        // speed_tier / context_pressure NON sono performance_tier.
        assert!(!is_performance_tier("fast"));
        assert!(!is_performance_tier("low"));
    }
}

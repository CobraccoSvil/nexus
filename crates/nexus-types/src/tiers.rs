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
//! Perche' vive in `nexus-types` e non piu' in `nexus-agent-graph`. Il
//! vocabolario nasceva accanto al suo primo chiamante (l'escalation del grafo),
//! ma un vocabolario non appartiene al motore agentico: lo usano anche i
//! validatori admin (`admin-service`, che NON puo' dipendere dal grafo — crate
//! pesante: nexus-graph, tokio-util, sqlx checkpointer). Un punto unico
//! raggiungibile solo da meta' dei suoi chiamanti smette di essere unico alla
//! prima riga di codice che non puo' importarlo. `nexus-types` e' il crate leaf
//! gia' dipendenza di tutti. `nexus_agent_graph::decisions::tiers` resta come
//! re-export: i call site storici non cambiano una riga.
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

/// La catena dei tier da `from` IN SU, dal meno al piu' capace (`from` incluso).
/// GENERATA da [`PERFORMANCE_TIERS`]: nessuna scala scritta a mano.
///
/// E' la gemella ascendente della degradazione, per i casi in cui scendere non
/// e' un ripiego ma un danno. Il caso reale (misurato 2026-07-16): le figure del
/// consiglio chiedono `medium`; con openai e anthropic in cooldown per credito
/// esaurito il tier restava vuoto e la catena discendente arrivava a `light`,
/// dove il piu' economico e' `groq/gpt-oss-20b` (agentic_index 3.1, il peggiore
/// del parco). Quel run non falliva: rispondeva FUORI TEMA e si dichiarava
/// `completed`. Per un consigliere un modello piu' caro e' un costo; uno che
/// mente e' un danno.
///
/// Un `from` fuori vocabolario prende il rank neutro di [`tier_rank`]
/// (`medium`), come ovunque: la catena parte da li' in su.
pub fn tier_chain_up(from: &str) -> Vec<&'static str> {
    let soglia = tier_rank(from);
    PERFORMANCE_TIERS
        .iter()
        .filter(|t| tier_rank(t) >= soglia)
        .copied()
        .collect()
}

/// Rank del tier come espressione SQL, GENERATA da [`PERFORMANCE_TIERS`] e
/// [`tier_rank`]: la scala ha UN solo posto anche per chi ordina in SQL.
///
/// Perche' esiste (incidente 2026-07-15). La scala viveva solo in Rust, quindi
/// ogni query che voleva ordinare per capacita' era COSTRETTA a riscriverla a
/// mano: 9 copie (2 in stringhe SQL Rust, 6 nelle migrazioni, 1 qui). Con nove
/// copie manuali la divergenza non e' sfortuna, e' questione di tempo — e infatti
/// `agent_run.rs` aveva
///   `CASE performance_tier WHEN 'heavy' THEN 2 WHEN 'medium' THEN 1 ELSE 0 END`
/// cioe' una scala a 3 livelli sopravvissuta al passaggio a 5 (mig 0528):
/// `frontier` e `high` collassavano a 0 come `light`, e l'escalation "sali al
/// modello piu' capace" scartava tutti e 7 i frontier (misurato: sceglieva
/// gpt-5.4-pro/heavy invece di gpt-5.5-pro/frontier) e preferiva un `medium`
/// (1) a un `frontier` (0). Nessun compilatore poteva vederlo: e' una stringa.
///
/// `col` e' il nome della colonna/espressione da classificare (es.
/// `"performance_tier"`, `"cat.performance_tier"`): NON e' input utente, i call
/// site passano un letterale. Un tier NULL o fuori vocabolario prende lo stesso
/// rank neutro di [`tier_rank`] (`medium`), cosi' le due implementazioni
/// coincidono per costruzione; l'invariante e' verificata contro Postgres da
/// `tier_rank_sql_coincide_col_rank_rust` (mcp-core).
pub fn tier_rank_sql(col: &str) -> String {
    let rami: String = PERFORMANCE_TIERS
        .iter()
        .map(|t| format!(" WHEN '{t}' THEN {}", tier_rank(t)))
        .collect();
    // ELSE = rank del valore ignoto: preso da tier_rank stesso (nessun numero
    // scritto a mano qui, altrimenti sarebbe la decima copia).
    format!(
        "(CASE lower(trim({col})){rami} ELSE {} END)",
        tier_rank("__ignoto__")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_monotono_sulla_scala_a_5() {
        let ranks: Vec<u8> = PERFORMANCE_TIERS.iter().map(|t| tier_rank(t)).collect();
        assert_eq!(
            ranks,
            vec![1, 2, 3, 4, 5],
            "ordine light<medium<high<heavy<frontier"
        );
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
    fn la_catena_ascendente_parte_dal_tier_e_sale() {
        assert_eq!(tier_chain_up("medium"), vec!["medium", "high", "heavy", "frontier"]);
        assert_eq!(tier_chain_up("heavy"), vec!["heavy", "frontier"]);
        // Il piu' capace non ha dove salire: resta solo se stesso (la catena non
        // e' mai vuota, quindi il bersaglio viene sempre provato).
        assert_eq!(tier_chain_up("frontier"), vec!["frontier"]);
        assert_eq!(tier_chain_up("light"), PERFORMANCE_TIERS.to_vec());
        // MAI verso il basso: e' l'unica proprieta' che conta, ed e' il motivo
        // per cui la catena esiste (un consigliere 'light' che mente e' peggio
        // di uno 'heavy' che costa).
        for t in PERFORMANCE_TIERS {
            assert!(
                tier_chain_up(t).iter().all(|c| tier_rank(c) >= tier_rank(t)),
                "la catena ascendente di {t} contiene un tier piu' debole"
            );
        }
        // Fuori vocabolario: parte dal neutro (medium), come tier_rank.
        assert_eq!(tier_chain_up("boh"), tier_chain_up("medium"));
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

#[cfg(test)]
mod tier_rank_sql_tests {
    use super::*;

    /// L'SQL generato copre TUTTI i livelli del vocabolario: aggiungerne uno a
    /// PERFORMANCE_TIERS lo porta nell'SQL senza toccare altro (era il difetto:
    /// la scala SQL restava indietro rispetto a quella Rust).
    #[test]
    fn sql_generato_copre_tutto_il_vocabolario() {
        let sql = tier_rank_sql("performance_tier");
        for t in PERFORMANCE_TIERS {
            assert!(
                sql.contains(&format!("WHEN '{t}' THEN {}", tier_rank(t))),
                "il livello '{t}' manca nell'SQL generato: {sql}"
            );
        }
        // Il ramo ELSE usa il rank neutro di tier_rank, non un numero a mano.
        assert!(
            sql.ends_with(&format!("ELSE {} END)", tier_rank("__ignoto__"))),
            "ELSE deve venire da tier_rank: {sql}"
        );
        // La colonna passata dal call site finisce dentro, normalizzata come in
        // tier_rank (lower+trim): stessa tolleranza nelle due implementazioni.
        assert!(sql.contains("lower(trim(performance_tier))"));
    }

    /// La scala a 3 livelli che viveva in agent_run.rs NON deve poter tornare:
    /// frontier e high devono stare SOPRA medium, non collassare a zero.
    #[test]
    fn frontier_e_high_non_collassano_su_light() {
        assert!(tier_rank("frontier") > tier_rank("heavy"));
        assert!(tier_rank("heavy") > tier_rank("high"));
        assert!(tier_rank("high") > tier_rank("medium"));
        assert!(tier_rank("medium") > tier_rank("light"));
        // Il difetto misurato: col CASE rotto frontier valeva 0 e medium 1.
        assert!(
            tier_rank("frontier") > tier_rank("medium"),
            "un frontier non puo' MAI ordinarsi sotto un medium"
        );
    }
}

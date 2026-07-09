//! `governance`: PUNTO UNICO (regola L) della scelta a RUNTIME tra candidati
//! modello/provider GIA' AMMISSIBILI, ordinati per PROBABILITA' di successo
//! derivata da TELEMETRIA STRUTTURATA (regola M: mai prosa, mai testo LLM).
//!
//! Scope DISGIUNTO dal meta-reasoner LLM (`recover`/`orchestrate`/`assess_scale`):
//! qui NON si consulta alcun LLM. La scelta e' DETERMINISTICA e replay-stabile,
//! calcolata da segnali strutturati gia' raccolti a monte (esiti recenti del
//! `model_health_probe`, contatori del catalog, stato cooldown). Gemello puro di
//! [`crate::decisions::escalation`]: la SELEZIONE vive qui, l'I/O (lettura
//! `ai_model_health_history` + `ai_price_catalog` + snapshot cooldown) sta
//! nell'impl della porta in mcp-core.
//!
//! Il ROUTING BASE resta DB-driven (regola G): questo modulo NON sceglie la
//! config ne' inventa modelli. RIORDINA una lista di candidati che il routing ha
//! GIA' selezionato come ammissibili (tier/capability/tool-use/cooldown gia'
//! applicati dal punto unico `select_agentic_model`).
//!
//! ## Retro-compatibilita' (vincolo primario)
//!
//! L'ordinamento e' STABILE (`slice::sort_by` e' stabile): a parita' di punteggio
//! l'ordine di USCITA coincide con l'ordine di INGRESSO. Con telemetria assente o
//! uniforme (nessun segnale distintivo) l'output == input == comportamento fisso
//! attuale. Il chiamante applica il riordino SOLO a flag ON (regola G): a flag OFF
//! il riordino non viene neppure invocato -> comportamento bit-identico.
//!
//! ## Segnali (regola M, tutti strutturati)
//!
//! - `recent_failures / recent_checks`: error-rate recente dallo storico probe
//!   (`ai_model_health_history`), sotto `min_recent_checks` il segnale e' troppo
//!   rumoroso e viene ignorato (no penalita' da rate).
//! - `consecutive_failures` / `consecutive_tool_failures`: contatori del catalog
//!   (`ai_price_catalog`, mig 0172/0269) — fallimenti model-specific / tool.
//! - `last_error_kind`: categoria d'errore STRUTTURATA (`error_kind`, popolata dal
//!   probe con la nomenclatura di `provider_error_classifier`). La distinzione
//!   model-specific vs provider-wide segue la stessa semantica del probe: un errore
//!   provider-wide (billing/quota/rate_limit) NON e' colpa del modello e non lo
//!   penalizza (regola M: la causa e' strutturata, non dedotta dal testo).
//! - `provider_in_cooldown`: gate ADR 0020 (snapshot in-memory). Segnale forte:
//!   un provider in cooldown viene RETROCESSO ma non cancellato (fail-safe).

use std::collections::HashMap;

/// Segnale di TELEMETRIA strutturato per un `(provider, model)` (regola M).
/// Risolto dall'impl della porta in mcp-core dai segnali gia' raccolti dai worker
/// (`model_health_probe`) e dal catalog; qui e' un input PURO, nessun I/O.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct ModelTelemetry {
    /// Provider del modello (chiave, lowercase per convenzione del chiamante).
    pub provider: String,
    /// Modello (chiave).
    pub model: String,
    /// Numero di check recenti considerati (finestra `ai_model_health_history`).
    /// `0` = nessuno storico -> error-rate ignoto (nessuna penalita' da rate).
    pub recent_checks: u32,
    /// Quanti dei `recent_checks` erano falliti (`healthy = FALSE`).
    pub recent_failures: u32,
    /// Latenza media recente in ms (`0` = ignota). Solo TIE-BREAKER: non esclude,
    /// separa candidati altrimenti a pari punteggio.
    pub avg_latency_ms: i64,
    /// Fallimenti consecutivi model-specific dal catalog (`consecutive_failures`).
    pub consecutive_failures: i64,
    /// Fallimenti consecutivi su turni con tool (`consecutive_tool_failures`).
    pub consecutive_tool_failures: i64,
    /// Categoria d'errore piu' recente (STRUTTURATA, non prosa). `None` se sano o
    /// ignoto. Le categorie provider-wide (billing/quota/rate_limit) NON penalizzano
    /// il modello (regola M: non e' colpa sua).
    pub last_error_kind: Option<String>,
    /// Provider attualmente in cooldown (gate ADR 0020). Retrocede fortemente.
    pub provider_in_cooldown: bool,
}

impl ModelTelemetry {
    /// Chiave canonica `(provider, model)` per il match nella mappa di telemetria.
    /// Case-insensitive sul provider (convenzione del progetto), model esatto.
    pub fn key(provider: &str, model: &str) -> (String, String) {
        (
            provider.trim().to_ascii_lowercase(),
            model.trim().to_string(),
        )
    }
}

/// Soglie DETERMINISTICHE della governance (regola G: i valori arrivano dai
/// settings risolti a monte dal chiamante, qui e' solo il contratto). NON sono
/// magic fallback su un comportamento (non accendono feature ne' scelgono
/// modelli): sono soglie di calcolo puro, come le soglie di `context_pressure`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GovernancePolicy {
    /// Error-rate recente (`recent_failures / recent_checks`) oltre cui il
    /// candidato e' "recently_failed" e viene RETROCESSO. Valutato solo se
    /// `recent_checks >= min_recent_checks`.
    pub exclude_error_rate: f64,
    /// `consecutive_failures` (o tool) oltre cui il candidato e' "recently_failed"
    /// e viene RETROCESSO, a prescindere dall'error-rate recente.
    pub exclude_consecutive_failures: i64,
    /// Numero minimo di check recenti perche' l'error-rate sia considerato
    /// affidabile. Sotto questa soglia lo storico e' troppo rumoroso.
    pub min_recent_checks: u32,
    /// Latenza (ms) di riferimento per la penalita' di latenza (tie-breaker):
    /// `avg_latency_ms / latency_ref_ms` scala la penalita' [0, cap].
    pub latency_ref_ms: i64,
    /// Affinita' di tier nel FAILOVER (`pick_failover_model`): moltiplicatore
    /// applicato per OGNI livello di tier SOTTO quello corrente (`penalty^delta`).
    /// Il tier corrente e' un'INDICAZIONE, mai un filtro: un candidato piu'
    /// debole ma con likelihood nettamente migliore puo' comunque vincere.
    /// Range valido (0, 1]; 1.0 = indicazione disattivata.
    pub failover_downgrade_penalty: f64,
}

impl Default for GovernancePolicy {
    /// Default SICURI (non un magic fallback su un modello: sono soglie di calcolo,
    /// regola G/M). Coerenti con la soglia di auto-disable del probe (3): retrocede
    /// gia' a 2 fallimenti consecutivi, PRIMA che il probe disabiliti il modello.
    fn default() -> Self {
        Self {
            exclude_error_rate: 0.5,
            exclude_consecutive_failures: 2,
            min_recent_checks: 2,
            latency_ref_ms: 20_000,
            failover_downgrade_penalty: 0.85,
        }
    }
}

/// Categorie d'errore PROVIDER-WIDE (regola M, semantica del `model_health_probe`
/// e di `reason_is_billing`/`provider_error_classifier`): NON sono colpa del
/// modello, quindi non lo penalizzano nel punteggio (retriable a livello provider,
/// gestite dal cooldown/gate, non dalla governance del modello). Match su
/// sottostringa della categoria strutturata (non del testo umano dell'errore).
fn is_provider_wide_error(kind: &str) -> bool {
    let k = kind.trim().to_ascii_lowercase();
    k.contains("billing")
        || k.contains("quota")
        || k.contains("rate_limit")
        || k.contains("rate limit")
        || k.contains("overload")
        || k.contains("service_unavailable")
        || k.contains("credit")
        || k.contains("balance")
}

/// `true` se il candidato e' "recently_failed" secondo la policy (error-rate
/// recente oltre soglia con storico affidabile, OPPURE fallimenti consecutivi
/// oltre soglia, OPPURE provider in cooldown). I "recently_failed" vengono
/// RETROCESSI (non cancellati) dal ranking: il chiamante che prende il TOP ottiene
/// cosi' un candidato sano se esiste, ma non resta mai a mani vuote (fail-safe).
pub fn is_recently_failed(t: &ModelTelemetry, policy: &GovernancePolicy) -> bool {
    if t.provider_in_cooldown {
        return true;
    }
    if t.consecutive_failures >= policy.exclude_consecutive_failures
        || t.consecutive_tool_failures >= policy.exclude_consecutive_failures
    {
        return true;
    }
    if t.recent_checks >= policy.min_recent_checks {
        let rate = t.recent_failures as f64 / t.recent_checks as f64;
        if rate >= policy.exclude_error_rate {
            return true;
        }
    }
    false
}

/// Punteggio DETERMINISTICO di probabilita' di successo in `[0, 1]` (piu' alto =
/// piu' probabile). Funzione PURA e monotona: piu' fallimenti/error-rate/cooldown
/// abbassano il punteggio, la latenza e' solo un tie-breaker fine. Con telemetria
/// vuota (`ModelTelemetry::default`) ritorna `1.0` -> nessuna distinzione ->
/// ordine invariato (retro-compat).
pub fn likelihood_score(t: &ModelTelemetry, policy: &GovernancePolicy) -> f64 {
    let mut score = 1.0_f64;

    // Cooldown provider: segnale piu' forte (retrocessione netta), ma non azzera
    // (fail-safe: se TUTTI in cooldown, un ordine relativo resta utile).
    if t.provider_in_cooldown {
        score *= 0.05;
    }

    // Error-rate recente (solo con storico affidabile).
    if t.recent_checks >= policy.min_recent_checks {
        let rate = (t.recent_failures as f64 / t.recent_checks as f64).clamp(0.0, 1.0);
        // 0 fail -> *1.0 ; tutti fail -> *0.1 (non 0: fail-safe).
        score *= 1.0 - 0.9 * rate;
    }

    // Fallimenti consecutivi (catalog): decadimento 1/(1+n). Sommo i due contatori
    // (model-specific + tool) perche' entrambi predicono un fallimento del turno.
    let cons = (t.consecutive_failures.max(0) + t.consecutive_tool_failures.max(0)) as f64;
    if cons > 0.0 {
        score *= 1.0 / (1.0 + 0.5 * cons);
    }

    // Ultimo errore MODEL-SPECIFIC (non provider-wide): penalita' aggiuntiva. Un
    // errore provider-wide (billing/quota/rate_limit) NON penalizza il modello.
    if let Some(kind) = t.last_error_kind.as_deref() {
        if !kind.trim().is_empty() && !is_provider_wide_error(kind) {
            score *= 0.7;
        }
    }

    // Latenza: tie-breaker fine (penalita' piccola e cappata a 0.1 del punteggio).
    if t.avg_latency_ms > 0 && policy.latency_ref_ms > 0 {
        let ratio = (t.avg_latency_ms as f64 / policy.latency_ref_ms as f64).clamp(0.0, 1.0);
        score *= 1.0 - 0.1 * ratio;
    }

    score.clamp(0.0, 1.0)
}

/// PUNTO UNICO (regola L) del riordino telemetria-aware di candidati AMMISSIBILI.
///
/// - `candidates`: coppie `(provider, model)` gia' selezionate dal routing, in
///   ordine di preferenza attuale (es. catena escalation per rank, o catalog per
///   featured/costo). L'ordine in ingresso e' l'ancora di retro-compat.
/// - `telemetry`: telemetria per (un sottoinsieme di) i candidati. I candidati
///   senza telemetria ricevono `ModelTelemetry::default` (punteggio 1.0, non
///   penalizzati): l'assenza di segnale non e' un segnale negativo.
/// - `exclude`: coppie da RIMUOVERE del tutto (hard drop): tipicamente il modello
///   corrente + quelli gia' provati in questo run. Case-insensitive sul provider.
/// - `policy`: soglie DB-driven (regola G).
///
/// Ritorna i candidati (meno gli `exclude`) ORDINATI: prima i non-"recently_failed"
/// per punteggio DESC, poi i "recently_failed" per punteggio DESC (retrocessi ma
/// non persi: fail-safe). L'ordinamento e' STABILE: a parita' di bucket+punteggio
/// l'ordine d'ingresso e' preservato -> con telemetria uniforme l'output == input.
pub fn rank_candidates(
    candidates: &[(String, String)],
    telemetry: &[ModelTelemetry],
    exclude: &[(String, String)],
    policy: &GovernancePolicy,
) -> Vec<(String, String)> {
    // Mappa telemetria per chiave canonica.
    let tmap: HashMap<(String, String), &ModelTelemetry> = telemetry
        .iter()
        .map(|t| (ModelTelemetry::key(&t.provider, &t.model), t))
        .collect();
    // Insieme di esclusione per chiave canonica.
    let excl: std::collections::HashSet<(String, String)> = exclude
        .iter()
        .map(|(p, m)| ModelTelemetry::key(p, m))
        .collect();

    let default_t = ModelTelemetry::default();
    // Arricchisco ogni candidato col suo bucket (0 = sano, 1 = recently_failed) e
    // punteggio, preservando l'indice d'ingresso per la stabilita'.
    let mut enriched: Vec<(usize, &(String, String), u8, f64)> = candidates
        .iter()
        .enumerate()
        .filter(|(_, (p, m))| !excl.contains(&ModelTelemetry::key(p, m)))
        .map(|(i, pm)| {
            let t = tmap
                .get(&ModelTelemetry::key(&pm.0, &pm.1))
                .copied()
                .unwrap_or(&default_t);
            let bucket = u8::from(is_recently_failed(t, policy));
            let score = likelihood_score(t, policy);
            (i, pm, bucket, score)
        })
        .collect();

    // Ordina: bucket ASC (sani prima), poi punteggio DESC. `sort_by` e' STABILE:
    // a parita' di (bucket, punteggio) l'ordine d'ingresso (indice) e' preservato.
    enriched.sort_by(|a, b| {
        a.2.cmp(&b.2)
            .then(b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
    });

    enriched
        .into_iter()
        .map(|(_, pm, _, _)| pm.clone())
        .collect()
}

/// GOVERNANCE costo/beneficio del ROLLING-SUMMARY (decisione trasversale, regola
/// M: segnale strutturato, non prosa). Il rolling-summary costa una chiamata LLM
/// economica per riassumere il prefisso vecchio della conversazione: se il
/// prefisso da riassumere e' PICCOLO, il beneficio (contesto risparmiato) non
/// giustifica il costo -> `false` (si salta il summary questo giro). Funzione PURA.
///
/// - `prefix_len`: numero di messaggi del prefisso che verrebbero riassunti
///   (cutoff gia' calcolato dal punto unico `select_rolling_summary_cutoff`; e' il
///   proxy di BENEFICIO).
/// - `min_prefix_len`: soglia minima DB-driven (regola G) sotto cui si salta.
///   Clampata ad almeno 1 (un prefisso di 0 non e' mai da riassumere).
///
/// Il chiamante applica questo gate SOLO col sub-flag di governance ON: a flag OFF
/// il comportamento resta quello storico (`select_rolling_summary_cutoff` decide da
/// solo) -> bit-identico.
pub fn rolling_summary_worthwhile(prefix_len: i64, min_prefix_len: i64) -> bool {
    prefix_len >= min_prefix_len.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tel(provider: &str, model: &str) -> ModelTelemetry {
        ModelTelemetry {
            provider: provider.to_string(),
            model: model.to_string(),
            ..Default::default()
        }
    }

    fn pm(provider: &str, model: &str) -> (String, String) {
        (provider.to_string(), model.to_string())
    }

    // ── likelihood_score ────────────────────────────────────────────────────

    #[test]
    fn score_telemetria_vuota_e_uno() {
        // Nessun segnale -> 1.0 (nessuna distinzione, retro-compat).
        let p = GovernancePolicy::default();
        assert_eq!(likelihood_score(&ModelTelemetry::default(), &p), 1.0);
    }

    #[test]
    fn score_error_rate_abbassa_il_punteggio() {
        let p = GovernancePolicy::default();
        let mut t = tel("google", "gemini");
        t.recent_checks = 4;
        t.recent_failures = 4; // 100% fail
        let s_bad = likelihood_score(&t, &p);
        t.recent_failures = 0; // 0% fail
        let s_good = likelihood_score(&t, &p);
        assert!(s_good > s_bad);
        // Tutti fail non azzera (fail-safe): resta > 0.
        assert!(s_bad > 0.0);
    }

    #[test]
    fn score_error_rate_ignorato_sotto_min_checks() {
        // 1 solo check: rumoroso -> nessuna penalita' da rate (default min = 2).
        let p = GovernancePolicy::default();
        let mut t = tel("google", "gemini");
        t.recent_checks = 1;
        t.recent_failures = 1;
        assert_eq!(likelihood_score(&t, &p), 1.0);
    }

    #[test]
    fn score_consecutive_failures_penalizza() {
        let p = GovernancePolicy::default();
        let mut t = tel("deepseek", "chat");
        t.consecutive_failures = 3;
        assert!(likelihood_score(&t, &p) < 1.0);
        // Tool failures penalizzano allo stesso modo.
        let mut t2 = tel("deepseek", "chat");
        t2.consecutive_tool_failures = 3;
        assert!(likelihood_score(&t2, &p) < 1.0);
    }

    #[test]
    fn score_errore_provider_wide_non_penalizza_il_modello() {
        // billing/quota/rate_limit: colpa del provider, non del modello (regola M).
        let p = GovernancePolicy::default();
        let mut t = tel("anthropic", "claude");
        t.last_error_kind = Some("billing_error".into());
        assert_eq!(likelihood_score(&t, &p), 1.0);
        let mut t2 = tel("google", "gemini");
        t2.last_error_kind = Some("rate_limit".into());
        assert_eq!(likelihood_score(&t2, &p), 1.0);
    }

    #[test]
    fn score_errore_model_specific_penalizza() {
        let p = GovernancePolicy::default();
        let mut t = tel("google", "gemini-3.5");
        t.last_error_kind = Some("model_not_found".into());
        assert!(likelihood_score(&t, &p) < 1.0);
        let mut t2 = tel("google", "gemini-2.5-pro");
        t2.last_error_kind = Some("hollow_completion".into());
        assert!(likelihood_score(&t2, &p) < 1.0);
    }

    #[test]
    fn score_cooldown_retrocede_forte() {
        let p = GovernancePolicy::default();
        let mut t = tel("openai", "gpt");
        t.provider_in_cooldown = true;
        assert!(likelihood_score(&t, &p) < 0.1);
    }

    #[test]
    fn score_latenza_solo_tie_breaker() {
        // Due modelli identici salvo latenza: il piu' lento ha punteggio <= ma di poco.
        let p = GovernancePolicy::default();
        let mut slow = tel("a", "m");
        slow.avg_latency_ms = 20_000;
        let fast = tel("b", "n"); // 0 = ignota, nessuna penalita'
        let s_slow = likelihood_score(&slow, &p);
        let s_fast = likelihood_score(&fast, &p);
        assert!(s_fast >= s_slow);
        // Penalita' piccola: entro il 10%.
        assert!(s_slow >= 0.9);
    }

    // ── is_recently_failed ──────────────────────────────────────────────────

    #[test]
    fn recently_failed_su_error_rate_alto() {
        let p = GovernancePolicy::default();
        let mut t = tel("google", "gemini");
        t.recent_checks = 4;
        t.recent_failures = 3; // 75% >= 0.5
        assert!(is_recently_failed(&t, &p));
    }

    #[test]
    fn recently_failed_su_consecutive() {
        let p = GovernancePolicy::default();
        let mut t = tel("deepseek", "chat");
        t.consecutive_failures = 2; // >= 2
        assert!(is_recently_failed(&t, &p));
    }

    #[test]
    fn recently_failed_su_cooldown() {
        let p = GovernancePolicy::default();
        let mut t = tel("openai", "gpt");
        t.provider_in_cooldown = true;
        assert!(is_recently_failed(&t, &p));
    }

    #[test]
    fn non_recently_failed_se_sano() {
        let p = GovernancePolicy::default();
        assert!(!is_recently_failed(&ModelTelemetry::default(), &p));
    }

    // ── rank_candidates ─────────────────────────────────────────────────────

    #[test]
    fn rank_telemetria_vuota_preserva_ordine() {
        // Nessuna telemetria -> ordine invariato (retro-compat, bit-identico).
        let p = GovernancePolicy::default();
        let cands = vec![pm("a", "1"), pm("b", "2"), pm("c", "3")];
        let out = rank_candidates(&cands, &[], &[], &p);
        assert_eq!(out, cands);
    }

    #[test]
    fn rank_esclude_le_coppie_in_exclude() {
        let p = GovernancePolicy::default();
        let cands = vec![pm("a", "1"), pm("b", "2"), pm("c", "3")];
        let out = rank_candidates(&cands, &[], &[pm("b", "2")], &p);
        assert_eq!(out, vec![pm("a", "1"), pm("c", "3")]);
    }

    #[test]
    fn rank_exclude_case_insensitive_sul_provider() {
        let p = GovernancePolicy::default();
        let cands = vec![pm("Anthropic", "claude"), pm("google", "gemini")];
        let out = rank_candidates(&cands, &[], &[pm("anthropic", "claude")], &p);
        assert_eq!(out, vec![pm("google", "gemini")]);
    }

    #[test]
    fn rank_promuove_il_candidato_sano_sopra_il_fallito() {
        // 'a' e' primo in ingresso ma ha error-rate alto; 'b' e' sano -> 'b' sale.
        let p = GovernancePolicy::default();
        let cands = vec![pm("a", "1"), pm("b", "2")];
        let mut ta = tel("a", "1");
        ta.recent_checks = 4;
        ta.recent_failures = 4;
        let tb = tel("b", "2"); // sano
        let out = rank_candidates(&cands, &[ta, tb], &[], &p);
        assert_eq!(out, vec![pm("b", "2"), pm("a", "1")]);
    }

    #[test]
    fn rank_retrocede_non_cancella_i_falliti() {
        // Entrambi falliti: nessuno viene perso, restano in ordine di punteggio.
        let p = GovernancePolicy::default();
        let cands = vec![pm("a", "1"), pm("b", "2")];
        let mut ta = tel("a", "1");
        ta.consecutive_failures = 5; // peggiore
        let mut tb = tel("b", "2");
        tb.consecutive_failures = 2; // meno peggio
        let out = rank_candidates(&cands, &[ta, tb], &[], &p);
        // b (meno peggio) prima di a, ma entrambi presenti.
        assert_eq!(out, vec![pm("b", "2"), pm("a", "1")]);
    }

    #[test]
    fn rank_sano_batte_sempre_il_fallito_anche_se_dopo_in_ingresso() {
        // Un sano in coda all'ingresso supera un fallito in testa (bucket).
        let p = GovernancePolicy::default();
        let cands = vec![pm("fallito", "x"), pm("sano", "y")];
        let mut tf = tel("fallito", "x");
        tf.provider_in_cooldown = true;
        let ts = tel("sano", "y");
        let out = rank_candidates(&cands, &[tf, ts], &[], &p);
        assert_eq!(out[0], pm("sano", "y"));
    }

    #[test]
    fn rank_stabile_a_parita_di_punteggio() {
        // Tre candidati tutti sani (punteggio 1.0): ordine d'ingresso preservato.
        let p = GovernancePolicy::default();
        let cands = vec![pm("a", "1"), pm("b", "2"), pm("c", "3")];
        let tele = vec![tel("a", "1"), tel("b", "2"), tel("c", "3")];
        let out = rank_candidates(&cands, &tele, &[], &p);
        assert_eq!(out, cands);
    }

    // ── rolling_summary_worthwhile ─────────────────────────────────────────

    #[test]
    fn rolling_summary_worthwhile_gate_su_prefisso() {
        // Prefisso >= soglia -> vale la pena; sotto -> si salta.
        assert!(rolling_summary_worthwhile(6, 6));
        assert!(rolling_summary_worthwhile(10, 6));
        assert!(!rolling_summary_worthwhile(5, 6));
        // Soglia <= 0 clampata a 1: un prefisso 0 non e' mai da riassumere.
        assert!(!rolling_summary_worthwhile(0, 0));
        assert!(rolling_summary_worthwhile(1, 0));
    }

    #[test]
    fn rank_candidato_senza_telemetria_non_e_penalizzato() {
        // 'b' non ha telemetria -> default (sano). 'a' e' fallito -> 'b' primo.
        let p = GovernancePolicy::default();
        let cands = vec![pm("a", "1"), pm("b", "2")];
        let mut ta = tel("a", "1");
        ta.consecutive_failures = 5;
        let out = rank_candidates(&cands, &[ta], &[], &p);
        assert_eq!(out, vec![pm("b", "2"), pm("a", "1")]);
    }
}

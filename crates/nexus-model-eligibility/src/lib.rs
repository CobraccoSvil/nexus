//! ELEGGIBILITA' della batteria di qualificazione: punto unico (regola L) della
//! domanda "questo modello va misurato ADESSO?".
//!
//! Il claim di produzione (`mcp-core::model_qualification::claim_candidates`) e
//! l'explain diagnostico (`xtask battery-explain`) NON hanno due query: hanno le
//! stesse [`CONDITIONS`], rese in SQL da [`where_clause`]. La non-divergenza non
//! e' una convenzione da ricordare, e' una costruzione: non esiste un modo di
//! cambiare la regola per uno solo dei due.
//!
//! Root cause (2026-07-17, regola O): durante una diagnosi uno script aveva
//! RICOPIATO a mano la query del claim, leggendo la suite dalla tabella sbagliata
//! (`ai_price_catalog` invece di `ai_model_probe_profile`). Riportava "0 candidati
//! eleggibili" mentre erano 29. Non ha fallito: ha MENTITO con la faccia seria, e
//! la diagnosi e' partita da un fatto inesistente. Il guard
//! `diagnostica-non-imita` (scripts/check-single-source.sh) vieta la copia; questo
//! crate e' la strada che rende il divieto praticabile — chi diagnostica PONE la
//! domanda al sistema invece di riscriverla.
//!
//! Il crate compone SQL e non lo esegue: e' senza dipendenze e i suoi test non
//! hanno bisogno di un DB.

/// Chiavi settings della batteria (mig 0591/0593, regola G: la config sta nel DB).
/// Vivono qui perche' la premessa dell'explain ("il giro e' acceso? quanti ne
/// prende per volta?") deve leggere le STESSE chiavi che legge il worker.
pub const KEY_ROUND_ENABLED: &str = "agent.model_qualification.round_enabled";
pub const KEY_MAX_PER_ROUND: &str = "agent.model_qualification.max_models_per_round";
pub const KEY_TTL_DAYS: &str = "agent.model_qualification.requalify_ttl_days";
pub const KEY_BACKOFF_HOURS: &str = "agent.model_qualification.backoff_hours";
/// Minuti di backoff FISSO dopo un round NON MISURANTE (mig 0720): il giro che
/// non ha potuto guardare il modello (fornitore in cooldown, inconclusivi tutti
/// del fornitore) non e' un tentativo — niente attempts+1, niente esponenziale.
pub const KEY_NOT_MEASURING_BACKOFF_MINUTES: &str =
    "agent.model_qualification.not_measuring_backoff_minutes";

/// Default dei settings sopra, quando la chiave manca dal DB. Sono i valori che
/// il worker usa davvero: l'explain deve dichiarare il numero VERO del giro, non
/// un suo simile.
pub const DEFAULT_MAX_PER_ROUND: i64 = 4;
pub const DEFAULT_NOT_MEASURING_BACKOFF_MINUTES: i64 = 60;

/// Lock `probing` stantio: oltre questa eta' il claim e' di un worker morto e la
/// riga torna reclamabile.
pub const STALE_PROBING_MINUTES: i64 = 15;

/// La fonte della SUITE: `ai_model_probe_profile`, i profili ENABLED. E' la
/// premessa che lo script diagnostico aveva sbagliato — qui e' una sola stringa,
/// e chi carica i profili e chi legge la versione corrente la condividono.
///
/// E' un macro e non una `const` perche' i chiamanti la CONCATENANO alle loro
/// colonne: cosi' la query resta un letterale composto a compile-time
/// (`concat!`), invece di essere costruita a runtime con `format!`. Una query
/// che non si costruisce non si puo' costruire male.
#[macro_export]
macro_rules! profile_source {
    () => {
        "FROM ai_model_probe_profile WHERE enabled = TRUE"
    };
}

/// La fonte dei profili, per chi la vuole come valore invece che come letterale.
pub const SQL_PROFILE_SOURCE: &str = profile_source!();

/// Le versioni di suite dei profili attivi. La riduzione a UN numero e'
/// [`current_suite_version`].
pub const SQL_PROFILE_SUITE_VERSIONS: &str = concat!("SELECT suite_version ", profile_source!());

/// La versione corrente della suite: il MAX dei profili enabled. Un catalogo
/// senza profili vale 1 (nessuna suite = nessuna ri-qualificazione forzata).
pub fn current_suite_version(versions: impl IntoIterator<Item = i32>) -> i32 {
    versions.into_iter().max().unwrap_or(1)
}

/// Una condizione dell'eleggibilita': il suo SQL e il PERCHE' esclude quando e'
/// falsa. La spiegazione sta accanto alla condizione, non in una tabella di
/// traduzione a parte: una spiegazione che vive altrove smette di descrivere il
/// codice al primo cambio.
pub struct EligibilityCondition {
    /// Identificatore canonico (regola N). E' anche l'alias della colonna
    /// booleana nella query dell'explain: il lettore non re-inventa i nomi.
    pub name: &'static str,
    /// Espressione booleana SQL sull'alias `p` (= `ai_price_catalog`). I
    /// segnaposto `{stale_minutes}` e `{suite_version}` li rende [`render`].
    pub sql: &'static str,
    /// Cosa dire a un umano quando questa condizione e' l'ostacolo.
    pub perche_esclude: &'static str,
}

/// LA regola. Aggiungere/togliere/cambiare una condizione QUI la cambia insieme
/// per il claim e per l'explain: e' l'unico punto in cui l'eleggibilita' esiste.
pub const CONDITIONS: &[EligibilityCondition] = &[
    EligibilityCondition {
        name: "catalog_abilitato",
        sql: "p.is_enabled = TRUE",
        perche_esclude: "disabilitato nel catalog (is_enabled = FALSE)",
    },
    EligibilityCondition {
        name: "dichiara_tool_use",
        sql: "p.supports_tool_use = TRUE",
        perche_esclude: "il catalog non dichiara supports_tool_use: la batteria \
                         agentica non ha nulla da provare",
    },
    EligibilityCondition {
        name: "backoff_scaduto",
        sql: "(p.qualification_backoff_until IS NULL \
               OR p.qualification_backoff_until < NOW())",
        perche_esclude: "backoff attivo dopo un giro fallito o inconclusivo \
                         (qualification_backoff_until nel futuro)",
    },
    EligibilityCondition {
        name: "lock_libero",
        sql: "(p.qualification_started_at IS NULL \
               OR p.qualification_started_at < NOW() - make_interval(mins => {stale_minutes}::int))",
        perche_esclude: "gia' claimato da un altro worker da meno di \
                         STALE_PROBING_MINUTES (qualification_started_at recente)",
    },
    EligibilityCondition {
        name: "da_misurare",
        sql: "(p.qualification_state IN ('unqualified','quarantined','probing') \
               OR (p.qualification_state = 'qualified' \
                   AND (p.qualification_expires_at < NOW() \
                        OR p.qualification_suite_version < {suite_version})))",
        perche_esclude: "gia' qualified, TTL non scaduto (qualification_expires_at \
                         nel futuro) e suite gia' alla versione corrente: non c'e' \
                         nulla da rimisurare",
    },
    // Il filtro sul COOLDOWN sta QUI, non solo a valle: `qualify_claimed`
    // controlla `is_provider_in_cooldown` col commento "non sprecare il giro", ma
    // a quel punto il giro e' gia' speso — il claim ha consumato uno dei
    // `max_per_round` posti per un modello che verra' scartato in 10 millisecondi.
    //
    // Misurato il 2026-07-16: due giri consecutivi hanno reclamato 8 modelli,
    // TUTTI di openai/anthropic (in cooldown per `credit_balance_too_low`), e li
    // hanno buttati tutti. Non e' sfortuna: quei due provider sono 76 modelli su
    // 116 e l'ORDER BY per scadenza li pesca quasi sempre. A 4 per giro ogni 30
    // minuti servivano ~9 ore per smaltirli prima di toccare i 34 modelli
    // misurabili — cioe' il "tier dai fatti" non avrebbe misurato nulla per
    // un'intera giornata, con la batteria che girava e sembrava sana.
    //
    // La fonte e' `nexus_provider_health.billing_cooldown_until` (il cooldown
    // lungo PERSISTENTE, ADR 0020/0030). Il cooldown BREVE non sta qui: vive in
    // memoria di mcp-core e arriva come BIND del chiamante (condizione
    // `fuori_cooldown_breve`, sotto).
    EligibilityCondition {
        name: "provider_senza_cooldown",
        sql: "NOT EXISTS (SELECT 1 FROM nexus_provider_health h \
                           WHERE h.provider = p.provider \
                             AND h.billing_cooldown_until > NOW())",
        perche_esclude: "il provider e' in cooldown di billing \
                         (nexus_provider_health.billing_cooldown_until nel futuro): \
                         il giro sarebbe sprecato",
    },
    // Il cooldown BREVE (registro in-process per-coppia di mcp-core,
    // `provider_cooldown::ChiaveCooldown`, 13/08/2026). Non e' interrogabile da
    // SQL: la REGOLA («un fornitore o una coppia dichiarati esclusi non si
    // misurano») sta qui, i FATTI (chi e' escluso ADESSO) sono bind del
    // chiamante. Il claim di produzione binda il registro vivo; l'explain —
    // processo separato, che quella memoria non la vede — binda array vuoti e
    // DICHIARA la premessa (regola O: un numero senza premessa e' un'opinione).
    //
    // `lower()` sul lato catalogo perche' il registro normalizza a lowercase nei
    // costruttori di `ChiaveCooldown`; la concatenazione `provider || '/' ||
    // model` e' la convenzione di `ChiaveCooldown::etichetta()` — il ponte fra
    // le due lo copre un test sqlx in mcp-core.
    EligibilityCondition {
        name: "fuori_cooldown_breve",
        sql: "(NOT (lower(p.provider) = ANY({cooldown_providers})) \
               AND NOT (lower(p.provider || '/' || p.model) = ANY({cooldown_pairs})))",
        perche_esclude: "il fornitore (o questa coppia) e' nel registro cooldown \
            in-process di mcp-core: il giro sarebbe speso contro un \
            fornitore che rifiutera'",
    },
];

/// L'ordine di PRIORITA' del giro: prima chi non e' mai stato misurato, poi per
/// scadenza. Lo condividono claim ed explain, cosi' "chi e' eleggibile adesso"
/// elenca nell'ordine in cui il giro li prendera' davvero.
pub const ORDER_BY: &str =
    "(p.qualification_state = 'unqualified') DESC, p.qualification_expires_at ASC NULLS FIRST";

/// I segnaposto delle condizioni parametriche, resi coi bind del chiamante:
/// il claim numera da `$2` in poi (dopo il LIMIT `$1`), l'explain da `$1`.
fn render(
    sql: &str,
    stale_minutes: &str,
    suite_version: &str,
    cooldown_providers: &str,
    cooldown_pairs: &str,
) -> String {
    sql.replace("{stale_minutes}", stale_minutes)
        .replace("{suite_version}", suite_version)
        .replace("{cooldown_providers}", cooldown_providers)
        .replace("{cooldown_pairs}", cooldown_pairs)
}

/// Le [`CONDITIONS`] in AND: la clausola che decide l'eleggibilita'. E' il punto
/// unico che il claim e l'explain chiamano ENTRAMBI. `cooldown_providers` e
/// `cooldown_pairs` sono i bind `text[]` delle esclusioni del registro breve
/// in-process (lowercase; le coppie nella convenzione `provider/model` di
/// `ChiaveCooldown::etichetta`): array vuoti = nessuna esclusione.
pub fn where_clause(
    stale_minutes: &str,
    suite_version: &str,
    cooldown_providers: &str,
    cooldown_pairs: &str,
) -> String {
    CONDITIONS
        .iter()
        .map(|c| render(c.sql, stale_minutes, suite_version, cooldown_providers, cooldown_pairs))
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// I bind del claim: `$1` = quanti per giro, `$2` = minuti di lock stantio,
/// `$3` = versione corrente della suite, `$4`/`$5` = esclusioni del registro
/// cooldown breve (fornitori interi / coppie `provider/model`).
pub const CLAIM_LIMIT_PARAM: &str = "$1";
pub const CLAIM_STALE_PARAM: &str = "$2";
pub const CLAIM_SUITE_PARAM: &str = "$3";
pub const CLAIM_COOLDOWN_PROVIDERS_PARAM: &str = "$4";
pub const CLAIM_COOLDOWN_PAIRS_PARAM: &str = "$5";

/// I bind dell'explain: `$1` = lock stantio, `$2` = suite, `$3`/`$4` =
/// esclusioni del registro breve, `$5` = modello cercato (solo
/// [`sql_explain_model`]).
///
/// Il modello e' l'ULTIMO e non il terzo, e non e' estetica: la query degli
/// eleggibili non lo referenzia, e Postgres rifiuta al PREPARE una statement
/// coi parametri non contigui («could not determine data type of parameter
/// $3») — col modello a `$3`, `sql_explain_eligible` referenzierebbe
/// `$1,$2,$4,$5` con un buco.
pub const EXPLAIN_STALE_PARAM: &str = "$1";
pub const EXPLAIN_SUITE_PARAM: &str = "$2";
pub const EXPLAIN_COOLDOWN_PROVIDERS_PARAM: &str = "$3";
pub const EXPLAIN_COOLDOWN_PAIRS_PARAM: &str = "$4";
pub const EXPLAIN_MODEL_PARAM: &str = "$5";

/// Il CLAIM di produzione (CAS). I candidati gia' `qualified` (scaduti o con
/// suite vecchia) sono ri-provati IN SHADOW: lo state resta `qualified` (il pool
/// non si svuota durante la ri-qualificazione); il lock del claim e'
/// `qualification_started_at`.
pub fn sql_claim() -> String {
    format!(
        "UPDATE ai_price_catalog c SET \
         qualification_state = CASE WHEN c.qualification_state = 'qualified' \
                                    THEN 'qualified' ELSE 'probing' END, \
         qualification_started_at = NOW() \
     FROM ( \
         SELECT p.provider, p.model FROM ai_price_catalog p \
          WHERE {where_} \
          ORDER BY {ORDER_BY} \
          LIMIT {CLAIM_LIMIT_PARAM} \
          FOR UPDATE SKIP LOCKED \
     ) cand \
     WHERE c.provider = cand.provider AND c.model = cand.model \
     RETURNING c.provider, c.model, c.capabilities",
        where_ = where_clause(
            CLAIM_STALE_PARAM,
            CLAIM_SUITE_PARAM,
            CLAIM_COOLDOWN_PROVIDERS_PARAM,
            CLAIM_COOLDOWN_PAIRS_PARAM,
        ),
    )
}

/// Chi e' eleggibile ADESSO, nell'ordine del giro. Stessa clausola del claim,
/// senza UPDATE e senza lock: l'explain OSSERVA, non consuma il giro.
pub fn sql_explain_eligible() -> String {
    format!(
        "SELECT p.provider, p.model FROM ai_price_catalog p WHERE {where_} ORDER BY {ORDER_BY}",
        where_ = where_clause(
            EXPLAIN_STALE_PARAM,
            EXPLAIN_SUITE_PARAM,
            EXPLAIN_COOLDOWN_PROVIDERS_PARAM,
            EXPLAIN_COOLDOWN_PAIRS_PARAM,
        ),
    )
}

/// PERCHE' un modello e' eleggibile o no: ogni condizione valutata come booleano,
/// con l'alias del suo `name`. Le colonne le genera la stessa lista che compone
/// il claim, quindi una condizione nuova compare qui senza che nessuno se ne
/// ricordi. Il match e' su `model` oppure `provider/model`.
pub fn sql_explain_model() -> String {
    let colonne: Vec<String> = CONDITIONS
        .iter()
        .map(|c| {
            format!(
                "({}) AS {}",
                render(
                    c.sql,
                    EXPLAIN_STALE_PARAM,
                    EXPLAIN_SUITE_PARAM,
                    EXPLAIN_COOLDOWN_PROVIDERS_PARAM,
                    EXPLAIN_COOLDOWN_PAIRS_PARAM,
                ),
                c.name
            )
        })
        .collect();
    // I timestamp escono come testo: l'explain li STAMPA e basta, e il casting
    // qui gli risparmia un binding di tipi data solo per rileggerli.
    // Tier, fonte e SCORE MISURATO (mig 0615) escono insieme allo stato: senza,
    // la scala relativa e' un numero opaco che l'explain non sa mostrare.
    format!(
        "SELECT p.provider, p.model, p.qualification_state, p.qualification_suite_version, \
                p.qualification_expires_at::text AS qualification_expires_at, \
                p.qualification_backoff_until::text AS qualification_backoff_until, \
                p.qualification_started_at::text AS qualification_started_at, \
                p.performance_tier, p.tier_source, \
                p.measured_score, p.measured_score_suite, \
                p.measured_score_at::text AS measured_score_at, {colonne} \
           FROM ai_price_catalog p \
          WHERE p.model = {EXPLAIN_MODEL_PARAM} \
             OR p.provider || '/' || p.model = {EXPLAIN_MODEL_PARAM} \
          ORDER BY p.provider, p.model",
        colonne = colonne.join(", "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// IL test del cantiere: il claim e l'explain non possono divergere perche'
    /// la clausola del claim E' `where_clause`, alla lettera.
    ///
    /// Fallisce se qualcuno cambia la regola in UN posto solo: sia scrivendo una
    /// condizione a mano dentro `sql_claim` (la WHERE non coincide piu' con la
    /// clausola condivisa), sia togliendone una dalla lista (idem). E' la
    /// proprieta' che rende onesto l'explain: senza, tornerebbe a essere una
    /// copia — cioe' esattamente il difetto che deve prevenire.
    /// La WHERE effettiva di una query: cio' che sta tra la prima `WHERE` e la
    /// prima `ORDER BY`. Estrarla dalla stringa FINITA e' il punto: verifica la
    /// query che parte davvero, non l'intenzione di chi l'ha composta.
    fn where_effettiva(sql: &str) -> String {
        sql.split_once(" WHERE ")
            .and_then(|(_, coda)| coda.split_once(" ORDER BY "))
            .map(|(w, _)| w.trim().to_string())
            .expect("la query ha una WHERE seguita da ORDER BY")
    }

    #[test]
    fn la_where_del_claim_e_quella_dell_explain_sono_la_stessa_clausola() {
        // Il claim: la sua WHERE e' la clausola condivisa, alla lettera. Fallisce
        // se qualcuno scrive una condizione a mano dentro `sql_claim` — la mossa
        // naturale di chi ha fretta ("aggiungo il filtro dove sto guardando").
        assert_eq!(
            where_effettiva(&sql_claim()),
            where_clause(
                CLAIM_STALE_PARAM,
                CLAIM_SUITE_PARAM,
                CLAIM_COOLDOWN_PROVIDERS_PARAM,
                CLAIM_COOLDOWN_PAIRS_PARAM,
            ),
            "REGRESSIONE: la WHERE del claim non e' piu' la clausola condivisa. \
             L'explain risponderebbe su una regola che la produzione non usa: \
             tornerebbe a MENTIRE con la faccia seria (regola O)."
        );
        // E l'explain, simmetricamente: una condizione in piu' QUI e' altrettanto
        // grave — direbbe 'non eleggibile' di un modello che il giro prendera'.
        assert_eq!(
            where_effettiva(&sql_explain_eligible()),
            where_clause(
                EXPLAIN_STALE_PARAM,
                EXPLAIN_SUITE_PARAM,
                EXPLAIN_COOLDOWN_PROVIDERS_PARAM,
                EXPLAIN_COOLDOWN_PAIRS_PARAM,
            ),
            "REGRESSIONE: l'explain filtra su una regola sua, diversa dal claim."
        );
    }

    /// Il registro cooldown BREVE entra nella clausola CONDIVISA come coppia di
    /// bind: fornitori interi E coppie, entrambe confrontate lowercase.
    ///
    /// MUTAZIONE che lo fa rosseggiare: togliere `fuori_cooldown_breve` da
    /// `CONDITIONS` (il claim tornerebbe a spendere i posti del giro contro un
    /// fornitore saturo, e l'unico presidio resterebbe il check a valle che
    /// conta lo skip come tentativo), oppure rilassare la condizione a
    /// solo-provider (la coppia — il caso groq/gpt-oss del 13/08 — non sarebbe
    /// piu' vista), oppure togliere `lower()` dal lato catalogo.
    #[test]
    fn la_condizione_cooldown_breve_e_nella_clausola_condivisa() {
        let claim = sql_claim();
        assert!(
            claim.contains(&format!("lower(p.provider) = ANY({CLAIM_COOLDOWN_PROVIDERS_PARAM})")),
            "il claim non binda piu' i fornitori esclusi del registro breve: {claim}"
        );
        assert!(
            claim.contains(&format!(
                "lower(p.provider || '/' || p.model) = ANY({CLAIM_COOLDOWN_PAIRS_PARAM})"
            )),
            "il claim non binda piu' le coppie escluse del registro breve: {claim}"
        );
        // La condizione e' della LISTA, non scritta a mano nel claim: cosi'
        // l'explain la eredita (test delle colonne) e la where-uguaglianza vale.
        assert!(
            CONDITIONS.iter().any(|c| c.name == "fuori_cooldown_breve"),
            "la condizione deve stare in CONDITIONS, non bolted-on su sql_claim"
        );
    }

    /// Ogni condizione della regola e' OSSERVABILE dall'explain: se ne aggiungi
    /// una al claim, l'explain sa gia' dire che e' lei a escludere. Senza questo,
    /// un modello potrebbe risultare non-eleggibile con tutte le condizioni note
    /// verdi — una diagnosi che si ferma a "boh".
    #[test]
    fn ogni_condizione_del_claim_ha_la_sua_colonna_nell_explain() {
        let per_modello = sql_explain_model();
        for c in CONDITIONS {
            assert!(
                per_modello.contains(&format!(" AS {}", c.name)),
                "la condizione '{}' non e' osservabile nell'explain",
                c.name
            );
            assert!(
                !c.perche_esclude.trim().is_empty(),
                "la condizione '{}' esclude senza dire perche'",
                c.name
            );
        }
    }

    /// I segnaposto sono RESI: un `{suite_version}` che sopravvive nella query e'
    /// un errore di sintassi che Postgres scoprirebbe solo a runtime, cioe'
    /// durante l'incidente in cui stai usando l'explain.
    #[test]
    fn nessun_segnaposto_sopravvive_nelle_query_finite() {
        for (nome, sql) in [
            ("claim", sql_claim()),
            ("explain_eligible", sql_explain_eligible()),
            ("explain_model", sql_explain_model()),
        ] {
            assert!(
                !sql.contains('{') && !sql.contains('}'),
                "segnaposto non reso in {nome}: {sql}"
            );
        }
    }

    /// Il claim resta un CAS che non svuota il pool: chi e' `qualified` resta
    /// `qualified` mentre lo si rimisura in shadow.
    #[test]
    fn il_claim_rimisura_in_shadow_senza_svuotare_il_pool() {
        let claim = sql_claim();
        assert!(claim.contains("THEN 'qualified' ELSE 'probing' END"));
        assert!(claim.contains("FOR UPDATE SKIP LOCKED"), "il claim resta concorrente");
        assert!(
            claim.contains("RETURNING c.provider, c.model, c.capabilities"),
            "il claim ritorna cio' che il giro consuma"
        );
    }

    /// L'explain OSSERVA: se scrivesse, guardarlo cambierebbe cio' che guarda —
    /// e in mano a chi diagnostica un incidente consumerebbe i posti del giro.
    #[test]
    fn l_explain_non_scrive_e_non_blocca() {
        for sql in [sql_explain_eligible(), sql_explain_model()] {
            assert!(sql.starts_with("SELECT "), "l'explain e' una lettura: {sql}");
            assert!(!sql.contains("UPDATE"), "l'explain non scrive: {sql}");
            assert!(!sql.contains("FOR UPDATE"), "l'explain non blocca righe: {sql}");
        }
    }

    /// La premessa dell'explain viene dalla tabella dei PROFILI. E' l'errore
    /// esatto dello script del 2026-07-17: la suite letta da `ai_price_catalog`
    /// dava "0 candidati" mentre erano 29.
    #[test]
    fn la_suite_si_legge_dai_profili_non_dal_catalogo() {
        let sql = SQL_PROFILE_SUITE_VERSIONS;
        assert!(sql.contains("ai_model_probe_profile"), "la suite sta nei profili: {sql}");
        assert!(
            !sql.contains("ai_price_catalog"),
            "il catalogo NON e' la fonte della suite: {sql}"
        );
        assert!(sql.contains("enabled = TRUE"), "solo i profili attivi: {sql}");
    }

    /// La versione corrente e' il MAX dei profili attivi: un profilo vecchio
    /// rimasto acceso non deve abbassare la suite (ri-qualificherebbe tutto).
    #[test]
    fn la_suite_corrente_e_il_max_dei_profili_attivi() {
        assert_eq!(current_suite_version([3, 2, 3]), 3);
        assert_eq!(current_suite_version([]), 1, "nessun profilo: suite 1");
    }
}

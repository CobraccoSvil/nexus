//! Adapter contabile del gateway: dai TIPI del gateway al punto unico.
//!
//! Convenzione Nexus: `tenant_id = project_id` (UUID).
//!
//! Regola L. Qui non vive piu' ne' il LISTINO (crate `nexus-pricing`: quanto
//! costa un modello) ne' la CONTABILITA' (crate `nexus-ledger`: quale riga si
//! scrive, quanto ha consumato uno scope). Resta cio' che e' davvero del
//! gateway, e che nessun altro puo' fare al posto suo: estrarre dai propri tipi
//! (`LlmRequest`, `LlmResponse`) l'identita' del chiamante, i token e la stima
//! del consumo, e tradurre l'esito nel proprio confine HTTP.
//!
//! La differenza conta. Le domande "quale riga scrivo?" e "quanto ha gia'
//! consumato questo scope?" sono le stesse per chiunque le ponga, e infatti le
//! due copie — questa e quella di mcp-core — erano tenute gemelle a mano e
//! divergevano gia' (vedi il doc del crate `nexus-ledger`). La domanda "come si
//! stimano i token di QUESTA richiesta" invece dipende dai tipi di chi chiede, e
//! resta qui.
//!
//! Regola F: nessun prompt/response/segreto nei log; solo importi/conteggi.

use anyhow::Result;
use sqlx::PgPool;

use nexus_pricing::TokenUsage;

use crate::types::{LedgerOutcome, LlmRequest, LlmResponse, MessageContent};

// Vocabolario contabile dal punto unico. Ri-esportato perche' i quattro handler
// media e i confini HTTP lo nominano: `routes.rs` fa `downcast_ref::<QuotaExceeded>()`
// e costruisce `MediaUsage`, e deve vedere gli STESSI tipi che il ledger scrive.
pub use nexus_ledger::{MediaKind, MediaUsage, QuantitySource, QuotaExceeded};

/// Stima i token di input dai messaggi (char/4, parita' col server.ts).
pub fn estimate_prompt_tokens(req: &LlmRequest) -> i64 {
    let chars: usize = req
        .messages
        .iter()
        .map(|m| match &m.content {
            MessageContent::Text(t) => t.len(),
            // I blocchi non-testo non contribuiscono alla stima char-based.
            MessageContent::Blocks(_) => 0,
        })
        .sum();
    // Ceil division per 4 (parita' con Math.ceil(chars/4) del server.ts).
    ((chars as i64) + 3) / 4
}

/// Come si stima il consumo ai fini della quota.
///
/// Esiste perche' la stima a token ha senso solo per le chiamate testuali. Sulle
/// modalita' media produce numeri senza rapporto col consumo: il prompt di
/// un'immagine diviso quattro, e per la trascrizione addirittura zero (il
/// messaggio user della richiesta sintetica e' vuoto). Finche' quelle chiamate
/// non avevano identita' la quota era comunque no-op e la cosa non si vedeva;
/// dal momento in cui l'identita' c'e', ereditare quella stima significherebbe
/// consumare la quota-token di un progetto con un numero inventato — e nel caso
/// peggiore rifiutare una richiesta per un motivo falso.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaEstimate {
    /// Chiamata testuale: token stimati da messaggi e `max_tokens`.
    Testuale,
    /// Modalita' non-testuale: nessun token stimato. Resta attiva la quota di
    /// COSTO, che diventa efficace appena `ai_price_catalog_unit` e' popolata.
    NonTestuale,
}

/// L'identita' contabile della richiesta, o `None` se non e' utilizzabile.
///
/// Cio' che e' del gateway e' sapere DOVE stanno le due stringhe nei propri
/// tipi; la REGOLA che decide se sono utilizzabili sta nel punto unico
/// (`nexus_ledger::identity_from_metadata`, regola L), perche' se la pone anche
/// chi ha prenotato dall'altro lato del wire — sulla richiesta che ha MANDATO —
/// per sapere se un "non ho scritto" e' legittimo o sospetto. Con la regola
/// ricopiata qui, quel confronto direbbe soltanto che il gateway e' d'accordo
/// con se stesso.
///
/// `None` non e' un errore: e' il caso reale delle chiamate interne
/// (`GwMetadata::default`), dove non c'e' nessuno a cui addebitare.
fn identita(req: &LlmRequest) -> Option<nexus_ledger::Identity> {
    nexus_ledger::identity_from_metadata(&req.metadata.tenant_id, &req.metadata.user_id)
}

/// Enforce quota PRIMA della chiamata al provider (guardrail).
///
/// No-op se mancano `tenant_id`/`user_id` (parita' server.ts): senza identita'
/// non c'e' scope a cui applicare un vincolo. La decisione — quali quote sono
/// attive, quanto e' stato consumato, quale limite e' superato — la prende il
/// punto unico, la stessa che usa chi prenota.
pub async fn enforce_quota(
    db: &PgPool,
    req: &LlmRequest,
    provider: &str,
    model: &str,
    stima: QuotaEstimate,
) -> Result<()> {
    let Some(identity) = identita(req) else {
        return Ok(());
    };

    let (prompt_tokens, completion_tokens) = match stima {
        QuotaEstimate::Testuale => (
            estimate_prompt_tokens(req),
            req.max_tokens.map(|t| t as i64).unwrap_or(0),
        ),
        // Nessun token: queste chiamate non ne consumano, e fingere il contrario
        // eroderebbe la quota-token del progetto con un numero arbitrario.
        QuotaEstimate::NonTestuale => (0, 0),
    };

    nexus_ledger::check_quota(db, identity, provider, model, prompt_tokens, completion_tokens).await
}

/// Registra l'usage effettivo nel ledger come `finalized` (parita' con
/// `recordUsageToLedger`). Best-effort: gli errori sono loggati dal punto unico
/// ma non interrompono la risposta al chiamante.
///
/// RITORNA cosa e' stato fatto ([`LedgerOutcome`]), sempre e in forma
/// strutturata. Non e' telemetria: e' il segnale (regola M) su cui il chiamante
/// decide se addebitare a sua volta. I tre esiti non sono deducibili dall'esito
/// della chiamata LLM, che e' RIUSCITA in tutti e tre:
///   - `NoIdentity`: `tenant_id`/`user_id` assenti o non-UUID. E' il caso di
///     `NeuralCoreClient::generate_completion`, che manda `GwMetadata::default`;
///   - `WriteFailed`: la INSERT e' fallita (DB);
///   - `Written`: la riga c'e', e porta lei l'addebito.
///
/// Nei primi due il chiamante DEVE finalizzare la propria prenotazione, o
/// l'addebito si perde del tutto; nel terzo deve rilasciarla, o si paga due
/// volte. Sono decisioni opposte prese sullo stesso "la chiamata e' riuscita":
/// ecco perche' la risposta non si deduce, si dichiara.
pub async fn record_usage_to_ledger(
    db: &PgPool,
    req: &LlmRequest,
    resp: &LlmResponse,
) -> LedgerOutcome {
    let Some(identity) = identita(req) else {
        return LedgerOutcome::NoIdentity;
    };
    match nexus_ledger::record_tokens(
        db,
        identity,
        &resp.provider_used,
        &resp.model_used,
        &token_usage_from(&resp.usage),
        &req.metadata.request_id,
        &req.metadata.feature,
    )
    .await
    {
        Some(entry) => LedgerOutcome::Written(entry),
        None => LedgerOutcome::WriteFailed,
    }
}

/// Scrive il consumo e lo DICHIARA sulla risposta che sta per partire.
///
/// Le due cose stanno in una funzione sola perche' separarle E' il difetto: per
/// tutta la vita del gateway la riga si scriveva e la dichiarazione non
/// esisteva, cosi' chi aveva prenotato finalizzava anche lei e la chiamata
/// veniva addebitata due volte (incidente 2026-07-27). Un solo punto (regola L)
/// che le tiene insieme rende impossibile aggiungere una scrittura muta.
///
/// E' anche l'unico modo di TESTARE la pubblicazione: la riga
/// `response.ledger = ...` viveva dentro la pipeline HTTP completa — provider
/// reali, routing, redazione — dove nessun test poteva arrivarci, ed era proprio
/// la riga su cui poggia l'intero fix.
pub async fn record_and_declare(db: &PgPool, req: &LlmRequest, resp: &mut LlmResponse) {
    let outcome = record_usage_to_ledger(db, req, resp).await;
    resp.ledger = Some(outcome);
}

/// Registra il consumo di una chiamata non-testuale.
///
/// Il fallimento dell'identita' si DICE: quando scatta, un consumo reale resta
/// fuori dalla contabilita', e il silenzio e' esattamente il motivo per cui il
/// buco e' rimasto aperto tanto a lungo (fino alla mig 0634 queste chiamate non
/// producevano NESSUNA riga).
pub async fn record_media_usage_to_ledger(
    db: &PgPool,
    req: &LlmRequest,
    provider_used: &str,
    model_used: &str,
    usage: MediaUsage,
) {
    let Some(identity) = identita(req) else {
        tracing::warn!(
            kind = usage.kind.as_str(),
            "gateway-ledger: consumo media NON registrato, identita' assente o non-UUID nei metadata"
        );
        return;
    };
    nexus_ledger::record_media(
        db,
        identity,
        provider_used,
        model_used,
        &usage,
        &req.metadata.request_id,
        &req.metadata.feature,
    )
    .await;
}

/// Dalla `LlmUsage` (gia' normalizzata dall'adapter al prompt LORDO, vedi
/// `LlmUsage::normalized`) ai token che il ledger registra e il listino tariffa.
///
/// Trasporto e basta: i due contratti hanno la stessa convenzione — prompt
/// lordo, cache come sottoinsieme — e lo scorporo lo fa `nexus-pricing` al
/// momento di moltiplicare per le tariffe.
///
/// PUNTO UNICO della conversione (regola L): e' l'unico passaggio fra il
/// contratto del gateway e quello della contabilita'. Prima le due quantita' di
/// cache si fermavano qui — venivano lette dagli adapter, propagate fino a
/// questa funzione e poi semplicemente non nominate nella INSERT, restando al
/// DEFAULT 0 su tutte le righe del ledger.
fn token_usage_from(usage: &crate::types::LlmUsage) -> TokenUsage {
    TokenUsage {
        prompt_tokens: usage.input_tokens as i64,
        completion_tokens: usage.output_tokens as i64,
        cache_read_tokens: usage.cache_read_tokens.unwrap_or(0) as i64,
        cache_creation_tokens: usage.cache_creation_tokens.unwrap_or(0) as i64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LlmMessage, RequestMetadata};
    use nexus_pricing::{calculate_cost, calculate_cost_breakdown};
    // `Row::get` per rileggere le colonne del ledger nel test della giuntura.
    use sqlx::Row;
    use uuid::Uuid;

    fn req(messages: Vec<&str>, max_tokens: Option<u32>) -> LlmRequest {
        LlmRequest {
            model: "m".into(),
            messages: messages
                .into_iter()
                .map(|t| LlmMessage {
                    role: "user".into(),
                    content: MessageContent::Text(t.into()),
                    tool_call_id: None,
                    tool_calls: None,
                    name: None,
                    thinking_signature: None,
                    reasoning: None,
                })
                .collect(),
            temperature: None,
            max_tokens,
            tools: None,
            response_format: None,
            stream: None,
            thinking: None,
            tool_choice: None,
            pin_provider: None,
            metadata: RequestMetadata {
                tenant_id: "t".into(),
                user_id: "u".into(),
                request_id: "r".into(),
                sensitivity_tier: 0,
                feature: "chat".into(),
            },
            run_timeout_secs: None,
        }
    }

    #[test]
    fn stima_token_char_su_4_arrotonda_per_eccesso() {
        // 9 char -> ceil(9/4) = 3.
        assert_eq!(estimate_prompt_tokens(&req(vec!["123456789"], None)), 3);
        // 8 char -> 2; somma su piu' messaggi.
        assert_eq!(estimate_prompt_tokens(&req(vec!["1234", "5678"], None)), 2);
        // vuoto -> 0.
        assert_eq!(estimate_prompt_tokens(&req(vec![""], None)), 0);
    }

    #[test]
    fn quota_exceeded_display() {
        let e = QuotaExceeded {
            scope: "user".into(),
            reason: "token_limit".into(),
        };
        assert_eq!(e.to_string(), "quota_exceeded:user:token_limit");
    }

    /// L'identita' contabile si estrae dai metadata, e i due casi in cui NON si
    /// estrae sono quelli in cui il gateway non deve scrivere.
    #[test]
    fn lidentita_esce_solo_da_metadata_utilizzabili() {
        // Default dei metadata: nessuna identita'.
        assert!(identita(&req(vec!["ciao"], None)).is_none());

        // Presenti ma non-UUID: nemmeno.
        let mut r = req(vec!["ciao"], None);
        r.metadata.tenant_id = "non-un-uuid".into();
        r.metadata.user_id = "nemmeno".into();
        assert!(identita(&r).is_none());

        // Due UUID buoni: tenant_id e' il PROGETTO, user_id l'UTENTE. Lo scambio
        // dei due non e' un errore di compilazione e addebiterebbe al progetto
        // sbagliato.
        let (u, p) = (Uuid::new_v4(), Uuid::new_v4());
        let mut r = req(vec!["ciao"], None);
        r.metadata.tenant_id = p.to_string();
        r.metadata.user_id = u.to_string();
        let id = identita(&r).expect("identita' valida");
        assert_eq!(id.project_id, p);
        assert_eq!(id.user_id, u);
    }

    /// Dalla `LlmUsage` che gli adapter producono ai token che il ledger scrive.
    #[test]
    fn i_token_di_cache_arrivano_alla_riga_di_ledger() {
        // Il valore parte dal suo PRODUTTORE (`LlmUsage::normalized`, lo stesso
        // che chiamano gli adapter), non da un letterale scritto qui.
        let anthropic = crate::types::LlmUsage::normalized(
            crate::types::PromptCacheReporting::CachedReportedSeparately,
            100,
            20,
            Some(900),
            Some(50),
        );
        let t = token_usage_from(&anthropic);
        assert_eq!(t.prompt_tokens, 1_050, "il wire era al netto: 100+900+50");
        assert_eq!(t.cache_read_tokens, 900);
        assert_eq!(t.cache_creation_tokens, 50);
        // `total_tokens` e' prompt lordo + completion: la cache e' gia' dentro.
        assert_eq!(t.total_tokens(), 1_070);

        let openai = crate::types::LlmUsage::normalized(
            crate::types::PromptCacheReporting::CachedIncludedInPrompt,
            1_000,
            20,
            Some(900),
            None,
        );
        let t = token_usage_from(&openai);
        assert_eq!(t.prompt_tokens, 1_000, "il wire era gia' lordo");
        assert_eq!(t.cache_read_tokens, 900);
        // Il totale resta 1.020 come prima di questo lavoro: la serie storica
        // di quote e report non ha un gradino al deploy.
        assert_eq!(t.total_tokens(), 1_020);
    }

    /// La CONSEGUENZA sul costo, sulla stessa strada del ledger: con la tariffa
    /// di cache a listino, i token cached non pagano il prezzo pieno di input.
    #[test]
    fn il_costo_della_riga_scorpora_la_cache() {
        // Listino Anthropic tipico (mig 0403: read 0.1x, creation 1.25x).
        let price = nexus_pricing::PriceSnapshot {
            input_cost_per_million_tokens: 3.0,
            output_cost_per_million_tokens: 15.0,
            cache_read_cost_per_million_tokens: Some(0.3),
            cache_creation_cost_per_million_tokens: Some(3.75),
            currency: "USD".into(),
        };
        let usage = crate::types::LlmUsage::normalized(
            crate::types::PromptCacheReporting::CachedIncludedInPrompt,
            1_000_000,
            0,
            Some(900_000),
            None,
        );
        let tokens = token_usage_from(&usage);
        let costo = calculate_cost_breakdown(&price, &tokens);

        // Scorporato: 100k a 3.0 + 900k a 0.3 = 0.30 + 0.27 = 0.57.
        assert!(
            (costo.total_cost - 0.57).abs() < 1e-9,
            "totale {}",
            costo.total_cost
        );
        assert!((costo.cache_read_cost - 0.27).abs() < 1e-9);
        assert_eq!(costo.cache_price_state(), "priced");

        // A tariffa piena (la formula di prima) 1M di token costerebbe 3.0: il
        // test rosseggia se lo scorporo sparisce.
        let a_tariffa_piena = calculate_cost(&price, tokens.prompt_tokens, 0).2;
        assert!((a_tariffa_piena - 3.0).abs() < 1e-9);
        assert!(costo.total_cost < a_tariffa_piena);

        // Senza tariffa a listino il costo torna ESATTAMENTE quello di prima —
        // mai peggiore — e il ripiego e' dichiarato.
        let senza_listino_cache = nexus_pricing::PriceSnapshot {
            cache_read_cost_per_million_tokens: None,
            cache_creation_cost_per_million_tokens: None,
            ..price.clone()
        };
        let ripiego = calculate_cost_breakdown(&senza_listino_cache, &tokens);
        assert!((ripiego.total_cost - a_tariffa_piena).abs() < 1e-12);
        assert_eq!(ripiego.cache_price_state(), "cache_price_missing");
    }

    // ── La GIUNTURA: dai tipi del gateway alle COLONNE ─────────
    //
    // I test qui sopra coprono i PEZZI: la normalizzazione, la conversione verso
    // il listino, lo scorporo del costo, l'estrazione dell'identita'. Nessuno
    // dimostra la giuntura, cioe' che percorrendo l'adapter per intero i valori
    // finiscano nelle colonne giuste. L'unico modo di dimostrarlo e' percorrere
    // la strada della produzione fino in fondo e RILEGGERE la riga dal database
    // vero, sullo schema reale applicato dal META_MIGRATOR (regola O).
    //
    // Il gemello di questo test — quello che verifica che davanti a questa riga
    // nessuno ne aggiunga una seconda — vive in
    // `crates/nexus-ledger/tests/una_sola_riga_finalizzata.rs`, dove entrambi i
    // produttori sono raggiungibili.

    /// Identita' che le FK del ledger esigono, dal seeder unico dello schema.
    async fn seed_identita(pool: &PgPool) -> (Uuid, Uuid, Uuid) {
        let (user, project) = nexus_test_schema::seed_identita_meta(pool).await;
        (user, project, Uuid::new_v4())
    }

    /// Listino con ENTRAMBE le tariffe di cache valorizzate (forma della mig
    /// 0403) e le quattro tariffe DISTINTE: e' cio' che rende osservabile uno
    /// scambio di posizione fra i bind.
    async fn seed_listino(pool: &PgPool) {
        sqlx::query(
            "INSERT INTO ai_price_catalog ( \
                 provider, model, \
                 input_cost_per_million_tokens, output_cost_per_million_tokens, \
                 cache_read_cost_per_million_tokens, cache_creation_cost_per_million_tokens, \
                 currency, pricing_state \
             ) VALUES ('anthropic', 'claude-x', 3.0, 15.0, 0.3, 3.75, 'USD', 'priced')",
        )
        .execute(pool)
        .await
        .expect("seed ai_price_catalog");
    }

    /// Richiesta con l'identita' che il gateway richiede per contabilizzare.
    fn req_con_identita(project: Uuid, user: Uuid, run: Uuid) -> LlmRequest {
        let mut r = req(vec!["ciao"], Some(64));
        r.metadata.tenant_id = project.to_string();
        r.metadata.user_id = user.to_string();
        r.metadata.request_id = run.to_string();
        r
    }

    /// Risposta come la costruisce un adapter: l'usage nasce dal suo PRODUTTORE
    /// (`LlmUsage::normalized`), che e' l'unico posto dove si decide se i token
    /// di cache vanno sommati al prompt per arrivare al lordo.
    fn resp_anthropic_con_cache() -> LlmResponse {
        LlmResponse {
            content: "ok".to_string(),
            tool_calls: None,
            usage: crate::types::LlmUsage::normalized(
                crate::types::PromptCacheReporting::CachedReportedSeparately,
                1_000_000,
                400_000,
                Some(2_000_000),
                Some(500_000),
            ),
            model_used: "claude-x".to_string(),
            provider_used: "anthropic".to_string(),
            latency_ms: 7,
            finish_reason: "stop".to_string(),
            privacy_rerouted: None,
            reasoning: None,
            thinking_signature: None,
            citations: None,
            ledger: None,
        }
    }

    /// I dodici numeri della riga, ognuno nella sua colonna. Scelti distinti a
    /// due a due proprio perche' uno scambio non possa passare inosservato.
    #[sqlx::test(migrator = "nexus_test_schema::META_MIGRATOR")]
    async fn record_usage_scrive_ogni_numero_nella_sua_colonna(pool: PgPool) {
        let (user, project, run) = seed_identita(&pool).await;
        seed_listino(&pool).await;

        // La strada della produzione per intero: si scrive e si DICHIARA sulla
        // risposta, con la stessa funzione che chiama la pipeline HTTP.
        let mut resp = resp_anthropic_con_cache();
        record_and_declare(&pool, &req_con_identita(project, user, run), &mut resp).await;

        let riga = sqlx::query(
            "SELECT id, user_id, project_id, provider, model, status, currency, details, \
                    prompt_tokens, completion_tokens, total_tokens, \
                    cache_read_tokens, cache_creation_tokens, \
                    input_cost::float8          AS input_cost, \
                    output_cost::float8         AS output_cost, \
                    cache_read_cost::float8     AS cache_read_cost, \
                    cache_creation_cost::float8 AS cache_creation_cost, \
                    total_cost::float8          AS total_cost \
               FROM ai_usage_ledger WHERE run_id = $1",
        )
        .bind(run)
        .fetch_one(&pool)
        .await
        .expect(
            "la riga di ledger deve esistere: l'insert e' best-effort e un errore \
             SQL qui sarebbe solo loggato, cioe' invisibile",
        );

        // Identita' e chiavi testuali: anche queste sono bind posizionali.
        assert_eq!(riga.get::<Uuid, _>("user_id"), user);
        assert_eq!(riga.get::<Uuid, _>("project_id"), project);
        assert_eq!(riga.get::<String, _>("provider"), "anthropic");
        assert_eq!(riga.get::<String, _>("model"), "claude-x");
        assert_eq!(riga.get::<String, _>("status"), "finalized");
        assert_eq!(riga.get::<String, _>("currency"), "USD");

        // I quattro CONTEGGI. `prompt_tokens` e' il LORDO (il wire Anthropic era
        // al netto: 1M + 2M + 0.5M) e i due conteggi di cache ne sono il
        // dettaglio; il totale e' prompt lordo + completion.
        assert_eq!(riga.get::<i32, _>("prompt_tokens"), 3_500_000);
        assert_eq!(riga.get::<i32, _>("completion_tokens"), 400_000);
        assert_eq!(riga.get::<i64, _>("cache_read_tokens"), 2_000_000);
        assert_eq!(riga.get::<i64, _>("cache_creation_tokens"), 500_000);
        assert_eq!(riga.get::<i32, _>("total_tokens"), 3_900_000);

        // I cinque IMPORTI, alle quattro tariffe distinte del listino:
        // 1M x 3.0, 0.4M x 15.0, 2M x 0.3, 0.5M x 3.75. Il messaggio riporta il
        // valore LETTO: su uno scambio di bind e' quello che dice quale colonna
        // ha preso il posto di quale.
        for (colonna, atteso) in [
            ("input_cost", 3.0),
            ("output_cost", 6.0),
            ("cache_read_cost", 0.6),
            ("cache_creation_cost", 1.875),
            ("total_cost", 11.475),
        ] {
            let letto: f64 = riga.get(colonna);
            assert!(
                (letto - atteso).abs() < 1e-9,
                "{colonna}: letto {letto}, atteso {atteso}"
            );
        }

        // E lo stato del listino, che e' il segnale con cui si distingue uno zero
        // "gratis" da uno zero "non so quanto costa" (regola M).
        let details: serde_json::Value = riga.get("details");
        assert_eq!(details["price_state"], "priced");
        assert_eq!(details["price_missing"], false);
        assert_eq!(details["cache_price_state"], "priced");

        // E cio' che il gateway DICHIARA SULLA RISPOSTA e' la riga che ha
        // scritto davvero. Non e' una formalita': su questa dichiarazione il
        // chiamante che ha prenotato decide di NON addebitare una seconda volta
        // (`nexus_ledger::settle`). Se l'id o l'importo dichiarati non fossero
        // quelli della riga, la correlazione punterebbe altrove e il costo
        // mostrato all'utente divergerebbe dal ledger.
        let dichiarazione = resp
            .ledger
            .as_ref()
            .expect("la pipeline deve dichiarare SEMPRE un esito contabile");
        assert_eq!(dichiarazione.as_str(), "written");
        let entry = dichiarazione
            .entry()
            .expect("il gateway ha scritto: deve dichiarare la riga");
        assert_eq!(entry.id, riga.get::<Uuid, _>("id"));
        assert_eq!(entry.currency, riga.get::<String, _>("currency"));
        assert!(
            (entry.total_cost - riga.get::<f64, _>("total_cost")).abs() < 1e-9,
            "costo dichiarato {} != costo scritto {}",
            entry.total_cost,
            riga.get::<f64, _>("total_cost")
        );
    }

    /// La domanda che decide se rilasciare una prenotazione e' "il gateway ha
    /// scritto?", e la risposta NON e' "la chiamata e' riuscita".
    ///
    /// Qui la chiamata riesce (la `LlmResponse` e' identica al test sopra) ma i
    /// metadata non portano identita': e' esattamente cio' che manda
    /// `NeuralCoreClient::generate_completion` (`GwMetadata::default`). Il
    /// gateway non scrive nulla, e deve DIRLO: un chiamante che rilasciasse la
    /// prenotazione fidandosi dell'esito perderebbe l'intero addebito.
    ///
    /// E il "non ho scritto" e' DETTO, non taciuto: `no_identity` viaggia sul
    /// wire, e il silenzio resta libero di significare una cosa sola — un
    /// gateway che non parla questo contratto.
    ///
    /// MUTAZIONE: facendo dichiarare una riga fabbricata prima della guardia
    /// d'identita', questo test e il suo gemello qui sopra rosseggiano entrambi;
    /// facendo lasciare `resp.ledger` a `None` invece che a `Some(NoIdentity)`,
    /// rosseggia la prima asserzione — ed e' il caso in cui il chiamante non puo'
    /// piu' distinguere una scelta da una build vecchia.
    #[sqlx::test(migrator = "nexus_test_schema::META_MIGRATOR")]
    async fn senza_identita_il_gateway_non_scrive_e_lo_dichiara(pool: PgPool) {
        seed_listino(&pool).await;

        // `req` senza tenant_id/user_id: la forma di default dei metadata.
        let mut resp = resp_anthropic_con_cache();
        record_and_declare(&pool, &req(vec!["ciao"], Some(64)), &mut resp).await;

        let dichiarazione = resp
            .ledger
            .as_ref()
            .expect("anche il 'non ho scritto' va DICHIARATO: il silenzio dice un'altra cosa");
        assert_eq!(dichiarazione.as_str(), "no_identity");
        assert!(
            dichiarazione.entry().is_none(),
            "senza identita' il gateway non scrive: dichiarare una riga inesistente \
             autorizzerebbe il chiamante a rilasciare la prenotazione, perdendo l'addebito"
        );
        let righe: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ai_usage_ledger")
            .fetch_one(&pool)
            .await
            .expect("conteggio ledger");
        assert_eq!(righe, 0, "nessuna riga doveva essere scritta");
    }
}

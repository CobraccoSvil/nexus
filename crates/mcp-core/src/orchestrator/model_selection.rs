//! Punto unico (regola L) dei sotto-componenti CONDIVISI della selezione di un
//! modello dal `ai_price_catalog`.
//!
//! FASE 1 del consolidamento del selettore modello (vedi ADR 0030): questo
//! modulo elimina le duplicazioni ACCIDENTALI (pesi di scoring hardcoded,
//! normalizzazione case-insensitive dei provider esclusi sparsa/incoerente)
//! SENZA cambiare alcun comportamento osservabile dei call site. Il selettore
//! unico vero e proprio (`EligibilityFilter` + `RankStrategy` + `select_models`)
//! arrivera' in FASE 2 e vivra' qui.
//!
//! Regole applicate:
//!   - G: i pesi di scoring NON sono hardcoded nel codice; vengono dalla riga
//!     sentinella `intent='*'` di `nexus_intent_routing_requirements`. Se la
//!     riga manca, il sistema FALLISCE in modo visibile (Err), niente fallback.
//!   - L: un solo posto sa come si costruisce la lista dei provider esclusi
//!     (cooldown snapshot + extra del chiamante, tutti lowercase) e quali sono
//!     i pesi di default.

use nexus_cache::TtlCache;
use sqlx::{PgPool, Row};
use std::sync::OnceLock;
use std::time::Duration;

/// Pesi dello scoring multi-fattore usato dall'auto-promoter e dal routing
/// slot-based. Fonte unica: riga sentinella DB (regola G).
#[derive(Debug, Clone)]
pub(crate) struct ScoringWeights {
    pub tier: f32,
    pub cost: f32,
    pub context: f32,
    pub capabilities: f32,
}

static WEIGHTS_CACHE: OnceLock<TtlCache<String, ScoringWeights>> = OnceLock::new();

fn weights_cache() -> &'static TtlCache<String, ScoringWeights> {
    WEIGHTS_CACHE.get_or_init(|| TtlCache::new(Duration::from_secs(60)))
}

/// Legge i pesi di default dalla riga sentinella `intent='*', behavior_mode='*'`
/// di `nexus_intent_routing_requirements`. NESSUNA cache (per testabilita'
/// isolata): il wrapper con cache e' `default_scoring_weights`.
///
/// Regola G: se la riga sentinella non esiste ritorna `Err` (fail visibile,
/// niente pesi hardcoded di emergenza). I campi sono letti senza `unwrap_or`
/// numerico: un errore di deserializzazione si propaga invece di mascherare un
/// peso fittizio.
async fn fetch_default_weights(db: &PgPool) -> Result<ScoringWeights, String> {
    let row = sqlx::query(
        "SELECT weight_tier, weight_cost, weight_context, weight_capabilities \
           FROM nexus_intent_routing_requirements \
          WHERE intent = '*' AND behavior_mode = '*'",
    )
    .fetch_optional(db)
    .await
    .map_err(|e| format!("default_scoring_weights: query fallita: {e}"))?
    .ok_or_else(|| {
        "default_scoring_weights: riga sentinella intent='*'/behavior_mode='*' assente in \
         nexus_intent_routing_requirements; applicare la migrazione 0379 dei pesi di default \
         (regola G: nessun fallback hardcoded)"
            .to_string()
    })?;
    Ok(ScoringWeights {
        tier: row
            .try_get("weight_tier")
            .map_err(|e| format!("default_scoring_weights: weight_tier: {e}"))?,
        cost: row
            .try_get("weight_cost")
            .map_err(|e| format!("default_scoring_weights: weight_cost: {e}"))?,
        context: row
            .try_get("weight_context")
            .map_err(|e| format!("default_scoring_weights: weight_context: {e}"))?,
        capabilities: row
            .try_get("weight_capabilities")
            .map_err(|e| format!("default_scoring_weights: weight_capabilities: {e}"))?,
    })
}

/// Pesi di scoring di default, con cache 60s (TtlCache, punto unico cache,
/// regola L). Usato dal routing slot-based (`select_models_for_requirement`)
/// e in FASE 2 dalle viste runtime. Regola G: niente pesi hardcoded.
///
/// La chiave di cache e' l'identita' del DATABASE (`nexus_auth::pool_identity`,
/// la stessa della cache dei settings), non una costante: la riga sentinella
/// esiste in ogni database e i valori possono differire, quindi una chiave
/// costante servirebbe i pesi del primo lettore a tutti gli altri.
pub(crate) async fn default_scoring_weights(db: &PgPool) -> Result<ScoringWeights, String> {
    let ck = nexus_auth::pool_identity(db);
    if let Some(w) = weights_cache().get(&ck) {
        return Ok(w);
    }
    let w = fetch_default_weights(db).await?;
    weights_cache().insert(ck, w.clone());
    Ok(w)
}

/// Cio' che la selezione NON puo' instradare adesso: i FORNITORI esclusi per
/// intero e le COPPIE (fornitore, modello) escluse. Due liste perche' sono due
/// domande — appiattirle in una sola era il difetto D1 (vedi
/// [`crate::provider_cooldown::ChiaveCooldown`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct EsclusioniSelezione {
    /// Nomi di fornitore in lowercase, deduplicati. Alimentano
    /// `AND LOWER(provider) <> ALL(...)`.
    pub fornitori: Vec<String>,
    /// Coppie `(provider, model)` in lowercase. Alimentano l'anti-join sulle
    /// coppie: escludono QUEL modello e lasciano gli altri del fornitore.
    pub coppie: Vec<(String, String)>,
}

/// Costruisce le esclusioni della selezione, normalizzate in lowercase (regola
/// L): cooldown attivi (quando `apply_cooldown`) PIU' gli `extra` indicati dal
/// chiamante, deduplicati.
///
/// Punto unico della normalizzazione: prima alcuni call site facevano
/// `LOWER(provider)` mentre il ramo non-agentico di `best_model_for_tier`
/// confrontava il nome RAW del catalog contro lo snapshot (gia' lowercase),
/// con possibile mismatch. Qui la sorgente e' una sola.
///
/// Gli `extra` del chiamante restano FORNITORI interi: chi li passa (veto del
/// giudice, provider gia' tentati nel turno) intende escludere l'account, non
/// una coppia.
pub(crate) fn esclusioni_selezione(extra: &[String], apply_cooldown: bool) -> EsclusioniSelezione {
    let mut fornitori: Vec<String> = if apply_cooldown {
        crate::provider_cooldown::fornitori_in_cooldown()
    } else {
        Vec::new()
    };
    for p in extra {
        let pl = p.trim().to_lowercase();
        if !fornitori.contains(&pl) {
            fornitori.push(pl);
        }
    }
    EsclusioniSelezione {
        fornitori,
        // Le coppie hanno senso SOLO col cooldown attivo: nessun chiamante
        // dichiara oggi un'esclusione per coppia che non venga da li'.
        coppie: if apply_cooldown {
            crate::provider_cooldown::coppie_in_cooldown()
        } else {
            Vec::new()
        },
    }
}

/// Predicato di ELEGGIBILITA' di un modello del catalog (FASE 2, regola L).
///
/// Un solo posto definisce QUALI filtri si applicano; i call site scelgono i
/// flag. Sostituisce le WHERE duplicate di `select_agentic_model` (path
/// agentico) e del ramo non-agentico inline di `best_model_for_tier`.
///
/// NON include `consecutive_failures`: la salute e' gia' garantita da
/// `is_enabled = TRUE` (il `model_health_probe` auto-disabilita a soglia) e
/// filtrare `consecutive_failures = 0` causerebbe starvation (ADR 0025).
#[derive(Debug, Clone)]
pub(super) struct EligibilityFilter<'a> {
    /// `true` => `AND supports_tool_use = TRUE` (path agentico).
    pub require_tool_use: bool,
    /// `true` => `AND agentic_thinking_policy <> 'exclude'` e abilita il
    /// TIE-BREAKER `(agentic_thinking_policy = 'none') DESC` come ULTIMO criterio
    /// di ORDER BY (ADR 0025, declassato: era pre-ordinamento PRIMARIO, ma con i
    /// modelli forti moderni ormai tutti dual-mode escludeva i migliori a favore
    /// dei completion/legacy; l'affidabilita' sotto `tool_choice` e' garantita dal
    /// gateway). Per il path non-agentico (vision/embedding) il thinking non si
    /// applica -> `false`, niente tie-breaker.
    pub require_thinking_non_exclude: bool,
    /// Capability richiesta. Le capability con una COLONNA canonica dedicata
    /// (vision + i media kind image_gen/audio_in/audio_out/video_gen, mig 0478)
    /// si filtrano via `AND supports_<x> = TRUE` (vedi `capability_to_column`);
    /// ogni altra capability => `capabilities @> ["c"]` nel jsonb. Quando la
    /// capability richiesta NON e' un media kind, i modelli media vengono ESCLUSI
    /// (un image-gen non risale la classifica dei purpose testuali).
    pub capability: Option<&'a str>,
    /// `>0` => `AND context_window >= N`.
    pub min_context_window: i64,
    /// `Some(t)` => `AND tier_rank(performance_tier) >= tier_rank(t)`: il PAVIMENTO
    /// di capacita'. E' ELEGGIBILITA', non preferenza — un modello sotto il
    /// pavimento non e' un'alternativa peggiore, e' un'alternativa che NON
    /// FUNZIONA per un run agentico.
    ///
    /// Perche' esiste (misurato il 16/07): il failover enumerava con `AnyTier` e
    /// lasciava scegliere al modulo puro col "tier come indicazione". Con openai e
    /// anthropic senza credito, il piu' economico e sano e' risultato
    /// `groq/gpt-oss-20b` — agentic_index **3.1**, il peggiore del parco. Il run
    /// non e' fallito: ha prodotto una risposta FUORI TEMA (parlava del modello
    /// stesso invece del task) e l'ha dichiarata `completed`. Un esito bugiardo e'
    /// peggio di un fallimento, perche' l'utente ci si fida.
    ///
    /// Il confronto usa il vocabolario unico (`tier_rank_sql`, regola L): un tier
    /// NULL o ignoto prende il rank neutro di `tier_rank` (medium), come ovunque.
    pub min_tier: Option<&'a str>,
    /// Provider extra da escludere (oltre al cooldown se `apply_cooldown`).
    pub exclude_providers: &'a [String],
    /// `true` => esclude anche i provider attualmente in cooldown (snapshot).
    pub apply_cooldown: bool,
    /// `Some(p)` => RESTRINGE la selezione al SOLO provider `p` (filtro POSITIVO
    /// `AND LOWER(provider) = LOWER(p)`), oltre a tutti gli altri filtri. Usato
    /// per la propagazione del PIN del provider ai sub-agenti worker: il pin e'
    /// una preferenza-forte tier-aware (tier+capability+tool_use invariati, solo
    /// il provider e' vincolato). `None` (default storico) => nessuna restrizione,
    /// comportamento bit-identico per i ~13 costruttori esistenti. Il valore e'
    /// BINDATO (mai interpolato): niente SQL injection.
    pub only_provider: Option<&'a str>,
    /// `true` => richiede l'EVIDENZA che il modello regga il profilo d'uso reale
    /// (gate di qualificazione, mig 0591): `qualification_state = 'qualified'`
    /// non scaduto, e le capability jsonb si filtrano su `qualified_capabilities`
    /// (PROVATE dal qualificatore) invece di `capabilities` (dichiarate).
    /// Distinto da `require_tool_use`: dichiarato != provato — e' l'assunzione
    /// "la salute e' gia' garantita da is_enabled" che ha permesso gli incidenti
    /// 2026-07-14/15 (11 modelli 404 e un 429-quota scoperti dalle richieste di
    /// produzione). Acceso dal flag DB `agent.model_qualification.enforce_routing_gate`
    /// nel solo path AGENTICO; `false` = comportamento storico.
    pub require_qualified: bool,
    /// `true` => esclude i modelli preview/experimental dalla selezione. I
    /// pre-GA girano su capacita' CONDIVISA best-effort (Vertex Dynamic Shared
    /// Quota: 429 RESOURCE_EXHAUSTED a intermittenza anche a basso volume) e
    /// vengono ritirati con ~2 settimane di preavviso (404 improvvisi su tutte
    /// le region) — e' esattamente la coppia di incidenti 2026-07-14/15 dei
    /// consiglieri. Google stessa dichiara gli experimental non adatti alla
    /// produzione. Acceso dal flag DB `agent.model_qualification.exclude_preview_agentic`
    /// nel solo path AGENTICO (le chain agentiche muoiono su un singolo 429/404);
    /// il pin esplicito dell'utente non passa di qui e resta libero.
    pub exclude_preview: bool,
}

/// Frammento WHERE (statico, niente input utente) che riconosce i modelli
/// pre-GA dal SUFFISSO canonico di naming dei provider: `-preview`/`preview-`,
/// `-exp` terminale o seguito da separatore (gemini-2.0-flash-exp,
/// gemini-exp-1206), `experimental`. PUNTO UNICO (regola L) del criterio: i
/// call site non duplicano la regex.
const PRE_GA_MODEL_PREDICATE_SQL: &str =
    " AND model !~* '(preview|experimental|[-_]exp([-_.]|$))'";

/// Flag del gate di qualificazione applicati al path AGENTICO (mig 0591/0592).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct QualificationGate {
    /// `agent.model_qualification.enforce_routing_gate`: richiede
    /// `qualification_state='qualified'` non scaduto + capability PROVATE.
    pub require_qualified: bool,
    /// `agent.model_qualification.exclude_preview_agentic`: esclude i modelli
    /// preview/experimental (capacita' best-effort + ritiri improvvisi).
    pub exclude_preview: bool,
}

/// Chiavi in `settings` dei due flag del gate di qualificazione (mig 0591/0592).
const ENFORCE_ROUTING_GATE_KEY: &str = "agent.model_qualification.enforce_routing_gate";
const EXCLUDE_PREVIEW_AGENTIC_KEY: &str = "agent.model_qualification.exclude_preview_agentic";

/// Lettura dei flag del gate di qualificazione (mig 0591/0592) dal PUNTO UNICO
/// dei settings (`nexus_auth`, regola L), che ha gia' la cache 60s — chiavata
/// per DATABASE (`user@host:porta/db`). Il routing la consulta a ogni selezione
/// AGENTICA senza martellare il DB. Chiave assente o illeggibile -> `false`
/// (comportamento storico: i rollout si accendono SOLO con la riga in settings,
/// regola G, nessun default nascosto che scavalchi il DB).
///
/// Perche' NON ha una cache propria (2026-07-27, causa radice di 6 test flaky).
/// Qui viveva una cache statica di processo `OnceLock<Mutex<Option<(_, Instant)>>>`
/// SENZA chiave: memorizzava il valore letto da UN database e lo serviva a
/// QUALUNQUE altro per 60s. mcp-core interroga piu' database (il meta e un
/// `<slug>_nexus` per progetto), quindi era un difetto di produzione prima ancora
/// che di test: il gate del meta poteva decidere una selezione fatta sul DB di un
/// progetto, e viceversa. Nei test lo stesso difetto era gia' VISIBILE: i
/// `#[sqlx::test(migrator = "META_MIGRATOR")]` girano su un DB dove la mig 0595
/// porta `enforce_routing_gate` a `true`, e la loro prima selezione accendeva il
/// gate per tutti gli altri test del processo — che seminano un catalog
/// `unqualified` e si vedevano il pool svuotato, con esito dipendente da chi
/// vinceva la corsa (regole F e O). La gemella `select_model_with_gate` era nata
/// per aggirare proprio questa cache; ora non c'e' piu' nulla da aggirare.
pub(crate) async fn qualification_gate(db: &PgPool) -> QualificationGate {
    QualificationGate {
        require_qualified: flag_settato(db, ENFORCE_ROUTING_GATE_KEY).await,
        exclude_preview: flag_settato(db, EXCLUDE_PREVIEW_AGENTIC_KEY).await,
    }
}

/// `true` solo se la chiave esiste ed e' accesa. Errore DB o chiave assente ->
/// `false` (fail-safe storico del gate: nessun rollout si accende da solo).
async fn flag_settato(db: &PgPool, key: &str) -> bool {
    match nexus_auth::get_bool_setting(db, key).await {
        Ok(v) => v.unwrap_or(false),
        Err(err) => {
            tracing::warn!(
                error = %err,
                key = %key,
                "qualification_gate: lettura settings fallita, flag spento"
            );
            false
        }
    }
}

/// PUNTO UNICO (regola L) del mapping capability -> colonna booleana canonica
/// di `ai_price_catalog`. Ritorna il nome colonna per le capability che hanno
/// una colonna dedicata (vision, mig 0318; i 4 media kind, mig 0478), `None`
/// per le capability che vivono nel jsonb `capabilities` (chat/code/reasoning/...).
///
/// Aggiungere un nuovo media kind richiede UNA riga qui + la colonna in
/// migrazione: nessun `if`/`match` duplicato sparso nei call site (regola L).
/// I valori ritornati sono nomi-colonna STATICI (whitelist): vengono interpolati
/// nella SQL ma NON derivano da input utente -> niente SQL injection.
fn capability_to_column(capability: &str) -> Option<&'static str> {
    match capability {
        "vision" => Some("supports_vision"),
        "image_gen" => Some("supports_image_gen"),
        "audio_in" => Some("supports_audio_in"),
        "audio_out" => Some("supports_audio_out"),
        "video_gen" => Some("supports_video_gen"),
        _ => None,
    }
}

/// True se la capability e' un MEDIA kind (image/audio/video), non testuale
/// (chat/code/reasoning) ne' vision. Punto unico (regola L) usato per decidere
/// se ESCLUDERE i modelli media dalla selezione: i purpose testuali NON devono
/// pescare un image-gen; un purpose media (es. generate_image) si'.
fn is_media_capability(capability: &str) -> bool {
    matches!(
        capability,
        "image_gen" | "audio_in" | "audio_out" | "video_gen"
    )
}

/// Selezione TIER-CHAIN + `ORDER BY` SQL: semantica del routing LIVE.
///
/// Prova i tier di `tier_chain` in ordine (degradazione); il PRIMO tier con
/// almeno un candidato eleggibile vince (corto-circuito). Entro quel tier
/// ordina per `order_by` (SEGUITO dal tie-breaker `(agentic_thinking_policy='none')
/// DESC` se `require_thinking_non_exclude`) e ritorna i primi `limit`. `tier_chain`
/// vuoto = qualunque tier (singola query).
///
/// Punto unico (regola L) della WHERE di eleggibilita' del path live: prima
/// duplicata tra `select_agentic_model` (SQL agentico) e il ramo non-agentico
/// inline di `best_model_for_tier`. Propaga `Result` (regola H): l'errore SQL
/// non e' piu' silenziato come "nessun modello".
///
/// Ritorna `(provider, model, performance_tier)`: il tier viaggia con la riga
/// (i selettori tier-aware, es. il failover agentico, lo usano come indicazione
/// senza un lookup extra); i caller che non ne hanno bisogno lo ignorano.
/// Come la capability richiesta si traduce in filtri: colonna dedicata, jsonb, o
/// esclusione dei media. Calcolata una volta per tutta la tier-chain.
struct QueryShape {
    /// PUNTO UNICO (regola L) del mapping capability -> colonna canonica del
    /// catalog. Le capability con una colonna booleana dedicata (vision + i media
    /// kind della mig 0478) si filtrano via colonna; ogni altra capability (chat,
    /// 'code', 'reasoning', ...) resta nel jsonb `capabilities`. Aggiungere un
    /// nuovo media kind = una riga in `capability_to_column`, niente if sparsi.
    capability_column: Option<&'static str>,
    capability_json: Option<String>,
    requested_is_media: bool,
}

/// Costruisce la query di UN anello della tier-chain.
///
/// I placeholder sono assegnati con un idx incrementale: $1 = array provider
/// esclusi per intero, $2/$3 = le due colonne parallele delle COPPIE escluse
/// (sempre presenti, anche vuote), poi tier, capability jsonb,
/// min_context_window, only_provider.
/// **L'ordine dei `push_str` qui DEVE combaciare con l'ordine dei `bind` nel
/// chiamante**: e' un accoppiamento posizionale che il tipo non protegge, ed e'
/// l'unica ragione per cui le due meta' vanno lette insieme.
///
/// L'anti-join sulle coppie e' il pezzo che fa ANTICIPARE la selezione. Prima
/// qui c'era il solo filtro per fornitore, quindi un cooldown per coppia — la
/// forma che un rate limit prende dal 07/08/2026 — non toglieva nulla dai
/// candidati: la coppia veniva scelta, mandata, e rifiutata dal gateway, che il
/// cooldown lo applica bene e ATTENDE («attendo cooldown transitorio breve prima
/// di ritentare wait_s=25»). Un giro di selezione sprecato piu' l'attesa, per
/// ogni occorrenza.
///
/// Due array PARALLELI e non una chiave concatenata: comporre
/// `provider || sep || model` in SQL rimetterebbe la convenzione della chiave in
/// un secondo posto, che e' precisamente il difetto da cui si viene (regola L).
fn build_tierchain_sql(
    filter: &EligibilityFilter<'_>,
    shape: &QueryShape,
    tier: Option<&str>,
    order_by: &str,
    limit: i64,
) -> String {
    // $1 fornitori esclusi, $2/$3 coppie escluse: tre bind SEMPRE presenti.
    let mut idx = 3;
    let mut sql = String::from(
        "SELECT provider, model, performance_tier FROM ai_price_catalog \
         WHERE is_enabled = TRUE \
           AND LOWER(provider) <> ALL($1) \
           AND NOT EXISTS ( \
                 SELECT 1 FROM unnest($2::text[], $3::text[]) AS coppia_esclusa(p, m) \
                  WHERE coppia_esclusa.p = LOWER(ai_price_catalog.provider) \
                    AND coppia_esclusa.m = LOWER(ai_price_catalog.model)) \
           AND (auto_disabled_reason IS NULL \
                OR (auto_disabled_reason NOT LIKE 'invalid_model%' \
                    AND auto_disabled_reason NOT LIKE 'model_not_found%'))",
    );
    push_gate_predicates(&mut sql, filter, shape);
    if tier.is_some() {
        idx += 1;
        sql.push_str(&format!(" AND performance_tier = ${idx}"));
    }
    if shape.capability_json.is_some() {
        idx += 1;
        push_capability_json(&mut sql, filter, idx);
    }
    if filter.min_context_window > 0 {
        idx += 1;
        sql.push_str(&format!(" AND context_window >= ${idx}"));
    }
    push_min_tier(&mut sql, filter);
    if filter.only_provider.is_some() {
        // PIN provider (filtro POSITIVO): restringe al solo provider pinnato.
        // Ultimo placeholder DOPO min_context_window per preservare lo schema idx
        // incrementale; il valore e' bindato lowercase (no interpolazione).
        idx += 1;
        sql.push_str(&format!(" AND LOWER(provider) = ${idx}"));
    }
    push_order_by(&mut sql, filter, order_by, limit);
    sql
}

/// PAVIMENTO di capacita' (ELEGGIBILITA', non preferenza: un modello sotto soglia
/// non e' un'alternativa peggiore, e' un'alternativa che non funziona).
/// L'espressione del rank viene GENERATA dal vocabolario unico: la scala non si
/// riscrive a mano nemmeno qui (regola L). `tier_rank` del floor e' calcolato in
/// Rust dalla STESSA funzione, quindi le due meta' non possono divergere.
fn push_min_tier(sql: &mut String, filter: &EligibilityFilter<'_>) {
    if let Some(floor) = filter.min_tier {
        use nexus_agent_graph::decisions::tiers::{tier_rank, tier_rank_sql};
        sql.push_str(&format!(
            " AND {} >= {}",
            tier_rank_sql("performance_tier"),
            tier_rank(floor)
        ));
    }
}

/// Col gate acceso le capability jsonb si verificano sul PROVATO
/// (`qualified_capabilities`, scritto solo dal qualificatore), non sul dichiarato:
/// una capability affermata a mano e mai dimostrata non instrada piu' nessuno.
/// Nomi colonna statici, niente injection.
fn push_capability_json(sql: &mut String, filter: &EligibilityFilter<'_>, idx: i32) {
    let cap_col = if filter.require_qualified {
        "qualified_capabilities"
    } else {
        "capabilities"
    };
    sql.push_str(&format!(" AND {cap_col} @> ${idx}::jsonb"));
}

/// I predicati di ELEGGIBILITA' che non dipendono dall'anello di tier-chain:
/// gate di qualificazione, tool use, pre-GA, thinking, capability per colonna,
/// esclusione dei media. Tutti frammenti statici, nessun input interpolato.
fn push_gate_predicates(sql: &mut String, filter: &EligibilityFilter<'_>, shape: &QueryShape) {
    if filter.require_tool_use {
        sql.push_str(" AND supports_tool_use = TRUE");
    }
    if filter.require_qualified {
        // Gate di qualificazione (mig 0591): solo modelli PROVATI e non scaduti.
        sql.push_str(
            " AND qualification_state = 'qualified' \
              AND (qualification_expires_at IS NULL OR qualification_expires_at > NOW())",
        );
    }
    if filter.exclude_preview {
        sql.push_str(PRE_GA_MODEL_PREDICATE_SQL);
    }
    if filter.require_thinking_non_exclude {
        sql.push_str(" AND agentic_thinking_policy <> 'exclude'");
    }
    if let Some(col) = shape.capability_column {
        // `col` proviene da `capability_to_column` (whitelist statica di nomi
        // colonna): nessun input utente interpolato, niente SQL injection.
        sql.push_str(&format!(" AND {col} = TRUE"));
    }
    if !shape.requested_is_media {
        // I modelli media non risalgono la classifica dei purpose testuali.
        sql.push_str(
            " AND supports_image_gen = FALSE AND supports_audio_in = FALSE \
              AND supports_audio_out = FALSE AND supports_video_gen = FALSE",
        );
    }
}

/// ORDER BY: capacita'/costo (`order_by`) e' il criterio PRIMARIO. Il
/// pre-ordinamento ADR 0025 (preferire i modelli nativamente non-thinking,
/// `policy='none'`) e' declassato a TIE-BREAKER finale. Razionale (regola H,
/// causa radice): i modelli forti moderni sono ORMAI TUTTI dual-mode
/// (`disable_for_tools`: claude opus/sonnet, gpt-5.x, deepseek-v4), mentre i
/// `none` rimasti sono i completion/legacy deboli (deepseek-coder/chat,
/// codestral, gpt-4.1). Con `none` come criterio PRIMARIO il routing agentico
/// sceglieva sistematicamente i modelli peggiori. L'affidabilita' sotto
/// `tool_choice` forzato e' garantita a monte dal gateway (disabilita il thinking
/// quando ci sono tool, vedi nexus-gateway providers). Resta come SPAREGGIO a
/// parita' di `order_by` (preferenza conservata dove non costa).
fn push_order_by(sql: &mut String, filter: &EligibilityFilter<'_>, order_by: &str, limit: i64) {
    sql.push_str(" ORDER BY ");
    sql.push_str(order_by);
    if filter.require_thinking_non_exclude {
        sql.push_str(", (agentic_thinking_policy = 'none') DESC");
    }
    sql.push_str(&format!(" LIMIT {limit}"));
}

/// `min_distinct_providers` governa la CONDIZIONE DI USCITA dalla tier-chain.
///
/// Con `0` o `1` vale la regola storica: si esce al primo tier che restituisce
/// righe, e i candidati restano omogenei di fascia.
///
/// Con `>= 2` la domanda cambia natura: il chiamante non chiede "dei modelli",
/// chiede "modelli su N provider DISTINTI" (fan-out multi-provider). La
/// non-vuotezza smette di essere una risposta: un tier con dieci modelli di un
/// solo provider non soddisfa la richiesta, e uscire li' significa dichiarare
/// "provider insufficienti" senza aver mai guardato i tier successivi, che erano
/// gia' autorizzati dalla catena. E' il difetto osservato il 20/07: 6 provider
/// sani abilitati, panel degradato con `got=1 min=2`. In quel caso si accumula
/// scendendo la catena fino a raggiungere la soglia, e l'omogeneita' di fascia
/// diventa una preferenza (i tier migliori restano in testa) invece di un
/// vincolo che fa fallire il panel.
pub(super) async fn select_models_tierchain(
    db: &PgPool,
    filter: &EligibilityFilter<'_>,
    tier_chain: &[&str],
    order_by: &str,
    limit: i64,
    min_distinct_providers: usize,
) -> Result<Vec<(String, String, Option<String>)>, String> {
    let esclusioni = esclusioni_selezione(filter.exclude_providers, filter.apply_cooldown);
    let excluded = &esclusioni.fornitori;
    // Le due colonne parallele dell'anti-join sulle coppie: stessa lunghezza per
    // costruzione, perche' nascono dalla stessa lista.
    let (coppie_provider, coppie_model): (Vec<String>, Vec<String>) =
        esclusioni.coppie.iter().cloned().unzip();

    let tiers: Vec<Option<&str>> = if tier_chain.is_empty() {
        vec![None]
    } else {
        tier_chain.iter().map(|t| Some(*t)).collect()
    };

    // PUNTO UNICO (regola L) del mapping capability -> colonna canonica del
    // catalog. Le capability con una colonna booleana dedicata (vision + i media
    // kind della mig 0478) si filtrano via colonna; ogni altra capability (chat,
    // 'code', 'reasoning', ...) resta nel jsonb `capabilities`. Aggiungere un
    // nuovo media kind = una riga qui (e la colonna in mig), niente if sparsi.
    let shape = QueryShape {
        capability_column: filter.capability.and_then(capability_to_column),
        // jsonb solo per le capability SENZA colonna dedicata.
        capability_json: filter
            .capability
            .filter(|c| capability_to_column(c).is_none())
            .map(|c| format!("[\"{c}\"]")),
        // Una capability media o vision e' "specializzata": NON va esclusa da se
        // stessa. Le capability TESTUALI (chat/code/None/vision) NON devono pescare
        // modelli media (un image-gen non e' un modello di testo): esclusione
        // esplicita dei flag media quando la capability richiesta NON e' un media kind.
        requested_is_media: filter.capability.map(is_media_capability).unwrap_or(false),
    };

    // Usato solo quando il chiamante chiede diversita' di provider (>= 2).
    let mut accumulate: Vec<(String, String, Option<String>)> = Vec::new();
    let tier_totali = tiers.len();

    for tier in tiers {
        let sql = build_tierchain_sql(filter, &shape, tier, order_by, limit);
        let mut q = sqlx::query_as::<_, (String, String, Option<String>)>(&sql)
            .bind(excluded)
            .bind(&coppie_provider)
            .bind(&coppie_model);
        if let Some(t) = tier {
            q = q.bind(t);
        }
        if let Some(c) = shape.capability_json.as_ref() {
            q = q.bind(c);
        }
        if filter.min_context_window > 0 {
            q = q.bind(filter.min_context_window);
        }
        if let Some(p) = filter.only_provider {
            // Stesso ordine dei placeholder: bind DOPO min_context_window.
            q = q.bind(p.to_lowercase());
        }
        let rows = q
            .fetch_all(db)
            .await
            .map_err(|e| format!("select_models_tierchain: query fallita: {e}"))?;

        if min_distinct_providers <= 1 {
            if !rows.is_empty() {
                return Ok(rows);
            }
            continue;
        }

        // Fan-out: si accumula scendendo, saltando le coppie gia' viste (i tier
        // della catena possono sovrapporsi). L'ordine di visita e' quello della
        // catena, quindi i tier migliori restano in testa al risultato.
        for row in rows {
            if accumulate.iter().any(|(p, m, _)| p == &row.0 && m == &row.1) {
                continue;
            }
            accumulate.push(row);
        }
        let distinti: std::collections::HashSet<&str> =
            accumulate.iter().map(|(p, _, _)| p.as_str()).collect();
        if distinti.len() >= min_distinct_providers {
            return Ok(accumulate);
        }
    }

    // Catena esaurita senza raggiungere la soglia: si restituisce comunque cio'
    // che si e' trovato. Chi ha chiesto la diversita' e' l'unico che sa cosa
    // farne (il panel degrada con `got`/`min` STRUTTURATI, regola M); qui un
    // Vec vuoto al posto di un candidato solo cancellerebbe quell'informazione.
    if !accumulate.is_empty() {
        tracing::info!(
            trovati = accumulate.len(),
            provider_distinti = accumulate
                .iter()
                .map(|(p, _, _)| p.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            richiesti = min_distinct_providers,
            tier_esplorati = tier_totali,
            "tier-chain: diversita' provider non raggiunta a catena esaurita"
        );
        return Ok(accumulate);
    }
    if filter.require_qualified {
        // Pool VUOTO col gate acceso: il sintomo giusto e' "il gate non ha
        // candidati provati" (es. worker di qualificazione fermo, batteria
        // troppo severa, qualificazioni scadute in massa), non un generico
        // "nessun modello". Fail-closed VOLUTO (design gate, regola G): il
        // chiamante gestisce il None; qui il log dice DOVE guardare.
        tracing::warn!(
            capability = filter.capability.unwrap_or("-"),
            "gate qualificazione: NESSUN modello 'qualified' non scaduto per il \
             filtro richiesto — verificare il worker di qualificazione \
             (agent.model_qualification.*) e ai_model_probe_evidence"
        );
    }
    Ok(Vec::new())
}

/// FASE 3 (Stadio 1) — SHADOW-COMPARE del routing per-intent (ADR 0030).
///
/// Opt-in via settings `routing.per_intent_runtime_shadow` (default false). NON
/// cambia la decisione servita all'utente: calcola IN PARALLELO la risoluzione
/// tier-runtime (requirements + cooldown caller-side, stesso ordine di
/// `route_by_slots`) e logga la divergenza vs la decisione STATICA del lookup
/// matrix, per misurare la parita' prima di abilitare il routing runtime (stadi
/// 2-3). Best-effort, non solleva. Chiamare SOLO su intent senza manual_override
/// (il chiamante verifica `RoutingMatrix::is_manual_override`).
pub(crate) async fn shadow_compare_per_intent(
    db: &PgPool,
    intent: &str,
    behavior_mode: &str,
    estimated_tokens: u32,
    static_provider: &str,
    static_model: &str,
) {
    let enabled = crate::settings::get_setting(db, "routing.per_intent_runtime_shadow")
        .await
        .ok()
        .flatten()
        .map(|v| {
            matches!(
                v.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    if !enabled {
        return;
    }
    // Requisito tier per (intent, behavior_mode): STESSA fonte dell'auto-promoter
    // (nexus_intent_routing_requirements). Se manca, nessuno shadow per la chiave.
    let req = sqlx::query_as::<_, (String, Vec<String>, bool, String)>(
        "SELECT preferred_tier, required_capabilities, requires_tool_use, cost_direction \
         FROM nexus_intent_routing_requirements WHERE intent = $1 AND behavior_mode = $2",
    )
    .bind(intent)
    .bind(behavior_mode)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let Some((tier, caps, tool, cost_dir)) = req else {
        return;
    };
    // Risoluzione tier-runtime via il selettore unico (regola L), poi cooldown
    // caller-side: primo candidato con provider non in cooldown (come route_by_slots).
    let candidates = match crate::routing_matrix_auto_promoter::select_models_for_requirement(
        db, &tier, &caps, tool, &cost_dir,
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("shadow_compare_per_intent: select_models fallita: {e}");
            return;
        }
    };
    let (runtime_provider, runtime_model) = candidates
        .into_iter()
        .find(|(p, _)| !crate::provider_cooldown::is_provider_in_cooldown(p))
        .unwrap_or_else(|| ("__no_model__".to_string(), String::new()));
    let is_match = runtime_provider == static_provider && runtime_model == static_model;
    tracing::info!(
        target: "routing_shadow",
        intent = %intent,
        behavior_mode = %behavior_mode,
        estimated_tokens,
        static_provider = %static_provider,
        static_model = %static_model,
        runtime_provider = %runtime_provider,
        runtime_model = %runtime_model,
        is_match,
        "FASE3 shadow-compare per-intent (statico vs tier-runtime)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Schema REALE (regola O): `nexus_intent_routing_requirements` arriva dalla
    /// migrazione (mig 0174), gia' seminata con la riga sentinella `('*','*',
    /// 0.35, 0.25, 0.20, 0.20, 'asc')` (mig 0379). Il DELETE isola i test dai
    /// requirement di produzione — incluso il caso "sentinella assente", che con
    /// lo schema reale altrimenti non sarebbe mai riproducibile.
    async fn create_requirements_table(pool: &sqlx::PgPool) {
        sqlx::query("DELETE FROM nexus_intent_routing_requirements")
            .execute(pool)
            .await
            .expect("pulizia requirements");
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn fetch_default_weights_legge_riga_sentinella(pool: sqlx::PgPool) {
        create_requirements_table(&pool).await;
        sqlx::query(
            "INSERT INTO nexus_intent_routing_requirements \
             (intent, behavior_mode, weight_tier, weight_cost, weight_context, weight_capabilities, cost_direction) \
             VALUES ('*', '*', 0.40, 0.30, 0.15, 0.15, 'desc')",
        )
        .execute(&pool)
        .await
        .expect("insert sentinella");
        let w = fetch_default_weights(&pool).await.expect("pesi presenti");
        assert!((w.tier - 0.40).abs() < 1e-6);
        assert!((w.cost - 0.30).abs() < 1e-6);
        assert!((w.context - 0.15).abs() < 1e-6);
        assert!((w.capabilities - 0.15).abs() < 1e-6);
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn fetch_default_weights_err_se_sentinella_assente(pool: sqlx::PgPool) {
        create_requirements_table(&pool).await;
        // Solo righe per intent reali, nessuna sentinella '*'.
        sqlx::query(
            "INSERT INTO nexus_intent_routing_requirements (intent, behavior_mode) VALUES ('chat', 'bilanciata')",
        )
        .execute(&pool)
        .await
        .expect("insert intent reale");
        let res = fetch_default_weights(&pool).await;
        assert!(
            res.is_err(),
            "senza riga sentinella deve fallire visibilmente (regola G)"
        );
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn tierchain_agentico_sceglie_tool_capable_piu_economico(pool: sqlx::PgPool) {
        // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
             ('mistral', 'caro', true, 'none', 'heavy', 10.0, 10.0, 'USD', now()), \
             ('openai', 'economico', true, 'none', 'heavy', 2.0, 2.0, 'USD', now()), \
             ('google', 'no-tool', false, 'none', 'heavy', 0.5, 0.5, 'USD', now())",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let f = EligibilityFilter {
            require_tool_use: true,
            require_thinking_non_exclude: true,
            capability: None,
            min_context_window: 0,
            min_tier: None,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out = select_models_tierchain(
            &pool,
            &f,
            &["heavy"],
            "input_cost_per_million_tokens ASC",
            1,
            1,
        )
        .await
        .expect("ok");
        // Esclude no-tool; tra i tool-capable sceglie il piu' economico.
        assert_eq!(
            out,
            vec![(
                "openai".to_string(),
                "economico".to_string(),
                Some("heavy".to_string())
            )]
        );
    }

    /// Il seed della mig 0690 rende kimi VISIBILE alla catena agentica che filtra
    /// su `reasoning` — la sola cosa che ai tre provider precedenti e' mancata.
    ///
    /// Groq e OpenRouter furono registrati correttamente (chiave, flag, registry,
    /// catalog) e non venivano MAI scelti: la causa dominante, misurata il
    /// 13/07/2026, era che i loro modelli erano seedati con `["chat","code"]`
    /// mentre sette intent agentici pretendono `capabilities @> ["reasoning"]`.
    /// Servi' la mig 0582 a posteriori. Nessun test copriva quel salto, e non
    /// poteva coprirlo un test sulla stringa del seed: la domanda e' se la
    /// SELEZIONE li trovi.
    ///
    /// Il test parte dalle righe che la MIGRAZIONE ha scritto (il migrator
    /// embedded le applica davvero: non si ricostruisce il seed a mano, regola O)
    /// e attraversa `select_models_tierchain`, cioe' il punto unico della WHERE di
    /// eleggibilita' usato da tutta la famiglia `best_model_for_tier*` /
    /// `select_agentic_model*`.
    ///
    /// L'abilitazione la fa il test, e non e' una scorciatoia: `is_enabled` e' un
    /// fatto d'esercizio che nasce dal probe reale sul provider (gate mig 0629),
    /// non un contenuto del seed. Cio' che qui si verifica e' che il seed porti
    /// tutto il resto — capability, tool, thinking policy, prezzo — perche' il
    /// modello sia eleggibile NON APPENA il probe passa.
    ///
    /// MUTAZIONE DI CONTROLLO: togliendo `"reasoning"` dalle capabilities nella
    /// mig 0690, la prima asserzione trova zero candidati.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn kimi_seedato_e_visibile_alla_catena_agentica(pool: sqlx::PgPool) {
        // Cio' che la migrazione ha scritto davvero, non un INSERT di comodo.
        let seedati: i64 =
            sqlx::query_scalar("SELECT count(*) FROM ai_price_catalog WHERE provider = 'kimi'")
                .fetch_one(&pool)
                .await
                .expect("conteggio seed kimi");
        assert!(
            seedati > 0,
            "la mig 0690 non ha seedato alcun modello kimi: il resto del test non prova nulla"
        );

        // Il probe reale non puo' girare qui: se ne simula l'ESITO, che e' l'unico
        // pezzo mancante fra il seed e l'eleggibilita'.
        sqlx::query(
            "UPDATE ai_price_catalog SET is_enabled = true, last_probe_healthy_at = now(), \
             auto_disabled_reason = NULL WHERE provider = 'kimi'",
        )
        .execute(&pool)
        .await
        .expect("simulazione esito probe");

        let filtro = |capability| EligibilityFilter {
            require_tool_use: true,
            require_thinking_non_exclude: true,
            capability,
            min_context_window: 0,
            min_tier: None,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: Some("kimi"),
            require_qualified: false,
            exclude_preview: true,
        };

        // Nessun tier nella catena: si guarda l'eleggibilita', non la fascia (che
        // al seed e' volutamente NULL, perche' la scrivono l'indice e la batteria).
        let agentici = select_models_tierchain(
            &pool,
            &filtro(Some("reasoning")),
            &[],
            "input_cost_per_million_tokens ASC",
            10,
            1,
        )
        .await
        .expect("query di selezione");
        assert!(
            !agentici.is_empty(),
            "nessun modello kimi eleggibile per gli intent che pretendono reasoning: \
             e' lo stesso stato in cui groq e openrouter sono rimasti per mesi"
        );

        // E per la capability con cui il fornitore si presenta.
        let di_codice = select_models_tierchain(
            &pool,
            &filtro(Some("code")),
            &[],
            "input_cost_per_million_tokens ASC",
            10,
            1,
        )
        .await
        .expect("query di selezione");
        assert!(!di_codice.is_empty(), "nessun modello kimi eleggibile per code");

        // Il piu' economico vince l'ordinamento per costo: e' la stessa regola con
        // cui la selezione agentica sceglie fra pari.
        assert_eq!(
            agentici.first().map(|(p, m, _)| (p.as_str(), m.as_str())),
            Some(("kimi", "kimi-k2.6")),
            "l'ordine per costo non corrisponde ai prezzi di listino seedati"
        );
    }

    /// TEST 6 — only_provider (PIN): `Some(p)` RESTRINGE la selezione al solo
    /// provider `p` (filtro positivo bindato); `None` = query identica alla
    /// precedente (nessuna regressione per i chiamanti storici). Discriminante:
    /// senza pin vince il piu' economico (mistral); col pin='openai' vince openai
    /// anche se piu' caro (il pin e' preferenza-forte tier-aware).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn tierchain_only_provider_restringe_e_none_invariato(pool: sqlx::PgPool) {
        // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
             ('mistral', 'm-economico', true, 'none', 'heavy', 2.0, 2.0, 'USD', now()), \
             ('openai', 'o-caro', true, 'none', 'heavy', 10.0, 10.0, 'USD', now())",
        )
        .execute(&pool)
        .await
        .expect("insert");
        // None: nessuna restrizione -> il piu' economico (mistral).
        let f_none = EligibilityFilter {
            require_tool_use: true,
            require_thinking_non_exclude: true,
            capability: None,
            min_context_window: 0,
            min_tier: None,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out_none = select_models_tierchain(
            &pool,
            &f_none,
            &["heavy"],
            "input_cost_per_million_tokens ASC",
            1,
            1,
        )
        .await
        .expect("ok");
        assert_eq!(
            out_none,
            vec![(
                "mistral".to_string(),
                "m-economico".to_string(),
                Some("heavy".to_string())
            )],
            "None: query invariata, vince il piu' economico"
        );
        // Some('openai'): restringe a openai anche se piu' caro.
        let f_pin = EligibilityFilter {
            only_provider: Some("openai"),
            ..f_none.clone()
        };
        let out_pin = select_models_tierchain(
            &pool,
            &f_pin,
            &["heavy"],
            "input_cost_per_million_tokens ASC",
            1,
            1,
        )
        .await
        .expect("ok");
        assert_eq!(
            out_pin,
            vec![(
                "openai".to_string(),
                "o-caro".to_string(),
                Some("heavy".to_string())
            )],
            "Some('openai'): filtro positivo, solo openai"
        );
        // Some di un provider ASSENTE dal catalog -> nessun candidato (pool vuoto).
        let f_absent = EligibilityFilter {
            only_provider: Some("deepseek"),
            ..f_none.clone()
        };
        let out_absent = select_models_tierchain(
            &pool,
            &f_absent,
            &["heavy"],
            "input_cost_per_million_tokens ASC",
            1,
            1,
        )
        .await
        .expect("ok");
        assert!(
            out_absent.is_empty(),
            "provider pinnato assente -> pool vuoto"
        );
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn tierchain_preferisce_policy_none_su_dual_mode(pool: sqlx::PgPool) {
        // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        // Stesso costo: il TIE-BREAKER (policy='none' DESC, ultimo criterio dopo
        // order_by) fa vincere il nativamente non-thinking A PARITA' di order_by.
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
             ('a', 'dual', true, 'disable_for_tools', 'heavy', 1.0, 1.0, 'USD', now()), \
             ('b', 'nativo', true, 'none', 'heavy', 1.0, 1.0, 'USD', now())",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let f = EligibilityFilter {
            require_tool_use: true,
            require_thinking_non_exclude: true,
            capability: None,
            min_context_window: 0,
            min_tier: None,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out = select_models_tierchain(
            &pool,
            &f,
            &["heavy"],
            "input_cost_per_million_tokens ASC",
            1,
            1,
        )
        .await
        .expect("ok");
        assert_eq!(
            out,
            vec![(
                "b".to_string(),
                "nativo".to_string(),
                Some("heavy".to_string())
            )]
        );
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn tierchain_capacita_costo_vince_sul_tiebreaker_thinking(pool: sqlx::PgPool) {
        // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        // REGRESSIONE (fix routing agentico, regola H): con i modelli forti moderni
        // tutti dual-mode ('disable_for_tools') e i 'none' rimasti deboli/legacy, il
        // criterio PRIMARIO deve essere order_by (qui: costo), NON la policy thinking.
        // Il forte ed economico ('forte', disable_for_tools, 0.14) deve battere il
        // debole piu' caro ('debole', none, 1.0): col vecchio pre-ordinamento PRIMARIO
        // avrebbe vinto 'debole' (causa radice di "agentic usa deepseek-coder").
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
             ('a', 'forte', true, 'disable_for_tools', 'medium', 0.14, 0.14, 'USD', now()), \
             ('b', 'debole', true, 'none', 'medium', 1.0, 1.0, 'USD', now())",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let f = EligibilityFilter {
            require_tool_use: true,
            require_thinking_non_exclude: true,
            capability: None,
            min_context_window: 0,
            min_tier: None,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out = select_models_tierchain(
            &pool,
            &f,
            &["medium"],
            "input_cost_per_million_tokens ASC",
            1,
            1,
        )
        .await
        .expect("ok");
        assert_eq!(
            out,
            vec![(
                "a".to_string(),
                "forte".to_string(),
                Some("medium".to_string())
            )]
        );
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn tierchain_esclude_policy_exclude_quando_richiesto(pool: sqlx::PgPool) {
        // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
             ('a', 'escluso', true, 'exclude', 'heavy', 1.0, 1.0, 'USD', now())",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let f = EligibilityFilter {
            require_tool_use: true,
            require_thinking_non_exclude: true,
            capability: None,
            min_context_window: 0,
            min_tier: None,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out = select_models_tierchain(
            &pool,
            &f,
            &["heavy"],
            "input_cost_per_million_tokens ASC",
            1,
            1,
        )
        .await
        .expect("ok");
        assert!(
            out.is_empty(),
            "agentic_thinking_policy='exclude' deve essere escluso"
        );
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn tierchain_degrada_al_tier_successivo(pool: sqlx::PgPool) {
        // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        // Nessun heavy: la chain deve scendere a medium.
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, agentic_thinking_policy, performance_tier, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
             ('a', 'medio', true, 'none', 'medium', 1.0, 1.0, 'USD', now())",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let f = EligibilityFilter {
            require_tool_use: true,
            require_thinking_non_exclude: true,
            capability: None,
            min_context_window: 0,
            min_tier: None,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out = select_models_tierchain(
            &pool,
            &f,
            &["heavy", "medium"],
            "input_cost_per_million_tokens ASC",
            1,
            1,
        )
        .await
        .expect("ok");
        assert_eq!(
            out,
            vec![(
                "a".to_string(),
                "medio".to_string(),
                Some("medium".to_string())
            )]
        );
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn tierchain_vision_via_supports_vision(pool: sqlx::PgPool) {
        // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, supports_vision, performance_tier, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
             ('a', 'no-vision', false, false, 'medium', 1.0, 1.0, 'USD', now()), \
             ('b', 'vision', false, true, 'medium', 1.0, 1.0, 'USD', now())",
        )
        .execute(&pool)
        .await
        .expect("insert");
        // Ramo non-agentico: nessun filtro tool/policy, capability='vision'.
        let f = EligibilityFilter {
            require_tool_use: false,
            require_thinking_non_exclude: false,
            capability: Some("vision"),
            min_context_window: 0,
            min_tier: None,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out = select_models_tierchain(
            &pool,
            &f,
            &["medium"],
            "is_featured DESC, input_cost_per_million_tokens ASC",
            1,
            1,
        )
        .await
        .expect("ok");
        assert_eq!(
            out,
            vec![(
                "b".to_string(),
                "vision".to_string(),
                Some("medium".to_string())
            )]
        );
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn tierchain_capability_none_esclude_media(pool: sqlx::PgPool) {
        // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        // Un image-gen tool-capable (assurdo, ma testa che l'esclusione media
        // scatti a prescindere) NON deve entrare nel routing chat (capability=None).
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, supports_image_gen, agentic_thinking_policy, performance_tier, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
             ('openai', 'dall-e-3', true, true, 'none', 'medium', 0.1, 0.1, 'USD', now()), \
             ('openai', 'gpt-4o', true, false, 'none', 'medium', 1.0, 1.0, 'USD', now())",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let f = EligibilityFilter {
            require_tool_use: false,
            require_thinking_non_exclude: false,
            capability: None,
            min_context_window: 0,
            min_tier: None,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out = select_models_tierchain(
            &pool,
            &f,
            &["medium"],
            "input_cost_per_million_tokens ASC",
            5,
            1,
        )
        .await
        .expect("ok");
        // Solo il chat: il media (image-gen) e' escluso dai purpose testuali.
        assert_eq!(
            out,
            vec![(
                "openai".to_string(),
                "gpt-4o".to_string(),
                Some("medium".to_string())
            )]
        );
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn tierchain_image_gen_via_supports_image_gen(pool: sqlx::PgPool) {
        // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, supports_tool_use, supports_image_gen, performance_tier, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
             ('openai', 'gpt-4o', false, false, 'light', 1.0, 1.0, 'USD', now()), \
             ('openai', 'gpt-image-1', false, true, 'light', 1.0, 1.0, 'USD', now())",
        )
        .execute(&pool)
        .await
        .expect("insert");
        // Gemello del test vision: capability='image_gen' deve filtrare via colonna
        // canonica supports_image_gen e selezionare SOLO il modello media.
        let f = EligibilityFilter {
            require_tool_use: false,
            require_thinking_non_exclude: false,
            capability: Some("image_gen"),
            min_context_window: 0,
            min_tier: None,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified: false,
            exclude_preview: false,
        };
        let out = select_models_tierchain(
            &pool,
            &f,
            &["light"],
            "is_featured DESC, input_cost_per_million_tokens ASC",
            5,
            1,
        )
        .await
        .expect("ok");
        assert_eq!(
            out,
            vec![(
                "openai".to_string(),
                "gpt-image-1".to_string(),
                Some("light".to_string())
            )]
        );
    }

    #[test]
    fn capability_to_column_mappa_solo_le_capability_con_colonna() {
        assert_eq!(capability_to_column("vision"), Some("supports_vision"));
        assert_eq!(
            capability_to_column("image_gen"),
            Some("supports_image_gen")
        );
        assert_eq!(capability_to_column("audio_in"), Some("supports_audio_in"));
        assert_eq!(
            capability_to_column("audio_out"),
            Some("supports_audio_out")
        );
        assert_eq!(
            capability_to_column("video_gen"),
            Some("supports_video_gen")
        );
        // capability nel jsonb: nessuna colonna dedicata.
        assert_eq!(capability_to_column("code"), None);
        assert_eq!(capability_to_column("reasoning"), None);
    }

    #[test]
    fn is_media_capability_distingue_media_da_testuali() {
        assert!(is_media_capability("image_gen"));
        assert!(is_media_capability("audio_in"));
        assert!(is_media_capability("audio_out"));
        assert!(is_media_capability("video_gen"));
        // vision NON e' media (e' una capability di input testuale-multimodale).
        assert!(!is_media_capability("vision"));
        assert!(!is_media_capability("code"));
    }

    #[test]
    fn esclusioni_selezione_normalizza_e_deduplica() {
        // Cooldown NON applicato: verifichiamo la normalizzazione/dedup degli
        // extra, che restano fornitori interi.
        let out = esclusioni_selezione(&["OpenAI".into(), "openai".into(), "Google".into()], false);
        assert!(out.fornitori.contains(&"openai".to_string()));
        assert!(out.fornitori.contains(&"google".to_string()));
        // "OpenAI" e "openai" collassano in un solo elemento.
        assert_eq!(out.fornitori.iter().filter(|p| *p == "openai").count(), 1);
        assert!(
            out.coppie.is_empty(),
            "senza cooldown non ci sono coppie escluse"
        );
    }

    /// D1 — la selezione ANTICIPA la coppia in cooldown.
    ///
    /// Il difetto misurato il 13/08/2026: `chiave_cooldown` produceva
    /// `provider` oppure `provider\u{1}model`, e la selezione proiettava quella
    /// chiave GREZZA in `AND LOWER(provider) <> ALL($1)`. Una chiave composta non
    /// eguaglia nessun `provider` del catalogo, quindi la coppia in cooldown
    /// restava fra i candidati: veniva scelta, mandata, e il gateway la rifiutava
    /// attendendo — un giro di selezione sprecato piu' l'attesa, ogni volta.
    ///
    /// La misura attraversa la catena REALE (regola O): il produttore
    /// `metti_in_cooldown_breve`, il punto unico `coppie_in_cooldown`, la query
    /// costruita da `build_tierchain_sql` e il catalog con lo schema di
    /// produzione. Nessuna chiave scritta a mano, nessun SQL ricopiato.
    ///
    /// MUTAZIONE 1: togliere l'anti-join sulle coppie da `build_tierchain_sql` ->
    /// il primo assert rosseggia (la coppia satura torna fra i candidati).
    /// MUTAZIONE 2: far ricadere `fornitori_in_cooldown` su tutte le voci dello
    /// snapshot -> il secondo assert rosseggia (il modello sano dello stesso
    /// fornitore sparisce, cioe' il difetto del 07/08 rientra dalla porta del
    /// lettore).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_selezione_anticipa_la_coppia_in_cooldown(pool: sqlx::PgPool) {
        // Nomi propri di questo test: lo stato del cooldown e' globale al processo.
        let fornitore = "__test_sel_coppia";
        let saturo = "modello-saturo";
        let sano = "modello-sano";
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
             ($1, $2, 1.0, 1.0, 'USD', now()), \
             ($1, $3, 2.0, 2.0, 'USD', now())",
        )
        .bind(fornitore)
        .bind(saturo)
        .bind(sano)
        .execute(&pool)
        .await
        .expect("insert");

        let filtro = EligibilityFilter {
            apply_cooldown: true,
            ..gate_filter(false, false)
        };
        // Prima del cooldown entrambi i modelli sono candidati.
        let prima = select_models_tierchain(
            &pool,
            &filtro,
            &[],
            "input_cost_per_million_tokens ASC",
            10,
            1,
        )
        .await
        .expect("ok");
        assert_eq!(prima.len(), 2, "premessa: entrambi eleggibili");

        // Il rate limit colpisce UN modello: e' un tetto suo, non del fornitore.
        crate::provider_cooldown::metti_in_cooldown_breve(
            fornitore,
            Some(saturo),
            "Rate limit raggiunto",
            60,
        );
        let dopo = select_models_tierchain(
            &pool,
            &filtro,
            &[],
            "input_cost_per_million_tokens ASC",
            10,
            1,
        )
        .await
        .expect("ok");
        let modelli: Vec<&str> = dopo.iter().map(|(_, m, _)| m.as_str()).collect();
        assert!(
            !modelli.contains(&saturo),
            "la coppia in cooldown non deve essere nemmeno proposta: {modelli:?}"
        );
        assert!(
            modelli.contains(&sano),
            "l'altro modello dello stesso fornitore ha quota propria e resta candidato: {modelli:?}"
        );
        crate::provider_cooldown::remove_cooldown(fornitore);
    }

    // ── Gate di qualificazione (mig 0591/0592) ────────────────────────────────
    // Incidenti 2026-07-14/15: il routing pinnava alle figure del consiglio
    // modelli DICHIARATI nel catalog ma mai provati (404 su Vertex) o pre-GA in
    // quota condivisa satura (429). Il gate richiede l'EVIDENZA.

    /// Filtro agentico base dei test del gate (i flag del gate variano per test).
    fn gate_filter(require_qualified: bool, exclude_preview: bool) -> EligibilityFilter<'static> {
        EligibilityFilter {
            require_tool_use: true,
            require_thinking_non_exclude: true,
            capability: None,
            min_context_window: 0,
            min_tier: None,
            exclude_providers: &[],
            apply_cooldown: false,
            only_provider: None,
            require_qualified,
            exclude_preview,
        }
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn gate_qualificazione_esclude_i_non_provati_e_gli_scaduti(pool: sqlx::PgPool) {
        // Schema REALE (regola O): il DELETE isola dal catalog di produzione. Il
        // gate qui e' INIETTATO da `gate_filter`, non letto da `settings`.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, input_cost_per_million_tokens, output_cost_per_million_tokens, qualification_state, qualification_expires_at, currency, last_probe_healthy_at) VALUES \
             ('a', 'dichiarato-mai-provato', 1.0, 1.0, 'unqualified', NULL, 'USD', now()), \
             ('b', 'provato-ma-scaduto',     2.0, 2.0, 'qualified',   NOW() - interval '1 hour', 'USD', now()), \
             ('c', 'provato-valido',         3.0, 3.0, 'qualified',   NOW() + interval '1 day', 'USD', now())",
        )
        .execute(&pool)
        .await
        .expect("insert");
        // Gate ACCESO: resta solo il provato non scaduto, anche se costa di piu'.
        let out = select_models_tierchain(
            &pool,
            &gate_filter(true, false),
            &[],
            "input_cost_per_million_tokens ASC",
            10,
            1,
        )
        .await
        .expect("ok");
        assert_eq!(
            out.iter().map(|(_, m, _)| m.as_str()).collect::<Vec<_>>(),
            vec!["provato-valido"],
            "il gate deve escludere unqualified e qualified scaduto"
        );
        // Gate SPENTO: comportamento storico, tutti e tre eleggibili.
        let out = select_models_tierchain(
            &pool,
            &gate_filter(false, false),
            &[],
            "input_cost_per_million_tokens ASC",
            10,
            1,
        )
        .await
        .expect("ok");
        assert_eq!(out.len(), 3, "gate spento = comportamento storico");
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn gate_capability_verificata_sul_provato_non_sul_dichiarato(pool: sqlx::PgPool) {
        // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        // 'millantatore' DICHIARA reasoning ma il qualificatore non gliel'ha
        // provato; 'provato' ce l'ha in qualified_capabilities.
        sqlx::query(
            "INSERT INTO ai_price_catalog \
             (provider, model, input_cost_per_million_tokens, output_cost_per_million_tokens, capabilities, qualification_state, qualified_capabilities, currency, last_probe_healthy_at) VALUES \
             ('a', 'millantatore', 1.0, 1.0, '[\"chat\",\"reasoning\"]', 'qualified', '[]', 'USD', now()), \
             ('b', 'provato',      2.0, 2.0, '[\"chat\",\"reasoning\"]', 'qualified', '[\"chat\",\"reasoning\"]', 'USD', now())",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let mut f = gate_filter(true, false);
        f.capability = Some("reasoning");
        let out = select_models_tierchain(&pool, &f, &[], "input_cost_per_million_tokens ASC", 10, 1)
            .await
            .expect("ok");
        assert_eq!(
            out.iter().map(|(_, m, _)| m.as_str()).collect::<Vec<_>>(),
            vec!["provato"],
            "col gate la capability si verifica su qualified_capabilities"
        );
        // Gate spento: si crede al dichiarato (comportamento storico).
        let mut f = gate_filter(false, false);
        f.capability = Some("reasoning");
        let out = select_models_tierchain(&pool, &f, &[], "input_cost_per_million_tokens ASC", 10, 1)
            .await
            .expect("ok");
        assert_eq!(out.len(), 2);
    }

    /// REGRESSIONE (2026-07-27): il gate letto per UN database non deve valere
    /// per un ALTRO. Qui viveva una cache statica di processo senza chiave: il
    /// primo che leggeva fissava il gate per tutti per 60s.
    ///
    /// Nei test l'effetto era misurabile e ricorrente — un
    /// `#[sqlx::test(migrator = "META_MIGRATOR")]` gira su un DB dove la mig 0595
    /// accende `enforce_routing_gate`, e da li' in poi ogni altro test del
    /// processo si vedeva il catalog svuotato (i sei test di `internal_routing`
    /// falliti/passati a seconda di chi partiva per primo). In produzione era lo
    /// stesso difetto: mcp-core interroga il DB meta e un `<slug>_nexus` per
    /// progetto, e la configurazione dell'uno decideva le selezioni dell'altro.
    ///
    /// Il test attraversa `qualification_gate` (la funzione della produzione, non
    /// una sua imitazione) su DUE database vivi nello stesso processo, nell'ordine
    /// che rompeva: prima quello col flag acceso.
    ///
    /// MUTAZIONE: rimettendo una cache di processo non chiavata, la seconda
    /// lettura torna `require_qualified = true` e l'asserzione fallisce.
    #[sqlx::test]
    async fn il_gate_di_un_database_non_decide_per_un_altro(pool: sqlx::PgPool) {
        use sqlx::postgres::PgPoolOptions;

        // Il DB gemello nasce accanto a quello del fixture, sullo STESSO cluster:
        // le sue coordinate si derivano dal pool, non da una URL ricopiata. Il
        // nome e' corto e univoco: quello del fixture occupa gia' i 63 byte che
        // Postgres concede a un identificatore, e derivarne uno per suffisso
        // significherebbe farselo TRONCARE addosso — cioe' droppare il database
        // del test in corso.
        let opts = pool.connect_options().as_ref().clone();
        let gemello = format!("gate_iso_{}", uuid::Uuid::new_v4().simple());
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone().database("postgres"))
            .await
            .expect("connessione al database di manutenzione");
        sqlx::query(&format!("CREATE DATABASE \"{gemello}\""))
            .execute(&admin)
            .await
            .expect("creazione del database gemello");

        let pool_gemello = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts.database(&gemello))
            .await
            .expect("connessione al database gemello");
        crate::test_support::create_settings_table_with(&pool_gemello, ENFORCE_ROUTING_GATE_KEY, "true")
            .await;
        crate::test_support::seed_setting(&pool_gemello, EXCLUDE_PREVIEW_AGENTIC_KEY, "true").await;

        // Il DB del fixture ha la tabella, ma NESSUNA delle due chiavi: il gate
        // deve restare spento (fail-safe storico).
        crate::test_support::create_settings_table(&pool).await;

        let acceso = qualification_gate(&pool_gemello).await;
        assert!(
            acceso.require_qualified && acceso.exclude_preview,
            "il gemello ha entrambe le chiavi a 'true': {acceso:?}"
        );

        let qui = qualification_gate(&pool).await;

        pool_gemello.close().await;
        sqlx::query(&format!("DROP DATABASE IF EXISTS \"{gemello}\""))
            .execute(&admin)
            .await
            .expect("rimozione del database gemello");

        assert!(
            !qui.require_qualified && !qui.exclude_preview,
            "il gate di un database non puo' decidere per un altro: qui le chiavi \
             non ci sono e il gate deve restare spento. Ricevuto: {qui:?}"
        );
    }

    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn exclude_preview_taglia_i_pre_ga_ma_non_i_ga(pool: sqlx::PgPool) {
        // Schema REALE (regola O): il DELETE isola dal catalog di produzione.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        sqlx::query(
            "INSERT INTO ai_price_catalog (provider, model, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES \
             ('g', 'gemini-3.1-pro-preview',  1.0, 1.0, 'USD', now()), \
             ('g', 'gemini-2.0-flash-exp',    1.1, 1.1, 'USD', now()), \
             ('g', 'gemini-exp-1206',         1.2, 1.2, 'USD', now()), \
             ('x', 'modello-experimental',    1.3, 1.3, 'USD', now()), \
             ('g', 'gemini-2.5-flash',        2.0, 2.0, 'USD', now()), \
             ('m', 'model-express',           3.0, 3.0, 'USD', now())",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let out = select_models_tierchain(
            &pool,
            &gate_filter(false, true),
            &[],
            "input_cost_per_million_tokens ASC",
            10,
            1,
        )
        .await
        .expect("ok");
        let models: Vec<&str> = out.iter().map(|(_, m, _)| m.as_str()).collect();
        assert_eq!(
            models,
            vec!["gemini-2.5-flash", "model-express"],
            "i pre-GA (preview/-exp/experimental) sono esclusi; i GA e i nomi \
             che CONTENGONO 'exp' senza esserlo (express) restano"
        );
    }
    /// IL TEST PONTE fra le due meta' del vocabolario (regola L).
    ///
    /// La scala dei tier deve vivere in UN posto solo, ma Rust e SQL sono
    /// linguaggi diversi: l'espressione SQL e' GENERATA da `tier_rank_sql` a
    /// partire dalle stesse `PERFORMANCE_TIERS`/`tier_rank`. Questo test chiude
    /// il cerchio provandola su POSTGRES VERO: se le due meta' divergessero —
    /// com'era successo con la scala a 3 livelli di `agent_run.rs`, dove
    /// `frontier` e `high` collassavano su 0 come `light` — qui diventa rosso.
    ///
    /// Copre anche i casi che il CASE scritto a mano sbagliava piu' spesso: il
    /// tier NULL (la colonna sta per diventare nullable) e un valore fuori
    /// vocabolario, che devono prendere lo stesso rank neutro di `tier_rank`.
    #[sqlx::test]
    async fn tier_rank_sql_coincide_col_rank_rust(pool: sqlx::PgPool) {
        use nexus_agent_graph::decisions::tiers::{tier_rank, tier_rank_sql, PERFORMANCE_TIERS};

        let expr = tier_rank_sql("t");
        for tier in PERFORMANCE_TIERS {
            let sql_rank: i32 = sqlx::query_scalar(&format!("SELECT {expr} FROM (SELECT $1::text AS t) s"))
                .bind(tier)
                .fetch_one(&pool)
                .await
                .expect("query rank");
            assert_eq!(
                sql_rank as u8,
                tier_rank(tier),
                "Postgres e Rust ordinano '{tier}' in modo diverso: la scala si e'                  sdoppiata (SQL={sql_rank}, Rust={})",
                tier_rank(tier)
            );
        }
        // Tolleranza identica: maiuscole e spazi.
        let sql_rank: i32 = sqlx::query_scalar(&format!("SELECT {expr} FROM (SELECT '  HEAVY '::text AS t) s"))
            .fetch_one(&pool)
            .await
            .expect("query rank");
        assert_eq!(sql_rank as u8, tier_rank("  HEAVY "));
        // Valore ignoto e NULL -> rank neutro, come tier_rank.
        for ignoto in ["ultra", "fast"] {
            let sql_rank: i32 = sqlx::query_scalar(&format!("SELECT {expr} FROM (SELECT $1::text AS t) s"))
                .bind(ignoto)
                .fetch_one(&pool)
                .await
                .expect("query rank");
            assert_eq!(sql_rank as u8, tier_rank(ignoto), "'{ignoto}' deve avere il rank neutro");
        }
        let sql_null: i32 = sqlx::query_scalar(&format!("SELECT {expr} FROM (SELECT NULL::text AS t) s"))
            .fetch_one(&pool)
            .await
            .expect("query rank null");
        assert_eq!(
            sql_null as u8,
            tier_rank(""),
            "un tier NULL deve prendere il rank neutro, non sparire dall'ordinamento"
        );
    }

    /// L'ordinamento REALE sul catalog: il difetto misurato il 15/07 era che
    /// l'escalation "sali al modello piu' capace" sceglieva un heavy scartando i
    /// frontier. Con l'espressione generata il primo e' il frontier.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn ordinare_col_rank_generato_mette_il_frontier_in_testa(pool: sqlx::PgPool) {
        use nexus_agent_graph::decisions::tiers::tier_rank_sql;
        // Schema REALE (regola O): il DELETE isola dal catalog di produzione —
        // essenziale qui, l'assert confronta l'ORDINE ESATTO di tutte le righe.
        sqlx::query("DELETE FROM ai_price_catalog")
            .execute(&pool)
            .await
            .expect("pulizia catalog");
        sqlx::query(
            "INSERT INTO ai_price_catalog (provider, model, performance_tier, input_cost_per_million_tokens, output_cost_per_million_tokens, currency, last_probe_healthy_at) VALUES              ('openai', 'gpt-frontier', 'frontier', 1.0, 1.0, 'USD', now()),              ('openai', 'gpt-heavy', 'heavy', 1.0, 1.0, 'USD', now()),              ('mistral', 'mistral-medium', 'medium', 1.0, 1.0, 'USD', now()),              ('openai', 'gpt-high', 'high', 1.0, 1.0, 'USD', now()),              ('openai', 'gpt-light', 'light', 1.0, 1.0, 'USD', now())",
        )
        .execute(&pool)
        .await
        .expect("insert");
        let ordinati: Vec<String> = sqlx::query_scalar(&format!(
            "SELECT model FROM ai_price_catalog ORDER BY {} DESC",
            tier_rank_sql("performance_tier")
        ))
        .fetch_all(&pool)
        .await
        .expect("select");
        assert_eq!(
            ordinati,
            vec!["gpt-frontier", "gpt-heavy", "gpt-high", "mistral-medium", "gpt-light"],
            "l'ordine deve seguire la scala a 5 livelli; col CASE a 3 livelli              frontier e high finivano in fondo, sotto il medium"
        );
    }
}

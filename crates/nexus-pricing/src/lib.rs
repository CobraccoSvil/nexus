//! Listino dei modelli AI: PUNTO UNICO (regola L / ADR 0026).
//!
//! Risponde a UNA domanda: *quanto costa (provider, model) adesso?* La domanda e'
//! una sola; le POLICY su cosa fare quando la risposta non c'e' sono dei chiamanti
//! (il gateway degrada e annota il ledger, la stima di quota non blocca nulla).
//! Qui vivono il calcolo e l'interpretazione dello stato del listino; nessuna
//! decisione di prodotto.
//!
//! ## Perche' esiste
//!
//! La stessa funzione era scritta tre volte (mcp-core, nexus-gateway,
//! billing-service) e le tre copie erano DIVERGENTI su assi che cambiano i soldi
//! (billing-service e' poi stato rimosso: era un fork mai attivato):
//!
//! | Asse | mcp-core | nexus-gateway | billing-service |
//! |---|---|---|---|
//! | filtro `is_enabled` | no (corretto) | si | si |
//! | currency di default | `USD` | `EUR` | `EUR` |
//! | `pricing_state` | ignorato | ignorato | ignorato |
//! | voce mancante | `{0,0}` + warn | `{0,0}` muto | `{0,0}` muto |
//!
//! Il default `EUR` non e' un dettaglio: ha gia' prodotto un incidente (righe di
//! ledger con `currency='EUR'` e `total_cost=0`, corretto dalla migrazione 0294
//! allineando `billing_base_currency='USD'`). Due copie su tre lo avevano ancora.
//!
//! Meccanismo (regola L): logica stateless + IO singolo -> funzioni in un modulo.
//! Niente trait, niente struct con stato.
//!
//! ## Decisioni incorporate una volta sola
//!
//! - **Nessun filtro `is_enabled`.** `is_enabled` e' il gate del ROUTING (chi puo'
//!   usare cosa), non della contabilita' di una chiamata GIA' avvenuta. Filtrarlo
//!   produce una sottostima silenziosa sui run che hanno usato un modello poi
//!   disabilitato (pin espliciti, run legacy).
//! - **`pricing_state` nella STESSA query** che legge il prezzo: guida l'esito
//!   (regola M). Leggerlo da una query separata puo' vedere una riga diversa.
//! - **Nessun default di currency** (regola G): se `billing_base_currency` manca,
//!   [`platform_currency`] propaga l'errore. La visibilita' si ottiene all'AVVIO
//!   con [`assert_configured`], dove fallire e' gratuito; a runtime i chiamanti
//!   sul percorso della richiesta degradano esplicitamente invece di respingerla.
//! - **`i64` sui token**: le copie divergevano tra `i32` e `i64`. Il cast int->int
//!   in Rust e' wrapping, quindi la firma stretta era un troncamento latente.

use anyhow::{Context, Result};
use sqlx::PgPool;

/// Chiave del setting che dichiara la currency di piattaforma.
pub const CURRENCY_SETTING: &str = "billing_base_currency";

/// Prezzo di un modello, per milione di token, nella currency dichiarata.
///
/// Le due tariffe di cache sono `Option` e non `f64`: a catalog le colonne sono
/// NULLABLE (mig 0130) e la 0403 popola solo alcuni provider. `NULL` significa
/// "tariffa di cache ignota", che NON e' "cache gratis" — collassare i due casi
/// con un `unwrap_or(0.0)` sarebbe il magic fallback vietato dalla regola G, e
/// renderebbe i token di cache gratuiti per costruzione. Il ripiego dichiarato
/// vive in [`calculate_cost_breakdown`].
#[derive(Debug, Clone, PartialEq)]
pub struct PriceSnapshot {
    pub input_cost_per_million_tokens: f64,
    pub output_cost_per_million_tokens: f64,
    /// Tariffa dei token di prompt serviti da cache (`0.1x`-`0.5x` dell'input a
    /// seconda del provider). `None` = non a listino.
    pub cache_read_cost_per_million_tokens: Option<f64>,
    /// Tariffa dei token scritti in cache (Anthropic `1.25x` dell'input).
    /// `None` = non a listino; per i provider con caching automatico non esiste
    /// proprio un costo di scrittura.
    pub cache_creation_cost_per_million_tokens: Option<f64>,
    pub currency: String,
}

/// Esito della ricerca del listino: tre stati DISTINTI.
///
/// Un `Option<PriceSnapshot>` (o peggio un `PriceSnapshot{0,0}`) li collassa in
/// uno solo, e "costa zero" diventa indistinguibile da "non so quanto costa" —
/// il magic fallback che la regola G vieta.
#[derive(Debug, Clone, PartialEq)]
pub enum PriceLookup {
    /// Listino noto. Include il gratuito REALE (`pricing_state = 'free'`): li' lo
    /// zero e' un prezzo, non l'assenza di un prezzo.
    Priced(PriceSnapshot),
    /// La riga esiste ma il listino non e' noto (`pricing_state = 'unknown'`,
    /// mig 0477): il costo 0 sarebbe un PLACEHOLDER. Tipico dei modelli scoperti
    /// via API, che non espongono i prezzi.
    Unknown,
    /// Nessuna riga attiva a catalogo per (provider, model) in questa currency.
    NotInCatalog,
}

/// Etichetta "listino noto", condivisa dai tre stati che la telemetria del
/// ledger confronta fra loro (`details.price_state` del listino a token e di
/// quello a unita', `details.cache_price_state`).
///
/// Scritta una volta sola: e' un identificatore macchina (regola N) e ogni copia
/// e' un posto in cui puo' divergere in silenzio da chi la legge.
const STATO_PRICED: &str = "priced";

impl PriceLookup {
    /// Etichetta stabile per la telemetria (`details.price_state` del ledger) e i
    /// log. Identificatore macchina, non testo umano (regola M).
    pub fn state_label(&self) -> &'static str {
        match self {
            PriceLookup::Priced(_) => STATO_PRICED,
            PriceLookup::Unknown => "unknown",
            PriceLookup::NotInCatalog => "not_in_catalog",
        }
    }

    /// `true` se il costo NON e' calcolabile: nessun prezzo da applicare.
    pub fn is_missing(&self) -> bool {
        !matches!(self, PriceLookup::Priced(_))
    }
}

/// Currency di piattaforma, dal setting `billing_base_currency`.
///
/// PROPAGA: nessun default hardcoded (regola G). Un default silenzioso qui e' gia'
/// costato 3.993 righe di ledger orfane (`currency='EUR'` + `total_cost=0`) prima
/// della mig 0294. La lettura passa dal punto unico dei settings di `nexus-auth`,
/// che a sua volta e' l'unico posto dove vive la query su `settings`.
pub async fn platform_currency(db: &PgPool) -> Result<String> {
    let raw = nexus_auth::get_setting_nonempty(db, CURRENCY_SETTING)
        .await
        .with_context(|| format!("lettura del setting '{CURRENCY_SETTING}' fallita"))?;
    let currency = raw.ok_or_else(|| {
        anyhow::anyhow!(
            "setting '{CURRENCY_SETTING}' assente o vuoto: la currency di piattaforma non e' \
             configurata e non viene indovinata (regola G). Applicare la migrazione 0294 o \
             valorizzare il setting."
        )
    })?;
    Ok(currency.to_uppercase())
}

/// Verifica all'AVVIO che il listino sia configurato. Da chiamare nel bootstrap
/// dei servizi che fatturano.
///
/// Fallire qui e' gratuito e rumoroso; fallire a ogni richiesta sarebbe un outage.
/// Stesso pattern di `RoutingMatrixCache::init` (CLAUDE.md, regola G).
pub async fn assert_configured(db: &PgPool) -> Result<()> {
    let currency = platform_currency(db).await?;
    tracing::info!(currency = %currency, "nexus-pricing: currency di piattaforma configurata");
    Ok(())
}

/// Prezzo attivo per (provider, model) nella currency di piattaforma.
///
/// Una sola query: prezzo e `pricing_state` letti insieme, cosi' l'esito non puo'
/// riferirsi a righe diverse.
pub async fn resolve_active_price(db: &PgPool, provider: &str, model: &str) -> Result<PriceLookup> {
    let currency = platform_currency(db).await?;
    resolve_active_price_in(db, provider, model, &currency).await
}

/// Come [`resolve_active_price`] ma con la currency gia' risolta dal chiamante
/// (evita di rileggere il setting quando ne serve piu' d'uno nello stesso giro).
pub async fn resolve_active_price_in(
    db: &PgPool,
    provider: &str,
    model: &str,
    currency: &str,
) -> Result<PriceLookup> {
    // NB: nessun filtro `is_enabled` — vedi la doc del modulo.
    let row = sqlx::query_as::<_, CatalogPriceRow>(
        "SELECT input_cost_per_million_tokens::float8 AS input_cost_per_million_tokens, \
                output_cost_per_million_tokens::float8 AS output_cost_per_million_tokens, \
                cache_read_cost_per_million_tokens::float8 AS cache_read_cost_per_million_tokens, \
                cache_creation_cost_per_million_tokens::float8 \
                    AS cache_creation_cost_per_million_tokens, \
                currency, \
                pricing_state \
           FROM ai_price_catalog \
          WHERE provider = $1 \
            AND model = $2 \
            AND currency = $3 \
            AND effective_from <= NOW() \
            AND (effective_to IS NULL OR effective_to > NOW()) \
          ORDER BY effective_from DESC \
          LIMIT 1",
    )
    .bind(provider)
    .bind(model)
    .bind(currency)
    .fetch_optional(db)
    .await
    .with_context(|| format!("lettura listino di {provider}/{model} fallita"))?;

    Ok(interpret_row(row))
}

/// Riga di listino letta dal catalog.
///
/// E' una struct con NOMI e non una tupla posizionale: con quattro `f64`
/// adiacenti (input, output, cache read, cache creation) uno scambio di
/// posizione non e' un errore che il compilatore veda, ed e' un errore che si
/// paga in denaro. `FromRow` lega ogni campo al nome della colonna.
#[derive(Debug, Clone, sqlx::FromRow)]
struct CatalogPriceRow {
    input_cost_per_million_tokens: f64,
    output_cost_per_million_tokens: f64,
    cache_read_cost_per_million_tokens: Option<f64>,
    cache_creation_cost_per_million_tokens: Option<f64>,
    currency: String,
    pricing_state: String,
}

/// Parte PURA: dalla riga del catalog all'esito. Estratta per essere testabile
/// senza DB (il punto sensibile e' l'interpretazione, non la query).
fn interpret_row(row: Option<CatalogPriceRow>) -> PriceLookup {
    match row {
        None => PriceLookup::NotInCatalog,
        Some(r) if r.pricing_state == "unknown" => PriceLookup::Unknown,
        Some(r) => PriceLookup::Priced(PriceSnapshot {
            input_cost_per_million_tokens: r.input_cost_per_million_tokens,
            output_cost_per_million_tokens: r.output_cost_per_million_tokens,
            cache_read_cost_per_million_tokens: r.cache_read_cost_per_million_tokens,
            cache_creation_cost_per_million_tokens: r.cache_creation_cost_per_million_tokens,
            currency: r.currency,
        }),
    }
}

/// Token di una chiamata, nella convenzione del sistema: `prompt_tokens` e' il
/// LORDO e i due campi di cache ne sono SOTTOINSIEMI.
///
/// La convenzione e' fissata alla FONTE da `LlmUsage::normalized`
/// (`crates/nexus-gateway/src/types.rs`), che porta al lordo i provider che
/// riportano il prompt gia' al netto. Qui si assume e basta: questo crate non
/// deve sapere chi ha risposto (portare un `match provider` dentro il listino
/// sarebbe la regola L a rovescio).
///
/// Lo SCORPORO — quanti token pagano la tariffa piena — avviene solo dentro
/// [`calculate_cost_breakdown`], perche' e' l'unica domanda a cui il netto
/// risponde.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// Token di prompt LORDI: comprendono i due conteggi di cache qui sotto.
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    /// Token di prompt serviti da cache (sottoinsieme di `prompt_tokens`).
    pub cache_read_tokens: i64,
    /// Token di prompt scritti in cache (sottoinsieme di `prompt_tokens`).
    pub cache_creation_tokens: i64,
}

impl TokenUsage {
    /// Il caso senza cache: la forma delle stime EX-ANTE, dove i cache hit non
    /// sono conoscibili perche' la chiamata non e' ancora avvenuta.
    pub fn senza_cache(prompt_tokens: i64, completion_tokens: i64) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        }
    }

    /// Totale dei token della chiamata: prompt LORDO + completion.
    ///
    /// Serve a quote e report, che misurano il consumo e non la tariffa. I token
    /// di cache sono gia' dentro `prompt_tokens`: sommarli di nuovo qui li
    /// conterebbe due volte.
    pub fn total_tokens(&self) -> i64 {
        self.prompt_tokens.max(0) + self.completion_tokens.max(0)
    }
}

/// Costo di una chiamata, voce per voce.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CostBreakdown {
    pub input_cost: f64,
    pub output_cost: f64,
    pub cache_read_cost: f64,
    pub cache_creation_cost: f64,
    /// Somma delle quattro voci.
    pub total_cost: f64,
    /// Token di cache tariffati a prezzo PIENO di input perche' il listino non
    /// porta la loro tariffa. `0` = nessun ripiego. Segnale STRUTTURATO
    /// (regola M): finisce in `details.cache_price_state` del ledger, cosi' un
    /// `cache_read_cost = 0` per "tariffa ignota" resta distinguibile da un
    /// `cache_read_cost = 0` per "nessun token da cache".
    pub cache_tokens_billed_as_input: i64,
}

impl CostBreakdown {
    /// Etichetta stabile dello stato del listino di CACHE, con lo stesso
    /// vocabolario di [`PriceLookup::state_label`].
    pub fn cache_price_state(&self) -> &'static str {
        if self.cache_tokens_billed_as_input > 0 {
            "cache_price_missing"
        } else if self.cache_read_cost > 0.0 || self.cache_creation_cost > 0.0 {
            STATO_PRICED
        } else {
            "no_cache_tokens"
        }
    }
}

/// Costo di una chiamata dal prompt LORDO, dai due conteggi di cache e dal
/// listino.
///
/// PUNTO UNICO del calcolo (regola L): insieme a [`UnitPriceLookup::cost_for`] e'
/// l'unico posto del workspace che moltiplica un prezzo per una quantita' — ed e'
/// anche l'UNICO posto in cui il prompt viene scorporato. Ovunque altrove
/// `prompt_tokens` e' il lordo, perche' il netto risponde a una sola domanda (a
/// quanti token si applica la tariffa piena) e quella domanda si pone qui.
///
/// Lo scorporo e' `lordo - cache_read - cache_creation`, con clamp a `>= 0`: se
/// un provider riportasse piu' token di cache che di prompt (dato incoerente) il
/// monte a tariffa piena e' zero, mai un numero negativo che genererebbe credito.
///
/// ## Cosa succede se la tariffa di cache manca dal listino
///
/// I token di cache tornano nel monte a tariffa PIENA di input e il fatto viene
/// DICHIARATO in `cache_tokens_billed_as_input`. Le alternative sono peggiori:
///
/// - tariffarli a 0 li renderebbe gratis per costruzione — magic fallback
///   (regola G) e sottostima silenziosa, cioe' l'errore opposto a quello che
///   questo lavoro chiude;
/// - inventare un rapporto (0.1x, 0.25x...) sarebbe un prezzo non a listino,
///   scritto nel codice invece che nel DB (regola G di nuovo).
///
/// Il ripiego ha inoltre una proprieta' che serve: rimette nel monte esattamente
/// i token che ne erano stati tolti, quindi con il listino di cache assente il
/// costo e' ESATTAMENTE quello che il sistema calcolava prima di questo lavoro
/// (tutto il prompt a tariffa piena). Non peggiora: resta quello di oggi.
/// L'identita' vale sui conteggi coerenti (cache <= lordo, l'unico caso che un
/// provider produce); sul dato incoerente il clamp e' gia' intervenuto e si
/// fattura la cache dichiarata, che e' il comportamento conservativo.
pub fn calculate_cost_breakdown(price: &PriceSnapshot, usage: &TokenUsage) -> CostBreakdown {
    let per_milione = |tokens: i64, tariffa: f64| (tokens.max(0) as f64 / 1_000_000.0) * tariffa;

    let cache_read = usage.cache_read_tokens.max(0);
    let cache_creation = usage.cache_creation_tokens.max(0);

    // Lo scorporo: i token di cache escono dal monte a tariffa piena. Le due
    // quantita' sono sottoinsiemi del lordo, mai addendi.
    let mut prompt_a_tariffa_piena =
        (usage.prompt_tokens.max(0) - cache_read - cache_creation).max(0);
    // Ogni voce di cache senza tariffa RIENTRA nel monte input a prezzo pieno.
    let mut ripiego = 0_i64;

    let cache_read_cost = match price.cache_read_cost_per_million_tokens {
        Some(t) => per_milione(cache_read, t),
        None => {
            prompt_a_tariffa_piena += cache_read;
            ripiego += cache_read;
            0.0
        }
    };
    let cache_creation_cost = match price.cache_creation_cost_per_million_tokens {
        Some(t) => per_milione(cache_creation, t),
        None => {
            prompt_a_tariffa_piena += cache_creation;
            ripiego += cache_creation;
            0.0
        }
    };

    let input_cost = per_milione(prompt_a_tariffa_piena, price.input_cost_per_million_tokens);
    let output_cost = per_milione(
        usage.completion_tokens,
        price.output_cost_per_million_tokens,
    );

    CostBreakdown {
        input_cost,
        output_cost,
        cache_read_cost,
        cache_creation_cost,
        total_cost: input_cost + output_cost + cache_read_cost + cache_creation_cost,
        cache_tokens_billed_as_input: ripiego,
    }
}

/// Costo `(input, output, totale)` dati prezzo e token, SENZA cache.
///
/// Resta per le stime EX-ANTE (quota, prenotazione), dove i cache hit non sono
/// conoscibili. Delega a [`calculate_cost_breakdown`] invece di rifare la
/// moltiplicazione: la formula sta scritta una volta sola.
///
/// `i64` sui token: le copie storiche divergevano (`i32` in mcp-core e
/// billing-service, `i64` nel gateway). I chiamanti con `i32` fanno widening,
/// che e' sicuro.
pub fn calculate_cost(
    price: &PriceSnapshot,
    prompt_tokens: i64,
    completion_tokens: i64,
) -> (f64, f64, f64) {
    let b = calculate_cost_breakdown(
        price,
        &TokenUsage::senza_cache(prompt_tokens, completion_tokens),
    );
    (b.input_cost, b.output_cost, b.total_cost)
}

// ── Listino per unita' non-token (immagini, secondi, caratteri) ─────────────
//
// Le modalita' non-testuali del gateway non si pagano a token: un'immagine, un
// secondo di video o di audio, un carattere sintetizzato. Il listino a token non
// puo' esprimerle e `calculate_cost` non e' estendibile senza snaturarla, quindi
// qui convivono due funzioni di calcolo — ma restano UNA sola fonte (regola L):
// nessun altro modulo puo' moltiplicare un prezzo per una quantita'.

/// Unita' di consumo di una chiamata non-testuale. E' un enum e non una stringa
/// perche' finisce in una colonna con CHECK e in una chiave di listino: un refuso
/// non deve poter arrivare al DB (regola M).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageUnit {
    /// Immagini generate.
    Image,
    /// Secondi di audio o video, in ingresso o in uscita.
    Second,
    /// Caratteri sintetizzati (TTS).
    Character,
}

impl UsageUnit {
    /// Etichetta che va nel DB (`ai_usage_ledger.quantity_unit`,
    /// `ai_price_catalog_unit.unit`). Coincide coi CHECK della mig 0634.
    pub fn as_str(&self) -> &'static str {
        match self {
            UsageUnit::Image => "image",
            UsageUnit::Second => "second",
            UsageUnit::Character => "character",
        }
    }
}

/// Prezzo di UNA unita' (non per milione, a differenza di [`PriceSnapshot`]).
#[derive(Debug, Clone, PartialEq)]
pub struct UnitPriceSnapshot {
    pub unit_cost: f64,
    pub unit: UsageUnit,
    pub currency: String,
}

/// Esito della ricerca nel listino per-unita'. Tipo PROPRIO, non un
/// [`PriceLookup`] con dentro un prezzo unitario travestito da prezzo per
/// milione: far viaggiare un numero in un campo che significa un'altra cosa e'
/// il difetto che questo lavoro sta bonificando altrove.
///
/// Due stati e non tre: qui non esiste l'equivalente di
/// `pricing_state = 'unknown'`, perche' una riga in `ai_price_catalog_unit` la
/// si inserisce solo quando il prezzo lo si conosce davvero.
#[derive(Debug, Clone, PartialEq)]
pub enum UnitPriceLookup {
    Priced(UnitPriceSnapshot),
    /// Nessun listino per questa (provider, model, unita'). Con la tabella
    /// ancora vuota e' l'esito NORMALE, e va mostrato come tale: significa
    /// "non so quanto costa", mai "e' gratis".
    NotInCatalog,
}

impl UnitPriceLookup {
    /// Etichetta per `details.price_state` del ledger, allineata a quella del
    /// listino a token cosi' i due percorsi si leggono con lo stesso vocabolario.
    pub fn state_label(&self) -> &'static str {
        match self {
            UnitPriceLookup::Priced(_) => STATO_PRICED,
            UnitPriceLookup::NotInCatalog => "not_in_catalog",
        }
    }

    /// Costo totale della quantita' consumata, o `None` se il prezzo non e'
    /// noto. Il chiamante che riceve `None` scrive 0 DICHIARANDO il perche' in
    /// `details.price_state`, non un importo inventato.
    pub fn cost_for(&self, quantity: f64) -> Option<f64> {
        match self {
            UnitPriceLookup::Priced(p) => Some(quantity.max(0.0) * p.unit_cost),
            UnitPriceLookup::NotInCatalog => None,
        }
    }
}

/// Prezzo attivo per (provider, model, unita') nella currency data.
pub async fn resolve_unit_price(
    db: &PgPool,
    provider: &str,
    model: &str,
    unit: UsageUnit,
    currency: &str,
) -> Result<UnitPriceLookup> {
    let row = sqlx::query_as::<_, (f64, String)>(
        "SELECT unit_cost::float8, currency \
           FROM ai_price_catalog_unit \
          WHERE provider = $1 \
            AND model = $2 \
            AND unit = $3 \
            AND currency = $4 \
            AND effective_from <= NOW() \
            AND (effective_to IS NULL OR effective_to > NOW()) \
          ORDER BY effective_from DESC \
          LIMIT 1",
    )
    .bind(provider)
    .bind(model)
    .bind(unit.as_str())
    .bind(currency)
    .fetch_optional(db)
    .await
    .with_context(|| {
        format!(
            "lettura listino per-{} di {provider}/{model} fallita",
            unit.as_str()
        )
    })?;

    Ok(interpret_unit_row(row, unit))
}

/// Parte PURA: dalla riga del listino unitario all'esito. Estratta per essere
/// testabile senza DB, come [`interpret_row`] per il listino a token.
fn interpret_unit_row(row: Option<(f64, String)>, unit: UsageUnit) -> UnitPriceLookup {
    match row {
        None => UnitPriceLookup::NotInCatalog,
        Some((unit_cost, currency)) => UnitPriceLookup::Priced(UnitPriceSnapshot {
            unit_cost,
            unit,
            currency,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(input: f64, output: f64, state: &str) -> Option<CatalogPriceRow> {
        Some(CatalogPriceRow {
            input_cost_per_million_tokens: input,
            output_cost_per_million_tokens: output,
            cache_read_cost_per_million_tokens: None,
            cache_creation_cost_per_million_tokens: None,
            currency: "USD".to_string(),
            pricing_state: state.to_string(),
        })
    }

    /// Listino con le tariffe di cache valorizzate, come le popola la mig 0403.
    fn row_con_cache(
        input: f64,
        output: f64,
        cache_read: Option<f64>,
        cache_creation: Option<f64>,
    ) -> Option<CatalogPriceRow> {
        Some(CatalogPriceRow {
            input_cost_per_million_tokens: input,
            output_cost_per_million_tokens: output,
            cache_read_cost_per_million_tokens: cache_read,
            cache_creation_cost_per_million_tokens: cache_creation,
            currency: "USD".to_string(),
            pricing_state: "priced".to_string(),
        })
    }

    fn prezzo(
        input: f64,
        output: f64,
        cache_read: Option<f64>,
        cache_creation: Option<f64>,
    ) -> PriceSnapshot {
        match interpret_row(row_con_cache(input, output, cache_read, cache_creation)) {
            PriceLookup::Priced(p) => p,
            other => panic!("atteso Priced, ottenuto {other:?}"),
        }
    }

    /// Listino assente e listino a zero sono due cose diverse anche per le unita'.
    #[test]
    fn unita_listino_assente_non_e_gratis() {
        let assente = interpret_unit_row(None, UsageUnit::Image);
        assert_eq!(assente, UnitPriceLookup::NotInCatalog);
        // Nessun costo calcolabile: il chiamante deve scrivere 0 dichiarando il
        // perche', non un importo dedotto.
        assert_eq!(assente.cost_for(3.0), None);
        assert_eq!(assente.state_label(), "not_in_catalog");

        // Un prezzo REALE a zero (modello gratuito) resta un prezzo: da' Some(0.0).
        let gratis = interpret_unit_row(Some((0.0, "USD".into())), UsageUnit::Image);
        assert_eq!(gratis.cost_for(3.0), Some(0.0));
        assert_eq!(gratis.state_label(), "priced");
    }

    /// Il costo e' quantita' x prezzo unitario, non diviso per un milione.
    #[test]
    fn costo_unitario_moltiplica_la_quantita() {
        let p = interpret_unit_row(Some((0.04, "USD".into())), UsageUnit::Image);
        assert_eq!(p.cost_for(3.0), Some(0.12));

        let audio = interpret_unit_row(Some((0.006, "USD".into())), UsageUnit::Second);
        let atteso = 42.0 * 0.006;
        let ottenuto = audio.cost_for(42.0).unwrap();
        assert!((ottenuto - atteso).abs() < 1e-9, "atteso {atteso}, ottenuto {ottenuto}");
    }

    /// Una quantita' negativa non genera un credito.
    #[test]
    fn quantita_negativa_non_produce_costo_negativo() {
        let p = interpret_unit_row(Some((0.04, "USD".into())), UsageUnit::Image);
        assert_eq!(p.cost_for(-5.0), Some(0.0));
    }

    /// Le etichette finiscono nel DB sotto CHECK: devono combaciare con la
    /// migrazione 0634, non somigliarle.
    #[test]
    fn etichette_unita_allineate_ai_check_della_migrazione() {
        assert_eq!(UsageUnit::Image.as_str(), "image");
        assert_eq!(UsageUnit::Second.as_str(), "second");
        assert_eq!(UsageUnit::Character.as_str(), "character");
    }

    #[test]
    fn i_tre_stati_sono_distinti() {
        assert!(matches!(interpret_row(None), PriceLookup::NotInCatalog));
        assert!(matches!(
            interpret_row(row(0.0, 0.0, "unknown")),
            PriceLookup::Unknown
        ));
        assert!(matches!(
            interpret_row(row(2.0, 6.0, "priced")),
            PriceLookup::Priced(_)
        ));
    }

    #[test]
    fn free_e_un_prezzo_non_un_ignoto() {
        // REGRESSIONE: 'free' e' il gratuito REALE. Se finisse in Unknown, il
        // modello verrebbe trattato come non contabilizzabile e (via il ciclo
        // prezzo del catalog_sync) reso non routabile — l'opposto dell'intento.
        let v = interpret_row(row(0.0, 0.0, "free"));
        match v {
            PriceLookup::Priced(p) => {
                assert_eq!(p.input_cost_per_million_tokens, 0.0);
                assert_eq!(p.output_cost_per_million_tokens, 0.0);
            }
            other => panic!("free deve essere Priced, non {other:?}"),
        }
    }

    #[test]
    fn priced_disabilitato_resta_priced() {
        // REGRESSIONE (misurata: 109 righe di ledger su 5 modelli oggi disabled,
        // 1.741.020 token, TUTTE priced): la query non filtra is_enabled, quindi
        // un modello disabilitato ma chiamato viene comunque prezzato. Filtrarlo
        // (come faceva billing-service) e' una sottostima permanente.
        // Qui si fissa l'invariante dell'interpretazione: lo stato del listino non
        // dipende dall'abilitazione.
        assert!(matches!(
            interpret_row(row(0.15, 0.6, "priced")),
            PriceLookup::Priced(_)
        ));
    }

    #[test]
    fn state_label_e_is_missing_coerenti() {
        assert_eq!(interpret_row(None).state_label(), "not_in_catalog");
        assert_eq!(interpret_row(row(0.0, 0.0, "unknown")).state_label(), "unknown");
        assert_eq!(interpret_row(row(1.0, 2.0, "priced")).state_label(), "priced");
        assert!(interpret_row(None).is_missing());
        assert!(interpret_row(row(0.0, 0.0, "unknown")).is_missing());
        assert!(!interpret_row(row(1.0, 2.0, "priced")).is_missing());
    }

    #[test]
    fn calculate_cost_scala_per_milione() {
        // Migrato da nexus-gateway (unico test esistente delle 3 copie).
        let p = prezzo(3.0, 15.0, None, None);
        let (i, o, t) = calculate_cost(&p, 1_000_000, 1_000_000);
        assert!((i - 3.0).abs() < 1e-9);
        assert!((o - 15.0).abs() < 1e-9);
        assert!((t - 18.0).abs() < 1e-9);
    }

    #[test]
    fn calculate_cost_riproduce_il_caso_reale() {
        // Sub-run 3259e65f: 63.177 in / 4.103 out su x-ai/grok-4.5 (2/6) = 0,150972.
        let p = prezzo(2.0, 6.0, None, None);
        let (_, _, t) = calculate_cost(&p, 63_177, 4_103);
        assert!((t - 0.150972).abs() < 1e-9, "atteso 0.150972, ottenuto {t}");
    }

    #[test]
    fn token_negativi_non_generano_credito() {
        let p = prezzo(3.0, 15.0, None, None);
        let (i, o, t) = calculate_cost(&p, -5, -5);
        assert_eq!((i, o, t), (0.0, 0.0, 0.0));

        // Anche sulle voci di cache: un conteggio negativo non genera credito.
        let b = calculate_cost_breakdown(
            &prezzo(3.0, 15.0, Some(0.3), Some(3.75)),
            &TokenUsage {
                prompt_tokens: -1,
                completion_tokens: -1,
                cache_read_tokens: -1,
                cache_creation_tokens: -1,
            },
        );
        assert_eq!(b.total_cost, 0.0);
        assert_eq!(b.cache_tokens_billed_as_input, 0);
    }

    /// Le tariffe di cache devono ARRIVARE dalla riga di catalog: prima non
    /// erano nemmeno selezionate, e il costo di cache non era calcolabile.
    #[test]
    fn il_listino_porta_le_tariffe_di_cache() {
        let p = prezzo(3.0, 15.0, Some(0.3), Some(3.75));
        assert_eq!(p.cache_read_cost_per_million_tokens, Some(0.3));
        assert_eq!(p.cache_creation_cost_per_million_tokens, Some(3.75));

        // NULL a catalog resta `None`: "tariffa ignota", non "gratis".
        let senza = prezzo(3.0, 15.0, None, None);
        assert_eq!(senza.cache_read_cost_per_million_tokens, None);
        assert_eq!(senza.cache_creation_cost_per_million_tokens, None);
    }

    /// Il test che ROSSEGGIA se i token di cache tornassero a pagare la tariffa
    /// piena di input.
    ///
    /// Numeri scelti perche' le due formule NON coincidono: listino Anthropic
    /// tipico (input 3.0, cache read 0.1x = 0.3, cache creation 1.25x = 3.75),
    /// prompt LORDO di 2M token di cui 1M letto da cache e 1M scritto — quindi
    /// zero token a tariffa piena.
    ///   - a tariffa piena (formula di prima): 2M x 3.0 / 1M      = 6.0
    ///   - scorporato:      1M x 0.3 + 1M x 3.75                  = 4.05
    #[test]
    fn i_token_di_cache_non_pagano_la_tariffa_piena_di_input() {
        let p = prezzo(3.0, 15.0, Some(0.3), Some(3.75));
        let usage = TokenUsage {
            // LORDO: i due conteggi di cache qui sotto ne sono sottoinsiemi.
            prompt_tokens: 2_000_000,
            completion_tokens: 0,
            cache_read_tokens: 1_000_000,
            cache_creation_tokens: 1_000_000,
        };
        let b = calculate_cost_breakdown(&p, &usage);

        assert!(
            (b.cache_read_cost - 0.3).abs() < 1e-9,
            "cache_read: {}",
            b.cache_read_cost
        );
        assert!(
            (b.cache_creation_cost - 3.75).abs() < 1e-9,
            "cache_creation: {}",
            b.cache_creation_cost
        );
        // Lo scorporo ha svuotato il monte a tariffa piena: 2M - 1M - 1M = 0.
        assert_eq!(b.input_cost, 0.0);
        assert_eq!(b.cache_tokens_billed_as_input, 0);
        assert!(
            (b.total_cost - 4.05).abs() < 1e-9,
            "totale: {}",
            b.total_cost
        );

        // La conseguenza, non la stringa: il costo e' MINORE di quello che la
        // formula a tariffa piena produrrebbe sullo STESSO prompt lordo.
        let a_tariffa_piena = calculate_cost(&p, usage.prompt_tokens, 0).2;
        assert!((a_tariffa_piena - 6.0).abs() < 1e-9);
        assert!(
            b.total_cost < a_tariffa_piena,
            "lo scorporo deve costare meno: {} vs {a_tariffa_piena}",
            b.total_cost
        );
    }

    /// Prompt lordo minore dei token di cache dichiarati (dato incoerente del
    /// provider): il monte a tariffa piena clampa a zero, mai un negativo che
    /// genererebbe credito sul totale.
    #[test]
    fn cache_maggiore_del_prompt_non_genera_credito() {
        let b = calculate_cost_breakdown(
            &prezzo(3.0, 15.0, Some(0.3), None),
            &TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 0,
                cache_read_tokens: 1_000_000,
                cache_creation_tokens: 0,
            },
        );
        assert_eq!(b.input_cost, 0.0);
        assert!((b.total_cost - 0.3).abs() < 1e-9, "totale {}", b.total_cost);
    }

    /// Tariffa di cache assente: il costo resta ESATTAMENTE quello di oggi, non
    /// peggiore, e il ripiego e' dichiarato.
    #[test]
    fn senza_tariffa_di_cache_il_costo_resta_quello_di_oggi() {
        let p = prezzo(3.0, 15.0, None, None);
        let usage = TokenUsage {
            // 10.000 token di prompt LORDI, di cui 9.000 serviti da cache.
            prompt_tokens: 10_000,
            completion_tokens: 500,
            cache_read_tokens: 9_000,
            cache_creation_tokens: 0,
        };
        let b = calculate_cost_breakdown(&p, &usage);

        // Senza tariffa di cache i 9.000 token rientrano nel monte da cui erano
        // stati tolti: tutto il prompt a tariffa piena, come prima di questo
        // lavoro. Lo stesso numero, non uno peggiore.
        let oggi = calculate_cost(&p, 10_000, 500);
        assert!(
            (b.total_cost - oggi.2).abs() < 1e-12,
            "{} vs {}",
            b.total_cost,
            oggi.2
        );

        // Zero a listino NON significa zero costo: il ripiego e' visibile.
        assert_eq!(b.cache_read_cost, 0.0);
        assert_eq!(b.cache_tokens_billed_as_input, 9_000);
        assert_eq!(b.cache_price_state(), "cache_price_missing");
    }

    /// I tre stati del listino di cache sono distinguibili senza guardare gli
    /// importi (regola M): uno zero ha sempre un perche'.
    #[test]
    fn lo_stato_del_listino_cache_e_un_segnale_strutturato() {
        let con_tariffa = prezzo(3.0, 15.0, Some(0.3), None);
        let senza_tariffa = prezzo(3.0, 15.0, None, None);
        let con_cache = TokenUsage {
            prompt_tokens: 1_010,
            completion_tokens: 10,
            cache_read_tokens: 1_000,
            cache_creation_tokens: 0,
        };
        let senza_cache = TokenUsage::senza_cache(10, 10);

        assert_eq!(
            calculate_cost_breakdown(&con_tariffa, &con_cache).cache_price_state(),
            "priced"
        );
        assert_eq!(
            calculate_cost_breakdown(&senza_tariffa, &con_cache).cache_price_state(),
            "cache_price_missing"
        );
        assert_eq!(
            calculate_cost_breakdown(&con_tariffa, &senza_cache).cache_price_state(),
            "no_cache_tokens"
        );
    }

    /// `total_tokens` e' prompt LORDO + completion. I token di cache sono gia'
    /// dentro il prompt: sommarli a parte li conterebbe due volte, e la serie
    /// storica di quote e report avrebbe un gradino.
    #[test]
    fn il_totale_non_somma_due_volte_la_cache() {
        let u = TokenUsage {
            prompt_tokens: 1_050,
            completion_tokens: 20,
            cache_read_tokens: 900,
            cache_creation_tokens: 50,
        };
        assert_eq!(u.total_tokens(), 1_070);
        // Il caso ex-ante non inventa cache.
        assert_eq!(TokenUsage::senza_cache(100, 20).total_tokens(), 120);
    }
}

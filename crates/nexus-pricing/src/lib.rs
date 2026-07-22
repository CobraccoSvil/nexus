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
#[derive(Debug, Clone, PartialEq)]
pub struct PriceSnapshot {
    pub input_cost_per_million_tokens: f64,
    pub output_cost_per_million_tokens: f64,
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

impl PriceLookup {
    /// Etichetta stabile per la telemetria (`details.price_state` del ledger) e i
    /// log. Identificatore macchina, non testo umano (regola M).
    pub fn state_label(&self) -> &'static str {
        match self {
            PriceLookup::Priced(_) => "priced",
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
    let row = sqlx::query_as::<_, (f64, f64, String, String)>(
        "SELECT input_cost_per_million_tokens::float8, \
                output_cost_per_million_tokens::float8, \
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

/// Parte PURA: dalla riga del catalog all'esito. Estratta per essere testabile
/// senza DB (il punto sensibile e' l'interpretazione, non la query).
fn interpret_row(row: Option<(f64, f64, String, String)>) -> PriceLookup {
    match row {
        None => PriceLookup::NotInCatalog,
        Some((_, _, _, state)) if state == "unknown" => PriceLookup::Unknown,
        Some((input_cost_per_million_tokens, output_cost_per_million_tokens, currency, _)) => {
            PriceLookup::Priced(PriceSnapshot {
                input_cost_per_million_tokens,
                output_cost_per_million_tokens,
                currency,
            })
        }
    }
}

/// Costo `(input, output, totale)` dati prezzo e token.
///
/// `i64` sui token: le copie storiche divergevano (`i32` in mcp-core e
/// billing-service, `i64` nel gateway). I chiamanti con `i32` fanno widening,
/// che e' sicuro.
pub fn calculate_cost(
    price: &PriceSnapshot,
    prompt_tokens: i64,
    completion_tokens: i64,
) -> (f64, f64, f64) {
    let input_cost =
        (prompt_tokens.max(0) as f64 / 1_000_000.0) * price.input_cost_per_million_tokens;
    let output_cost =
        (completion_tokens.max(0) as f64 / 1_000_000.0) * price.output_cost_per_million_tokens;
    (input_cost, output_cost, input_cost + output_cost)
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
            UnitPriceLookup::Priced(_) => "priced",
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

    fn row(input: f64, output: f64, state: &str) -> Option<(f64, f64, String, String)> {
        Some((input, output, "USD".to_string(), state.to_string()))
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
        let p = PriceSnapshot {
            input_cost_per_million_tokens: 3.0,
            output_cost_per_million_tokens: 15.0,
            currency: "USD".into(),
        };
        let (i, o, t) = calculate_cost(&p, 1_000_000, 1_000_000);
        assert!((i - 3.0).abs() < 1e-9);
        assert!((o - 15.0).abs() < 1e-9);
        assert!((t - 18.0).abs() < 1e-9);
    }

    #[test]
    fn calculate_cost_riproduce_il_caso_reale() {
        // Sub-run 3259e65f: 63.177 in / 4.103 out su x-ai/grok-4.5 (2/6) = 0,150972.
        let p = PriceSnapshot {
            input_cost_per_million_tokens: 2.0,
            output_cost_per_million_tokens: 6.0,
            currency: "USD".into(),
        };
        let (_, _, t) = calculate_cost(&p, 63_177, 4_103);
        assert!((t - 0.150972).abs() < 1e-9, "atteso 0.150972, ottenuto {t}");
    }

    #[test]
    fn token_negativi_non_generano_credito() {
        let p = PriceSnapshot {
            input_cost_per_million_tokens: 3.0,
            output_cost_per_million_tokens: 15.0,
            currency: "USD".into(),
        };
        let (i, o, t) = calculate_cost(&p, -5, -5);
        assert_eq!((i, o, t), (0.0, 0.0, 0.0));
    }
}

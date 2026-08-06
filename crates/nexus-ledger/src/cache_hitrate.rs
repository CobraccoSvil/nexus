//! Hit-rate di prompt-cache OSSERVATO, per (provider, model).
//!
//! Terza domanda di lettura sul ledger, distinta dalle due di [`crate::quote`]
//! ("quali vincoli sono attivi", "quanto ha consumato questo scope"): *che
//! frazione del prompt questo modello si e' visto servire da cache?* Vive qui e
//! non in `quote.rs` perche' non misura un consumo da confrontare con un tetto —
//! misura un comportamento del provider, e la risposta serve a SCEGLIERE un
//! modello, non ad ammettere o respingere una chiamata.
//!
//! ## Perche' la si chiede
//!
//! La catena di escalation ordinava i modelli su un costo di LISTINO
//! (`blended_cost` della vista `v_model_escalation_chain`, mig 0471): il prezzo
//! pieno dell'input. In un loop agentico il prefisso — system prompt, tool
//! schemas, primi messaggi — e' identico a ogni iterazione, quindi una quota
//! grande e sistematica del prompt costa una frazione. Misurato il 29/07/2026 su
//! 7 giorni: deepseek 67,0% di hit contro mistral 5,2%, e sullo stesso task
//! mistral e' costato 18 volte deepseek.
//!
//! ## Perche' per MODELLO e non per provider
//!
//! Perche' dentro lo stesso provider il valore varia piu' che fra provider
//! diversi (misura del 29/07/2026):
//!
//! | provider | modello | hit |
//! |---|---|---|
//! | mistral | `mistral-small-latest` | 17,1% |
//! | mistral | `mistral-medium-latest` | 0,0% |
//! | openrouter | `z-ai/glm-4.7-flash` | 43,1% |
//! | openrouter | `qwen/qwen3-235b-a22b-2507` | 0,0% |
//!
//! La media per provider (mistral ≈ 5%) cancella proprio il segnale su cui si
//! decide. La chiave e' la coppia, sempre.
//!
//! ## Regola M
//!
//! Il rapporto viene dalle colonne STRUTTURATE `cache_read_tokens` /
//! `prompt_tokens`, mai stimato dal nome del provider o dal testo di una
//! risposta. Le due colonne sono attendibili dal 28/07/2026 (commit 587595b9):
//! prima riportavano zero per costruzione, e uno zero li' non significa "nessun
//! hit" ma "nessuno guardava" — motivo per cui la finestra di default (7 giorni)
//! non ha ragione di allargarsi oltre quella data.
//!
//! ## Quante chiamate servono per MISURARE un hit-rate
//!
//! Dipende dal provider, e ignorarlo produce zeri che sembrano fatti. Due
//! famiglie:
//!
//! - **cache dichiarata**: Anthropic (`cache_control` esplicito) e i provider con
//!   caching automatico deterministico sul prefisso (OpenAI, DeepSeek, Mistral).
//!   La seconda chiamata con lo stesso prefisso riporta l'hit. Due chiamate
//!   bastano, e infatti danno 98-99%.
//! - **cache implicita best-effort**: Google (Gemini 2.5+ su Vertex). Il provider
//!   decide se materializzare la voce, e l'hit arriva dopo un numero VARIABILE di
//!   chiamate. Due non bastano: lo zero che producono non distingue "non serve
//!   cache" da "questa volta non e' toccato".
//!
//! Misurato il 29/07/2026 interrogando Vertex direttamente e leggendo
//! `usageMetadata` grezzo (`gemini-2.5-flash`, prefisso di ~9.900 token, prova
//! ripetuta con prefissi mai visti per partire da cache vuota):
//!
//! | strada | region | primo hit | valore |
//! |---|---|---|---|
//! | diretta a Vertex | `europe-west4` | giro 2 su 4 | `cachedContentTokenCount` 9.190 |
//! | diretta a Vertex | `global` | giro 3 su 4 | `cachedContentTokenCount` 9.189 |
//! | attraverso il gateway | (routing) | giro 5 su 6 | `cache_read_tokens` 9.189 |
//!
//! Quindi: Vertex serve hit impliciti su ENTRAMBE le regioni, il gateway li
//! riceve, li mappa e li scrive nel ledger. Una prova precedente a due sole
//! chiamate aveva dato zero su quattro modelli Google e fatto sospettare un
//! difetto di parsing, di composizione della richiesta o dell'endpoint regionale:
//! non c'era nessuno dei tre. Era lo strumento a non raggiungere l'oggetto come
//! lo raggiunge la produzione (regola O), dove un loop agentico ripete lo stesso
//! prefisso decine di volte.
//!
//! Conseguenza pratica: l'hit-rate di un provider a cache implicita si misura
//! sulla FINESTRA del ledger, dove le chiamate sono migliaia, mai con una prova
//! a due colpi. E' l'ordine di grandezza che questo modulo gia' usa
//! (`min_samples`), e vale la pena ricordarne il motivo.

use anyhow::{Context, Result};
use nexus_pricing::CacheHitRate;
use sqlx::PgPool;
use std::collections::HashMap;

/// Migrazione che porta i due setting qui sotto. Nominata invece che scritta
/// dentro il messaggio d'errore: chi legge "applicare la migrazione" deve poter
/// risalire al file, e un numero ripetuto nella prosa si aggiorna in un posto e
/// si dimentica nell'altro.
const MIGRAZIONE_SETTINGS: &str = "0656_escalation_costo_atteso_cache";

/// Chiave del setting con l'ampiezza in ore della finestra di osservazione.
pub const WINDOW_SETTING: &str = "escalation.cache_hitrate_window_hours";
/// Chiave del setting con il numero minimo di chiamate perche' il rapporto conti.
pub const MIN_SAMPLES_SETTING: &str = "escalation.cache_hitrate_min_samples";

/// Parametri della misura, letti dal DB (regola G: nessun numero nel codice).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HitRateWindow {
    pub window_hours: i64,
    pub min_samples: i64,
}

impl HitRateWindow {
    /// Legge finestra e soglia dai settings (mig 0656).
    ///
    /// PROPAGA l'errore invece di ripiegare su valori scritti qui: un default
    /// silenzioso renderebbe la misura diversa a seconda che il DB risponda, e
    /// due chiamanti otterrebbero hit-rate calcolati su finestre diverse senza
    /// che nulla lo dichiari (regola G, stesso motivo di
    /// `nexus_pricing::platform_currency`).
    pub async fn load(db: &PgPool) -> Result<Self> {
        Ok(Self {
            window_hours: leggi_intero(db, WINDOW_SETTING).await?,
            min_samples: leggi_intero(db, MIN_SAMPLES_SETTING).await?,
        })
    }
}

async fn leggi_intero(db: &PgPool, key: &str) -> Result<i64> {
    let raw = nexus_auth::get_setting_nonempty(db, key)
        .await
        .with_context(|| format!("lettura del setting '{key}' fallita"))?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "setting '{key}' assente o vuoto: la finestra di osservazione \
                 dell'hit-rate non e' configurata e non viene indovinata (regola G). \
                 Applicare la migrazione {MIGRAZIONE_SETTINGS}."
            )
        })?;
    raw.trim().parse::<i64>().map_err(|e| {
        anyhow::anyhow!("setting '{key}' non e' un intero ('{raw}'): {e}")
    })
}

/// Hit-rate osservato per OGNI modello di un provider, in una sola query.
///
/// Batch e non per-modello perche' il chiamante e' la catena di escalation, che
/// ne valuta l'intera lista in un colpo: una query per modello moltiplicherebbe
/// il costo di una scelta che deve essere veloce (sta sul percorso di uscita da
/// un loop agentico gia' in difficolta').
///
/// Un modello ASSENTE dalla mappa e' un modello di cui non si sa nulla: il
/// chiamante lo tratta come [`CacheHitRate::Unknown`], che porta al costo di
/// listino. La mappa non contiene mai `Unknown` come valore — l'assenza E' la
/// dichiarazione di ignoranza, e averla in due forme darebbe due modi di dire la
/// stessa cosa.
///
/// Filtri, e perche':
/// - `status = 'finalized'`: una riga prenotata porta una STIMA, non un fatto
///   misurato; contarla falserebbe il rapporto con numeri che nessun provider ha
///   mai riportato (regola M);
/// - `prompt_tokens > 0`: e' il denominatore, e una riga a prompt zero non dice
///   nulla sull'hit-rate pur contando come campione;
/// - `count(*) >= min_samples`: poche chiamate danno un rapporto instabile.
pub async fn observed_cache_hit_rates(
    db: &PgPool,
    provider: &str,
    window: HitRateWindow,
) -> Result<HashMap<String, CacheHitRate>> {
    if provider.trim().is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query_as::<_, (String, i64, i64, i64)>(
        "SELECT model, \
                COALESCE(sum(prompt_tokens), 0)::bigint, \
                COALESCE(sum(cache_read_tokens), 0)::bigint, \
                count(*)::bigint \
           FROM ai_usage_ledger \
          WHERE provider = $1 \
            AND status = 'finalized' \
            AND prompt_tokens > 0 \
            AND created_at > now() - make_interval(hours => $2::int) \
          GROUP BY model \
         HAVING count(*) >= $3",
    )
    .bind(provider)
    .bind(window.window_hours as i32)
    .bind(window.min_samples)
    .fetch_all(db)
    .await
    .with_context(|| format!("lettura hit-rate di cache per il provider '{provider}'"))?;

    Ok(rows
        .into_iter()
        .filter_map(|(model, prompt_tot, cache_tot, _campioni)| {
            // Denominatore zero: `HAVING` garantisce i campioni, non il monte
            // token. Senza denominatore non c'e' rapporto — e non lo si inventa.
            if prompt_tot <= 0 {
                return None;
            }
            Some((
                model,
                CacheHitRate::observed(cache_tot as f64 / prompt_tot as f64),
            ))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::identita;
    use crate::Identity;
    use nexus_pricing::TokenUsage;
    use serde_json::json;
    use sqlx::PgPool;

    /// Righe FINALIZED scritte dal produttore reale ([`crate::record_tokens`]),
    /// non da un INSERT che ricopia le colonne a mano (regola O): e' la stessa
    /// strada che percorre una chiamata vera, quindi se un domani la scrittura
    /// cambia forma, questo test la segue invece di misurare una sua imitazione.
    async fn seed_finalized(
        pool: &PgPool,
        id: Identity,
        provider: &str,
        model: &str,
        righe: usize,
        prompt: i64,
        cache_read: i64,
    ) {
        for _ in 0..righe {
            let usage = TokenUsage {
                prompt_tokens: prompt,
                completion_tokens: 0,
                cache_read_tokens: cache_read,
                cache_creation_tokens: 0,
            };
            crate::record_tokens(pool, id, provider, model, &usage, "", "test")
                .await
                .expect("record_tokens deve scrivere la riga");
        }
    }

    /// Sposta all'indietro il `created_at` delle righe di un modello.
    ///
    /// Il produttore non espone il tempo — scrive sempre `now()` — quindi l'unico
    /// modo di esercitare la finestra e' spostare l'asse DOPO che la riga e' nata
    /// dalla strada vera. Si tocca solo `created_at`: tutto il resto della riga
    /// resta quello che la produzione avrebbe scritto.
    async fn invecchia(pool: &PgPool, model: &str, ore: i64) {
        sqlx::query(
            "UPDATE ai_usage_ledger \
                SET created_at = created_at - make_interval(hours => $2::int) \
              WHERE model = $1",
        )
        .bind(model)
        .bind(ore as i32)
        .execute(pool)
        .await
        .expect("invecchia righe");
    }

    fn finestra() -> HitRateWindow {
        HitRateWindow {
            window_hours: 168,
            min_samples: 20,
        }
    }

    /// Il rapporto e' per MODELLO: due modelli dello stesso provider con
    /// comportamenti opposti devono restare distinti. E' il caso reale misurato
    /// su mistral (`small-latest` 17,1% contro `medium-latest` 0,0%): una media
    /// per provider li fonderebbe e cancellerebbe il segnale.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn due_modelli_dello_stesso_provider_non_si_mediano(pool: PgPool) {
        let id = identita(&pool).await;
        seed_finalized(&pool, id, "mistral", "small", 25, 1_000, 170).await;
        seed_finalized(&pool, id, "mistral", "medium", 25, 1_000, 0).await;

        let m = observed_cache_hit_rates(&pool, "mistral", finestra())
            .await
            .expect("lettura");

        assert_eq!(m.get("small"), Some(&CacheHitRate::Observed(0.17)));
        assert_eq!(m.get("medium"), Some(&CacheHitRate::Observed(0.0)));
        // La media per provider sarebbe 8,5%: non deve esistere da nessuna parte
        // come risposta, perche' nasconderebbe che `medium` non ha cache affatto.
        assert_eq!(m.len(), 2, "la risposta e' per modello, mai aggregata");
    }

    /// Campioni sotto la soglia: il modello NON compare, quindi il chiamante lo
    /// tratta come ignoto e resta sul listino. Un modello nuovo non deve
    /// ereditare un hit-rate apparente di 0% che lo penalizzerebbe a vita.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn sotto_la_soglia_di_campioni_il_modello_non_compare(pool: PgPool) {
        let id = identita(&pool).await;
        seed_finalized(&pool, id, "deepseek", "nuovo", 5, 1_000, 600).await;

        let m = observed_cache_hit_rates(&pool, "deepseek", finestra())
            .await
            .expect("lettura");

        assert!(
            !m.contains_key("nuovo"),
            "5 campioni sotto la soglia di 20 non devono produrre un hit-rate"
        );
    }

    /// Fuori finestra: le righe vecchie non contano. Serve anche a non leggere i
    /// giorni in cui le colonne di cache erano zero per costruzione (prima del
    /// 28/07/2026), dove lo zero non significa "nessun hit".
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn le_righe_fuori_finestra_non_contano(pool: PgPool) {
        // 30 righe con hit alto, poi spinte a 30 giorni fa.
        let id = identita(&pool).await;
        seed_finalized(&pool, id, "deepseek", "vecchio", 30, 1_000, 700).await;
        invecchia(&pool, "vecchio", 720).await;

        let m = observed_cache_hit_rates(&pool, "deepseek", finestra())
            .await
            .expect("lettura");

        assert!(!m.contains_key("vecchio"), "fuori finestra: nessun rapporto");
    }

    /// Una riga PRENOTATA porta una stima, non un fatto misurato: non deve
    /// entrare nel rapporto (regola M). Qui le prenotazioni da sole
    /// basterebbero a superare la soglia, quindi se il filtro mancasse il
    /// modello comparirebbe.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn le_righe_non_finalizzate_non_entrano_nel_rapporto(pool: PgPool) {
        let id = identita(&pool).await;
        for _ in 0..30 {
            crate::reserve(&pool, id, "openai", "gpt", 1_000, 100, json!({}))
                .await
                .expect("reserve");
        }

        let m = observed_cache_hit_rates(&pool, "openai", finestra())
            .await
            .expect("lettura");

        assert!(
            !m.contains_key("gpt"),
            "solo prenotazioni: nessun fatto misurato da cui ricavare un hit-rate"
        );
    }

    /// I parametri vengono dal DB e la migrazione 0656 li porta. Il test
    /// attraversa `HitRateWindow::load`, cioe' la stessa strada della produzione:
    /// costruire la finestra a mano proverebbe solo che la struct ha due campi.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn la_finestra_arriva_dai_settings(pool: PgPool) {
        let w = HitRateWindow::load(&pool).await.expect("load");
        assert_eq!(w.window_hours, 168);
        assert_eq!(w.min_samples, 20);
    }
}

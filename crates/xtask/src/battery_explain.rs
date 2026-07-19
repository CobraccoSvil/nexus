//! battery-explain — chi diagnostica PONE la domanda al sistema (regola O).
//!
//! Risponde a due domande sulla batteria di qualificazione:
//!   - chi e' eleggibile ADESSO, e quanti sono;
//!   - per un modello dato, PERCHE' e' eleggibile o quale condizione lo esclude.
//!
//! Il vincolo che lo rende affidabile: NON conosce la regola. La chiede a
//! `nexus_model_eligibility`, lo stesso crate da cui il claim di produzione
//! (`mcp-core::model_qualification::claim_candidates`) compone la sua UPDATE.
//! Cambiare l'eleggibilita' per uno solo dei due non e' una svista possibile:
//! non c'e' il posto dove scriverla due volte.
//!
//! Nasce da un incidente del 2026-07-17: uno script diagnostico aveva ricopiato
//! la query del claim leggendo la suite dalla tabella sbagliata e riportava "0
//! candidati eleggibili" mentre erano 29. La diagnosi parti' da un fatto
//! inesistente. Per questo l'output DICHIARA sempre da dove guarda: un numero
//! senza la sua premessa e' un'opinione.
//!
//! Uso:
//!   cargo xtask battery-explain                  # premessa + chi e' eleggibile
//!   cargo xtask battery-explain <modello>        # perche' SI / perche' NO
//!     <modello> = 'gpt-4o' oppure 'openai/gpt-4o'

use std::time::Duration;

use anyhow::{Context, Result};
use nexus_model_eligibility as elig;
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};

/// Oltre questa soglia l'elenco si tronca: durante un incidente 400 righe di
/// scroll nascondono la risposta invece di darla. Il totale resta dichiarato.
const MAX_ELENCO: usize = 40;

/// La PREMESSA della risposta: da dove guarda l'explain. Ogni campo qui e' una
/// cosa che, se sbagliata, rende falso ogni numero che segue — ed e' esattamente
/// cio' che e' successo (suite letta dalla tabella sbagliata).
struct Premessa {
    database: String,
    suite: i32,
    profili_attivi: usize,
    catalog_righe: i64,
    round_enabled: String,
    max_per_round: String,
}

/// Punto d'ingresso del sottocomando. Il primo argomento non-flag e' il modello
/// da spiegare; senza, risponde "chi e' eleggibile adesso".
pub fn run(args: &[String]) -> Result<i32> {
    let modello = args.iter().find(|a| !a.starts_with("--")).cloned();
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").context(
        "DATABASE_URL assente (.env del repo): l'explain non inventa una connessione, \
         perche' rispondere sul DB sbagliato e' il difetto che deve prevenire",
    )?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("runtime tokio")?;
    rt.block_on(esegui(database_url, modello))
}

async fn esegui(database_url: String, modello: Option<String>) -> Result<i32> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&database_url)
        .await
        .with_context(|| format!("connessione a {}", dichiara_db(&database_url)))?;
    let premessa = leggi_premessa(&pool, &database_url).await?;
    stampa_premessa(&premessa);
    stampa_scala_relativa(&pool).await;
    match modello {
        Some(m) => stampa_modello(&pool, &premessa, &m).await?,
        None => stampa_eleggibili(&pool, &premessa).await?,
    }
    pool.close().await;
    Ok(0)
}

/// La SCALA RELATIVA dei tier (mig 0615/0615): ancore, percentuali e pesi dello
/// score, letti GREZZI dal DB (regola O: si mostra cio' che c'e', non un parse
/// nostro). Senza questa premessa, uno score o una banda relativa sono numeri
/// opachi che non si possono contestare.
async fn stampa_scala_relativa(pool: &PgPool) {
    println!();
    println!("scala relativa (mig #0615/#0616 — banda = % del leader):");
    println!(
        "  percentuali    frontier {} | heavy {} | high {} | medium {}",
        setting_dichiarato(pool, "catalog.tier_relative.frontier_pct").await,
        setting_dichiarato(pool, "catalog.tier_relative.heavy_pct").await,
        setting_dichiarato(pool, "catalog.tier_relative.high_pct").await,
        setting_dichiarato(pool, "catalog.tier_relative.medium_pct").await,
    );
    println!(
        "  ancora prior   {} (modello {}, al {})  [tier synced dall'agentic_index]",
        setting_dichiarato(pool, "catalog.tier_relative.anchor").await,
        setting_dichiarato(pool, "catalog.tier_relative.anchor_model").await,
        setting_dichiarato(pool, "catalog.tier_relative.anchor_at").await,
    );
    println!(
        "  ancora measured {} (modello {}, al {})  [bande dallo score della batteria]",
        setting_dichiarato(pool, "catalog.measured_band.anchor").await,
        setting_dichiarato(pool, "catalog.measured_band.anchor_model").await,
        setting_dichiarato(pool, "catalog.measured_band.anchor_at").await,
    );
    println!(
        "  bande measured deadband {} | demote_margin {} | min_population {}",
        setting_dichiarato(pool, "catalog.measured_band.anchor_deadband_pct").await,
        setting_dichiarato(pool, "catalog.measured_band.demote_margin").await,
        setting_dichiarato(pool, "catalog.measured_band.min_population").await,
    );
    println!(
        "  pesi score     chain {} | recovery {} | real {} | latent {} | longctx {}",
        setting_dichiarato(pool, "catalog.measured_score.w_chain").await,
        setting_dichiarato(pool, "catalog.measured_score.w_recovery").await,
        setting_dichiarato(pool, "catalog.measured_score.w_real").await,
        setting_dichiarato(pool, "catalog.measured_score.w_latent").await,
        setting_dichiarato(pool, "catalog.measured_score.w_longctx").await,
    );
}

/// La suite corrente viene dai PROFILI, con la query del crate: e' la premessa
/// che lo script incriminato aveva preso dal catalogo. I settings si leggono col
/// punto unico `nexus_auth::get_setting_checked` (regola L), non con una SELECT
/// nostra.
async fn leggi_premessa(pool: &PgPool, database_url: &str) -> Result<Premessa> {
    let versioni: Vec<(i32,)> = sqlx::query_as(elig::SQL_PROFILE_SUITE_VERSIONS)
        .fetch_all(pool)
        .await
        .context("lettura delle suite dai profili")?;
    let (catalog_righe,): (i64,) = sqlx::query_as("SELECT count(*) FROM ai_price_catalog")
        .fetch_one(pool)
        .await
        .context("conteggio ai_price_catalog")?;
    Ok(Premessa {
        database: dichiara_db(database_url),
        suite: elig::current_suite_version(versioni.iter().map(|(v,)| *v)),
        profili_attivi: versioni.len(),
        catalog_righe,
        round_enabled: setting_dichiarato(pool, elig::KEY_ROUND_ENABLED).await,
        max_per_round: setting_dichiarato(pool, elig::KEY_MAX_PER_ROUND).await,
    })
}

/// Il valore GREZZO del setting, con la sua assenza dichiarata. L'explain non
/// rifa' il parse del worker: mostra cio' che c'e' nel DB e lascia vedere un
/// valore assurdo invece di normalizzarlo in silenzio.
async fn setting_dichiarato(pool: &PgPool, key: &str) -> String {
    match nexus_auth::get_setting_checked(pool, key).await {
        Ok(Some(v)) => format!("'{v}'"),
        Ok(None) => "(chiave assente)".into(),
        Err(e) => format!("(lettura fallita: {e})"),
    }
}

/// La password non entra nell'output: un explain si incolla nei ticket.
fn dichiara_db(url: &str) -> String {
    let Some((schema, resto)) = url.split_once("://") else {
        return "(DATABASE_URL non interpretabile)".into();
    };
    let Some((credenziali, host)) = resto.split_once('@') else {
        return format!("{schema}://{resto}");
    };
    let utente = credenziali.split_once(':').map_or(credenziali, |(u, _)| u);
    format!("{schema}://{utente}@{host}")
}

fn stampa_premessa(p: &Premessa) {
    println!("xtask battery-explain — eleggibilita' della batteria di qualificazione");
    println!();
    println!("premessa (da dove guardo):");
    println!("  DB              {}  (DATABASE_URL)", p.database);
    println!(
        "  suite corrente  {}  (MAX(suite_version) su ai_model_probe_profile WHERE enabled = TRUE; \
         {} profili attivi)",
        p.suite, p.profili_attivi
    );
    println!("  catalogo        ai_price_catalog: {} righe", p.catalog_righe);
    println!(
        "  lock stantio    {} min  (STALE_PROBING_MINUTES, nexus-model-eligibility)",
        elig::STALE_PROBING_MINUTES
    );
    println!(
        "  giro            {} = {}  |  {} = {}  (default codice: {})",
        elig::KEY_ROUND_ENABLED,
        p.round_enabled,
        elig::KEY_MAX_PER_ROUND,
        p.max_per_round,
        elig::DEFAULT_MAX_PER_ROUND
    );
    println!(
        "  regola          nexus_model_eligibility::where_clause() — {} condizioni, \
         le STESSE che compongono il claim di produzione",
        elig::CONDITIONS.len()
    );
}

/// Chi e' eleggibile ADESSO. I bind sono gli stessi valori che il worker passa
/// al claim: un elenco calcolato con altri parametri descriverebbe un giro che
/// non avverra' mai.
async fn stampa_eleggibili(pool: &PgPool, premessa: &Premessa) -> Result<()> {
    let righe: Vec<(String, String)> = sqlx::query_as(&elig::sql_explain_eligible())
        .bind(elig::STALE_PROBING_MINUTES as i32)
        .bind(premessa.suite)
        .fetch_all(pool)
        .await
        .context("query degli eleggibili")?;
    println!();
    println!(
        "eleggibili ADESSO: {} su {} righe del catalogo",
        righe.len(),
        premessa.catalog_righe
    );
    if righe.is_empty() {
        println!(
            "  (nessuno. Non e' di per se' un errore: puo' voler dire che la batteria ha \
             gia' misurato tutto cio' che poteva. Confronta la premessa qui sopra prima \
             di concludere che e' rotta.)"
        );
        return Ok(());
    }
    println!("  (nell'ordine del giro: prima i mai misurati, poi per scadenza)");
    for (i, (provider, model)) in righe.iter().take(MAX_ELENCO).enumerate() {
        println!("  {:>3}. {provider}/{model}", i + 1);
    }
    if righe.len() > MAX_ELENCO {
        println!("  ... e altri {} (mostrati i primi {MAX_ELENCO})", righe.len() - MAX_ELENCO);
    }
    println!();
    println!("  perche' uno di questi SI o NO:  cargo xtask battery-explain <provider/modello>");
    Ok(())
}

async fn stampa_modello(pool: &PgPool, premessa: &Premessa, modello: &str) -> Result<()> {
    let righe = sqlx::query(&elig::sql_explain_model())
        .bind(elig::STALE_PROBING_MINUTES as i32)
        .bind(premessa.suite)
        .bind(modello)
        .fetch_all(pool)
        .await
        .context("query del singolo modello")?;
    println!();
    if righe.is_empty() {
        println!(
            "modello '{modello}': NESSUNA riga in ai_price_catalog.\n  \
             Non e' 'non eleggibile': e' assente dal catalogo. Cerca la causa nel \
             catalog_sync del provider, non nella batteria."
        );
        return Ok(());
    }
    println!("modello '{modello}': {} righe nel catalogo", righe.len());
    for r in &righe {
        stampa_verdetto(r);
    }
    Ok(())
}

/// Il verdetto su UNA riga: le condizioni le enumera la lista del crate, quindi
/// una condizione aggiunta al claim compare qui senza che nessuno se ne ricordi.
fn stampa_verdetto(r: &PgRow) {
    let esiti: Vec<(&elig::EligibilityCondition, bool)> = elig::CONDITIONS
        .iter()
        .map(|c| (c, r.try_get::<Option<bool>, _>(c.name).ok().flatten().unwrap_or(false)))
        .collect();
    let ostacoli = esiti.iter().filter(|(_, ok)| !ok).count();
    let verdetto = if ostacoli == 0 { "ELEGGIBILE" } else { "NON eleggibile" };
    println!();
    println!("  {}/{}  ->  {verdetto}", campo(r, "provider"), campo(r, "model"));
    println!(
        "    stato: {} | suite: {} | scade: {} | backoff: {} | claim: {}",
        campo(r, "qualification_state"),
        campo_int(r, "qualification_suite_version"),
        campo(r, "qualification_expires_at"),
        campo(r, "qualification_backoff_until"),
        campo(r, "qualification_started_at")
    );
    println!(
        "    tier: {} (fonte: {}) | score misurato: {} (suite: {}, al: {})",
        campo(r, "performance_tier"),
        campo(r, "tier_source"),
        campo_f64(r, "measured_score"),
        campo_int(r, "measured_score_suite"),
        campo(r, "measured_score_at")
    );
    for (c, ok) in &esiti {
        if *ok {
            println!("    [ok] {}", c.name);
        } else {
            println!("    [NO] {}  ->  {}", c.name, c.perche_esclude);
        }
    }
}

fn campo(r: &PgRow, nome: &str) -> String {
    r.try_get::<Option<String>, _>(nome)
        .ok()
        .flatten()
        .unwrap_or_else(|| "-".into())
}

fn campo_int(r: &PgRow, nome: &str) -> String {
    r.try_get::<Option<i32>, _>(nome)
        .ok()
        .flatten()
        .map_or_else(|| "-".into(), |n| n.to_string())
}

fn campo_f64(r: &PgRow, nome: &str) -> String {
    r.try_get::<Option<f64>, _>(nome)
        .ok()
        .flatten()
        .map_or_else(|| "-".into(), |n| format!("{n:.2}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un explain si incolla in un ticket: la password non ci deve finire.
    #[test]
    fn la_premessa_dichiara_il_db_senza_la_password() {
        assert_eq!(
            dichiara_db("postgres://nexus:segretissima@localhost:5433/nexus"),
            "postgres://nexus@localhost:5433/nexus"
        );
        assert_eq!(
            dichiara_db("postgres://localhost:5433/nexus"),
            "postgres://localhost:5433/nexus"
        );
        assert_eq!(dichiara_db("spazzatura"), "(DATABASE_URL non interpretabile)");
    }

    /// L'explain lega i suoi bind ai segnaposto dichiarati dal crate: se il claim
    /// rinumerasse i parametri, questo test lo direbbe prima che lo dica Postgres
    /// in mezzo a un incidente.
    #[test]
    fn i_bind_dell_explain_seguono_i_segnaposto_del_crate() {
        assert_eq!(elig::EXPLAIN_STALE_PARAM, "$1", "1o bind: STALE_PROBING_MINUTES");
        assert_eq!(elig::EXPLAIN_SUITE_PARAM, "$2", "2o bind: suite corrente");
        assert_eq!(elig::EXPLAIN_MODEL_PARAM, "$3", "3o bind: il modello cercato");
    }
}

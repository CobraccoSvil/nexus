//! error-code-census — «quali codici d'errore dei fornitori sappiamo leggere, e
//! quali ci stanno passando sotto il naso».
//!
//! Il canale di scoperta NON puo' essere il log, ed e' misurato: `code=` esce
//! solo dal ramo `ClientError` del gateway, e delle 4439 chiamate in cui il
//! credito esaurito di openai e' stato classificato `transient` non e' rimasta
//! UNA riga con quel codice. Per questo il gateway scrive cio' che non sa
//! leggere in `nexus_provider_error_code_unknown`, e questo comando la legge.
//!
//! Non ricopia nessun criterio: il verdetto lo ha gia' dato il gateway, che ha
//! scritto la riga dichiarando con quale classe di ripiego ha proceduto. Qui si
//! legge, si dichiara la premessa e si trasforma in un rosso cio' che ha superato
//! la soglia (regola O).
//!
//! Uso:
//!   cargo run -q -p xtask -- error-code-census           # censimento
//!   cargo run -q -p xtask -- error-code-census --gate    # esce 1 sopra soglia
//!   cargo run -q -p xtask -- error-code-census --corpus  # i corpi reali distinti

use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::{PgPool, Row};

use crate::premessa::db_dichiarato;

/// Soglia oltre la quale un codice non dichiarato fa uscire 1 con `--gate`.
/// Dal DB (regola G): un numero qui sarebbe una seconda verita' da allineare.
const CHIAVE_SOGLIA: &str = "gateway.error_catalog.unknown_alert_occurrences";
const SOGLIA_DI_RIPIEGO: i64 = 20;

/// La PREMESSA: da dove guarda. Un numero senza la sua premessa e' un'opinione.
struct Premessa {
    database: String,
    righe_catalogo: i64,
    fornitori_dichiarati: i64,
    righe_jolly: i64,
    soglia: i64,
}

pub fn run(args: &[String]) -> Result<i32> {
    let gate = args.iter().any(|a| a == "--gate");
    let corpus = args.iter().any(|a| a == "--corpus");
    dotenvy::dotenv().ok();
    let database_url = std::env::var("DATABASE_URL").context(
        "DATABASE_URL assente (.env del repo): il censimento non inventa una \
         connessione, perche' rispondere sul DB sbagliato e' il difetto che deve \
         prevenire",
    )?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("runtime tokio")?;
    rt.block_on(esegui(database_url, gate, corpus))
}

async fn esegui(database_url: String, gate: bool, corpus: bool) -> Result<i32> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&database_url)
        .await
        .context("connessione al DB meta")?;

    let premessa = premessa(&pool, &database_url).await?;
    stampa_premessa(&premessa);

    if corpus {
        return stampa_corpus(&pool).await.map(|_| 0);
    }

    let sopra_soglia = stampa_non_dichiarati(&pool, premessa.soglia).await?;
    stampa_copertura(&pool).await?;

    if gate {
        if sopra_soglia == 0 {
            println!("\nGATE: nessun codice non dichiarato sopra la soglia.");
            return Ok(0);
        }
        println!(
            "\nGATE: {sopra_soglia} codici d'errore non dichiarati oltre {} occorrenze. \
             Il rimedio e' una RIGA in nexus_provider_error_code (vale in <=60s, \
             nessun redeploy), non un pattern nel codice.",
            premessa.soglia
        );
        return Ok(1);
    }
    Ok(0)
}

async fn premessa(pool: &PgPool, database_url: &str) -> Result<Premessa> {
    let righe_catalogo: i64 =
        sqlx::query_scalar("SELECT count(*) FROM nexus_provider_error_code")
            .fetch_one(pool)
            .await
            .context(
                "nexus_provider_error_code assente: applicare la migrazione 0705 \
                 (senza catalogo il gateway non parte)",
            )?;
    let fornitori_dichiarati: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT provider) FROM nexus_provider_error_code WHERE provider <> '*'",
    )
    .fetch_one(pool)
    .await?;
    let righe_jolly: i64 =
        sqlx::query_scalar("SELECT count(*) FROM nexus_provider_error_code WHERE provider = '*'")
            .fetch_one(pool)
            .await?;
    let soglia = nexus_auth::get_setting(pool, CHIAVE_SOGLIA)
        .await
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(SOGLIA_DI_RIPIEGO);
    Ok(Premessa {
        database: db_dichiarato(database_url),
        righe_catalogo,
        fornitori_dichiarati,
        righe_jolly,
        soglia,
    })
}

fn stampa_premessa(p: &Premessa) {
    println!("PREMESSA");
    println!("  database:              {}", p.database);
    println!("  righe di catalogo:     {}", p.righe_catalogo);
    println!("  fornitori dichiarati:  {}", p.fornitori_dichiarati);
    println!("  righe di convenzione:  {} (provider '*')", p.righe_jolly);
    println!("  soglia del gate:       {} occorrenze", p.soglia);
}

/// Cio' che il catalogo non sa. E' la lista delle cose da dichiarare, ordinata
/// per quanto sta costando.
async fn stampa_non_dichiarati(pool: &PgPool, soglia: i64) -> Result<usize> {
    let righe = sqlx::query(
        "SELECT provider, campo, valore, status_ultimo, classe_di_ripiego, occorrenze, \
                to_char(ultimo_visto,'YYYY-MM-DD HH24:MI') AS ultimo, esempio \
           FROM nexus_provider_error_code_unknown \
          ORDER BY occorrenze DESC",
    )
    .fetch_all(pool)
    .await
    .context("lettura dei codici non dichiarati")?;

    println!("\nCODICI NON DICHIARATI ({} righe)", righe.len());
    if righe.is_empty() {
        println!("  nessuno: ogni codice osservato ha la sua riga di catalogo.");
        return Ok(0);
    }
    let mut sopra = 0usize;
    for r in &righe {
        let occorrenze: i64 = r.get("occorrenze");
        if occorrenze >= soglia {
            sopra += 1;
        }
        let marca = if occorrenze >= soglia { "!!" } else { "  " };
        let status: Option<i16> = r.get("status_ultimo");
        println!(
            "{marca} {:<12} {:<30} {:<28} status={:<4} ripiego={:<15} x{:<7} ultimo={}",
            r.get::<String, _>("provider"),
            r.get::<String, _>("campo"),
            r.get::<String, _>("valore"),
            status.map(|s| s.to_string()).unwrap_or_else(|| "-".into()),
            r.get::<String, _>("classe_di_ripiego"),
            occorrenze,
            r.get::<String, _>("ultimo"),
        );
        if let Some(e) = r.get::<Option<String>, _>("esempio") {
            println!("     esempio: {}", e.chars().take(140).collect::<String>());
        }
    }
    Ok(sopra)
}

/// Quante righe dichiara ciascun fornitore, e con quale prova.
async fn stampa_copertura(pool: &PgPool) -> Result<()> {
    let righe = sqlx::query(
        "SELECT provider, \
                count(*) AS totale, \
                count(*) FILTER (WHERE origine = 'measured') AS misurate, \
                count(*) FILTER (WHERE causa IS NULL) AS ambigue \
           FROM nexus_provider_error_code GROUP BY provider ORDER BY provider",
    )
    .fetch_all(pool)
    .await?;
    println!("\nCATALOGO PER FORNITORE");
    for r in &righe {
        println!(
            "  {:<12} {:>3} righe  ({} misurate, {} dichiarate ambigue)",
            r.get::<String, _>("provider"),
            r.get::<i64, _>("totale"),
            r.get::<i64, _>("misurate"),
            r.get::<i64, _>("ambigue"),
        );
    }
    Ok(())
}

/// I corpi d'errore distinti realmente ricevuti, per rigenerare il corpus dei
/// test quando il traffico cambia. Il corpo persistito e' troncato a 500
/// caratteri (`truncate_chars`): dove il JSON non chiude, va dichiarato nel test
/// invece che ricostruito in silenzio.
async fn stampa_corpus(pool: &PgPool) -> Result<()> {
    let righe = sqlx::query(
        "WITH e AS ( \
           SELECT provider, \
                  (regexp_match(error_message, 'HTTP (\\d{3})'))[1] AS status, \
                  substring(error_message from position('{' in error_message)) AS body, \
                  error_kind, checked_at \
             FROM nexus_provider_health_history \
            WHERE healthy = false AND source = 'gateway' AND error_message LIKE '%HTTP%{%' \
         ) \
         SELECT DISTINCT ON (provider, status) provider, status, error_kind, body \
           FROM e ORDER BY provider, status, checked_at DESC",
    )
    .fetch_all(pool)
    .await
    .context("lettura dei corpi d'errore reali")?;
    println!("\nCORPI REALI DISTINTI ({} righe)", righe.len());
    for r in &righe {
        let body: String = r.get("body");
        println!(
            "\n-- {} {} (classe osservata: {})",
            r.get::<String, _>("provider"),
            r.get::<Option<String>, _>("status").unwrap_or_default(),
            r.get::<String, _>("error_kind"),
        );
        println!("{}", body.replace('\n', " "));
        if !body.trim_end().ends_with('}') {
            println!("   ^ TRONCATO in persistenza: dichiararlo nel corpus del test");
        }
    }
    Ok(())
}

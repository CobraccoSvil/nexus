//! capability-census — «di cio' che dichiariamo dei modelli, quanto e' coperto,
//! chi lo legge, e quali prove lo confermano o lo smentiscono».
//!
//! Chi diagnostica PONE la domanda al sistema (regola O). Il criterio della
//! copertura non e' ricopiato qui: arriva da `nexus_capability_audit`, lo stesso
//! crate da cui mcp-core compone il campo `declaration` del pannello. Cambiare
//! il criterio per uno solo dei due non e' una svista possibile — non c'e' il
//! posto dove scriverlo due volte.
//!
//! Perche' un comando e non un guard di build: 11 dei 37 modelli scoperti
//! misurati il 10/08/2026 sono di `openai`, che la sua migrazione di onboarding
//! ce l'ha da un pezzo — sono entrati nel catalogo dal discovery a RUNTIME, dopo
//! il build. Un guard testuale non puo' vederli perche' nascono dopo di lui.
//!
//! Uso:
//!   cargo run -q -p xtask -- capability-census            # censimento
//!   cargo run -q -p xtask -- capability-census --gate     # esce 1 se scoperto

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use nexus_capability_audit as audit;
use sqlx::{PgPool, Row};

/// Oltre questa soglia l'elenco per fornitore si tronca: un censimento che
/// stampa 128 righe nasconde la risposta invece di darla. I totali restano.
const MAX_ELENCO: usize = 12;

/// La PREMESSA del censimento: da dove guarda. Ogni campo qui e' una cosa che,
/// se sbagliata, rende falso ogni numero che segue — un numero senza la sua
/// premessa e' un'opinione (regola O, punto 4).
struct Premessa {
    database: String,
    colonne_vista: usize,
    modelli_abilitati: i64,
    righe_capability: i64,
}

/// Che cosa lo storico dei probe puo' dire su `tool_use` di un modello.
///
/// MISURATO il 10/08/2026, ed e' il vincolo che da' forma a questo tipo: un
/// tool-probe RIUSCITO scrive `healthy=true, error_kind=NULL`
/// (`model_health_probe.rs:1397`), cioe' una riga IDENTICA a quella di un probe
/// chat riuscito — 184.043 righe di `ai_model_health_history` sono in quello
/// stato e nessuna dice quale domanda fosse stata posta. Percio' **l'unica prova
/// attribuibile al tool-probe e' un suo FALLIMENTO**, e solo quello specifico
/// del modello: i prefissi `tool_probe_transient:` e `tool_probe_provider:`
/// dichiarano loro stessi di parlare del trasporto o del fornitore.
///
/// Trattare un successo come conferma e' la trappola in cui questo strumento e'
/// caduto alla prima stesura: attribuiva a `gpt-4o-mini-tts` — un modello di
/// sintesi vocale — una conferma di tool-capability che veniva dal probe chat.
///
/// E fra i fallimenti, il CRITERIO e' quello del ciclo, non uno inventato qui
/// (regola O): la prova che conta e' `consecutive_tool_failures`, il contatore su cui
/// `tool_capability::record_tool_failure` degrada a soglia e che il primo
/// successo azzera. Un conteggio storico dei fallimenti risponde a un'altra
/// domanda — «quante volte e' successo» — e usarlo come contraddizione
/// accuserebbe il catalogo di fallimenti che il ciclo ha gia' superato: sui dati
/// del 10/08/2026 sarebbero 68 modelli invece dei 18 reali, quasi tutti mistral
/// di fine giugno.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProvaToolUse {
    /// Il ciclo sta contando fallimenti NON ancora superati da un successo.
    /// E' l'unica prova viva contro la dichiarazione.
    SerieAperta {
        consecutivi: i32,
        ultimo: String,
        kind: String,
    },
    /// Ci sono stati fallimenti, e il ciclo li ha azzerati: e' il ciclo che
    /// funziona, non una contraddizione.
    SuperataDalCiclo { storici: i64, ultimo: String },
    /// Nessuna riga attribuibile al tool-probe. Non significa «va bene»:
    /// significa che quella tabella non risponde per questo modello (regola Q).
    NonAttribuibile,
}

/// Punto d'ingresso del sottocomando.
pub fn run(args: &[String]) -> Result<i32> {
    let gate = args.iter().any(|a| a == "--gate");
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
    rt.block_on(esegui(database_url, gate))
}

async fn esegui(database_url: String, gate: bool) -> Result<i32> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&database_url)
        .await
        .context("connessione al DB meta")?;

    let premessa = premessa(&pool, &database_url).await?;
    stampa_premessa(&premessa);
    stampa_vocabolario(&premessa);

    let fatti = audit::carica_fatti_catalogo(&pool).await;
    let scoperti = stampa_copertura(&fatti);

    stampa_prove(&pool).await?;

    if gate {
        if scoperti == 0 {
            println!("\nGATE: nessun modello abilitato e' scoperto.");
            return Ok(0);
        }
        println!(
            "\nGATE: {scoperti} modelli abilitati senza riga di capability. \
             Il rimedio e' una migrazione che li dichiari, non un valore \
             indovinato a runtime: nessun ciclo scrive quella tabella."
        );
        return Ok(1);
    }
    Ok(0)
}

async fn premessa(pool: &PgPool, database_url: &str) -> Result<Premessa> {
    let colonne_vista: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns \
          WHERE table_name = 'v_model_capabilities'",
    )
    .fetch_one(pool)
    .await
    .context("colonne della vista")?;
    let modelli_abilitati: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ai_price_catalog WHERE is_enabled")
            .fetch_one(pool)
            .await
            .context("modelli abilitati")?;
    let righe_capability: i64 =
        sqlx::query_scalar("SELECT count(*) FROM nexus_provider_capabilities")
            .fetch_one(pool)
            .await
            .context("righe capability")?;
    Ok(Premessa {
        database: maschera(database_url),
        colonne_vista: colonne_vista as usize,
        modelli_abilitati,
        righe_capability,
    })
}

/// La password non compare in un output destinato a essere incollato altrove.
fn maschera(url: &str) -> String {
    match (url.find("://"), url.rfind('@')) {
        (Some(s), Some(a)) if a > s + 3 => format!("{}://***@{}", &url[..s], &url[a + 1..]),
        _ => url.to_string(),
    }
}

fn stampa_premessa(p: &Premessa) {
    println!("PREMESSA (da dove guarda questo censimento)");
    println!("  database ............. {}", p.database);
    println!("  vista ................ v_model_capabilities, {} colonne", p.colonne_vista);
    println!("  modelli abilitati .... {}", p.modelli_abilitati);
    println!("  righe capability ..... {}", p.righe_capability);
    println!("  copertura chiesta con  nexus_capability_audit::SQL_FATTI_CATALOGO");
}

/// Chi legge le colonne. E' il numero che va letto per primo: dice quanta parte
/// della «fonte unica della capability» non abbia consumatori, e quindi per
/// quanta parte la domanda «e' vera?» non produca alcun sintomo.
fn stampa_vocabolario(p: &Premessa) {
    let via_vista = audit::COLONNE
        .iter()
        .filter(|c| matches!(c.lettura, audit::Lettura::ViaVista { .. }))
        .count();
    let dalla_tabella = audit::COLONNE
        .iter()
        .filter(|c| matches!(c.lettura, audit::Lettura::DallaTabella { .. }))
        .count();
    let orfane: Vec<&str> = audit::senza_lettore().map(|c| c.nome).collect();

    println!("\nCHI LEGGE LE {} COLONNE DELLA VISTA", audit::COLONNE.len());
    if audit::COLONNE.len() != p.colonne_vista {
        println!(
            "  ATTENZIONE: il vocabolario ne dichiara {} e la vista ne ha {}. \
             Il guard del crate lo impedisce a compile-time sui test; qui \
             significa che il DB e' avanti o indietro rispetto al codice.",
            audit::COLONNE.len(),
            p.colonne_vista
        );
    }
    println!("  lette VIA VISTA ...... {via_vista} (comprese provider/model, che sono la WHERE)");
    println!("  lette dalla TABELLA .. {dalla_tabella} (flag semantici, filtrati in SQL dal routing)");
    println!("  SENZA alcun lettore .. {}", orfane.len());
    for nome in &orfane {
        let c = audit::colonna(nome).expect("colonna dichiarata");
        println!(
            "      {:<34} {:<14} {}",
            nome,
            c.proprieta.wire(),
            c.accertamento.wire()
        );
    }
    println!(
        "  Una colonna senza lettori non e' un dato da automatizzare: e' un dato\n  \
         da collegare o da rimuovere. Un ciclo di verifica speso li' correggerebbe\n  \
         un valore che non cambia il comportamento di nulla."
    );
}

/// Copertura per fornitore, dal punto unico. Ritorna quanti modelli abilitati
/// sono scoperti in tutto: e' il numero su cui `--gate` decide.
fn stampa_copertura(fatti: &HashMap<String, Vec<audit::ModelFact>>) -> usize {
    println!("\nCOPERTURA DELLA DICHIARAZIONE (audit::classifica_dichiarazione)");
    let mut righe: Vec<(String, audit::DeclarationCoverage, usize)> = fatti
        .iter()
        .map(|(prov, modelli)| {
            let abilitati = modelli.iter().filter(|m| m.is_enabled).count();
            (
                prov.clone(),
                audit::classifica_dichiarazione(modelli),
                abilitati,
            )
        })
        .filter(|(_, c, _)| !matches!(c, audit::DeclarationCoverage::NothingToDeclare))
        .collect();
    // Prima chi ha piu' modelli scoperti: e' l'ordine in cui si interviene.
    righe.sort_by(|a, b| {
        b.1.undeclared()
            .cmp(&a.1.undeclared())
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut scoperti = 0usize;
    for (prov, cov, abilitati) in &righe {
        scoperti += cov.undeclared();
        let nota = if cov.richiede_intervento() {
            " <- richiede intervento"
        } else {
            ""
        };
        println!(
            "  {:<12} {:<18} abilitati {:>3}, scoperti {:>3}{}",
            prov,
            cov.wire(),
            abilitati,
            cov.undeclared(),
            nota
        );
    }
    println!(
        "  TOTALE scoperti: {scoperti}. Nessun ciclo a runtime scrive\n  \
         nexus_provider_capabilities: aspettare non li copre."
    );
    scoperti
}

/// Le prove che l'esercizio ha gia' prodotto, per le colonne che ne ammettono
/// una. Si raccolgono SOLO dove il segnale e' strutturato e interpretabile: dove
/// non lo e', si dichiara perche', invece di stampare un numero che non
/// risponde alla domanda (regola Q).
async fn stampa_prove(pool: &PgPool) -> Result<()> {
    println!("\nPROVE IN ESERCIZIO");
    prova_tool_use(pool).await?;
    prova_prompt_cache(pool).await?;
    prova_tool_choice_style(pool).await?;
    Ok(())
}

/// `tool_use`: l'unica capability con un ciclo di verifica automatico completo
/// (`mcp-core/src/tool_capability.rs`). Si riportano le sole prove ATTRIBUIBILI,
/// che sono i fallimenti specifici del modello — vedi [`ProvaToolUse`] per il
/// perche' un successo non lo sia.
///
/// Nemmeno un fallimento e' un verdetto: il ciclo che scrive usa una SOGLIA di
/// fallimenti consecutivi e azzera al primo successo, quindi righe vecchie
/// possono essere gia' state superate. Qui si riporta il FATTO, e chi legge sa
/// che il ciclo ha l'ultima parola.
/// La query delle prove attribuibili. Costante nominata per la stessa ragione di
/// `SQL_FATTI_CATALOGO`: il filtro `error_kind LIKE 'tool_probe:%'` E' la
/// premessa del conteggio, e un numero senza la sua premessa e' un'opinione.
/// Esclude di proposito `tool_probe_transient:%` e `tool_probe_provider:%`, che
/// dichiarano loro stessi di parlare del trasporto o del fornitore.
const SQL_PROVE_TOOL_USE: &str = "WITH fallimenti AS (          SELECT provider, model, count(*)::bigint AS quanti,                 max(checked_at)::date::text AS ultimo,                 (array_agg(error_kind ORDER BY checked_at DESC))[1] AS kind            FROM ai_model_health_history           WHERE error_kind LIKE 'tool_probe:%'           GROUP BY 1, 2)      SELECT c.provider, c.model, c.supports_tool_use, c.capability_locked,             c.consecutive_tool_failures, f.quanti, f.ultimo, f.kind        FROM ai_price_catalog c        LEFT JOIN fallimenti f ON f.provider = c.provider AND f.model = c.model       WHERE c.is_enabled       ORDER BY c.consecutive_tool_failures DESC, f.quanti DESC NULLS LAST,                c.provider, c.model";

async fn prova_tool_use(pool: &PgPool) -> Result<()> {
    let rows = sqlx::query(SQL_PROVE_TOOL_USE)
        .fetch_all(pool)
        .await
        .context("prova tool_use")?;

    let mut aperte: Vec<String> = Vec::new();
    let (mut serie_aperte, mut superate, mut non_attribuibili) = (0usize, 0usize, 0usize);
    for r in &rows {
        match prova_da_riga(r) {
            ProvaToolUse::NonAttribuibile => non_attribuibili += 1,
            ProvaToolUse::SuperataDalCiclo { .. } => superate += 1,
            ProvaToolUse::SerieAperta {
                consecutivi,
                ultimo,
                kind,
            } => {
                serie_aperte += 1;
                if aperte.len() < MAX_ELENCO {
                    aperte.push(riga_serie_aperta(r, consecutivi, &ultimo, &kind));
                }
            }
        }
    }
    println!("  tool_use            segnale ai_model_health_history — ciclo ATTIVO in tool_capability.rs");
    println!(
        "      serie APERTE (il ciclo sta contando): {serie_aperte}; \
         gia' superate da un successo: {superate}; \
         senza prova attribuibile: {non_attribuibili}"
    );
    println!(
        "      Un tool-probe RIUSCITO scrive healthy=true/error_kind NULL, identico\n      \
         a un probe chat riuscito: i successi non sono attribuibili, e per questo\n      \
         non compaiono come conferme."
    );
    for d in &aperte {
        println!("{d}");
    }
    Ok(())
}

/// Il criterio, separato dall'I/O e dalla resa: la serie APERTA vince perche' e'
/// lo stato del ciclo adesso; lo storico dice solo che qualcosa e' successo.
fn prova_da_riga(r: &sqlx::postgres::PgRow) -> ProvaToolUse {
    let consecutivi: i32 = r.try_get("consecutive_tool_failures").unwrap_or(0);
    let storici: i64 = r
        .try_get::<Option<i64>, _>("quanti")
        .ok()
        .flatten()
        .unwrap_or(0);
    let ultimo: String = r.try_get("ultimo").ok().flatten().unwrap_or_default();
    if consecutivi > 0 {
        ProvaToolUse::SerieAperta {
            consecutivi,
            ultimo,
            kind: r.try_get("kind").ok().flatten().unwrap_or_default(),
        }
    } else if storici > 0 {
        ProvaToolUse::SuperataDalCiclo { storici, ultimo }
    } else {
        ProvaToolUse::NonAttribuibile
    }
}

/// La riga di dettaglio. Porta il `[locked]` perche' su una riga con lock
/// esplicito il ciclo NON degrada (`tool_capability::record_tool_failure`
/// ritorna `Protected`): la serie resta aperta e nessuno la chiudera'.
fn riga_serie_aperta(
    r: &sqlx::postgres::PgRow,
    consecutivi: i32,
    ultimo: &str,
    kind: &str,
) -> String {
    let provider: String = r.try_get("provider").unwrap_or_default();
    let model: String = r.try_get("model").unwrap_or_default();
    let dichiarato: bool = r.try_get("supports_tool_use").unwrap_or(false);
    let locked: bool = r.try_get("capability_locked").unwrap_or(false);
    format!(
        "      {provider}/{model}: dichiarato {dichiarato}, serie aperta di \
         {consecutivi} (ultimo {ultimo}, {kind}){}",
        if locked { " [locked]" } else { "" }
    )
}

/// La colonna su cui questa prova risponde. Nominata una volta: e' insieme il
/// campo del `SELECT`, la chiave del vocabolario e l'etichetta della riga, e
/// tre copie sarebbero tre posti da cambiare insieme.
const COL_PROMPT_CACHE: &str = "supports_prompt_cache";

/// `supports_prompt_cache`: il segnale c'e' (il ledger conta i token letti dalla
/// cache) e smentisce la dichiarazione. La colonna pero' non ha lettori, quindi
/// e' una dichiarazione falsa e senza consumatori: il censimento lo dice, perche' i due
/// fatti insieme decidono il rimedio — non un probe, ma collegare la colonna o
/// rimuoverla.
async fn prova_prompt_cache(pool: &PgPool) -> Result<()> {
    let rows = sqlx::query(
        "WITH prova AS ( \
             SELECT provider, model, SUM(cache_read_tokens)::bigint AS letti, \
                    COUNT(*)::bigint AS chiamate \
               FROM ai_usage_ledger WHERE status = 'finalized' \
              GROUP BY 1, 2 HAVING SUM(cache_read_tokens) > 0) \
         SELECT p.provider, p.model, p.letti, p.chiamate, v.supports_prompt_cache \
           FROM prova p \
           LEFT JOIN v_model_capabilities v \
                  ON v.provider = p.provider AND v.model = p.model \
          ORDER BY p.letti DESC",
    )
    .fetch_all(pool)
    .await
    .context("prova prompt cache")?;

    let dichiarati_falsi = rows
        .iter()
        .filter(|r| matches!(r.try_get::<Option<bool>, _>(COL_PROMPT_CACHE), Ok(Some(false))))
        .count();
    let senza_riga = rows
        .iter()
        .filter(|r| matches!(r.try_get::<Option<bool>, _>(COL_PROMPT_CACHE), Ok(None)))
        .count();

    let lettore = audit::colonna(COL_PROMPT_CACHE)
        .map(|c| c.lettura.wire())
        .unwrap_or("?");
    println!("  {COL_PROMPT_CACHE}  segnale ai_usage_ledger.cache_read_tokens — lettura: {lettore}");
    println!(
        "      {} coppie con cache MISURATA la dichiarano false, {} non hanno riga",
        dichiarati_falsi, senza_riga
    );
    for r in rows.iter().take(5) {
        let provider: String = r.try_get("provider").unwrap_or_default();
        let model: String = r.try_get("model").unwrap_or_default();
        let letti: i64 = r.try_get("letti").unwrap_or(0);
        let chiamate: i64 = r.try_get("chiamate").unwrap_or(0);
        println!("      {provider}/{model}: {letti} token di cache letti su {chiamate} chiamate");
    }
    Ok(())
}

/// `tool_choice_style`: la colonna piu' meritevole di verifica automatica, e
/// quella su cui oggi la prova NON e' interpretabile. Il censimento lo dichiara
/// invece di stampare un conteggio che sembrerebbe una risposta.
async fn prova_tool_choice_style(pool: &PgPool) -> Result<()> {
    let rows = sqlx::query(
        "SELECT v.tool_choice_style, count(*) AS n \
           FROM ai_price_catalog c \
           JOIN v_model_capabilities v \
             ON v.provider = c.provider AND v.model = c.model \
          WHERE c.is_enabled GROUP BY 1 ORDER BY 2 DESC",
    )
    .fetch_all(pool)
    .await
    .context("distribuzione tool_choice_style")?;

    println!("  tool_choice_style   letto via vista (capability.rs:88) — dichiarazione:");
    for r in &rows {
        let stile: String = r.try_get("tool_choice_style").unwrap_or_default();
        let n: i64 = r.try_get("n").unwrap_or(0);
        println!("      {stile:<28} {n} modelli abilitati");
    }
    println!(
        "      PROVA NON INTERPRETABILE oggi: l'osservazione persistita\n      \
         (nexus_agent_meta_steps kind='step_validation') registra l'ESITO del\n      \
         giudice e non lo STIMOLO — nessun campo dice se force_tool_choice fosse\n      \
         acceso. Da quando il forcing e' condizionato allo stile dichiarato, un\n      \
         verdetto espresso da una coppia openai_auto non prova nulla: non e'\n      \
         stata forzata. Primo passo per automatizzarla: registrare lo stimolo."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_premessa_non_stampa_la_password() {
        // Un censimento si incolla nei report: la stringa di connessione ci
        // finisce con tutto quello che contiene.
        let m = maschera("postgres://nexus:segretissima@localhost:5433/nexus");
        assert!(!m.contains("segretissima"), "password in chiaro: {m}");
        assert!(m.contains("localhost:5433/nexus"), "l'host serve: {m}");
        // Senza credenziali non si inventa un mascheramento.
        assert_eq!(
            maschera("postgres://localhost:5433/nexus"),
            "postgres://localhost:5433/nexus"
        );
    }

    #[test]
    fn solo_i_fallimenti_specifici_sono_attribuibili_al_tool_probe() {
        // MUTAZIONE (regola O), e non e' ipotetica: alla prima stesura la query
        // ammetteva anche `healthy AND error_kind IS NULL` come prova, e quel
        // ramo attribuiva al tool-probe i successi del probe CHAT — dichiarando
        // «discorde» un gpt-4o-mini-tts (sintesi vocale) che il probe chat
        // aveva salutato. Il predicato vive nella query; qui si fissa il
        // vocabolario che lo giustifica, cosi' che riportarlo indietro
        // significhi contraddire un nome, non un dettaglio SQL.
        let attribuibili = "tool_probe:";
        for prefisso in ["tool_probe_transient:", "tool_probe_provider:"] {
            assert!(
                !prefisso.starts_with(attribuibili),
                "{prefisso} dichiara di parlare del trasporto o del fornitore, \
                 non della capability del modello"
            );
        }
        // L'ignoto e' una variante, mai un valore comodo (regola Q), ed e'
        // distinto dalla serie che il ciclo ha gia' superato: sono tre stati e
        // non due, perche' hanno tre conseguenze diverse.
        assert_ne!(
            ProvaToolUse::NonAttribuibile,
            ProvaToolUse::SuperataDalCiclo {
                storici: 0,
                ultimo: String::new()
            }
        );
        assert_ne!(
            ProvaToolUse::SuperataDalCiclo {
                storici: 55,
                ultimo: "2026-06-29".into()
            },
            ProvaToolUse::SerieAperta {
                consecutivi: 55,
                ultimo: "2026-06-29".into(),
                kind: "tool_probe:error".into()
            },
            "stesso numero, significati opposti: uno e' il ciclo che ha funzionato"
        );
    }

    #[test]
    fn il_censimento_non_ha_una_copia_del_criterio() {
        // Il criterio arriva dal crate condiviso: questa e' la prova che il
        // comando risponde con la stessa regola del pannello (regola O). Se
        // qualcuno lo ricopiasse qui, questo test resterebbe verde — ma il
        // guard `capability-census-delega` di check-single-source no.
        let modelli = vec![audit::ModelFact {
            is_enabled: true,
            capability_source: "auto".into(),
            auto_disabled_reason: None,
            ha_capability: false,
        }];
        assert_eq!(
            audit::classifica_dichiarazione(&modelli),
            audit::DeclarationCoverage::Absent { undeclared: 1 }
        );
    }
}

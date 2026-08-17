//! «Servizio avviato» deve voler dire che il servizio E' VIVO.
//!
//! # Il difetto, misurato il 17/08/2026 in esercizio
//!
//! Run reale dalla UI (app libri). L'agente ha scritto il backend, ha chiamato
//! `run_service` con `node backend/src/index.js`, e ha proseguito. I fatti:
//!
//! - `nexus_port_allocations`: porta 27113 allocata al progetto;
//! - `agent_processes`: `service-de59bbca` -> `status=failed`, `exit_code=1`;
//! - `curl http://localhost:27113/` -> connessione rifiutata;
//! - causa reale, riprodotta a mano: `Error: Cannot find module 'express'`
//!   (l'`npm install` era fallito a meta': `package.json` dichiara express,
//!   better-sqlite3 e cors, in `node_modules` c'era UN pacchetto);
//! - il run e' proseguito ignaro, e il final gate ha poi chiuso senza avere
//!   nulla da chiedere.
//!
//! Il sistema aveva scritto `failed` nella propria tabella. Nessuno lo ha detto
//! all'agente.
//!
//! # Perche' il tool dichiarava la nascita e non la vita
//!
//! `tool_run_service` chiamava `spawn_agent_process` e, appena il processo
//! esisteva, componeva una risposta RIUSCITA. Ma «partito» li' significava «il
//! processo e' nato»: su Windows la shell nasce sempre, anche se il comando
//! dentro muore un istante dopo. Un servizio che esce con codice 1 in due
//! secondi produceva esattamente la stessa risposta di un servizio sano.
//!
//! # Il criterio della vita non nasce qui: si delega
//!
//! Due punti unici rispondono gia', e questo modulo NON ne aggiunge un terzo
//! (regola L):
//!
//! - «la porta risponde, e non per un istante?» ->
//!   [`crate::project_workspace::service_recovery::await_port_ready`], cioe'
//!   `probe_port` + `stable_enough` + il ciclo dei due orologi. E' il contratto
//!   della remediation, gia' riusato dal gate di readiness del runner
//!   Playwright. L'unica cosa che cambia qui e' la DURATA della stabilita', ed
//!   e' per questo che e' diventata un parametro invece di una seconda
//!   funzione: chi rimedia e chi lancia una suite pretendono un servizio CALDO,
//!   `run_service` chiede «e' salito?» subito dopo lo spawn.
//! - «il capostipite e' uscito, o e' solo la shell che se n'e' andata mentre il
//!   server figlio vive?» -> lo ha gia' deciso chi ha atteso il child. Il task
//!   di background di `spawn_agent_process` interroga
//!   [`crate::project_workspace::service_liveness`] PRIMA di scrivere una morte
//!   (`agent_processes.rs`, ramo `servizio_ancora_vivo`): se il servizio e'
//!   vivo lascia la riga `running`. Quindi una riga che dichiara l'uscita e'
//!   gia' un verdetto del punto unico, e qui la si LEGGE — non la si ricalcola.
//!
//! Il valore aggiunto di questo modulo e' percio' UNO solo: porre le due
//! domande al momento giusto (subito dopo lo spawn, mentre l'agente puo' ancora
//! correggere) e dichiararne l'esito in un campo invece che nella prosa.

use std::time::Duration;

use nexus_types::tool_outcome::RispostaTool;
use sqlx::PgPool;
use uuid::Uuid;

use crate::agent_processes::ProcessOutput;
use crate::project_workspace::service_recovery::await_port_ready;

/// Default di `agent.service.readiness_timeout_s` (regola G: il valore vive nel
/// DB, questo e' cio' che si usa quando la chiave non e' leggibile).
const DEFAULT_READINESS_S: u64 = 20;
/// Default di `agent.service.morte_precoce_finestra_s`.
const DEFAULT_MORTE_PRECOCE_S: u64 = 5;
/// Pausa fra due letture della riga mentre si osserva l'uscita del capostipite.
const INTERVALLO_LETTURA_MS: u64 = 400;

/// Com'e' andato l'AVVIO. Tre varianti perche' i casi hanno rimedi diversi
/// (regola Q), e perche' due di essi erano finora indistinguibili da un
/// successo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AvvioServizio {
    /// La porta attesa risponde: il servizio serve. `atteso_ms` e' quanto e'
    /// costato accertarlo, e serve a sapere se il tetto e' dimensionato bene.
    Vivo { porta: u16, atteso_ms: u64 },
    /// Il processo e' USCITO durante l'attesa: e' un fallimento del lancio, e
    /// l'output dice perche' (il «Cannot find module 'express'» che l'agente
    /// non ha mai visto).
    ///
    /// L'`exit_code` non e' decorazione: e' cio' che distingue un servizio
    /// crollato da un comando one-shot che ha finito il proprio lavoro, e la
    /// conseguenza sul tool_result la deriva da li' (vedi [`Self::risposta`]).
    MortoSubito {
        exit_code: Option<i32>,
        output: String,
    },
    /// Il processo e' vivo, ma nessuna porta risponde entro il tetto. NON e'
    /// una morte: puo' essere un servizio lento, un worker senza ascolto, o un
    /// comando che gira in background legittimamente. Si dichiara e si lascia
    /// decidere all'agente.
    ///
    /// `porta_attesa` distingue i due silenzi, che hanno peso opposto:
    /// `Some(p)` significa «gli avevamo promesso la porta p e non l'ha presa»
    /// (avvertenza vera), `None` significa «nessuna porta era attesa» (un
    /// worker che vive e' esattamente cio' che doveva succedere). Senza il
    /// campo, l'avvertenza andrebbe a tutti e diventerebbe rumore che nessuno
    /// legge — cioe' il modo in cui un avviso vero si perde.
    VivoMaSilenzioso {
        porta_attesa: Option<u16>,
        atteso_ms: u64,
        output: String,
    },
}

/// Cosa la riga di `agent_processes` dichiara del capostipite.
///
/// Tre risposte e non un `bool`: «non l'ho letta» non e' «e' vivo», e trattarlo
/// come tale trasformerebbe un guasto del DB in una morte dichiarata all'agente.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UscitaCapostipite {
    /// La riga dichiara il processo ancora in corsa.
    NonUscito,
    /// Il processo e' uscito. Il verdetto viene da chi ha atteso il child e ha
    /// gia' interrogato `service_liveness`: se il server fosse un discendente
    /// vivo, questa riga direbbe ancora `running`.
    Uscito { exit_code: Option<i32> },
    /// La riga non e' stata letta (DB non raggiungibile, processo non trovato).
    NonOsservata,
}

/// Cosa si e' osservato sulla porta che il servizio doveva prendere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EsitoPorta {
    /// Nessuna porta era attesa: non c'e' alcun ascolto da pretendere, e la sua
    /// assenza non e' un difetto.
    NessunaAttesa,
    /// La porta risponde, secondo il contratto di [`await_port_ready`].
    Risponde { porta: u16 },
    /// La porta e' rimasta muta per tutta la finestra.
    Muta { porta: u16 },
}

/// I FATTI su cui si decide, separati dal criterio che li giudica: cosi' il
/// criterio si esercita senza dover produrre a comando un processo morto o una
/// porta lenta, e chi lo esercita passa comunque dai produttori veri invece di
/// fabbricare l'esito che vorrebbe (regola O).
///
/// L'OUTPUT non e' qui, ed e' deliberato: e' evidenza per l'agente, non un
/// input della decisione. Classificare un avvio leggendo il testo dello stderr
/// e' esattamente cio' che la regola M vieta — il fatto e' `exit_code`, non la
/// parola «Error».
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FattiAvvio {
    pub uscita: UscitaCapostipite,
    pub porta: EsitoPorta,
}

/// Il CRITERIO, puro.
///
/// # Precedenza: la porta che risponde vince sull'uscita del capostipite
///
/// Non e' una scelta di comodo, e' la stessa di
/// [`crate::project_workspace::service_liveness::classifica_servizio`]: un
/// servizio non si giudica morto da una prova sola quando ne esistono due, e il
/// pid registrato e' la SHELL — il server e' un discendente che le sopravvive
/// (misurato su gestione-corsi: `bash -> bash -> dotnet -> SchoolCoursesApi`
/// vivo e in ascolto col capostipite morto). Se la porta risponde, il servizio
/// serve, qualunque cosa sia successo alla shell.
///
/// # L'ignoto non degrada a morte
///
/// `NonOsservata` non autorizza a dichiarare morto (stessa disciplina di
/// `StatoProcesso::autorizza_a_dichiararlo_morto`): senza aver letto la riga non
/// si e' osservato nulla, e la variante corretta e' quella che dichiara il
/// silenzio, non quella che accusa.
pub(crate) fn classifica_avvio(
    fatti: &FattiAvvio,
    atteso_ms: u64,
    output: String,
) -> AvvioServizio {
    if let EsitoPorta::Risponde { porta } = fatti.porta {
        return AvvioServizio::Vivo { porta, atteso_ms };
    }
    if let UscitaCapostipite::Uscito { exit_code } = fatti.uscita {
        return AvvioServizio::MortoSubito { exit_code, output };
    }
    AvvioServizio::VivoMaSilenzioso {
        porta_attesa: match fatti.porta {
            EsitoPorta::Muta { porta } => Some(porta),
            EsitoPorta::Risponde { porta } => Some(porta),
            EsitoPorta::NessunaAttesa => None,
        },
        atteso_ms,
        output,
    }
}

impl AvvioServizio {
    /// La CONSEGUENZA sul tool_result, composta DAI campi (regola Q: il testo
    /// si compone dalla struttura, mai il contrario).
    ///
    /// `MortoSubito` con codice diverso da zero e' l'unico caso FALLITO, ed e'
    /// `rimediabile` perche' e' precisamente cio' che l'agente puo' correggere:
    /// nel caso misurato avrebbe letto «Cannot find module 'express'» e avrebbe
    /// installato le dipendenze invece di proseguire.
    ///
    /// Un'uscita con codice ZERO non e' un fallimento: e' un comando che ha
    /// finito il proprio lavoro. Qui passano anche i `run_in_terminal`
    /// declassati a `task` (`npm install`, uno script che termina), e
    /// dichiararli falliti perche' «non sono rimasti su» sarebbe il difetto
    /// opposto a quello che questo modulo chiude — con lo stesso costo, un
    /// esito che non corrisponde ai fatti.
    /// `nota` e' la diagnosi che il chiamante puo' avere in piu' (dove il
    /// servizio stia ascoltando DAVVERO) e va PRIMA dell'output: cambia cosa
    /// l'agente deve cercare li' dentro — un servizio in ascolto altrove non ha
    /// un errore da correggere nello stderr, ha una porta da riconciliare.
    pub(crate) fn risposta(&self, intestazione: &str, nota: Option<&str>) -> RispostaTool {
        let nota = nota.unwrap_or("");
        match self {
            AvvioServizio::Vivo { porta, atteso_ms } => RispostaTool::riuscito(format!(
                "{intestazione}\nVIVO: in ascolto sulla porta {porta}, verificato \
                 {atteso_ms} ms dopo l'avvio."
            )),
            AvvioServizio::MortoSubito { exit_code, output } => {
                risposta_uscito(intestazione, nota, *exit_code, output)
            }
            AvvioServizio::VivoMaSilenzioso {
                porta_attesa,
                atteso_ms,
                output,
            } => risposta_silenzioso(intestazione, nota, *porta_attesa, *atteso_ms, output),
        }
    }
}

/// Il processo e' USCITO.
///
/// Codice ZERO: non e' un fallimento, e' un comando che ha finito il proprio
/// lavoro (qui passano anche i `run_in_terminal` declassati a `task`).
///
/// Codice diverso da zero: RIMEDIABILE, e la differenza con il transitorio del
/// gemello qui sotto e' tutta la sostanza del fix. Un processo uscito per un
/// modulo mancante non torna su ritentando identico, e `Transitorio` dichiara
/// letteralmente che ritentare identico e' la strategia corretta: detto a un
/// `Cannot find module 'express'`, e' l'istruzione a rifare il giro che non
/// puo' riuscire.
fn risposta_uscito(
    intestazione: &str,
    nota: &str,
    exit_code: Option<i32>,
    output: &str,
) -> RispostaTool {
    if exit_code == Some(0) {
        return RispostaTool::riuscito(format!(
            "{intestazione}\nTERMINATO subito con codice 0: il comando ha concluso il \
             proprio lavoro e non e' rimasto in esecuzione.\n{}",
            sezione_output(output)
        ));
    }
    let codice = exit_code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "sconosciuto".into());
    RispostaTool::fallito_rimediabile(format!(
        "{intestazione}\nIL SERVIZIO NON E' PARTITO: il processo e' USCITO con codice \
         {codice} subito dopo l'avvio. Non e' in esecuzione, e nessuna porta risponde.\n\
         Ritentare lo stesso comando non lo rimettera' su: l'output qui sotto dice \
         perche' e' morto, correggi quella causa e riavvia.\n{nota}{}",
        sezione_output(output)
    ))
}

/// Il processo e' VIVO ma non risponde su alcuna porta.
///
/// Con una porta PROMESSA e' un fallimento TRANSITORIO, ed e' il precedente
/// gia' deciso in main per questa identica firma: il processo e' vivo e la
/// causa piu' frequente e' un avvio lento, dove ritentare (dopo aver guardato)
/// e' davvero la strada.
///
/// Senza porta attesa non c'e' nulla da pretendere: un worker che vive e' cio'
/// che doveva succedere, e un'avvertenza data a tutti sarebbe il rumore in cui
/// si perde quella vera.
fn risposta_silenzioso(
    intestazione: &str,
    nota: &str,
    porta_attesa: Option<u16>,
    atteso_ms: u64,
    output: &str,
) -> RispostaTool {
    let Some(porta) = porta_attesa else {
        return RispostaTool::riuscito(format!(
            "{intestazione}\nVIVO dopo {atteso_ms} ms. Nessuna porta era attesa per questo \
             comando, quindi non c'e' ascolto da verificare.\n{}",
            sezione_output(output)
        ));
    };
    RispostaTool::fallito_transitorio(format!(
        "{intestazione}\nNESSUN ASCOLTO sulla porta {porta} entro {atteso_ms} ms. Il \
         processo e' ancora vivo, ma questo NON basta: alcuni runner (nodemon, watcher) \
         sopravvivono al crash dell'applicazione.\nCorreggi la causa e riavvia, invece di \
         proseguire come se il servizio rispondesse.\n{nota}{}",
        sezione_output(output)
    ))
}

/// L'output del processo come sezione leggibile. Vuoto dichiarato, non taciuto:
/// «non ha stampato nulla» e' un'informazione diagnostica quanto un errore.
fn sezione_output(output: &str) -> String {
    if output.trim().is_empty() {
        "(Nessun output catturato. Usa read_service_output fra qualche secondo.)".to_string()
    } else {
        format!("\n--- OUTPUT DEL PROCESSO ---\n{output}")
    }
}

/// Cio' che l'attesa ha prodotto: il verdetto, e la riga letta per darlo.
///
/// L'output viaggia col verdetto perche' la riga si legge UNA volta: farla
/// rileggere al chiamante significherebbe due fotografie di un processo che nel
/// frattempo si muove, e la seconda potrebbe non confermare la prima.
pub(crate) struct EsitoAvvio {
    pub avvio: AvvioServizio,
    /// La riga di `agent_processes` letta a verdetto dato: serve al chiamante
    /// per pid, porta rilevata nell'output ed eventi di pannello. `Err` = non
    /// si e' potuta leggere, ed e' distinto dal non averla mai chiesta.
    pub info: Result<ProcessOutput, String>,
}

/// Le due domande, poste insieme e col tetto giusto.
///
/// Con una porta attesa le due attese CORRONO: un servizio sano ritorna appena
/// la porta risponde (non paga il tetto), uno morto ritorna appena la riga
/// dichiara l'uscita (non paga l'attesa della porta). Senza porta attesa resta
/// la sola osservazione dell'uscita, per la finestra della morte precoce: e'
/// cio' che sostituisce lo `sleep` cieco di prima, e non e' piu' lento — a
/// parita' di finestra ritorna prima quando c'e' qualcosa da dire.
///
/// STABILITA' ZERO alla porta, e non e' un allentamento: il ciclo osserva
/// comunque due volte, quindi il fatto resta «ha risposto, e un istante dopo
/// rispondeva ancora». Pretendere qui i quindici secondi della remediation
/// costerebbe quel tempo a OGNI avvio sano per rispondere a una domanda — «e'
/// caldo?» — che a questo punto del lavoro nessuno pone.
pub(crate) async fn attendi_avvio(
    db: &PgPool,
    project_id: Uuid,
    process_id: Uuid,
    porta_attesa: Option<u16>,
    readiness: Duration,
    morte_precoce: Duration,
) -> EsitoAvvio {
    let inizio = std::time::Instant::now();

    let porta = match porta_attesa {
        Some(port) => corsa_porta_o_uscita(db, project_id, process_id, port, readiness).await,
        None => {
            attende_uscita(db, project_id, process_id, morte_precoce).await;
            EsitoPorta::NessunaAttesa
        }
    };

    let atteso_ms = inizio.elapsed().as_millis() as u64;
    let info = crate::agent_processes::read_process_output(db, project_id, process_id, 4000).await;
    let fatti = FattiAvvio {
        uscita: uscita_dalla_riga(info.as_ref().ok()),
        porta,
    };
    let output = info
        .as_ref()
        .map(testo_di_output)
        .unwrap_or_else(|_| String::new());
    EsitoAvvio {
        avvio: classifica_avvio(&fatti, atteso_ms, output),
        info,
    }
}

/// Le due attese CORRONO, e vince chi ha qualcosa da dire per primo: un
/// servizio sano ritorna appena la porta risponde (non paga il tetto), uno morto
/// appena la riga dichiara l'uscita (non paga l'attesa della porta).
///
/// L'uscita del capostipite chiude la corsa senza concedere altro tempo, e non
/// e' una scorciatoia: chi scrive quel fatto ha GIA' interrogato
/// `service_liveness` (`agent_processes`, ramo `servizio_ancora_vivo`), quindi
/// il caso «shell morta, server figlio vivo» a questo punto e' gia' escluso —
/// li' la riga sarebbe rimasta `running`. L'esito e' comunque `Muta` e non una
/// morte: chi giudica e' [`classifica_avvio`], che vede entrambi i fatti.
async fn corsa_porta_o_uscita(
    db: &PgPool,
    project_id: Uuid,
    process_id: Uuid,
    porta: u16,
    readiness: Duration,
) -> EsitoPorta {
    let attesa_porta = await_port_ready(porta, readiness, Duration::ZERO);
    let attesa_uscita = attende_uscita(db, project_id, process_id, readiness);
    tokio::pin!(attesa_porta, attesa_uscita);
    let risponde = tokio::select! {
        pronta = &mut attesa_porta => pronta.ready(),
        _ = &mut attesa_uscita => false,
    };
    if risponde {
        EsitoPorta::Risponde { porta }
    } else {
        EsitoPorta::Muta { porta }
    }
}

/// stdout e stderr in un blocco solo: all'agente serve la diagnosi, non sapere
/// da quale descrittore e' uscita. Node scrive `Cannot find module` su stderr,
/// altri runtime lo scrivono su stdout, e chi legge non deve indovinare dove
/// guardare.
fn testo_di_output(info: &ProcessOutput) -> String {
    let mut testo = String::new();
    if !info.stdout.trim().is_empty() {
        testo.push_str(info.stdout.trim_end());
    }
    if !info.stderr.trim().is_empty() {
        if !testo.is_empty() {
            testo.push('\n');
        }
        testo.push_str(info.stderr.trim_end());
    }
    testo
}

/// Il fatto «il capostipite e' uscito» dalla riga che lo dichiara.
///
/// Il segnale e' STRUTTURATO (regola M): `exit_code` valorizzato, oppure uno
/// status del vocabolario chiuso di `agent_processes` (`starting`, `running`,
/// `stopped`, `failed`). Mai il testo dell'output.
///
/// `exit_code` presente basta da solo: lo scrive unicamente chi ha atteso il
/// child, e nessun altro percorso puo' produrlo.
fn uscita_dalla_riga(info: Option<&ProcessOutput>) -> UscitaCapostipite {
    let Some(info) = info else {
        return UscitaCapostipite::NonOsservata;
    };
    let terminale = matches!(info.status.as_str(), "stopped" | "failed");
    if info.exit_code.is_some() || terminale {
        return UscitaCapostipite::Uscito {
            exit_code: info.exit_code,
        };
    }
    UscitaCapostipite::NonUscito
}

/// Osserva la riga finche' dichiara l'uscita o scade la finestra. Ritorna
/// appena c'e' qualcosa da dire: e' il ramo che paga il difetto misurato, e
/// farlo attendere il tetto intero significherebbe venti secondi di silenzio
/// prima di una diagnosi che era pronta al secondo due.
async fn attende_uscita(db: &PgPool, project_id: Uuid, process_id: Uuid, finestra: Duration) {
    let scade = tokio::time::Instant::now() + finestra;
    loop {
        let info = crate::agent_processes::read_process_output(db, project_id, process_id, 1).await;
        if matches!(
            uscita_dalla_riga(info.as_ref().ok()),
            UscitaCapostipite::Uscito { .. }
        ) {
            return;
        }
        if tokio::time::Instant::now() >= scade {
            return;
        }
        tokio::time::sleep(Duration::from_millis(INTERVALLO_LETTURA_MS)).await;
    }
}

/// Quanto si attende la vita prima di dichiarare il silenzio (regola G: dal DB,
/// nessun fallback hardcoded oltre al default della chiave assente).
pub(crate) async fn finestra_readiness(db: &PgPool) -> Duration {
    secondi(db, "agent.service.readiness_timeout_s", DEFAULT_READINESS_S).await
}

/// Entro quanto la morte del processo si considera «del lancio».
pub(crate) async fn finestra_morte_precoce(db: &PgPool) -> Duration {
    secondi(
        db,
        "agent.service.morte_precoce_finestra_s",
        DEFAULT_MORTE_PRECOCE_S,
    )
    .await
}

async fn secondi(db: &PgPool, chiave: &str, default: u64) -> Duration {
    let secs = crate::settings::get_setting(db, chiave)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default);
    Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fatti(uscita: UscitaCapostipite, porta: EsitoPorta) -> FattiAvvio {
        FattiAvvio { uscita, porta }
    }

    /// IL CASO MISURATO, ridotto ai suoi fatti: il processo e' uscito con
    /// codice 1 e la porta assegnata non risponde.
    ///
    /// MUTAZIONE: facendo ritornare `classifica_avvio` la variante `Vivo` (cioe'
    /// il comportamento di prima, che dichiarava riuscito qualunque spawn), il
    /// test rosseggia mostrando che l'agente non riceverebbe la diagnosi.
    #[test]
    fn un_processo_uscito_con_errore_e_morto_non_avviato() {
        let esito = classifica_avvio(
            &fatti(
                UscitaCapostipite::Uscito { exit_code: Some(1) },
                EsitoPorta::Muta { porta: 27113 },
            ),
            2_000,
            "Error: Cannot find module 'express'".into(),
        );
        let AvvioServizio::MortoSubito { exit_code, output } = esito else {
            panic!("uscito con codice 1: non puo' essere un avvio riuscito, e' {esito:?}");
        };
        assert_eq!(exit_code, Some(1));
        assert!(output.contains("Cannot find module"));
    }

    /// La conseguenza del caso misurato: FALLITA, con l'output dentro e con la
    /// natura RIMEDIABILE. E' la meta' che conta — un esito giusto in un campo
    /// che nessuno legge non avrebbe corretto niente.
    ///
    /// La natura non e' un dettaglio: `Transitorio` dichiara che ritentare
    /// identico e' la strategia corretta, e su un modulo mancante e' l'ordine di
    /// rifare un giro che non puo' riuscire.
    ///
    /// MUTAZIONE: sostituendo `fallito_rimediabile` con `riuscito` (il
    /// comportamento del ramo senza porta attesa) rosseggia la prima asserzione;
    /// sostituendolo con `fallito_transitorio` rosseggia la terza.
    #[test]
    fn la_morte_del_lancio_produce_una_risposta_fallita_e_rimediabile() {
        let risposta = AvvioServizio::MortoSubito {
            exit_code: Some(1),
            output: "Error: Cannot find module 'express'".into(),
        }
        .risposta("Servizio 'backend' (process_id: X)", None);
        assert!(
            risposta.esito.e_fallito(),
            "il lancio e' fallito: {risposta:?}"
        );
        assert!(
            risposta.testo.contains("Cannot find module"),
            "l'output e' la diagnosi che l'agente deve leggere: {}",
            risposta.testo
        );
        assert_eq!(
            risposta.natura,
            Some(nexus_types::tool_outcome::NaturaFallimento::Rimediabile),
            "un processo uscito non torna su ritentando identico: {risposta:?}"
        );
    }

    /// Un comando che termina con codice 0 ha fatto il proprio lavoro: NON e'
    /// un fallimento. Copre i `run_in_terminal` declassati a `task`, che di qui
    /// passano e che prima ricevevano un successo cieco.
    #[test]
    fn un_uscita_pulita_non_e_un_fallimento() {
        let risposta = AvvioServizio::MortoSubito {
            exit_code: Some(0),
            output: "added 3 packages".into(),
        }
        .risposta("intestazione", None);
        assert!(!risposta.esito.e_fallito(), "codice 0: {risposta:?}");
    }

    /// La porta che risponde vince sull'uscita del capostipite: la shell se n'e'
    /// andata, il server e' un discendente ancora in ascolto. Stessa precedenza
    /// di `service_liveness::classifica_servizio`, e senza di essa un servizio
    /// sano verrebbe dichiarato morto ogni volta che il comando fa `&`.
    #[test]
    fn la_porta_che_risponde_vince_sulla_shell_uscita() {
        let esito = classifica_avvio(
            &fatti(
                UscitaCapostipite::Uscito { exit_code: Some(0) },
                EsitoPorta::Risponde { porta: 27113 },
            ),
            900,
            String::new(),
        );
        assert_eq!(
            esito,
            AvvioServizio::Vivo {
                porta: 27113,
                atteso_ms: 900
            }
        );
    }

    /// Vivo ma muto sulla porta PROMESSA: resta un fallimento TRANSITORIO,
    /// esattamente com'era prima di questo modulo. Il fix apre il caso del
    /// processo uscito e non tocca questo: declassarlo a successo mentre si
    /// chiude un buco vicino toglierebbe un presidio che gia' funzionava.
    #[test]
    fn vivo_senza_ascolto_sulla_porta_promessa_resta_un_fallimento_transitorio() {
        let esito = classifica_avvio(
            &fatti(
                UscitaCapostipite::NonUscito,
                EsitoPorta::Muta { porta: 31000 },
            ),
            20_000,
            String::new(),
        );
        assert!(matches!(
            esito,
            AvvioServizio::VivoMaSilenzioso {
                porta_attesa: Some(31000),
                ..
            }
        ));
        let risposta = esito.risposta("x", None);
        assert!(risposta.esito.e_fallito(), "{risposta:?}");
        assert_eq!(
            risposta.natura,
            Some(nexus_types::tool_outcome::NaturaFallimento::Transitorio),
            "il processo e' VIVO: la causa piu' frequente e' un avvio lento"
        );
    }

    /// La nota diagnostica del chiamante precede l'output, perche' cambia cosa
    /// l'agente deve cercarci dentro.
    #[test]
    fn la_nota_diagnostica_precede_l_output() {
        let risposta = AvvioServizio::MortoSubito {
            exit_code: Some(1),
            output: "Cannot find module".into(),
        }
        .risposta("x", Some("Il servizio ascolta sulla porta 24804.\n"));
        let nota = risposta.testo.find("ascolta sulla porta").expect("nota");
        let output = risposta.testo.find("Cannot find module").expect("output");
        assert!(nota < output, "nota dopo l'output: {}", risposta.testo);
    }

    /// Nessuna porta attesa e processo vivo: e' cio' che doveva succedere, e il
    /// messaggio non deve contenere un'avvertenza. Senza la distinzione,
    /// l'avviso andrebbe a ogni worker e diventerebbe rumore.
    #[test]
    fn senza_porta_attesa_non_si_avverte_di_un_silenzio_che_non_e_un_difetto() {
        let esito = classifica_avvio(
            &fatti(UscitaCapostipite::NonUscito, EsitoPorta::NessunaAttesa),
            5_000,
            String::new(),
        );
        let risposta = esito.risposta("intestazione", None);
        assert!(!risposta.esito.e_fallito());
        assert!(
            !risposta.testo.contains("NESSUN ASCOLTO"),
            "nessuna porta attesa: niente da avvertire, invece: {}",
            risposta.testo
        );
    }

    /// La riga non letta NON autorizza a dichiarare morto (stessa disciplina di
    /// `StatoProcesso::autorizza_a_dichiararlo_morto`): un guasto del DB non
    /// deve diventare un fallimento imputato all'agente.
    #[test]
    fn una_riga_non_letta_non_diventa_una_morte() {
        let esito = classifica_avvio(
            &fatti(UscitaCapostipite::NonOsservata, EsitoPorta::NessunaAttesa),
            1_000,
            String::new(),
        );
        assert!(matches!(esito, AvvioServizio::VivoMaSilenzioso { .. }));
        assert!(!esito.risposta("x", None).esito.e_fallito());
    }

    /// Il fatto si legge dai CAMPI STRUTTURATI, mai dal testo (regola M): un
    /// output che grida «Error» su un processo ancora in corsa non e' una
    /// morte, e un'uscita silenziosa lo e'.
    #[test]
    fn l_uscita_si_legge_dai_campi_e_non_dall_output() {
        let in_corsa = ProcessOutput {
            command: "node index.js".into(),
            pid: Some(42),
            status: "running".into(),
            exit_code: None,
            stdout: String::new(),
            stderr: "Error: qualcosa di rumoroso ma non fatale".into(),
        };
        assert_eq!(
            uscita_dalla_riga(Some(&in_corsa)),
            UscitaCapostipite::NonUscito
        );

        let uscito = ProcessOutput {
            status: "failed".into(),
            exit_code: Some(1),
            stderr: String::new(),
            ..in_corsa.clone()
        };
        assert_eq!(
            uscita_dalla_riga(Some(&uscito)),
            UscitaCapostipite::Uscito { exit_code: Some(1) }
        );
    }

    /// IL CASO MISURATO, con la fixture VERA e il runtime VERO.
    ///
    /// Non simula niente: scrive l'`index.js` del caso reale (un `require` di
    /// un modulo che in `node_modules` non c'e', perche' `node_modules` non
    /// esiste affatto), lo esegue con `node`, e costruisce i fatti dai valori
    /// che il SO ha davvero prodotto — exit code da `status.code()`, testo da
    /// `stderr`. La riga di `agent_processes` porta esattamente questi due
    /// campi, scritti dal task che fa `child.wait()`: qui li si misura invece
    /// di scriverli, cosi' il test non fissa l'assunto che dovrebbe verificare
    /// (regola O).
    ///
    /// MUTAZIONE ESEGUITA: facendo ritornare `classifica_avvio` la variante
    /// `Vivo` prima del controllo sull'uscita — cioe' il comportamento del ramo
    /// senza porta attesa, che dichiarava riuscito qualunque spawn — il test
    /// rosseggia su `MortoSubito`, e con esso `la_morte_del_lancio_produce_una_
    /// risposta_fallita_e_rimediabile`.
    #[test]
    fn un_index_js_senza_le_proprie_dipendenze_e_una_morte_del_lancio() {
        let Some(node) = runtime_node() else {
            // SALTATO e DICHIARATO: un test verde per assenza dello strumento
            // e' peggio di un test che non c'e'.
            eprintln!("SALTATO: `node` non e' invocabile su questa macchina");
            return;
        };
        let dir = std::env::temp_dir().join(format!("nexus-avvio-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir di prova");
        let entry = dir.join("index.js");
        // Il contenuto del caso reale: express dichiarato in package.json e mai
        // installato. Nessun `node_modules` in questa directory.
        std::fs::write(
            &entry,
            "const express = require('express');\n\
             const app = express();\n\
             app.listen(process.env.PORT || 3000);\n",
        )
        .expect("fixture");

        let uscita = std::process::Command::new(&node)
            .arg("index.js")
            .current_dir(&dir)
            .output()
            .expect("node deve poter eseguire la fixture");
        let _ = std::fs::remove_dir_all(&dir);

        // I FATTI, misurati: nessun letterale.
        let info = ProcessOutput {
            command: "node index.js".into(),
            pid: None,
            status: if uscita.status.success() {
                "stopped".into()
            } else {
                "failed".into()
            },
            exit_code: uscita.status.code(),
            stdout: String::from_utf8_lossy(&uscita.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&uscita.stderr).into_owned(),
        };
        assert_eq!(
            info.exit_code,
            Some(1),
            "la fixture deve fallire davvero, altrimenti il test non misura il caso"
        );

        let fatti = FattiAvvio {
            uscita: uscita_dalla_riga(Some(&info)),
            // La porta assegnata resta muta: il processo non e' mai arrivato ad
            // ascoltare. E' cio' che `await_port_ready` osserverebbe.
            porta: EsitoPorta::Muta { porta: 27113 },
        };
        let esito = classifica_avvio(&fatti, 1_800, testo_di_output(&info));

        let AvvioServizio::MortoSubito { exit_code, output } = &esito else {
            panic!("il processo e' uscito con 1: non e' un avvio riuscito, e' {esito:?}");
        };
        assert_eq!(*exit_code, Some(1));
        assert!(
            output.contains("Cannot find module"),
            "la causa vera deve sopravvivere fino all'esito: {output}"
        );

        // E la conseguenza: cio' che l'agente riceve davvero.
        let risposta = esito.risposta("Servizio 'backend' (process_id: X)", None);
        assert!(
            risposta.esito.e_fallito(),
            "il run era proseguito ignaro proprio perche' qui c'era un successo: {risposta:?}"
        );
        assert!(
            risposta.testo.contains("Cannot find module"),
            "senza la diagnosi nel tool_result l'agente non ha nulla da correggere: {}",
            risposta.testo
        );
    }

    /// `node` invocabile, oppure niente. Si interroga il runtime vero invece di
    /// assumerlo presente: su una macchina senza node il test deve dichiararsi
    /// saltato, non passare.
    fn runtime_node() -> Option<String> {
        for candidato in ["node", "node.exe"] {
            if std::process::Command::new(candidato)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
            {
                return Some(candidato.to_string());
            }
        }
        None
    }

    /// REGOLA L, il test che vede un secondo criterio: sugli STESSI fatti —
    /// capostipite uscito, ma una porta del servizio in ascolto — questo modulo
    /// e il punto unico della vita di un servizio devono dire la stessa cosa.
    ///
    /// E' il caso Windows in cui la shell registrata se n'e' andata e il server
    /// e' un discendente che le sopravvive. Se qualcuno scrivesse qui una
    /// precedenza propria (per esempio «morto batte tutto», che e' la lettura
    /// istintiva), questo test rosseggerebbe.
    #[test]
    fn sulla_shell_uscita_col_server_vivo_i_due_criteri_rispondono_uguale() {
        use crate::process_liveness::{CausaMorte, StatoProcesso};
        use crate::project_workspace::service_liveness::{
            classifica_servizio, AscoltoPorte, StatoServizio,
        };

        let porta = 27113u16;
        let secondo_il_punto_unico = classifica_servizio(
            StatoProcesso::Morto(CausaMorte::Uscito),
            AscoltoPorte::Ascolta { porta, pid: 9931 },
        );
        assert!(
            matches!(secondo_il_punto_unico, StatoServizio::Vivo(_)),
            "premessa del confronto: {secondo_il_punto_unico:?}"
        );

        let secondo_questo_modulo = classifica_avvio(
            &FattiAvvio {
                uscita: UscitaCapostipite::Uscito { exit_code: Some(0) },
                porta: EsitoPorta::Risponde { porta },
            },
            500,
            String::new(),
        );
        assert!(
            matches!(secondo_questo_modulo, AvvioServizio::Vivo { .. }),
            "il punto unico lo dichiara vivo, qui risulta {secondo_questo_modulo:?}: \
             sono due criteri diversi sulla stessa domanda"
        );
    }

    /// stdout e stderr arrivano all'agente in un blocco solo: la diagnosi non
    /// deve dipendere da quale descrittore il runtime ha scelto.
    #[test]
    fn l_output_riunisce_i_due_descrittori() {
        let info = ProcessOutput {
            command: "node index.js".into(),
            pid: Some(1),
            status: "failed".into(),
            exit_code: Some(1),
            stdout: "avvio in corso".into(),
            stderr: "Error: Cannot find module 'express'".into(),
        };
        let testo = testo_di_output(&info);
        assert!(testo.contains("avvio in corso"));
        assert!(testo.contains("Cannot find module"));
    }
}

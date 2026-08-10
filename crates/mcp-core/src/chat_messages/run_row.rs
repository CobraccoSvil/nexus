//! Nascita della riga di `agent_runs`: l'unico punto in cui un run comincia a
//! esistere per il DB, e l'unico che dichiara se ci e' riuscito.
//!
//! ## Perche' un punto unico (regola L)
//!
//! I tre percorsi che fanno nascere un run scrivevano ognuno la propria INSERT,
//! e tutti e tre ne buttavano via l'esito con `let _ = sqlx::query(...)`:
//!
//! | percorso | file | status | colonna in piu' |
//! |---|---|---|---|
//! | turno agentico | `agent_run::spawn_agent_run` | `running` | — |
//! | nessun provider capace | `agent_run::no_capable_provider_stop` | `failed` | — |
//! | ripresa di un run interrotto | `handlers::try_resume_interrupted_run` | `running` | `parent_run_id` |
//!
//! ## Perche' il motivo del fallimento NON e' una colonna
//!
//! La prima stesura di questo punto unico prese le colonne dall'UNIONE dei tre
//! percorsi, e il percorso "nessun provider capace" ne portava una quarta:
//! `error`. Quella colonna non esiste in `agent_runs` — non la crea
//! `0002_run.sql`, non l'aggiunge nessuna migrazione project, e nell'intero
//! workspace nessuna query la legge. Il percorso raro era percio' rotto DA
//! SEMPRE, e in silenzio: e' esattamente il difetto che questo modulo denuncia,
//! trovato nel percorso che lo denunciava.
//!
//! Unificando, pero', il difetto smise di essere raro: la INSERT unica la
//! citava per tutti e tre, quindi NESSUN run nasceva piu'. MISURATO in esercizio
//! il 10/08/2026 su un progetto nuovo, col binario delle 12:12: `la colonna
//! "error" della relazione "agent_runs" non esiste`, run non nato, fallback al
//! turno singolo, e il modello che risponde «non ho alcun task da eseguire»
//! perche' il turno agentico non e' mai partito. L'ultimo run nato su
//! `vetrina-statica` risaliva al binario precedente (06:23:23Z).
//!
//! Il rimedio NON e' aggiungere la colonna: sarebbe un campo scritto e mai
//! letto, il debito che la regola L chiede di non creare. Il motivo raggiunge
//! gia' l'utente per un altro canale — `emit_provider_unavailable` emette
//! l'alert SSE, e lo fa anche quando la riga non nasce.
//!
//! Tre copie della stessa INSERT sono debito; tre copie che ignorano l'errore
//! sono un difetto (regola M): il chiamante prosegue con un `run_id` che in
//! tabella non esiste, spawna il task, e ogni UPDATE successivo su quel run non
//! trova righe. L'utente non riceve nulla e nessun log lo dice.
//!
//! ## Perche' e' la seconda forma del messaggio orfano
//!
//! La scrittura e' preceduta, in due percorsi su tre, da `supersede_active_runs`,
//! che mette `cancelled` TUTTI i run attivi della sessione PRIMA di questa
//! INSERT. Se la INSERT fallisce e l'errore e' ingoiato, la sessione resta senza
//! alcun run: i precedenti cancellati, il nuovo mai nato. Il messaggio utente
//! e' gia' persistito e resta li' senza esito — indistinguibile, per chi legge,
//! da un turno Study che un run non lo prevede.
//!
//! Misurato il 09/08/2026 su `vetrina-statica`: 1 messaggio utente su 5 senza
//! run ne' risposta (`e7ec13e5`, 20:55:43Z). In quel caso l'INSERT non era
//! nemmeno stata raggiunta, ma il difetto e' lo stesso visto un passo prima:
//! fra il messaggio persistito e il run che lo onora non c'e' nessun campo che
//! dichiari l'esito, e qui l'unico segnale che esisteva veniva scartato.
//!
//! ## Esito tipizzato (regola Q)
//!
//! `EsitoNascitaRun` porta l'esito in un campo, non nel testo. Non e' un `bool`:
//! `GiaEsistente` (chiave primaria duplicata, 23505) e' un caso a se' — la riga
//! c'e', il chiamante puo' proseguire — mentre `NonScritta` significa che il run
//! non esiste e proseguire fabbricherebbe lavoro su un id fantasma.

use sqlx::PgPool;
use uuid::Uuid;

/// SQLSTATE di `unique_violation`: la riga di quel `run_id` c'e' gia'.
const SQLSTATE_UNIQUE_VIOLATION: &str = "23505";

/// I campi con cui un run comincia a esistere. `status` e `parent_run_id` sono
/// cio' che distingue i tre percorsi di nascita: stanno qui come CAMPI e non
/// come tre funzioni, cosi' aggiungere un percorso non aggiunge una quarta
/// copia della INSERT.
///
/// Ogni campo qui dentro DEVE corrispondere a una colonna che
/// `db/migrations/project` crea davvero: un campo in piu' non fa fallire il
/// percorso che lo porta, fa fallire la INSERT di TUTTI (vedi la nota sul
/// motivo del fallimento in testa al modulo).
pub(crate) struct NuovaRigaRun<'a> {
    pub run_id: Uuid,
    pub session_id: Uuid,
    pub project_id: Uuid,
    pub user_id: Uuid,
    /// Messaggio UTENTE che ha innescato il run: e' l'ancora da cui la chat
    /// ritrova il run (`load_session_message_views` ci fa il LEFT JOIN sopra).
    pub run_message_id: Uuid,
    pub status: &'a str,
    pub automation_mode: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    pub supervisor_mode: &'a str,
    /// Valorizzato solo dalla ripresa: il run nuovo discende da quello ripreso.
    pub parent_run_id: Option<Uuid>,
}

/// Esito della nascita di un run (regola Q: l'esito e' un campo, l'ignoto una
/// variante). `NonScritta` porta il messaggio del DB per il log del chiamante,
/// mai per essere riletto da codice (regola M).
#[derive(Debug)]
pub(crate) enum EsitoNascitaRun {
    /// La riga e' stata inserita: da qui in poi il run esiste per il DB.
    Scritta,
    /// Un run con quell'id c'era gia' (23505). Il chiamante puo' proseguire:
    /// l'invariante che gli serve — "la riga esiste" — e' soddisfatta.
    GiaEsistente,
    /// La riga NON esiste. Proseguire significherebbe lavorare su un run
    /// fantasma: il chiamante deve degradare, mai andare avanti.
    NonScritta { causa: String },
}

impl EsitoNascitaRun {
    /// La riga di run esiste in tabella? Unico predicato con cui un chiamante
    /// decide se puo' proseguire.
    pub(crate) fn run_esiste(&self) -> bool {
        matches!(self, Self::Scritta | Self::GiaEsistente)
    }
}

/// Inserisce la riga iniziale di un run e DICHIARA se c'e' riuscita.
///
/// Non propaga un `Result`: il chiamante non ha sempre un canale d'errore verso
/// l'utente (lo spawn e' gia' oltre il punto in cui la risposta HTTP e' decisa),
/// e un `?` qui sostituirebbe un difetto silenzioso con un 500 dove prima c'era
/// un degrado. L'esito e' un valore che il chiamante DEVE guardare: il tipo non
/// e' `()`, quindi ignorarlo e' una scelta scritta, non una svista.
pub(crate) async fn inserisci_riga_run(pool: &PgPool, riga: NuovaRigaRun<'_>) -> EsitoNascitaRun {
    let esito = sqlx::query(
        r#"INSERT INTO agent_runs
           (id, session_id, project_id, user_id, run_message_id, status,
            automation_mode, provider, model, supervisor_mode, iteration_count,
            parent_run_id, created_at)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,0,$11,NOW())"#,
    )
    .bind(riga.run_id)
    .bind(riga.session_id)
    .bind(riga.project_id)
    .bind(riga.user_id)
    .bind(riga.run_message_id)
    .bind(riga.status)
    .bind(riga.automation_mode)
    .bind(riga.provider)
    .bind(riga.model)
    .bind(riga.supervisor_mode)
    .bind(riga.parent_run_id)
    .execute(pool)
    .await;

    match esito {
        Ok(_) => EsitoNascitaRun::Scritta,
        Err(e) => classifica_errore_insert(e, &riga),
    }
}

/// Traduce l'errore della INSERT nell'esito che il chiamante guarda.
///
/// Il discriminante e' il codice SQLSTATE, mai il testo del messaggio (regola
/// M): `23505` dice che la riga di quel `run_id` c'era gia' — il run esiste e
/// si prosegue — mentre qualunque altro errore dice che non esiste. Le due
/// letture hanno rimedi opposti e il testo del DB, che cambia per versione e
/// per lingua, non le distingue in modo affidabile.
fn classifica_errore_insert(e: sqlx::Error, riga: &NuovaRigaRun<'_>) -> EsitoNascitaRun {
    let duplicato = matches!(
        &e,
        sqlx::Error::Database(db) if db.code().as_deref() == Some(SQLSTATE_UNIQUE_VIOLATION)
    );
    if duplicato {
        tracing::warn!(
            run_id = %riga.run_id,
            session_id = %riga.session_id,
            "nascita run: riga gia' presente (23505), il run esiste"
        );
        return EsitoNascitaRun::GiaEsistente;
    }
    tracing::error!(
        run_id = %riga.run_id,
        session_id = %riga.session_id,
        project_id = %riga.project_id,
        status = riga.status,
        error = %e,
        "nascita run: INSERT fallita, il run NON esiste in tabella"
    );
    EsitoNascitaRun::NonScritta {
        causa: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// I tre percorsi di nascita differiscono per `status` e `parent_run_id`:
    /// la struttura li rappresenta tutti e tre senza che nessuno debba
    /// riscrivere la INSERT.
    #[test]
    fn i_tre_percorsi_stanno_nella_stessa_struttura() {
        let id = Uuid::nil();
        let agentico = NuovaRigaRun {
            run_id: id,
            session_id: id,
            project_id: id,
            user_id: id,
            run_message_id: id,
            status: "running",
            automation_mode: "automatic",
            provider: "mistral",
            model: "mistral-small-latest",
            supervisor_mode: "none",
            parent_run_id: None,
        };
        assert_eq!(agentico.status, "running");
        assert!(agentico.parent_run_id.is_none());

        let senza_provider = NuovaRigaRun {
            status: "failed",
            ..agentico
        };
        assert_eq!(senza_provider.status, "failed");

        let ripresa = NuovaRigaRun {
            parent_run_id: Some(id),
            ..agentico
        };
        assert!(ripresa.parent_run_id.is_some());
    }

    /// La INSERT gira contro lo SCHEMA REALE del DB-progetto, non contro una
    /// struttura in memoria (regola O).
    ///
    /// I due test qui sopra costruiscono `NuovaRigaRun` e asseriscono sui suoi
    /// campi: nessuno dei due tocca il DB, quindi NESSUNO dei due poteva vedere
    /// che una delle colonne citate dalla INSERT non esiste. E infatti non l'ha
    /// vista: `error` e' arrivata in produzione, dove ha impedito la nascita di
    /// OGNI run (10/08/2026, misura in testa al modulo).
    ///
    /// MUTAZIONE che lo fa rosseggiare, ed e' il difetto reale: rimettere
    /// `error` fra le colonne della INSERT (con il suo `$` e il suo bind). Il
    /// DB risponde `la colonna "error" della relazione "agent_runs" non
    /// esiste`, l'esito diventa `NonScritta` e l'assert cade — col messaggio
    /// del difetto vero, non con un fallimento generico.
    #[sqlx::test(migrator = "crate::test_support::PROJECT_MIGRATOR")]
    async fn la_riga_nasce_davvero_nello_schema_di_progetto(pool: PgPool) {
        let project_id = Uuid::new_v4();
        let session_id = crate::test_support::seed_chat_session(&pool, project_id).await;
        let run_message_id =
            crate::test_support::seed_chat_message(&pool, session_id, project_id).await;
        let run_id = Uuid::new_v4();

        let esito = inserisci_riga_run(
            &pool,
            NuovaRigaRun {
                run_id,
                session_id,
                project_id,
                user_id: Uuid::new_v4(),
                run_message_id,
                status: "running",
                automation_mode: "automatic",
                provider: "mistral",
                model: "mistral-small-latest",
                supervisor_mode: "none",
                parent_run_id: None,
            },
        )
        .await;

        assert!(
            esito.run_esiste(),
            "la riga di run non e' nata sullo schema reale: {esito:?}"
        );
        let presenti: i64 = sqlx::query_scalar("SELECT count(*) FROM agent_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .expect("conteggio agent_runs");
        assert_eq!(presenti, 1, "il run non e' in tabella dopo una INSERT dichiarata riuscita");
    }

    /// Il predicato con cui il chiamante decide se proseguire distingue le tre
    /// varianti nel modo che conta: la riga ESISTE, o non esiste.
    ///
    /// MUTAZIONE che lo fa rosseggiare: far rientrare `NonScritta` fra le
    /// varianti di `run_esiste` (`matches!(self, Self::Scritta | Self::GiaEsistente | Self::NonScritta{..})`),
    /// cioe' il comportamento del `let _ =` che questo modulo sostituisce — il
    /// chiamante proseguiva sempre, run in tabella o no.
    #[test]
    fn solo_scritta_e_gia_esistente_autorizzano_a_proseguire() {
        assert!(EsitoNascitaRun::Scritta.run_esiste());
        assert!(EsitoNascitaRun::GiaEsistente.run_esiste());
        assert!(!EsitoNascitaRun::NonScritta {
            causa: "connessione caduta".to_string()
        }
        .run_esiste());
    }
}

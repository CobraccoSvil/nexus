//! L'ambiente reale nel system prompt: punto unico (regola L) di COME il fatto
//! rilevato entra nel contesto di chi eseguira' i comandi.
//!
//! ## ROOT CAUSE (02/08/2026, progetto bacheca-attivita)
//!
//! La figura `verify` (sub-run a5f7419c) ha esaurito i suoi 180s in 16 iterazioni
//! senza produrre nulla, spendendone la coda per scoprire — un tentativo alla
//! volta — che stava girando su Windows: `which jq` a vuoto, un accertamento che
//! `jq` non c'e', e infine `sudo apt-get update`, che su quell'host non puo'
//! funzionare (il gestore privilegiato e' un binario Linux).
//!
//! Non era ignoranza casuale. Il blocco `<privilegi_sistema>` di
//! `system.nexus_base` AFFERMA che si installano pacchetti con
//! `sudo apt-get install -y`, e lo afferma con l'autorita' del system prompt. Su
//! un host senza apt non e' un'omissione: e' un'indicazione sbagliata verso un
//! vicolo cieco, e nessuna quantita' di iterazioni la corregge.
//!
//! ## Le due mosse, e perche' sono UNA sola funzione
//!
//! 1. Cio' che il sistema SA lo dichiara: sistema operativo, shell che eseguira'
//!    davvero i comandi, gestori di pacchetti presenti e — soprattutto — quelli
//!    ASSENTI, nominati. E' il fatto rilevato da
//!    [`nexus_agent_tools::ambiente`].
//! 2. Cio' che il prompt AFFERMA di poter fare va tolto quando l'host non lo
//!    consente: la direttiva sui privilegi presuppone un gestore, e se quel
//!    gestore non c'e' la direttiva sparisce.
//!
//! Separarle produrrebbe il caso peggiore: un contesto che dice «apt-get non
//! esiste qui» e, poche righe dopo, «per installare usa `sudo apt-get`». Un
//! contesto che si contraddice e' peggio di uno incompleto, quindi la
//! dichiarazione e la rimozione avvengono nello stesso punto o non avvengono.
//!
//! ## Chi lo chiama, e perche' sta DENTRO i compositori
//!
//! I due percorsi che compongono un system prompt di esecuzione: la chat
//! (`chat_messages::handlers::compose_chat_system_context`) e il sub-run
//! (`agent_tools::subagent_native::resolve_system_text`). L'innesto vive dentro
//! quelle funzioni, non nei loro chiamanti: e' la lezione di
//! `prompt_memories` (in mcp-core) — finche' il consumo stava sul ramo di un
//! chiamante solo, l'altro percorso restava scoperto e nessuno se ne accorgeva.
//!
//! ## Costo sul prefisso riusabile: nessuno
//!
//! Il blocco e' STABILE per tutta la vita del processo (sistema operativo e
//! shell non cambiano; i gestori hanno una cache a TTL), e viene appeso senza
//! `CONFINE_DI_TURNO`: resta nella parte stabile del system e non tocca il
//! prefisso che il fornitore riusa (vedi `nexus_types::system_prompt`).
//!
//! Il «resta» pero' non dipende solo da qui: il confine e' POSIZIONALE, quindi
//! chiamare questa funzione dopo un blocco che il confine lo introduce spinge
//! anche il fatto d'ambiente fuori dal prefisso. Per questo il compositore della
//! chat la chiama PRIMA di appendere il blocco Knowledge Base, e lo asserisce
//! (`tests_system_prompt_della_chat`).

use nexus_agent_tools::ambiente::{self, Disponibilita};
use sqlx::PgPool;

/// Tag del blocco che AFFERMA la capacita' di installare pacchetti con
/// privilegi (`system.nexus_base`). E' il testo che su un host senza quel
/// gestore manda l'agente contro un muro.
const TAG_PRIVILEGI_APERTURA: &str = "<privilegi_sistema>";
const TAG_PRIVILEGI_CHIUSURA: &str = "</privilegi_sistema>";

/// Rende un system prompt COERENTE con l'host su cui girera'.
///
/// Non fallisce mai verso l'alto: un DB irraggiungibile lascia il vocabolario
/// vuoto e il blocco dichiara comunque sistema operativo e shell, che sono la
/// parte che non dipende da nessuna configurazione.
pub async fn con_ambiente(db: &PgPool, system: String) -> String {
    let ambiente = ambiente::rileva(db).await;
    // La direttiva si toglie solo su un'assenza ACCERTATA. `NonInterrogabile`
    // (PATH illeggibile, gestore fuori vocabolario, presupposto non dichiarato)
    // lascia il prompt com'e': togliere testo perche' non si e' potuto guardare
    // sarebbe decidere su un'ignoranza.
    let mut testo = match ambiente::gestore_privilegiato(db).await {
        Some(nome) if ambiente.stato_di(&nome) == Disponibilita::Assente => {
            crate::blocchi::strip_block_between(
                &system,
                TAG_PRIVILEGI_APERTURA,
                TAG_PRIVILEGI_CHIUSURA,
            )
        }
        _ => system,
    };
    // Idempotenza: un system gia' dichiarato che ripassa di qui resta invariato.
    // La domanda la pone il punto unico, che sola conosce la forma del blocco.
    if ambiente::blocco_gia_presente(&testo) {
        return testo;
    }
    testo.push_str("\n\n");
    testo.push_str(&ambiente.blocco());
    testo.push('\n');
    testo
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_agent_tools::ambiente::{AmbienteEsecuzione, GestorePacchetti};

    /// Il testo di `system.nexus_base` che afferma la capacita' privilegiata,
    /// nella forma con cui vive in DB (mig 0463 e seguenti).
    const SYSTEM_CON_PRIVILEGI: &str = "\
<role>Sei Nexus.</role>

<privilegi_sistema>
Puoi installare dipendenze di SISTEMA con run_command:
  sudo apt-get install -y <pacchetto>
</privilegi_sistema>

<final_summary>chiudi con un riepilogo</final_summary>";

    fn ambiente_windows() -> AmbienteEsecuzione {
        AmbienteEsecuzione {
            sistema_operativo: "windows",
            shell: r"C:\Program Files\Git\bin\bash.exe".to_string(),
            gestori: vec![GestorePacchetti {
                nome: "apt-get".to_string(),
                stato: Disponibilita::Assente,
            }],
        }
    }

    /// Il taglio della direttiva passa dal punto unico dello strip, quindi il
    /// test lo esercita per la strada della produzione (regola O): quello che
    /// verifica e' il comportamento REALE su un testo nella forma del template.
    fn strip(system: &str) -> String {
        crate::blocchi::strip_block_between(
            system,
            TAG_PRIVILEGI_APERTURA,
            TAG_PRIVILEGI_CHIUSURA,
        )
    }

    /// IL difetto: su un host senza apt, il system prompt non deve piu' dire di
    /// usare apt. Il resto del prompt resta intatto — si toglie un blocco, non
    /// si riscrive il testo.
    ///
    /// MUTAZIONE: rimettere la direttiva (saltare lo strip) fa ricomparire
    /// `apt-get` nel prompt e la prima asserzione cade.
    #[test]
    fn su_un_host_senza_il_gestore_la_direttiva_privilegiata_sparisce() {
        let ridotto = strip(SYSTEM_CON_PRIVILEGI);
        assert!(!ridotto.contains("apt-get"), "{ridotto}");
        assert!(!ridotto.contains("privilegi_sistema"), "{ridotto}");
        assert!(ridotto.contains("<role>Sei Nexus.</role>"), "{ridotto}");
        assert!(ridotto.contains("<final_summary>"), "{ridotto}");
    }

    /// L'altra meta': il contesto non puo' limitarsi a TACERE su apt, deve dire
    /// che non c'e'. Un modello addestrato su host Linux, davanti al silenzio,
    /// prova — ed e' esattamente cio' che ha fatto la figura misurata.
    #[test]
    fn il_contesto_dichiara_l_assenza_non_si_limita_a_tacerla() {
        let blocco = ambiente_windows().blocco();
        assert!(blocco.contains("apt-get: NON disponibile"), "{blocco}");
        assert!(blocco.contains("windows"), "{blocco}");
        assert!(blocco.contains("bash.exe"), "{blocco}");
    }

    /// Le due mosse insieme: il testo finale non deve MAI contenere sia
    /// l'affermazione sia la sua smentita. E' la ragione per cui vivono in una
    /// funzione sola.
    #[test]
    fn il_prompt_risultante_non_si_contraddice() {
        let ambiente = ambiente_windows();
        let finale = format!("{}\n\n{}", strip(SYSTEM_CON_PRIVILEGI), ambiente.blocco());
        assert!(
            !finale.contains("sudo apt-get install"),
            "il prompt afferma e smentisce nella stessa pagina: {finale}"
        );
        assert!(finale.contains("apt-get: NON disponibile"), "{finale}");
    }

    /// Su un host DOVE il gestore c'e', la direttiva resta: il gate toglie testo
    /// solo su un'assenza accertata, non per default.
    #[test]
    fn dove_il_gestore_esiste_la_direttiva_resta() {
        let linux = AmbienteEsecuzione {
            sistema_operativo: "linux",
            shell: "/bin/bash".to_string(),
            gestori: vec![GestorePacchetti {
                nome: "apt-get".to_string(),
                stato: Disponibilita::Disponibile,
            }],
        };
        assert_eq!(linux.stato_di("apt-get"), Disponibilita::Disponibile);
        // Il ramo di `con_ambiente` che tocca il testo scatta solo su `Assente`:
        // qui il system resta quello di partenza.
        assert!(SYSTEM_CON_PRIVILEGI.contains("sudo apt-get install"));
    }

    /// LA prova end-to-end, sul vocabolario REALE (la migrazione 0670 e' nel set
    /// META, quindi il migrator la porta) e sull'host REALE su cui gira il test:
    /// nessun ambiente costruito a mano, nessun setting inventato (regola O).
    ///
    /// L'asserzione e' l'INVARIANTE, non l'esito su una piattaforma: qualunque
    /// sia l'host, il prompt non puo' contemporaneamente ordinare di usare un
    /// gestore e dichiararlo assente. Su Linux la direttiva resta e il blocco lo
    /// conferma disponibile; su Windows la direttiva sparisce e il blocco lo
    /// dichiara assente. Un test che asserisse «apt-get non c'e'» misurerebbe la
    /// macchina di chi lo esegue, non il codice.
    ///
    /// MUTAZIONE: far tornare `con_ambiente` il testo ricevuto (saltare le due
    /// mosse) lascia il prompt senza il blocco e l'ultima asserzione cade; farne
    /// solo una delle due rompe la prima, che e' il caso peggiore — un contesto
    /// che si contraddice.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_system_composto_e_coerente_con_l_host_reale(pool: PgPool) {
        let finale = con_ambiente(&pool, SYSTEM_CON_PRIVILEGI.to_string()).await;

        let ordina_apt = finale.contains("sudo apt-get install");
        let dichiara_assente = finale.contains("apt-get: NON disponibile");
        assert!(
            !(ordina_apt && dichiara_assente),
            "il prompt afferma e smentisce nella stessa pagina:\n{finale}"
        );
        // Qualunque host: il fatto entra sempre, e porta le due cose che non
        // dipendono da nessuna configurazione.
        assert!(ambiente::blocco_gia_presente(&finale), "{finale}");
        assert!(finale.contains(std::env::consts::OS), "{finale}");
        assert!(finale.contains("Shell con cui i tuoi comandi"), "{finale}");
        // Il resto del system prompt non viene toccato: si aggiunge un blocco (e
        // al piu' se ne toglie uno), non si riscrive il testo.
        assert!(finale.contains("<role>Sei Nexus.</role>"), "{finale}");
        assert!(finale.contains("<final_summary>"), "{finale}");
    }

    /// Un system gia' dichiarato che ripassa dal compositore non raddoppia il
    /// blocco: l'idempotenza qui non e' eleganza, e' cio' che evita due
    /// dichiarazioni d'ambiente in un prompt il giorno in cui un percorso ne
    /// chiamasse due (il resend che ricompone sopra un testo gia' composto).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn il_blocco_non_si_raddoppia(pool: PgPool) {
        let una = con_ambiente(&pool, SYSTEM_CON_PRIVILEGI.to_string()).await;
        let due = con_ambiente(&pool, una.clone()).await;
        assert_eq!(una, due);
        assert_eq!(due.matches(ambiente::TAG_BLOCCO).count(), 1);
    }

    /// Un gestore MAI SONDATO non autorizza a togliere niente: `Assente` e
    /// `NonInterrogabile` sono due fatti diversi, e solo il primo e' una prova.
    #[test]
    fn un_gestore_non_sondato_non_e_una_prova_di_assenza() {
        let ambiente = AmbienteEsecuzione {
            sistema_operativo: "windows",
            shell: "bash".to_string(),
            gestori: Vec::new(),
        };
        assert_eq!(
            ambiente.stato_di("apt-get"),
            Disponibilita::NonInterrogabile,
            "senza vocabolario non si e' guardato: non si puo' dire che manchi"
        );
    }
}

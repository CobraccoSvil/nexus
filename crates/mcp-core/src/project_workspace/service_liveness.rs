//! PUNTO UNICO (regola L) della domanda «questo SERVIZIO e' vivo?», che NON e'
//! la stessa di «questo processo e' vivo?» ([`crate::process_liveness`], a cui
//! questo modulo delega la prima meta' della risposta).
//!
//! CAUSA RADICE (misurata su gestione-corsi, 07-08/08/2026). `spawn_agent_process`
//! esegue il comando del servizio tramite `agent_shell` e registra `child.id()`:
//! il pid della SHELL, non del server. Su Windows i figli sopravvivono al padre,
//! quindi il capostipite registrato puo' morire lasciando in piedi tutto cio' che
//! conta. Misura:
//!
//! - `agent_processes` dava `schoolcoursesfe` = `failed`, pid 20728, `started_at`
//!   2026-08-07 21:03:56Z, e quel pid non esisteva davvero: il criterio sul
//!   PROCESSO diceva il vero;
//! - la porta 34859, che `nexus_port_allocations` assegna a quella label
//!   sull'unit `gestione-corsi-schoolcoursesfe.service`, era in ascolto dal pid
//!   3860 — `node`, avviato DUE SECONDI dopo la registrazione, con
//!   `school-courses-fe/node_modules/next/dist/server/lib/start-server.js` a riga
//!   di comando;
//! - la catena `bash -> bash -> npm -> cmd -> next -> node` era viva fin sopra al
//!   25384, e oltre quello il capostipite era morto.
//!
//! Il servizio girava. Il pannello lo mostrava «inattivo (dead)», e nessun
//! criterio piu' preciso sul PID avrebbe potuto accorgersene: la risposta non
//! stava nel pid, stava altrove.
//!
//! LA SECONDA PROVA. Un'allocazione di porta e' chiavata su `(project_id, label)`
//! e la `service_unit` ne DISCENDE, quindi un listener su una porta allocata a
//! questo servizio e' una prova strutturale che qualcosa di suo e' vivo. E' lo
//! stesso criterio con cui `service_recovery::judge_recovery` dichiara riuscita
//! una remediation (stato Running E una porta ALLOCATA a quella unit che
//! risponde): qui viene posto anche dove si decide se un servizio e' morto —
//! perche' un servizio non si giudica morto da una prova sola quando ne esistono
//! due, ed e' la prova che il registro NON possiede.
//!
//! ORDINE DELLE PROVE. Prima il pid, poi la porta. La porta risponde «qualcosa di
//! mio ascolta li'», che basta per non dichiararlo morto ma non identifica il
//! processo: chi ha bisogno di un pid su cui agire (fermare, campionare) usa
//! quello registrato, e lo ha solo dalla prima prova.
//!
//! CHI DEVE PASSARE DI QUI. Ogni punto che DICHIARA morto un servizio, non solo
//! quelli che lo mostrano. MISURATO l'08/08/2026 su gestione-corsi, riproducendo
//! il caso a comando: ucciso il solo capostipite registrato (`bash` 10896), il
//! server e' rimasto vivo (`SchoolCoursesApi.exe` 12284, porta 34894 allocata a
//! quella label, HTTP 200) e il pannello ha detto `inactive (dead)` lo stesso.
//! La prova per porta non era stata nemmeno interrogata: 39 secondi dopo il kill
//! il `task_watchdog` aveva marcato la riga `failed` guardando il SOLO pid — con
//! `process_alive` grezzo, che non e' neppure il criterio sul processo — e la
//! riconciliazione del pannello interroga il criterio solo sulle righe ancora
//! `running`/`starting`. Il verso e' quello giusto (una riga gia' morta non
//! resuscita); era la SCRITTURA a essere arrivata da un punto che non poneva la
//! domanda. Sui tre progetti vivi, 17 righe `failed` su 39 hanno `exit_code`
//! NULL: nessuna uscita osservata, un verdetto dato sul solo pid.

use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use crate::process_liveness::{CausaMorte, StatoProcesso};

/// Da quale FATTO nasce il verdetto di vita. Serve a dichiarare la premessa nei
/// log (regola O: un verdetto senza la sua fonte e' un'opinione) e a distinguere
/// i due casi per chi ha bisogno di un pid su cui agire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProvaDiVita {
    /// Il pid registrato esiste ed e' il nostro.
    PidRegistrato { pid: u32 },
    /// Una porta allocata a questo servizio ha un listener: il server e' un
    /// discendente sopravvissuto al capostipite registrato.
    PortaAllocataInAscolto { porta: u16, pid: u32 },
}

/// Cosa si e' osservato sulle porte ALLOCATE al servizio. Quattro risposte: le
/// due che non sono ne' «ascolta» ne' «silenzio» hanno conseguenze opposte fra
/// loro, e un `Option` le confonderebbe. «Non ha porte» e' una risposta piena
/// (non esiste una seconda prova da attendersi); «non ho letto i listener» e' il
/// contrario (la prova c'era e non e' stata raccolta).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AscoltoPorte {
    Ascolta {
        porta: u16,
        pid: u32,
    },
    /// Le porte allocate ci sono, e nessuno ci ascolta.
    Silenzio,
    /// Il servizio non ha porte allocate: nessuna seconda prova e' possibile, e
    /// il verdetto sul pid resta l'unico che si puo' dare.
    NessunaPortaAllocata,
    /// L'elenco dei listener non e' stato letto (scan non interrogabile): la
    /// seconda prova era possibile ma manca.
    NonOsservato {
        motivo: String,
    },
}

/// Il verdetto sul SERVIZIO. Stesse tre facce del verdetto sul processo, e per
/// la stessa ragione: cio' che non si e' potuto accertare non deve diventare uno
/// stato scritto in DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatoServizio {
    Vivo(ProvaDiVita),
    Morto(CausaMorte),
    NonInterrogabile { motivo: String },
}

impl StatoServizio {
    pub(crate) fn e_vivo(&self) -> bool {
        matches!(self, StatoServizio::Vivo(_))
    }

    /// Come per il processo: solo una morte ACCERTATA autorizza a scriverla.
    pub(crate) fn autorizza_a_dichiararlo_morto(&self) -> bool {
        matches!(self, StatoServizio::Morto(_))
    }

    /// Il verdetto sul solo PROCESSO, portato nel vocabolario del servizio: le
    /// tre facce sono le stesse, e un chiamante che tratta righe di kind misto
    /// non deve avere due tipi per la stessa decisione. Il pid lo rimette chi
    /// chiama, perche' `StatoProcesso::Vivo` non lo porta con se'.
    fn dal_processo(stato: StatoProcesso, pid: Option<i32>) -> Self {
        match stato {
            StatoProcesso::Vivo => StatoServizio::Vivo(ProvaDiVita::PidRegistrato {
                pid: pid.unwrap_or(0).max(0) as u32,
            }),
            StatoProcesso::Morto(causa) => StatoServizio::Morto(causa),
            StatoProcesso::NonInterrogabile(motivo) => StatoServizio::NonInterrogabile {
                motivo: StatoProcesso::NonInterrogabile(motivo).descrizione(),
            },
        }
    }

    pub(crate) fn descrizione(&self) -> String {
        match self {
            StatoServizio::Vivo(ProvaDiVita::PidRegistrato { pid }) => {
                format!("vivo: pid registrato {pid}")
            }
            StatoServizio::Vivo(ProvaDiVita::PortaAllocataInAscolto { porta, pid }) => format!(
                "vivo: la porta allocata {porta} e' in ascolto (pid {pid}); il pid registrato \
                 non c'e' piu', ma era la shell, non il server"
            ),
            StatoServizio::Morto(causa) => StatoProcesso::Morto(causa.clone()).descrizione(),
            StatoServizio::NonInterrogabile { motivo } => motivo.clone(),
        }
    }
}

/// Il CRITERIO, puro: date le due osservazioni, il servizio e' vivo?
pub(crate) fn classifica_servizio(
    stato_pid: StatoProcesso,
    ascolto: AscoltoPorte,
) -> StatoServizio {
    // Prima prova: il processo registrato. Se e' il nostro ed e' vivo non serve
    // altro, e il pid resta disponibile a chi deve agirci sopra.
    if let StatoProcesso::Vivo = stato_pid {
        // `stato_pid` non porta con se' il pid: lo rimette il chiamante tramite
        // `valuta`, che e' l'unico a conoscerlo. Qui la variante senza pid non e'
        // rappresentabile, quindi la costruisce `valuta`.
        return StatoServizio::Vivo(ProvaDiVita::PidRegistrato { pid: 0 });
    }

    // Seconda prova: un listener su una porta allocata a QUESTO servizio. Vale
    // sia quando il pid e' morto (il capostipite se n'e' andato, il server no)
    // sia quando non era interrogabile.
    if let AscoltoPorte::Ascolta { porta, pid } = ascolto {
        return StatoServizio::Vivo(ProvaDiVita::PortaAllocataInAscolto { porta, pid });
    }

    match stato_pid {
        StatoProcesso::Vivo => unreachable!("gestito sopra"),
        StatoProcesso::NonInterrogabile(motivo) => StatoServizio::NonInterrogabile {
            motivo: StatoProcesso::NonInterrogabile(motivo).descrizione(),
        },
        StatoProcesso::Morto(causa) => match ascolto {
            // Silenzio su porte che ci sono, o nessuna porta da interrogare: il
            // verdetto sul pid e' l'unico disponibile ed e' una morte accertata.
            AscoltoPorte::Silenzio | AscoltoPorte::NessunaPortaAllocata => {
                StatoServizio::Morto(causa)
            }
            // La seconda prova esisteva e non e' stata raccolta: il pid morto da
            // solo non basta piu' a dichiarare morto il servizio, perche' e'
            // esattamente il caso in cui sbagliava.
            AscoltoPorte::NonOsservato { motivo } => StatoServizio::NonInterrogabile {
                motivo: format!(
                    "pid registrato morto, ma i listener non sono stati letti ({motivo}): \
                     il server puo' essere un discendente ancora vivo"
                ),
            },
            AscoltoPorte::Ascolta { .. } => unreachable!("gestito sopra"),
        },
    }
}

/// La domanda completa su una riga di servizio: compone le due prove e rimette
/// il pid registrato nella variante che lo dichiara.
///
/// `porte_allocate` sono le porte di QUESTO servizio (label esatta: l'allocazione
/// e' chiavata su `(project_id, label)`); `listener` e' la fotografia
/// `(porta, pid)` dei processi in ascolto, oppure il motivo per cui non e' stata
/// letta.
pub(crate) fn valuta(
    pid: Option<i32>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    porte_allocate: &[u16],
    listener: Result<&[(u16, u32)], String>,
) -> StatoServizio {
    let stato_pid = crate::process_liveness::stato_da_riga(pid, started_at);
    let ascolto = osserva_porte(porte_allocate, listener);
    match classifica_servizio(stato_pid, ascolto) {
        StatoServizio::Vivo(ProvaDiVita::PidRegistrato { .. }) => {
            StatoServizio::Vivo(ProvaDiVita::PidRegistrato {
                pid: pid.unwrap_or(0).max(0) as u32,
            })
        }
        altro => altro,
    }
}

/// Traduce la fotografia dei listener nella risposta sulle porte del servizio.
fn osserva_porte(porte_allocate: &[u16], listener: Result<&[(u16, u32)], String>) -> AscoltoPorte {
    if porte_allocate.is_empty() {
        return AscoltoPorte::NessunaPortaAllocata;
    }
    let listener = match listener {
        Ok(l) => l,
        Err(motivo) => return AscoltoPorte::NonOsservato { motivo },
    };
    for (porta, pid) in listener {
        if porte_allocate.contains(porta) {
            return AscoltoPorte::Ascolta {
                porta: *porta,
                pid: *pid,
            };
        }
    }
    AscoltoPorte::Silenzio
}

/// Le due prove raccolte UNA volta per l'intero progetto, nella forma in cui il
/// criterio le consuma.
///
/// Esiste perche' i consumatori sono QUATTRO — pannello Servizi, observer dei
/// Problemi, watchdog, boot-recovery — e la raccolta e' la meta' della domanda
/// che si sbaglia per omissione: chi non sa di dover leggere le porte non
/// interroga la seconda prova e ricade sul solo pid, che e' il difetto che
/// questo modulo esiste per chiudere. Con un raccoglitore unico non c'e' piu'
/// una versione della domanda che si possa porre a meta'.
///
/// La fotografia e' istantanea e NON si condivide fra un consumatore e l'altro:
/// ciascuno chiama [`ProveDiVita::del_progetto`] per il proprio giro (una
/// syscall, ~21ms), perche' decidere su listener osservati in un altro momento
/// significherebbe dare per vivo cio' che nel frattempo e' morto.
pub(crate) struct ProveDiVita {
    /// Porte registrate a ciascuna label di questo progetto.
    porte_per_label: HashMap<String, Vec<u16>>,
    /// Chi ascolta su cosa, oppure il motivo per cui non lo sappiamo.
    listener: Result<Vec<(u16, u32)>, String>,
}

impl ProveDiVita {
    pub(crate) async fn del_progetto(db: &PgPool, project_id: Uuid) -> Self {
        Self {
            porte_per_label: porte_allocate_per_label(db, project_id).await,
            listener: osserva_listener().await,
        }
    }

    /// Il verdetto su una riga di servizio, con le prove gia' in mano.
    pub(crate) fn verdetto(
        &self,
        label: &str,
        pid: Option<i32>,
        started_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> StatoServizio {
        valuta(
            pid,
            started_at,
            self.porte_per_label
                .get(label)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            self.listener.as_deref().map_err(String::clone),
        )
    }
}

/// Il verdetto su una riga di `agent_processes`, qualunque sia il suo `kind`.
///
/// PUNTO UNICO della scelta fra le due domande: per un SERVIZIO vale quella del
/// servizio (il pid registrato e' la shell), per tutto il resto quella del
/// PROCESSO. La scelta si fa qui e non nei chiamanti perche' sono tre — il
/// boot-recovery, i canali della Console e il watchdog — e prima ognuno la
/// ricopiava: e' cosi' che il watchdog era rimasto indietro con `process_alive`.
///
/// `prove` e' `None` quando il chiamante non ha (o non ha voluto raccogliere) le
/// prove del servizio: allora anche una riga `service` ricade sul solo processo,
/// che e' il comportamento vecchio — mai un verdetto inventato.
pub(crate) fn verdetto_di_riga(
    kind: &str,
    label: &str,
    pid: Option<i32>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    prove: Option<&ProveDiVita>,
) -> StatoServizio {
    match (kind, prove) {
        ("service", Some(prove)) => prove.verdetto(label, pid, started_at),
        _ => StatoServizio::dal_processo(
            crate::process_liveness::stato_da_riga(pid, started_at),
            pid,
        ),
    }
}

/// Porte registrate a ciascuna label di questo progetto, dal META
/// (`nexus_port_allocations`). La chiave e' la label ESATTA perche' l'allocazione
/// e' chiavata su `(project_id, label)`: e' quella corrispondenza a rendere il
/// listener una prova di vita di QUEL servizio. Il vocabolario largo di
/// `similar_service_labels` risponde a un'altra domanda («due label indicano lo
/// stesso RUOLO?») e qui attribuirebbe a un servizio la porta di un altro.
///
/// Un DB che non risponde da' una mappa vuota, non un errore: il chiamante
/// degrada a `NessunaPortaAllocata`, cioe' al verdetto sul solo pid — che e'
/// esattamente il comportamento di prima, quindi non peggiora nulla.
async fn porte_allocate_per_label(db: &PgPool, project_id: Uuid) -> HashMap<String, Vec<u16>> {
    let righe: Vec<(String, i32)> = sqlx::query_as(
        "SELECT label, port FROM nexus_port_allocations WHERE project_id = $1 AND label <> ''",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    let mut mappa: HashMap<String, Vec<u16>> = HashMap::new();
    for (label, port) in righe {
        if let Ok(p) = u16::try_from(port) {
            mappa.entry(label).or_default().push(p);
        }
    }
    mappa
}

/// La fotografia dei listener nella forma che [`valuta`] si aspetta: chi
/// ascolta su cosa, oppure il motivo per cui non lo sappiamo.
///
/// Sta qui e non nei chiamanti perche' i chiamanti sono piu' d'uno e devono
/// vedere lo stesso sistema operativo: se uno di loro traducesse un elenco non
/// letto in una lista vuota, due pannelli darebbero verdetti opposti sullo
/// stesso servizio, che e' il difetto da cui nasce questo modulo.
pub(crate) async fn osserva_listener() -> Result<Vec<(u16, u32)>, String> {
    match super::port_recovery::scan_listening_ports().await {
        super::port_recovery::ListenerScan::Osservati(v) => {
            Ok(v.into_iter().map(|(porta, pid, _)| (porta, pid)).collect())
        }
        // Un elenco NON LETTO non e' «nessuno ascolta»: la seconda prova era
        // possibile e manca, e a valle questo impedisce di dichiarare morto un
        // servizio il cui server potrebbe essere vivo.
        super::port_recovery::ListenerScan::NonInterrogabile { motivo } => {
            Err(format!("elenco dei listener non letto: {motivo}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_liveness::MotivoIgnoto;

    const VIVO: StatoProcesso = StatoProcesso::Vivo;

    fn morto() -> StatoProcesso {
        StatoProcesso::Morto(CausaMorte::PidAssente)
    }

    fn ignoto() -> StatoProcesso {
        StatoProcesso::NonInterrogabile(MotivoIgnoto::AvvioRealeNonLeggibile)
    }

    /// IL DIFETTO MISURATO su gestione-corsi: il pid registrato e' morto DAVVERO
    /// (era la shell), e il servizio gira lo stesso perche' il server e' un suo
    /// discendente, in ascolto sulla porta allocata a quella label.
    ///
    /// MUTAZIONE: togliere la seconda prova — cioe' ritornare `Morto` appena il
    /// pid e' morto, che e' cio' che il pannello faceva — fa fallire questo test
    /// con i numeri reali del 07/08/2026: pid registrato 20728 inesistente,
    /// porta 34859 in ascolto dal pid 3860.
    #[test]
    fn il_capostipite_morto_non_uccide_il_servizio_che_ascolta() {
        let stato = classifica_servizio(
            morto(),
            AscoltoPorte::Ascolta {
                porta: 34859,
                pid: 3860,
            },
        );
        assert_eq!(
            stato,
            StatoServizio::Vivo(ProvaDiVita::PortaAllocataInAscolto {
                porta: 34859,
                pid: 3860
            })
        );
        assert!(stato.e_vivo());
        assert!(
            !stato.autorizza_a_dichiararlo_morto(),
            "un servizio in ascolto sulla propria porta stava per essere scritto come stopped"
        );
        assert!(stato.descrizione().contains("34859"));
    }

    /// Il pid vivo basta da solo: la seconda prova non e' un obbligo, e un
    /// servizio senza porte (un worker) non diventa per questo ingiudicabile.
    ///
    /// Attraversa `valuta`, cioe' la funzione che la produzione chiama, con il
    /// processo di TEST come soggetto: e' il solo pid di cui questo test conosca
    /// con certezza esistenza e istante d'avvio. Un pid inventato darebbe un
    /// verdetto che dipende da cosa gira sulla macchina.
    #[test]
    fn il_pid_vivo_basta_da_solo() {
        let me = std::process::id();
        let avvio = crate::process_util::process_start_unix(me)
            .expect("istante d'avvio del processo di test leggibile");
        let atteso = chrono::DateTime::from_timestamp(avvio, 0).expect("timestamp valido");

        // Nessuna porta allocata e listener mai osservati: la seconda prova non
        // e' nemmeno possibile, e la prima basta.
        let stato = valuta(
            Some(me as i32),
            Some(atteso),
            &[],
            Err("mai osservati".to_string()),
        );
        assert_eq!(
            stato,
            StatoServizio::Vivo(ProvaDiVita::PidRegistrato { pid: me })
        );

        // Lo stesso pid senza attesa registrata: esiste, ma nulla lo lega a
        // questo servizio. Non e' vivo, e soprattutto non e' morto.
        let stato = valuta(Some(me as i32), None, &[], Err("mai osservati".to_string()));
        assert!(!stato.e_vivo());
        assert!(
            !stato.autorizza_a_dichiararlo_morto(),
            "{}",
            stato.descrizione()
        );

        // E la parte pura: `Vivo` non ha bisogno delle porte per concludere.
        assert!(matches!(
            classifica_servizio(VIVO, AscoltoPorte::NessunaPortaAllocata),
            StatoServizio::Vivo(ProvaDiVita::PidRegistrato { .. })
        ));
    }

    /// Silenzio sulle porte allocate e pid morto: morte accertata, si puo'
    /// scrivere. Senza questo ramo la riconciliazione non correggerebbe piu'
    /// nessuna riga stantia, cioe' il difetto opposto.
    #[test]
    fn pid_morto_e_porte_mute_e_una_morte_accertata() {
        for ascolto in [AscoltoPorte::Silenzio, AscoltoPorte::NessunaPortaAllocata] {
            let stato = classifica_servizio(morto(), ascolto);
            assert_eq!(stato, StatoServizio::Morto(CausaMorte::PidAssente));
            assert!(stato.autorizza_a_dichiararlo_morto());
        }
    }

    /// Se i listener non sono stati letti, la seconda prova ESISTEVA e manca: il
    /// pid morto da solo non autorizza piu' a dichiarare morto il servizio,
    /// perche' e' il caso in cui sbagliava.
    #[test]
    fn senza_la_fotografia_dei_listener_non_si_dichiara_morto() {
        let stato = classifica_servizio(
            morto(),
            AscoltoPorte::NonOsservato {
                motivo: "tabella IPv6 non letta".to_string(),
            },
        );
        assert!(!stato.autorizza_a_dichiararlo_morto());
        assert!(!stato.e_vivo());
        assert!(stato.descrizione().contains("IPv6"));
    }

    /// Pid non interrogabile ma porta in ascolto: il servizio e' vivo lo stesso.
    /// E' il caso dei processi elevati, dove il SO non risponde sul pid.
    #[test]
    fn la_porta_risolve_anche_il_pid_non_interrogabile() {
        let stato = classifica_servizio(
            ignoto(),
            AscoltoPorte::Ascolta {
                porta: 34894,
                pid: 999,
            },
        );
        assert!(stato.e_vivo());

        // Senza la porta resta ignoto, non morto.
        let stato = classifica_servizio(ignoto(), AscoltoPorte::Silenzio);
        assert!(!stato.autorizza_a_dichiararlo_morto());
        assert!(!stato.e_vivo());
    }

    /// IL CASO REALE, riprodotto a comando invece che raccontato: una shell
    /// avviata come la produzione la avvia ([`nexus_tool_kit::sandbox::agent_shell`],
    /// lo STESSO punto unico usato da `spawn_agent_process`) lancia un server e
    /// muore, lasciando in piedi il proprio discendente in ascolto.
    ///
    /// Il test attraversa i produttori veri (regola O): il pid morto lo dichiara
    /// il SISTEMA OPERATIVO tramite `stato_da_riga`, i listener li legge
    /// `osserva_listener` dalla tabella TCP del kernel. Nessun fatto e' costruito
    /// a mano — se lo fossero, fisserebbero come premessa proprio cio' che il fix
    /// deve dimostrare, e il test resterebbe verde anche col criterio rotto.
    ///
    /// MUTAZIONE: togliere la seconda prova (ritornare `Morto` appena il pid e'
    /// morto, che e' cio' che `task_watchdog` faceva) fa fallire l'ultimo assert,
    /// quello sulla CONSEGUENZA — «si puo' scrivere failed» — non sulla forma del
    /// valore. E' la scrittura il danno: il pannello mostra morto un servizio che
    /// risponde, e l'observer gli apre una diagnosi di crash.
    #[tokio::test]
    async fn il_servizio_sopravvive_alla_shell_che_lo_ha_avviato() {
        // Il discendente e' un `node`: il repo lo esige gia' (pnpm), quindi c'e'
        // sia in locale sia in CI. Dove non c'e', il test DICHIARA di non aver
        // misurato invece di risultare verde per assenza.
        if tokio::process::Command::new("node")
            .arg("--version")
            .output()
            .await
            .is_err()
        {
            eprintln!("SALTATO: node non disponibile, il discendente non e' avviabile");
            return;
        }

        // Il server sceglie la porta (`listen(0)`) e la stampa: senza questo il
        // test dovrebbe indovinarne una libera e potrebbe collidere. La shell
        // esce SUBITO (`&`), la pipe resta aperta dal figlio che le sopravvive —
        // che e' esattamente la forma del difetto.
        let script = "const s=require('net').createServer();\
                      s.listen(0,'127.0.0.1',()=>console.log('PORTA='+s.address().port));\
                      setTimeout(()=>process.exit(0),30000)";
        let mut shell = tokio::process::Command::new(nexus_tool_kit::sandbox::agent_shell())
            .args(["-c", &format!("node -e \"{script}\" &")])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("avvio della shell");
        // Cio' che la produzione registra: il pid della SHELL e l'istante in cui
        // lo annota (`started_at = NOW()` subito dopo lo spawn).
        let pid_shell = shell.id().expect("pid della shell");
        let avvio = chrono::Utc::now();
        let stdout = shell.stdout.take().expect("pipe della shell");

        let porta = {
            use tokio::io::AsyncBufReadExt;
            let mut righe = tokio::io::BufReader::new(stdout).lines();
            let letta = tokio::time::timeout(std::time::Duration::from_secs(20), async {
                while let Ok(Some(riga)) = righe.next_line().await {
                    if let Some(p) = riga.trim().strip_prefix("PORTA=") {
                        return p.parse::<u16>().ok();
                    }
                }
                None
            })
            .await;
            match letta {
                Ok(Some(p)) => p,
                _ => {
                    let _ = shell.kill().await;
                    panic!("il discendente non ha dichiarato la propria porta");
                }
            }
        };
        // La shell e' uscita da sola: e' il capostipite che se ne va per primo.
        let _ = shell.wait().await;

        // Premessa del test, verificata e non assunta: il pid REGISTRATO e' morto
        // davvero. Senza questo controllo il verde direbbe soltanto che qualcosa
        // ascolta, non che la seconda prova serviva.
        let sul_processo = crate::process_liveness::stato_da_riga(Some(pid_shell as i32), Some(avvio));
        assert!(
            sul_processo.autorizza_a_dichiararlo_morto(),
            "premessa non riprodotta, la shell e' ancora viva: {}",
            sul_processo.descrizione()
        );

        let listener = osserva_listener().await;
        let stato = valuta(
            Some(pid_shell as i32),
            Some(avvio),
            &[porta],
            listener.as_deref().map_err(String::clone),
        );

        let StatoServizio::Vivo(ProvaDiVita::PortaAllocataInAscolto { porta: p, pid }) = &stato
        else {
            panic!("servizio dichiarato non vivo mentre il suo server ascolta: {stato:?}");
        };
        assert_eq!(*p, porta);
        assert_ne!(
            *pid, pid_shell,
            "il pid riportato dev'essere quello del LISTENER: il registrato non c'e' piu'"
        );
        assert!(
            !stato.autorizza_a_dichiararlo_morto(),
            "un servizio che risponde stava per essere scritto 'failed' in DB"
        );

        crate::process_util::kill_pid(*pid).await;
    }

    /// L'osservazione delle porte distingue i quattro casi. In particolare
    /// «nessuna porta allocata» non e' «non ho guardato»: il primo lascia
    /// decidere al pid, il secondo lo sospende.
    #[test]
    fn losservazione_delle_porte_distingue_i_quattro_casi() {
        assert_eq!(
            osserva_porte(&[], Ok(&[(34859, 3860)])),
            AscoltoPorte::NessunaPortaAllocata
        );
        assert_eq!(
            osserva_porte(&[34859], Ok(&[(34859, 3860)])),
            AscoltoPorte::Ascolta {
                porta: 34859,
                pid: 3860
            }
        );
        assert_eq!(
            osserva_porte(&[34859], Ok(&[(1234, 9)])),
            AscoltoPorte::Silenzio
        );
        assert!(matches!(
            osserva_porte(&[34859], Err("scan fallito".to_string())),
            AscoltoPorte::NonOsservato { .. }
        ));
    }
}

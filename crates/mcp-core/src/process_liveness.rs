//! PUNTO UNICO (regola L) della domanda «questo processo registrato e' ancora
//! vivo?», posta a un PID che viene da un REGISTRO (`agent_processes.pid`, il
//! `nexus-dev.pids.json` degli script di deploy) e non dal sistema operativo un
//! istante fa.
//!
//! PERCHE' LA DOMANDA NON E' «esiste un processo con questo pid?». Un pid nudo
//! non e' un'identita': Windows li ricicla in modo aggressivo, e un pid scritto
//! in un registro non sa nulla di cio' che e' successo dopo. La domanda completa
//! e' «questo pid esiste ancora, ED E' il processo che credo?», e la risposta
//! vuole DUE fatti dal SO: l'esistenza e un discriminante che leghi il pid
//! all'identita' attesa — qui l'istante di AVVIO, confrontato con quello che il
//! registro ha annotato quando ha spawnato il processo.
//!
//! PERCHE' UN TIPO A TRE VARIANTI E NON UN `bool` (regola Q). Prima di questo
//! modulo la stessa domanda aveva risposte diverse in punti diversi, e ognuna
//! collassava a un booleano un caso che booleano non e':
//!
//! - `process_alive(pid)` da solo: dice VIVO su un pid RICICLATO, perche' non
//!   guarda l'identita'. E' la direzione in cui il registro dichiara vivo cio'
//!   che non c'e' piu'.
//! - `process_alive(pid) && pid_identity_confirmed(pid, started_at)`: dice MORTO
//!   quando l'identita' non e' CONFERMABILE — `started_at` non registrato,
//!   creation-time non leggibile perche' il processo appartiene a un altro
//!   utente o a una sessione elevata. E' la direzione opposta: un servizio che
//!   gira, dichiarato morto, e — nei consumatori che persistono — marcato
//!   `stopped`/`failed` in DB.
//!
//! Le due direzioni non sono due difetti: sono lo stesso difetto, cioe' un
//! canale a due valori per una domanda che ne ha tre. `NonInterrogabile` esiste
//! perche' «non ho potuto guardare» non degradi ne' a vivo ne' a morto, e i due
//! predicati che ne derivano — [`StatoProcesso::e_vivo`] e
//! [`StatoProcesso::autorizza_a_dichiararlo_morto`] — NON sono l'uno la
//! negazione dell'altro. Un `bool` non poteva esprimerlo, ed e' la ragione per
//! cui ogni consumatore ne sceglieva una a caso.
//!
//! PORTATA. Questo modulo risponde su UN PROCESSO. Un SERVIZIO non e' il suo
//! processo capostipite: `spawn_agent_process` registra il pid della shell, e il
//! server vero e' un discendente che le sopravvive. Quella e' una seconda
//! domanda, e ha un suo punto unico che delega a questo:
//! [`crate::project_workspace::service_liveness`].

/// Tolleranza (secondi) fra l'avvio REALE del processo e quello ATTESO dal
/// registro. Un pid riciclato ha creation-time arbitrario, tipicamente lontano;
/// lo scarto legittimo e' di frazioni di secondo, perche' il registro annota
/// l'attesa subito dopo lo spawn. Margine ampio abbastanza da non invalidare mai
/// un processo vero, stretto abbastanza da scartare un estraneo.
pub(crate) const TOLLERANZA_AVVIO_S: i64 = 10;

/// Perche' il processo NON c'e' piu'. Il consumatore che persiste uno stato ha
/// bisogno della causa, non del solo verdetto: «pid riciclato» e «uscito» si
/// scrivono uguali in DB ma si diagnosticano in modo opposto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CausaMorte {
    /// Nessun processo con quel pid: il SO lo dichiara inesistente.
    PidAssente,
    /// Il processo esiste come oggetto kernel ma e' gia' USCITO (un handle
    /// aperto altrove — tipicamente la shell padre — ne tiene in vita la
    /// struttura). Invisibile a `Get-Process`, ma morto.
    Uscito,
    /// Il pid esiste ed e' in esecuzione, ma NON e' il nostro processo: il SO lo
    /// ha riassegnato a un estraneo, e l'istante d'avvio lo dimostra.
    PidRiciclato {
        avvio_reale_unix: i64,
        avvio_atteso_unix: i64,
    },
}

/// Perche' non si e' potuto rispondere. Non e' un ripiego: e' cio' che
/// impedisce a un'ignoranza di diventare un verdetto persistito.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MotivoIgnoto {
    /// Il registro non ha annotato l'istante d'avvio (`started_at` NULL): esiste
    /// un processo con quel pid, ma nulla lo lega al nostro. Succede sulle righe
    /// nate `starting` (l'avvio si annota solo dopo lo spawn riuscito) e su
    /// quelle scritte da percorsi che adottano un processo preesistente.
    AvvioAttesoNonRegistrato,
    /// Il processo esiste ma il SO non ne dichiara l'istante d'avvio.
    AvvioRealeNonLeggibile,
    /// Il SO non ha voluto o potuto rispondere sull'esistenza stessa: tipicamente
    /// accesso negato (il processo appartiene a un altro utente, o gira elevato
    /// mentre noi no). `codice` e' l'errore del SO, per la diagnostica.
    EsistenzaNonInterrogabile { codice: u32 },
}

/// La risposta. Tre varianti perche' i casi sono tre (regola Q).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatoProcesso {
    /// Esiste, e' in esecuzione, ed e' il processo atteso.
    Vivo,
    Morto(CausaMorte),
    NonInterrogabile(MotivoIgnoto),
}

impl StatoProcesso {
    /// «Posso trattarlo come vivo?» Solo `Vivo`. Un processo la cui identita'
    /// non e' accertata non e' il nostro, e agire come se lo fosse e' il modo in
    /// cui una porta finisce attribuita al servizio sbagliato.
    pub(crate) fn e_vivo(&self) -> bool {
        matches!(self, StatoProcesso::Vivo)
    }

    /// «Posso SCRIVERE che e' morto?» Solo `Morto`. Non e' `!e_vivo()`: con
    /// `NonInterrogabile` non abbiamo osservato nulla, e persistere `stopped` o
    /// `failed` su una non-osservazione e' esattamente il modo in cui il pannello
    /// Servizi dichiarava `inattivo (dead)` un servizio che stava girando —
    /// scrivendolo poi in DB, cosi' che la prossima lettura confermasse l'errore.
    pub(crate) fn autorizza_a_dichiararlo_morto(&self) -> bool {
        matches!(self, StatoProcesso::Morto(_))
    }

    /// Motivo leggibile per log e diagnostica. Composto DAI campi, e mai riletto
    /// da codice (regola Q): chi decide guarda le varianti.
    pub(crate) fn descrizione(&self) -> String {
        match self {
            StatoProcesso::Vivo => "vivo (pid esistente, identita' confermata)".to_string(),
            StatoProcesso::Morto(CausaMorte::PidAssente) => "morto: pid inesistente".to_string(),
            StatoProcesso::Morto(CausaMorte::Uscito) => {
                "morto: processo uscito (handle ancora aperto)".to_string()
            }
            StatoProcesso::Morto(CausaMorte::PidRiciclato {
                avvio_reale_unix,
                avvio_atteso_unix,
            }) => format!(
                "morto: pid riciclato su un processo estraneo (avvio reale {avvio_reale_unix}, \
                 atteso {avvio_atteso_unix})"
            ),
            StatoProcesso::NonInterrogabile(MotivoIgnoto::AvvioAttesoNonRegistrato) => {
                "non interrogabile: il registro non ha annotato l'istante d'avvio".to_string()
            }
            StatoProcesso::NonInterrogabile(MotivoIgnoto::AvvioRealeNonLeggibile) => {
                "non interrogabile: il SO non dichiara l'istante d'avvio del processo".to_string()
            }
            StatoProcesso::NonInterrogabile(MotivoIgnoto::EsistenzaNonInterrogabile { codice }) => {
                format!("non interrogabile: il SO non risponde sull'esistenza (codice {codice})")
            }
        }
    }
}

/// Cosa il SO dice dell'ESISTENZA di un pid. Quattro risposte, non un `bool`:
/// «non esiste» e «non me lo lasci vedere» hanno conseguenze opposte, e
/// `OpenProcess` le riporta con lo stesso handle nullo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Esistenza {
    InEsecuzione,
    /// L'oggetto kernel c'e' ma il processo e' terminato.
    Uscito,
    Assente,
    /// Il SO ha rifiutato la domanda (accesso negato, ecc.).
    NonInterrogabile {
        codice: u32,
    },
}

/// I fatti raccolti dal SO su un pid. Separati dal criterio che li giudica
/// perche' il criterio si possa provare senza dover produrre a comando un pid
/// riciclato o un accesso negato (regola O): il test costruisce i fatti, la
/// produzione li misura, e a giudicarli e' la stessa funzione.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FattiProcesso {
    pub esistenza: Esistenza,
    /// Istante d'avvio reale (epoch unix), se il SO lo dichiara.
    pub avvio_reale_unix: Option<i64>,
}

/// Il CRITERIO, puro. `avvio_atteso_unix` e' cio' che il registro ha annotato:
/// `None` significa che il registro non lo sa, mai che vada bene qualunque
/// processo.
pub(crate) fn classifica(
    fatti: FattiProcesso,
    avvio_atteso_unix: Option<i64>,
    tolleranza_s: i64,
) -> StatoProcesso {
    match fatti.esistenza {
        Esistenza::Assente => StatoProcesso::Morto(CausaMorte::PidAssente),
        Esistenza::Uscito => StatoProcesso::Morto(CausaMorte::Uscito),
        Esistenza::NonInterrogabile { codice } => {
            StatoProcesso::NonInterrogabile(MotivoIgnoto::EsistenzaNonInterrogabile { codice })
        }
        Esistenza::InEsecuzione => {
            // Da qui in poi un processo con quel pid c'e' di sicuro. Resta la
            // seconda meta' della domanda: e' il NOSTRO?
            let Some(atteso) = avvio_atteso_unix else {
                return StatoProcesso::NonInterrogabile(MotivoIgnoto::AvvioAttesoNonRegistrato);
            };
            let Some(reale) = fatti.avvio_reale_unix else {
                return StatoProcesso::NonInterrogabile(MotivoIgnoto::AvvioRealeNonLeggibile);
            };
            if (reale - atteso).abs() <= tolleranza_s {
                StatoProcesso::Vivo
            } else {
                StatoProcesso::Morto(CausaMorte::PidRiciclato {
                    avvio_reale_unix: reale,
                    avvio_atteso_unix: atteso,
                })
            }
        }
    }
}

/// La domanda completa su un pid PERSISTITO: interroga il SO e giudica.
///
/// `avvio_atteso_unix` viene da `agent_processes.started_at` (o dall'equivalente
/// del registro che ha scritto quel pid). Un pid <= 0 non e' un pid.
///
/// VINCOLO TIMEBASE: l'attesa nasce da `NOW()` del server Postgres, l'avvio
/// reale dal clock dell'host. Nell'ambiente canonico Nexus i Postgres sono
/// NATIVI Windows (stesso clock host, scarto misurato < 1s). Se il DB girasse
/// con un orologio derivante rispetto all'host, la tolleranza fissa non
/// basterebbe e l'attesa andrebbe ancorata a un dato host-side registrato allo
/// spawn — non a un allargamento della tolleranza, che renderebbe di nuovo
/// invisibile il riciclo.
pub(crate) fn stato_del_pid(pid: u32, avvio_atteso_unix: Option<i64>) -> StatoProcesso {
    if pid == 0 {
        return StatoProcesso::Morto(CausaMorte::PidAssente);
    }
    let fatti = FattiProcesso {
        esistenza: crate::process_util::esistenza_processo(pid),
        avvio_reale_unix: crate::process_util::process_start_unix(pid),
    };
    classifica(fatti, avvio_atteso_unix, TOLLERANZA_AVVIO_S)
}

/// Variante per i pid letti da `agent_processes`, dove il tipo in colonna e'
/// `Option<i32>` e l'avvio e' un timestamp. Esiste perche' la conversione non
/// venga ricopiata a ogni call site — e con essa la scelta di cosa fare di un
/// pid NULL, che e' parte del criterio: una riga senza pid non e' un processo
/// non interrogabile, e' una riga che non ne ha mai avuto uno.
pub(crate) fn stato_da_riga(
    pid: Option<i32>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
) -> StatoProcesso {
    match pid.filter(|p| *p > 0) {
        Some(p) => stato_del_pid(p as u32, started_at.map(|t| t.timestamp())),
        None => StatoProcesso::Morto(CausaMorte::PidAssente),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fatti(esistenza: Esistenza, avvio: Option<i64>) -> FattiProcesso {
        FattiProcesso {
            esistenza,
            avvio_reale_unix: avvio,
        }
    }

    /// L'identita' combacia entro tolleranza: e' il nostro processo.
    #[test]
    fn avvio_combaciante_e_vivo() {
        let s = classifica(
            fatti(Esistenza::InEsecuzione, Some(1_000_002)),
            Some(1_000_000),
            TOLLERANZA_AVVIO_S,
        );
        assert_eq!(s, StatoProcesso::Vivo);
        assert!(s.e_vivo());
        assert!(!s.autorizza_a_dichiararlo_morto());
    }

    /// IL DIFETTO, DIREZIONE «MORTI DICHIARATI VIVI»: un pid in esecuzione ma con
    /// avvio lontano dall'atteso e' un ESTRANEO, non il nostro servizio.
    ///
    /// MUTAZIONE: togliere il confronto e ritornare `Vivo` per ogni pid in
    /// esecuzione (cioe' tornare a `process_alive` da solo) fa fallire questo
    /// test. E' la forma con cui il registro dichiarava attivi i nove processi
    /// dello stack dev gia' morti: i pid c'erano ancora nel file, e a nessuno era
    /// stato chiesto se fossero ancora gli stessi.
    #[test]
    fn pid_riciclato_non_e_vivo() {
        let s = classifica(
            fatti(Esistenza::InEsecuzione, Some(1_050_000)),
            Some(1_000_000),
            TOLLERANZA_AVVIO_S,
        );
        assert_eq!(
            s,
            StatoProcesso::Morto(CausaMorte::PidRiciclato {
                avvio_reale_unix: 1_050_000,
                avvio_atteso_unix: 1_000_000,
            })
        );
        assert!(!s.e_vivo());
        // Un riciclo E' un'osservazione: il nostro processo non c'e' piu'.
        assert!(s.autorizza_a_dichiararlo_morto());
    }

    /// IL DIFETTO, DIREZIONE «VIVI DICHIARATI MORTI»: l'identita' non
    /// confermabile non e' una morte. Prima `pid_identity_confirmed` ritornava
    /// `false` in entrambi questi casi, e i consumatori che persistono lo
    /// scrivevano in DB come `stopped`.
    ///
    /// MUTAZIONE: far degradare i due rami a `Morto(PidAssente)` — che e' cio'
    /// che faceva il booleano — fa fallire l'assert su
    /// `autorizza_a_dichiararlo_morto`, cioe' esattamente la conseguenza (la
    /// scrittura in DB), non la forma del valore.
    #[test]
    fn identita_non_confermabile_non_e_una_morte() {
        let senza_attesa = classifica(
            fatti(Esistenza::InEsecuzione, Some(1_000_000)),
            None,
            TOLLERANZA_AVVIO_S,
        );
        assert_eq!(
            senza_attesa,
            StatoProcesso::NonInterrogabile(MotivoIgnoto::AvvioAttesoNonRegistrato)
        );

        let senza_reale = classifica(
            fatti(Esistenza::InEsecuzione, None),
            Some(1_000_000),
            TOLLERANZA_AVVIO_S,
        );
        assert_eq!(
            senza_reale,
            StatoProcesso::NonInterrogabile(MotivoIgnoto::AvvioRealeNonLeggibile)
        );

        for s in [senza_attesa, senza_reale] {
            assert!(!s.e_vivo(), "l'ignoto non e' un permesso ad agire");
            assert!(
                !s.autorizza_a_dichiararlo_morto(),
                "l'ignoto non autorizza a scrivere che il processo e' morto: \
                 {}",
                s.descrizione()
            );
        }
    }

    /// Il SO che rifiuta la domanda (accesso negato: processo di un altro utente
    /// o sessione elevata) non dice «non esiste». Con `OpenProcess` entrambi
    /// danno handle nullo, ed e' per questo che l'esistenza e' un enum e non un
    /// bool.
    #[test]
    fn accesso_negato_non_e_assenza() {
        let s = classifica(
            fatti(Esistenza::NonInterrogabile { codice: 5 }, None),
            Some(1_000_000),
            TOLLERANZA_AVVIO_S,
        );
        assert_eq!(
            s,
            StatoProcesso::NonInterrogabile(MotivoIgnoto::EsistenzaNonInterrogabile { codice: 5 })
        );
        assert!(!s.autorizza_a_dichiararlo_morto());
        assert!(s.descrizione().contains('5'), "il codice del SO va nel log");
    }

    /// Assenza e uscita sono morti accertate: si possono persistere. La causa
    /// resta distinta perche' si diagnosticano in modo opposto.
    #[test]
    fn assente_e_uscito_sono_morti_accertate() {
        for (esistenza, attesa) in [
            (Esistenza::Assente, CausaMorte::PidAssente),
            (Esistenza::Uscito, CausaMorte::Uscito),
        ] {
            let s = classifica(fatti(esistenza, None), Some(1_000_000), TOLLERANZA_AVVIO_S);
            assert_eq!(s, StatoProcesso::Morto(attesa));
            assert!(s.autorizza_a_dichiararlo_morto());
        }
    }

    /// La morte accertata NON dipende dall'attesa: un pid assente e' assente
    /// anche se il registro non ha annotato l'avvio. Senza questo ordine dei
    /// rami, una riga `starting` (avvio non ancora annotato) con pid gia' morto
    /// resterebbe per sempre «non interrogabile», cioe' per sempre running.
    #[test]
    fn il_pid_assente_non_diventa_ignoto_per_mancanza_di_attesa() {
        let s = classifica(fatti(Esistenza::Assente, None), None, TOLLERANZA_AVVIO_S);
        assert!(s.autorizza_a_dichiararlo_morto());
    }

    /// Il processo di test e' vivo e il suo avvio reale e' leggibile: passandolo
    /// come atteso, il criterio deve confermarlo. Attraversa la funzione che la
    /// produzione usa (regola O), non solo la parte pura.
    #[test]
    fn il_processo_corrente_e_vivo_con_il_proprio_avvio() {
        let me = std::process::id();
        let reale = crate::process_util::process_start_unix(me)
            .expect("l'avvio del processo corrente e' leggibile");
        assert_eq!(stato_del_pid(me, Some(reale)), StatoProcesso::Vivo);
        // Lo stesso pid con un'attesa lontana e' un riciclo: e' il caso che
        // distingue questa funzione da `process_alive`.
        assert!(matches!(
            stato_del_pid(me, Some(reale - 3600)),
            StatoProcesso::Morto(CausaMorte::PidRiciclato { .. })
        ));
        // E senza attesa non si puo' dire nulla: non e' una morte.
        assert!(!stato_del_pid(me, None).autorizza_a_dichiararlo_morto());
    }

    #[test]
    fn un_pid_assurdo_e_morto() {
        let s = stato_del_pid(u32::MAX, Some(1_000_000));
        assert!(s.autorizza_a_dichiararlo_morto(), "{}", s.descrizione());
    }

    /// Una riga senza pid non ha mai avuto un processo: e' una morte accertata,
    /// non un'ignoranza. Il contrario lascerebbe eternamente `running` le righe
    /// il cui spawn non e' mai riuscito.
    #[test]
    fn riga_senza_pid_e_morte_accertata() {
        assert_eq!(
            stato_da_riga(None, None),
            StatoProcesso::Morto(CausaMorte::PidAssente)
        );
        assert_eq!(
            stato_da_riga(Some(0), None),
            StatoProcesso::Morto(CausaMorte::PidAssente)
        );
        assert_eq!(
            stato_da_riga(Some(-1), None),
            StatoProcesso::Morto(CausaMorte::PidAssente)
        );
    }
}

//! «Questo comando in esecuzione E' un server?» — la risposta OSSERVATA, non
//! indovinata dal suo nome (punto unico, regola L).
//!
//! # Il difetto che lo motiva
//!
//! Il probe di `run_command` decideva con `is_long_oneshot`, una lista di
//! sottostringhe: `install`, `npm ci`, `" build"`, `tsc`, `cargo build`,
//! `compile`, `migrate`, `prisma generate`, `playwright test`, `npm add`...
//! Copre Node e Rust. Non contiene `create-next-app`, `dotnet new`,
//! `dotnet restore`, `git clone`, `mvn`, `gradle` — e non potrebbe contenerli
//! tutti, perche' la lista cresce di una voce per ogni strumento che il mondo
//! inventa.
//!
//! MISURATO il 06/08/2026 sul progetto gestione-corsi, primo tentativo con uno
//! stack mai provato (Next.js + ASP.NET Core). `npx create-next-app` impiega
//! piu' di 10 secondi perche' scarica pacchetti; non essendo nella lista, allo
//! scadere del probe e' stato classificato «long-running» e RILANCIATO come
//! servizio. Il rilancio riparte da capo, ma la prima esecuzione aveva gia'
//! scritto meta' dei file, e lo scaffolder al secondo giro si e' rifiutato:
//!
//!   The directory school-courses-fe contains files that could conflict:
//!     app/  eslint.config.mjs  next.config.ts  package.json  public/  ...
//!
//! Da li' la figura ha speso 26 comandi e i suoi 600 secondi tentando di
//! ripulire la directory (`rm -rf`, `rd /s /q`, `rmdir`, `del`, `mv`,
//! `cmd.exe /c rmdir`), mentre il gate sui passi critici — correttamente —
//! rifiutava i comandi distruttivi. Un'euristica sbagliata a t=10s ha prodotto
//! un run morto a t=600s.
//!
//! # Perche' non basta una lista migliore
//!
//! E' la stessa forma di difetto che [`super::avvio_server`] ha gia' corretto
//! una volta: li' il token nudo `vite` rendeva «servizio web» la stringa
//! `VITE_API_URL`, e un `grep` uccideva un dev-server vivo da 3h45m. La
//! correzione fu porre la domanda sull'ESEGUIBILE invece che sulla riga intera
//! — meglio, ma il vocabolario resta: un elenco di nomi noti che qualcuno deve
//! tenere aggiornato, e che sbaglia in silenzio su cio' che non conosce.
//!
//! Qui la domanda cambia natura. Un server non si riconosce da come si chiama:
//! si riconosce perche' **apre una porta e resta in ascolto**. E' un fatto del
//! sistema operativo, osservabile mentre il processo gira, e vero per qualunque
//! runtime — Node, .NET, Python, Go, un binario scritto stanotte. Un one-shot,
//! per quanto lungo, non ascolta niente: sta lavorando, e prima o poi termina.
//!
//! E' la regola M applicata al caso: lo stato tecnico si legge da un segnale
//! strutturato (la tabella dei listener del kernel), mai dal testo.
//!
//! # Cosa NON risolve
//!
//! Un servizio che non ascolta su TCP (un worker di coda, un batch daemon) qui
//! risulta `NonServe`: verrebbe atteso come un one-shot e, a tetto scaduto,
//! dichiarato fallito. E' un degrado accettabile e DICHIARATO — quel processo
//! non riceverebbe comunque una porta iniettata, che e' il servizio principale
//! reso dal ramo «server». Il caso non e' teorico ma e' raro, e il rimedio
//! (l'agente usa `run_service` esplicitamente) esiste gia'.

use crate::project_workspace::port_recovery::{scan_listening_ports, ListenerScan};

/// Cosa si e' potuto osservare del processo mentre girava.
///
/// Tre casi e non due (regola Q): «non ho potuto guardare» non e' «non e' un
/// server». Sulle piattaforme dove la tabella dei listener non e' interrogabile
/// il chiamante deve poter ripiegare sapendo di ripiegare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NaturaOsservata {
    /// Il processo, o un suo discendente, e' in ascolto su almeno una porta.
    Serve { porte: Vec<u16> },
    /// La tabella dei listener e' stata letta per intero e nessuno dell'albero
    /// vi compare: il processo sta lavorando, non servendo.
    NonServe,
    /// La domanda non ha avuto risposta.
    NonOsservabile { motivo: String },
}

impl NaturaOsservata {
    /// Come si racconta questo esito in un log.
    pub(crate) fn descrizione(&self) -> String {
        match self {
            Self::Serve { porte } => format!(
                "in ascolto su {}",
                porte
                    .iter()
                    .map(u16::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::NonServe => "nessuna porta in ascolto nell'albero".to_string(),
            Self::NonOsservabile { motivo } => format!("non osservabile: {motivo}"),
        }
    }
}

/// Il processo `pid`, o un suo discendente, sta ascoltando su una porta?
///
/// Si guarda l'ALBERO e non il solo pid perche' il processo lanciato e' quasi
/// sempre un tramite: `npm run dev` genera lo script, che genera il bundler,
/// che apre la porta. Chiedere del solo padre direbbe «non serve» di ogni
/// dev-server lanciato attraverso un gestore di pacchetti — cioe' di quasi
/// tutti.
///
/// La discendenza si risolve dallo STESSO snapshot che l'albero processi
/// espone (punto unico `windows_process_snapshot`), risalendo da ogni pid in
/// ascolto verso i suoi padri: e' piu' economico che espandere i figli, perche'
/// i listener sono pochi e l'albero e' grande. Il limite di risalita evita
/// cicli, che una tabella pid->padre corrotta potrebbe presentare.
pub(crate) async fn natura_osservata(pid: u32) -> NaturaOsservata {
    let listener = match scan_listening_ports().await {
        ListenerScan::Osservati(v) => v,
        ListenerScan::NonInterrogabile { motivo } => {
            return NaturaOsservata::NonOsservabile { motivo }
        }
    };
    if listener.is_empty() {
        return NaturaOsservata::NonServe;
    }

    let padri = match snapshot_padri() {
        Some(p) => p,
        None => {
            return NaturaOsservata::NonOsservabile {
                motivo: "albero dei processi non leggibile".to_string(),
            }
        }
    };

    let porte: Vec<u16> = listener
        .iter()
        .filter(|(_, listener_pid, _)| discende_da(*listener_pid, pid, &padri))
        .map(|(porta, _, _)| *porta)
        .collect();

    if porte.is_empty() {
        NaturaOsservata::NonServe
    } else {
        NaturaOsservata::Serve { porte }
    }
}

/// `figlio` e' `antenato`, o un suo discendente?
///
/// Puro e testabile senza sistema operativo: la mappa pid -> padre e' il solo
/// ingresso. Il tetto di risalita e' una difesa contro una mappa ciclica, non
/// un limite di profondita' realistico (gli alberi veri hanno pochi livelli).
fn discende_da(
    figlio: u32,
    antenato: u32,
    padri: &std::collections::HashMap<u32, u32>,
) -> bool {
    let mut corrente = figlio;
    for _ in 0..64 {
        if corrente == antenato {
            return true;
        }
        match padri.get(&corrente) {
            Some(&p) if p != corrente && p != 0 => corrente = p,
            _ => return false,
        }
    }
    false
}

#[cfg(windows)]
fn snapshot_padri() -> Option<std::collections::HashMap<u32, u32>> {
    let snap = crate::process_util::windows_process_snapshot();
    if snap.is_empty() {
        return None;
    }
    Some(snap.into_iter().map(|(pid, e)| (pid, e.parent_pid)).collect())
}

#[cfg(not(windows))]
fn snapshot_padri() -> Option<std::collections::HashMap<u32, u32>> {
    // Su questa piattaforma l'albero non e' esposto dal punto unico: si
    // DICHIARA invece di rispondere che nessuno discende da nessuno, il che
    // farebbe passare per one-shot ogni dev-server.
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn albero(coppie: &[(u32, u32)]) -> HashMap<u32, u32> {
        coppie.iter().copied().collect()
    }

    /// Il caso reale: `npm run dev` (100) genera lo script (200) che genera il
    /// bundler (300), ed e' il bundler ad aprire la porta. Chiedere del solo
    /// padre direbbe «non serve» di quasi ogni dev-server.
    #[test]
    fn un_nipote_in_ascolto_conta_come_albero_in_ascolto() {
        let padri = albero(&[(300, 200), (200, 100), (100, 1)]);
        assert!(discende_da(300, 100, &padri));
        assert!(discende_da(200, 100, &padri));
        assert!(discende_da(100, 100, &padri), "il pid stesso discende da se'");
    }

    /// Un processo di un altro ramo non deve rendere «server» il nostro
    /// comando: sarebbe il difetto di prima con un'altra faccia.
    #[test]
    fn un_estraneo_in_ascolto_non_ci_riguarda() {
        let padri = albero(&[(300, 200), (200, 100), (999, 1)]);
        assert!(!discende_da(999, 100, &padri));
    }

    /// Una mappa ciclica non deve bloccare la risalita.
    #[test]
    fn un_ciclo_non_manda_in_stallo() {
        let padri = albero(&[(10, 20), (20, 10)]);
        assert!(!discende_da(10, 999, &padri));
    }

    /// Radice raggiunta senza incontrare l'antenato: risposta negativa, non un
    /// giro infinito.
    #[test]
    fn la_radice_chiude_la_risalita() {
        let padri = albero(&[(5, 0)]);
        assert!(!discende_da(5, 42, &padri));
    }

    /// Il vocabolario dei tre esiti resta leggibile: e' cio' che finisce nei
    /// log quando qualcuno indaghera' un instradamento sbagliato.
    #[test]
    fn ogni_esito_si_racconta() {
        assert!(NaturaOsservata::Serve { porte: vec![3000] }
            .descrizione()
            .contains("3000"));
        assert_eq!(
            NaturaOsservata::NonServe.descrizione(),
            "nessuna porta in ascolto nell'albero"
        );
        assert!(NaturaOsservata::NonOsservabile {
            motivo: "syscall".into()
        }
        .descrizione()
        .contains("syscall"));
    }
}

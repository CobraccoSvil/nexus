//! PUNTO UNICO (regola L) della domanda: **un run e' morto per tempo scaduto —
//! su che cosa lo stava spendendo?**
//!
//! ## Il difetto che ha reso necessario il modulo (02/08/2026)
//!
//! Progetto bacheca-attivita, sub-run a5f7419c, kind `verify`: `status='timeout'`
//! dopo 180s esatti e 16 iterazioni. Il pannello diceva «tempo scaduto», che e'
//! vero e non serve a nulla: non dice se il budget e' finito su lavoro che
//! procedeva o su una strada che non poteva funzionare. In quel run la seconda,
//! e la storia lo diceva chiaramente — `which jq` a vuoto, poi
//! `sudo apt-get update` su un host Windows dove il gestore privilegiato non
//! esiste.
//!
//! Il fatto era gia' registrato in forma leggibile (`agent_steps`, un record per
//! blocco di iterazione, col risultato di ogni tool) e nessuno lo guardava
//! quando il timer scattava. La chiusura in scadenza era l'unico ramo terminale
//! che non diceva NIENTE del lavoro svolto.
//!
//! ## Il criterio
//!
//! Si guarda la CODA della storia, perche' e' li' che il budget e' finito, e si
//! risponde con l'esito piu' specifico che i fatti sostengono:
//!
//! - piu' tentativi FALLITI di fila con la stessa firma -> il run stava
//!   ripetendo una strada chiusa;
//! - un solo fallimento in coda -> si dichiara quello, con il suo exit code;
//! - la coda non e' un fallimento -> lo si dice: il tempo e' finito su lavoro
//!   che procedeva, e allargare il budget e' una domanda legittima (mentre nel
//!   primo caso sarebbe la toppa che la regola H vieta);
//! - nessuno step osservabile -> [`CausaTimeout::NonOsservabile`], mai un
//!   silenzio travestito da «tutto bene» (regola Q).
//!
//! «Fallito» viene dal CAMPO dell'esito, non dal testo: i fatti li costruisce il
//! chiamante passando dal ponte unico `nexus_types::tool_outcome::RispostaTool`,
//! che sa leggere sia il marker sia `EXIT CODE: N` (regola M). Qui non c'e'
//! nessuna euristica sul linguaggio, e non deve arrivarcene mai una: il giorno in
//! cui un tool dichiarasse il proprio esito diversamente, l'unico posto da
//! toccare e' quel ponte.
//!
//! ## Confine (regola L)
//!
//! Qui vive la REGOLA, pura e verificabile senza DB. La lettura di `agent_steps`
//! e la costruzione dei fatti stanno in `mcp-core` (`agent_tools::subagent_timeout`),
//! che porta i FATTI e non li giudica — stessa separazione di
//! [`super::correction_progress`] e della sua porta.
//!
//! ## Perche' e' una MISURA e non un rimedio
//!
//! Il modulo non allunga nessun budget e non riavvia nulla: dichiara. Un timeout
//! che nomina la propria causa e' cio' che permette di distinguere «questa figura
//! ha bisogno di piu' tempo» da «questa figura ha bisogno di sapere dove sta
//! girando» — due diagnosi opposte che, finche' l'esito era la sola parola
//! «timeout», avevano la stessa faccia.

use serde::Serialize;

/// Quanti fallimenti consecutivi con la STESSA firma bastano a dire «ripetuto».
///
/// Non e' una soglia da tarare (quindi non e' un setting, regola G): due volte
/// e' la definizione di ripetere. Il valore serve al criterio, non alla politica.
const RIPETIZIONI_MINIME: usize = 2;

/// Quanti caratteri del messaggio d'errore si portano nella dichiarazione. Il
/// messaggio e' per l'umano: deve stare in una riga del pannello, non essere il
/// log intero.
const MAX_MESSAGGIO: usize = 400;

/// Un tentativo osservato nella storia di un run: che cosa ha invocato e come e'
/// andata.
///
/// La `firma` e' la risposta a «e' la stessa strada?» e la decide il chiamante,
/// che conosce la forma dell'input del tool: due invocazioni di `run_command`
/// sono la stessa strada se il comando lo e', non perche' il tool lo e'. Il
/// criterio la confronta e basta — se la calcolasse anche lui, due idee di
/// «stessa strada» finirebbero per divergere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TentativoOsservato {
    /// Nome del tool invocato.
    pub strumento: String,
    /// Identita' della STRADA tentata (vedi sopra).
    pub firma: String,
    /// Il tool ha DICHIARATO un fallimento (campo dell'esito, mai il testo).
    pub fallito: bool,
    /// Stato d'uscita del processo, quando il tentativo ne ha eseguito uno.
    /// `None` non e' uno zero: e' «nessun processo», ed e' il caso di un tool
    /// rifiutato prima dell'esecuzione.
    pub exit_code: Option<i32>,
    /// Testo per l'umano. Nessun consumatore lo analizza per decidere.
    pub messaggio: String,
}

/// Su che cosa il budget si e' esaurito.
///
/// Serializzato col vocabolario canonico in inglese (regola N): e' cio' che
/// viaggia nel blocco `outcome` del sub-run e nel payload del meta-step, quindi
/// e' un identificatore macchina, non un'etichetta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CausaTimeout {
    /// Il run ha ritentato la STESSA strada fallendo ogni volta, fino allo
    /// scadere. E' il caso in cui allungare il budget non cambierebbe l'esito.
    RepeatedFailures {
        /// La strada ripetuta.
        firma: String,
        /// Tentativi falliti consecutivi in coda alla storia.
        tentativi: usize,
        /// Tool dell'ultimo tentativo.
        strumento: String,
        /// Stato d'uscita dell'ultimo tentativo, se c'era un processo.
        exit_code: Option<i32>,
        /// Messaggio dell'ultimo tentativo.
        messaggio: String,
    },
    /// L'ultima cosa osservata e' UN fallimento: si dichiara quello.
    LastAttemptFailed {
        firma: String,
        strumento: String,
        exit_code: Option<i32>,
        messaggio: String,
    },
    /// La storia c'e' e non finisce con un fallimento: il tempo e' scaduto su
    /// lavoro che procedeva.
    NoFailureAtEnd {
        /// Tentativi osservati nella finestra guardata.
        osservati: usize,
    },
    /// Nessuno step osservabile. Non si puo' dire perche', e dirlo e' meglio che
    /// dedurre che andasse tutto bene.
    NotObservable,
}

impl CausaTimeout {
    /// Identificatore canonico (regola N), lo stesso che la serializzazione
    /// mette in `kind`. I chiamanti lo chiedono qui invece di ricavarlo con un
    /// `matches!` proprio.
    pub fn key(&self) -> &'static str {
        match self {
            Self::RepeatedFailures { .. } => "repeated_failures",
            Self::LastAttemptFailed { .. } => "last_attempt_failed",
            Self::NoFailureAtEnd { .. } => "no_failure_at_end",
            Self::NotObservable => "not_observable",
        }
    }

    /// La riga per l'umano, composta DAI campi (regola Q): il pannello e il
    /// resoconto la leggono, nessuno la ri-analizza.
    pub fn nota(&self) -> String {
        match self {
            Self::RepeatedFailures {
                firma,
                tentativi,
                exit_code,
                messaggio,
                ..
            } => format!(
                "budget esaurito su {tentativi} tentativi falliti consecutivi di '{firma}'{}: {messaggio}",
                exit(*exit_code)
            ),
            Self::LastAttemptFailed {
                firma,
                exit_code,
                messaggio,
                ..
            } => format!(
                "budget esaurito; ultimo tentativo fallito '{firma}'{}: {messaggio}",
                exit(*exit_code)
            ),
            Self::NoFailureAtEnd { osservati } => format!(
                "budget esaurito su lavoro in corso ({osservati} passi osservati, \
                 l'ultimo non e' un fallimento)"
            ),
            Self::NotObservable => {
                "budget esaurito; nessun passo osservabile per dirne la causa".to_string()
            }
        }
    }
}

fn exit(code: Option<i32>) -> String {
    match code {
        Some(c) => format!(" (exit {c})"),
        None => String::new(),
    }
}

/// IL CRITERIO, in un posto solo.
///
/// `storia` e' in ordine CRONOLOGICO: l'ultimo elemento e' l'ultima cosa che il
/// run ha fatto prima che il tempo finisse.
pub fn classifica_causa_timeout(storia: &[TentativoOsservato]) -> CausaTimeout {
    let Some(ultimo) = storia.last() else {
        return CausaTimeout::NotObservable;
    };
    if !ultimo.fallito {
        return CausaTimeout::NoFailureAtEnd {
            osservati: storia.len(),
        };
    }
    // Quanti falliti consecutivi, all'indietro, condividono la firma dell'ultimo.
    // Un tentativo RIUSCITO interrompe la serie anche a firma uguale: fra due
    // fallimenti separati da un successo non c'e' una strada chiusa, c'e' un
    // esito che cambia.
    let tentativi = storia
        .iter()
        .rev()
        .take_while(|t| t.fallito && t.firma == ultimo.firma)
        .count();
    let messaggio = tronca(&ultimo.messaggio);
    if tentativi >= RIPETIZIONI_MINIME {
        return CausaTimeout::RepeatedFailures {
            firma: ultimo.firma.clone(),
            tentativi,
            strumento: ultimo.strumento.clone(),
            exit_code: ultimo.exit_code,
            messaggio,
        };
    }
    CausaTimeout::LastAttemptFailed {
        firma: ultimo.firma.clone(),
        strumento: ultimo.strumento.clone(),
        exit_code: ultimo.exit_code,
        messaggio,
    }
}

/// Taglio del messaggio a [`MAX_MESSAGGIO`] caratteri (non byte: il testo e'
/// UTF-8 e un taglio a byte spezzerebbe un carattere accentato).
fn tronca(testo: &str) -> String {
    let testo = testo.trim();
    if testo.chars().count() <= MAX_MESSAGGIO {
        return testo.to_string();
    }
    let tagliato: String = testo.chars().take(MAX_MESSAGGIO).collect();
    format!("{tagliato}...")
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_types::tool_outcome::{tool_failure, RispostaTool};

    /// Costruisce un tentativo passando dal PONTE reale (regola O): l'esito non
    /// e' un `bool` scritto nel test ma quello che
    /// `RispostaTool::da_testo_legacy` ricava dal risultato del tool, cioe' la
    /// stessa funzione che la porta di produzione usa sui record di
    /// `agent_steps`. Fissare `fallito: true` a mano fisserebbe l'assunto da
    /// verificare.
    fn dal_risultato(strumento: &str, firma: &str, risultato: String) -> TentativoOsservato {
        let r = RispostaTool::da_testo_legacy(risultato);
        TentativoOsservato {
            strumento: strumento.to_string(),
            firma: firma.to_string(),
            fallito: r.esito.e_fallito(),
            exit_code: r.exit_code,
            messaggio: r.testo,
        }
    }

    /// IL caso misurato (sub-run a5f7419c): il budget finisce su un tentativo
    /// fallito, e la dichiarazione deve NOMINARLO. Prima diceva solo «timeout».
    #[test]
    fn l_ultimo_fallimento_entra_nella_dichiarazione() {
        let storia = vec![
            dal_risultato("run_command", "run_command(which)", "EXIT CODE: 1\nwhich: no jq".into()),
            dal_risultato("run_command", "run_command(echo)", "EXIT CODE: 0\njq is NOT installed".into()),
            dal_risultato(
                "run_command",
                "run_command(sudo)",
                tool_failure("[sudo] apt-get update fallito: binary nexus-sudo-runner non trovato"),
            ),
        ];
        let causa = classifica_causa_timeout(&storia);
        assert_eq!(causa.key(), "last_attempt_failed");
        let nota = causa.nota();
        assert!(nota.contains("run_command(sudo)"), "{nota}");
        assert!(nota.contains("nexus-sudo-runner"), "{nota}");
    }

    /// Stessa strada, falliti di fila: e' la forma in cui allungare il budget
    /// non servirebbe a niente, e va detta come tale.
    ///
    /// MUTAZIONE: togliere `&& t.firma == ultimo.firma` dal `take_while` fa
    /// contare come ripetizione anche una serie di fallimenti su strade diverse
    /// — cioe' un run che sta provando alternative diventerebbe un run bloccato.
    /// Il secondo caso di questo test lo cattura.
    #[test]
    fn i_falliti_consecutivi_sulla_stessa_strada_sono_una_ripetizione() {
        let apt = || {
            dal_risultato(
                "run_command",
                "run_command(apt-get)",
                tool_failure("apt-get: command not found"),
            )
        };
        let causa = classifica_causa_timeout(&[apt(), apt(), apt()]);
        match &causa {
            CausaTimeout::RepeatedFailures {
                tentativi, firma, ..
            } => {
                assert_eq!(*tentativi, 3);
                assert_eq!(firma, "run_command(apt-get)");
            }
            altro => panic!("atteso repeated_failures, ottenuto {altro:?}"),
        }
        assert!(causa.nota().contains("3 tentativi falliti"), "{}", causa.nota());

        // Strade DIVERSE: tre fallimenti, ma non una strada chiusa ripetuta.
        let storia = vec![
            dal_risultato("run_command", "run_command(apt-get)", tool_failure("boom")),
            dal_risultato("run_command", "run_command(brew)", tool_failure("boom")),
            dal_risultato("run_command", "run_command(winget)", tool_failure("boom")),
        ];
        assert_eq!(
            classifica_causa_timeout(&storia).key(),
            "last_attempt_failed",
            "tentare alternative diverse non e' ripetere la stessa strada"
        );
    }

    /// Un successo in mezzo interrompe la serie: due fallimenti separati da un
    /// esito buono non sono una strada chiusa.
    #[test]
    fn un_successo_interrompe_la_serie() {
        let fallito = || dal_risultato("run_command", "run_command(npm)", tool_failure("boom"));
        let riuscito =
            dal_risultato("run_command", "run_command(npm)", "EXIT CODE: 0\nok".into());
        let storia = vec![fallito(), riuscito, fallito()];
        assert_eq!(classifica_causa_timeout(&storia).key(), "last_attempt_failed");
    }

    /// Il tempo finito su lavoro che procedeva e' una diagnosi DIVERSA, ed e'
    /// l'unico caso in cui la domanda «il budget di questo kind e' dimensionato
    /// bene?» ha senso. Collassarlo con gli altri renderebbe la misura inutile
    /// proprio per la decisione che deve informare.
    #[test]
    fn una_coda_riuscita_non_e_un_fallimento() {
        let storia = vec![
            dal_risultato("read_file", "read_file", "contenuto".into()),
            dal_risultato("run_command", "run_command(npm)", "EXIT CODE: 0\nok".into()),
        ];
        let causa = classifica_causa_timeout(&storia);
        assert_eq!(causa, CausaTimeout::NoFailureAtEnd { osservati: 2 });
        assert!(causa.nota().contains("lavoro in corso"), "{}", causa.nota());
    }

    /// Nessuno step: si dichiara di non poter dire. Mai un silenzio che somigli
    /// a un esito.
    #[test]
    fn senza_storia_la_causa_e_non_osservabile() {
        let causa = classifica_causa_timeout(&[]);
        assert_eq!(causa, CausaTimeout::NotObservable);
        assert_eq!(causa.key(), "not_observable");
    }

    /// Un comando che esce con codice != 0 e' un TOOL riuscito che riporta un
    /// comando fallito: i due assi restano distinti (vedi `RispostaTool::comando`).
    /// Contarlo come fallimento del tentativo renderebbe «il build e' rosso»
    /// indistinguibile da «il tool non e' riuscito a lanciarlo», che e'
    /// esattamente la distinzione su cui si decide se ritentare o cambiare strada.
    #[test]
    fn un_exit_code_non_zero_non_e_di_per_se_un_tentativo_fallito() {
        let storia = vec![dal_risultato(
            "run_command",
            "run_command(cargo)",
            "EXIT CODE: 101\nerror[E0308]".into(),
        )];
        let causa = classifica_causa_timeout(&storia);
        assert_eq!(causa.key(), "no_failure_at_end");
    }

    /// Il vocabolario sul wire e' quello canonico in inglese (regola N) e
    /// coincide con [`CausaTimeout::key`]: chi legge il JSON e chi legge il tipo
    /// vedono lo stesso nome.
    #[test]
    fn la_serializzazione_usa_il_vocabolario_canonico() {
        let causa = classifica_causa_timeout(&[dal_risultato(
            "run_command",
            "run_command(apt-get)",
            tool_failure("apt-get: command not found"),
        )]);
        let v = serde_json::to_value(&causa).expect("serializzabile");
        assert_eq!(v["kind"], causa.key());
        assert_eq!(v["firma"], "run_command(apt-get)");
    }
}

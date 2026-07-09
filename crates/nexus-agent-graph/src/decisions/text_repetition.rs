//! `text_repetition`: rilevazione PURA del *repetition-collapse* di un turno
//! assistant — il testo degenere in cui il modello ripete la STESSA sottostringa
//! decine/centinaia di volte consecutive (tipico collasso dei modelli piccoli,
//! es. `codestral` che emette "error Command failed with exit code 1. " x898).
//!
//! PUNTO UNICO (regola L) del rilevamento collapse-per-ripetizione: il call site
//! dell'executor delega qui invece di ispezionare il testo a mano. Complementare
//! ai detector anti-loop esistenti — [`super::loop_signatures`] guarda la FIRMA
//! delle tool call ripetute, mentre qui il segnale e' la periodicita' STRUTTURALE
//! del TESTO del turno (una tool call singola + un muro di testo ripetuto non
//! triggera il signature-loop, ma e' comunque spazzatura non-verificata).
//!
//! REGOLA M: il segnale e' STRUTTURALE (periodo minimo della coda del testo,
//! calcolato deterministicamente), NON semantico. Non si giudica il CONTENUTO
//! ("sembra un errore"): si misura che una sottostringa si ripete `>= min_repeats`
//! volte coprendo `>= min_total_len` caratteri. L'esito (chiudere il run come
//! non-verificato) e' del chiamante; qui c'e' solo la misura.
//!
//! Tutte le funzioni sono pure (nessun IO, nessuna lettura DB): le soglie
//! DB-driven (regola G) arrivano come parametro esplicito, cosi' il modulo resta
//! deterministico e testabile a qualunque soglia.

/// Soglie DB-driven (regola G) del rilevamento repetition-collapse. I default
/// valgono SOLO se il DB non fornisce i setting `agent.anti_repetition.*` (mai
/// come magic fallback nella logica). Passate come parametro cosi' il modulo
/// resta puro e testabile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepetitionThresholds {
    /// Lunghezza MINIMA (in caratteri) dell'unita' ripetuta considerata. `1`
    /// cattura anche il collasso di un singolo carattere ripetuto; le unita' di
    /// soli whitespace sono comunque escluse (padding, non collasso semantico).
    pub min_unit_len: usize,
    /// Lunghezza MASSIMA dell'unita' ripetuta: oltre questa (`> max_unit_len`) un
    /// periodo "lungo" e' quasi sempre struttura legittima (paragrafi simili, non
    /// un loop degenere) -> non conta come collapse.
    pub max_unit_len: usize,
    /// Numero MINIMO di ripetizioni consecutive della stessa unita' oltre cui
    /// (`>=`) il testo e' considerato in collasso.
    pub min_repeats: usize,
    /// Caratteri MINIMI coperti dalla ripetizione (`repeats * unit_len`) perche'
    /// conti come collasso. Evita falsi positivi su ripetizioni brevi legittime
    /// (es. "ok ok ok"): la porzione ripetuta deve essere sostanziosa.
    pub min_total_len: usize,
    /// Cap dei caratteri della CODA ispezionati. Il collasso degenera verso la
    /// fine del testo, quindi si analizza la coda (ultimi `scan_tail_cap` char):
    /// mantiene il costo O(coda) anche su final_answer molto lunghi. `0` =
    /// disabilitato (nessuna ispezione).
    pub scan_tail_cap: usize,
}

impl Default for RepetitionThresholds {
    fn default() -> Self {
        // Default safe (regola G): valgono a DB irraggiungibile. `min_repeats=20`
        // + `min_total_len=400` sono conservativi (bassa probabilita' di falso
        // positivo su testo legittimo), ma catturano nettamente il caso reale
        // (unita' ~40 char x 898 = ~36k char coperti).
        Self {
            min_unit_len: 1,
            max_unit_len: 512,
            min_repeats: 20,
            min_total_len: 400,
            scan_tail_cap: 16_384,
        }
    }
}

/// Esito del rilevamento: l'unita' ripetuta (troncata per l'anteprima), quante
/// volte si ripete nella coda ispezionata e quanti caratteri copre.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepetitionHit {
    /// L'unita' che si ripete (i primi `unit_len` caratteri della coda periodica).
    pub unit: String,
    /// Numero di ripetizioni consecutive dell'unita' nella coda ispezionata.
    pub repeats: usize,
    /// Caratteri coperti dalla ripetizione (`repeats * unit_len`).
    pub span_len: usize,
}

impl RepetitionHit {
    /// Anteprima dell'unita' ripetuta, whitespace collassato e troncata a
    /// `max_chars` (per il messaggio onesto di chiusura / meta_step). Deterministica.
    pub fn unit_preview(&self, max_chars: usize) -> String {
        let collapsed: String = {
            let mut out = String::with_capacity(self.unit.len());
            let mut prev_ws = false;
            for c in self.unit.chars() {
                if c.is_whitespace() {
                    if !prev_ws {
                        out.push(' ');
                    }
                    prev_ws = true;
                } else {
                    out.push(c);
                    prev_ws = false;
                }
            }
            out.trim().to_string()
        };
        let mut it = collapsed.chars();
        let head: String = it.by_ref().take(max_chars).collect();
        if it.next().is_some() {
            format!("{head}...")
        } else {
            head
        }
    }
}

/// Rileva un repetition-collapse nel testo: `Some(hit)` se la coda del testo e'
/// periodica con un periodo `p` in `[min_unit_len, max_unit_len]` ripetuto
/// `>= min_repeats` volte coprendo `>= min_total_len` caratteri; `None` altrimenti.
///
/// Metodo (regola M, deterministico): sulla CODA del testo (ultimi
/// `scan_tail_cap` char) si prova ogni periodo candidato `p` crescente e si
/// contano le ripetizioni consecutive dell'ULTIMA unita' `tail[m-p..m]` andando
/// all'indietro. Ancorare alla FINE rende il rilevamento robusto a un prefisso
/// non periodico (testo normale seguito da collasso). Un testo NON periodico ha
/// `repeats = 1` per ogni `p` -> `None`. Le unita' di soli whitespace (padding)
/// sono escluse: non sono un collasso semantico. Nota: testo STRUTTURATO con
/// righe DIVERSE (es. tabella markdown con dati distinti) non e' periodico ->
/// non rilevato; solo righe IDENTICHE ripetute (degeneri) lo sono.
pub fn detect_repetition_collapse(text: &str, th: RepetitionThresholds) -> Option<RepetitionHit> {
    if th.scan_tail_cap == 0 || th.min_repeats == 0 {
        return None;
    }
    // Char-based (unicode-safe): il collasso puo' contenere multibyte.
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n < th.min_total_len {
        return None;
    }
    let start = n.saturating_sub(th.scan_tail_cap);
    let tail = &chars[start..];
    let m = tail.len();
    if m < th.min_total_len {
        return None;
    }

    // Ancoraggio alla FINE (regola M): per ogni periodo candidato `p` si contano
    // le ripetizioni consecutive dell'ULTIMA unita' `tail[m-p..m]` andando
    // all'indietro. Ancorare alla fine (non all'intera coda) rende il rilevamento
    // robusto a un PREFISSO non periodico (testo normale seguito da collasso): il
    // conteggio si ferma appena il testo smette di ripetersi, ignorando il
    // prefisso. `p` cresce dal minimo -> il primo che soddisfa e' il periodo
    // FONDAMENTALE (l'unita' estratta e' allineata all'unita' naturale, es.
    // "FAIL\n" invece di uno shift "AIL\nF"). O(max_p^2 + span) nel caso peggiore.
    let min_p = th.min_unit_len.max(1);
    // Cap: oltre `m / min_repeats` non ci stanno abbastanza ripetizioni.
    let max_p = th.max_unit_len.min(m / th.min_repeats);
    for p in min_p..=max_p {
        let unit = &tail[m - p..m];
        // L'unita' di soli whitespace e' padding, non collasso semantico: salta
        // a un periodo diverso invece di scartare del tutto (un collasso reale
        // preceduto da whitespace ha comunque il suo periodo piu' avanti).
        if unit.iter().all(|c| c.is_whitespace()) {
            continue;
        }
        let mut repeats = 1usize;
        let mut pos = m - p;
        while pos >= p && &tail[pos - p..pos] == unit {
            repeats += 1;
            pos -= p;
        }
        let span_len = repeats * p;
        if repeats >= th.min_repeats && span_len >= th.min_total_len {
            return Some(RepetitionHit {
                unit: unit.iter().collect(),
                repeats,
                span_len,
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn th() -> RepetitionThresholds {
        RepetitionThresholds::default()
    }

    #[test]
    fn caso_reale_command_failed_ripetuto() {
        // Riproduce il collasso osservato (run de7477e9): la frase ripetuta ~898
        // volte. Con default (min_repeats=20) deve essere rilevato.
        let unit = "error Command failed with exit code 1. ";
        let text = format!(
            "vite.config.ts:1:1: error: Unable to resolve path to module 'vite'. {}",
            unit.repeat(898)
        );
        let hit = detect_repetition_collapse(&text, th()).expect("collapse atteso");
        assert!(hit.repeats >= 20, "repeats={}", hit.repeats);
        assert!(hit.span_len >= 400);
        assert!(hit.unit.contains("Command failed"));
    }

    #[test]
    fn singolo_carattere_ripetuto_e_collapse() {
        let text = "a".repeat(1000);
        let hit = detect_repetition_collapse(&text, th()).expect("collapse atteso");
        assert_eq!(hit.unit, "a");
        assert!(hit.repeats >= 20);
    }

    #[test]
    fn testo_normale_non_e_collapse() {
        let text = "Ho analizzato il file, corretto l'import mancante e rieseguito \
                    la build: ora compila senza errori. Restano due warning di lint \
                    non bloccanti che ho annotato nel report finale per il follow-up.";
        assert!(detect_repetition_collapse(text, th()).is_none());
    }

    #[test]
    fn tabella_markdown_righe_diverse_non_e_collapse() {
        // FALSO POSITIVO da escludere (finding review #10): una tabella con dati
        // DISTINTI per riga NON e' periodica -> non deve triggerare, anche con
        // 40+ righe simili nella forma.
        let mut t = String::from("| Nome | Valore |\n|------|--------|\n");
        for i in 0..40 {
            t.push_str(&format!("| campo_{i:03} | risultato numero {i} |\n"));
        }
        assert!(detect_repetition_collapse(&t, th()).is_none());
    }

    #[test]
    fn lista_puntata_contenuti_diversi_non_e_collapse() {
        // Anche una lista lunga con voci diverse non e' periodica.
        let mut t = String::from("Passi eseguiti:\n");
        for i in 0..50 {
            t.push_str(&format!("- step {i}: azione completata con esito ok\n"));
        }
        assert!(detect_repetition_collapse(&t, th()).is_none());
    }

    #[test]
    fn righe_identiche_ripetute_sono_collapse() {
        // Righe IDENTICHE ripetute (contenuto degenere, non dati) SONO un collasso:
        // comportamento intenzionale (documentato) distinto dalla tabella con dati.
        let t = "| x | y |\n".repeat(80);
        let hit = detect_repetition_collapse(&t, th()).expect("collapse atteso");
        assert!(hit.repeats >= 20);
    }

    #[test]
    fn testo_corto_sotto_soglia() {
        // Ripetizione presente ma span < min_total_len -> non conta.
        let text = "ok ".repeat(10);
        assert!(detect_repetition_collapse(&text, th()).is_none());
    }

    #[test]
    fn whitespace_ripetuto_escluso() {
        let text = format!("Fine risposta.{}", " ".repeat(1000));
        assert!(detect_repetition_collapse(&text, th()).is_none());
    }

    #[test]
    fn ripetizione_solo_in_coda() {
        // Testa che la coda periodica sia rilevata anche con un prefisso normale.
        let prefix = "Ecco il risultato del comando che ho eseguito:\n";
        let text = format!("{prefix}{}", "FAIL\n".repeat(300));
        let hit = detect_repetition_collapse(&text, th()).expect("collapse atteso");
        assert!(hit.unit.contains("FAIL"));
        assert!(hit.repeats >= 20);
    }

    #[test]
    fn soglia_disabilitata_no_op() {
        let text = "x".repeat(5000);
        let disabled = RepetitionThresholds {
            scan_tail_cap: 0,
            ..RepetitionThresholds::default()
        };
        assert!(detect_repetition_collapse(&text, disabled).is_none());
    }

    #[test]
    fn min_repeats_zero_no_op() {
        let text = "y".repeat(5000);
        let disabled = RepetitionThresholds {
            min_repeats: 0,
            ..RepetitionThresholds::default()
        };
        assert!(detect_repetition_collapse(&text, disabled).is_none());
    }
}

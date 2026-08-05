//! PUNTO UNICO (regola L) della domanda: **questo rimando in correzione ha
//! prodotto progresso?**
//!
//! ## Il difetto che ha reso necessario il modulo (28/07/2026)
//!
//! Il ciclo review -> correzione contava i TENTATIVI ma non il PROGRESSO. Un
//! rimando in cui l'agente non modificava nulla consumava un tentativo
//! esattamente come uno in cui correggeva. Misurato sul progetto
//! `gestione-spese`: il panel emette un rilievo CORRETTO
//! (`vite.config.js` usa `process.env.API_URL`, i componenti
//! `import.meta.env.VITE_API_URL`), l'agente risponde tre volte "Nessuna azione
//! necessaria. Il task e' stato completato e verificato nei turni precedenti", e
//! il run chiude al cap dopo tre bocciature e ZERO file toccati. Costo:
//! 1.243.417 token (1.178.170 in ingresso contro 6.138 in uscita) e tre
//! convocazioni di un panel di due revisori sullo STESSO identico codice.
//!
//! Il meccanismo di rimando funzionava: il gate valutava cio' che l'agente
//! DICEVA. Il fatto — ha modificato dei file? — era gia' registrato in forma
//! strutturata (`file_mutations.before_sha256` / `after_sha256`, mig 0349) e
//! nessuno lo leggeva. Regola M applicata al posto sbagliato.
//!
//! ## Il criterio
//!
//! Non "il tool di scrittura e' stato chiamato" ma
//! `before_sha256 != after_sha256`. Riscrivere un file con contenuto identico
//! non e' una correzione, ed e' esattamente il modo in cui un agente puo'
//! simulare attivita' senza produrne: il write compare nei log, nei tool_result
//! e in `agent_steps`, e solo gli hash sanno che non e' successo niente.
//!
//! ## Confine (regola L)
//!
//! Qui vive la REGOLA (cosa conta come progresso), pura e verificabile senza DB.
//! L'I/O (leggere le scritture registrate) e' la porta
//! [`crate::runtime::ports::MutationProgressPort`], che porta soltanto i FATTI
//! ([`WriteFact`]) e non li giudica: due giudizi in due posti darebbero due idee
//! diverse di "corretto", ed e' il caso in cui la query SQL e il nodo
//! divergerebbero in silenzio.
//!
//! ## Perche' il final_gate NON usa questo criterio per saltare la verifica
//!
//! Il final_gate ha la stessa forma di ciclo (bocciatura -> rimando -> nuovo
//! giro) e quindi lo stesso rischio apparente. La valutazione, fatta insieme a
//! questo modulo, dice di NON applicargli il taglio del punto 1:
//!
//! - Il review panel giudica IL CODICE, e il codice e' esattamente cio' che
//!   `file_mutations` misura: "nessun file cambiato -> stesso verdetto" e' vero
//!   per costruzione. I criteri del final_gate misurano invece lo STATO
//!   DELL'AMBIENTE (`typecheck`, `build`, `test`, endpoint, log): possono
//!   cambiare esito senza che un file dell'agente cambi — un servizio avviato,
//!   una dipendenza installata, una porta liberata. Saltarli perche' non risulta
//!   una scrittura dichiarerebbe fermo un run che ha risolto il problema in un
//!   altro modo: un falso negativo peggiore della spesa che eviterebbe.
//! - La spesa e' di natura diversa: un giro del final_gate costa tempo macchina
//!   (comandi locali), un giro di review costa un panel di sub-run a pagamento.
//!   Il risparmio che qui vale 2 convocazioni su 3 li' vale una `npm test`.
//!
//! Resta applicabile al final_gate il punto 2 (il fatto misurato come
//! contestazione nel rimando), che non tocca la spesa ma il prompt: se lo si
//! vorra' fare, si chiami questo modulo — non si riscriva il confronto.

/// Fatto grezzo di UNA scrittura registrata (regola M: gli hash del contenuto,
/// mai il testo del tool_result ne' la prosa dell'agente).
///
/// `None` su un lato e' un'informazione, non un vuoto: `before` assente = file
/// CREATO, `after` assente = file CANCELLATO. Entrambi i casi cambiano il
/// contenuto, ed e' il confronto a dirlo senza che serva un campo `op`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteFact {
    /// SHA-256 del contenuto PRIMA della scrittura (`None` = il file non esisteva).
    pub before_sha256: Option<String>,
    /// SHA-256 del contenuto DOPO la scrittura (`None` = il file e' stato cancellato).
    pub after_sha256: Option<String>,
    /// La scrittura ha cambiato i SOLI fine-riga? Misurato alla fonte, dove i
    /// byte esistono ancora (`mcp-core::file_mutations`, mig 0680), perche' da
    /// due hash non e' deducibile.
    ///
    /// `None` = non misurato: righe anteriori alla migrazione, o casi in cui la
    /// domanda non si pone (file creato o cancellato). Li' il criterio ricade
    /// sul confronto degli hash, cioe' sul comportamento di prima — l'ignoto
    /// non degrada ne' a "e' cambiato" ne' a "non e' cambiato" (regola Q).
    pub solo_fine_riga: Option<bool>,
}

impl WriteFact {
    /// IL CRITERIO, in un posto solo.
    ///
    /// Una scrittura conta come cambiamento solo se l'hash del contenuto dopo
    /// differisce da quello prima. Il caso `Some(x) == Some(x)` (riscrittura
    /// identica) e' il difetto che questo modulo esiste per vedere; il caso
    /// `None == None` (chiamata degenere senza contenuto da nessun lato) non e'
    /// un cambiamento e cade sotto lo stesso confronto.
    pub fn cambia_il_contenuto(&self) -> bool {
        // Una riscrittura che cambia i soli fine-riga ha hash DIVERSI e contenuto
        // identico: senza questo ramo passerebbe per lavoro fatto, ed e' la
        // stessa cosa che questo modulo esiste per vedere — solo travestita.
        // Misurata alla fonte (mig 0680) perche' da due digest non si deduce.
        //
        // Non e' teorico su Windows: `core.autocrlf=true` e' attivo, e il
        // 2026-08-05 quindici script del repo risultavano CRLF sul disco col
        // blob a LF.
        if self.solo_fine_riga == Some(true) {
            return false;
        }
        self.before_sha256 != self.after_sha256
    }
}

/// Verdetto sul progresso di UN rimando in correzione.
///
/// Le tre varianti NON sono gradazioni della stessa cosa: distinguono un agente
/// che non ha alzato un dito da uno che ha riscritto file identici. La seconda
/// e' piu' grave della prima come segnale — l'attivita' c'e' stata, e non ha
/// prodotto nulla — e in un resoconto va detta come tale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrectionProgress {
    /// Almeno una scrittura ha cambiato il contenuto di un file.
    Effettivo {
        /// Scritture che hanno cambiato contenuto.
        scritture_efficaci: usize,
    },
    /// Ci sono state scritture, ma NESSUNA ha cambiato contenuto: file riscritti
    /// identici a se stessi.
    SoloRiscritture {
        /// Scritture registrate, tutte a contenuto invariato.
        riscritture: usize,
    },
    /// Nessuna scrittura registrata dopo il rimando.
    NessunaScrittura,
}

impl CorrectionProgress {
    /// `true` solo per [`CorrectionProgress::Effettivo`]: e' la domanda che i
    /// chiamanti pongono, e la pongono a questo metodo invece di fare `matches!`
    /// per conto proprio.
    pub fn e_progresso(&self) -> bool {
        matches!(self, Self::Effettivo { .. })
    }

    /// Identificatore canonico (regola N): un solo nome per verdetto, in
    /// inglese, per log, payload dei meta-step e resoconto.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Effettivo { .. } => "effective",
            Self::SoloRiscritture { .. } => "rewrites_only",
            Self::NessunaScrittura => "no_writes",
        }
    }

    /// Il FATTO da opporre all'agente, in parole, quando il rimando e' andato a
    /// vuoto. `None` quando c'e' stato progresso: non c'e' niente da opporre.
    ///
    /// E' l'unico punto in cui la misura diventa testo. La frase nasce DAL
    /// verdetto strutturato (regola M): il chiamante non ricompone da capo una
    /// descrizione del proprio conteggio, altrimenti due rimandi direbbero la
    /// stessa cosa in due modi e nessuno dei due sarebbe la misura.
    pub fn fatto_opponibile(&self) -> Option<String> {
        match self {
            Self::Effettivo { .. } => None,
            Self::SoloRiscritture { riscritture } => Some(format!(
                "sono state registrate {riscritture} scritture, ma NESSUNA ha cambiato \
                 il contenuto di un file (hash del contenuto identico prima e dopo)"
            )),
            Self::NessunaScrittura => Some(
                "nessun file risulta scritto dopo il rimando precedente".to_string(),
            ),
        }
    }
}

/// Classifica il progresso di un rimando dalle scritture registrate dopo di esso.
///
/// L'ordine di precedenza e' load-bearing: basta UNA scrittura efficace perche'
/// il rimando abbia prodotto progresso, per quante riscritture a vuoto la
/// accompagnino. Il contrario (pretendere che tutte siano efficaci) leggerebbe
/// come "a vuoto" una correzione reale accompagnata da un salvataggio idempotente
/// — e sopprimerebbe la ri-review di un lavoro fatto davvero.
pub fn classify_correction_progress(facts: &[WriteFact]) -> CorrectionProgress {
    let efficaci = facts.iter().filter(|f| f.cambia_il_contenuto()).count();
    if efficaci > 0 {
        return CorrectionProgress::Effettivo {
            scritture_efficaci: efficaci,
        };
    }
    if facts.is_empty() {
        CorrectionProgress::NessunaScrittura
    } else {
        CorrectionProgress::SoloRiscritture {
            riscritture: facts.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    /// (a) del difetto: una correzione reale e' progresso.
    #[test]
    fn contenuto_cambiato_e_progresso() {
        let facts = vec![WriteFact {
            before_sha256: hash("aaa"),
            after_sha256: hash("bbb"),
            solo_fine_riga: None,
        }];
        let p = classify_correction_progress(&facts);
        assert_eq!(p, CorrectionProgress::Effettivo { scritture_efficaci: 1 });
        assert!(p.e_progresso());
        assert_eq!(p.fatto_opponibile(), None, "niente da opporre: ha corretto");
    }

    /// (b) del difetto: nessuna scrittura -> nessun progresso. E' il caso
    /// letterale del run osservato: "Nessuna azione necessaria" ripetuto tre volte.
    #[test]
    fn nessuna_scrittura_non_e_progresso() {
        let p = classify_correction_progress(&[]);
        assert_eq!(p, CorrectionProgress::NessunaScrittura);
        assert!(!p.e_progresso());
        assert!(p
            .fatto_opponibile()
            .expect("c'e' un fatto da opporre")
            .contains("nessun file risulta scritto"));
    }

    /// (c) del difetto, il caso che il criterio esiste per vedere: il write c'e'
    /// stato, l'hash e' lo STESSO. Un contatore di chiamate a `write_file` qui
    /// direbbe "ha lavorato"; il confronto degli hash dice che non e' successo
    /// niente.
    ///
    /// MUTAZIONE: cambiare [`WriteFact::cambia_il_contenuto`] in `true` costante
    /// (cioe' contare le CHIAMATE invece dei cambiamenti) rende questo test rosso
    /// con `Effettivo { scritture_efficaci: 2 }`.
    #[test]
    fn riscrittura_identica_non_e_progresso() {
        let facts = vec![
            WriteFact {
                before_sha256: hash("uguale"),
                after_sha256: hash("uguale"),
                solo_fine_riga: None,
            },
            WriteFact {
                before_sha256: hash("anche-questo"),
                after_sha256: hash("anche-questo"),
                solo_fine_riga: None,
            },
        ];
        let p = classify_correction_progress(&facts);
        assert_eq!(
            p,
            CorrectionProgress::SoloRiscritture { riscritture: 2 },
            "due write a contenuto invariato non sono una correzione"
        );
        assert!(!p.e_progresso());
        assert!(p
            .fatto_opponibile()
            .expect("c'e' un fatto da opporre")
            .contains("NESSUNA ha cambiato"));
    }

    /// Una correzione reale in mezzo a riscritture a vuoto resta progresso: il
    /// criterio e' esistenziale, non universale. Senza questa precedenza, un
    /// salvataggio idempotente accanto a una modifica vera sopprimerebbe la
    /// ri-review di un lavoro fatto davvero — un falso negativo peggiore del
    /// difetto che stiamo chiudendo.
    #[test]
    fn una_sola_scrittura_efficace_basta() {
        let facts = vec![
            WriteFact {
                before_sha256: hash("x"),
                after_sha256: hash("x"),
                solo_fine_riga: None,
            },
            WriteFact {
                before_sha256: hash("y"),
                after_sha256: hash("z"),
                solo_fine_riga: None,
            },
            WriteFact {
                before_sha256: hash("w"),
                after_sha256: hash("w"),
                solo_fine_riga: None,
            },
        ];
        assert_eq!(
            classify_correction_progress(&facts),
            CorrectionProgress::Effettivo { scritture_efficaci: 1 }
        );
    }

    /// Creazione e cancellazione cambiano il contenuto: il lato assente e' un
    /// dato, non un vuoto da trattare come "niente". Senza, un file creato ex
    /// novo per correggere il rilievo non conterebbe come correzione.
    #[test]
    fn creazione_e_cancellazione_cambiano_il_contenuto() {
        let creato = WriteFact {
            before_sha256: None,
            after_sha256: hash("nuovo"),
            solo_fine_riga: None,
        };
        let cancellato = WriteFact {
            before_sha256: hash("vecchio"),
            after_sha256: None,
            solo_fine_riga: None,
        };
        let degenere = WriteFact {
            before_sha256: None,
            after_sha256: None,
            solo_fine_riga: None,
        };
        assert!(creato.cambia_il_contenuto());
        assert!(cancellato.cambia_il_contenuto());
        assert!(
            !degenere.cambia_il_contenuto(),
            "nessun contenuto da nessun lato: non e' un cambiamento"
        );
    }

    /// Gli identificatori sono canonici e distinti (regola N): il resoconto e i
    /// log li usano per dire QUALE dei due modi di non correggere si e' visto.
    #[test]
    fn identificatori_canonici_distinti() {
        assert_eq!(
            CorrectionProgress::Effettivo { scritture_efficaci: 1 }.as_str(),
            "effective"
        );
        assert_eq!(
            CorrectionProgress::SoloRiscritture { riscritture: 1 }.as_str(),
            "rewrites_only"
        );
        assert_eq!(CorrectionProgress::NessunaScrittura.as_str(), "no_writes");
    }

    /// IL CASO DELLA MIG 0680: una riscrittura che cambia i soli fine-riga ha
    /// hash DIVERSI, e senza il campo misurato alla fonte passerebbe per
    /// progresso — cioe' per lavoro fatto su un file in cui non e' cambiato
    /// nulla. E' la stessa cosa che questo modulo esiste per vedere, travestita.
    #[test]
    fn una_riscrittura_di_soli_fine_riga_non_e_progresso() {
        let facts = vec![WriteFact {
            before_sha256: Some("hash-del-file-lf".into()),
            after_sha256: Some("hash-del-file-crlf".into()),
            solo_fine_riga: Some(true),
        }];
        assert!(!facts[0].cambia_il_contenuto());
        assert_eq!(
            classify_correction_progress(&facts),
            CorrectionProgress::SoloRiscritture { riscritture: 1 }
        );
    }

    /// Il controcampo: hash diversi E contenuto davvero diverso restano
    /// progresso. Senza questo, un `return false` incondizionato passerebbe il
    /// test sopra e romperebbe il criterio.
    #[test]
    fn un_contenuto_diverso_resta_progresso() {
        let facts = vec![WriteFact {
            before_sha256: Some("a".into()),
            after_sha256: Some("b".into()),
            solo_fine_riga: Some(false),
        }];
        assert!(facts[0].cambia_il_contenuto());
        assert_eq!(
            classify_correction_progress(&facts),
            CorrectionProgress::Effettivo { scritture_efficaci: 1 }
        );
    }

    /// `None` = non misurato (righe anteriori alla mig 0680): il criterio ricade
    /// sul confronto degli hash, cioe' sul comportamento di prima. L'ignoto non
    /// degrada a "non e' cambiato".
    #[test]
    fn il_non_misurato_ricade_sul_confronto_degli_hash() {
        let f = WriteFact {
            before_sha256: Some("a".into()),
            after_sha256: Some("b".into()),
            solo_fine_riga: None,
        };
        assert!(f.cambia_il_contenuto());
    }
}

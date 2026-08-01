//! Contratto MACCHINA del fallimento di un tool agente: come un tool lo
//! DICHIARA e come chiunque lo RICONOSCE.
//!
//! # Perche' vive qui, e perche' e' uno solo
//!
//! I tool agente ritornano una `String` nuda: nessun canale d'errore separato.
//! L'unico modo per un tool di dire "ho fallito" e' scriverlo nel proprio
//! risultato, in una forma che il chiamante possa leggere senza interpretare la
//! prosa. Quella forma e' il marker [`TOOL_FAILURE_MARKER`] in TESTA al
//! risultato: un segnale strutturato in un canale testuale, non un'euristica
//! sul linguaggio (regola M) — nella stessa famiglia di `EXIT CODE: N`, che e'
//! un formato prodotto da noi e riletto da noi.
//!
//! Il contratto sta in `nexus-types` perche' i tre lati che lo devono
//! condividere non si vedono fra loro:
//! - `nexus-agent-tools` PRODUCE ([`tool_failure`]);
//! - `mcp-core` lo traduce in `ToolOutcome.is_error` al confine del dispatch;
//! - `nexus-agent-graph` lo LEGGE dai messaggi (anti-loop, supervisore, gate),
//!   e non dipende da `nexus-agent-tools`.
//!
//! Prima di questo modulo il criterio era scritto due volte con due
//! vocabolari diversi — il marker in `mcp-core::tool_runner_server`, il
//! prefisso `[Errore` nei tool — e quindi la domanda "questo tool e' fallito?"
//! aveva risposte diverse a seconda di chi la ponesse. Un tool che falliva
//! senza marker risultava RIUSCITO a `is_error`, e tre consumatori erano
//! costretti a indovinare l'esito dal testo.
//!
//! # Il canale c'e': [`RispostaTool`] (regola Q)
//!
//! La premessa sopra — "nessun canale d'errore separato" — era vera e non lo e'
//! piu': [`RispostaTool`] porta l'esito in un CAMPO e il testo resta testo. Un
//! marker in testa a una stringa e' un campo travestito da prosa, e il modo in
//! cui questo si e' visto e' istruttivo: [`is_tool_failure`] guarda la testa,
//! due composizioni legittime vi anteponevano prosa di successo, e l'apparato
//! anti-loop dedicato alla firma "servizio non in ascolto" e' rimasto
//! irraggiungibile per costruzione — senza che un test potesse accorgersene,
//! perche' il contratto non era un tipo.
//!
//! # Regola per chi scrive un tool nuovo
//!
//! Si ritorna [`RispostaTool`], costruita con [`RispostaTool::riuscito`],
//! [`RispostaTool::fallito`] o [`RispostaTool::comando`]. Il marker e le
//! funzioni che lo maneggiano restano SOLO per il ponte legacy
//! ([`RispostaTool::da_testo_legacy`]) finche' l'ultimo tool non e' migrato: un
//! tool nuovo che scrivesse il marker a mano reintrodurrebbe il difetto in un
//! sistema che ha gia' il campo.

use std::fmt::Display;

/// Come e' andata l'esecuzione di un tool, DICHIARATA dal tool stesso.
///
/// Due casi, non tre: il tool sa sempre se ha fatto cio' che doveva. L'ignoto
/// appartiene a chi INTERPRETA un esito ([`crate::tool_outcome`] non ha voce in
/// capitolo su "il criterio e' soddisfatto?"), e il guasto infrastrutturale
/// appartiene a chi il tool lo INVOCA: se il tool non risponde affatto, non c'e'
/// nessuna `RispostaTool` da leggere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EsitoTool {
    /// Il tool ha fatto cio' che doveva.
    Riuscito,
    /// Errore APPLICATIVO: input rifiutato, risorsa assente, operazione negata,
    /// comando terminato male. Il testo dice cosa e' successo e come correggere;
    /// il campo dice che e' andata male, e i due non si sostituiscono.
    Fallito,
}

impl EsitoTool {
    /// `true` se il tool ha dichiarato un fallimento applicativo. E' il valore
    /// che confluisce in `ToolOutcome.is_error` al confine del dispatch.
    pub fn e_fallito(self) -> bool {
        matches!(self, EsitoTool::Fallito)
    }
}

/// Cio' che un tool agente restituisce: l'esito in un campo, il testo per
/// l'umano e per il modello in un altro, lo stato d'uscita del processo in un
/// terzo quando il tool ne ha eseguito uno.
///
/// E' il tipo che la regola Q pretende: finche' la firma era `-> String`,
/// l'esito non aveva dove stare e finiva nel testo per NECESSITA' — il parsing
/// a valle era la conseguenza, non la causa.
#[derive(Debug, Clone, PartialEq)]
pub struct RispostaTool {
    /// Testo leggibile. Nessun consumatore lo analizza per decidere.
    pub testo: String,
    pub esito: EsitoTool,
    /// Stato d'uscita del processo, quando il tool ne ha eseguito uno. `None`
    /// per i tool che non eseguono processi — e per un comando RIFIUTATO prima
    /// dell'esecuzione, che infatti e' `Fallito` senza `exit_code`: e' la
    /// distinzione che il final_gate usa per non assolvere un criterio la cui
    /// invocazione e' stata negata.
    pub exit_code: Option<i32>,
}

impl RispostaTool {
    /// Il tool ha fatto cio' che doveva. Nessun exit code: se il tool ha
    /// eseguito un processo, si usa [`Self::comando`].
    pub fn riuscito(testo: impl Display) -> Self {
        Self {
            testo: testo.to_string(),
            esito: EsitoTool::Riuscito,
            exit_code: None,
        }
    }

    /// Errore APPLICATIVO del tool. Il testo dice cosa e' successo e come
    /// correggere; il campo dice che e' andata male, e comporre il primo non
    /// tocca il secondo — che e' l'intero punto di questo tipo.
    pub fn fallito(testo: impl Display) -> Self {
        Self {
            testo: testo.to_string(),
            esito: EsitoTool::Fallito,
            exit_code: None,
        }
    }

    /// Il tool ha ESEGUITO un comando e ne riporta lo stato d'uscita.
    ///
    /// L'esito resta `Riuscito` anche con `exit_code != 0`, e non e' una svista:
    /// "il tool ha fatto il suo lavoro" e "il comando e' andato bene" sono due
    /// assi distinti, e il sistema li tiene separati da sempre — un `pnpm build`
    /// che esce 1 e' un tool RIUSCITO che riporta un build FALLITO, e chi deve
    /// giudicare il build guarda `exit_code`. Collassarli renderebbe un comando
    /// fallito indistinguibile da un tool che non e' riuscito a eseguirlo, che e'
    /// esattamente la distinzione su cui il final_gate decide se un criterio va
    /// rieseguito o se il codice va corretto.
    pub fn comando(testo: impl Display, exit_code: i32) -> Self {
        Self {
            testo: testo.to_string(),
            esito: EsitoTool::Riuscito,
            exit_code: Some(exit_code),
        }
    }

    /// PONTE per i tool non ancora migrati: ricostruisce l'esito dal testo, col
    /// marker e col formato `EXIT CODE: N`.
    ///
    /// E' l'unico punto del sistema autorizzato a farlo, ed e' DEBITO: ogni tool
    /// migrato e' una chiamata in meno, e quando non ne resta nessuna spariscono
    /// il ponte, il marker e le funzioni che lo leggono. Finche' vive, un
    /// prefisso anteposto al testo di un tool legacy ne annulla il fallimento —
    /// il difetto originale, ora circoscritto a un punto che si sa dove cercare.
    pub fn da_testo_legacy(testo: String) -> Self {
        let esito = if is_tool_failure(&testo) {
            EsitoTool::Fallito
        } else {
            EsitoTool::Riuscito
        };
        Self {
            exit_code: exit_code_legacy(&testo),
            esito,
            testo,
        }
    }
}

impl From<String> for RispostaTool {
    fn from(testo: String) -> Self {
        Self::da_testo_legacy(testo)
    }
}

/// Estrae `EXIT CODE: N` dal testo di un tool legacy. Parte del ponte, non del
/// contratto: un tool migrato porta l'exit code nel proprio campo.
fn exit_code_legacy(risultato: &str) -> Option<i32> {
    const MARKER: &str = "EXIT CODE: ";
    let start = risultato.find(MARKER)? + MARKER.len();
    let rest = &risultato[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(rest.len());
    rest[..end].trim().parse::<i32>().ok()
}

/// Marker in testa al risultato di un tool FALLITO (U+274C).
///
/// E' il segnale che `mcp-core` traduce in `ToolOutcome.is_error` e che
/// `nexus-agent-graph` legge dai `ContentBlock::ToolResult`. Valore stabile:
/// cambiarlo cambia il contratto fra produttori e consumatori, e va fatto qui.
pub const TOOL_FAILURE_MARKER: char = '\u{274C}';

/// Prefisso completo con cui un risultato dichiara il fallimento: il marker
/// seguito da uno spazio. E' cio' che [`tool_failure`] antepone al messaggio.
pub const TOOL_FAILURE_PREFIX: &str = "\u{274C} ";

/// Compone il risultato di un tool FALLITO: il marker piu' il messaggio
/// leggibile.
///
/// IDEMPOTENTE: se il messaggio porta gia' il marker (caso tipico della
/// propagazione a catena, dove un tool inoltra il fallimento di un helper) non
/// lo raddoppia. Senza questa proprieta' un errore propagato due volte
/// resterebbe riconoscibile, ma con un prefisso diverso a ogni salto.
///
/// Il messaggio e' per l'umano e per il modello; il marker e' per la macchina.
/// Nessun consumatore deve leggere il messaggio per decidere l'esito.
pub fn tool_failure(messaggio: impl Display) -> String {
    let testo = messaggio.to_string();
    if is_tool_failure(&testo) {
        return testo;
    }
    format!("{TOOL_FAILURE_PREFIX}{testo}")
}

/// `true` se il risultato testuale di un tool DICHIARA un fallimento.
///
/// Il criterio e' il marker in testa (spazi iniziali ignorati), mai il
/// contenuto del messaggio: un `read_file` RIUSCITO che restituisce un sorgente
/// contenente la parola "timeout" non e' un fallimento, e prima di questo punto
/// unico veniva contato come tale.
pub fn is_tool_failure(risultato: &str) -> bool {
    risultato.trim_start().starts_with(TOOL_FAILURE_MARKER)
}

/// Antepone una premessa al risultato di un tool CONSERVANDONE la dichiarazione
/// d'esito.
///
/// Esiste perche' il criterio di [`is_tool_failure`] e' la TESTA della stringa:
/// un chiamante che avvolge il risultato di un tool interno in una frase propria
/// (`"Servizio X riavviato.\n{risultato}"`, `"[Auto-routing] ...\n{risultato}"`)
/// spinge il marker in mezzo al testo e il fallimento smette di essere
/// riconosciuto — l'anti-loop lo vede come una ripetizione RIUSCITA e lo tratta
/// come stallo invece che come causa radice da diagnosticare (regola M).
///
/// Il marker resta in testa, la premessa lo segue: cosi' l'esito e' leggibile
/// dalla macchina e il contesto resta leggibile dall'umano. Su un risultato
/// riuscito la composizione e' la concatenazione nuda, senza inventare esiti.
pub fn prepend_preserving_failure(premessa: impl Display, risultato: &str) -> String {
    if !is_tool_failure(risultato) {
        return format!("{premessa}\n{risultato}");
    }
    // Il marker si toglie dal corpo e si rimette in testa alla composizione: un
    // secondo marker in mezzo sarebbe rumore, e lasciarlo li' senza rimetterlo
    // in testa perderebbe la dichiarazione.
    let corpo = risultato
        .trim_start()
        .trim_start_matches(TOOL_FAILURE_MARKER)
        .trim_start();
    tool_failure(format!("{premessa}\n{corpo}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_premessa_non_nasconde_il_fallimento() {
        // Il fallimento nasce dal PRODUTTORE, non da un letterale composto a
        // mano: e' l'unica forma in cui il test attraversa il contratto reale.
        let fallito = tool_failure("nessun ascolto sulla porta 24806");
        let composto = prepend_preserving_failure("Servizio 'frontend' riavviato.", &fallito);
        assert!(
            is_tool_failure(&composto),
            "la premessa ha spinto il marker fuori dalla testa: {composto}"
        );
        assert!(composto.contains("Servizio 'frontend' riavviato."));
        assert!(composto.contains("nessun ascolto sulla porta 24806"));
        // Un solo marker: quello in testa.
        assert_eq!(composto.matches(TOOL_FAILURE_MARKER).count(), 1);
    }

    #[test]
    fn su_un_risultato_riuscito_non_inventa_un_fallimento() {
        let composto = prepend_preserving_failure("Servizio 'api' riavviato.", "Avviato, pid 42");
        assert!(!is_tool_failure(&composto));
        assert_eq!(composto, "Servizio 'api' riavviato.\nAvviato, pid 42");
    }

    #[test]
    fn il_costruttore_antepone_il_marker() {
        let t = tool_failure("directory non leggibile");
        assert!(t.starts_with(TOOL_FAILURE_MARKER));
        assert!(t.ends_with("directory non leggibile"));
        // Cio' che il costruttore produce, il riconoscitore lo riconosce: e'
        // l'unica proprieta' che tiene insieme i due lati del contratto.
        assert!(is_tool_failure(&t));
    }

    #[test]
    fn il_costruttore_non_raddoppia_il_marker() {
        let una_volta = tool_failure("boom");
        let due_volte = tool_failure(&una_volta);
        assert_eq!(una_volta, due_volte);
        assert_eq!(due_volte.matches(TOOL_FAILURE_MARKER).count(), 1);
    }

    #[test]
    fn un_risultato_riuscito_non_e_un_fallimento() {
        // Il testo NOMINA un errore ma non lo dichiara: e' il contenuto di un
        // file, non l'esito del tool che l'ha letto.
        assert!(!is_tool_failure(
            "fn main() { panic!(\"error: timeout\") }"
        ));
        assert!(!is_tool_failure("Error: qualcosa"));
        assert!(!is_tool_failure(""));
    }

    #[test]
    fn il_marker_conta_solo_in_testa() {
        // In mezzo al testo non dichiara nulla: un tool riuscito che RIPORTA il
        // fallimento altrui (un elenco di esiti, un riepilogo) non e' fallito.
        assert!(!is_tool_failure(&format!(
            "3 file elaborati, 1 saltato: {TOOL_FAILURE_PREFIX}accesso negato"
        )));
        // Spazi iniziali non nascondono la dichiarazione.
        assert!(is_tool_failure(&format!("\n  {TOOL_FAILURE_PREFIX}boom")));
    }

    // ── RispostaTool: l'esito sta nel CAMPO (regola Q) ───────────────────────

    #[test]
    fn una_premessa_non_puo_nascondere_un_fallimento_dichiarato_nel_campo() {
        // E' LA prova del refactor. Con l'esito nel testo, comporre una premessa
        // davanti al risultato ne annullava la dichiarazione: `is_tool_failure`
        // guarda la testa della stringa, e due composizioni legittime del repo
        // vi anteponevano prosa di successo. Col campo, comporre il testo non
        // tocca l'esito — e non c'e' modo di sbagliare, perche' non c'e' niente
        // da ricordarsi di preservare.
        let fallita = RispostaTool::fallito("nessun ascolto sulla porta 24806");
        let composta = RispostaTool {
            testo: format!("Servizio 'frontend' riavviato.\n{}", fallita.testo),
            ..fallita
        };
        assert_eq!(
            composta.esito,
            EsitoTool::Fallito,
            "l'esito e' un dato: una premessa non lo puo' coprire"
        );

        // Il canale legacy, per confronto: la stessa composizione lo perde. Non
        // e' un difetto del ponte, e' la ragione per cui il ponte deve sparire.
        let legacy = tool_failure("nessun ascolto sulla porta 24806");
        let composta_legacy =
            RispostaTool::da_testo_legacy(format!("Servizio 'frontend' riavviato.\n{legacy}"));
        assert_eq!(
            composta_legacy.esito,
            EsitoTool::Riuscito,
            "col marker nel testo la premessa NASCONDE il fallimento: e' il              difetto che il campo elimina"
        );
    }

    #[test]
    fn un_comando_fallito_non_e_un_tool_fallito() {
        // Due assi distinti: il tool ha fatto il suo lavoro (ha eseguito e ha
        // riportato), il comando e' andato male. Collassarli renderebbe un build
        // rotto indistinguibile da un tool che non e' riuscito a lanciarlo.
        let build_rotto = RispostaTool::comando("error TS2322", 1);
        assert_eq!(build_rotto.esito, EsitoTool::Riuscito);
        assert_eq!(build_rotto.exit_code, Some(1));

        // Un'invocazione RIFIUTATA e' invece un fallimento del tool, e non ha
        // exit code perche' nessun processo e' partito: e' la coppia di campi su
        // cui il final_gate decide se rieseguire il criterio o correggere il
        // codice.
        let rifiutata = RispostaTool::fallito("[working_dir gia' applicato] togli 'cd frontend'");
        assert_eq!(rifiutata.esito, EsitoTool::Fallito);
        assert_eq!(rifiutata.exit_code, None);
    }

    #[test]
    fn il_ponte_legge_l_exit_code_del_formato_legacy() {
        let r = RispostaTool::da_testo_legacy("EXIT CODE: 0
STDOUT:
ok".to_string());
        assert_eq!(r.exit_code, Some(0));
        assert_eq!(r.esito, EsitoTool::Riuscito);

        let r = RispostaTool::da_testo_legacy("EXIT CODE: -1
STDERR:
boom".to_string());
        assert_eq!(r.exit_code, Some(-1), "gli exit negativi sono exit");

        let r = RispostaTool::da_testo_legacy("contenuto del file letto".to_string());
        assert_eq!(r.exit_code, None, "un tool non-comando non ha exit code");
    }
}

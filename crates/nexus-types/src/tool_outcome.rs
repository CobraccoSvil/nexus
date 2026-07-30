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
//! # Regola per chi scrive un tool nuovo
//!
//! Su fallimento si ritorna [`tool_failure`], mai un letterale composto a mano:
//! un tool che sceglie una forma propria e' un tool il cui fallimento nessuno
//! vede. Il guard `contratto-fallimento-tool` di `scripts/check-single-source.sh`
//! rifiuta i nuovi messaggi d'errore privi del marker.

use std::fmt::Display;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}

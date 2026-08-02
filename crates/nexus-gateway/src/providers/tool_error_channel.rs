//! Come un dialetto che NON ha il campo dichiara al modello un tool_result
//! fallito.
//!
//! # La domanda, e perche' ha un punto solo
//!
//! L'esito di un tool viaggia in un campo fino al confine col provider
//! (`LlmMessage::is_error`, regola Q). Li' i dialetti si dividono:
//!
//! - Anthropic ha `is_error` sul blocco `tool_result`: l'adapter lo emette e
//!   questo modulo non lo riguarda;
//! - OpenAI-compat (`{role:"tool", tool_call_id, content}`) e Google
//!   (`functionResponse{name, response}`) NON hanno un campo equivalente. Il
//!   solo veicolo che resta e' il testo che il modello legge.
//!
//! Inventare li' un campo di protocollo sarebbe peggio del silenzio: un campo
//! sconosciuto viene ignorato dall'API nel caso migliore e rifiutato con HTTP
//! 400 nel peggiore, e in entrambi i casi il sistema si racconta di aver
//! dichiarato l'esito. Quindi il degrado e' DICHIARATO: su quei dialetti la
//! dichiarazione e' testuale, composta QUI, dal campo.
//!
//! # Non e' un ritorno al marker nel testo
//!
//! La regola Q vieta di trasportare lo stato tecnico nel testo perche' il
//! consumatore a valle sarebbe costretto a riparsarlo. Qui il consumatore e' il
//! MODELLO, che il testo lo legge per mestiere, e la direzione e' quella
//! consentita: il testo si compone DOPO, dai campi (regola Q punto 3). Nessun
//! codice di Nexus rilegge questo prefisso per sapere com'e' andata — l'esito
//! resta nel campo per tutti i consumatori macchina, e questo modulo e'
//! l'ultimo passo prima del wire.
//!
//! Quando un dialetto acquisira' il proprio campo, si toglie la sua chiamata:
//! il punto da cambiare e' uno.

/// Prefisso con cui un tool_result dichiara il fallimento ai dialetti senza
/// campo. Vocabolario allineato all'`error_class` che il dispatch gia' assegna
/// a un errore applicativo di tool (`tool_error`), cosi' il testo che il
/// modello legge e il campo che il sistema registra dicono la stessa parola.
pub(crate) const PREFISSO_FALLIMENTO: &str = "[tool_error] ";

/// Compone il contenuto che il modello leggera' per un messaggio `role="tool"`,
/// dichiarando il fallimento quando il campo lo afferma.
///
/// Interviene SOLO su `Some(true)`: un tool riuscito non riceve decorazioni, e
/// un esito NON dichiarato (`None`) non ne inventa uno — chi non sa, tace.
///
/// IDEMPOTENTE: un testo che porta gia' il prefisso non lo raddoppia. Guardare
/// il testo per non ripetersi non e' leggerne lo stato tecnico (regola M):
/// l'esito arriva dal campo `is_error` e questa funzione non lo deduce mai dal
/// contenuto.
///
/// RIDONDANZA TRANSITORIA: un tool non ancora migrato a `RispostaTool` scrive
/// ancora il marker `U+274C` in testa al proprio testo, quindi su quei dialetti
/// il modello vedra' per un po' entrambe le dichiarazioni. Toglierla
/// richiederebbe di riconoscere il marker qui, cioe' di far dipendere il
/// confine col provider dal vocabolario legacy che la migrazione sta
/// rimuovendo: la ridondanza sparisce da se' quando l'ultimo tool e' migrato.
pub(crate) fn testo_con_esito_dichiarato(testo: String, is_error: Option<bool>) -> String {
    if is_error != Some(true) || testo.starts_with(PREFISSO_FALLIMENTO) {
        return testo;
    }
    format!("{PREFISSO_FALLIMENTO}{testo}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dichiara_il_fallimento_solo_quando_il_campo_lo_afferma() {
        assert_eq!(
            testo_con_esito_dichiarato("boom".to_string(), Some(true)),
            "[tool_error] boom"
        );
        // Riuscito: nessuna decorazione.
        assert_eq!(
            testo_con_esito_dichiarato("fatto".to_string(), Some(false)),
            "fatto"
        );
    }

    #[test]
    fn un_esito_non_dichiarato_non_ne_inventa_uno() {
        // `None` e' "non lo so", non "e' andata bene" e nemmeno "e' fallito":
        // un messaggio tool ricostruito dal sanitizer non deve acquisire una
        // dichiarazione che nessuno ha fatto.
        assert_eq!(
            testo_con_esito_dichiarato("risultato".to_string(), None),
            "risultato"
        );
    }

    #[test]
    fn non_raddoppia_la_dichiarazione() {
        let una_volta = testo_con_esito_dichiarato("boom".to_string(), Some(true));
        let due_volte = testo_con_esito_dichiarato(una_volta.clone(), Some(true));
        assert_eq!(una_volta, due_volte);
        assert_eq!(due_volte.matches(PREFISSO_FALLIMENTO).count(), 1);
    }
}

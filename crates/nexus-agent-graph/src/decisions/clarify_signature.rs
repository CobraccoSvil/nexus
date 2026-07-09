//! `clarify_signature`: firma PURA di una domanda-chiarimento posta all'utente,
//! per la loop-detection CROSS-RUN delle domande ripetute (asse
//! [`crate::decisions::progress_controller::Axis::RepeatedUserQuestion`]).
//!
//! PUNTO UNICO (regola L) della firma-domanda: sia l'impl DB della porta
//! [`crate::runtime::ports::ClarifyHistoryPort`] (che firma le domande STORICHE
//! dei meta_step `kind='clarify'` della sessione) sia il call site che passa la
//! domanda CORRENTE devono usare QUESTA funzione, cosi' due domande "uguali"
//! collidono in modo deterministico (analogo di `name|sha1` per i tool, vedi
//! [`crate::decisions::loop_signatures::build_signature`]).
//!
//! REGOLA M: la firma-testo e' SOLO un'euristica di loop-detection (decide se e'
//! la STESSA domanda ripetuta). La DECISIONE di contare deriva dal segnale
//! strutturato — l'esistenza di un meta_step `kind='clarify'` — non da questa
//! firma. Due domande semanticamente identiche ma lessicalmente diverse non
//! collidono (limite accettato, vedi trade-off nel piano): la similarita'
//! semantica e' un miglioramento successivo.

use sha1::{Digest, Sha1};

/// Normalizza il testo di una domanda per la firma di loop-detection: `trim` +
/// lowercase + collasso di ogni sequenza di whitespace (spazi/tab/newline) in un
/// singolo spazio. Robusto alle differenze di formattazione (indentazione,
/// a-capo, spazi doppi) che non cambiano la domanda. Deterministica e pura.
///
/// NON rimuove punteggiatura ne' parole (troppo aggressivo -> falsi positivi tra
/// domande diverse): il collasso whitespace + lowercase e' l'euristica minima
/// che riconosce la ri-emissione della STESSA domanda (il caso email: identica
/// ad ogni giro).
pub fn normalize_question(question: &str) -> String {
    question
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Firma di una domanda-chiarimento: sha1 esadecimale (12 char) della domanda
/// NORMALIZZata. Stesso schema di [`crate::decisions::loop_signatures::build_signature`]
/// (primi 6 byte -> 12 char hex lowercase), senza il prefisso `name|` (qui non
/// c'e' un nome-tool: la firma e' della sola domanda).
///
/// Una domanda vuota/whitespace-only normalizza a `""`: la sua firma e' quella
/// della stringa vuota (deterministica). Il call site NON deve firmare una
/// domanda vuota (nessun clarify e' stato posto): e' compito del chiamante
/// evitarlo (vedi il gate `question.is_empty()` in `clarify_or_expand`).
pub fn clarify_signature(question: &str) -> String {
    let normalized = normalize_question(question);
    let mut hasher = Sha1::new();
    hasher.update(normalized.as_bytes());
    let digest = hasher.finalize();
    let mut hex12 = String::with_capacity(12);
    for byte in digest.iter().take(6) {
        hex12.push_str(&format!("{byte:02x}"));
    }
    hex12
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collassa_whitespace_e_lowercase() {
        assert_eq!(
            normalize_question("  Qual e'  la\tTUA\n email? "),
            "qual e' la tua email?"
        );
    }

    #[test]
    fn firma_stabile_su_stessa_domanda_variando_spazi() {
        // La STESSA domanda con formattazione diversa deve produrre la STESSA
        // firma (e' il cuore della loop-detection cross-run: l'email ri-chiesta
        // ad ogni giro e' lessicalmente identica a meno di spazi/case).
        let a = clarify_signature("Qual e' la tua email di login?");
        let b = clarify_signature("  qual e' la TUA   email di login?  ");
        assert_eq!(a, b, "domanda uguale a meno di spazi/case -> firma uguale");
    }

    #[test]
    fn firma_diversa_su_domande_diverse() {
        let a = clarify_signature("Qual e' la tua email?");
        let b = clarify_signature("Quale database vuoi usare?");
        assert_ne!(a, b, "domande diverse -> firme diverse");
    }

    #[test]
    fn firma_e_hex12() {
        let sig = clarify_signature("test");
        assert_eq!(sig.len(), 12, "12 char esadecimali (6 byte)");
        assert!(
            sig.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hex lowercase"
        );
    }

    #[test]
    fn domanda_vuota_normalizza_a_stringa_vuota() {
        assert_eq!(normalize_question("   \t\n  "), "");
        // firma deterministica della stringa vuota (sha1(""))
        assert_eq!(clarify_signature(""), clarify_signature("   "));
    }
}

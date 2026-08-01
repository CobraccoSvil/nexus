//! Riconoscimento degli errori del DATABASE dal loro CODICE, mai dal messaggio.
//!
//! Un `sqlx::Error` porta due cose molto diverse: un codice SQLSTATE, che
//! Postgres garantisce ed e' identico su ogni installazione, e un `Display` per
//! l'umano che dipende da `lc_messages` del server. Chiunque decida qualcosa
//! leggendo il secondo sta scrivendo codice che funziona su una macchina e tace
//! sull'altra (regola M).
//!
//! Non e' una fragilita' teorica. MISURATO il 01/08/2026 sul Postgres di questo
//! ambiente, che risponde in italiano: «un valore chiave duplicato viola il
//! vincolo univoco "..."». Ne' `unique` ne' `duplicate` compaiono in quella
//! frase, quindi ogni `contains` su quelle parole era gia' cieco — non "a
//! rischio di diventarlo". Un profilo con nome gia' preso rispondeva 500
//! «errore interno» invece del 409 che dice all'utente di cambiare nome.
//!
//! Vive in `nexus-types` perche' i lati che devono condividere il criterio non
//! si vedono fra loro: `mcp-core`, `admin-service` e i DTO di questo stesso
//! crate. Prima erano tre copie, di cui almeno una cieca per locale.

/// `true` se l'errore e' una violazione di vincolo di unicita' (SQLSTATE 23505).
///
/// Delega al driver, che il codice ce l'ha gia' tipizzato: non si compone il
/// confronto a mano e non si guarda il messaggio. Un errore che non viene dal
/// database (I/O, pool esaurito, decodifica) non e' un conflitto e risponde
/// `false` — cio' che il chiamante deve fare in quel caso e' un'altra domanda,
/// e va decisa da lui.
pub fn is_unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .is_some_and(|db| db.is_unique_violation())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Il criterio e' il CODICE: un errore che non viene dal database non e' un
    /// conflitto, per quanto il suo testo possa somigliargli.
    ///
    /// MUTAZIONE: sostituendo il corpo con
    /// `e.to_string().to_lowercase().contains("unique")` questo test rosseggia,
    /// perche' il messaggio inventato qui contiene la parola e il codice no.
    #[test]
    fn il_testo_non_fa_un_conflitto() {
        let non_db = sqlx::Error::Protocol("unique constraint chatter".into());
        assert!(
            !is_unique_violation(&non_db),
            "senza SQLSTATE non c'e' conflitto, qualunque cosa dica il messaggio"
        );
        assert!(!is_unique_violation(&sqlx::Error::RowNotFound));
    }
}

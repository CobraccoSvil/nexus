//! «Questi due contenuti differiscono solo nei fine-riga?»
//!
//! # Perche' e' un punto unico
//!
//! Il difetto si e' presentato **tre volte**, ogni volta in un posto diverso:
//!
//! 1. 2026-07-02, migrazione 0500: checkout CRLF, checksum diverso, mcp-core
//!    rifiuta di avviarsi (citato in `.gitattributes`);
//! 2. 2026-08-05, migrazioni 117/118: il registro conservava l'hash dei byte
//!    CRLF e respingeva **ogni albero conforme** (citato in `check-eol.sh`);
//! 3. 2026-08-05, `correction_progress`: il ciclo di correzione confronta
//!    `before_sha256 != after_sha256` per decidere se un rimando ha prodotto
//!    progresso — e una riscrittura che cambia i soli fine-riga ha hash diversi,
//!    quindi passa per **lavoro fatto**.
//!
//! Sopravvive perche' git lo nasconde: con `core.autocrlf=true` ne' `git diff`
//! ne' `git status` mostrano niente, e a vedere la differenza e' solo chi legge
//! i byte.
//!
//! # Perche' CLASSIFICA invece di normalizzare
//!
//! Un `.replace("\r\n", "\n")` sparso nei confronti sarebbe la toppa (regola H)
//! che rende il difetto di nuovo invisibile — la stessa proprieta' per cui e'
//! sopravvissuto tre volte. E sarebbe **sbagliato**: i chiamanti pongono domande
//! diverse sugli stessi byte.
//!
//! - Per una migrazione applicata, i byte SONO il contratto: `SoloFineRiga` e'
//!   un difetto da riparare, e nel verso giusto (si ricrea il file, mai il
//!   registro).
//! - Per il progresso di una correzione, `SoloFineRiga` significa **niente e'
//!   cambiato**.
//! - Per «qualcuno ha toccato questo file dopo?» (`mutations_api`), perfino un
//!   fine-riga diverso e' una modifica, e li' non si normalizza affatto.
//!
//! Il verdetto e' un enum e non un `bool` (regola Q): tre casi vogliono tre
//! risposte, e collassarli costringerebbe ogni chiamante a indovinare quale.
//!
//! # Dove sta
//!
//! Qui e non in un crate nuovo perche' i due consumatori con i byte in mano —
//! `registro` (in questo crate) e `mcp-core::file_mutations` — lo raggiungono
//! entrambi senza nuove dipendenze: `mcp-core` dipende gia' da
//! `nexus-migrations`. La direzione resta quella giusta: il migrator non
//! dipende da nessuno dei due.

/// L'esito del confronto fra due sequenze di byte, dal punto di vista dei
/// fine-riga.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EsitoFineRiga {
    /// Byte identici: non c'e' niente da decidere.
    Identici,
    /// Stesso contenuto, convenzione di fine-riga diversa. **La prova e'
    /// costruttiva**: la variante e' stata generata e confrontata, non dedotta
    /// da un'euristica sul numero di byte di differenza.
    SoloFineRiga,
    /// Il contenuto e' un altro.
    ContenutoDiverso,
}

impl EsitoFineRiga {
    /// Il contenuto e' cambiato davvero? `SoloFineRiga` risponde **no**.
    ///
    /// Comodita' per il chiamante che ha bisogno del solo si'/no — non un
    /// sostituto dell'enum: chi deve *riparare* ha bisogno di sapere quale dei
    /// tre casi sia, e da un `bool` non lo ricava.
    pub const fn contenuto_cambiato(self) -> bool {
        matches!(self, Self::ContenutoDiverso)
    }
}

/// La stessa sequenza con i fine-riga normalizzati a LF.
pub fn a_lf(byte: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(byte.len());
    let mut i = 0;
    while i < byte.len() {
        if byte[i] == b'\r' && byte.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(byte[i]);
        i += 1;
    }
    out
}

/// La stessa sequenza con i fine-riga a CRLF.
///
/// Passa per la forma LF anche in andata, cosi' un file gia' misto non produce
/// `\r\r\n`.
pub fn a_crlf(byte: &[u8]) -> Vec<u8> {
    let lf = a_lf(byte);
    let mut out = Vec::with_capacity(lf.len());
    for b in lf {
        if b == b'\n' {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
}

/// Il verdetto sul confronto fra due contenuti.
pub fn classifica_contenuto(a: &[u8], b: &[u8]) -> EsitoFineRiga {
    if a == b {
        return EsitoFineRiga::Identici;
    }
    if a_lf(a) == a_lf(b) {
        return EsitoFineRiga::SoloFineRiga;
    }
    EsitoFineRiga::ContenutoDiverso
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_identici() {
        assert_eq!(classifica_contenuto(b"a\nb", b"a\nb"), EsitoFineRiga::Identici);
    }

    #[test]
    fn solo_i_fine_riga_cambiano() {
        assert_eq!(
            classifica_contenuto(b"a\nb\n", b"a\r\nb\r\n"),
            EsitoFineRiga::SoloFineRiga
        );
        // E' il caso di `correction_progress`: gli hash differirebbero, ma il
        // contenuto no — e passerebbe per lavoro fatto.
        assert!(!classifica_contenuto(b"a\nb\n", b"a\r\nb\r\n").contenuto_cambiato());
    }

    #[test]
    fn un_contenuto_diverso_resta_diverso() {
        assert_eq!(
            classifica_contenuto(b"a\nb", b"a\nc"),
            EsitoFineRiga::ContenutoDiverso
        );
        assert!(classifica_contenuto(b"a\nb", b"a\nc").contenuto_cambiato());
    }

    #[test]
    fn un_file_misto_non_diventa_cr_doppio() {
        // Entrata gia' mista: la conversione a CRLF non deve produrre `\r\r\n`.
        let misto = b"a\r\nb\nc";
        let crlf = a_crlf(misto);
        assert!(!crlf.windows(2).any(|w| w == b"\r\r"));
        assert_eq!(a_lf(&crlf), a_lf(misto));
    }

    #[test]
    fn il_vuoto_non_e_un_caso_speciale() {
        assert_eq!(classifica_contenuto(b"", b""), EsitoFineRiga::Identici);
        assert_eq!(classifica_contenuto(b"", b"x"), EsitoFineRiga::ContenutoDiverso);
    }
}

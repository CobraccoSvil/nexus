//! `severity`: PUNTO UNICO (regola L) del vocabolario di GRAVITA' degli elementi
//! di evidenza dei panel (rischi del consiglio, finding della review, rischi del
//! debate).
//!
//! Perche' esiste: il test "questo elemento porta evidenza GRAVE?" — che decide
//! il veto avversario in minoranza — era implementato a mano in ogni panel
//! (`advisory_panel` sui `risks[].severity`, `adversarial_review` sui
//! `findings[].severity`), e il debate sarebbe stata la terza copia dello stesso
//! confronto letterale. Un nuovo livello o un sinonimo avrebbe richiesto di
//! toccare lo stesso `match` in N posti: sintomo che il punto unico mancava.
//!
//! Confine con [`super::panel_quorum`]: li' vive la PRECEDENZA fra gli esiti a
//! partire da un conteggio gia' fatto (soglia -> veto -> condizionale -> approva),
//! deliberatamente indipendente dal formato dei dati. Qui vive il vocabolario
//! della gravita' e la sua lettura dal JSON strutturato. Concern disgiunti.
//!
//! Regola M: legge il campo STRUTTURATO `severity`, mai la prosa della
//! descrizione. Funzioni PURE: nessun I/O, replay-stabili.
//!
//! Vocabolario canonico (regola N): `alta` > `media` > `bassa`. E' il vocabolario
//! dichiarato dagli schemi dei tool `advisory_verdict` / `review_verdict` /
//! `debate_position` e dai prompt delle figure: si estende QUI e negli schemi,
//! mai con un sinonimo accettato in un solo call site.

use serde_json::Value;

/// Livello di gravita' canonico di un elemento di evidenza.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Evidenza grave: da sola giustifica il veto avversario in minoranza.
    High,
    Medium,
    Low,
}

impl Severity {
    /// Parse canonico del campo `severity` (trim + case-insensitive).
    /// Qualunque altro valore -> `None` (gravita' ignota: non e' `bassa`, e' un
    /// dato che non sappiamo leggere — chi ordina la mette in fondo, chi decide
    /// il veto non la considera evidenza grave).
    pub fn try_parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "alta" => Some(Self::High),
            "media" => Some(Self::Medium),
            "bassa" => Some(Self::Low),
            _ => None,
        }
    }

    /// Etichetta canonica (regola N). `const` cosi' i vocabolari esposti come
    /// liste (es. `VALID_FINDING_SEVERITIES`, confrontato col catalogo dei tool
    /// dal test di coerenza cross-crate) si DERIVINO da qui invece di ripetere i
    /// letterali.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::High => "alta",
            Self::Medium => "media",
            Self::Low => "bassa",
        }
    }
}

/// Gravita' dichiarata da UN elemento di evidenza (rischio o finding), letta dal
/// campo strutturato `severity`. `None` = campo assente o vocabolario ignoto.
pub fn severity_of(item: &Value) -> Option<Severity> {
    Severity::try_parse(item.get("severity").and_then(Value::as_str)?)
}

/// `true` se l'elemento porta evidenza GRAVE. PUNTO UNICO del test che abilita
/// il veto avversario in minoranza (`block`/`fail` con evidenza vince anche da
/// solo, vedi [`super::panel_quorum::classify_panel`]).
pub fn is_high(item: &Value) -> bool {
    severity_of(item) == Some(Severity::High)
}

/// `true` se ALMENO un elemento della lista porta evidenza grave. Forma usata da
/// tutti i panel (rischi del consiglio, finding della review, rischi del debate).
pub fn any_high(items: &[Value]) -> bool {
    items.iter().any(is_high)
}

/// `true` se ALMENO un elemento raggiunge la gravita' `minima`: e' il test di
/// SOSTEGNO di un verdetto che chiede lavoro.
///
/// Un revisore dichiara due cose nello stesso oggetto — il verdetto e l'evidenza
/// che lo sostiene — e le due possono non reggersi a vicenda. Misurato il
/// 01/08/2026 su bacheca-attivita (run 397c0824): due revisori, uno vota `pass`
/// con zero finding, l'altro `needs_changes` con UN finding di gravita' `bassa`
/// il cui testo dice "Not a blocker" e "Codice accettabile" su uno scenario che
/// il revisore stesso dichiara impossibile ("SELECT COUNT(*) never returns
/// NULL"). Il panel ha prodotto NeedsChanges, il ciclo di correzione e' girato a
/// vuoto due volte e il run e' chiuso `failed_diagnosed` — con l'applicazione
/// funzionante.
///
/// E' il corollario della regola Q: una struttura non rende vera l'affermazione
/// che contiene. Il campo `verdict` e' una DICHIARAZIONE; i `findings` sono cio'
/// che il revisore porta a sostegno, ed e' l'unica parte che qualcun altro puo'
/// pesare. Quando il sostegno manca, a valere e' l'evidenza — non perche' il
/// revisore menta, ma perche' un verdetto non sostenuto non e' distinguibile da
/// un'abitudine a chiedere sempre una modifica in piu'.
///
/// Gravita' IGNOTA (`None`) non sostiene: se cosi' non fosse, un `severity`
/// scritto male varrebbe piu' di uno scritto `bassa`.
pub fn any_at_least(items: &[Value], minima: Severity) -> bool {
    items
        .iter()
        .any(|i| severity_of(i).is_some_and(|s| s <= minima))
}

/// Rank per l'ordinamento dei rischi/finding: piu' basso = piu' grave, cosi' un
/// sort ASCENDENTE (stabile) porta le `alta` in cima e lascia le gravita' ignote
/// in fondo senza perderle.
pub fn rank(item: &Value) -> u8 {
    match severity_of(item) {
        Some(Severity::High) => 0,
        Some(Severity::Medium) => 1,
        Some(Severity::Low) => 2,
        None => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_canonico_e_tollerante_a_case_e_spazi() {
        assert_eq!(Severity::try_parse(" Alta "), Some(Severity::High));
        assert_eq!(Severity::try_parse("MEDIA"), Some(Severity::Medium));
        assert_eq!(Severity::try_parse("bassa"), Some(Severity::Low));
    }

    #[test]
    fn vocabolario_ignoto_non_e_bassa() {
        // Regola M: un valore che non sappiamo leggere NON viene declassato a
        // "poco grave" per comodita' — resta ignoto e va in fondo all'ordine.
        assert_eq!(Severity::try_parse("critical"), None);
        assert_eq!(Severity::try_parse(""), None);
        assert_eq!(rank(&json!({"severity": "critical"})), 3);
        assert!(!is_high(&json!({"severity": "critical"})));
    }

    #[test]
    fn is_high_solo_su_alta() {
        assert!(is_high(&json!({"severity": "alta"})));
        assert!(!is_high(&json!({"severity": "media"})));
        assert!(!is_high(&json!({"description": "senza severity"})));
    }

    #[test]
    fn any_high_su_lista_mista() {
        let items = vec![
            json!({"severity": "bassa"}),
            json!({"severity": "alta"}),
            json!({"severity": "media"}),
        ];
        assert!(any_high(&items));
        let no_high = vec![json!({"severity": "media"}), json!({"severity": "bassa"})];
        assert!(!any_high(&no_high));
        assert!(!any_high(&[]));
    }

    #[test]
    fn rank_ordina_alta_prima_ignota_in_fondo() {
        let mut items = [
            json!({"severity": "bassa"}),
            json!({"severity": "boh"}),
            json!({"severity": "alta"}),
            json!({"severity": "media"}),
        ];
        items.sort_by_key(rank);
        let sevs: Vec<&str> = items
            .iter()
            .map(|i| i.get("severity").and_then(Value::as_str).unwrap_or(""))
            .collect();
        assert_eq!(sevs, vec!["alta", "media", "bassa", "boh"]);
    }
}

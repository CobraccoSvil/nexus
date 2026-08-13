//! «Che cosa il gateway ha DICHIARATO di stare rifiutando, e per quanto» —
//! il contratto del blocco `details.failures` letto dai DUE lati del confine.
//!
//! ## Il difetto che questo modulo chiude
//!
//! Lo stato «questo fornitore e' utilizzabile adesso?» aveva DUE risposte in
//! DUE processi, e chi SCEGLIE consultava quella che non sapeva nulla:
//!
//!   - il gateway tiene il proprio `CooldownManager` (in-memory, suo processo)
//!     e vi scrive quando riceve un 429, un `Retry-After` o un billing error —
//!     cioe' impara dalle chiamate VERE;
//!   - mcp-core tiene `provider_cooldown` (in-memory, altro processo) e la
//!     selezione dei modelli esclude i fornitori che vi trova
//!     (`EligibilityFilter::apply_cooldown`) — ma quel registro imparava solo
//!     dal proprio probe periodico e dal pannello provider, MAI dai rifiuti che
//!     il gateway gli comunicava a ogni chiamata.
//!
//! Il segnale attraversava gia' il confine, tipizzato (`details.primary_cause`
//! + `details.failures[].class`), e un consumatore lo leggeva gia' per decidere
//! il failover (`classify_gateway_error`): nessuno pero' ne traeva la
//! conseguenza sul registro locale, quindi mcp-core continuava a CONVOCARE
//! fornitori che il gateway avrebbe rifiutato.
//!
//! MISURATO il 12/08/2026 sul gate duale dopo il deploy delle 22:50: tre
//! validatori convocati, `openai`, `kimi` e `openrouter`, e tutti e tre
//! astenuti con causa `cooldown` — il gateway li stava rifiutando senza
//! chiamarli, mentre la selezione di mcp-core li aveva appena scelti come i
//! migliori disponibili.
//!
//! Prima del fix del ripristino (commit `119acd55`) la desincronizzazione
//! durava al massimo un ciclo di re-probe del gateway (10 minuti), perche' era
//! il gateway stesso a cancellare i propri cooldown troppo presto. Ora che una
//! scadenza dichiarata dal fornitore viene onorata fino in fondo — che e' il
//! comportamento giusto — la cecita' di mcp-core dura quanto il cooldown: per
//! un `Retry-After` di groq sono ~1800s, per un billing 6h. Il fix precedente
//! non ha creato questo difetto: lo ha reso strutturale, e quindi visibile.
//!
//! ## Perche' il vocabolario sta QUI e non nei due lati
//!
//! Le CLASSI e i nomi dei campi sono il contratto, e un contratto scritto due
//! volte diverge in silenzio: un rename da un lato lascerebbe l'altro a leggere
//! una chiave che non esiste piu', cioe' un trasporto muto con tutti i test
//! verdi da entrambe le parti (regola O). Vivendo in `nexus-types` — da cui
//! ENTRAMBI i crate dipendono gia' — a rompersi e' la compilazione, non
//! l'esercizio.

use serde_json::Value;

/// Le classi con cui il gateway qualifica il fallimento di UN fornitore della
/// chain. Vocabolario CHIUSO e canonico (regola N): identificatori in inglese,
/// scritti da `nexus-gateway` e letti da `mcp-core`.
pub mod classe {
    /// La chiamata e' stata fatta e il fornitore ha risposto «niente credito».
    pub const BILLING: &str = "billing";
    /// Il fornitore e' stato SALTATO senza chiamarlo: cooldown di credito attivo.
    pub const COOLDOWN_BILLING: &str = "cooldown_billing";
    /// Il fornitore e' stato SALTATO senza chiamarlo: cooldown transitorio attivo.
    pub const COOLDOWN: &str = "cooldown";
    /// La chiamata e' stata fatta ed e' fallita per una causa transitoria
    /// (429, 5xx, timeout del tentativo): il gateway ha marcato un cooldown breve.
    pub const TRANSIENT: &str = "transient";
    /// Errore deterministico della richiesta: ritentarla non cambia nulla, e il
    /// fornitore e' sano.
    pub const CLIENT_ERROR: &str = "client_error";
    /// Richiesta troppo grande per QUESTO modello: un altro puo' accettarla.
    pub const CONTEXT_TOO_LONG: &str = "context_too_long";
    /// HTTP 200 senza output utile: il fornitore e' sano, il turno improduttivo.
    pub const EMPTY_COMPLETION: &str = "empty_completion";
}

/// Chiavi del blocco `details` che il gateway compone e mcp-core rilegge.
pub mod chiave {
    /// Array dei fallimenti per fornitore, in ordine di tentativo.
    pub const FAILURES: &str = "failures";
    /// Classe del fallimento PRIMARIO (il primo dell'array).
    pub const PRIMARY_CAUSE: &str = "primary_cause";
    pub const CLASSE: &str = "class";
    pub const PROVIDER: &str = "provider";
    pub const MODELLO: &str = "model";
    /// Secondi che il gateway dichiara di dover ancora attendere su quel
    /// fornitore. E' il RESIDUO letto dal registro che lo ha appena scritto,
    /// non una durata ricalcolata: qualunque ramo abbia messo il cooldown, il
    /// numero descrive lo stato vero al momento della risposta.
    pub const ATTESA_S: &str = "retry_after_seconds";
}

/// Cio' che il gateway ha dichiarato di stare rifiutando, in forma su cui si
/// puo' decidere (regola Q: l'esito nel campo, mai nella prosa — il residuo
/// viveva dentro `"in cooldown, {secs}s rimanenti"`, dove per leggerlo serviva
/// una regex).
///
/// La PORTATA e' il FORNITORE in entrambe le varianti, e non e' una
/// semplificazione: e' fedelta'. Il `CooldownManager` del gateway ragiona per
/// account — quando esclude, esclude tutti i modelli di quel fornitore — e cio'
/// che questo tipo rappresenta non e' «che limite ha imposto il fornitore» ma
/// «che cosa il gateway rifiutera' se glielo richiedo». Registrare la sola
/// coppia fornitore+modello renderebbe mcp-core piu' permissivo del gateway, e
/// la convocazione successiva finirebbe di nuovo contro un rifiuto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EsclusioneDichiarata {
    /// Fuori per credito. La DURATA non viaggia: quanto tenerlo fuori lo decide
    /// chi ha un ciclo di guarigione (in mcp-core il probe-then-reenable, che
    /// libera appena il credito torna), non chi trasporta la notizia.
    Credito { provider: String },
    /// Fuori per un tempo che il gateway ha DICHIARATO.
    Attesa { provider: String, secondi: u64 },
    /// Niente da propagare al registro locale: il fallimento non parla della
    /// disponibilita' del fornitore (errore di richiesta, contesto, turno
    /// vuoto), oppure parla di un'attesa di cui non e' stata dichiarata la
    /// durata.
    Nessuna,
}

impl EsclusioneDichiarata {
    /// Il criterio, PURO. `attesa_s` assente su una classe di attesa produce
    /// [`EsclusioneDichiarata::Nessuna`] e non una durata di ripiego: un
    /// gateway che non parla questa versione del contratto non autorizza a
    /// inventare per quanto tempo un fornitore resti fuori (regola G: niente
    /// magic fallback). L'errore cade dalla parte del convocare una volta di
    /// troppo, mai dell'escludere per un tempo che nessuno ha dichiarato.
    pub fn dal_fallimento(classe: &str, provider: &str, attesa_s: Option<u64>) -> Self {
        let provider = provider.trim();
        if provider.is_empty() {
            return Self::Nessuna;
        }
        match classe.trim() {
            classe::BILLING | classe::COOLDOWN_BILLING => Self::Credito {
                provider: provider.to_lowercase(),
            },
            classe::COOLDOWN | classe::TRANSIENT => match attesa_s {
                Some(s) if s > 0 => Self::Attesa {
                    provider: provider.to_lowercase(),
                    secondi: s,
                },
                _ => Self::Nessuna,
            },
            _ => Self::Nessuna,
        }
    }

    /// Legge il fallimento PRIMARIO dal blocco `details` di una risposta
    /// d'errore del gateway.
    ///
    /// Il primario e' `failures[0]`, lo STESSO elemento da cui il gateway
    /// deriva `primary_cause`: causa ed esclusione descrivono percio' sempre il
    /// medesimo fallimento. Si legge dall'array e non da `primary_cause`
    /// perche' li' il nome del fornitore non c'e', e un'esclusione senza il
    /// fornitore non e' registrabile.
    ///
    /// Vive qui, accanto al vocabolario, ed e' il LETTORE che il produttore usa
    /// nei propri test: cosi' il json che il gateway compone e' verificato
    /// contro la funzione che lo consumera' davvero, invece che contro
    /// un'imitazione scritta nel test (regola O).
    pub fn dal_blocco_details(details: Option<&Value>) -> Self {
        let Some(primo) = details
            .and_then(|d| d.get(chiave::FAILURES))
            .and_then(|f| f.as_array())
            .and_then(|a| a.first())
        else {
            return Self::Nessuna;
        };
        let stringa = |k: &str| primo.get(k).and_then(|v| v.as_str()).unwrap_or_default();
        Self::dal_fallimento(
            stringa(chiave::CLASSE),
            stringa(chiave::PROVIDER),
            primo.get(chiave::ATTESA_S).and_then(|v| v.as_u64()),
        )
    }

    /// Il fornitore nominato, quando c'e' un'esclusione da registrare.
    pub fn provider(&self) -> Option<&str> {
        match self {
            Self::Credito { provider } | Self::Attesa { provider, .. } => Some(provider),
            Self::Nessuna => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn le_classi_di_credito_producono_esclusione_per_credito() {
        for c in [classe::BILLING, classe::COOLDOWN_BILLING] {
            assert_eq!(
                EsclusioneDichiarata::dal_fallimento(c, "Anthropic", Some(120)),
                EsclusioneDichiarata::Credito {
                    provider: "anthropic".into()
                },
                "classe {c}: la durata non partecipa, il credito non scade col timer del gateway"
            );
        }
    }

    #[test]
    fn le_classi_di_attesa_pretendono_una_durata_dichiarata() {
        assert_eq!(
            EsclusioneDichiarata::dal_fallimento(classe::COOLDOWN, "groq", Some(1800)),
            EsclusioneDichiarata::Attesa {
                provider: "groq".into(),
                secondi: 1800
            }
        );
        // Nessuna durata dichiarata: non si inventa per quanto tempo escludere.
        assert_eq!(
            EsclusioneDichiarata::dal_fallimento(classe::TRANSIENT, "groq", None),
            EsclusioneDichiarata::Nessuna
        );
        assert_eq!(
            EsclusioneDichiarata::dal_fallimento(classe::COOLDOWN, "groq", Some(0)),
            EsclusioneDichiarata::Nessuna
        );
    }

    #[test]
    fn le_classi_che_non_parlano_di_disponibilita_non_escludono_nessuno() {
        for c in [
            classe::CLIENT_ERROR,
            classe::CONTEXT_TOO_LONG,
            classe::EMPTY_COMPLETION,
            "classe_che_non_esiste",
        ] {
            assert_eq!(
                EsclusioneDichiarata::dal_fallimento(c, "openai", Some(600)),
                EsclusioneDichiarata::Nessuna,
                "classe {c}: il fornitore e' sano, escluderlo sarebbe un danno"
            );
        }
    }

    #[test]
    fn senza_fornitore_non_si_registra_nulla() {
        assert_eq!(
            EsclusioneDichiarata::dal_fallimento(classe::COOLDOWN, "   ", Some(60)),
            EsclusioneDichiarata::Nessuna
        );
    }

    #[test]
    fn il_lettore_prende_il_primario_e_regge_i_details_incompleti() {
        let details = json!({
            chiave::PRIMARY_CAUSE: classe::COOLDOWN,
            chiave::FAILURES: [
                { chiave::PROVIDER: "kimi", chiave::CLASSE: classe::COOLDOWN, chiave::ATTESA_S: 300 },
                { chiave::PROVIDER: "openai", chiave::CLASSE: classe::BILLING },
            ],
        });
        assert_eq!(
            EsclusioneDichiarata::dal_blocco_details(Some(&details)),
            EsclusioneDichiarata::Attesa {
                provider: "kimi".into(),
                secondi: 300
            },
            "il primario e' failures[0], lo stesso da cui nasce primary_cause"
        );

        // Gateway che non parla questa versione del contratto: campo assente.
        let senza_campo = json!({
            chiave::FAILURES: [{ chiave::PROVIDER: "kimi", chiave::CLASSE: classe::COOLDOWN }],
        });
        assert_eq!(
            EsclusioneDichiarata::dal_blocco_details(Some(&senza_campo)),
            EsclusioneDichiarata::Nessuna
        );

        assert_eq!(
            EsclusioneDichiarata::dal_blocco_details(None),
            EsclusioneDichiarata::Nessuna
        );
        assert_eq!(
            EsclusioneDichiarata::dal_blocco_details(Some(&json!({ chiave::FAILURES: [] }))),
            EsclusioneDichiarata::Nessuna
        );
    }
}

//! Punto unico (regola L): «la decisione di chiarimento su QUESTO mandato e'
//! gia' stata presa — e da chi?»
//!
//! La domanda non era ponibile, e la conseguenza e' che veniva pagata una volta
//! per FIGLIO invece che una volta per DOMANDA.
//!
//! ## Il fatto (18/08/2026, progetto `app-libri-18-08`, run `abdbc7c4`)
//!
//! Il Consiglio convoca otto figure e il nodo
//! [`crate::nodes::ClarifyOrExpandNode`] gira per ognuna: otto chiamate al
//! modello in tre secondi, 7715 token, tutte sullo stesso fornitore, e la nona
//! riceve il 429 causato dalle otto precedenti. Ogni figura paga anche una
//! `list_files` propria per il contesto di progetto.
//!
//! Le otto figure non stavano rispondendo a otto domande. Misurato sul DB del
//! progetto (`nexus_subagent_runs`, `parent_run_id='abdbc7c4-…'`):
//!
//! | figure | `md5(task_description)` | `md5(context_blob)` | `md5(expected_format)` |
//! |---|---|---|---|
//! | 6 del Consiglio | `2b70cd10…` | vuoto | `ddf3298d…` |
//! | 2 `provider_analyst` | `2b70cd10…` | `00ddb047…` / `21949e6b…` | `ab50abb6…` |
//!
//! Le sei del Consiglio hanno un mandato BYTE-IDENTICO: sei chiamate per una
//! domanda sola. Le due `provider_analyst` NO — stesso task, contesto diverso —
//! e sono la ragione per cui l'identita' della decisione e' il TESTO e non la
//! convocazione (vedi sotto).
//!
//! ## Perche' l'identita' e' (CONVOCAZIONE, TESTO) e non una delle due
//!
//! - **Non la sola convocazione.** Cio' su cui il modello decide e' il
//!   messaggio che riceve, e quel messaggio e' il mandato INTERO: task piu'
//!   `context_blob` piu' `expected_format` ([`super::clarify_or_expand`] legge
//!   l'ultimo messaggio umano, che per un sub-run e' `initial_msg`). Una sola
//!   decisione per convocazione darebbe alle due `provider_analyst` la risposta
//!   data sul contesto dell'altra: e' la domanda posta a un testo, non alla
//!   figura, e due testi diversi sono due domande.
//! - **Non il solo testo.** Una memoizzazione globale sul testo sarebbe una
//!   cache: risponderebbe anche a chi non ha nulla a che vedere con questa
//!   convocazione, e la decisione non avrebbe piu' un proprietario. Qui il
//!   proprietario e' la CONVOCAZIONE — il run che ha convocato le figure — ed e'
//!   il motivo per cui la chiave lo porta dentro.
//!
//! Il testo entra nella chiave COSI' COM'E' (trimmato), senza normalizzazioni:
//! e' esattamente cio' che finisce nella richiesta al modello, quindi chiave
//! uguale significa richiesta uguale. Una normalizzazione lo renderebbe un
//! «quasi uguale» deciso da noi.
//!
//! ## Il contesto di progetto sta dentro la decisione, non accanto
//!
//! La richiesta al modello porta anche il blocco `CONTESTO PROGETTO`, prodotto
//! da una `list_files` sulla radice. E' un fatto del PROGETTO, non della figura:
//! le figure di una convocazione condividono sessione e radice, quindi il blocco
//! e' lo stesso per tutte. Sta dentro cio' che si paga una volta sola — le otto
//! `list_files` diventano una — e non e' un secondo criterio da tenere allineato.
//!
//! ## Perche' una memoizzazione e non un valore calcolato prima del fan-out
//!
//! La strada che sembra ovvia — decidere PRIMA di spawnare, come
//! `convene_council` gia' fa per l'assegnazione dei fornitori — non e'
//! percorribile qui senza spostare la chiamata al modello fuori dal nodo: la
//! decisione dipende dai gate che il nodo applica sullo stato del run
//! ([`super::ClarifyOrExpandNode::pre_llm_gate`]), e ricostruirli nel dispatcher
//! significherebbe misurare un'imitazione dei gate veri (regola O). La
//! memoizzazione tiene la decisione dove i suoi gate vivono e le da' comunque un
//! proprietario: la chiave.
//!
//! Le figure partono in parallelo e chiedono tutte entro pochi millisecondi, per
//! cui un «guarda se c'e' gia'» non basterebbe: la prima non ha ancora
//! risposto quando le altre guardano, e tutte e otto pagherebbero. Serve che le
//! altre ASPETTINO la prima, ed e' cio' che fa [`tokio::sync::OnceCell`]: una
//! sola esecuzione, le altre sospese sul risultato. E' anche cancellation-safe
//! per costruzione — se la figura che sta decidendo viene cancellata (timeout,
//! run interrotto) la cella resta vuota e la successiva decide, invece di
//! lasciare le altre appese per sempre.
//!
//! ## Un esito NON PRESO si eredita come tutti gli altri
//!
//! Se la chiamata fallisce, la stessa causa raggiunge le figure sorelle senza
//! che ognuna la riscopra a proprie spese. E' deliberato e ha una misura dietro:
//! il fallimento tipico di questa finestra e' il 429 PRODOTTO dalle chiamate
//! precedenti, quindi riprovare sette volte e' il modo piu' efficace di
//! confermarlo. La causa e' un campo, non l'assenza di un valore
//! ([`MotivoNonPresa`], regola Q): «non ho deciso perche' il modello non ha
//! risposto» e «non ho deciso perche' non ha emesso il verdetto» hanno rimedi
//! diversi e non collassano in un `None`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::OnceCell;

use crate::decisions::clarify_signature::clarify_signature;
use crate::state::AgentState;

use super::clarify_or_expand::LlmDecision;

/// Perche' la decisione non e' stata presa.
///
/// Due varianti e non un `None`: la prima e' un guasto del trasporto (il
/// fornitore non ha risposto), la seconda e' un modello che ha risposto SENZA
/// emettere la tool call del verdetto. Il primo si rimedia altrove (cooldown,
/// carico), il secondo e' una questione di modello o di schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotivoNonPresa {
    /// La chiamata al modello e' fallita (errore di porta).
    ChiamataFallita,
    /// Il modello ha risposto senza emettere la tool call `clarify_or_expand`.
    VerdettoNonEmesso,
}

impl MotivoNonPresa {
    /// Identificatore canonico (regola N) per log e payload.
    pub fn identificatore(self) -> &'static str {
        match self {
            Self::ChiamataFallita => "llm_call_failed",
            Self::VerdettoNonEmesso => "verdict_not_emitted",
        }
    }
}

/// L'esito della decisione di chiarimento su un mandato: cio' che si eredita.
#[derive(Debug, Clone, PartialEq)]
pub enum EsitoDecisione {
    /// Il modello ha deciso: questa e' la decisione.
    Presa(LlmDecision),
    /// Nessuna decisione, con la causa dichiarata.
    NonPresa(MotivoNonPresa),
}

/// Identita' di una decisione di chiarimento: la CONVOCAZIONE che la possiede e
/// il TESTO su cui si decide.
///
/// Campi privati e un solo costruttore: la chiave non si compone a mano, o due
/// call site potrebbero comporla con due idee diverse di «stessa domanda».
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChiaveDecisione {
    convocazione: String,
    mandato: String,
}

impl ChiaveDecisione {
    /// Il criterio: esiste una convocazione a cui questa decisione appartiene?
    ///
    /// La convocazione e' il run che ha convocato la figura
    /// (`AgentState::parent_run_id`, scritto all'origine dal dispatcher). Senza,
    /// il run e' quello di chat: non c'e' nessuna convocazione, la domanda se la
    /// pone lui solo, e la decisione non si condivide con nessuno — `None`.
    ///
    /// NON e' [`crate::decisions::interlocutore::Interlocutore`], che risponde a
    /// «c'e' qualcuno a cui chiedere?» e basta a se stesso con la sola
    /// profondita': qui serve il NOME della convocazione, perche' e' lui la
    /// chiave. Un sub-run che dichiarasse la profondita' e non il padre non ha
    /// una convocazione nominabile e decide per conto proprio — l'errore cade
    /// dalla parte di pagare una chiamata in piu', mai di ereditare da un
    /// insieme sbagliato.
    pub fn della_convocazione(state: &AgentState, mandato: &str) -> Option<Self> {
        let convocazione = state.parent_run_id.as_deref().map(str::trim)?;
        if convocazione.is_empty() {
            return None;
        }
        Some(Self {
            convocazione: convocazione.to_string(),
            mandato: mandato.to_string(),
        })
    }

    /// Il run che possiede questa decisione.
    pub fn convocazione(&self) -> &str {
        &self.convocazione
    }

    /// Identificatore CORTO del testo su cui si e' deciso, per il payload e per
    /// il log.
    ///
    /// Delega alla firma di un testo che il crate ha gia'
    /// ([`clarify_signature`]): due normalizzazioni darebbero due idee di
    /// «stesso testo». E' resa, non identita': a decidere l'uguaglianza e' il
    /// testo intero dentro la chiave, quindi una collisione di firma non puo'
    /// far ereditare la decisione di un altro mandato.
    pub fn firma_mandato(&self) -> String {
        clarify_signature(&self.mandato)
    }
}

/// Da dove viene la decisione che questo run APPLICA (regola Q: il campo, non
/// un'inferenza di chi legge).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvenienzaDecisione {
    /// Nessuna convocazione: questo run e' l'unico a porsi la domanda.
    FuoriConvocazione,
    /// L'ha presa questo run, ed e' ora la decisione della convocazione.
    Presa,
    /// Gia' presa per QUESTO testo dalla convocazione: nessuna chiamata al
    /// modello, nessuna `list_files`.
    Ereditata {
        /// Il run che possiede la decisione.
        convocazione: String,
        /// Il testo su cui e' stata presa.
        firma_mandato: String,
    },
}

impl ProvenienzaDecisione {
    /// Identificatore canonico (regola N) per il campo strutturato dei log.
    pub fn identificatore(&self) -> &'static str {
        match self {
            Self::FuoriConvocazione => "outside_convocation",
            Self::Presa => "taken_here",
            Self::Ereditata { .. } => "inherited",
        }
    }

    /// I campi da allegare a cio' che il run produce, quando c'e' qualcosa da
    /// dichiarare.
    ///
    /// `None` dove la decisione l'ha presa questo run: li' non c'e' stata
    /// nessuna deviazione, e scrivere una provenienza dove non e' successo
    /// niente la renderebbe indistinguibile da una che e' avvenuta (stessa
    /// disciplina di
    /// [`crate::decisions::interlocutore::Interlocutore::motivo_assenza`]).
    pub fn dichiarazione(&self) -> Option<Value> {
        match self {
            Self::FuoriConvocazione | Self::Presa => None,
            Self::Ereditata {
                convocazione,
                firma_mandato,
            } => Some(json!({
                "provenienza": self.identificatore(),
                "convocazione": convocazione,
                "firma_mandato": firma_mandato,
            })),
        }
    }
}

/// Vita massima di una voce del registro.
///
/// NON e' un TTL di cache — la chiave e' gia' la convocazione, quindi una voce
/// non puo' in nessun caso rispondere a un'altra: e' igiene di memoria per un
/// registro di processo, dimensionata sopra la durata di qualunque convocazione
/// (i tetti dei sub-run stanno nell'ordine dei minuti).
const VITA_MASSIMA_VOCE: Duration = Duration::from_secs(1800);

/// Tetto al numero di voci vive. Superato, si potano le piu' VECCHIE.
const VOCI_MASSIME: usize = 512;

/// Una decisione della convocazione, con l'istante in cui la voce e' nata (e'
/// l'eta' a governare la potatura, non l'ultimo accesso: una voce vale finche'
/// dura la convocazione che l'ha creata).
struct Voce {
    cella: Arc<OnceCell<EsitoDecisione>>,
    nata: Instant,
}

/// Registro di processo delle decisioni per convocazione.
///
/// Sta qui, accanto al nodo che decide, e non fra le `decisions`: quelle sono
/// pure per contratto, questo tiene stato. E' la stessa forma del registro del
/// carico per fornitore (`mcp-core::provider_inflight`): coordinamento locale al
/// processo di una domanda che ha un proprietario.
static REGISTRO: LazyLock<Mutex<HashMap<ChiaveDecisione, Voce>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// La cella della chiave, creandola se manca. Pota prima di inserire.
fn cella_per(chiave: &ChiaveDecisione) -> Arc<OnceCell<EsitoDecisione>> {
    // Un mutex avvelenato non e' un buon motivo per smettere di condividere le
    // decisioni: si riprende il contenuto e si prosegue.
    let mut registro = REGISTRO.lock().unwrap_or_else(|e| e.into_inner());
    pota(&mut registro);
    registro
        .entry(chiave.clone())
        .or_insert_with(|| Voce {
            cella: Arc::new(OnceCell::new()),
            nata: Instant::now(),
        })
        .cella
        .clone()
}

/// Igiene di memoria: via le voci scadute, e se restano troppe via le piu'
/// vecchie. Una voce potata mentre la convocazione e' ancora viva costa una
/// chiamata in piu', mai una decisione sbagliata.
fn pota(registro: &mut HashMap<ChiaveDecisione, Voce>) {
    let ora = Instant::now();
    registro.retain(|_, v| ora.duration_since(v.nata) < VITA_MASSIMA_VOCE);
    if registro.len() <= VOCI_MASSIME {
        return;
    }
    let mut eta: Vec<(ChiaveDecisione, Instant)> =
        registro.iter().map(|(k, v)| (k.clone(), v.nata)).collect();
    eta.sort_by_key(|(_, nata)| *nata);
    for (chiave, _) in eta.into_iter().take(registro.len() - VOCI_MASSIME) {
        registro.remove(&chiave);
    }
}

/// La decisione della convocazione: la prende il PRIMO che arriva, gli altri la
/// ereditano senza chiamare il modello.
///
/// `calcola` e' eseguita AL PIU' UNA VOLTA per chiave. Chi arriva mentre e' in
/// corso attende il suo esito; chi arriva dopo lo legge. Ritorna anche la
/// provenienza, che e' cio' che il chiamante dichiara nei propri campi.
pub async fn una_volta_per_convocazione<F, Fut>(
    chiave: &ChiaveDecisione,
    calcola: F,
) -> (EsitoDecisione, ProvenienzaDecisione)
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = EsitoDecisione>,
{
    let cella = cella_per(chiave);
    // `get_or_init` non dice CHI ha inizializzato: lo dichiara la closure stessa.
    let presa_qui = AtomicBool::new(false);
    let esito = cella
        .get_or_init(|| async {
            presa_qui.store(true, Ordering::SeqCst);
            calcola().await
        })
        .await
        .clone();
    let provenienza = if presa_qui.load(Ordering::SeqCst) {
        ProvenienzaDecisione::Presa
    } else {
        ProvenienzaDecisione::Ereditata {
            convocazione: chiave.convocazione.clone(),
            firma_mandato: chiave.firma_mandato(),
        }
    };
    (esito, provenienza)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::nodes::ClarifyMode;

    fn decisione_finta() -> LlmDecision {
        LlmDecision::from_tool_input(&json!({"mode": "expand", "expanded_query": "x"}))
    }

    fn stato_figura(padre: Option<&str>) -> AgentState {
        AgentState {
            parent_run_id: padre.map(str::to_string),
            subagent_depth: padre.map(|_| 1),
            ..Default::default()
        }
    }

    /// Il criterio: chi ha una convocazione, e chi no.
    ///
    /// MUTAZIONE: far ritornare `None` sempre — il difetto reale, cioe' nessuna
    /// identita' condivisa — fa rosseggiare le due asserzioni sulla figura.
    #[test]
    fn solo_una_figura_convocata_ha_una_convocazione() {
        assert!(
            ChiaveDecisione::della_convocazione(&stato_figura(None), "task").is_none(),
            "run di chat: nessuna convocazione, decide per se'"
        );
        let k = ChiaveDecisione::della_convocazione(&stato_figura(Some("padre-1")), "task")
            .expect("figura convocata");
        assert_eq!(k.convocazione(), "padre-1");
        assert_eq!(
            ChiaveDecisione::della_convocazione(&stato_figura(Some("  ")), "task"),
            None,
            "un padre vuoto non e' un padre: e' un campo non popolato sul wire"
        );
    }

    /// Due mandati DIVERSI nella stessa convocazione sono due domande: e' il
    /// caso misurato delle due `provider_analyst`.
    #[test]
    fn testi_diversi_nella_stessa_convocazione_sono_chiavi_diverse() {
        let s = stato_figura(Some("padre-1"));
        let a = ChiaveDecisione::della_convocazione(&s, "analizza openai").unwrap();
        let b = ChiaveDecisione::della_convocazione(&s, "analizza mistral").unwrap();
        assert_ne!(a, b);
        let c = ChiaveDecisione::della_convocazione(&s, "analizza openai").unwrap();
        assert_eq!(a, c, "stesso testo, stessa convocazione -> stessa domanda");
    }

    /// Lo stesso testo in DUE convocazioni diverse non si eredita: la decisione
    /// ha un proprietario, e non e' il testo.
    #[test]
    fn lo_stesso_testo_in_due_convocazioni_non_e_la_stessa_chiave() {
        let a = ChiaveDecisione::della_convocazione(&stato_figura(Some("padre-1")), "t").unwrap();
        let b = ChiaveDecisione::della_convocazione(&stato_figura(Some("padre-2")), "t").unwrap();
        assert_ne!(a, b);
    }

    /// N richieste CONCORRENTI sulla stessa chiave -> UNA esecuzione.
    ///
    /// Concorrenti e non in sequenza: e' la forma reale del fan-out, e un
    /// «guarda se c'e' gia'» senza attesa qui fallirebbe (nessuno ha ancora
    /// risposto quando gli altri guardano).
    #[tokio::test]
    async fn otto_richieste_concorrenti_una_sola_esecuzione() {
        let chiave =
            ChiaveDecisione::della_convocazione(&stato_figura(Some(&uuid::Uuid::new_v4().to_string())), "mandato")
                .unwrap();
        let esecuzioni = Arc::new(AtomicUsize::new(0));
        let mut attese = Vec::new();
        for _ in 0..8 {
            let chiave = chiave.clone();
            let esecuzioni = esecuzioni.clone();
            attese.push(async move {
                una_volta_per_convocazione(&chiave, || async {
                    esecuzioni.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    EsitoDecisione::Presa(decisione_finta())
                })
                .await
            });
        }
        let esiti = futures::future::join_all(attese).await;
        assert_eq!(
            esecuzioni.load(Ordering::SeqCst),
            1,
            "otto figure, una sola decisione"
        );
        let prese = esiti
            .iter()
            .filter(|(_, p)| *p == ProvenienzaDecisione::Presa)
            .count();
        assert_eq!(prese, 1, "una sola l'ha presa, le altre l'hanno ereditata");
        for (esito, _) in &esiti {
            assert!(matches!(
                esito,
                EsitoDecisione::Presa(d) if d.mode == ClarifyMode::Expand
            ));
        }
    }

    /// L'esito NON PRESO si eredita come gli altri: la causa raggiunge le
    /// sorelle senza che ognuna la riscopra a proprie spese.
    #[tokio::test]
    async fn anche_il_fallimento_si_eredita_con_la_sua_causa() {
        let chiave = ChiaveDecisione::della_convocazione(
            &stato_figura(Some(&uuid::Uuid::new_v4().to_string())),
            "mandato",
        )
        .unwrap();
        let esecuzioni = Arc::new(AtomicUsize::new(0));
        for _ in 0..3 {
            let esecuzioni = esecuzioni.clone();
            let (esito, _) = una_volta_per_convocazione(&chiave, || async {
                esecuzioni.fetch_add(1, Ordering::SeqCst);
                EsitoDecisione::NonPresa(MotivoNonPresa::ChiamataFallita)
            })
            .await;
            assert_eq!(
                esito,
                EsitoDecisione::NonPresa(MotivoNonPresa::ChiamataFallita)
            );
        }
        assert_eq!(esecuzioni.load(Ordering::SeqCst), 1);
    }

    /// La provenienza si DICHIARA solo dove c'e' qualcosa da dichiarare.
    #[test]
    fn la_provenienza_parla_solo_quando_e_ereditata() {
        assert_eq!(ProvenienzaDecisione::Presa.dichiarazione(), None);
        assert_eq!(ProvenienzaDecisione::FuoriConvocazione.dichiarazione(), None);
        let d = ProvenienzaDecisione::Ereditata {
            convocazione: "padre-1".to_string(),
            firma_mandato: "abc123abc123".to_string(),
        }
        .dichiarazione()
        .expect("ereditata dichiara");
        assert_eq!(d["provenienza"], json!("inherited"));
        assert_eq!(d["convocazione"], json!("padre-1"));
        assert_eq!(d["firma_mandato"], json!("abc123abc123"));
    }

    /// Le cause non collassano in un `None`: hanno rimedi diversi.
    #[test]
    fn le_due_cause_di_non_decisione_restano_distinte() {
        assert_ne!(
            MotivoNonPresa::ChiamataFallita.identificatore(),
            MotivoNonPresa::VerdettoNonEmesso.identificatore()
        );
    }

    /// La potatura non puo' far ereditare la decisione di un'altra
    /// convocazione: toglie voci, non le rimescola.
    #[test]
    fn la_potatura_toglie_le_piu_vecchie_e_non_rimescola() {
        let mut reg: HashMap<ChiaveDecisione, Voce> = HashMap::new();
        let scaduta =
            ChiaveDecisione::della_convocazione(&stato_figura(Some("vecchio")), "t").unwrap();
        reg.insert(
            scaduta.clone(),
            Voce {
                cella: Arc::new(OnceCell::new()),
                nata: Instant::now() - VITA_MASSIMA_VOCE - Duration::from_secs(1),
            },
        );
        let viva = ChiaveDecisione::della_convocazione(&stato_figura(Some("nuovo")), "t").unwrap();
        reg.insert(
            viva.clone(),
            Voce {
                cella: Arc::new(OnceCell::new()),
                nata: Instant::now(),
            },
        );
        pota(&mut reg);
        assert!(!reg.contains_key(&scaduta), "la voce scaduta esce");
        assert!(reg.contains_key(&viva), "la voce viva resta");
    }
}

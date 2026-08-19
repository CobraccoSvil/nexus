//! Le direttive CONDIVISE nel system prompt: punto unico (regola L) di QUALI
//! righe di `nexus_shared_directives` entrano in un contesto, in quale ORDINE, e
//! di come si rendono.
//!
//! ## ROOT CAUSE (misurata sul META vivo il 19/08/2026)
//!
//! La tabella nasce con la mig 0135 per togliere dai template le copie inline
//! di due blocchi (`<safety_progetto>` della 0096, `<anti_narration>` della
//! 0127) e tenerne UNA riga ciascuno: la 0135 li RIMUOVE dai template proprio
//! perche' l'iniezione a runtime li avrebbe rimessi. L'iniettore era
//! `brain/agents/prompt_registry.py` (commit fc7db83e, 12/05/2026); il porting
//! zero-Python (75a6d621, 27/06/2026) ha cancellato il brain, e con esso
//! l'unico consumatore. La tabella e' sopravvissuta, l'iniezione no.
//!
//! Il porting Rust del registro (`nexus_orchestrator::prompt_registry`) e' una
//! `HashMap` in memoria senza accesso al DB: non poteva ereditare quella meta'.
//!
//! MISURA del 19/08/2026: tre righe attive (`project_isolation`,
//! `anti_narration`, `config_restart`), e i soli lettori della tabella in tutto
//! il repo erano la pagina admin e il CRUD di `admin-service`. **Nessun
//! compositore di prompt.** Per 53 giorni nessun modello ha ricevuto le regole
//! di isolamento progetto (cleanup Docker filtrato, container `ideai-*`
//! intoccabili, scope alla `project_root`) — e il difetto non poteva fallire
//! nulla: un blocco che non arriva non rompe niente.
//!
//! ## Perche' l'ambito e' un TIPO e non un prefisso di chiave
//!
//! La colonna `scope` ha tre valori (`agent`, `system`, `all`) e il commento
//! della 0135 li descrive come prefissi: «agent = solo agent.*, system = solo
//! system.*». Quel criterio LESSICALE oggi non seleziona: le figure convocate
//! hanno chiavi `subagent.*` (21 su 21 in produzione, scritte a runtime dal
//! FigureWizard e mai da una migrazione), che non cominciano per `agent.` —
//! quindi una direttiva `agent` non raggiungerebbe nemmeno loro. Il perimetro
//! non e' una proprieta' del NOME della chiave: e' cio' che il prompt E',
//! e il chiamante lo DICHIARA passando [`AmbitoPrompt`] (regola Q).
//!
//! ## Idempotenza
//!
//! Il system della chat viene composto una volta e poi ripassa da
//! `compose_agent_system_text` quando il turno apre un run agentico: la stessa
//! ragione per cui `con_ambiente` e' idempotente. Il criterio e' per DIRETTIVA
//! e non per insieme — un prompt che ne ha gia' una e non le altre riceve solo
//! le mancanti. Si decide sul tag di CHIUSURA quando il contenuto e' un blocco
//! XML (una MENZIONE cita l'apertura, mai la coppia: e' la trappola che la 0674
//! ha documentato per il blocco del processo operativo, vedi
//! [`crate::processo::TAG_CHIUSURA`]), altrimenti sul testo esatto.
//!
//! ## Costo sul prefisso riusabile: nessuno
//!
//! Le direttive sono STABILI (cambiano solo su edit admin) e vanno appese PRIMA
//! di qualunque blocco che introduca `CONFINE_DI_TURNO`. Nel run agentico
//! entrano nel gruppo `stabili` di `componi_system_di_run`, mai in coda: appese
//! dopo, finirebbero dietro il confine appena esiste una parte variabile.

use sqlx::PgPool;

use crate::composizione::{ChiaveBlocco, Composizione, Segmento};

/// Valori canonici della colonna `scope` (regola N: l'identificatore vive in un
/// posto solo, e il DB e' la fonte).
const SCOPE_AGENT: &str = "agent";
const SCOPE_SYSTEM: &str = "system";
const SCOPE_ALL: &str = "all";

/// Che cosa E' il prompt che si sta componendo. Lo dichiara il chiamante: e'
/// l'unico che lo sa, e dedurlo dal nome della chiave e' il confronto lessicale
/// che questo modulo esiste per togliere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmbitoPrompt {
    /// Il system prompt di base dell'agente Nexus (`system.nexus_base`): la
    /// chat e il run agentico che ne discende.
    Sistema,
    /// Il prompt di una figura specializzata o convocata (`agent.*` storiche,
    /// `subagent.*` del wizard).
    Figura,
}

/// Il perimetro che la RIGA dichiara, letto dalla colonna `scope`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeDichiarato {
    /// `agent`: i prompt delle figure.
    Figure,
    /// `system`: il prompt di base dell'agente Nexus.
    Sistema,
    /// `all`: entrambi.
    Ovunque,
    /// Valore fuori vocabolario. Non e' un ripiego: e' una variante dichiarata
    /// (regola Q), perche' «non so dove vada» non degradi ne' a «ovunque» ne' a
    /// un silenzio.
    Sconosciuto(String),
}

impl ScopeDichiarato {
    /// Dal valore grezzo della colonna. Il confronto e' case-insensitive sul
    /// valore trimmato: la colonna e' TEXT libero e la scrive anche l'admin.
    pub fn dal_valore(grezzo: &str) -> Self {
        match grezzo.trim().to_ascii_lowercase().as_str() {
            SCOPE_AGENT => Self::Figure,
            SCOPE_SYSTEM => Self::Sistema,
            SCOPE_ALL => Self::Ovunque,
            _ => Self::Sconosciuto(grezzo.to_string()),
        }
    }

    /// Questa riga si applica al contesto che si sta componendo?
    ///
    /// `Sconosciuto` risponde NO: mettere una direttiva in un contesto che
    /// nessuno ha dichiarato e' rumore con l'autorita' del system prompt. La
    /// perdita non e' silenziosa — la dichiara un WARN al caricamento.
    pub fn si_applica(&self, ambito: AmbitoPrompt) -> bool {
        match self {
            Self::Ovunque => true,
            Self::Sistema => ambito == AmbitoPrompt::Sistema,
            Self::Figure => ambito == AmbitoPrompt::Figura,
            Self::Sconosciuto(_) => false,
        }
    }
}

/// Una riga della tabella, gia' filtrata sull'ambito del chiamante.
#[derive(Debug, Clone)]
pub struct DirettivaCondivisa {
    pub key: String,
    pub content: String,
    pub priority: i32,
}

impl DirettivaCondivisa {
    /// Il marcatore su cui decidere l'idempotenza: il tag di CHIUSURA se il
    /// contenuto e' un blocco, altrimenti il testo intero. Per la diagnostica e
    /// per i test; la DOMANDA la pone [`Self::gia_presente`].
    pub fn marcatore(&self) -> String {
        match riconoscimento_di(&self.content) {
            Riconoscimento::Blocco(k) => k.chiusura(),
            Riconoscimento::Testo => self.content.clone(),
        }
    }

    /// La direttiva e' gia' in questo prompt?
    ///
    /// Dove il contenuto E' un blocco la domanda si pone alla STRUTTURA
    /// ([`Composizione::ha`]): uguaglianza su un nome di tag, mai una
    /// sottostringa. Il criterio precedente era `system.contains("</tag>")`, e
    /// contava come presente anche un tag di chiusura ORFANO — che nel corpus
    /// reale non esiste (misurato: il ponte Rust/SQL non trova divergenze su 174
    /// righe attive), quindi il comportamento non cambia e la classe di errore
    /// sparisce.
    pub fn gia_presente(&self, system: &str) -> bool {
        match riconoscimento_di(&self.content) {
            Riconoscimento::Blocco(k) => Composizione::scomponi(system).ha(&k),
            Riconoscimento::Testo => system.contains(&self.content),
        }
    }
}

/// Come si riconosce che questa direttiva e' gia' in un prompt.
///
/// Le due varianti non sono un'ottimizzazione: `nexus_shared_directives` e' una
/// tabella che l'admin riempie, e nulla obbliga il contenuto di una riga a
/// essere un blocco. L'ignoto non degrada a «blocco senza nome» (regola Q): dove
/// non c'e' una chiave si torna al testo esatto, che e' l'unica identita' che
/// quel contenuto possiede.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Riconoscimento {
    /// Il contenuto E' un blocco: si decide sulla sua CHIAVE.
    Blocco(ChiaveBlocco),
    /// Prosa, o piu' blocchi: si decide sul testo esatto.
    Testo,
}

/// UN blocco solo, eventualmente circondato da spazi: e' cio' che rende il
/// contenuto identificabile da una chiave.
///
/// Sull'APERTURA non si decide mai: un template che MENZIONA `<safety_progetto>`
/// in prosa renderebbe la direttiva vera indistinguibile da una citazione, e il
/// blocco non entrerebbe (trappola trovata dalla review avversaria del 04/08 sul
/// blocco del processo operativo). La scomposizione la fa il punto unico, che
/// una menzione non la conta gia' per costruzione.
fn riconoscimento_di(content: &str) -> Riconoscimento {
    let mut trovata: Option<ChiaveBlocco> = None;
    for segmento in Composizione::scomponi(content).segmenti() {
        match segmento {
            Segmento::Interstizio(t) if t.trim().is_empty() => {}
            Segmento::Blocco { chiave, .. } if trovata.is_none() => {
                trovata = Some(chiave.clone());
            }
            _ => return Riconoscimento::Testo,
        }
    }
    trovata.map_or(Riconoscimento::Testo, Riconoscimento::Blocco)
}

/// Perche' nessuna direttiva entra. Tre cause con tre rimedi diversi: il
/// registro vuoto e' una configurazione da popolare, l'ambito scoperto e' una
/// riga con lo `scope` sbagliato, il registro illeggibile e' un guasto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MotivoAssenza {
    /// Nessuna riga attiva in tabella.
    RegistroVuoto,
    /// Righe attive esistono, nessuna dichiara questo ambito.
    NessunaPerAmbito,
    /// La lettura e' fallita: non si sa se ce ne fossero.
    RegistroNonLeggibile,
}

/// Cio' che il registro ha da dire per un ambito.
#[derive(Debug, Clone)]
pub enum Direttive {
    Presenti(Vec<DirettivaCondivisa>),
    Nessuna(MotivoAssenza),
}

impl Direttive {
    /// Il blocco da appendere: le sole direttive NON gia' presenti nel prompt,
    /// nell'ordine di `priority`, separate da una riga vuota.
    ///
    /// `None` quando non c'e' niente da aggiungere — cosi' il chiamante non
    /// appende una stringa vuota, che nel run agentico introdurrebbe comunque
    /// una parte non vuota nel gruppo stabile.
    pub fn blocco_mancante(&self, system: &str) -> Option<String> {
        let Self::Presenti(righe) = self else {
            return None;
        };
        let mancanti: Vec<&str> = righe
            .iter()
            .filter(|d| !d.gia_presente(system))
            .map(|d| d.content.trim())
            .filter(|c| !c.is_empty())
            .collect();
        if mancanti.is_empty() {
            None
        } else {
            Some(mancanti.join("\n\n"))
        }
    }

    /// Le chiavi servite, nell'ordine di iniezione. Per i test e la diagnostica.
    pub fn chiavi(&self) -> Vec<&str> {
        match self {
            Self::Presenti(righe) => righe.iter().map(|d| d.key.as_str()).collect(),
            Self::Nessuna(_) => Vec::new(),
        }
    }
}

/// Le direttive attive che dichiarano questo ambito, in ordine di `priority`.
///
/// L'ordine ha `key` come secondo criterio: due righe a pari priorita' uscirebbero
/// in ordine non deterministico e il prefisso del prompt cambierebbe senza che
/// nulla sia cambiato (stessa disciplina di [`crate::learned`]).
///
/// Il registro vuoto e' un caso DICHIARATO (warn), mai un silenzio: e' la forma
/// esatta in cui questo difetto e' vissuto 53 giorni.
pub async fn section(db: &PgPool, ambito: AmbitoPrompt) -> Direttive {
    let righe = sqlx::query_as::<_, (String, String, String, i32)>(
        "SELECT key, content, scope, priority FROM nexus_shared_directives \
          WHERE is_active = TRUE ORDER BY priority ASC, key ASC",
    )
    .fetch_all(db)
    .await;
    let righe = match righe {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                errore = %e,
                "nexus_shared_directives non leggibile: nessuna direttiva condivisa nel prompt"
            );
            return Direttive::Nessuna(MotivoAssenza::RegistroNonLeggibile);
        }
    };
    if righe.is_empty() {
        tracing::warn!(
            "nexus_shared_directives non ha righe attive: nessuna direttiva condivisa nel prompt"
        );
        return Direttive::Nessuna(MotivoAssenza::RegistroVuoto);
    }
    let mut scelte = Vec::new();
    for (key, content, scope_grezzo, priority) in righe {
        let scope = ScopeDichiarato::dal_valore(&scope_grezzo);
        if let ScopeDichiarato::Sconosciuto(v) = &scope {
            tracing::warn!(
                direttiva = %key,
                scope = %v,
                "scope fuori vocabolario (agent|system|all): la direttiva non entra in nessun prompt"
            );
            continue;
        }
        if scope.si_applica(ambito) && !content.trim().is_empty() {
            scelte.push(DirettivaCondivisa { key, content, priority });
        }
    }
    if scelte.is_empty() {
        tracing::warn!(
            ?ambito,
            "nessuna direttiva condivisa dichiara questo ambito: il prompt resta senza"
        );
        return Direttive::Nessuna(MotivoAssenza::NessunaPerAmbito);
    }
    Direttive::Presenti(scelte)
}

/// Appende al system le direttive condivise dell'ambito (idempotente per
/// direttiva).
///
/// Per la chat e per il sub-run. Il run agentico NON usa questa funzione: li' il
/// blocco deve entrare nel gruppo `stabili` di `componi_system_di_run`, e
/// appenderlo alla fine lo spingerebbe dietro `CONFINE_DI_TURNO`.
pub async fn con_direttive(db: &PgPool, system: String, ambito: AmbitoPrompt) -> String {
    match section(db, ambito).await.blocco_mancante(&system) {
        Some(blocco) => {
            let mut testo = system;
            testo.push_str("\n\n");
            testo.push_str(&blocco);
            testo.push('\n');
            testo
        }
        None => system,
    }
}

/// Il blocco pronto per il gruppo STABILE di `componi_system_di_run`: le sole
/// direttive che `sistema_gia_composto` non porta gia', isolate dai propri a
/// capo. Stringa VUOTA quando non c'e' niente da aggiungere (quel gruppo
/// concatena senza separatori, e un blocco vuoto e' un no-op).
///
/// Gemella di [`con_direttive`], stessa idempotenza: cambia solo CHI mette il
/// testo nel prompt. Il run agentico non puo' usare `con_direttive` perche' li'
/// il blocco deve entrare PRIMA del confine di turno, e appenderlo al testo gia'
/// composto lo metterebbe dietro. La resa vive qui e non nel compositore: due
/// avvolgimenti diversi darebbero due spaziature per lo stesso blocco.
pub async fn blocco_stabile(
    db: &PgPool,
    sistema_gia_composto: &str,
    ambito: AmbitoPrompt,
) -> String {
    section(db, ambito)
        .await
        .blocco_mancante(sistema_gia_composto)
        .map(|blocco| format!("\n{blocco}\n"))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYSTEM: &str = "<role>Sei l'agente Nexus.</role>";

    /// Il contratto in una riga: la direttiva di isolamento progetto — la riga
    /// che per 53 giorni non ha raggiunto nessun modello — entra nel system, e
    /// il testo viene dal DB migrato (regola O: la semina e' la 0135 del set
    /// META, non una fixture).
    ///
    /// MUTAZIONE: togliere l'append da `con_direttive` (o riportare `scope` di
    /// `project_isolation` ad `agent` nella 0743) fa cadere la prima asserzione
    /// col valore del difetto reale: nessun `<safety_progetto>` nel prompt.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn l_isolamento_progetto_entra_nel_prompt_di_sistema(pool: PgPool) {
        let finale = con_direttive(&pool, SYSTEM.to_string(), AmbitoPrompt::Sistema).await;
        assert!(finale.contains("<safety_progetto>"), "{finale}");
        assert!(finale.contains("</safety_progetto>"), "{finale}");
        // Ancore del contenuto seminato: le due regole che il difetto toglieva.
        assert!(finale.contains("ideai-"), "{finale}");
        assert!(finale.contains("docker system prune"), "{finale}");
        // Si aggiunge, non si riscrive.
        assert!(finale.contains(SYSTEM), "{finale}");
    }

    /// Una figura riceve tutte e tre le direttive attive: l'isolamento (scope
    /// `all` dalla 0743) piu' le due dichiarate per le figure.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn una_figura_riceve_le_direttive_di_ambito(pool: PgPool) {
        let finale = con_direttive(&pool, SYSTEM.to_string(), AmbitoPrompt::Figura).await;
        assert!(finale.contains("</safety_progetto>"), "{finale}");
        assert!(finale.contains("</anti_narration>"), "{finale}");
        assert!(finale.contains("</config_restart>"), "{finale}");
    }

    /// L'ordine e' quello della colonna `priority`, e la posizione relativa fra
    /// due direttive e' osservabile nel testo finale.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn l_ordine_viene_dalla_priorita(pool: PgPool) {
        let d = section(&pool, AmbitoPrompt::Figura).await;
        assert_eq!(d.chiavi(), vec!["project_isolation", "anti_narration", "config_restart"]);
        let finale = con_direttive(&pool, SYSTEM.to_string(), AmbitoPrompt::Figura).await;
        let iso = finale.find("<safety_progetto>").expect("isolamento");
        let narr = finale.find("<anti_narration>").expect("narrazione");
        let cfg = finale.find("<config_restart>").expect("restart");
        assert!(iso < narr && narr < cfg, "{finale}");
    }

    /// Due passaggi dal compositore = una copia sola (il caso resend, e il caso
    /// chat -> run agentico che ricompone sopra lo stesso testo).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn le_direttive_non_si_raddoppiano(pool: PgPool) {
        let una = con_direttive(&pool, SYSTEM.to_string(), AmbitoPrompt::Figura).await;
        let due = con_direttive(&pool, una.clone(), AmbitoPrompt::Figura).await;
        assert_eq!(una, due);
        assert_eq!(due.matches("<safety_progetto>").count(), 1);
        assert_eq!(due.matches("<anti_narration>").count(), 1);
    }

    /// Idempotenza PARZIALE: un prompt che ha gia' una direttiva riceve le
    /// altre. Il criterio e' per riga, non per insieme.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn una_direttiva_gia_presente_non_blocca_le_altre(pool: PgPool) {
        let d = section(&pool, AmbitoPrompt::Figura).await;
        let Direttive::Presenti(righe) = &d else { panic!("attese direttive: {d:?}") };
        let solo_prima = format!("{SYSTEM}\n\n{}", righe[0].content);
        let blocco = d.blocco_mancante(&solo_prima).expect("le altre due mancano");
        assert!(!blocco.contains(&righe[0].content), "la prima e' stata riappesa");
        assert!(blocco.contains(&righe[1].marcatore()), "{blocco}");
    }

    /// Tutte disattivate dall'admin = registro vuoto DICHIARATO, system
    /// invariato: una configurazione assente si vede, non si supplisce con un
    /// letterale che nessuno ha scelto (regola G).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn registro_vuoto_nessun_blocco(pool: PgPool) {
        sqlx::query("UPDATE nexus_shared_directives SET is_active = false")
            .execute(&pool)
            .await
            .expect("disattiva tutte");
        let d = section(&pool, AmbitoPrompt::Figura).await;
        assert!(matches!(d, Direttive::Nessuna(MotivoAssenza::RegistroVuoto)), "{d:?}");
        let finale = con_direttive(&pool, SYSTEM.to_string(), AmbitoPrompt::Figura).await;
        assert_eq!(finale, SYSTEM);
    }

    /// Righe attive che non dichiarano l'ambito: causa DIVERSA dal registro
    /// vuoto (rimedio: correggere lo `scope`, non popolare la tabella).
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn ambito_scoperto_e_una_causa_a_se(pool: PgPool) {
        sqlx::query("UPDATE nexus_shared_directives SET scope = $1")
            .bind(SCOPE_SYSTEM)
            .execute(&pool)
            .await
            .expect("scope system");
        let d = section(&pool, AmbitoPrompt::Figura).await;
        assert!(matches!(d, Direttive::Nessuna(MotivoAssenza::NessunaPerAmbito)), "{d:?}");
        // E l'altro ambito le riceve tutte: la causa e' lo scope, non il vuoto.
        assert_eq!(section(&pool, AmbitoPrompt::Sistema).await.chiavi().len(), 3);
    }

    /// Uno `scope` fuori vocabolario non entra da nessuna parte, e non trascina
    /// con se' le righe sane.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn uno_scope_ignoto_non_entra_e_non_contagia(pool: PgPool) {
        sqlx::query("UPDATE nexus_shared_directives SET scope = 'ovunque' WHERE key = $1")
            .bind("anti_narration")
            .execute(&pool)
            .await
            .expect("scope ignoto");
        let chiavi = section(&pool, AmbitoPrompt::Figura).await.chiavi().join(",");
        assert!(!chiavi.contains("anti_narration"), "{chiavi}");
        assert!(chiavi.contains("project_isolation"), "{chiavi}");
        assert!(chiavi.contains("config_restart"), "{chiavi}");
    }

    /// La MENZIONE di una direttiva in prosa non ne impedisce l'ingresso: si
    /// decide sul tag di chiusura, che una citazione non porta.
    ///
    /// MUTAZIONE: decidere sull'apertura (`<safety_progetto>`) fa credere la
    /// direttiva gia' presente e il blocco non entra — il difetto che la 0674
    /// ha documentato per il processo operativo.
    #[sqlx::test(migrator = "nexus_migrations_embedded::META_MIGRATOR")]
    async fn una_menzione_non_vale_come_direttiva(pool: PgPool) {
        let con_menzione =
            format!("{SYSTEM}\nVedi il blocco <safety_progetto> per le regole di isolamento.");
        let finale = con_direttive(&pool, con_menzione, AmbitoPrompt::Sistema).await;
        assert!(finale.contains("</safety_progetto>"), "{finale}");
    }

    fn marcatore_di(content: &str) -> String {
        DirettivaCondivisa { key: "k".into(), content: content.into(), priority: 0 }.marcatore()
    }

    #[test]
    fn il_marcatore_e_il_tag_di_chiusura_o_il_testo() {
        assert_eq!(marcatore_di("<a>corpo</a>"), "</a>");
        assert_eq!(marcatore_di("\n<safety_progetto>\nx\n</safety_progetto>"), "</safety_progetto>");
        // Nessun blocco: il marcatore e' il testo esatto.
        assert_eq!(marcatore_di("testo semplice"), "testo semplice");
        // Apertura senza chiusura corrispondente: non e' un blocco.
        assert_eq!(marcatore_di("<a>corpo</b>"), "<a>corpo</b>");
        assert_eq!(marcatore_di("<a>corpo"), "<a>corpo");
        // DUE blocchi non hanno UNA chiave: si torna al testo.
        assert_eq!(marcatore_di("<a>x</a><b>y</b>"), "<a>x</a><b>y</b>");
        // Un'apertura con ATTRIBUTI E' un blocco (il parser scritto a mano che
        // viveva qui la rifiutava, in disaccordo col criterio della 0744: tre
        // template attivi aprono cosi').
        assert_eq!(marcatore_di("<a b=\"c\">x</a>"), "</a>");
    }

    /// Il criterio e' STRUTTURALE, non una sottostringa: un tag di chiusura
    /// orfano non fa credere presente una direttiva che non c'e'.
    ///
    /// MUTAZIONE: tornare a `system.contains(marcatore)` fa passare la prima
    /// asserzione e il blocco non entrerebbe piu' in un prompt che, in prosa,
    /// nomini la chiusura.
    #[test]
    fn una_chiusura_orfana_non_vale_come_direttiva() {
        let d = DirettivaCondivisa {
            key: "iso".into(),
            content: "<safety_progetto>x</safety_progetto>".into(),
            priority: 0,
        };
        assert!(!d.gia_presente("prosa che cita </safety_progetto> e basta"));
        assert!(d.gia_presente("testa\n<safety_progetto>x</safety_progetto>\ncoda"));
    }

    #[test]
    fn lo_scope_si_legge_dai_valori_canonici() {
        assert_eq!(ScopeDichiarato::dal_valore("agent"), ScopeDichiarato::Figure);
        assert_eq!(ScopeDichiarato::dal_valore(" SYSTEM "), ScopeDichiarato::Sistema);
        assert_eq!(ScopeDichiarato::dal_valore("all"), ScopeDichiarato::Ovunque);
        assert!(matches!(
            ScopeDichiarato::dal_valore("figure"),
            ScopeDichiarato::Sconosciuto(_)
        ));
        assert!(ScopeDichiarato::Ovunque.si_applica(AmbitoPrompt::Sistema));
        assert!(ScopeDichiarato::Ovunque.si_applica(AmbitoPrompt::Figura));
        assert!(!ScopeDichiarato::Figure.si_applica(AmbitoPrompt::Sistema));
        assert!(!ScopeDichiarato::Sistema.si_applica(AmbitoPrompt::Figura));
        assert!(!ScopeDichiarato::Sconosciuto("x".into()).si_applica(AmbitoPrompt::Sistema));
    }
}

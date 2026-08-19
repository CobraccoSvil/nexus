//! RACCOLTA di un'allocazione di porta: «questa riga di `nexus_port_allocations`
//! va raccolta, o qualcosa la tiene in vita?»
//!
//! # Il difetto: DUE raccoglitori, UNA domanda
//!
//! La stessa riga puo' essere cancellata da due punti diversi del sistema:
//!
//!  1. [`crate::port_registry::cleanup_orphaned_ports`] — il GC periodico, che
//!     scandisce le allocazioni non-`manual` oltre la grace di TUTTI i progetti.
//!  2. `agent_tools::service::cleanup_dead_process_ports` — invocato da
//!     `dedup_and_cleanup_ports` a OGNI `run_service` con `kind="service"`,
//!     sulle allocazioni `dynamic` del progetto che sta avviando un servizio.
//!
//! Rispondono alla STESSA domanda e avevano DUE criteri. Il primo ne accettava
//! tre prove di vita (listener, prenotazione del run, riserva di una unit); il
//! secondo una sola, e la piu' debole: «la porta non e' bindabile?». Cio' che il
//! GC preservava, il raccoglitore dell'avvio cancellava — e lo faceva nel
//! momento peggiore, mentre un servizio parte, senza nemmeno la grace.
//!
//! Il difetto NON e' teorico e non e' un caso di confine: `request_port` scrive
//! righe `allocation_mode = 'dynamic'`, cioe' esattamente il corpus del secondo
//! raccoglitore. Un agente che prenota `backend` e `frontend` e poi avvia
//! `backend` per primo perde la prenotazione di `frontend` all'istante: la sua
//! label non e' quella in avvio, nessun processo la usa ancora, nessuno ascolta.
//!
//! # Il rimedio: un criterio, due chiamanti, fatti propri
//!
//! [`giudica`] e' l'UNICO posto in cui si decide, e dichiara l'ORDINE delle
//! prove una volta sola. I fatti non sono un `struct` ma un TRATTO
//! ([`FattiAllocazione`]) per una ragione misurabile: il GC scandisce ogni riga
//! di ogni progetto a ogni giro, e le due prove costose — la prenotazione
//! (apre il DB del progetto) e la riserva di unit (query o lettura di file) —
//! non vanno pagate su una riga che il listener ha gia' salvato. Con un `struct`
//! raccolto in anticipo o si paga tutto, o la pigrizia diventa un SECONDO
//! ordine scritto nel raccoglitore: cioe' di nuovo due criteri.
//!
//! Ogni chiamante porta i fatti che PUO' osservare e dichiara quelli che non
//! osserva ([`ImpiegoDellaLabel::NonInterrogato`]): «non ho chiesto» non e'
//! «non c'e'», e non e' nemmeno «non ho potuto chiedere» (regola Q).

use sqlx::PgPool;
use uuid::Uuid;

use super::port_recovery::{ListenerScan, PortBind};
use super::prenotazione_porta::{stato_prenotazione, StatoPrenotazione, VitaDelRun};

/// Chiave del setting che dice quanto una riga puo' restare giovane. Letta da
/// [`grace_secs`], che e' l'unico lettore: due letture con due default diversi
/// darebbero due idee di quando una riga diventa giudicabile.
pub const CHIAVE_GRACE: &str = "agent.port_gc.grace_seconds";

/// Grace di default (mig 0262). Vale se il setting manca o non e' un numero.
pub const GRACE_DEFAULT_SECS: i64 = 180;

/// Quanti secondi una riga e' troppo giovane per essere giudicata.
pub async fn grace_secs(db: &PgPool) -> i64 {
    crate::settings::get_setting(db, CHIAVE_GRACE)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(GRACE_DEFAULT_SECS)
}

/// «Questa riga ha superato la grace?»
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EtaAllocazione {
    /// Troppo giovane: nessuna prova ha ancora avuto il tempo di formarsi.
    DentroLaGrace,
    /// Abbastanza vecchia da essere giudicata.
    OltreLaGrace,
}

impl EtaAllocazione {
    /// Dall'istante di creazione della riga. Punto unico del confronto: il GC lo
    /// esprime come predicato SQL (e passa il fatto gia' deciso), il
    /// raccoglitore dell'avvio ha la colonna in mano e lo calcola qui.
    pub fn da_creazione(
        creata: chrono::DateTime<chrono::Utc>,
        grace: i64,
        adesso: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        if (adesso - creata).num_seconds() < grace.max(0) {
            Self::DentroLaGrace
        } else {
            Self::OltreLaGrace
        }
    }
}

/// «Questo progetto puo' usare questa porta?» Delega il criterio al punto unico
/// `nexus_tool_kit::ports`; qui e' solo il FATTO nella forma che il giudizio usa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutorizzazionePorta {
    Autorizzata,
    NonAutorizzata,
}

/// «Qualcuno ascolta su questa porta?»
///
/// Tre valori e mai un `bool`: «libera» e «non ho potuto osservare» portano a
/// decisioni opposte, e i due raccoglitori osservano con strumenti diversi (una
/// fotografia dei listener il GC, un tentativo di bind il raccoglitore
/// dell'avvio) che sbagliano in modi diversi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ascolto {
    /// Qualcuno ascolta: la porta e' in uso.
    Presente,
    /// Nessuno ascolta.
    Nessuno,
    /// Non si e' potuto osservare.
    NonOsservabile(String),
}

impl Ascolto {
    /// Dalla fotografia dei listener del GC. `None` = scansione non riuscita.
    pub fn da_scan(scan: &ListenerScan, porta: u16) -> Self {
        match scan.ascolta(porta) {
            Some(true) => Self::Presente,
            Some(false) => Self::Nessuno,
            None => Self::NonOsservabile(scan.descrizione()),
        }
    }

    /// Dal tentativo di bind del raccoglitore dell'avvio. `Occupata` e' l'unico
    /// esito che autorizza a parlare di un occupante (vedi [`PortBind`]).
    pub fn da_bind(bind: &PortBind) -> Self {
        match bind {
            PortBind::Libera => Self::Nessuno,
            PortBind::Occupata => Self::Presente,
            PortBind::NonInterrogabile { errore, .. } => Self::NonOsservabile(errore.clone()),
        }
    }
}

/// «Un servizio con la label di questa riga la sta usando ADESSO?»
///
/// Le tre varianti non sono gradazioni della stessa cosa: `NonInterrogato` e'
/// il chiamante che dichiara di non porre la domanda (il GC non legge i
/// processi, e il suo criterio non ci ha mai fatto affidamento), mentre
/// `NonInterrogabile` e' la domanda posta e rimasta senza risposta — e su quella
/// si preserva, perche' e' l'unico caso in cui potremmo star togliendo la porta
/// a un servizio che sta partendo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImpiegoDellaLabel {
    InUso,
    NessunProcesso,
    NonInterrogato,
    NonInterrogabile(String),
}

/// «La riga e' la riserva di un servizio configurato ma fermo?»
///
/// Prima era un `bool` con due `return true` di prudenza dentro: «il servizio
/// esiste» e «non ho potuto chiederlo al DB» uscivano indistinguibili, e chi
/// leggeva il verdetto non poteva sapere se la porta fosse trattenuta o solo
/// non giudicata (regola Q).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiservaServizio {
    /// Un servizio installato/configurato dichiara ancora questa porta.
    Riserva,
    /// La riga non nomina nessuna unit: non c'e' riserva da valutare.
    NessunaUnitDichiarata,
    /// La unit c'e' ma non regge piu': servizio rimosso, o porta non piu'
    /// dichiarata dopo una riconfigurazione (mapping stale).
    UnitNonPiuValida,
    /// Non si e' potuto chiedere (DB transitorio): non si distrugge.
    NonInterrogabile(String),
}

/// CHE COSA tiene in vita la riga. E' la ragione del `Preserva`, e finisce nel
/// log: «preservata» senza il perche' non distingue una porta trattenuta da una
/// porta che nessuno ha guardato.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvaDiTenuta {
    Grace,
    Ascolto,
    AscoltoNonOsservabile,
    Prenotazione,
    LabelInUso,
    ImpiegoNonInterrogabile,
    RiservaDiServizio,
    RiservaNonInterrogabile,
}

impl ProvaDiTenuta {
    /// Identificatore canonico (regola N).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Grace => "grace",
            Self::Ascolto => "ascolto",
            Self::AscoltoNonOsservabile => "ascolto_non_osservabile",
            Self::Prenotazione => "prenotazione",
            Self::LabelInUso => "label_in_uso",
            Self::ImpiegoNonInterrogabile => "impiego_non_interrogabile",
            Self::RiservaDiServizio => "riserva_di_servizio",
            Self::RiservaNonInterrogabile => "riserva_non_interrogabile",
        }
    }
}

/// PERCHE' la riga va raccolta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CausaRaccolta {
    /// La porta non e' autorizzata a questo progetto: la riga e' un artefatto, e
    /// proteggerla significherebbe proteggerla proprio mentre fa danno.
    PortaNonAutorizzata,
    /// Nessuna prova di vita: e' il residuo che entrambi i raccoglitori
    /// esistono per raccogliere.
    NessunaProvaDiVita,
}

impl CausaRaccolta {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PortaNonAutorizzata => "porta_non_autorizzata",
            Self::NessunaProvaDiVita => "nessuna_prova_di_vita",
        }
    }
}

/// L'esito del giudizio. Enum e non `bool`: il motivo viaggia col verdetto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerdettoRaccolta {
    Preserva(ProvaDiTenuta),
    Raccogli(CausaRaccolta),
}

impl VerdettoRaccolta {
    /// L'unica domanda che un raccoglitore deve porsi prima di cancellare.
    pub fn raccoglie(&self) -> bool {
        matches!(self, Self::Raccogli(_))
    }

    /// Identificatore canonico del motivo, per log e audit.
    pub fn motivo(&self) -> &'static str {
        match self {
            Self::Preserva(t) => t.as_str(),
            Self::Raccogli(c) => c.as_str(),
        }
    }
}

/// Chi sa rispondere alle domande del criterio.
///
/// Tratto e non `struct` perche' [`giudica`] si ferma alla prima prova che
/// regge: i fatti che seguono non vengono nemmeno chiesti, e le due prove
/// costose (DB del progetto) restano non pagate. Vedi la testa del modulo.
#[async_trait::async_trait]
pub trait FattiAllocazione: Send + Sync {
    async fn eta(&self) -> EtaAllocazione;
    async fn autorizzazione(&self) -> AutorizzazionePorta;
    async fn ascolto(&self) -> Ascolto;
    async fn prenotazione(&self) -> StatoPrenotazione;
    async fn impiego(&self) -> ImpiegoDellaLabel;
    async fn riserva(&self) -> RiservaServizio;
}

/// IL CRITERIO. Unico punto in cui si decide se un'allocazione va raccolta, e
/// unica dichiarazione dell'ORDINE delle prove.
///
/// L'ordine non e' arbitrario:
///
///  1. **Grace** prima di tutto: su una riga troppo giovane non si e' ancora
///     formata nessuna prova, e giudicarla significa raccoglierla sempre.
///  2. **Autorizzazione** prima delle prove di vita, e non dopo: quelle dicono
///     «questa riga e' viva, non toccarla», e hanno senso solo per una riga che
///     il progetto ha DIRITTO di avere. Una porta del bucket altrui con un
///     listener sopra e' un artefatto che si difende da solo.
///  3. **Ascolto**: se qualcuno la usa, e' in uso. L'ignoto preserva.
///  4. **Prenotazione**: la terza prova (mig 0741). Un run vivo la tiene.
///  5. **Impiego della label**: un servizio che sta partendo non ha ancora un
///     listener; il raccoglitore dell'avvio questo fatto ce l'ha, il GC no e lo
///     dichiara.
///  6. **Riserva di unit**: un servizio configurato ma fermo conserva la sua
///     porta fra i riavvii.
///
/// Ogni ignoto preserva. L'errore e' orientato: una porta lasciata allocata un
/// giro in piu' costa una porta, una porta tolta a chi la sta usando costa il
/// servizio.
pub async fn giudica(fatti: &dyn FattiAllocazione) -> VerdettoRaccolta {
    if matches!(fatti.eta().await, EtaAllocazione::DentroLaGrace) {
        return VerdettoRaccolta::Preserva(ProvaDiTenuta::Grace);
    }
    if matches!(
        fatti.autorizzazione().await,
        AutorizzazionePorta::NonAutorizzata
    ) {
        return VerdettoRaccolta::Raccogli(CausaRaccolta::PortaNonAutorizzata);
    }
    match fatti.ascolto().await {
        Ascolto::Presente => return VerdettoRaccolta::Preserva(ProvaDiTenuta::Ascolto),
        Ascolto::NonOsservabile(_) => {
            return VerdettoRaccolta::Preserva(ProvaDiTenuta::AscoltoNonOsservabile)
        }
        Ascolto::Nessuno => {}
    }
    if fatti.prenotazione().await.tiene_in_vita() {
        return VerdettoRaccolta::Preserva(ProvaDiTenuta::Prenotazione);
    }
    match fatti.impiego().await {
        ImpiegoDellaLabel::InUso => return VerdettoRaccolta::Preserva(ProvaDiTenuta::LabelInUso),
        ImpiegoDellaLabel::NonInterrogabile(_) => {
            return VerdettoRaccolta::Preserva(ProvaDiTenuta::ImpiegoNonInterrogabile)
        }
        ImpiegoDellaLabel::NessunProcesso | ImpiegoDellaLabel::NonInterrogato => {}
    }
    match fatti.riserva().await {
        RiservaServizio::Riserva => VerdettoRaccolta::Preserva(ProvaDiTenuta::RiservaDiServizio),
        RiservaServizio::NonInterrogabile(_) => {
            VerdettoRaccolta::Preserva(ProvaDiTenuta::RiservaNonInterrogabile)
        }
        RiservaServizio::NessunaUnitDichiarata | RiservaServizio::UnitNonPiuValida => {
            VerdettoRaccolta::Raccogli(CausaRaccolta::NessunaProvaDiVita)
        }
    }
}

/// Cio' che la RIGA di `nexus_port_allocations` dichiara di se'. I due
/// raccoglitori la leggono con la stessa `SELECT`.
#[derive(Debug, Clone)]
pub struct RigaAllocazione {
    pub project_id: Uuid,
    pub porta: u16,
    pub service_unit: Option<String>,
    pub prenotata_da_run: Option<Uuid>,
}

/// Cio' che il CHIAMANTE ha osservato per conto proprio, e che la riga non sa.
#[derive(Debug, Clone)]
pub struct OsservazioniDelChiamante {
    pub eta: EtaAllocazione,
    pub ascolto: Ascolto,
    pub impiego: ImpiegoDellaLabel,
}

/// I fatti come li produce la produzione: la riga piu' le osservazioni del
/// chiamante, con le due prove costose risolte solo se il criterio le chiede.
pub struct FattiDalRegistro<'a> {
    db: &'a PgPool,
    riga: RigaAllocazione,
    osservazioni: OsservazioniDelChiamante,
    vita: &'a dyn VitaDelRun,
}

impl<'a> FattiDalRegistro<'a> {
    pub fn new(
        db: &'a PgPool,
        riga: RigaAllocazione,
        osservazioni: OsservazioniDelChiamante,
        vita: &'a dyn VitaDelRun,
    ) -> Self {
        Self {
            db,
            riga,
            osservazioni,
            vita,
        }
    }
}

#[async_trait::async_trait]
impl FattiAllocazione for FattiDalRegistro<'_> {
    async fn eta(&self) -> EtaAllocazione {
        self.osservazioni.eta
    }

    async fn autorizzazione(&self) -> AutorizzazionePorta {
        if nexus_tool_kit::ports::port_authorized_for_project(
            self.db,
            &self.riga.project_id,
            self.riga.porta,
        )
        .await
        {
            AutorizzazionePorta::Autorizzata
        } else {
            AutorizzazionePorta::NonAutorizzata
        }
    }

    async fn ascolto(&self) -> Ascolto {
        self.osservazioni.ascolto.clone()
    }

    async fn prenotazione(&self) -> StatoPrenotazione {
        stato_prenotazione(
            self.vita,
            self.riga.project_id,
            self.riga.prenotata_da_run,
        )
        .await
    }

    async fn impiego(&self) -> ImpiegoDellaLabel {
        self.osservazioni.impiego.clone()
    }

    async fn riserva(&self) -> RiservaServizio {
        riserva_di_servizio(
            self.db,
            self.riga.project_id,
            self.riga.porta,
            self.riga.service_unit.as_deref(),
        )
        .await
    }
}

/// La domanda completa, come la pongono entrambi i raccoglitori.
pub async fn giudica_riga(
    db: &PgPool,
    riga: RigaAllocazione,
    osservazioni: OsservazioniDelChiamante,
    vita: &dyn VitaDelRun,
) -> VerdettoRaccolta {
    giudica(&FattiDalRegistro::new(db, riga, osservazioni, vita)).await
}

/// La riserva di un servizio configurato, per piattaforma.
///
/// Viveva in `port_registry` accanto al solo GC: qui perche' e' un FATTO del
/// criterio, e il criterio ha due chiamanti (regola L).
async fn riserva_di_servizio(
    db: &PgPool,
    project_id: Uuid,
    porta: u16,
    service_unit: Option<&str>,
) -> RiservaServizio {
    let Some(unit) = service_unit.map(str::trim).filter(|u| !u.is_empty()) else {
        return RiservaServizio::NessunaUnitDichiarata;
    };
    #[cfg(windows)]
    {
        let _ = porta;
        riserva_da_servizio_installato(db, project_id, unit).await
    }
    #[cfg(not(windows))]
    {
        let _ = (db, project_id);
        riserva_dal_file_unit(unit, porta).await
    }
}

/// POSIX: il file unit `~/.config/systemd/user/<unit>` esiste E dichiara ANCORA
/// `porta`. Punto unico del parsing porte: riusa `extract_ports_from_unit_content`.
///
/// File assente = servizio rimosso; porta non piu' dichiarata = mapping stale
/// dopo una riconfigurazione. Entrambi sono `UnitNonPiuValida`, non un ignoto:
/// la lettura e' avvenuta ed ha risposto.
#[cfg(not(windows))]
async fn riserva_dal_file_unit(unit: &str, porta: u16) -> RiservaServizio {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/administrator".to_string());
    let path = format!("{home}/.config/systemd/user/{unit}");
    match tokio::fs::read_to_string(&path).await {
        Ok(content) => {
            if crate::port_registry::extract_ports_from_unit_content(&content).contains(&porta) {
                RiservaServizio::Riserva
            } else {
                RiservaServizio::UnitNonPiuValida
            }
        }
        Err(_) => RiservaServizio::UnitNonPiuValida,
    }
}

/// Windows: NIENTE systemd. Il segnale strutturato (regola M) e' che il servizio
/// dietro l'allocazione ESISTA ancora, cioe' che una label di `agent_processes`
/// (kind='service') ricostruisca quella unit dal punto unico
/// `service_unit_name(slug, label)`. Uninstall e clear-finished cancellano quelle
/// righe -> `UnitNonPiuValida` -> la riga viene raccolta.
///
/// Ogni errore TRANSITORIO (META, pool del progetto, query) e' un ignoto
/// DICHIARATO: prima usciva come `true`, indistinguibile da una riserva vera.
#[cfg(windows)]
async fn riserva_da_servizio_installato(
    db: &PgPool,
    project_id: Uuid,
    unit: &str,
) -> RiservaServizio {
    let name = match sqlx::query_scalar::<_, String>("SELECT name FROM projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(db)
        .await
    {
        Ok(Some(n)) => n,
        // Progetto realmente rimosso: la riserva e' orfana.
        Ok(None) => return RiservaServizio::UnitNonPiuValida,
        Err(e) => return RiservaServizio::NonInterrogabile(e.to_string()),
    };
    let slug = crate::project_workspace::services::project_service_slug(&name);
    // `agent_processes` e' tabella migrata: instrada sul pool del progetto.
    let proj_pool = match crate::project_db_routes::project_data_pool_from(db, project_id).await {
        Ok(p) => p,
        Err(e) => return RiservaServizio::NonInterrogabile(e.to_string()),
    };
    // La domanda e' «esiste un servizio INSTALLATO con questa identita'?», non
    // «e' MAI esistita una riga con questa label»: `agent_processes` e'
    // append-only, e la seconda e' vera per costruzione e per sempre.
    // `visible_windows_services` e' lo stesso criterio con cui il pannello
    // decide cosa mostrare, quindi il GC e il pannello non hanno due idee di
    // quali servizi esistano.
    let righe: Vec<(String, String, chrono::DateTime<chrono::Utc>)> = match sqlx::query_as(
        "SELECT label, status, created_at FROM agent_processes \
          WHERE project_id = $1 AND kind = 'service' \
          ORDER BY label, created_at DESC",
    )
    .bind(project_id)
    .fetch_all(&proj_pool)
    .await
    {
        Ok(r) => r,
        Err(e) => return RiservaServizio::NonInterrogabile(e.to_string()),
    };
    let vive: Vec<String> =
        crate::project_workspace::services::visible_windows_services(&righe, project_id)
            .into_iter()
            .map(|(label, _running)| label)
            .collect();
    if crate::port_registry::windows_unit_backed_by_label(&slug, unit, &vive) {
        RiservaServizio::Riserva
    } else {
        RiservaServizio::UnitNonPiuValida
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// I fatti gia' noti, per provare il CRITERIO senza I/O. Non e' una seconda
    /// sorgente: implementa lo stesso tratto che la produzione implementa, e i
    /// test passano dalla stessa [`giudica`].
    struct FattiDichiarati {
        eta: EtaAllocazione,
        autorizzazione: AutorizzazionePorta,
        ascolto: Ascolto,
        prenotazione: StatoPrenotazione,
        impiego: ImpiegoDellaLabel,
        riserva: RiservaServizio,
    }

    impl Default for FattiDichiarati {
        /// Il caso in cui NIENTE tiene in vita la riga: ogni test accende una
        /// prova per volta, cosi' cio' che sta misurando e' quella.
        fn default() -> Self {
            Self {
                eta: EtaAllocazione::OltreLaGrace,
                autorizzazione: AutorizzazionePorta::Autorizzata,
                ascolto: Ascolto::Nessuno,
                prenotazione: StatoPrenotazione::Assente,
                impiego: ImpiegoDellaLabel::NonInterrogato,
                riserva: RiservaServizio::NessunaUnitDichiarata,
            }
        }
    }

    #[async_trait::async_trait]
    impl FattiAllocazione for FattiDichiarati {
        async fn eta(&self) -> EtaAllocazione {
            self.eta
        }
        async fn autorizzazione(&self) -> AutorizzazionePorta {
            self.autorizzazione
        }
        async fn ascolto(&self) -> Ascolto {
            self.ascolto.clone()
        }
        async fn prenotazione(&self) -> StatoPrenotazione {
            self.prenotazione.clone()
        }
        async fn impiego(&self) -> ImpiegoDellaLabel {
            self.impiego.clone()
        }
        async fn riserva(&self) -> RiservaServizio {
            self.riserva.clone()
        }
    }

    #[tokio::test]
    async fn senza_nessuna_prova_la_riga_si_raccoglie() {
        assert_eq!(
            giudica(&FattiDichiarati::default()).await,
            VerdettoRaccolta::Raccogli(CausaRaccolta::NessunaProvaDiVita)
        );
    }

    /// Ognuna delle prove, DA SOLA, basta a preservare — ed e' il senso stesso
    /// del punto unico: chi delega qui le ottiene tutte, comprese quelle che il
    /// proprio criterio non aveva.
    #[tokio::test]
    async fn ogni_prova_da_sola_preserva() {
        let run = Uuid::new_v4();
        let casi: Vec<(FattiDichiarati, ProvaDiTenuta)> = vec![
            (
                FattiDichiarati {
                    eta: EtaAllocazione::DentroLaGrace,
                    ..Default::default()
                },
                ProvaDiTenuta::Grace,
            ),
            (
                FattiDichiarati {
                    ascolto: Ascolto::Presente,
                    ..Default::default()
                },
                ProvaDiTenuta::Ascolto,
            ),
            (
                FattiDichiarati {
                    prenotazione: StatoPrenotazione::RunVivo(run),
                    ..Default::default()
                },
                ProvaDiTenuta::Prenotazione,
            ),
            (
                FattiDichiarati {
                    impiego: ImpiegoDellaLabel::InUso,
                    ..Default::default()
                },
                ProvaDiTenuta::LabelInUso,
            ),
            (
                FattiDichiarati {
                    riserva: RiservaServizio::Riserva,
                    ..Default::default()
                },
                ProvaDiTenuta::RiservaDiServizio,
            ),
        ];
        for (fatti, atteso) in casi {
            let verdetto = giudica(&fatti).await;
            assert_eq!(
                verdetto,
                VerdettoRaccolta::Preserva(atteso),
                "prova attesa: {}",
                atteso.as_str()
            );
            assert!(!verdetto.raccoglie());
        }
    }

    /// Ogni IGNOTO preserva, e ognuno dichiara QUALE domanda e' rimasta senza
    /// risposta: «preservata» e basta non distingue una porta trattenuta da una
    /// che nessuno ha potuto guardare.
    #[tokio::test]
    async fn ogni_ignoto_preserva_e_dice_quale() {
        let run = Uuid::new_v4();
        let casi: Vec<(FattiDichiarati, ProvaDiTenuta)> = vec![
            (
                FattiDichiarati {
                    ascolto: Ascolto::NonOsservabile("ss assente".into()),
                    ..Default::default()
                },
                ProvaDiTenuta::AscoltoNonOsservabile,
            ),
            (
                FattiDichiarati {
                    prenotazione: StatoPrenotazione::NonInterrogabile {
                        run_id: run,
                        causa: "DB progetto irraggiungibile".into(),
                    },
                    ..Default::default()
                },
                ProvaDiTenuta::Prenotazione,
            ),
            (
                FattiDichiarati {
                    impiego: ImpiegoDellaLabel::NonInterrogabile("pool assente".into()),
                    ..Default::default()
                },
                ProvaDiTenuta::ImpiegoNonInterrogabile,
            ),
            (
                FattiDichiarati {
                    riserva: RiservaServizio::NonInterrogabile("META transitorio".into()),
                    ..Default::default()
                },
                ProvaDiTenuta::RiservaNonInterrogabile,
            ),
        ];
        for (fatti, atteso) in casi {
            assert_eq!(
                giudica(&fatti).await,
                VerdettoRaccolta::Preserva(atteso),
                "l'ignoto non distrugge: {}",
                atteso.as_str()
            );
        }
    }

    /// «Non ho chiesto» non e' «non ho potuto chiedere». Il GC dichiara
    /// `NonInterrogato` e il suo criterio prosegue; il raccoglitore dell'avvio,
    /// se il DB del progetto non risponde, dichiara `NonInterrogabile` e
    /// preserva. Con un solo vocabolario per i due casi, il GC preserverebbe
    /// TUTTO — cioe' non raccoglierebbe piu' niente.
    #[tokio::test]
    async fn non_interrogato_non_e_non_interrogabile() {
        assert!(giudica(&FattiDichiarati {
            impiego: ImpiegoDellaLabel::NonInterrogato,
            ..Default::default()
        })
        .await
        .raccoglie());
        assert!(!giudica(&FattiDichiarati {
            impiego: ImpiegoDellaLabel::NonInterrogabile("x".into()),
            ..Default::default()
        })
        .await
        .raccoglie());
    }

    /// L'autorizzazione precede le prove di vita: un listener su una porta del
    /// bucket altrui difende un artefatto proprio mentre fa danno.
    ///
    /// MUTAZIONE che rende rosso: spostare il controllo dell'autorizzazione
    /// dopo l'ascolto in [`giudica`].
    #[tokio::test]
    async fn la_porta_non_autorizzata_si_raccoglie_anche_se_qualcuno_ascolta() {
        let verdetto = giudica(&FattiDichiarati {
            autorizzazione: AutorizzazionePorta::NonAutorizzata,
            ascolto: Ascolto::Presente,
            prenotazione: StatoPrenotazione::RunVivo(Uuid::new_v4()),
            riserva: RiservaServizio::Riserva,
            ..Default::default()
        })
        .await;
        assert_eq!(
            verdetto,
            VerdettoRaccolta::Raccogli(CausaRaccolta::PortaNonAutorizzata)
        );
    }

    /// La grace precede l'autorizzazione: e' il predicato con cui il GC filtra
    /// le righe, e una riga che non ha ancora avuto il tempo di formare prove
    /// non si giudica affatto.
    #[tokio::test]
    async fn la_grace_precede_tutto() {
        assert_eq!(
            giudica(&FattiDichiarati {
                eta: EtaAllocazione::DentroLaGrace,
                autorizzazione: AutorizzazionePorta::NonAutorizzata,
                ..Default::default()
            })
            .await,
            VerdettoRaccolta::Preserva(ProvaDiTenuta::Grace)
        );
    }

    #[test]
    fn l_eta_si_misura_dalla_creazione() {
        let adesso = chrono::Utc::now();
        assert_eq!(
            EtaAllocazione::da_creazione(adesso - chrono::Duration::seconds(10), 180, adesso),
            EtaAllocazione::DentroLaGrace
        );
        assert_eq!(
            EtaAllocazione::da_creazione(adesso - chrono::Duration::seconds(600), 180, adesso),
            EtaAllocazione::OltreLaGrace
        );
        // Grace a zero: nessuna riga e' troppo giovane.
        assert_eq!(
            EtaAllocazione::da_creazione(adesso, 0, adesso),
            EtaAllocazione::OltreLaGrace
        );
    }

    /// Il bind e la fotografia dei listener rispondono alla STESSA domanda con
    /// due strumenti: la traduzione sta in un posto solo, o i due raccoglitori
    /// tornerebbero a chiamare «libera» cose diverse.
    #[test]
    fn le_due_osservazioni_dell_ascolto_dicono_la_stessa_cosa() {
        assert_eq!(Ascolto::da_bind(&PortBind::Libera), Ascolto::Nessuno);
        assert_eq!(Ascolto::da_bind(&PortBind::Occupata), Ascolto::Presente);
        assert!(matches!(
            Ascolto::da_bind(&PortBind::NonInterrogabile {
                codice: Some(10055),
                errore: "porte effimere esaurite".into()
            }),
            Ascolto::NonOsservabile(_)
        ));

        let scan = ListenerScan::Osservati(vec![(31904, 0, String::new())]);
        assert_eq!(Ascolto::da_scan(&scan, 31904), Ascolto::Presente);
        assert_eq!(Ascolto::da_scan(&scan, 31905), Ascolto::Nessuno);
        assert!(matches!(
            Ascolto::da_scan(
                &ListenerScan::NonInterrogabile {
                    motivo: "ss assente".into()
                },
                31904
            ),
            Ascolto::NonOsservabile(_)
        ));
    }
}

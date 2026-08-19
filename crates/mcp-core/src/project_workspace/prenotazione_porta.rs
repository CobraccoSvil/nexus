//! PRENOTAZIONE di una porta: chi la tiene in vita, e finche' quando.
//!
//! # La domanda a cui questo modulo risponde
//!
//! «Questa riga di `nexus_port_allocations` e' una PROMESSA ancora valida, o il
//! RESIDUO di un tentativo fallito?» Prima non era ponibile, e le due cose
//! erano lo stesso byte.
//!
//! Il GC (`crate::port_registry::allocazione_da_preservare`) accettava due sole
//! prove di vita, entrambe OSSERVATE o DERIVATE DALL'AVVIO: un listener TCP
//! sulla porta, oppure la colonna `service_unit` che
//! `allocate_port::link_allocation_to_service_unit` scrive quando il servizio
//! parte. Una riga appena creata da `request_port` non ha ne' l'una ne'
//! l'altra PER COSTRUZIONE — il servizio non e' ancora nato — quindi cadeva
//! esattamente nella definizione di orfana.
//!
//! MISURATO il 18/08/2026 su biblioteca-18-08: porte 34184 e 34150 promesse
//! alle 20:49:28, «port_gc: rilasciate 2 allocazioni orfane» alle 20:54:16,
//! e il gate duale che cinque minuti dopo rifiuta l'avvio del backend perche'
//! «non risulta alcuna allocazione di porta per il servizio». Il circolo: la
//! porta entra durevolmente nel registro solo quando il servizio parte, e il
//! servizio non parte perche' il registro e' vuoto.
//!
//! # Il criterio, e perche' non e' un timer
//!
//! Una prenotazione e' viva finche' e' vivo il RUN che l'ha chiesta. Non e' una
//! grace piu' lunga sotto un altro nome: un run che chiude — completed, blocked
//! o interrupted — libera la porta subito, e un run che dura un'ora la tiene
//! per un'ora senza che nessuno debba indovinare un numero. Il run che muore da
//! FUORI (servizio riavviato, task tokio sparito) lo riconcilia
//! `crate::run_reaper`, che lo marca `interrupted`: la prenotazione non
//! sopravvive al proprio proprietario perche' esiste gia' chi lo seppellisce.
//!
//! # Perche' una PORTA e non una query in linea
//!
//! `agent_runs` vive nel DB del PROGETTO (mig 0507), `nexus_port_allocations`
//! nel META: la domanda attraversa due database e la risolve
//! `project_db_routes`. Una query in linea nel GC renderebbe il criterio
//! verificabile solo su un ambiente con la directory di routing popolata, e un
//! test che non riesce ad aprire il DB del progetto otterrebbe
//! [`StatoPrenotazione::NonInterrogabile`] — cioe' un VERDE per fail-closed
//! invece che per il criterio (regola O). La porta separa il fatto dal giudizio.

use sqlx::PgPool;
use uuid::Uuid;

/// Che cosa il REGISTRO dichiara di sapere su una riga di
/// `nexus_port_allocations`: chi la tiene in vita.
///
/// NON e' l'elenco di tutto cio' che puo' salvarla dal GC — il listener TCP e'
/// un'osservazione del SO, non una dichiarazione del registro, e vive dove si
/// osserva. Questa e' la risposta che `request_port` puo' dare al modello
/// SENZA interrogare il sistema operativo, ed e' cio' che rende la promessa
/// verificabile: il numero da solo non dice se durera' (regola Q).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenutaAllocazione {
    /// Un run l'ha prenotata: vive quanto lui.
    PrenotataDaRun(Uuid),
    /// La riga e' legata alla unit di un servizio: e' la riserva di un servizio
    /// configurato, e il GC la preserva anche da fermo.
    UnitDiServizio,
    /// Riserva esplicita di una persona (`allocation_mode = 'manual'`): il GC
    /// non la carica nemmeno.
    Manuale,
    /// NIENTE la tiene in vita. La riga esiste adesso e il GC la raccogliera'
    /// alla prima passata oltre la grace se nessuno si mettera' in ascolto.
    Nessuna,
}

impl TenutaAllocazione {
    /// Identificatore canonico per il wire (regola N).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PrenotataDaRun(_) => "prenotata_da_run",
            Self::UnitDiServizio => "unit_di_servizio",
            Self::Manuale => "manuale",
            Self::Nessuna => "nessuna",
        }
    }

    /// Vero se il registro sa dire perche' questa riga non sparira'.
    ///
    /// E' l'unica domanda che `request_port` deve porsi per sapere se puo'
    /// promettere: `false` significa «ho un numero, non ho un'assegnazione».
    pub fn e_ancorata(&self) -> bool {
        !matches!(self, Self::Nessuna)
    }
}

/// Che cosa si e' potuto sapere del run che ha prenotato una porta.
///
/// Le varianti hanno rimedi diversi e non collassano (regola Q): «nessuno l'ha
/// prenotata» e «l'ha prenotata un run che non riesco a interrogare» portano a
/// decisioni opposte, e leggerle come lo stesso fatto renderebbe invisibile un
/// DB di progetto irraggiungibile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatoPrenotazione {
    /// La colonna `prenotata_da_run` e' NULL: riga nata fuori da un run, o
    /// antecedente alla mig 0741.
    Assente,
    /// Il run che l'ha chiesta e' ancora attivo (punto unico
    /// [`crate::agent_types::ACTIVE_RUN_STATUSES`]).
    RunVivo(Uuid),
    /// Il run e' chiuso: la prenotazione non tiene piu' niente.
    RunChiuso { run_id: Uuid, stato: String },
    /// La prenotazione nomina un run che il DB del progetto non conosce
    /// (progetto ripristinato, run mai persistito). Non e' un ignoto: e' una
    /// risposta, ed e' «no».
    RunSconosciuto(Uuid),
    /// Non si e' potuto chiedere: DB del progetto irraggiungibile o query
    /// fallita. «Non ho guardato» non e' «e' morto».
    NonInterrogabile { run_id: Uuid, causa: String },
}

impl StatoPrenotazione {
    /// Vero se questa prenotazione, DA SOLA, vieta al GC di rilasciare la riga.
    ///
    /// Fail-closed sull'ignoto, con la stessa disciplina del ramo Windows di
    /// `allocazione_da_preservare`: una porta lasciata allocata un giro in piu'
    /// costa una porta, una porta tolta a un run vivo costa il run.
    pub fn tiene_in_vita(&self) -> bool {
        matches!(self, Self::RunVivo(_) | Self::NonInterrogabile { .. })
    }

    /// Identificatore canonico per log e audit (regola N).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Assente => "assente",
            Self::RunVivo(_) => "run_vivo",
            Self::RunChiuso { .. } => "run_chiuso",
            Self::RunSconosciuto(_) => "run_sconosciuto",
            Self::NonInterrogabile { .. } => "non_interrogabile",
        }
    }
}

/// Cio' che il DB del progetto ha risposto sul run. FATTO, non giudizio: la
/// porta lo porta, [`classifica_prenotazione`] lo interpreta.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EsitoInterrogazioneRun {
    /// La riga esiste e porta questo `status`.
    Stato(String),
    /// Nessuna riga con quell'id.
    RunAssente,
    /// Non si e' potuto chiedere.
    NonInterrogabile(String),
}

/// IL CRITERIO, puro: dati la colonna e la risposta del DB del progetto, che
/// cos'e' questa prenotazione.
///
/// «Attivo» lo decide il punto unico [`crate::agent_types::is_active_run_status`]
/// e non un elenco ricopiato qui: uno stato sospeso-vivo aggiunto li' vale
/// automaticamente anche per le porte prenotate.
pub fn classifica_prenotazione(
    prenotata_da: Option<Uuid>,
    esito: EsitoInterrogazioneRun,
) -> StatoPrenotazione {
    let Some(run_id) = prenotata_da else {
        return StatoPrenotazione::Assente;
    };
    match esito {
        EsitoInterrogazioneRun::Stato(stato) => {
            if crate::agent_types::is_active_run_status(&stato) {
                StatoPrenotazione::RunVivo(run_id)
            } else {
                StatoPrenotazione::RunChiuso { run_id, stato }
            }
        }
        EsitoInterrogazioneRun::RunAssente => StatoPrenotazione::RunSconosciuto(run_id),
        EsitoInterrogazioneRun::NonInterrogabile(causa) => {
            StatoPrenotazione::NonInterrogabile { run_id, causa }
        }
    }
}

/// Chi sa dire in che stato e' un run del DB di progetto.
///
/// Porta e non query in linea: vedi il perche' in testa al modulo.
#[async_trait::async_trait]
pub trait VitaDelRun: Send + Sync {
    async fn interroga(&self, project_id: Uuid, run_id: Uuid) -> EsitoInterrogazioneRun;
}

/// L'implementazione di produzione: risolve il pool del progetto (punto unico
/// `project_db_routes`) e legge `agent_runs.status`.
#[derive(Debug, Clone)]
pub struct RunsDelDbDiProgetto {
    meta: PgPool,
}

impl RunsDelDbDiProgetto {
    pub fn new(meta: PgPool) -> Self {
        Self { meta }
    }
}

#[async_trait::async_trait]
impl VitaDelRun for RunsDelDbDiProgetto {
    async fn interroga(&self, project_id: Uuid, run_id: Uuid) -> EsitoInterrogazioneRun {
        let pool =
            match crate::project_db_routes::project_data_pool_from(&self.meta, project_id).await {
                Ok(p) => p,
                Err(e) => return EsitoInterrogazioneRun::NonInterrogabile(e.to_string()),
            };
        match sqlx::query_scalar::<_, String>("SELECT status FROM agent_runs WHERE id = $1")
            .bind(run_id)
            .fetch_optional(&pool)
            .await
        {
            Ok(Some(stato)) => EsitoInterrogazioneRun::Stato(stato),
            Ok(None) => EsitoInterrogazioneRun::RunAssente,
            Err(e) => EsitoInterrogazioneRun::NonInterrogabile(e.to_string()),
        }
    }
}

/// La domanda completa: legge la colonna e la classifica. `prenotata_da` NULL
/// non produce nessun I/O — il DB del progetto non si apre per una riga che
/// nessuno ha prenotato.
pub async fn stato_prenotazione(
    vita: &dyn VitaDelRun,
    project_id: Uuid,
    prenotata_da: Option<Uuid>,
) -> StatoPrenotazione {
    let Some(run_id) = prenotata_da else {
        return StatoPrenotazione::Assente;
    };
    classifica_prenotazione(Some(run_id), vita.interroga(project_id, run_id).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Il vocabolario «run attivo» NON e' ricopiato qui: si attraversa il punto
    /// unico, cosi' uno stato sospeso-vivo aggiunto in `agent_types` vale anche
    /// per le porte prenotate senza che nessuno se ne ricordi (regola O).
    #[test]
    fn ogni_stato_attivo_tiene_in_vita_la_prenotazione() {
        let run = Uuid::new_v4();
        for stato in crate::agent_types::ACTIVE_RUN_STATUSES {
            let s = classifica_prenotazione(
                Some(run),
                EsitoInterrogazioneRun::Stato(stato.to_string()),
            );
            assert_eq!(s, StatoPrenotazione::RunVivo(run), "stato attivo: {stato}");
            assert!(s.tiene_in_vita());
        }
    }

    /// Un run chiuso libera la porta SUBITO: e' la differenza fra una
    /// prenotazione e una grace piu' lunga.
    #[test]
    fn un_run_chiuso_non_tiene_piu_niente() {
        let run = Uuid::new_v4();
        for stato in ["completed", "failed", "interrupted", "blocked_needs_input"] {
            let s = classifica_prenotazione(
                Some(run),
                EsitoInterrogazioneRun::Stato(stato.to_string()),
            );
            assert!(!s.tiene_in_vita(), "stato chiuso: {stato}");
            assert_eq!(s.as_str(), "run_chiuso");
        }
    }

    /// «Non ho potuto chiedere» non e' «e' morto»: sull'ignoto non si
    /// distrugge. E resta DISTINGUIBILE da «nessuno l'ha prenotata», che porta
    /// alla decisione opposta.
    #[test]
    fn l_ignoto_preserva_e_l_assenza_no() {
        let run = Uuid::new_v4();
        let ignoto = classifica_prenotazione(
            Some(run),
            EsitoInterrogazioneRun::NonInterrogabile("connessione rifiutata".into()),
        );
        assert!(ignoto.tiene_in_vita());
        assert_eq!(ignoto.as_str(), "non_interrogabile");

        let assente = classifica_prenotazione(None, EsitoInterrogazioneRun::RunAssente);
        assert_eq!(assente, StatoPrenotazione::Assente);
        assert!(!assente.tiene_in_vita());

        // Un run NOMINATO ma sconosciuto e' una risposta, non un ignoto.
        let sconosciuto = classifica_prenotazione(Some(run), EsitoInterrogazioneRun::RunAssente);
        assert_eq!(sconosciuto, StatoPrenotazione::RunSconosciuto(run));
        assert!(!sconosciuto.tiene_in_vita());
    }

    /// Solo `Nessuna` non e' un'ancora: e' il caso in cui `request_port` ha un
    /// numero e non un'assegnazione, e deve dirlo.
    #[test]
    fn la_tenuta_dichiara_se_la_promessa_regge() {
        assert!(TenutaAllocazione::PrenotataDaRun(Uuid::new_v4()).e_ancorata());
        assert!(TenutaAllocazione::UnitDiServizio.e_ancorata());
        assert!(TenutaAllocazione::Manuale.e_ancorata());
        assert!(!TenutaAllocazione::Nessuna.e_ancorata());
        assert_eq!(TenutaAllocazione::Nessuna.as_str(), "nessuna");
    }
}

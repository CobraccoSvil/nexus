//! PUNTO UNICO (regola L) della domanda «di CHI e' questo processo?».
//!
//! CAUSA RADICE (incidente gestione-spese 2026-07-28): `find_or_allocate` legava
//! una porta a un servizio dopo aver verificato che il processo trovato fosse nel
//! BUCKET del progetto. Il bucket risponde a «questo processo e' del progetto?»,
//! che e' una domanda piu' LARGA di quella necessaria: allocando per
//! `label=frontend`, il primo processo del bucket era il BACKEND, e la sua porta
//! (33649) diventava l'allocazione del frontend. Nexus iniettava poi quella porta
//! occupata come `PORT`, Vite (senza strictPort) ripiegava su 33650/33651 — FUORI
//! bucket — mentre 33604, la porta legittima del frontend, restava inutilizzata.
//!
//! Qui la domanda si stringe a «questo processo E' il servizio per cui sto
//! allocando?», e la risposta viene da dati STRUTTURATI (regola M): la label della
//! riga `agent_processes` di quel pid e la label dell'allocazione che copre quella
//! porta. Mai dal nome del programma (`node` non dice quale servizio sia) ne' dal
//! testo dei log.
//!
//! Vocabolario riusato, non re-implementato (regola L): `similar_service_labels`
//! e `is_generic_service_label` in `crate::agent_processes` sono gia' il punto
//! unico di «due label indicano lo stesso servizio?» — lo stesso criterio con cui
//! il pannello Servizi raggruppa le label e con cui `stop_similar_running_services`
//! decide chi fermare prima di uno spawn.

use sqlx::PgPool;
use uuid::Uuid;

/// Da quale FATTO strutturato nasce il verdetto. Serve a dichiarare la premessa
/// nei log e nell'audit (regola O: un verdetto senza la sua fonte e' un'opinione).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OwnershipEvidence {
    /// Riga `agent_processes` che porta quel pid (di qualunque status: una riga
    /// gia' marcata 'stopped' identifica comunque il servizio che l'ha avviata).
    TrackedProcess,
    /// Riga `nexus_port_allocations` che copre quella porta nello stesso progetto.
    PortAllocation,
}

/// A quale servizio del progetto appartiene un processo in ascolto.
///
/// `Unknown` NON e' un ripiego permissivo: e' il verdetto che impedisce di legare
/// una porta a un servizio sulla base di un'appartenenza mai accertata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ServiceOwnership {
    /// Prova positiva: il processo E' il servizio richiesto.
    Own { evidence: OwnershipEvidence },
    /// Prova positiva che il processo e' di un ALTRO servizio del progetto.
    Other {
        label: String,
        evidence: OwnershipEvidence,
    },
    /// Nessun dato strutturato lega il processo a una label: appartenenza IGNOTA.
    Unknown,
}

impl ServiceOwnership {
    pub(super) fn is_own(&self) -> bool {
        matches!(self, ServiceOwnership::Own { .. })
    }

    /// Etichetta breve per log e audit: dichiara il verdetto E la sua fonte.
    pub(super) fn reason(&self) -> String {
        match self {
            ServiceOwnership::Own { evidence } => format!("own:{}", evidence.as_str()),
            ServiceOwnership::Other { label, evidence } => {
                format!("other({label}):{}", evidence.as_str())
            }
            ServiceOwnership::Unknown => "unknown".to_string(),
        }
    }
}

impl OwnershipEvidence {
    fn as_str(self) -> &'static str {
        match self {
            OwnershipEvidence::TrackedProcess => "agent_processes",
            OwnershipEvidence::PortAllocation => "port_allocation",
        }
    }
}

/// Fatti strutturati su cui si decide l'appartenenza. I costruttori dichiarano la
/// PREMESSA: quali prove sono pertinenti dipende da quale porta si sta guardando,
/// e passare la prova sbagliata riprodurrebbe il difetto.
pub(super) struct OwnershipFacts {
    /// `agent_processes.label` della riga piu' recente con quel pid.
    process_label: Option<String>,
    /// `nexus_port_allocations.label` della riga che copre quella porta.
    port_label: Option<String>,
}

impl OwnershipFacts {
    /// Processo in ascolto sulla porta DELL'ALLOCAZIONE che stiamo risolvendo.
    ///
    /// Qui la label dell'allocazione NON e' una prova: quella porta e' registrata
    /// alla label richiesta per costruzione, quindi userebbe come prova cio' che
    /// deve dimostrare — ogni occupante risulterebbe «mio», che e' esattamente il
    /// difetto da chiudere. Solo il pid puo' dire di chi e' il PROCESSO.
    pub(super) fn own_port_occupant(process_label: Option<String>) -> Self {
        Self {
            process_label,
            port_label: None,
        }
    }

    /// Processo in ascolto su una porta DIVERSA da quella richiesta (candidato
    /// all'adozione). Qui entrambe le prove sono pertinenti: la riga del pid dice
    /// chi ha avviato il processo, quella della porta a quale servizio la porta e'
    /// gia' registrata.
    pub(super) fn foreign_port_listener(
        process_label: Option<String>,
        port_label: Option<String>,
    ) -> Self {
        Self {
            process_label,
            port_label,
        }
    }
}

/// Una label prova un'identita' solo se dice uno scopo: `""` e le generiche
/// ("Service", "dev-server") non identificano nulla, quindi non provano ne'
/// l'appartenenza ne' l'estraneita' (stesso vocabolario con cui
/// `resolve_service_label` rifiuta di dare un nome generico a un servizio).
fn identifying_label(label: Option<&str>) -> Option<&str> {
    let l = label.map(str::trim).filter(|l| !l.is_empty())?;
    if crate::agent_processes::is_generic_service_label(l) {
        return None;
    }
    Some(l)
}

/// Parte PURA della decisione (testabile senza DB): dati i fatti letti, di chi e'
/// il processo? Le prove si consultano dalla piu' diretta (chi ha avviato QUEL
/// processo) alla piu' indiretta (a chi e' registrata QUELLA porta).
pub(super) fn classify_ownership(
    requested_label: &str,
    facts: &OwnershipFacts,
) -> ServiceOwnership {
    // Una richiesta senza identita' non puo' accertare alcuna appartenenza: cio'
    // che il sistema rifiuta di dare come nome a un servizio non puo' nemmeno
    // usarlo come prova di chi sia un processo.
    let Some(requested) = identifying_label(Some(requested_label)) else {
        return ServiceOwnership::Unknown;
    };

    let prove = [
        (
            facts.process_label.as_deref(),
            OwnershipEvidence::TrackedProcess,
        ),
        (
            facts.port_label.as_deref(),
            OwnershipEvidence::PortAllocation,
        ),
    ];
    for (label, evidence) in prove {
        let Some(found) = identifying_label(label) else {
            continue;
        };
        return if crate::agent_processes::similar_service_labels(requested, found) {
            ServiceOwnership::Own { evidence }
        } else {
            ServiceOwnership::Other {
                label: found.to_string(),
                evidence,
            }
        };
    }
    ServiceOwnership::Unknown
}

/// `agent_processes.label` della riga piu' recente con quel pid. `None` quando il
/// DB del progetto non e' leggibile: un fatto non letto non e' una prova, e il
/// verdetto che ne segue (`Unknown`) e' gia' quello conservativo.
async fn process_label_for_pid(db: &PgPool, project_id: Uuid, pid: u32) -> Option<String> {
    let proj_pool = match crate::project_db_routes::project_data_pool_from(db, project_id).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                project_id = %project_id,
                pid,
                error = %e,
                "service_ownership: DB progetto non disponibile, appartenenza del processo non accertabile"
            );
            return None;
        }
    };
    sqlx::query_scalar::<_, String>(
        "SELECT label FROM agent_processes WHERE project_id = $1 AND pid = $2 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(project_id)
    .bind(pid as i64)
    .fetch_optional(&proj_pool)
    .await
    .ok()
    .flatten()
}

/// `nexus_port_allocations.label` (DB meta) della riga che copre quella porta.
async fn port_label_for_port(db: &PgPool, project_id: Uuid, port: u16) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT label FROM nexus_port_allocations WHERE project_id = $1 AND port = $2 LIMIT 1",
    )
    .bind(project_id)
    .bind(port as i32)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

/// Appartenenza del processo che occupa la porta GIA' allocata a `requested_label`.
pub(super) async fn ownership_of_occupant(
    db: &PgPool,
    project_id: Uuid,
    pid: u32,
    requested_label: &str,
) -> ServiceOwnership {
    let facts = OwnershipFacts::own_port_occupant(process_label_for_pid(db, project_id, pid).await);
    classify_ownership(requested_label, &facts)
}

/// Appartenenza di un processo in ascolto su un'ALTRA porta del bucket (candidato
/// all'adozione).
pub(super) async fn ownership_of_bucket_listener(
    db: &PgPool,
    project_id: Uuid,
    pid: u32,
    port: u16,
    requested_label: &str,
) -> ServiceOwnership {
    let facts = OwnershipFacts::foreign_port_listener(
        process_label_for_pid(db, project_id, pid).await,
        port_label_for_port(db, project_id, port).await,
    );
    classify_ownership(requested_label, &facts)
}

/// Un processo in ascolto candidato a essere legato al servizio, col verdetto di
/// appartenenza gia' risolto. Non necessariamente un orfano: nel ramo «nessuna
/// riga per questa label» i candidati sono TUTTE le risorse in ascolto del
/// progetto, registrate o meno.
pub(super) struct ListenerFacts {
    pub port: u16,
    pub pid: u32,
    pub program: String,
    pub ownership: ServiceOwnership,
}

/// PUNTO UNICO della domanda «fra questi processi in ascolto, quale e' il mio?».
/// Ritorna il primo la cui appartenenza e' PROVATA; `None` significa che nessuno
/// lo e' — non «nessuno c'era».
///
/// NON filtra per aspetto del programma: il filtro `looks_like_server_process`
/// serve solo dove l'unica informazione disponibile e' il nome del processo, e
/// applicarlo qui scarterebbe un servizio registrato il cui comando non somiglia
/// a un server. Un tool qualsiasi in ascolto nel bucket viene comunque escluso
/// dal verdetto (`Unknown`), non dall'aspetto.
pub(super) fn owned_listener<'a>(
    candidates: impl IntoIterator<Item = &'a ListenerFacts>,
) -> Option<&'a ListenerFacts> {
    candidates.into_iter().find(|c| c.ownership.is_own())
}

/// Esito del ramo «allocazione stantia»: quale porta finisce legata al servizio.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum StaleAdoption {
    /// Un processo del bucket appartiene DAVVERO al servizio richiesto: si adotta
    /// la porta su cui sta realmente ascoltando.
    AdoptOrphan { port: u16, pid: u32 },
    /// Nessun processo dimostrabilmente del servizio: si riusa la porta stantia.
    /// Non e' un ripiego triste — e' la porta CORRETTA: appartiene al servizio per
    /// registro, sta nel bucket del progetto e il probe TCP l'ha appena mostrata
    /// libera. Una porta libera in piu' non costa nulla; una porta condivisa fra
    /// due servizi produce l'incidente che questo modulo chiude.
    ReuseStale { port: u16 },
}

/// Decisione di adozione quando l'allocazione della label e' stantia: quale porta
/// resta legata al servizio. Delega la scelta a `owned_listener` e aggiunge il
/// solo pre-filtro pertinente a questo ramo — i candidati sono processi NON
/// registrati, e per un processo di cui non si sa nulla il nome del programma e'
/// l'unica cosa che distingue un server da un tool qualsiasi. Resta un filtro,
/// mai una prova di identita' (regola M: il nome e' testo, l'appartenenza e' un
/// dato).
pub(super) fn resolve_stale_adoption(
    stale_port: u16,
    candidates: &[ListenerFacts],
) -> StaleAdoption {
    let servers = candidates
        .iter()
        .filter(|c| super::port_recovery::looks_like_server_process(&c.program));
    match owned_listener(servers) {
        Some(c) => StaleAdoption::AdoptOrphan {
            port: c.port,
            pid: c.pid,
        },
        None => StaleAdoption::ReuseStale { port: stale_port },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// I verdetti dei test nascono SEMPRE da `classify_ownership` a partire dalle
    /// label grezze (regola O: si attraversa il produttore). Scrivere a mano
    /// `ServiceOwnership::Other` fisserebbe come premessa proprio cio' che il fix
    /// deve dimostrare.
    fn orphan(
        port: u16,
        pid: u32,
        program: &str,
        requested: &str,
        facts: OwnershipFacts,
    ) -> ListenerFacts {
        ListenerFacts {
            port,
            pid,
            program: program.to_string(),
            ownership: classify_ownership(requested, &facts),
        }
    }

    #[test]
    fn processo_di_un_altro_servizio_non_e_mio() {
        // L'incidente: si alloca per "frontend", il processo del bucket e' il
        // backend (riga agent_processes con label 'backend').
        let facts = OwnershipFacts::foreign_port_listener(Some("backend".to_string()), None);
        assert_eq!(
            classify_ownership("frontend", &facts),
            ServiceOwnership::Other {
                label: "backend".to_string(),
                evidence: OwnershipEvidence::TrackedProcess,
            }
        );
    }

    #[test]
    fn processo_dello_stesso_servizio_e_mio() {
        // Variante del contorno della label: stesso servizio (vocabolario
        // `similar_service_labels`, gia' usato dal pannello e da stop_similar).
        let facts = OwnershipFacts::foreign_port_listener(Some("frontend-dev".to_string()), None);
        assert_eq!(
            classify_ownership("frontend", &facts),
            ServiceOwnership::Own {
                evidence: OwnershipEvidence::TrackedProcess,
            }
        );
    }

    #[test]
    fn porta_registrata_ad_altri_prova_l_estraneita() {
        // Nessuna riga per il pid (processo avviato fuori Nexus), ma la porta e'
        // gia' registrata al backend: prova sufficiente per NON adottarla.
        let facts = OwnershipFacts::foreign_port_listener(None, Some("backend".to_string()));
        assert_eq!(
            classify_ownership("frontend", &facts),
            ServiceOwnership::Other {
                label: "backend".to_string(),
                evidence: OwnershipEvidence::PortAllocation,
            }
        );
    }

    #[test]
    fn senza_fatti_l_appartenenza_resta_ignota() {
        let facts = OwnershipFacts::foreign_port_listener(None, None);
        assert_eq!(
            classify_ownership("frontend", &facts),
            ServiceOwnership::Unknown
        );
        // Una label generica non identifica uno scopo: non prova nulla, ne' come
        // richiesta ne' come fatto trovato.
        let generica = OwnershipFacts::foreign_port_listener(Some("Service".to_string()), None);
        assert_eq!(
            classify_ownership("frontend", &generica),
            ServiceOwnership::Unknown
        );
        assert_eq!(
            classify_ownership(
                "Service",
                &OwnershipFacts::foreign_port_listener(Some("backend".to_string()), None)
            ),
            ServiceOwnership::Unknown
        );
    }

    /// La premessa dei due costruttori, resa verificabile: sulla porta della
    /// PROPRIA allocazione la label della porta non entra come prova. Se entrasse,
    /// ogni occupante risulterebbe «mio» — il difetto originale.
    #[test]
    fn sulla_propria_porta_la_registrazione_non_e_prova() {
        // Stessa situazione, due premesse: il processo e' del backend, la porta e'
        // registrata al frontend (che e' chi sta allocando).
        let occupante = OwnershipFacts::own_port_occupant(Some("backend".to_string()));
        assert!(
            !classify_ownership("frontend", &occupante).is_own(),
            "l'occupante della propria porta e' del backend: non e' mio"
        );
    }

    /// REGRESSIONE (caso minimo, regola O): il processo del servizio A in ascolto
    /// nel bucket NON deve diventare l'allocazione del servizio B, e B deve
    /// ottenere una porta DEL BUCKET diversa da quella di A.
    #[test]
    fn il_backend_del_bucket_non_diventa_la_porta_del_frontend() {
        // Progetto gestione-spese: bucket 33600-33649, frontend allocato su 33604
        // (stantia, nessun listener), backend vivo su 33649 (pid 10988, node).
        let project = Uuid::parse_str("39802bb6-9540-4d70-82c1-fcf35c3a9b65").expect("uuid");
        let stale_port = 33604u16;
        let candidati = vec![orphan(
            33649,
            10988,
            "node",
            "frontend",
            OwnershipFacts::foreign_port_listener(Some("backend".to_string()), None),
        )];

        let esito = resolve_stale_adoption(stale_port, &candidati);

        assert_eq!(
            esito,
            StaleAdoption::ReuseStale { port: 33604 },
            "il processo del backend non deve diventare l'allocazione del frontend"
        );
        let StaleAdoption::ReuseStale { port } = esito else {
            unreachable!()
        };
        assert_ne!(
            port, 33649,
            "la porta del backend non va legata al frontend"
        );
        assert!(
            nexus_tool_kit::ports::port_in_project_bucket(&project, port),
            "il servizio deve comunque ottenere una porta DEL bucket del progetto"
        );
    }

    #[test]
    fn il_processo_del_servizio_stesso_viene_adottato() {
        // Caso legittimo che l'adozione deve continuare a coprire: il MIO servizio
        // e' vivo su una porta diversa da quella registrata (riga stantia).
        let candidati = vec![orphan(
            33610,
            4242,
            "node",
            "frontend",
            OwnershipFacts::foreign_port_listener(Some("frontend".to_string()), None),
        )];
        assert_eq!(
            resolve_stale_adoption(33604, &candidati),
            StaleAdoption::AdoptOrphan {
                port: 33610,
                pid: 4242
            }
        );
    }

    #[test]
    fn appartenenza_ignota_non_basta_ad_adottare() {
        // Processo non attribuibile (avvio manuale, nessuna riga): NON si adotta.
        // Costa una porta libera in piu', evita una porta condivisa fra due servizi.
        let candidati = vec![orphan(
            33620,
            777,
            "node",
            "frontend",
            OwnershipFacts::foreign_port_listener(None, None),
        )];
        assert_eq!(
            resolve_stale_adoption(33604, &candidati),
            StaleAdoption::ReuseStale { port: 33604 }
        );
    }

    #[test]
    fn tra_piu_candidati_si_prende_quello_del_servizio() {
        // Il PRIMO del bucket e' di un altro servizio: l'adozione cieca prendeva
        // quello. Si deve scegliere il proprio, non il primo.
        let candidati = vec![
            orphan(
                33605,
                111,
                "node",
                "frontend",
                OwnershipFacts::foreign_port_listener(Some("backend".to_string()), None),
            ),
            orphan(
                33630,
                222,
                "node",
                "frontend",
                OwnershipFacts::foreign_port_listener(Some("frontend".to_string()), None),
            ),
        ];
        assert_eq!(
            resolve_stale_adoption(33604, &candidati),
            StaleAdoption::AdoptOrphan {
                port: 33630,
                pid: 222
            }
        );
    }

    /// Ramo «nessuna riga per questa label» (1-bis): un processo NON registrato
    /// non diventa l'allocazione di un servizio. Il criterio precedente sceglieva
    /// per CLASSE (`labels_match`) su una label che per questi processi veniva
    /// dedotta dal nome del programma — una via che non e' mai stata raggiungibile
    /// coi dati reali (vedi `orphan_placeholder_label`, misurato), ma che decideva
    /// su un'identita' inventata e ora non e' piu' esprimibile.
    #[test]
    fn un_processo_non_registrato_non_e_il_frontend() {
        let candidati = vec![orphan(
            33612,
            888,
            "node /proj/node_modules/.bin/vite",
            "frontend",
            // Non registrato: nessuna riga per il pid, nessuna per la porta.
            OwnershipFacts::foreign_port_listener(None, None),
        )];
        // La misura arriva fin qui: nessuna porta viene EREDITATA. Quale porta il
        // servizio ottenga poi e' della catena successiva
        // (`deterministic_project_port_for_key`, che sceglie nel bucket ed e'
        // testata a parte): asserirla anche qui misurerebbe quella, non questa.
        assert!(
            owned_listener(&candidati).is_none(),
            "un processo non attribuibile non e' il servizio richiesto, per quanto il suo comando somigli a un dev-server"
        );
    }

    /// L'altra meta' dello stesso ramo: due label DIVERSE e persistite non
    /// condividono una porta solo perche' sono entrambe "di frontend". La classe
    /// (che include web, ui, client, vue, next, react) non e' un'identita'.
    #[test]
    fn stessa_classe_non_basta_a_ereditare_la_porta() {
        let candidati = vec![orphan(
            33615,
            999,
            "node",
            "frontend",
            OwnershipFacts::foreign_port_listener(None, Some("web".to_string())),
        )];
        assert!(
            owned_listener(&candidati).is_none(),
            "'web' e 'frontend' sono due servizi distinti: nessuno eredita la porta dell'altro"
        );
    }

    /// Il ramo resta VIVO nel suo caso d'uso: il servizio richiesto e' gia' attivo
    /// sotto un contorno di label diverso, provato da un dato strutturato.
    #[test]
    fn il_servizio_attivo_sotto_altra_label_viene_riusato() {
        let candidati = vec![orphan(
            33618,
            1234,
            "node",
            "backend",
            OwnershipFacts::foreign_port_listener(Some("Backend Nodemon".to_string()), None),
        )];
        assert_eq!(
            owned_listener(&candidati).map(|c| c.port),
            Some(33618),
            "stesso servizio con contorno diverso: la sua porta si riusa, niente allocazione nuova"
        );
    }

    #[test]
    fn un_processo_non_server_non_e_candidato() {
        // Il filtro storico resta: un tool in ascolto nel bucket non e' un servizio
        // adottabile, nemmeno se la label combaciasse.
        let candidati = vec![orphan(
            33640,
            333,
            "psql",
            "frontend",
            OwnershipFacts::foreign_port_listener(Some("frontend".to_string()), None),
        )];
        assert_eq!(
            resolve_stale_adoption(33604, &candidati),
            StaleAdoption::ReuseStale { port: 33604 }
        );
    }
}

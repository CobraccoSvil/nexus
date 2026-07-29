//! Punto unico (regola L) per la domanda autoritativa:
//! "servizio del progetto -> attivo su quale porta -> riusa / riavvia / alloca".
//!
//! CAUSA RADICE del loop request_port (diagnosi confermata su Beauty-Book): lo
//! stato delle risorse runtime di un progetto era frammentato in 3 viste
//! disallineate, nessuna iniettata nel prompt e nessuna consultata da
//! `find_or_allocate`:
//!   1. unit systemd persistenti (REST-only per la UI);
//!   2. `agent_processes` (non vede gli unit systemd);
//!   3. `nexus_port_allocations` (righe duplicate, porte reali assenti).
//! L'agente, non vedendo le risorse, variava la label e otteneva sempre una
//! nuova allocazione 'dynamic'.
//!
//! Questo modulo riconcilia le fonti SEMPRE disponibili in WSL (dove
//! `systemctl --user` e' spesso cieco) in una sola struttura `ProjectResources`:
//!   - fonte primaria: `nexus_port_allocations` via `PortRegistryCache`
//!     (label -> porta governata);
//!   - verita' di runtime: porte realmente in LISTEN nel bucket del progetto via
//!     `port_recovery::listening_ports` (indipendente da systemd e dal come e'
//!     stato avviato il servizio: `npm run dev`/vite/nodemon inclusi);
//!   - `agent_processes` (gia' usato da `scan_bucket_orphans`) per distinguere
//!     orfani veri da processi gestiti.
//! systemd e' best-effort e mai bloccante: `service_unit` viene dalle righe DB,
//! non e' fonte di `listening`.
//!
//! Tutti i call site delegano qui: il prompt-builder (`agent_run.rs`) e
//! `find_or_allocate` (`allocate_port.rs`). Nessuno re-implementa "porta in
//! ascolto" o "riusa vs alloca".

use sqlx::PgPool;
use uuid::Uuid;

use super::port_recovery::{listening_ports, looks_like_server_process};
use super::services::project_bucket_range;
use crate::port_registry::PortRegistryCache;

/// Provenienza di una `ServiceResource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceSource {
    /// Riga in `nexus_port_allocations` (label -> porta governata).
    PortAllocation,
    /// Processo in LISTEN nel bucket senza riga DB corrispondente.
    ListeningOrphan,
}

/// Azione suggerita per il servizio, derivata in modo deterministico (no LLM).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestedAction {
    /// Servizio attivo (in ascolto): riusa la porta esistente.
    Reuse,
    /// Porta nota ma nessuno in ascolto: riavvia il servizio esistente.
    Restart,
    /// Nessuna risorsa per questo scopo: alloca una porta nuova. Variante di
    /// completezza del dominio: `reconcile` non la costruisce (elenca solo
    /// risorse gia' esistenti, non scopi nuovi), ma `render_prompt_block` la
    /// gestisce e i call site la mappano al verbo "alloca".
    #[allow(dead_code)]
    Allocate,
}

/// Una risorsa runtime del progetto (un servizio noto o un orfano in ascolto).
#[derive(Debug, Clone)]
pub struct ServiceResource {
    /// Label DB oppure derivata (per gli orfani).
    pub label: String,
    /// Porta nota dal DB/registry (`None` solo per orfani senza porta, mai oggi).
    pub port: Option<u16>,
    /// True se qualcuno e' davvero in LISTEN su `port` (TCP runtime reale).
    pub listening: bool,
    /// PID che ascolta (da `listening_ports`), se noto.
    pub pid: Option<u32>,
    /// Nome processo (per riconoscere npm/vite/node).
    pub program: Option<String>,
    /// Unit systemd se registrata dal wizard (solo informativo).
    pub service_unit: Option<String>,
    /// Provenienza della risorsa.
    pub source: ResourceSource,
    /// `allocation_mode` dal DB (dynamic/existing/adopted/auto/manual), se nota.
    pub allocation_mode: Option<String>,
    /// Azione consigliata (Reuse/Restart/Allocate).
    pub suggested_action: SuggestedAction,
}

/// Insieme riconciliato delle risorse runtime di un progetto.
#[derive(Debug, Clone)]
pub struct ProjectResources {
    pub services: Vec<ServiceResource>,
    pub bucket_start: u16,
    pub bucket_end: u16,
}

/// Due classi di scopo disgiunte per il matching label. Una richiesta "backend"
/// NON deve mai riusare la porta di un "frontend" attivo (rischio: rompere
/// l'app). Vedi `service_class`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceClass {
    Frontend,
    Backend,
}

/// Normalizza una label/scopo nella sua classe di servizio, se riconoscibile.
/// Le due classi sono disgiunte: un token che indica "frontend" non puo' anche
/// indicare "backend". Token ambigui -> `None` (nessun match per classe).
fn service_class(label: &str) -> Option<ServiceClass> {
    let l = label.to_lowercase();
    const FRONTEND: &[&str] = &[
        "frontend",
        "front-end",
        "vite",
        "react",
        "svelte",
        "nuxt",
        "astro",
        "ui",
        "web",
        "client",
        "next",
        "angular",
        "vue",
    ];
    const BACKEND: &[&str] = &[
        "backend", "back-end", "api", "server", "express", "fastify", "uvicorn", "gunicorn",
        "rails", "django", "flask",
    ];
    let is_front = FRONTEND.iter().any(|k| l.contains(k));
    let is_back = BACKEND.iter().any(|k| l.contains(k));
    // Ambiguo (entrambi presenti) -> nessuna classe certa: si evita un match
    // azzardato. Es. "frontend-api-gateway" non viene assegnato a una classe.
    match (is_front, is_back) {
        (true, false) => Some(ServiceClass::Frontend),
        (false, true) => Some(ServiceClass::Backend),
        _ => None,
    }
}

/// Normalizza una label per il confronto: lowercase, separatori a spazi.
fn normalize_label(label: &str) -> String {
    label
        .to_lowercase()
        .replace(['_', '-', '.', '/'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// True se `req` (label richiesta) e `res` (label risorsa) hanno lo stesso SCOPO.
/// Due criteri, fermandosi al primo:
///   a. uguaglianza case-insensitive normalizzata;
///   b. stessa CLASSE di servizio (frontend/backend), classi disgiunte: cosi'
///      "backend" ~ "backend-dev" e "backend" non matcha mai "frontend".
///
/// La (b) e' larga per costruzione: la classe frontend include web, ui, client,
/// vue, next, react, quindi "frontend" matcha QUALUNQUE risorsa di quella classe,
/// non solo le varianti del proprio nome. E' voluto qui — la domanda dei chiamanti
/// e' «esiste gia' un servizio dello stesso scopo?», che serve a non far nascere
/// due dev-server sulla stessa codebase — e NON va confusa con «questo processo e'
/// il servizio per cui sto allocando?», che ha un punto unico apposta
/// (`service_ownership`, regola L) e criteri piu' stretti.
///
/// Il commento storico prometteva anche un "contenimento bidirezionale su token
/// normalizzati" che il codice non ha mai fatto: chi leggeva credeva la funzione
/// piu' stretta di quanto sia, ed e' cosi' che ha finito per governare decisioni
/// che esigevano identita' e non scopo.
fn labels_match(req: &str, res: &str) -> bool {
    let rn = normalize_label(req);
    let sn = normalize_label(res);
    if rn == sn {
        return true;
    }
    // Stessa classe disgiunta.
    matches!(
        (service_class(req), service_class(res)),
        (Some(a), Some(b)) if a == b
    )
}

/// Costruisce l'insieme riconciliato delle risorse runtime del progetto.
///
/// Punto unico: incrocia `nexus_port_allocations` (label->porta) con le porte
/// realmente in LISTEN (`listening_ports`) e identifica gli orfani del bucket.
/// systemd non viene interrogato (cieco in WSL e non necessario: `listening` e'
/// la verita' di runtime).
pub async fn resolve_project_resources(
    registry: &PortRegistryCache,
    project_id: Uuid,
) -> ProjectResources {
    let (bucket_start, bucket_end) = project_bucket_range(&project_id);

    // Verita' di runtime: porte realmente in LISTEN (ss / fallback /proc).
    let listening = listening_ports().await;
    // Allocazioni DB del progetto (label -> porta governata).
    let allocations = registry.ports_for_project(&project_id).await;
    let own_pid = std::process::id();

    let services = reconcile(&allocations, &listening, bucket_start, bucket_end, own_pid);

    ProjectResources {
        services,
        bucket_start,
        bucket_end,
    }
}

/// Logica pura di riconciliazione (testabile senza `ss`/DB): incrocia le
/// allocazioni DB con le porte realmente in LISTEN e identifica gli orfani del
/// bucket. Punto unico della derivazione `listening`/`suggested_action`.
fn reconcile(
    allocations: &[crate::port_registry::PortAllocation],
    listening: &[(u16, u32, String)],
    bucket_start: u16,
    bucket_end: u16,
    own_pid: u32,
) -> Vec<ServiceResource> {
    let mut services: Vec<ServiceResource> = Vec::new();
    let mut covered_ports: std::collections::HashSet<u16> = std::collections::HashSet::new();

    // Base: una ServiceResource per ogni allocazione DB del progetto.
    for alloc in allocations {
        covered_ports.insert(alloc.port);
        let listen_hit = listening.iter().find(|(p, _, _)| *p == alloc.port);
        let is_listening = listen_hit.is_some();
        let suggested_action = if is_listening {
            SuggestedAction::Reuse
        } else {
            // Porta nota nel DB ma spenta: c'e' un servizio da riavviare.
            SuggestedAction::Restart
        };
        services.push(ServiceResource {
            label: alloc.label.clone(),
            port: Some(alloc.port),
            listening: is_listening,
            pid: listen_hit.map(|(_, pid, _)| *pid),
            program: listen_hit.map(|(_, _, prog)| prog.clone()),
            service_unit: alloc.service_unit.clone(),
            source: ResourceSource::PortAllocation,
            allocation_mode: Some(alloc.allocation_mode.clone()),
            suggested_action,
        });
    }

    // Orfani: porte in LISTEN nel bucket del progetto senza riga DB. Derivano una
    // label dal programma. Solo processi che assomigliano a server (esclude PID
    // di tool, shell, ecc.). Il PID di mcp-core stesso non e' nel bucket dei
    // progetti, quindi own_pid e' un filtro difensivo.
    let mut orphan_servers: Vec<&(u16, u32, String)> = listening
        .iter()
        .filter(|(p, pid, prog)| {
            *p >= bucket_start
                && *p <= bucket_end
                && *pid != 0
                && *pid != own_pid
                && !covered_ports.contains(p)
                && looks_like_server_process(prog)
        })
        .collect();
    // Porta crescente: ordine stabile della lista, non un criterio di identita'.
    orphan_servers.sort_by_key(|(p, _, _)| *p);

    for (port, pid, program) in orphan_servers.iter() {
        let label = orphan_placeholder_label(*port);
        services.push(ServiceResource {
            label,
            port: Some(*port),
            listening: true,
            pid: Some(*pid),
            program: Some(program.clone()),
            service_unit: None,
            source: ResourceSource::ListeningOrphan,
            allocation_mode: None,
            suggested_action: SuggestedAction::Reuse,
        });
    }

    services
}

/// Nome con cui compare un processo in LISTEN che NON ha una riga di allocazione:
/// `service-<port>`, cioe' un identificatore posizionale, non uno scopo.
///
/// Prima l'euristica pretendeva di indovinare: un programma il cui nome conteneva
/// vite/next/nuxt/astro/react/svelte, se primo per numero di porta, diventava
/// "frontend" — un'IDENTITA' DEDOTTA DAL NOME DEL PROGRAMMA (regola M), che
/// nessun consumatore poteva distinguere da una label vera letta dal DB, perche'
/// la lista delle risorse le mescola. Su quella label decidono tre percorsi:
/// `find_or_allocate` ne eredita la porta, `refuse_if_same_scope_active` nega
/// all'agente il proprio avvio indirizzandolo altrove, e
/// `free_listening_scope_port` ne UCCIDE l'albero di processi.
///
/// MISURATO il 29/07/2026 prima di rimuoverla: quel ramo era INERTE, non raro.
/// Il terzo campo di `listening_ports` e' il nome dell'ESEGUIBILE, mai la riga di
/// comando, su entrambe le piattaforme (Windows: `exe_name_senza_estensione(
/// szExeFile)`; Linux: `users:(("node",pid=...))` -> `split('"').nth(1)`), e ogni
/// dev-server JS gira come processo `node`. La funzione ha quindi sempre
/// restituito `service-<port>`: nessuno di quei tre percorsi ha mai potuto
/// scattare per questa via. Il fix non disinnesca una trappola che scattava, ne
/// toglie una ARMATA — pronta al primo che facesse passare la command line in
/// quel campo credendo di migliorare la diagnostica.
///
/// `service-<port>` non appartiene a nessuna classe di servizio, quindi
/// `labels_match` lo accetta solo per uguaglianza esatta: un processo non
/// identificato non viene scambiato per il servizio di nessuno. Il nome del
/// programma resta VISIBILE nel blocco prompt (campo `program`), dove informa
/// senza decidere.
fn orphan_placeholder_label(port: u16) -> String {
    format!("service-{port}")
}

/// Helper riusabile da `find_or_allocate`: costruisce le risorse e applica il
/// matching label/scopo. Ritorna la prima risorsa che corrisponde allo scopo
/// della `label` richiesta (uguaglianza o stessa classe di servizio), oppure
/// `None` se nessuna corrisponde (lo scopo e' nuovo).
///
/// La preferenza va a una risorsa in ascolto (Reuse) rispetto a una spenta
/// (Restart): tra due match dello stesso scopo si ritorna quella attiva.
pub async fn resolve_for_label(
    registry: &PortRegistryCache,
    project_id: Uuid,
    label: &str,
) -> Option<ServiceResource> {
    let resources = resolve_project_resources(registry, project_id).await;
    // Prima passata: match in ascolto (Reuse). Seconda: qualunque match.
    resources
        .services
        .iter()
        .find(|s| s.listening && labels_match(label, &s.label))
        .or_else(|| {
            resources
                .services
                .iter()
                .find(|s| labels_match(label, &s.label))
        })
        .cloned()
}

/// Costruisce il blocco di prompt "RISORSE PROGETTO" (conciso, 1 riga per
/// servizio). Ritorna stringa vuota se non c'e' alcuna risorsa: niente blocco
/// rumoroso. Il blocco e' deterministico, non chiama LLM, e va iniettato a valle
/// di `project_header` (non tocca il template DB ne' l'offload del system).
pub async fn render_prompt_block(
    _db: &PgPool,
    registry: &PortRegistryCache,
    project_id: Uuid,
) -> String {
    let resources = resolve_project_resources(registry, project_id).await;
    if resources.services.is_empty() {
        return String::new();
    }

    let mut lines = String::new();
    lines.push_str("=== RISORSE PROGETTO (stato runtime reale, gia' qui: non riscoprirle) ===\n");
    lines.push_str(&format!(
        "Bucket porte: {}-{}\n",
        resources.bucket_start, resources.bucket_end
    ));
    for s in &resources.services {
        let port_txt = s
            .port
            .map(|p| p.to_string())
            .unwrap_or_else(|| "?".to_string());
        // Suffisso informativo: orfano non registrato vs allocazione governata
        // (con il suo mode), + unit systemd se presente. Aiuta l'agente a capire
        // se la risorsa e' tracciata da Nexus o solo "vista" in ascolto.
        let suffix = match s.source {
            ResourceSource::ListeningOrphan => " [non registrato, in ascolto]".to_string(),
            ResourceSource::PortAllocation => {
                let mode = s.allocation_mode.as_deref().unwrap_or("?");
                let unit = s
                    .service_unit
                    .as_deref()
                    .map(|u| format!(", unit {u}"))
                    .unwrap_or_default();
                format!(" [mode {mode}{unit}]")
            }
        };
        match s.suggested_action {
            SuggestedAction::Reuse => {
                let prog = s.program.as_deref().unwrap_or("?");
                let pid_txt = s.pid.map(|p| format!(", pid {p}")).unwrap_or_default();
                lines.push_str(&format!(
                    "- {}: porta {} ATTIVA ({}{}) -> RIUSA, non riallocare{}\n",
                    s.label, port_txt, prog, pid_txt, suffix
                ));
            }
            SuggestedAction::Restart => {
                lines.push_str(&format!(
                    "- {}: porta {} allocata ma NON in ascolto -> RIAVVIA il servizio esistente{}\n",
                    s.label, port_txt, suffix
                ));
            }
            SuggestedAction::Allocate => {
                lines.push_str(&format!(
                    "- {}: porta {} -> alloca nuova{}\n",
                    s.label, port_txt, suffix
                ));
            }
        }
    }
    lines.push_str(
        "Regola: se un servizio e' gia' ATTIVO usa la sua porta; se e' allocato-ma-spento riavvialo; chiama request_port SOLO per un servizio NUOVO.\n",
    );
    lines.push_str("=== FINE RISORSE PROGETTO ===\n\n");
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_class_disgiunta() {
        assert_eq!(service_class("backend"), Some(ServiceClass::Backend));
        assert_eq!(
            service_class("Backend - Nodemon"),
            Some(ServiceClass::Backend)
        );
        assert_eq!(service_class("frontend"), Some(ServiceClass::Frontend));
        assert_eq!(service_class("Frontend Vite"), Some(ServiceClass::Frontend));
        // Ambiguo: contiene token di entrambe le classi -> nessuna classe.
        assert_eq!(service_class("frontend-api"), None);
        // Sconosciuto.
        assert_eq!(service_class("worker"), None);
    }

    #[test]
    fn labels_match_uguaglianza_normalizzata() {
        assert!(labels_match("backend", "backend"));
        assert!(labels_match("Backend", "backend"));
        assert!(labels_match("backend-dev", "backend dev"));
    }

    #[test]
    fn labels_match_stessa_classe() {
        // Variazioni del contorno della label, stessa classe backend -> match.
        assert!(labels_match("backend", "Backend - Nodemon (TypeScript)"));
        assert!(labels_match("api", "backend-server"));
        // Stessa classe frontend.
        assert!(labels_match("frontend", "vite-dev"));
    }

    #[test]
    fn labels_match_classi_diverse_no_match() {
        // RISCHIO 1: una richiesta "backend" non deve riusare un "frontend".
        assert!(!labels_match("backend", "frontend"));
        assert!(!labels_match("backend", "vite-dev"));
        assert!(!labels_match("frontend", "api-server"));
    }

    #[test]
    fn labels_match_classe_sconosciuta_solo_uguaglianza() {
        // Senza classe nota, solo l'uguaglianza normalizzata fa match.
        assert!(labels_match("worker", "worker"));
        assert!(!labels_match("worker", "queue-consumer"));
    }

    /// Un processo senza riga di allocazione non riceve uno SCOPO indovinato dal
    /// nome del suo programma. L'euristica rimossa lo prometteva ("node" con
    /// "vite" nel nome, primo per porta, -> "frontend") ma era INERTE: quel campo
    /// porta il nome dell'eseguibile, e ogni dev-server JS e' `node` (misurato,
    /// vedi `orphan_placeholder_label`). Il test fissa l'invariante che rendeva
    /// innocua quella via e che va mantenuta: la label di un processo non
    /// identificato non appartiene a nessuna classe, quindi non matcha nulla.
    #[test]
    fn un_orfano_non_riceve_uno_scopo_indovinato() {
        assert_eq!(orphan_placeholder_label(21000), "service-21000");
        // Il placeholder non appartiene a nessuna classe: nessun match spurio.
        assert_eq!(service_class(&orphan_placeholder_label(21000)), None);
        assert!(
            !labels_match("frontend", &orphan_placeholder_label(21000)),
            "un processo non identificato non e' il frontend di nessuno"
        );
        assert!(!labels_match("backend", &orphan_placeholder_label(21001)));
    }

    // ── Test della riconciliazione delle fonti (funzione pura `reconcile`) ──

    fn alloc(
        project_id: Uuid,
        port: u16,
        label: &str,
        mode: &str,
    ) -> crate::port_registry::PortAllocation {
        crate::port_registry::PortAllocation {
            id: Uuid::new_v4(),
            project_id,
            port,
            label: label.to_string(),
            allocation_mode: mode.to_string(),
            run_config_id: None,
            service_unit: None,
        }
    }

    #[test]
    fn reconcile_alloc_in_ascolto_e_reuse() {
        let proj = Uuid::new_v4();
        let allocations = vec![alloc(proj, 21001, "backend", "dynamic")];
        // La porta 21001 e' realmente in LISTEN (pid 1234, programma node).
        let listening = vec![(21001u16, 1234u32, "node".to_string())];
        let res = reconcile(&allocations, &listening, 21000, 21099, 9999);
        assert_eq!(res.len(), 1);
        let s = &res[0];
        assert_eq!(s.label, "backend");
        assert!(s.listening, "porta in LISTEN -> listening=true");
        assert_eq!(s.pid, Some(1234));
        assert_eq!(s.suggested_action, SuggestedAction::Reuse);
        assert_eq!(s.source, ResourceSource::PortAllocation);
    }

    #[test]
    fn reconcile_alloc_spenta_e_restart() {
        let proj = Uuid::new_v4();
        let allocations = vec![alloc(proj, 21001, "backend", "dynamic")];
        // Nessuno in ascolto sulla porta allocata.
        let listening: Vec<(u16, u32, String)> = vec![];
        let res = reconcile(&allocations, &listening, 21000, 21099, 9999);
        assert_eq!(res.len(), 1);
        assert!(!res[0].listening, "porta non in LISTEN -> listening=false");
        assert_eq!(
            res[0].suggested_action,
            SuggestedAction::Restart,
            "porta nota ma spenta -> RIAVVIA"
        );
    }

    #[test]
    fn reconcile_orfano_listen_senza_riga_db() {
        // Nessuna allocazione DB, ma un dev-server vite in LISTEN nel bucket.
        let allocations: Vec<crate::port_registry::PortAllocation> = vec![];
        let listening = vec![(
            21000u16,
            555u32,
            "node /p/node_modules/.bin/vite".to_string(),
        )];
        let res = reconcile(&allocations, &listening, 21000, 21099, 9999);
        assert_eq!(res.len(), 1);
        let s = &res[0];
        assert_eq!(s.source, ResourceSource::ListeningOrphan);
        assert_eq!(
            s.label, "service-21000",
            "un processo senza riga DB compare per posizione, non per uno scopo dedotto dal comando"
        );
        assert_eq!(
            s.program.as_deref(),
            Some("node /p/node_modules/.bin/vite"),
            "il nome del programma resta VISIBILE: informa il lettore senza decidere per lui"
        );
        assert!(s.listening);
        assert_eq!(s.suggested_action, SuggestedAction::Reuse);
    }

    #[test]
    fn reconcile_ignora_orfani_fuori_bucket_e_non_server() {
        let allocations: Vec<crate::port_registry::PortAllocation> = vec![];
        let listening = vec![
            // Fuori bucket -> ignorato.
            (40000u16, 1u32, "node vite".to_string()),
            // Nel bucket ma non e' un server (es. un tool) -> ignorato.
            (21010u16, 2u32, "psql".to_string()),
            // PID di mcp-core stesso (own_pid) -> ignorato.
            (21020u16, 9999u32, "node server".to_string()),
        ];
        let res = reconcile(&allocations, &listening, 21000, 21099, 9999);
        assert!(res.is_empty(), "nessun orfano valido nel bucket: {:?}", res);
    }

    #[test]
    fn reconcile_riconcilia_db_e_listen_insieme() {
        // Scenario Beauty-Book: backend allocato+attivo, frontend orfano in LISTEN.
        let proj = Uuid::new_v4();
        let allocations = vec![alloc(proj, 21001, "backend", "dynamic")];
        let listening = vec![
            (21001u16, 100u32, "node".to_string()), // backend allocato e attivo
            (21000u16, 200u32, "node vite".to_string()), // frontend orfano (porta piu' bassa)
        ];
        let res = reconcile(&allocations, &listening, 21000, 21099, 9999);
        assert_eq!(res.len(), 2);
        let backend = res.iter().find(|s| s.label == "backend").expect("backend");
        assert!(backend.listening);
        assert_eq!(backend.source, ResourceSource::PortAllocation);
        let orfano = res
            .iter()
            .find(|s| s.source == ResourceSource::ListeningOrphan)
            .expect("orfano in ascolto");
        assert_eq!(orfano.label, "service-21000");
        assert!(orfano.listening);
        // La riga REGISTRATA conserva la sua label vera: stringere la fonte
        // toglie le identita' inventate, non quelle lette dal DB.
        assert_eq!(backend.label, "backend");
    }
}

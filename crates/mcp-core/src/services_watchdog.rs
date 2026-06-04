//! Services Watchdog — monitoraggio e auto-restart dei microservizi Nexus.
//!
//! In produzione i microservizi (brain, gateway, admin/chat/doc/billing/plugin
//! -service, browser-bridge, web-ide) sono gestiti da systemd con
//! `Restart=on-failure`: se cadono, si rialzano da soli. In dev/WSL questo non
//! c'e', quindi un servizio caduto resta giu' finche' l'utente non lo riavvia a
//! mano. Questo watchdog colma il gap: TCP probe periodico di ogni servizio e,
//! se down per N cicli consecutivi, riavvio automatico.
//!
//! Scelta del meccanismo di riavvio (deploy-script vs comando diretto):
//!   Si invoca `deploy-local.sh --service <name> --debug` in modo detached
//!   (setsid nohup), NON un comando di spawn ad-hoc per servizio. Razionale:
//!   - lo script conosce gia' l'env corretto di OGNI kind di servizio
//!     (rust/builtin/brain/gateway/web-ide), le porte risolte dal DB, il
//!     DATABASE_URL/JWT di bootstrap, la build in debug;
//!   - duplicare quella logica qui significherebbe replicare 5 percorsi di
//!     avvio diversi e tenerli sincronizzati a mano (fragile, viola DRY).
//!   Il pattern detached e' lo stesso di `task_watchdog::try_restart_gateway`
//!   e `wizard::spawn_detached_service` (setsid nohup ... > log 2>&1 < /dev/null &).
//!
//! Anti-restart-loop:
//!   - dopo un riavvio, COOLDOWN (`agent.watchdog.restart_cooldown_seconds`)
//!     prima di poter ritentare lo stesso servizio;
//!   - contatore di riavvii consecutivi falliti (servizio ancora down dopo il
//!     cooldown); a `agent.watchdog.max_consecutive_restarts` il servizio e'
//!     marcato irrecuperabile (log ERROR, niente altri tentativi);
//!   - il contatore si azzera appena il servizio torna up.
//!
//! mcp-core NON e' monitorato: e' il processo che ospita questo watchdog.
//!
//! Tutta la config e' DB-driven (regola G), niente porte/comandi hardcoded
//! oltre i default in migrazione `0272_services_watchdog.sql`.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;
use sqlx::PgPool;
use tokio::time::sleep;

use crate::project_workspace::port_recovery::tcp_probe;

/// Timeout del TCP probe per ogni servizio.
const PROBE_TIMEOUT_MS: u64 = 1_500;

/// Attesa iniziale prima del primo ciclo (lascia stabilizzare l'avvio).
const STARTUP_DELAY_S: u64 = 20;

/// Voce della lista servizi monitorati (da `agent.watchdog.services`).
#[derive(Debug, Clone, Deserialize)]
pub struct WatchedService {
    /// Nome per `deploy-local.sh --service <name>` (allineato a SERVICES_CATALOG).
    pub name: String,
    /// Chiave di `settings` da cui risolvere la porta (regola G).
    pub port_setting_key: String,
}

/// Stato runtime per servizio, mantenuto in-memory tra i cicli.
#[derive(Debug, Default, Clone)]
struct ServiceState {
    /// Cicli consecutivi in cui il probe ha rilevato il servizio down.
    consecutive_down: u32,
    /// Riavvii consecutivi che NON hanno riportato up il servizio.
    failed_restarts: u32,
    /// Unix timestamp dell'ultimo tentativo di riavvio (per il cooldown).
    last_restart_ts: i64,
    /// True quando il cap di riavvii e' stato raggiunto: si smette di tentare
    /// finche' il servizio non torna up da solo.
    given_up: bool,
}

/// Parametri di configurazione del watchdog, risolti dal DB a ogni ciclo.
#[derive(Debug, Clone)]
struct WatchdogConfig {
    enabled: bool,
    interval_s: u64,
    fail_threshold: u32,
    restart_cooldown_s: i64,
    max_consecutive_restarts: u32,
    services: Vec<WatchedService>,
}

/// Decisione del watchdog per un singolo servizio, dato lo stato corrente.
/// Funzione PURA: separata dall'IO per essere testabile in isolamento.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// Servizio up: nessuna azione (e azzera i contatori a monte).
    Healthy,
    /// Down ma non ancora oltre la soglia, oppure cooldown attivo: attendi.
    Wait,
    /// Down e condizioni soddisfatte: riavvia.
    Restart,
    /// Down ma cap raggiunto: irrecuperabile, non tentare piu'.
    GiveUp,
}

/// Logica pura di decisione. `now_ts` = unix timestamp corrente.
/// `is_up` = esito del probe in questo ciclo. `st` = stato accumulato.
fn decide(
    is_up: bool,
    st: &ServiceState,
    fail_threshold: u32,
    restart_cooldown_s: i64,
    max_consecutive_restarts: u32,
    now_ts: i64,
) -> Decision {
    if is_up {
        return Decision::Healthy;
    }
    // Servizio down.
    if st.given_up {
        return Decision::GiveUp;
    }
    if st.failed_restarts >= max_consecutive_restarts {
        return Decision::GiveUp;
    }
    // Soglia di cicli down consecutivi non ancora raggiunta.
    // consecutive_down viene incrementato dal chiamante PRIMA di chiamare decide,
    // quindi qui confrontiamo >= threshold.
    if st.consecutive_down < fail_threshold {
        return Decision::Wait;
    }
    // Cooldown: se un riavvio e' avvenuto di recente, attendi.
    if st.last_restart_ts > 0 && (now_ts - st.last_restart_ts) < restart_cooldown_s {
        return Decision::Wait;
    }
    Decision::Restart
}

/// Carica la config dal DB. Errori di parsing/lettura -> default conservativi
/// che NON disabilitano il watchdog se i singoli setting mancano, ma la lista
/// servizi vuota di fatto lo rende inerte (nessun hardcode di servizi nel codice).
async fn load_config(db: &PgPool) -> WatchdogConfig {
    let get = |k: &'static str| crate::settings::get_setting(db, k);

    let enabled = get("agent.watchdog.enabled")
        .await
        .ok()
        .flatten()
        .map(|v| {
            !matches!(
                v.trim().to_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true);

    let interval_s = get("agent.watchdog.interval_seconds")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(30)
        .max(10);

    let fail_threshold = get("agent.watchdog.fail_threshold")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(2)
        .max(1);

    let restart_cooldown_s = get("agent.watchdog.restart_cooldown_seconds")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(120)
        .max(30);

    let max_consecutive_restarts = get("agent.watchdog.max_consecutive_restarts")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(5)
        .max(1);

    let services = get("agent.watchdog.services")
        .await
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str::<Vec<WatchedService>>(&v).ok())
        .unwrap_or_default();

    WatchdogConfig {
        enabled,
        interval_s,
        fail_threshold,
        restart_cooldown_s,
        max_consecutive_restarts,
        services,
    }
}

/// Risolve la porta di un servizio dalla sua chiave settings (regola G).
async fn resolve_port(db: &PgPool, port_setting_key: &str) -> Option<u16> {
    crate::settings::get_setting(db, port_setting_key)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u16>().ok())
}

/// Riavvia un servizio invocando `deploy-local.sh --service <name> --debug` in
/// modo detached. Best-effort: l'esito reale si verifica al ciclo successivo via
/// probe. Ritorna true se lo spawn dello script e' partito senza errori.
async fn restart_service(name: &str) -> bool {
    let root = std::env::var("NEXUS_REPO_ROOT")
        .unwrap_or_else(|_| "/home/administrator/ideai".to_string());
    let script = format!("{root}/deploy/deploy-local.sh");
    if !std::path::Path::new(&script).exists() {
        tracing::warn!(
            "services_watchdog: deploy-local.sh non trovato in {root}, impossibile riavviare {name}"
        );
        return false;
    }
    let log_path = format!("/tmp/nexus-watchdog-{name}.log");
    // setsid nohup ... < /dev/null & : il processo sopravvive alla morte di
    // mcp-core e non eredita stdin/stdout del watchdog.
    let shell = format!(
        "cd '{root}' && setsid nohup bash '{script}' --service '{name}' --debug \
         > '{log_path}' 2>&1 < /dev/null &"
    );
    match tokio::process::Command::new("bash")
        .args(["-lc", &shell])
        .output()
        .await
    {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            tracing::warn!(
                "services_watchdog: spawn riavvio {name} fallito ({}): {}",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            );
            false
        }
        Err(e) => {
            tracing::warn!("services_watchdog: exec deploy-local.sh per {name} fallito: {e}");
            false
        }
    }
}

/// Avvia il watchdog servizi in background. Restituisce subito.
/// Gating runtime: `agent.watchdog.enabled` viene riletto a ogni ciclo, cosi'
/// l'admin puo' disattivarlo/riattivarlo senza riavviare mcp-core.
pub fn spawn_services_watchdog(db: PgPool) {
    tokio::spawn(async move {
        sleep(Duration::from_secs(STARTUP_DELAY_S)).await;
        // Stato per servizio, persistente tra i cicli.
        let mut states: HashMap<String, ServiceState> = HashMap::new();
        loop {
            let cfg = load_config(&db).await;
            if !cfg.enabled {
                // Disabilitato: non monitorare, ma continua a rileggere il flag.
                sleep(Duration::from_secs(cfg.interval_s)).await;
                continue;
            }
            run_cycle(&db, &cfg, &mut states).await;
            sleep(Duration::from_secs(cfg.interval_s)).await;
        }
    });
    tracing::info!("services_watchdog: avviato (config DB-driven, gating runtime)");
}

/// Un ciclo: probe di ogni servizio, aggiornamento stato, eventuale riavvio.
async fn run_cycle(db: &PgPool, cfg: &WatchdogConfig, states: &mut HashMap<String, ServiceState>) {
    let now_ts = chrono::Utc::now().timestamp();

    for svc in &cfg.services {
        let port = match resolve_port(db, &svc.port_setting_key).await {
            Some(p) => p,
            None => {
                tracing::warn!(
                    "services_watchdog: porta non risolvibile per {} (setting {}), skip",
                    svc.name,
                    svc.port_setting_key
                );
                continue;
            }
        };

        let is_up = tcp_probe(port, PROBE_TIMEOUT_MS).await;
        let st = states.entry(svc.name.clone()).or_default();

        if is_up {
            // Recupero: log su transizione e reset contatori.
            if st.consecutive_down > 0 || st.failed_restarts > 0 || st.given_up {
                tracing::info!(
                    "services_watchdog: {} (porta {}) RIPRISTINATO",
                    svc.name,
                    port
                );
            }
            st.consecutive_down = 0;
            st.failed_restarts = 0;
            st.given_up = false;
            continue;
        }

        // Servizio down: incrementa il contatore PRIMA della decisione.
        st.consecutive_down = st.consecutive_down.saturating_add(1);
        if st.consecutive_down == cfg.fail_threshold {
            // Logga il DOWN una sola volta al raggiungimento soglia.
            tracing::info!(
                "services_watchdog: {} (porta {}) DOWN da {} cicli",
                svc.name,
                port,
                st.consecutive_down
            );
        }

        let decision = decide(
            false,
            st,
            cfg.fail_threshold,
            cfg.restart_cooldown_s,
            cfg.max_consecutive_restarts,
            now_ts,
        );

        match decision {
            Decision::Healthy => {} // impossibile con is_up=false
            Decision::Wait => {}
            Decision::GiveUp => {
                if !st.given_up {
                    st.given_up = true;
                    tracing::error!(
                        "services_watchdog: {} (porta {}) IRRECUPERABILE dopo {} riavvii falliti — \
                         stop tentativi finche' non torna up. Controlla {}",
                        svc.name, port, st.failed_restarts,
                        format!("/tmp/nexus-watchdog-{}.log", svc.name)
                    );
                }
            }
            Decision::Restart => {
                tracing::info!(
                    "services_watchdog: riavvio {} (porta {}) — tentativo #{}",
                    svc.name,
                    port,
                    st.failed_restarts + 1
                );
                let spawned = restart_service(&svc.name).await;
                st.last_restart_ts = now_ts;
                // Conteggio del riavvio come "fallito" finche' il prossimo ciclo
                // (dopo cooldown) non conferma up; verra' azzerato al recupero.
                st.failed_restarts = st.failed_restarts.saturating_add(1);
                if !spawned {
                    tracing::warn!("services_watchdog: spawn riavvio {} non partito", svc.name);
                }
            }
        }
    }
}

// ── Test della logica pura di decisione ──────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn st(
        consecutive_down: u32,
        failed_restarts: u32,
        last_restart_ts: i64,
        given_up: bool,
    ) -> ServiceState {
        ServiceState {
            consecutive_down,
            failed_restarts,
            last_restart_ts,
            given_up,
        }
    }

    #[test]
    fn servizio_up_e_healthy() {
        let s = st(0, 0, 0, false);
        assert_eq!(decide(true, &s, 2, 120, 5, 1000), Decision::Healthy);
    }

    #[test]
    fn down_sotto_soglia_attende() {
        // consecutive_down=1, threshold=2 -> ancora sotto soglia.
        let s = st(1, 0, 0, false);
        assert_eq!(decide(false, &s, 2, 120, 5, 1000), Decision::Wait);
    }

    #[test]
    fn down_a_soglia_senza_cooldown_riavvia() {
        let s = st(2, 0, 0, false);
        assert_eq!(decide(false, &s, 2, 120, 5, 1000), Decision::Restart);
    }

    #[test]
    fn down_a_soglia_in_cooldown_attende() {
        // ultimo riavvio a 950, ora 1000, cooldown 120 -> 50s < 120 -> attendi.
        let s = st(2, 1, 950, false);
        assert_eq!(decide(false, &s, 2, 120, 5, 1000), Decision::Wait);
    }

    #[test]
    fn down_a_soglia_cooldown_scaduto_riavvia() {
        // ultimo riavvio a 800, ora 1000, cooldown 120 -> 200s >= 120 -> riavvia.
        let s = st(2, 1, 800, false);
        assert_eq!(decide(false, &s, 2, 120, 5, 1000), Decision::Restart);
    }

    #[test]
    fn cap_riavvii_raggiunto_give_up() {
        // failed_restarts=5, max=5 -> irrecuperabile.
        let s = st(10, 5, 800, false);
        assert_eq!(decide(false, &s, 2, 120, 5, 1000), Decision::GiveUp);
    }

    #[test]
    fn gia_dato_per_perso_resta_give_up() {
        let s = st(20, 5, 800, true);
        assert_eq!(decide(false, &s, 2, 120, 5, 1000), Decision::GiveUp);
    }

    #[test]
    fn up_dopo_give_up_torna_healthy() {
        // Anche se given_up=true, un probe up ritorna Healthy: il chiamante
        // azzerera' i contatori.
        let s = st(20, 5, 800, true);
        assert_eq!(decide(true, &s, 2, 120, 5, 1000), Decision::Healthy);
    }
}

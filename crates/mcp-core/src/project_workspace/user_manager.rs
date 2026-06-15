//! Garanzia del systemd --user manager (Livello 2, ADR 0028).
//!
//! In WSL il manager `user@<UID>.service` non riparte deterministicamente al
//! boot anche con linger: quando e' giu', `systemctl --user` da "Connection
//! refused" e il service_observer diventa cieco. La garanzia PRIMARIA e' la unit
//! systemd --system oneshot `nexus-user-manager.service` (Livello 1, installata
//! da `deploy/install-user-manager.sh`), che gira come root al boot. Questo
//! modulo e' la CINTURA per la race-window: all'avvio di mcp-core, se il bus
//! utente e' ancora giu', lo risuscita UNA volta via il Sudo Manager esistente
//! (mig 0289), senza nuovi privilegi ne un watchdog periodico.
//!
//! Punto unico (regola L): il probe riusa `wizard::systemd_user_available()`;
//! l'azione privilegiata passa dal `sudo_manager` (unico canale root). Niente
//! sudo diretto, niente UID hardcoded (il command e' nel DB, iniettato a
//! install-time dall'installer). Best-effort: non solleva mai, non panica mai.

use sqlx::PgPool;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::db_settings::{read_bool, read_u64};

/// Floor HARDCODED non bypassabile tra due tentativi di risurrezione, anche se
/// il setting DB e' inferiore (o zero): evita di martellare root come se il
/// manager utente fosse in crash-loop.
const RESURRECTION_COOLDOWN_FLOOR_SECS: u64 = 60;

/// Purpose registrato in `nexus_sudo_purposes` (mig 0369).
const RESURRECTION_PURPOSE: &str = "user-manager-start";

/// Istante dell'ultimo tentativo di risurrezione (rate-limit cross-chiamata).
static LAST_ATTEMPT: Mutex<Option<Instant>> = Mutex::new(None);

/// Cooldown effettivo: il massimo tra il valore configurato e il floor hardcoded.
/// Funzione pura -> testabile senza DB.
fn effective_cooldown(configured_secs: u64) -> Duration {
    Duration::from_secs(configured_secs.max(RESURRECTION_COOLDOWN_FLOOR_SECS))
}

/// Garantisce (best-effort) che il systemd --user manager sia attivo.
/// Chiamare UNA volta all'avvio di mcp-core. Non blocca l'avvio in caso di
/// errore: logga e prosegue (il fallback detached del wizard copre comunque).
pub async fn ensure_user_manager(db: &PgPool) {
    // 1. Probe: il bus utente risponde gia'? (punto unico, regola L)
    if crate::project_workspace::wizard::systemd_user_available().await {
        tracing::debug!("user_manager: bus --user gia' attivo");
        return;
    }

    // 2. Gate DB (regola G): la garanzia di boot resta la unit --system anche
    //    se questo flag e' false; qui governiamo solo l'on-startup runtime.
    if !read_bool(db, "agent.user_manager.autostart_enabled", true).await {
        tracing::info!(
            "user_manager: bus --user giu' ma autostart disabilitato (agent.user_manager.autostart_enabled=false)"
        );
        return;
    }

    // 3. Cooldown con floor non bypassabile.
    let cooldown = effective_cooldown(
        read_u64(db, "agent.user_manager.resurrection_cooldown_seconds", 120).await,
    );
    {
        let mut last = LAST_ATTEMPT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prev) = *last {
            if prev.elapsed() < cooldown {
                tracing::debug!(
                    "user_manager: skip risurrezione (cooldown {}s non ancora scaduto)",
                    cooldown.as_secs()
                );
                return;
            }
        }
        *last = Some(Instant::now());
    }

    // 4. Risurrezione via Sudo Manager (unico canale privilegiato, mig 0289).
    tracing::warn!(
        "user_manager: bus --user giu', tentativo di risurrezione via sudo_manager ({RESURRECTION_PURPOSE})"
    );
    match crate::sudo_manager::execute(db, RESURRECTION_PURPOSE).await {
        Ok(outcome) if outcome.success => {
            if crate::project_workspace::wizard::systemd_user_available().await {
                tracing::info!(
                    "user_manager: manager utente risuscitato (systemd --user attivo, {}ms)",
                    outcome.duration_ms
                );
            } else {
                tracing::warn!(
                    "user_manager: comando eseguito ma il bus --user e' ancora giu' -> degradazione a fallback detached"
                );
            }
        }
        Ok(outcome) => {
            tracing::warn!(
                "user_manager: risurrezione fallita (exit={}): {}",
                outcome.exit_code,
                outcome.stderr.trim()
            );
        }
        Err(e) => {
            tracing::warn!(
                "user_manager: risurrezione non eseguibile: {e} (verifica deploy/install-user-manager.sh e install-sudo-manager.sh)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_rispetta_il_floor_anche_con_setting_zero() {
        // Anche con setting 0 il floor di 60s resta applicato.
        assert_eq!(effective_cooldown(0), Duration::from_secs(60));
        assert_eq!(effective_cooldown(30), Duration::from_secs(60));
    }

    #[test]
    fn cooldown_usa_il_valore_configurato_se_sopra_il_floor() {
        assert_eq!(effective_cooldown(120), Duration::from_secs(120));
        assert_eq!(effective_cooldown(300), Duration::from_secs(300));
    }

    #[test]
    fn cooldown_al_confine_del_floor() {
        assert_eq!(effective_cooldown(60), Duration::from_secs(60));
        assert_eq!(effective_cooldown(61), Duration::from_secs(61));
    }
}

//! Punto unico (regola L) per la verifica di liveness di un processo dato il PID,
//! in modo cross-platform. Estratto da `security::port_enforcer` durante il porting
//! a Windows nativo: l'implementazione storica leggeva solo `/proc/{pid}` (Linux),
//! restituendo SEMPRE `false` su Windows (port-enforcement cieco sulla liveness).
//!
//! I controlli di liveness e le terminazioni di singolo PID del sottosistema
//! GESTIONE PORTE (project_workspace/services.rs, port_recovery.rs, port_registry.rs,
//! security/port_enforcer.rs) delegano ora a questo modulo. Restano letture `/proc`
//! puramente Linux-centriche (net/tcp, cwd, PPid/stat, comm) marcate `#[cfg(unix)]`
//! o degradate a vuoto su Windows, dove non hanno equivalente affidabile.

/// `true` se esiste un processo vivo con questo `pid`.
///
/// - Unix: presenza di `/proc/{pid}` (coerente con l'implementazione storica).
/// - Windows: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` riesce solo se il PID
///   esiste ed e' accessibile; un PID terminato non e' apribile (handle 0 -> non-vivo).
///   Non si usa `WaitForSingleObject` perche' richiederebbe il diritto `SYNCHRONIZE`.
#[cfg(unix)]
pub(crate) fn process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(windows)]
pub(crate) fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    if pid == 0 {
        return false;
    }
    // In windows-sys 0.52 HANDLE e' un isize; OpenProcess ritorna 0 su fallimento
    // (accesso negato o PID inesistente), non INVALID_HANDLE_VALUE.
    // SAFETY: `pid` e' un intero senza vincoli; OpenProcess restituisce 0 in caso di
    // fallimento (gestito sotto). L'handle non-nullo viene subito rilasciato con
    // CloseHandle. Nessun puntatore a memoria condivisa, nessun aliasing.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == 0 {
            return false;
        }
        CloseHandle(handle);
        true
    }
}

/// Termina il processo `pid` in modo best-effort. PUNTO UNICO (regola L) per la
/// terminazione di un singolo PID: sostituisce i `Command::new("kill")` inline
/// sparsi (services, crud, port_enforcer), che su Windows erano no-op silenziosi
/// (`kill` non esiste -> processi mai terminati, porte mai liberate).
///
/// - Unix: `SIGTERM` (graceful), poi `SIGKILL` se ancora vivo dopo ~500ms.
/// - Windows: `taskkill /PID <pid> /T /F` — `/T` termina anche l'albero dei figli
///   (unico equivalente affidabile al kill del process group POSIX), `/F` forza.
///
/// Anti-suicidio: non tocca mai `pid` 0 o il proprio PID (coerente regola E).
#[cfg(unix)]
pub(crate) async fn kill_pid(pid: u32) {
    if pid == 0 || pid == std::process::id() {
        return;
    }
    let _ = tokio::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .output()
        .await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    if process_alive(pid) {
        let _ = tokio::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .output()
            .await;
    }
}

#[cfg(windows)]
pub(crate) async fn kill_pid(pid: u32) {
    if pid == 0 || pid == std::process::id() {
        return;
    }
    let _ = tokio::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output()
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_e_vivo() {
        // Il processo corrente deve sempre risultare vivo, su qualunque OS.
        assert!(process_alive(std::process::id()));
    }

    #[test]
    fn pid_inesistente_non_e_vivo() {
        // Un PID assurdo non deve mai risultare vivo.
        assert!(!process_alive(u32::MAX));
    }
}

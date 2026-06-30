//! Punto unico (regola L) per la verifica di liveness di un processo dato il PID,
//! in modo cross-platform. Estratto da `security::port_enforcer` durante il porting
//! a Windows nativo: l'implementazione storica leggeva solo `/proc/{pid}` (Linux),
//! restituendo SEMPRE `false` su Windows (port-enforcement cieco sulla liveness).
//!
//! NB: nel codebase restano altri controlli di liveness inline basati su
//! `/proc/{pid}` (project_workspace/services.rs, port_recovery.rs, logs.rs): fanno
//! parte del sottosistema di monitoring Linux-centrico, inerte su Windows, e vanno
//! migrati a delegare qui in un intervento dedicato (non bloccante per la baseline).

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

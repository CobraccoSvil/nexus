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

/// Metriche OS di un processo, cross-platform. PUNTO UNICO (regola L) del
/// campionamento risorse per PID: il `service_observer` (e chiunque altro serva)
/// legge da qui invece di re-implementare `/proc` (Linux) o le API Win32.
///
/// `cpu_seconds` e' il tempo CPU CUMULATIVO consumato dal processo dall'avvio
/// (user+kernel), gia' NORMALIZZATO in secondi: cosi' il calcolo della
/// percentuale CPU nel chiamante e' indipendente dall'OS
/// (`cpu_pct = delta_cpu_seconds / delta_wall_seconds * 100`), senza dover
/// conoscere USER_HZ (Linux) o l'unita' 100ns (Windows).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ProcessMetrics {
    /// Tempo CPU cumulativo (user+kernel) in secondi.
    pub cpu_seconds: f64,
    /// Working set / RSS in byte.
    pub rss_bytes: u64,
    /// Byte letti dal processo (I/O counters). 0 se non disponibile.
    pub io_read_bytes: u64,
    /// Byte scritti dal processo (I/O counters). 0 se non disponibile.
    pub io_write_bytes: u64,
}

/// Legge le metriche OS del processo `pid`. `None` se il PID non esiste piu'
/// (processo terminato) o non e' accessibile.
///
/// - Unix: `/proc/<pid>/{stat,statm,io}` (utime+stime in USER_HZ -> secondi,
///   pagine RSS -> byte, read_bytes/write_bytes).
/// - Windows: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` +
///   `GetProcessTimes` (kernel+user FILETIME in unita' 100ns -> secondi) +
///   `K32GetProcessMemoryInfo` (WorkingSetSize) + `GetProcessIoCounters`.
#[cfg(unix)]
pub(crate) fn read_process_metrics(pid: u32) -> Option<ProcessMetrics> {
    /// USER_HZ standard Linux: i tick di /proc/<pid>/stat sono 1/100 di secondo.
    const USER_HZ: f64 = 100.0;
    /// Dimensione pagina (Linux x86_64) per convertire le pagine RSS in byte.
    const PAGE_SIZE_BYTES: u64 = 4096;

    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Il campo comm (2o) e' tra parentesi e puo' contenere spazi: si parte dopo ')'.
    let after = stat.rsplit_once(')').map(|(_, b)| b).unwrap_or(&stat);
    let fields: Vec<&str> = after.split_whitespace().collect();
    // Dopo ')' i campi ripartono da "state" (campo 3). utime=campo14 -> idx 11,
    // stime=campo15 -> idx 12.
    let utime = fields
        .get(11)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let stime = fields
        .get(12)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let cpu_seconds = (utime + stime) as f64 / USER_HZ;

    let rss_bytes = std::fs::read_to_string(format!("/proc/{pid}/statm"))
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .nth(1)
                .and_then(|p| p.parse::<u64>().ok())
        })
        .map(|pages| pages * PAGE_SIZE_BYTES)
        .unwrap_or(0);

    let (mut io_read, mut io_write) = (0u64, 0u64);
    if let Ok(io) = std::fs::read_to_string(format!("/proc/{pid}/io")) {
        for line in io.lines() {
            if let Some(v) = line.strip_prefix("read_bytes:") {
                io_read = v.trim().parse().unwrap_or(0);
            } else if let Some(v) = line.strip_prefix("write_bytes:") {
                io_write = v.trim().parse().unwrap_or(0);
            }
        }
    }

    Some(ProcessMetrics {
        cpu_seconds,
        rss_bytes,
        io_read_bytes: io_read,
        io_write_bytes: io_write,
    })
}

#[cfg(windows)]
pub(crate) fn read_process_metrics(pid: u32) -> Option<ProcessMetrics> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessIoCounters, GetProcessTimes, OpenProcess, IO_COUNTERS,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return None;
    }
    // FILETIME in unita' 100ns: (dwHigh<<32 | dwLow) tick * 100ns = secondi.
    fn filetime_to_secs(ft: FILETIME) -> f64 {
        let ticks = ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64);
        ticks as f64 * 1e-7
    }

    // SAFETY: `pid` e' un intero senza vincoli. OpenProcess ritorna 0 su
    // fallimento (PID inesistente / accesso negato): gestito con early-return.
    // L'handle non-nullo viene SEMPRE rilasciato con CloseHandle prima di
    // uscire. Tutte le struct Win32 passate come *mut sono stack-locali,
    // zero-inizializzate e non escono dallo scope: nessun aliasing, nessun
    // puntatore condiviso. I BOOL di ritorno delle Get* sono controllati; su
    // 0 (fallimento) il valore corrispondente resta al default (0), documentato.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == 0 {
            return None;
        }

        // CPU: kernel+user time. creation/exit non servono ma sono obbligatori.
        let mut creation = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut exit = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut kernel = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut user = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut cpu_seconds = 0.0;
        if GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) != 0 {
            cpu_seconds = filetime_to_secs(kernel) + filetime_to_secs(user);
        }

        // RSS: WorkingSetSize da K32GetProcessMemoryInfo (kernel32, nessuna dep
        // psapi separata). cb va inizializzato alla dimensione della struct.
        let mut mem: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        mem.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let mut rss_bytes = 0u64;
        if K32GetProcessMemoryInfo(handle, &mut mem, mem.cb) != 0 {
            rss_bytes = mem.WorkingSetSize as u64;
        }

        // I/O: byte trasferiti in lettura/scrittura. Se la chiamata fallisce
        // (permessi), restano 0 (documentato, equivalente al /proc/io assente).
        let mut io: IO_COUNTERS = std::mem::zeroed();
        let (mut io_read_bytes, mut io_write_bytes) = (0u64, 0u64);
        if GetProcessIoCounters(handle, &mut io) != 0 {
            io_read_bytes = io.ReadTransferCount;
            io_write_bytes = io.WriteTransferCount;
        }

        CloseHandle(handle);
        Some(ProcessMetrics {
            cpu_seconds,
            rss_bytes,
            io_read_bytes,
            io_write_bytes,
        })
    }
}

/// (Solo Windows) Istante di AVVIO del processo `pid` come epoch unix (secondi).
/// `None` se il PID non esiste/non e' accessibile o il creation-time non e'
/// leggibile.
///
/// Serve a VALIDARE L'IDENTITA' di un PID persistito (regola M): Windows ricicla
/// i PID in modo aggressivo, quindi un `pid` letto dal DB (`agent_processes.pid`,
/// mai azzerato allo stop) puo' gia' appartenere a un processo ESTRANEO. Il
/// chiamante confronta questo start-time con lo `started_at` atteso del servizio:
/// se non combaciano (entro tolleranza), il PID e' riciclato e il servizio va
/// trattato come morto. `process_alive`/`read_process_metrics` da soli non
/// bastano: `OpenProcess` riesce su QUALSIASI processo con quel PID.
///
/// Su Unix questa validazione non serve (il PID viene da `systemctl MainPID`
/// fresco a ogni ciclo, mai stantio), quindi la funzione e' Windows-only.
#[cfg(windows)]
pub(crate) fn process_start_unix(pid: u32) -> Option<i64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return None;
    }
    // FILETIME e' in unita' 100ns dal 1601-01-01 UTC. Per l'epoch unix (1970)
    // si sottrae l'offset tra le due epoche in unita' 100ns.
    const FILETIME_UNIX_EPOCH_OFFSET_100NS: u64 = 11_644_473_600 * 10_000_000;

    // SAFETY: come in read_process_metrics/process_alive. OpenProcess ritorna 0
    // su fallimento (gestito). L'handle non-nullo e' SEMPRE rilasciato con
    // CloseHandle. `creation`/`exit`/`kernel`/`user` sono stack-locali zero-init,
    // passati come *mut e non escono dallo scope: nessun aliasing.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == 0 {
            return None;
        }
        let mut creation = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut exit = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut kernel = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut user = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let ok = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        let ft_100ns = ((creation.dwHighDateTime as u64) << 32) | (creation.dwLowDateTime as u64);
        if ft_100ns < FILETIME_UNIX_EPOCH_OFFSET_100NS {
            return None;
        }
        Some(((ft_100ns - FILETIME_UNIX_EPOCH_OFFSET_100NS) / 10_000_000) as i64)
    }
}

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
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return false;
    }
    // ATTENZIONE (fix root-cause): `OpenProcess` da solo NON basta a decidere la
    // liveness su Windows. Riesce (handle non-zero) anche su un processo GIA'
    // USCITO, finche' un handle al suo process object resta aperto altrove (es. la
    // shell padre che l'ha lanciato): il PID non e' riusato ma il processo e'
    // morto (zombie non-reaped, invisibile a Get-Process). Il vecchio codice
    // ritornava true in questo caso -> falso positivo su TUTTI i processi
    // terminati-non-reaped, e i consumer (riconciliazione stato servizi, detect
    // porte, cleanup) trattavano i morti come vivi. Serve il codice di uscita:
    // `GetExitCodeProcess` ritorna STILL_ACTIVE (259) solo se il processo e'
    // ancora in esecuzione. Edge noto e trascurabile: un processo che esce con
    // codice esattamente 259 verrebbe riportato vivo (i dev server escono con 0 o
    // il codice di crash, mai 259).
    // In windows-sys 0.52 HANDLE e' un isize; OpenProcess ritorna 0 su fallimento.
    // SAFETY: `pid` e' un intero senza vincoli; OpenProcess/GetExitCodeProcess
    // gestiscono il fallimento (handle 0 / ritorno 0). L'handle viene sempre
    // rilasciato con CloseHandle. Nessun puntatore a memoria condivisa.
    const STILL_ACTIVE: u32 = 259;
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == 0 {
            return false;
        }
        let mut exit_code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut exit_code);
        CloseHandle(handle);
        ok != 0 && exit_code == STILL_ACTIVE
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

    #[test]
    fn metriche_del_processo_corrente() {
        // Il processo di test e' vivo: read_process_metrics deve ritornare Some
        // con un RSS > 0 (il working set del processo corrente non e' mai zero).
        // cpu_seconds e' cumulativo e >= 0. Cross-platform (Unix /proc, Win32).
        let m = read_process_metrics(std::process::id())
            .expect("metriche del processo corrente disponibili");
        assert!(m.rss_bytes > 0, "rss_bytes deve essere positivo");
        assert!(m.cpu_seconds >= 0.0, "cpu_seconds non negativo");
    }

    #[test]
    fn metriche_pid_inesistente_none() {
        // Un PID assurdo non deve dare metriche.
        assert!(read_process_metrics(u32::MAX).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn start_unix_del_processo_corrente_plausibile() {
        // Il creation-time del processo corrente deve essere leggibile e coerente:
        // dopo il 2020-01-01 (1577836800) e non nel futuro. Serve alla validazione
        // anti-riciclo del PID nel collector Windows dell'observer.
        let start = process_start_unix(std::process::id())
            .expect("creation-time del processo corrente leggibile");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(i64::MAX);
        assert!(start > 1_577_836_800, "start-time troppo vecchio: {start}");
        assert!(start <= now + 5, "start-time nel futuro: {start} > {now}");
    }

    #[cfg(windows)]
    #[test]
    fn start_unix_pid_inesistente_none() {
        assert!(process_start_unix(u32::MAX).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn processo_uscito_con_handle_aperto_non_e_vivo() {
        // Regressione (fix falso positivo Windows): OpenProcess riesce anche su un
        // processo GIA' USCITO finche' un handle al process object resta aperto
        // (qui lo tiene lo std::process::Child NON reaped). process_alive deve
        // comunque riportarlo morto grazie al check del codice di uscita. Col
        // vecchio codice (solo OpenProcess) sarebbe rimasto "vivo" per sempre e il
        // loop andrebbe in timeout -> assert fallito.
        use std::process::Command;
        use std::time::Duration;
        let mut child = Command::new("cmd")
            .args(["/C", "exit 0"])
            .spawn()
            .expect("spawn del processo di test");
        let pid = child.id();
        // Attendi (senza reap: l'handle resta aperto nel Child) che process_alive
        // rilevi l'uscita. Timeout 3s: deterministico, non dipende dall'ordine.
        let mut dead = false;
        for _ in 0..60 {
            if !process_alive(pid) {
                dead = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.wait();
        assert!(
            dead,
            "un processo uscito (handle ancora aperto) deve risultare morto entro il timeout"
        );
    }
}

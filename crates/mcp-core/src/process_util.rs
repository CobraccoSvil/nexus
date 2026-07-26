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

/// (Solo Windows) Tolleranza (secondi) nel confronto tra il creation-time reale
/// del processo e lo `started_at` registrato in `agent_processes`, per validare
/// l'identita' del PID (anti-riciclo). `started_at` e' impostato a NOW() subito
/// dopo lo spawn della shell, il creation-time del figlio arriva pochi istanti
/// dopo: lo scarto legittimo e' di frazioni di secondo. Un PID riciclato ha
/// creation-time arbitrario (tipicamente molto distante). Margine ampio per non
/// invalidare mai un processo vero, stretto abbastanza da scartare un estraneo.
#[cfg(windows)]
pub(crate) const PID_IDENTITY_TOLERANCE_S: i64 = 10;

/// (Solo Windows) Predicato puro (regola L, testabile) dell'identita' di un PID:
/// il creation-time reale del processo (`real_start`, epoch unix) combacia con
/// lo `started_at` atteso (`expected_start`) entro `tolerance` secondi? Se non
/// combacia, il PID e' stato riciclato dal SO su un processo estraneo -> il
/// servizio va trattato come morto. Entrambi gli input sono Option: un dato
/// mancante = identita' non confermabile = false (fail-safe: meglio un possibile
/// crash segnalato che mascherato con metriche altrui).
#[cfg(windows)]
pub(crate) fn pid_identity_ok(
    real_start: Option<i64>,
    expected_start: Option<i64>,
    tolerance: i64,
) -> bool {
    match (real_start, expected_start) {
        (Some(real), Some(expected)) => (real - expected).abs() <= tolerance,
        _ => false,
    }
}

/// (Solo Windows) PUNTO UNICO (regola L) della verifica di identita' di un PID
/// persistito: il processo con quel `pid` e' ANCORA il processo registrato con
/// `started_at` atteso? Combina lettura del creation-time reale e predicato di
/// tolleranza. Usato dall'observer (stato servizi), dal port_enforcer
/// (attribuzione PID->progetto) e dalla riconciliazione del pannello Servizi:
/// senza questa verifica un PID riciclato dal SO su un processo estraneo (lsass,
/// svchost, postgres dell'infrastruttura) veniva attribuito al progetto e le sue
/// porte flaggate/killate come violazioni, o un servizio mostrato 'running'.
///
/// VINCOLO TIMEBASE: `expected_start_unix` deriva da `agent_processes.started_at`
/// (`NOW()` del server Postgres) mentre `real_start` viene dalle FILETIME Win32
/// (clock dell'host). Nell'ambiente canonico Nexus i Postgres sono NATIVI Windows
/// (`pg_ctl` su C:\Program Files\PostgreSQL, non container) -> stesso clock host,
/// scarto misurato < 1s, ben dentro la tolleranza. Se in futuro il DB girasse in
/// una VM/container con orologio derivante rispetto all'host, la tolleranza fissa
/// di 10s non basterebbe e servirebbe ancorare l'anti-riciclo a un dato host-side
/// (es. registrare il creation-time reale allo spawn, non `NOW()` del DB).
#[cfg(windows)]
pub(crate) fn pid_identity_confirmed(
    pid: u32,
    expected_start_unix: Option<i64>,
) -> bool {
    pid_identity_ok(
        process_start_unix(pid),
        expected_start_unix,
        PID_IDENTITY_TOLERANCE_S,
    )
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

/// Una riga della fotografia dei processi: chi e' il padre e come si chiama.
#[cfg(windows)]
#[derive(Debug, Clone)]
pub(crate) struct ProcessEntry {
    pub parent_pid: u32,
    /// Nome senza estensione, come lo dava `Get-Process`.ProcessName (es. `node`):
    /// i chiamanti storici confrontano quel formato.
    pub name: String,
}

/// Fotografia di TUTTI i processi (pid -> padre + nome) via Toolhelp32.
///
/// PUNTO UNICO (regola L) dell'albero processi su Windows. Sostituisce
/// `Get-CimInstance Win32_Process` e `Get-Process` lanciati in PowerShell: due
/// interpreti da avviare a ogni scansione, misurati in 3.2s e ~1s. Il
/// `port_enforcer` gira ogni 5s con un timeout di 10s, quindi quei probe da
/// soli (9.9s in due) mandavano OGNI iterazione in timeout e l'enforcement
/// delle porte non girava mai (log del 26/07: 33 "iterazione abortita").
///
/// Chiamata sincrona nell'ordine dei millisecondi: i chiamanti async la
/// avvolgono in `spawn_blocking`.
#[cfg(windows)]
pub(crate) fn windows_process_snapshot() -> std::collections::HashMap<u32, ProcessEntry> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let mut map = std::collections::HashMap::new();
    // SAFETY: `CreateToolhelp32Snapshot` non prende puntatori nostri e segnala il
    // fallimento con INVALID_HANDLE_VALUE, controllato subito sotto. `entry` e'
    // inizializzato con `dwSize` valorizzato come l'API richiede, e le due
    // Process32*W ricevono un handle valido e un puntatore alla nostra struct
    // viva per tutta la durata del ciclo. L'handle e' chiuso su ogni uscita.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return map;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                map.insert(entry.th32ProcessID, voce_da_entry(&entry));
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }
    map
}

/// Proietta una riga di Toolhelp32 nella nostra voce.
#[cfg(windows)]
fn voce_da_entry(
    e: &windows_sys::Win32::System::Diagnostics::ToolHelp::PROCESSENTRY32W,
) -> ProcessEntry {
    ProcessEntry {
        parent_pid: e.th32ParentProcessID,
        name: exe_name_senza_estensione(&e.szExeFile),
    }
}

/// `szExeFile` (UTF-16 terminato da NUL) -> nome senza `.exe`.
#[cfg(windows)]
fn exe_name_senza_estensione(raw: &[u16]) -> String {
    let fine = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    let nome = String::from_utf16_lossy(&raw[..fine]);
    match nome.rfind('.') {
        Some(i) if nome[i..].eq_ignore_ascii_case(".exe") => nome[..i].to_string(),
        _ => nome,
    }
}

/// Porte TCP in ascolto con il PID che le possiede, via `GetExtendedTcpTable`.
///
/// PUNTO UNICO (regola L) del rilevamento porte su Windows, gemello di
/// [`windows_process_snapshot`]. Sostituisce `Get-NetTCPConnection` in
/// PowerShell (misurato 6.7s per invocazione).
///
/// Enumera **entrambe** le famiglie di indirizzi. Non e' completezza per
/// simmetria: Node e Vite bindano `::` per default, e su questa macchina 6
/// porte risultano in ascolto SOLO su IPv6 — fra cui la 3000 (web-ide) e la
/// 32987, che sta dentro il bucket dei progetti. Enumerare il solo IPv4
/// renderebbe il `port_enforcer` cieco proprio sulla classe di processi che
/// deve sorvegliare, e in modo peggiore della cecita' totale: la sua guardia
/// fail-closed scatta solo su lista VUOTA, quindi con una lista parziale lo
/// sweep girerebbe comunque e `resolve_stale_runtime_port_violations`
/// chiuderebbe come "rientrate" violazioni ancora vive.
///
/// `TCP_TABLE_OWNER_PID_LISTENER` fa filtrare i soli listener al kernel: non
/// c'e' uno stato da riconoscere a valle. Chiamata sincrona nell'ordine dei
/// millisecondi: i chiamanti async la avvolgono in `spawn_blocking`.
#[cfg(windows)]
pub(crate) fn windows_listening_sockets() -> Vec<(u16, u32)> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        MIB_TCP6TABLE_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
    };
    use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

    let mut out: Vec<(u16, u32)> = Vec::new();
    raccogli_listener::<MIB_TCPTABLE_OWNER_PID>(AF_INET as u32, &mut out);
    raccogli_listener::<MIB_TCP6TABLE_OWNER_PID>(AF_INET6 as u32, &mut out);

    // Un processo in ascolto su `::` con dual-stack compare in entrambe le
    // tabelle: la stessa coppia (porta, pid) non e' due binding distinti.
    out.sort_unstable();
    out.dedup();
    out
}

/// Le due MIB dei listener (IPv4 e IPv6) hanno campi omonimi ma tipi distinti,
/// e nessun tratto comune in `windows-sys`. Questo tratto e' il loro minimo
/// denominatore, cosi' la lettura della tabella si scrive UNA volta sola
/// (regola L) invece di essere copiata e adattata per famiglia.
#[cfg(windows)]
trait MibListener {
    type Riga;
    fn righe(&self) -> usize;
    fn prima_riga(&self) -> *const Self::Riga;
    /// `(porta in ordine di rete, pid proprietario)` della riga.
    fn porta_e_pid(riga: &Self::Riga) -> (u32, u32);
}

/// Le due implementazioni differiscono SOLO per la coppia di tipi FFI: i corpi
/// sono identici (campi omonimi). Scriverle a mano due volte sarebbe copia-e-
/// adatta, il modo in cui due rami gemelli iniziano a divergere.
#[cfg(windows)]
macro_rules! impl_mib_listener {
    ($tabella:ty, $riga:ty) => {
        impl MibListener for $tabella {
            type Riga = $riga;
            fn righe(&self) -> usize {
                self.dwNumEntries as usize
            }
            fn prima_riga(&self) -> *const Self::Riga {
                self.table.as_ptr()
            }
            fn porta_e_pid(riga: &Self::Riga) -> (u32, u32) {
                (riga.dwLocalPort, riga.dwOwningPid)
            }
        }
    };
}

#[cfg(windows)]
impl_mib_listener!(
    windows_sys::Win32::NetworkManagement::IpHelper::MIB_TCPTABLE_OWNER_PID,
    windows_sys::Win32::NetworkManagement::IpHelper::MIB_TCPROW_OWNER_PID
);

#[cfg(windows)]
impl_mib_listener!(
    windows_sys::Win32::NetworkManagement::IpHelper::MIB_TCP6TABLE_OWNER_PID,
    windows_sys::Win32::NetworkManagement::IpHelper::MIB_TCP6ROW_OWNER_PID
);

/// Legge la tabella dei listener della famiglia `af` e accoda le coppie
/// `(porta, pid)` valide. Una sola implementazione per entrambe le famiglie:
/// il filtro delle righe prive di senso non puo' divergere fra IPv4 e IPv6.
#[cfg(windows)]
fn raccogli_listener<T: MibListener>(af: u32, out: &mut Vec<(u16, u32)>) {
    let Some(buffer) = tabella_listener::<T>(af) else {
        return;
    };
    // SAFETY: `tabella_listener` ritorna Some solo dopo una chiamata riuscita,
    // quindi l'header e' inizializzato e `dwNumEntries` righe lo seguono
    // contigue (layout documentato della MIB).
    unsafe {
        let table = &*buffer.as_ptr();
        for riga in std::slice::from_raw_parts(table.prima_riga(), table.righe()) {
            let (local_port, owning_pid) = T::porta_e_pid(riga);
            let porta = porta_da_dword_network_order(local_port);
            if porta > 0 && owning_pid > 0 {
                out.push((porta, owning_pid));
            }
        }
    }
}

/// Alloca e riempie la tabella dei listener per la famiglia `af`, col protocollo
/// a due chiamate documentato da `GetExtendedTcpTable` (la prima dice quanto
/// spazio serve).
///
/// Il buffer e' un `Vec<T>` e non un `Vec<u8>` per ottenere l'allineamento che
/// la struct richiede.
#[cfg(windows)]
fn tabella_listener<T>(af: u32) -> Option<Vec<T>> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, TCP_TABLE_OWNER_PID_LISTENER,
    };

    const NO_ERROR_U32: u32 = 0;
    const ERROR_INSUFFICIENT_BUFFER_U32: u32 = 122;

    // Le due chiamate differiscono SOLO per il buffer di destinazione: qui il
    // resto degli argomenti si scrive una volta.
    // SAFETY: `size` e' sempre la capacita' in byte di cio' che `ptr` indirizza
    // (0 e null alla prima chiamata); l'API non scrive oltre quel limite.
    let chiama = |ptr: *mut std::ffi::c_void, size: &mut u32| -> u32 {
        unsafe { GetExtendedTcpTable(ptr, size, 0, af, TCP_TABLE_OWNER_PID_LISTENER, 0) }
    };

    let mut size: u32 = 0;
    if chiama(std::ptr::null_mut(), &mut size) != ERROR_INSUFFICIENT_BUFFER_U32 || size == 0 {
        return None;
    }

    let elementi = (size as usize).div_ceil(std::mem::size_of::<T>());
    let mut buffer: Vec<T> = Vec::with_capacity(elementi.max(1));
    if chiama(buffer.as_mut_ptr().cast(), &mut size) != NO_ERROR_U32 {
        return None;
    }
    Some(buffer)
}

/// La MIB tiene la porta nei due byte bassi del DWORD, in ordine di rete.
/// Leggerla come intero nativo darebbe numeri come 32798 al posto di 8080.
#[cfg(windows)]
fn porta_da_dword_network_order(raw: u32) -> u16 {
    (((raw & 0x0000_00FF) << 8) | ((raw & 0x0000_FF00) >> 8)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La fotografia dei processi deve contenere ME: e' il solo PID di cui il
    /// test conosce con certezza l'esistenza, e ne conosce anche il nome.
    #[cfg(windows)]
    #[test]
    fn la_fotografia_dei_processi_contiene_il_processo_corrente() {
        let snapshot = windows_process_snapshot();
        let io = snapshot
            .get(&std::process::id())
            .expect("il processo di test manca dalla fotografia");
        assert!(
            !io.name.is_empty() && !io.name.to_ascii_lowercase().ends_with(".exe"),
            "nome inatteso (deve essere senza estensione, come Get-Process): {}",
            io.name
        );
        // Un processo ha sempre un padre (anche se gia' morto): il campo esiste.
        assert!(snapshot.len() > 1, "una sola voce: enumerazione interrotta");
    }

    /// Un sistema Windows vivo ha SEMPRE qualche listener (RPC, SMB, il DB
    /// locale...). Zero porte significa rilevamento cieco, che e' esattamente
    /// il modo in cui questo probe puo' fallire in silenzio.
    #[cfg(windows)]
    #[test]
    fn le_porte_in_ascolto_non_escono_mai_vuote() {
        let porte = windows_listening_sockets();
        assert!(!porte.is_empty(), "nessuna porta in ascolto: probe cieco");
        assert!(
            porte.iter().all(|(porta, pid)| *porta > 0 && *pid > 0),
            "riga con porta o pid nullo: {porte:?}"
        );
    }

    /// Il probe deve vedere un listener IPv6, non solo IPv4: Node e Vite bindano
    /// `::` per default, e su questa macchina 6 porte (fra cui la 3000 e la
    /// 32987, dentro il bucket progetti) risultano in ascolto SOLO su IPv6.
    ///
    /// Il test apre un socket VERO e ne cerca la porta: non c'e' modo di
    /// superarlo enumerando la sola tabella IPv4. "Le porte non escono mai
    /// vuote" invece resterebbe verde, perche' le decine di listener IPv4 della
    /// macchina bastano a riempire la lista — ed e' proprio quel riempimento
    /// parziale a disinnescare la guardia fail-closed del port_enforcer.
    #[cfg(windows)]
    #[test]
    fn il_probe_vede_anche_i_listener_ipv6() {
        let listener = match std::net::TcpListener::bind("[::1]:0") {
            Ok(l) => l,
            // Se l'host non ha lo stack IPv6 il test non ha oggetto: dirlo, non
            // fingere di aver verificato qualcosa.
            Err(e) => {
                eprintln!("IPv6 non disponibile su questo host, test saltato: {e}");
                return;
            }
        };
        let porta = listener.local_addr().expect("indirizzo locale").port();
        let mio_pid = std::process::id();
        let visti = windows_listening_sockets();
        assert!(
            visti.contains(&(porta, mio_pid)),
            "listener IPv6 su [::1]:{porta} (pid {mio_pid}) invisibile al probe: \
             enumerazione limitata a IPv4 -> il port_enforcer sarebbe cieco \
             proprio sui processi Node dei progetti"
        );
    }

    /// IL DIFETTO MISURATO (log mcp-core 26/07, 33 "iterazione abortita"): i due
    /// probe in PowerShell costavano 6.7s + 3.2s = 9.9s, contro un timeout di
    /// scan di 10s e un intervallo di 5s. Ogni iterazione del port_enforcer
    /// finiva in timeout e l'enforcement delle porte non girava mai.
    ///
    /// La soglia separa DUE REGIMI, non misura una prestazione: syscall = 21ms
    /// misurati, avvio di un interprete esterno = 1.1s il piu' veloce osservato
    /// (PowerShell gia' caldo; a freddo 3.2s e 6.7s). 300ms sta 14x sopra il
    /// primo e 3x sotto il secondo, quindi non e' flaky sotto carico e cattura
    /// comunque la reintroduzione di UN SOLO comando esterno.
    ///
    /// La prima stesura usava 2s: la mutazione la attraversava indenne, perche'
    /// un PowerShell caldo ci sta sotto. Un test che non rosseggia quando
    /// rimetti il difetto copre solo se stesso (regola O).
    #[cfg(windows)]
    #[test]
    fn i_due_probe_costano_millisecondi_non_secondi() {
        let inizio = std::time::Instant::now();
        let _ = windows_process_snapshot();
        let _ = windows_listening_sockets();
        let durata = inizio.elapsed();
        assert!(
            durata < std::time::Duration::from_millis(300),
            "i probe hanno impiegato {durata:?}: e' il regime del processo \
             esterno, quello che con un intervallo di scan di 5s e un timeout \
             di 10s abortiva ogni iterazione del port_enforcer"
        );
    }

    #[cfg(windows)]
    #[test]
    fn la_porta_si_legge_in_ordine_di_rete() {
        // 8080 sul wire e' 0x901F letto come intero nativo: senza lo scambio
        // dei due byte bassi il port_enforcer confronterebbe col bucket una
        // porta che non esiste.
        assert_eq!(porta_da_dword_network_order(0x0000_901F), 8080);
        assert_eq!(porta_da_dword_network_order(0x0000_5000), 80);
        assert_eq!(porta_da_dword_network_order(0x0000_BB01), 443);
    }

    #[cfg(windows)]
    #[test]
    fn il_nome_perde_solo_lestensione_eseguibile() {
        let utf16 = |s: &str| {
            let mut v: Vec<u16> = s.encode_utf16().collect();
            v.push(0);
            v
        };
        assert_eq!(exe_name_senza_estensione(&utf16("node.exe")), "node");
        assert_eq!(exe_name_senza_estensione(&utf16("Node.EXE")), "Node");
        // Un punto che non introduce l'estensione eseguibile resta dov'e'.
        assert_eq!(
            exe_name_senza_estensione(&utf16("my.service.host")),
            "my.service.host"
        );
        assert_eq!(exe_name_senza_estensione(&utf16("senza-punto")), "senza-punto");
    }

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
    fn pid_identity_riconosce_riciclo() {
        // Creation-time entro tolleranza dello started_at atteso = identita' OK
        // (e' il nostro servizio). started_at 1000, real 1002, tolleranza 10.
        assert!(pid_identity_ok(Some(1002), Some(1000), 10));
        assert!(pid_identity_ok(Some(1000), Some(1000), 10));
        // Creation-time molto distante = PID riciclato su un processo estraneo
        // (avviato in un altro momento) -> identita' FALLITA -> servizio morto.
        assert!(!pid_identity_ok(Some(1050), Some(1000), 10));
        assert!(!pid_identity_ok(Some(500), Some(1000), 10));
        // Dato mancante (started_at NULL o creation-time non leggibile) = identita'
        // non confermabile -> false (fail-safe).
        assert!(!pid_identity_ok(None, Some(1000), 10));
        assert!(!pid_identity_ok(Some(1000), None, 10));
        assert!(!pid_identity_ok(None, None, 10));
    }

    #[cfg(windows)]
    #[test]
    fn pid_identity_confirmed_processo_corrente() {
        // Il processo corrente con il PROPRIO start-time reale come atteso deve
        // confermare l'identita'; con uno started_at lontano deve rifiutarla
        // (simula il riciclo: riga DB stantia con PID riassegnato).
        let me = std::process::id();
        let real = process_start_unix(me).expect("start-time leggibile");
        assert!(pid_identity_confirmed(me, Some(real)));
        assert!(!pid_identity_confirmed(me, Some(real - 3600)));
        assert!(!pid_identity_confirmed(me, None));
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

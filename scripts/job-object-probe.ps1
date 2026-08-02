<#
.SYNOPSIS
  ADR 0042 / P0(a) — misura del PERNO: il Job Object di Windows regge come prova
  di appartenenza ("questo processo e' suo") per i servizi di progetto?

.DESCRIPTION
  Nel repo ci sono ZERO occorrenze di JobObject/CreateJobObject/
  AssignProcessToJobObject: e' una capacita' ASSUNTA, mai usata. Questo script la
  MISURA prima che qualcuno ci disegni sopra uno schema.

  Tre domande, tre misure, nessuna opinione:

  1. CONTENIMENTO. Venti avvii reali (node, cargo, python, npm, pnpm, npx, cmd,
     powershell), ognuno in un Job Object dedicato, creati SOSPESI e assegnati
     PRIMA di partire (nessuna corsa fra spawn e assegnazione). Per ciascuno:
     AssignProcessToJobObject riesce? IsProcessInJob conferma? E quanti
     DISCENDENTI il kernel conta nel job mentre gira (i wrapper .cmd fanno
     cmd -> node: il contenimento vale solo se ci finiscono anche loro)?

  2. NAMESPACE. Un nome "Local\..." vive nel namespace della sessione. Se
     launcher e osservatore stanno in sessioni diverse, OpenJobObject non
     risolve e ogni istanza diventa 'lost' dopo un riavvio di mcp-core. Si
     misura risolvendo lo STESSO nome per percorso assoluto nelle due sessioni
     (\Sessions\N\BaseNamedObjects\...), e tentando "Global\..." che richiede
     SeCreateGlobalPrivilege.

  3. SOPRAVVIVENZA. Il caso per cui il meccanismo esiste: mcp-core muore, il
     servizio no. Si chiude l'ULTIMO handle al job e si tenta di riaprirlo per
     nome. Se il nome non risolve piu', il perno non regge un riavvio.

.PARAMETER Mode
  probe    (default) esegue le tre misure nella sessione corrente.
  observe  osservatore puro: tenta di aprire un job per nome e riporta JSON.
           Serve alla misura cross-sessione (vedi -Session0).
  session0 orchestra la misura cross-sessione: crea il job in questa sessione e
           lancia -Mode observe come SYSTEM (sessione 0) via Utilita' di
           pianificazione. RICHIEDE ELEVAZIONE: registra e poi rimuove un task
           temporaneo. Senza elevazione dichiara la misura come non eseguita.

.PARAMETER JsonOut
  Percorso in cui scrivere il JSON. Se omesso il JSON va su stdout.

.EXAMPLE
  powershell -NoProfile -File scripts\job-object-probe.ps1
  powershell -NoProfile -File scripts\job-object-probe.ps1 -Mode session0
#>
[CmdletBinding()]
param(
    [ValidateSet('probe', 'observe', 'session0')]
    [string]$Mode = 'probe',
    [string]$JobName,
    [string]$JsonOut,
    [int]$HoldSeconds = 30,
    [switch]$Quiet
)

$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------------------
# Confine col kernel. Unico punto in cui questo script parla Win32.
# ---------------------------------------------------------------------------
if (-not ('NexusJobProbe.Kernel' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

namespace NexusJobProbe {

[StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
public struct STARTUPINFO {
    public int cb;
    public string lpReserved;
    public string lpDesktop;
    public string lpTitle;
    public int dwX, dwY, dwXSize, dwYSize, dwXCountChars, dwYCountChars;
    public int dwFillAttribute, dwFlags;
    public short wShowWindow, cbReserved2;
    public IntPtr lpReserved2, hStdInput, hStdOutput, hStdError;
}

[StructLayout(LayoutKind.Sequential)]
public struct PROCESS_INFORMATION {
    public IntPtr hProcess, hThread;
    public int dwProcessId, dwThreadId;
}

public static class Kernel {
    public const uint CREATE_SUSPENDED = 0x00000004;
    public const uint CREATE_NO_WINDOW = 0x08000000;
    // Diritto MINIMO che serve a chiedere i membri. Con ALL_ACCESS un
    // "non risolto" sarebbe ambiguo fra nome assente e diritti insufficienti.
    public const uint JOB_QUERY        = 0x0004;
    public const int  JobObjectBasicProcessIdList = 3;
    public const uint HANDLE_FLAG_INHERIT = 0x00000001;

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr CreateJobObjectW(IntPtr attrs, string name);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr OpenJobObjectW(uint access, bool inherit, string name);

    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);

    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool IsProcessInJob(IntPtr process, IntPtr job,
        [MarshalAs(UnmanagedType.Bool)] out bool result);

    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool QueryInformationJobObject(IntPtr job, int infoClass,
        IntPtr info, uint cb, IntPtr returned);

    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool TerminateJobObject(IntPtr job, uint exitCode);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern bool CreateProcessW(string app, StringBuilder cmdline,
        IntPtr pattr, IntPtr tattr, bool inherit, uint flags, IntPtr env,
        string cwd, ref STARTUPINFO si, out PROCESS_INFORMATION pi);

    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern uint ResumeThread(IntPtr thread);

    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool TerminateProcess(IntPtr process, uint code);

    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool GetExitCodeProcess(IntPtr process, out uint code);

    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern uint WaitForSingleObject(IntPtr handle, uint ms);

    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool CloseHandle(IntPtr handle);

    [DllImport("kernel32.dll")]
    public static extern IntPtr GetCurrentProcess();

    [DllImport("kernel32.dll", SetLastError = true)]
    public static extern bool SetHandleInformation(IntPtr handle, uint mask, uint flags);

    // Controllo incrociato sul privilegio: la section e' il tipo su cui
    // SeCreateGlobalPrivilege e' notoriamente verificato. Se il job passa e la
    // section no, il controllo e' PER TIPO, non per namespace.
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr CreateFileMappingW(IntPtr file, IntPtr attrs, uint protect,
        uint maxHigh, uint maxLow, string name);

    // Avvia un processo SOSPESO: l'assegnazione al job avviene prima che una
    // sola istruzione del figlio giri, quindi la misura non ha corse.
    public static PROCESS_INFORMATION SpawnSuspended(string app, string cmdline, out int err) {
        return SpawnSuspendedEx(app, cmdline, false, out err);
    }

    // inherit=true: il figlio riceve in eredita' gli handle ereditabili del
    // padre. Serve a misurare se un handle al job DENTRO un membro tiene vivo
    // il NOME dell'oggetto quando il launcher muore.
    public static PROCESS_INFORMATION SpawnSuspendedEx(string app, string cmdline, bool inherit, out int err) {
        STARTUPINFO si = new STARTUPINFO();
        si.cb = Marshal.SizeOf(typeof(STARTUPINFO));
        PROCESS_INFORMATION pi;
        bool ok = CreateProcessW(app, new StringBuilder(cmdline), IntPtr.Zero, IntPtr.Zero,
            inherit, CREATE_SUSPENDED | CREATE_NO_WINDOW, IntPtr.Zero, null, ref si, out pi);
        err = ok ? 0 : Marshal.GetLastWin32Error();
        if (!ok) { pi.hProcess = IntPtr.Zero; pi.hThread = IntPtr.Zero; pi.dwProcessId = 0; }
        return pi;
    }

    // Chiede al KERNEL chi sta nel job. E' la sola prova di appartenenza che
    // l'ADR accetta: non la somiglianza dei nomi, non la parentela.
    public static int[] JobMembers(IntPtr job, out int err) {
        const int cap = 512;
        int bytes = 8 + IntPtr.Size * cap;
        IntPtr buf = Marshal.AllocHGlobal(bytes);
        try {
            Marshal.WriteInt32(buf, 0, 0);
            Marshal.WriteInt32(buf, 4, 0);
            if (!QueryInformationJobObject(job, JobObjectBasicProcessIdList, buf, (uint)bytes, IntPtr.Zero)) {
                err = Marshal.GetLastWin32Error();
                return new int[0];
            }
            err = 0;
            int n = Marshal.ReadInt32(buf, 4);
            if (n > cap) { n = cap; }
            int[] pids = new int[n];
            for (int i = 0; i < n; i++) {
                IntPtr v = Marshal.ReadIntPtr(buf, 8 + i * IntPtr.Size);
                pids[i] = (int)v.ToInt64();
            }
            return pids;
        } finally {
            Marshal.FreeHGlobal(buf);
        }
    }
}
}
'@
}

$K = [NexusJobProbe.Kernel]

function Get-Sessione { (Get-Process -Id $PID).SessionId }

function Get-Elevazione {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal($id)).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

# Apre un job per nome e riporta l'esito TIPIZZATO (regola Q: l'esito sta in un
# campo, l'ignoto e' una variante, il testo si compone dai campi).
function Test-AperturaNome {
    param([string]$Nome)
    $h = $K::OpenJobObjectW($K::JOB_QUERY, $false, $Nome)
    if ($h -eq [IntPtr]::Zero) {
        $e = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        return [pscustomobject]@{
            nome = $Nome; esito = 'not_resolved'; win32 = $e
            win32_nome = (Get-NomeErrore $e); membri = $null
        }
    }
    $err = 0
    $pids = $K::JobMembers($h, [ref]$err)
    [void]$K::CloseHandle($h)
    [pscustomobject]@{
        nome = $Nome; esito = 'resolved'; win32 = 0
        win32_nome = 'ok'; membri = $pids.Count
    }
}

function Get-NomeErrore {
    param([int]$Codice)
    switch ($Codice) {
        0    { 'ok' }
        2    { 'ERROR_FILE_NOT_FOUND' }
        5    { 'ERROR_ACCESS_DENIED' }
        6    { 'ERROR_INVALID_HANDLE' }
        24   { 'ERROR_BAD_LENGTH' }
        87   { 'ERROR_INVALID_PARAMETER' }
        136  { 'ERROR_NOT_IN_JOB' }
        161  { 'ERROR_BAD_PATHNAME' }
        1314 { 'ERROR_PRIVILEGE_NOT_HELD' }
        default { "WIN32_$Codice" }
    }
}

# ---------------------------------------------------------------------------
# Misura 1 — contenimento su venti avvii reali
# ---------------------------------------------------------------------------
function Get-Bersagli {
    $node = (Get-Command node.exe -ErrorAction SilentlyContinue).Source
    $cargo = (Get-Command cargo.exe -ErrorAction SilentlyContinue).Source
    $py = (Get-Command python.exe -ErrorAction SilentlyContinue).Source
    $cmd = "$env:SystemRoot\system32\cmd.exe"
    $ps = "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe"

    $lista = @()
    $add = {
        param($fam, $forma, $app, $cl)
        if ($app -and (Test-Path $app)) {
            $script:lista += [pscustomobject]@{ famiglia = $fam; forma = $forma; app = $app; cmdline = $cl }
        }
    }
    $script:lista = @()

    # Eseguibile diretto: nessun wrapper fra noi e il processo.
    & $add 'node'   'exe_diretto' $node  "`"$node`" --version"
    & $add 'node'   'exe_diretto' $node  "`"$node`" -e setTimeout(function(){},2000)"
    & $add 'node'   'exe_diretto' $node  "`"$node`" -e setTimeout(function(){},2000)"
    & $add 'cargo'  'exe_diretto' $cargo "`"$cargo`" --version"
    & $add 'cargo'  'exe_diretto' $cargo "`"$cargo`" --version"
    & $add 'python' 'exe_diretto' $py    "`"$py`" --version"
    & $add 'python' 'exe_diretto' $py    "`"$py`" -c import time;time.sleep(2)"
    & $add 'python' 'exe_diretto' $py    "`"$py`" -c import time;time.sleep(2)"

    # Shim .cmd: la forma reale con cui l'ecosistema Node gira su Windows
    # (cmd.exe -> node.exe). E' qui che il contenimento dei DISCENDENTI conta.
    & $add 'npm'   'wrapper_cmd' $cmd "`"$cmd`" /d /s /c `"npm --version`""
    & $add 'npm'   'wrapper_cmd' $cmd "`"$cmd`" /d /s /c `"npm --version`""
    & $add 'pnpm'  'wrapper_cmd' $cmd "`"$cmd`" /d /s /c `"pnpm --version`""
    & $add 'pnpm'  'wrapper_cmd' $cmd "`"$cmd`" /d /s /c `"pnpm --version`""
    & $add 'npx'   'wrapper_cmd' $cmd "`"$cmd`" /d /s /c `"npx --version`""
    & $add 'npx'   'wrapper_cmd' $cmd "`"$cmd`" /d /s /c `"npx --version`""
    & $add 'cargo' 'wrapper_cmd' $cmd "`"$cmd`" /d /s /c `"cargo --version`""
    & $add 'node'  'wrapper_cmd' $cmd "`"$cmd`" /d /s /c `"node --version`""
    & $add 'cmd'   'wrapper_cmd' $cmd "`"$cmd`" /d /s /c `"ping -n 3 127.0.0.1`""

    # Shim .ps1: l'altra forma reale (powershell.exe -> npm.ps1 -> node.exe).
    & $add 'powershell' 'wrapper_ps1' $ps "`"$ps`" -NoProfile -Command Start-Sleep -Seconds 2"
    & $add 'npm'        'wrapper_ps1' $ps "`"$ps`" -NoProfile -Command npm --version"
    & $add 'pnpm'       'wrapper_ps1' $ps "`"$ps`" -NoProfile -Command pnpm --version"

    $script:lista
}

function Measure-Contenimento {
    $bersagli = Get-Bersagli
    $esiti = @()
    $i = 0
    foreach ($b in $bersagli) {
        $i++
        $nome = "Local\nexus-jobprobe-{0}-{1}" -f $PID, $i
        $job = $K::CreateJobObjectW([IntPtr]::Zero, $nome)
        if ($job -eq [IntPtr]::Zero) {
            $e = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
            $esiti += [pscustomobject]@{
                indice = $i; famiglia = $b.famiglia; forma = $b.forma
                membership = 'job_non_creato'; assign_win32 = $e
                assign_win32_nome = (Get-NomeErrore $e)
                pid = $null; membri_max = 0; exit_code = $null; durata_ms = 0
            }
            continue
        }

        $err = 0
        $pi = $K::SpawnSuspended($b.app, $b.cmdline, [ref]$err)
        if ($pi.hProcess -eq [IntPtr]::Zero) {
            [void]$K::CloseHandle($job)
            $esiti += [pscustomobject]@{
                indice = $i; famiglia = $b.famiglia; forma = $b.forma
                membership = 'spawn_fallito'; assign_win32 = $err
                assign_win32_nome = (Get-NomeErrore $err)
                pid = $null; membri_max = 0; exit_code = $null; durata_ms = 0
            }
            continue
        }

        # Assegnazione PRIMA della prima istruzione del figlio.
        $assegnato = $K::AssignProcessToJobObject($job, $pi.hProcess)
        $assErr = if ($assegnato) { 0 } else { [Runtime.InteropServices.Marshal]::GetLastWin32Error() }

        $inJob = $false
        [void]$K::IsProcessInJob($pi.hProcess, $job, [ref]$inJob)

        $membership = if (-not $assegnato) { 'assign_failed' }
                      elseif ($inJob) { 'in_job' }
                      else { 'not_in_job' }

        $t0 = [Diagnostics.Stopwatch]::StartNew()
        [void]$K::ResumeThread($pi.hThread)

        # Campionamento: il massimo dei membri visti dal kernel mentre gira, e
        # i loro NOMI. Senza i nomi "membri max 4" e' un numero senza premessa
        # (regola O): non direbbe se i discendenti del wrapper sono contenuti
        # davvero o se il conteggio e' gonfiato da conhost.exe.
        $max = 0
        $vivo = $true
        $noti = @{}
        while ($t0.ElapsedMilliseconds -lt 20000) {
            $qerr = 0
            $pids = $K::JobMembers($job, [ref]$qerr)
            if ($pids.Count -gt $max) { $max = $pids.Count }
            foreach ($p in $pids) {
                if (-not $noti.ContainsKey($p)) {
                    $pr = Get-Process -Id $p -ErrorAction SilentlyContinue
                    $noti[$p] = $(if ($pr) { $pr.ProcessName } else { 'gia_uscito' })
                }
            }
            if ($K::WaitForSingleObject($pi.hProcess, 25) -eq 0) { $vivo = $false; break }
        }
        $t0.Stop()
        $nomi = @($noti.Values | Sort-Object -Unique)

        $code = 0
        [void]$K::GetExitCodeProcess($pi.hProcess, [ref]$code)
        if ($vivo) { [void]$K::TerminateProcess($pi.hProcess, 1) }

        # Pulizia: si termina SOLO il job che questo script ha creato, quindi
        # solo processi che questo script ha avviato (regola E).
        [void]$K::TerminateJobObject($job, 1)
        [void]$K::CloseHandle($pi.hThread)
        [void]$K::CloseHandle($pi.hProcess)
        [void]$K::CloseHandle($job)

        $esiti += [pscustomobject]@{
            indice = $i; famiglia = $b.famiglia; forma = $b.forma
            membership = $membership; assign_win32 = $assErr
            assign_win32_nome = (Get-NomeErrore $assErr)
            pid = $pi.dwProcessId; membri_max = $max; membri_nomi = $nomi
            # conhost.exe e' il coinquilino della console nascosta, non un
            # discendente del comando: il contenimento REALE si misura sui
            # membri che non sono ne' conhost ne' il processo di testa.
            discendenti_reali = @($noti.Values | Where-Object { $_ -ne 'conhost' }).Count - 1
            exit_code = $(if ($vivo) { $null } else { [int]$code })
            durata_ms = [int]$t0.ElapsedMilliseconds
        }
        if (-not $Quiet) {
            Write-Host ("  [{0,2}/{1}] {2,-10} {3,-12} -> {4} (membri max {5}: {6})" -f `
                $i, $bersagli.Count, $b.famiglia, $b.forma, $membership, $max, ($nomi -join ','))
        }
    }
    $esiti
}

# ---------------------------------------------------------------------------
# Misura 2 — namespace: Local\ e' per-sessione, Global\ vuole un privilegio
# ---------------------------------------------------------------------------
function Measure-Namespace {
    $sid = Get-Sessione
    $nl = "nexus-jobprobe-loc-{0}" -f ([guid]::NewGuid().ToString('N').Substring(0, 12))
    $ng = "nexus-jobprobe-glo-{0}" -f ([guid]::NewGuid().ToString('N').Substring(0, 12))

    $hLocal = $K::CreateJobObjectW([IntPtr]::Zero, "Local\$nl")
    $errLocal = if ($hLocal -eq [IntPtr]::Zero) { [Runtime.InteropServices.Marshal]::GetLastWin32Error() } else { 0 }

    $hGlobal = $K::CreateJobObjectW([IntPtr]::Zero, "Global\$ng")
    $errGlobal = if ($hGlobal -eq [IntPtr]::Zero) { [Runtime.InteropServices.Marshal]::GetLastWin32Error() } else { 0 }

    $hSect = $K::CreateFileMappingW([IntPtr](-1), [IntPtr]::Zero, 0x04, 0, 4096, "Global\$ng-sect")
    $errSect = if ($hSect -eq [IntPtr]::Zero) { [Runtime.InteropServices.Marshal]::GetLastWin32Error() } else { 0 }
    if ($hSect -ne [IntPtr]::Zero) { [void]$K::CloseHandle($hSect) }

    # Andata e ritorno su OGNI nome creato: aprirlo col proprio prefisso deve
    # riuscire, aprirlo con l'ALTRO deve fallire. Senza il ritorno, un
    # "creazione riuscita" non dimostra in quale namespace l'oggetto sia
    # finito, e la risposta sul privilegio resterebbe un'opinione.
    $risoluzioni = @(
        (Test-AperturaNome "Local\$nl"),
        (Test-AperturaNome "Session\$sid\$nl"),
        (Test-AperturaNome "Session\0\$nl"),
        (Test-AperturaNome "Global\$nl"),
        (Test-AperturaNome "Global\$ng"),
        (Test-AperturaNome "Local\$ng"),
        (Test-AperturaNome "Session\0\$ng")
    )

    if ($hLocal -ne [IntPtr]::Zero) { [void]$K::CloseHandle($hLocal) }
    if ($hGlobal -ne [IntPtr]::Zero) { [void]$K::CloseHandle($hGlobal) }

    [pscustomobject]@{
        sessione = $sid
        nome_local = $nl
        nome_global = $ng
        crea_local = [pscustomobject]@{
            esito = $(if ($errLocal -eq 0) { 'ok' } else { 'fallita' })
            win32 = $errLocal; win32_nome = (Get-NomeErrore $errLocal)
        }
        crea_global = [pscustomobject]@{
            esito = $(if ($errGlobal -eq 0) { 'ok' } else { 'fallita' })
            win32 = $errGlobal; win32_nome = (Get-NomeErrore $errGlobal)
        }
        crea_global_section = [pscustomobject]@{
            esito = $(if ($errSect -eq 0) { 'ok' } else { 'fallita' })
            win32 = $errSect; win32_nome = (Get-NomeErrore $errSect)
        }
        risoluzioni = $risoluzioni
    }
}

# ---------------------------------------------------------------------------
# Misura 3 — sopravvivenza del nome alla morte del launcher
# ---------------------------------------------------------------------------
function Measure-SopravvivenzaVariante {
    param([bool]$HandleEreditato, [bool]$CapoEsce = $false)

    $node = (Get-Command node.exe -ErrorAction SilentlyContinue).Source
    if (-not $node) {
        return [pscustomobject]@{ esito = 'non_misurata'; motivo = 'node.exe assente' }
    }
    $app = $node
    $cl = "`"$node`" -e setTimeout(function(){},20000)"
    if ($CapoEsce) {
        # Il caso reale del detach: cmd.exe avvia e ESCE, il figlio resta. Se
        # l'handle vive solo nel capo, il nome muore col capo e non col
        # servizio. PING dorme senza richiedere caratteri che cmd interpreta.
        $app = "$env:SystemRoot\system32\cmd.exe"
        $cl = "`"$app`" /d /s /c `"start /b ping -n 21 127.0.0.1`""
    }
    $nome = "nexus-jobprobe-surv-{0}" -f ([guid]::NewGuid().ToString('N').Substring(0, 12))
    $job = $K::CreateJobObjectW([IntPtr]::Zero, "Local\$nome")
    if ($job -eq [IntPtr]::Zero) {
        return [pscustomobject]@{ esito = 'non_misurata'; motivo = 'CreateJobObject fallita' }
    }
    if ($HandleEreditato) {
        [void]$K::SetHandleInformation($job, $K::HANDLE_FLAG_INHERIT, $K::HANDLE_FLAG_INHERIT)
    }
    $err = 0
    $pi = $K::SpawnSuspendedEx($app, $cl, $HandleEreditato, [ref]$err)
    if ($pi.hProcess -eq [IntPtr]::Zero) {
        [void]$K::CloseHandle($job)
        return [pscustomobject]@{ esito = 'non_misurata'; motivo = "spawn fallito win32=$err" }
    }
    [void]$K::AssignProcessToJobObject($job, $pi.hProcess)
    [void]$K::ResumeThread($pi.hThread)

    $capoUscito = $false
    if ($CapoEsce) {
        $capoUscito = ($K::WaitForSingleObject($pi.hProcess, 8000) -eq 0)
    }
    Start-Sleep -Milliseconds 600

    # I membri ancora vivi, chiesti al kernel FINCHE' l'handle e' nostro: e'
    # anche l'elenco da ripulire se il nome poi si perde.
    $qerr = 0
    $membriPrima = @($K::JobMembers($job, [ref]$qerr))

    # CONTROLLO: col nostro handle ancora aperto il nome DEVE risolvere. Senza
    # questo, un "non risolto" dopo la chiusura potrebbe voler dire che il nome
    # non era mai esistito, e il test proverebbe se stesso (regola O).
    $prima = Test-AperturaNome "Local\$nome"

    # Il launcher "muore": si chiude il suo handle al job.
    [void]$K::CloseHandle($job)

    $vivi = @($membriPrima | Where-Object { $null -ne (Get-Process -Id $_ -ErrorAction SilentlyContinue) })
    $vivoDopo = $vivi.Count -gt 0
    $dopo = Test-AperturaNome "Local\$nome"

    foreach ($p in $membriPrima) { Stop-Process -Id $p -Force -ErrorAction SilentlyContinue }
    [void]$K::TerminateProcess($pi.hProcess, 1)
    [void]$K::CloseHandle($pi.hThread)
    [void]$K::CloseHandle($pi.hProcess)

    $esito = if ($prima.esito -ne 'resolved') { 'controllo_fallito' }
             elseif (-not $vivoDopo) { 'non_concludente_nessun_membro_vivo' }
             elseif ($dopo.esito -eq 'resolved') { 'nome_sopravvive' }
             else { 'nome_perso' }

    [pscustomobject]@{
        esito = $esito
        handle_ereditato_dal_membro = $HandleEreditato
        capo_uscito_prima_della_misura = $capoUscito
        pid_capo = $pi.dwProcessId
        membri_alla_chiusura = $membriPrima.Count
        membri_vivi_alla_riapertura = $vivi.Count
        prima_della_chiusura = $prima
        dopo_la_chiusura = $dopo
    }
}

# ---------------------------------------------------------------------------
# Misura 3-bis — la forma di PRODUZIONE: un server che ascolta davvero, avviato
# attraverso lo shim .cmd come fa `npm run dev`. Le venti prove misurano le
# FORME di avvio; questa misura la forma su cui l'ADR decide.
# Porta 0 = effimera assegnata dal SO, fuori dal bucket di progetto: la misura
# non tocca nexus_port_allocations ne' incrocia il port_enforcer (regola E).
# ---------------------------------------------------------------------------
function Measure-ServerReale {
    $node = (Get-Command node.exe -ErrorAction SilentlyContinue).Source
    if (-not $node) { return [pscustomobject]@{ esito = 'non_misurata'; motivo = 'node.exe assente' } }
    $js = Join-Path $env:TEMP "nexus-jobprobe-srv-$PID.js"
    "require('http').createServer(function(q,s){s.end('ok')}).listen(0,function(){setTimeout(function(){process.exit(0)},8000)})" |
        Out-File -FilePath $js -Encoding ascii
    $cmd = "$env:SystemRoot\system32\cmd.exe"
    $nome = "nexus-jobprobe-srv-{0}" -f ([guid]::NewGuid().ToString('N').Substring(0, 12))
    $job = $K::CreateJobObjectW([IntPtr]::Zero, "Local\$nome")
    if ($job -eq [IntPtr]::Zero) {
        Remove-Item $js -Force -ErrorAction SilentlyContinue
        return [pscustomobject]@{ esito = 'non_misurata'; motivo = 'CreateJobObject fallita' }
    }
    $err = 0
    $pi = $K::SpawnSuspendedEx($cmd, "`"$cmd`" /d /s /c `"node $js`"", $false, [ref]$err)
    if ($pi.hProcess -eq [IntPtr]::Zero) {
        [void]$K::CloseHandle($job); Remove-Item $js -Force -ErrorAction SilentlyContinue
        return [pscustomobject]@{ esito = 'non_misurata'; motivo = "spawn fallito win32=$err" }
    }
    $assegnato = $K::AssignProcessToJobObject($job, $pi.hProcess)
    [void]$K::ResumeThread($pi.hThread)
    Start-Sleep -Milliseconds 2500

    $qerr = 0
    $membri = @($K::JobMembers($job, [ref]$qerr))
    $nodePid = $null
    foreach ($p in $membri) {
        $pr = Get-Process -Id $p -ErrorAction SilentlyContinue
        if ($pr -and $pr.ProcessName -eq 'node') { $nodePid = $p }
    }
    $porta = $null
    $ascolto = 'non_interrogabile'
    if ($nodePid) {
        try {
            $c = Get-NetTCPConnection -State Listen -OwningProcess $nodePid -ErrorAction Stop |
                 Select-Object -First 1
            if ($c) { $porta = $c.LocalPort; $ascolto = 'in_ascolto' } else { $ascolto = 'nessun_listener' }
        } catch { $ascolto = 'non_interrogabile' }
    }

    [void]$K::TerminateJobObject($job, 1)
    foreach ($p in $membri) { Stop-Process -Id $p -Force -ErrorAction SilentlyContinue }
    [void]$K::CloseHandle($pi.hThread); [void]$K::CloseHandle($pi.hProcess); [void]$K::CloseHandle($job)
    Remove-Item $js -Force -ErrorAction SilentlyContinue

    $esito = if (-not $assegnato) { 'assign_failed' }
             elseif (-not $nodePid) { 'non_concludente_nessun_node_nel_job' }
             elseif ($ascolto -eq 'in_ascolto') { 'server_in_ascolto_dentro_il_job' }
             else { "node_nel_job_ma_$ascolto" }

    [pscustomobject]@{
        esito = $esito
        forma = 'cmd -> node -> listener'
        pid_capo = $pi.dwProcessId
        pid_node_membro = $nodePid
        porta_effimera = $porta
        membri = $membri.Count
    }
}

# ---------------------------------------------------------------------------
# Misura 4 — il contenimento serve anche a TERMINARE: un solo gesto sul job
# deve portare via l'albero, compreso il figlio che il capo ha lasciato dietro.
# ---------------------------------------------------------------------------
function Measure-Terminazione {
    $cmd = "$env:SystemRoot\system32\cmd.exe"
    $nome = "nexus-jobprobe-term-{0}" -f ([guid]::NewGuid().ToString('N').Substring(0, 12))
    $job = $K::CreateJobObjectW([IntPtr]::Zero, "Local\$nome")
    if ($job -eq [IntPtr]::Zero) {
        return [pscustomobject]@{ esito = 'non_misurata'; motivo = 'CreateJobObject fallita' }
    }
    $err = 0
    $pi = $K::SpawnSuspendedEx($cmd, "`"$cmd`" /d /s /c `"start /b ping -n 60 127.0.0.1`"", $false, [ref]$err)
    if ($pi.hProcess -eq [IntPtr]::Zero) {
        [void]$K::CloseHandle($job)
        return [pscustomobject]@{ esito = 'non_misurata'; motivo = "spawn fallito win32=$err" }
    }
    [void]$K::AssignProcessToJobObject($job, $pi.hProcess)
    [void]$K::ResumeThread($pi.hThread)
    [void]$K::WaitForSingleObject($pi.hProcess, 8000)
    Start-Sleep -Milliseconds 600

    $qerr = 0
    $prima = @($K::JobMembers($job, [ref]$qerr))
    $viviPrima = @($prima | Where-Object { $null -ne (Get-Process -Id $_ -ErrorAction SilentlyContinue) })

    [void]$K::TerminateJobObject($job, 1)
    Start-Sleep -Milliseconds 400
    $viviDopo = @($prima | Where-Object { $null -ne (Get-Process -Id $_ -ErrorAction SilentlyContinue) })

    foreach ($p in $prima) { Stop-Process -Id $p -Force -ErrorAction SilentlyContinue }
    [void]$K::CloseHandle($pi.hThread); [void]$K::CloseHandle($pi.hProcess); [void]$K::CloseHandle($job)

    $esito = if ($viviPrima.Count -lt 1) { 'non_concludente_nessun_orfano' }
             elseif ($viviDopo.Count -eq 0) { 'albero_terminato' }
             else { 'sopravvissuti' }

    [pscustomobject]@{
        esito = $esito
        membri_registrati = $prima.Count
        vivi_prima = $viviPrima.Count
        vivi_dopo = $viviDopo.Count
    }
}

function Measure-Sopravvivenza {
    [pscustomobject]@{
        # Le tre forme in cui mcp-core puo' morire lasciando vivo il servizio.
        senza_handle_ereditato = Measure-SopravvivenzaVariante -HandleEreditato $false
        con_handle_ereditato   = Measure-SopravvivenzaVariante -HandleEreditato $true
        con_handle_ereditato_capo_uscito = Measure-SopravvivenzaVariante -HandleEreditato $true -CapoEsce $true
    }
}

# ---------------------------------------------------------------------------
# Misura 4 — cross-sessione (richiede elevazione)
# ---------------------------------------------------------------------------
function Measure-Sessione0 {
    if (-not (Get-Elevazione)) {
        return [pscustomobject]@{
            esito = 'non_eseguita'
            motivo = 'token non elevato: la registrazione di un task come SYSTEM (sessione 0) richiede privilegi amministrativi'
            come_rifarla = 'da PowerShell elevato: powershell -NoProfile -File scripts\job-object-probe.ps1 -Mode session0'
        }
    }
    $node = (Get-Command node.exe -ErrorAction SilentlyContinue).Source
    $nome = "nexus-jobprobe-s0-{0}" -f ([guid]::NewGuid().ToString('N').Substring(0, 12))
    $job = $K::CreateJobObjectW([IntPtr]::Zero, "Local\$nome")
    $pi = $null
    if ($node -and $job -ne [IntPtr]::Zero) {
        $err = 0
        $pi = $K::SpawnSuspended($node, "`"$node`" -e setTimeout(function(){},$($HoldSeconds * 1000))", [ref]$err)
        if ($pi.hProcess -ne [IntPtr]::Zero) {
            [void]$K::AssignProcessToJobObject($job, $pi.hProcess)
            [void]$K::ResumeThread($pi.hThread)
        }
    }

    $out = Join-Path $env:TEMP "nexus-jobprobe-s0-$nome.json"
    $task = "NexusJobProbeSession0-$nome"
    $script = $PSCommandPath
    $esito = $null
    try {
        $arg = "-NoProfile -ExecutionPolicy Bypass -File `"$script`" -Mode observe -JobName `"$nome`" -JsonOut `"$out`" -Quiet"
        $a = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument $arg
        $p = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
        Register-ScheduledTask -TaskName $task -Action $a -Principal $p -Force | Out-Null
        Start-ScheduledTask -TaskName $task
        $n = 0
        while ($n -lt 60 -and -not (Test-Path $out)) { Start-Sleep -Milliseconds 500; $n++ }
        if (Test-Path $out) {
            $esito = Get-Content $out -Raw | ConvertFrom-Json
        }
    } catch {
        $esito = [pscustomobject]@{ errore = $_.Exception.Message }
    } finally {
        try { Unregister-ScheduledTask -TaskName $task -Confirm:$false -ErrorAction SilentlyContinue } catch {}
        if (Test-Path $out) { Remove-Item $out -Force -ErrorAction SilentlyContinue }
        if ($pi -and $pi.hProcess -ne [IntPtr]::Zero) {
            [void]$K::TerminateProcess($pi.hProcess, 1)
            [void]$K::CloseHandle($pi.hThread); [void]$K::CloseHandle($pi.hProcess)
        }
        if ($job -ne [IntPtr]::Zero) { [void]$K::CloseHandle($job) }
    }
    if (-not $esito) {
        return [pscustomobject]@{ esito = 'non_eseguita'; motivo = 'osservatore SYSTEM senza output entro 30s'; nome_job = $nome }
    }
    [pscustomobject]@{ esito = 'eseguita'; nome_job = $nome; osservatore = $esito }
}

# ---------------------------------------------------------------------------
# Autotest di mutazione — uno strumento che non sa dire "no" non misura nulla.
# Un 100% di in_job vale solo se IsProcessInJob risponde FALSO quando deve:
# senza l'assegnazione, e contro un job che non e' il proprio (regola O).
# ---------------------------------------------------------------------------
function Invoke-Autotest {
    $node = (Get-Command node.exe -ErrorAction SilentlyContinue).Source
    if (-not $node) { return [pscustomobject]@{ esito = 'non_eseguito'; motivo = 'node.exe assente' } }
    $cl = "`"$node`" -e setTimeout(function(){},4000)"
    $casi = @()

    $mkJob = { $K::CreateJobObjectW([IntPtr]::Zero, "Local\nexus-jobprobe-self-{0}" -f ([guid]::NewGuid().ToString('N').Substring(0, 10))) }

    # A — nessuna assegnazione: la risposta attesa e' falso.
    $jA = & $mkJob; $e = 0; $pA = $K::SpawnSuspendedEx($node, $cl, $false, [ref]$e)
    $r = $false; [void]$K::IsProcessInJob($pA.hProcess, $jA, [ref]$r)
    $casi += [pscustomobject]@{ caso = 'senza_assegnazione'; atteso = $false; ottenuto = $r }

    # B — assegnato a un job, interrogato contro un ALTRO job.
    $jB1 = & $mkJob; $jB2 = & $mkJob; $pB = $K::SpawnSuspendedEx($node, $cl, $false, [ref]$e)
    [void]$K::AssignProcessToJobObject($jB1, $pB.hProcess)
    $r2 = $false; [void]$K::IsProcessInJob($pB.hProcess, $jB2, [ref]$r2)
    $casi += [pscustomobject]@{ caso = 'job_sbagliato'; atteso = $false; ottenuto = $r2 }

    # C — controllo positivo.
    $r3 = $false; [void]$K::IsProcessInJob($pB.hProcess, $jB1, [ref]$r3)
    $casi += [pscustomobject]@{ caso = 'job_proprio'; atteso = $true; ottenuto = $r3 }

    foreach ($h in @($pA.hProcess, $pB.hProcess)) { [void]$K::TerminateProcess($h, 1); [void]$K::CloseHandle($h) }
    [void]$K::CloseHandle($pA.hThread); [void]$K::CloseHandle($pB.hThread)
    foreach ($h in @($jA, $jB1, $jB2)) { [void]$K::CloseHandle($h) }

    $ok = @($casi | Where-Object { $_.atteso -ne $_.ottenuto }).Count -eq 0
    [pscustomobject]@{ esito = $(if ($ok) { 'strumento_sa_dire_no' } else { 'strumento_non_falsificabile' }); casi = $casi }
}

# ---------------------------------------------------------------------------
# Modo osservatore: nessuna creazione, solo risoluzione di nomi.
# ---------------------------------------------------------------------------
function Invoke-Osservatore {
    param([string]$Nome)
    $sid = Get-Sessione
    $creaGlobal = $K::CreateJobObjectW([IntPtr]::Zero, "Global\nexus-jobprobe-obs-$PID")
    $errG = if ($creaGlobal -eq [IntPtr]::Zero) { [Runtime.InteropServices.Marshal]::GetLastWin32Error() } else { 0 }
    if ($creaGlobal -ne [IntPtr]::Zero) { [void]$K::CloseHandle($creaGlobal) }
    [pscustomobject]@{
        sessione = $sid
        utente = [Security.Principal.WindowsIdentity]::GetCurrent().Name
        crea_global = [pscustomobject]@{
            esito = $(if ($errG -eq 0) { 'ok' } else { 'fallita' })
            win32 = $errG; win32_nome = (Get-NomeErrore $errG)
        }
        risoluzioni = @(
            (Test-AperturaNome "Local\$Nome"),
            (Test-AperturaNome "Session\$sid\$Nome"),
            (Test-AperturaNome "Session\1\$Nome"),
            (Test-AperturaNome "Global\$Nome")
        )
    }
}

# ---------------------------------------------------------------------------
# Ingresso
# ---------------------------------------------------------------------------
$giaInJob = $false
[void]$K::IsProcessInJob($K::GetCurrentProcess(), [IntPtr]::Zero, [ref]$giaInJob)

$premessa = [pscustomobject]@{
    # Regola O: un numero senza la sua premessa e' un'opinione.
    data = (Get-Date).ToString('o')
    host_os = [Environment]::OSVersion.Version.ToString()
    sessione = Get-Sessione
    utente = [Security.Principal.WindowsIdentity]::GetCurrent().Name
    elevato = Get-Elevazione
    launcher_gia_in_un_job = $giaInJob
    powershell = $PSVersionTable.PSVersion.ToString()
}

switch ($Mode) {
    'observe' {
        if (-not $JobName) { throw '-Mode observe richiede -JobName' }
        $ris = [pscustomobject]@{ premessa = $premessa; osservazione = (Invoke-Osservatore -Nome $JobName) }
    }
    'session0' {
        $ris = [pscustomobject]@{ premessa = $premessa; cross_sessione = (Measure-Sessione0) }
    }
    default {
        if (-not $Quiet) { Write-Host 'ADR 0042 P0(a) — contenimento su venti avvii reali' }
        $autotest = Invoke-Autotest
        $cont = Measure-Contenimento
        $tot = $cont.Count
        $inJob = @($cont | Where-Object { $_.membership -eq 'in_job' }).Count
        $conDiscendenti = @($cont | Where-Object { $_.membri_max -gt 1 }).Count
        $perFamiglia = $cont | Group-Object famiglia | ForEach-Object {
            [pscustomobject]@{
                famiglia = $_.Name
                campioni = $_.Count
                in_job = @($_.Group | Where-Object { $_.membership -eq 'in_job' }).Count
                membri_max = ($_.Group | Measure-Object membri_max -Maximum).Maximum
            }
        }
        $ris = [pscustomobject]@{
            premessa = $premessa
            autotest = $autotest
            contenimento = [pscustomobject]@{
                campioni = $tot
                in_job = $inJob
                percentuale_in_job = $(if ($tot -gt 0) { [math]::Round(100.0 * $inJob / $tot, 1) } else { 0 })
                avvii_con_discendenti_nel_job = $conDiscendenti
                per_famiglia = $perFamiglia
                dettaglio = $cont
            }
            server_reale = Measure-ServerReale
            namespace = Measure-Namespace
            sopravvivenza = Measure-Sopravvivenza
            terminazione = Measure-Terminazione
            cross_sessione = Measure-Sessione0
        }
    }
}

$json = $ris | ConvertTo-Json -Depth 8
if ($JsonOut) { $json | Out-File -FilePath $JsonOut -Encoding utf8 } else { $json }

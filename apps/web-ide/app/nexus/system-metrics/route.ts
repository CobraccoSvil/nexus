import { NextResponse } from "next/server";
import os from "os";
import { exec, execSync } from "child_process";
import { readFileSync } from "fs";
import { promisify } from "util";

const execAsync = promisify(exec);
const IS_WIN = process.platform === "win32";

interface DiskInfo {
  device: string;
  mountPoint: string;
  totalMB: number;
  usedMB: number;
  availMB: number;
  usedPercent: number;
}

interface ProcInfo {
  pid: number;
  user: string;
  cpu: number;
  mem: number;
  command: string;
}

interface NetRate {
  rxBytesPerSec: number;
  txBytesPerSec: number;
}

// State modulo per calcoli delta (CPU e rete richiedono due misurazioni)
let _prevNet: { ts: number; rx: number; tx: number } | null = null;
let _prevCpu: { idle: number; total: number } | null = null;

// CPU usage CROSS-PLATFORM via os.cpus(): somma idle/total su tutti i core e
// calcola la percentuale dal delta fra due misurazioni. os.cpus() funziona sia
// su Linux sia su Windows (a differenza di /proc/stat, che su Windows non esiste
// -> CPU sempre 0), quindi un solo path (regola L).
function getCpuUsage(): number {
  try {
    let idle = 0;
    let total = 0;
    for (const cpu of os.cpus()) {
      for (const v of Object.values(cpu.times)) total += v;
      idle += cpu.times.idle;
    }
    if (!_prevCpu) {
      _prevCpu = { idle, total };
      return 0;
    }
    const deltaIdle = idle - _prevCpu.idle;
    const deltaTotal = total - _prevCpu.total;
    _prevCpu = { idle, total };
    if (deltaTotal <= 0) return 0;
    return Math.max(0, Math.min(100, Math.round((1 - deltaIdle / deltaTotal) * 100)));
  } catch {
    return 0;
  }
}

function networkPerSec(rx: number, tx: number): NetRate {
  const now = Date.now();
  if (!_prevNet) {
    _prevNet = { ts: now, rx, tx };
    return { rxBytesPerSec: 0, txBytesPerSec: 0 };
  }
  const dt = (now - _prevNet.ts) / 1000;
  const rxPerSec = dt > 0 ? Math.round((rx - _prevNet.rx) / dt) : 0;
  const txPerSec = dt > 0 ? Math.round((tx - _prevNet.tx) / dt) : 0;
  _prevNet = { ts: now, rx, tx };
  return { rxBytesPerSec: Math.max(0, rxPerSec), txBytesPerSec: Math.max(0, txPerSec) };
}

// ── Linux (lettura /proc + df + ps) ──────────────────────────────────────────

function getNetworkDeltaLinux(): NetRate {
  try {
    const content = readFileSync("/proc/net/dev", "utf8");
    let rx = 0, tx = 0;
    for (const line of content.split("\n")) {
      const trimmed = line.trim();
      if (!trimmed || trimmed.startsWith("Inter") || trimmed.startsWith("face")) continue;
      const colonIdx = trimmed.indexOf(":");
      if (colonIdx === -1) continue;
      const iface = trimmed.slice(0, colonIdx).trim();
      if (iface === "lo") continue;
      const parts = trimmed.slice(colonIdx + 1).trim().split(/\s+/).map(Number);
      rx += parts[0] ?? 0;
      tx += parts[8] ?? 0;
    }
    return networkPerSec(rx, tx);
  } catch {
    return { rxBytesPerSec: 0, txBytesPerSec: 0 };
  }
}

function getDiskInfoLinux(): DiskInfo[] {
  try {
    const out = execSync("df -BM -x tmpfs -x devtmpfs -x squashfs 2>/dev/null", {
      timeout: 3000,
    }).toString();
    return out
      .trim()
      .split("\n")
      .slice(1)
      .map((line): DiskInfo => {
        const p = line.trim().split(/\s+/);
        return {
          device: p[0] ?? "",
          mountPoint: p[5] ?? "/",
          totalMB: parseInt(p[1]) || 0,
          usedMB: parseInt(p[2]) || 0,
          availMB: parseInt(p[3]) || 0,
          usedPercent: parseInt(p[4]) || 0,
        };
      })
      .filter((d) => d.totalMB > 100);
  } catch {
    return [];
  }
}

function getTopProcessesLinux(): ProcInfo[] {
  try {
    const out = execSync("ps aux --sort=-%cpu --no-header 2>/dev/null", {
      timeout: 3000,
    }).toString();
    return out
      .trim()
      .split("\n")
      .slice(0, 15)
      .map((line): ProcInfo => {
        const p = line.trim().split(/\s+/);
        const cmd = p.slice(10).join(" ").slice(0, 45);
        return {
          pid: parseInt(p[1]) || 0,
          user: (p[0] ?? "").slice(0, 12),
          cpu: parseFloat(p[2]) || 0,
          mem: parseFloat(p[3]) || 0,
          command: cmd,
        };
      });
  } catch {
    return [];
  }
}

// ── Windows (una sola invocazione PowerShell per disk + processi + rete) ──────
// Get-Volume / Get-Process / Get-NetAdapterStatistics non richiedono privilegi.
// Una sola chiamata per non spawnare 3 processi a ogni poll.
interface WinVolume {
  DriveLetter?: number | string;
  Size?: number;
  SizeRemaining?: number;
}
interface WinProc {
  Id?: number;
  ProcessName?: string;
  CPU?: number;
  WorkingSet64?: number;
}
interface WinMetricsRaw {
  disks?: WinVolume | WinVolume[];
  procs?: WinProc | WinProc[];
  rx?: number;
  tx?: number;
}

const WIN_METRICS_PS =
  "$ErrorActionPreference='SilentlyContinue';" +
  "$d=Get-Volume | Where-Object {$_.DriveLetter} | Select-Object DriveLetter,Size,SizeRemaining;" +
  "$p=Get-Process | Sort-Object CPU -Descending | Select-Object -First 15 Id,ProcessName,CPU,WorkingSet64;" +
  "$n=Get-NetAdapterStatistics;" +
  "$rx=($n|Measure-Object ReceivedBytes -Sum).Sum;" +
  "$tx=($n|Measure-Object SentBytes -Sum).Sum;" +
  "@{disks=@($d);procs=@($p);rx=[double]$rx;tx=[double]$tx} | ConvertTo-Json -Depth 4 -Compress";

function asArray<T>(v: T | T[] | undefined): T[] {
  return Array.isArray(v) ? v : v == null ? [] : [v];
}

interface WinMetrics {
  disks: DiskInfo[];
  processes: ProcInfo[];
  network: NetRate;
}

const EMPTY_WIN: WinMetrics = {
  disks: [],
  processes: [],
  network: { rxBytesPerSec: 0, txBytesPerSec: 0 },
};
let _winCache: { ts: number; data: WinMetrics } | null = null;
let _winInflight: Promise<WinMetrics> | null = null;
const WIN_CACHE_MS = 2500;

// Wrapper cache + single-flight: il pannello polla ogni ~2s ma PowerShell impiega
// ~2-5s ad avviarsi. Con `exec` async non blocchiamo l'event loop, ma senza
// guard i poll ravvicinati avvierebbero chiamate CONCORRENTI sovrapposte (pile-up
// di processi powershell mentre il Monitor e' aperto). La cache serve i poll
// entro WIN_CACHE_MS; il single-flight fa condividere ai poll concorrenti la
// stessa Promise in volo. Il delta di rete resta corretto (calcolato solo sul
// campione reale, cioe' quando gatherWindowsMetrics gira davvero).
function getWindowsMetrics(): Promise<WinMetrics> {
  if (_winCache && Date.now() - _winCache.ts < WIN_CACHE_MS) {
    return Promise.resolve(_winCache.data);
  }
  if (_winInflight) return _winInflight;
  _winInflight = gatherWindowsMetrics().finally(() => {
    _winInflight = null;
  });
  return _winInflight;
}

async function gatherWindowsMetrics(): Promise<WinMetrics> {
  try {
    const { stdout } = await execAsync(
      `powershell -NoProfile -NonInteractive -Command "${WIN_METRICS_PS}"`,
      { timeout: 6000, maxBuffer: 4 * 1024 * 1024 }
    );
    const out = stdout.trim();
    if (!out) return EMPTY_WIN;
    const data = JSON.parse(out) as WinMetricsRaw;
    const totalMem = os.totalmem();

    const disks: DiskInfo[] = asArray(data.disks)
      .map((v): DiskInfo => {
        const totalMB = Math.round((v.Size ?? 0) / 1024 / 1024);
        const availMB = Math.round((v.SizeRemaining ?? 0) / 1024 / 1024);
        const usedMB = totalMB - availMB;
        const letter =
          typeof v.DriveLetter === "number"
            ? String.fromCharCode(v.DriveLetter)
            : String(v.DriveLetter ?? "?");
        return {
          device: `${letter}:`,
          mountPoint: `${letter}:`,
          totalMB,
          usedMB,
          availMB,
          usedPercent: totalMB > 0 ? Math.round((usedMB / totalMB) * 100) : 0,
        };
      })
      .filter((d) => d.totalMB > 100);

    // NB: su Windows Get-Process.CPU e' il tempo CPU CUMULATO in secondi, non una
    // percentuale istantanea; `mem` resta una percentuale (WorkingSet / RAM).
    const processes: ProcInfo[] = asArray(data.procs).map((p): ProcInfo => ({
      pid: Number(p.Id) || 0,
      user: "",
      cpu: Math.round((Number(p.CPU) || 0) * 10) / 10,
      mem: totalMem > 0 ? Math.round(((Number(p.WorkingSet64) || 0) / totalMem) * 1000) / 10 : 0,
      command: String(p.ProcessName ?? "").slice(0, 45),
    }));

    const network = networkPerSec(Number(data.rx) || 0, Number(data.tx) || 0);
    const result: WinMetrics = { disks, processes, network };
    _winCache = { ts: Date.now(), data: result };
    return result;
  } catch {
    return EMPTY_WIN;
  }
}

export async function GET() {
  const cpuUsage = getCpuUsage();
  // os.loadavg() non e' significativo su Windows (ritorna sempre [0,0,0]).
  const loadAvg = os.loadavg();
  const totalMB = Math.round(os.totalmem() / 1024 / 1024);
  const freeMB = Math.round(os.freemem() / 1024 / 1024);
  const usedMB = totalMB - freeMB;

  let disks: DiskInfo[];
  let processes: ProcInfo[];
  let network: NetRate;
  if (IS_WIN) {
    const w = await getWindowsMetrics();
    disks = w.disks;
    processes = w.processes;
    network = w.network;
  } else {
    disks = getDiskInfoLinux();
    processes = getTopProcessesLinux();
    network = getNetworkDeltaLinux();
  }

  return NextResponse.json({
    cpu: {
      usagePercent: cpuUsage,
      loadAvg1: Math.round(loadAvg[0] * 100) / 100,
      loadAvg5: Math.round(loadAvg[1] * 100) / 100,
      loadAvg15: Math.round(loadAvg[2] * 100) / 100,
      coreCount: os.cpus().length,
      model: os.cpus()[0]?.model?.replace(/\s+/g, " ").trim() ?? "Unknown",
    },
    memory: {
      totalMB,
      usedMB,
      freeMB,
      usedPercent: Math.round((usedMB / totalMB) * 100),
    },
    disks,
    network,
    processes,
    uptime: Math.round(os.uptime()),
    timestamp: Date.now(),
  });
}

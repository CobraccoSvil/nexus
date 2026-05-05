import { NextResponse } from "next/server";
import os from "os";
import { execSync } from "child_process";
import { readFileSync } from "fs";

// State modulo per calcoli delta (CPU e rete richiedono due misurazioni)
let _prevNet: { ts: number; rx: number; tx: number } | null = null;
let _prevCpu: { idle: number; total: number } | null = null;

function getCpuUsage(): number {
  try {
    const stat = readFileSync("/proc/stat", "utf8");
    const line = stat.split("\n")[0];
    const parts = line.trim().split(/\s+/).slice(1).map(Number);
    // user, nice, system, idle, iowait, irq, softirq, steal, guest, guest_nice
    const idle = (parts[3] ?? 0) + (parts[4] ?? 0); // idle + iowait
    const total = parts.reduce((a, b) => a + b, 0);

    if (!_prevCpu) {
      _prevCpu = { idle, total };
      return 0;
    }

    const deltaIdle = idle - _prevCpu.idle;
    const deltaTotal = total - _prevCpu.total;
    _prevCpu = { idle, total };

    if (deltaTotal <= 0) return 0;
    return Math.max(0, Math.round((1 - deltaIdle / deltaTotal) * 100));
  } catch {
    return 0;
  }
}

function getNetworkDelta(): { rxBytesPerSec: number; txBytesPerSec: number } {
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

    const now = Date.now();
    if (!_prevNet) {
      _prevNet = { ts: now, rx, tx };
      return { rxBytesPerSec: 0, txBytesPerSec: 0 };
    }

    const dt = (now - _prevNet.ts) / 1000;
    const rxPerSec = dt > 0 ? Math.round((rx - _prevNet.rx) / dt) : 0;
    const txPerSec = dt > 0 ? Math.round((tx - _prevNet.tx) / dt) : 0;
    _prevNet = { ts: now, rx, tx };

    return {
      rxBytesPerSec: Math.max(0, rxPerSec),
      txBytesPerSec: Math.max(0, txPerSec),
    };
  } catch {
    return { rxBytesPerSec: 0, txBytesPerSec: 0 };
  }
}

function getDiskInfo() {
  try {
    const out = execSync("df -BM -x tmpfs -x devtmpfs -x squashfs 2>/dev/null", {
      timeout: 3000,
    }).toString();
    return out
      .trim()
      .split("\n")
      .slice(1)
      .map((line) => {
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
      .filter((d) => d.totalMB > 100); // ignora partizioni troppo piccole
  } catch {
    return [];
  }
}

function getTopProcesses() {
  try {
    const out = execSync("ps aux --sort=-%cpu --no-header 2>/dev/null", {
      timeout: 3000,
    }).toString();
    return out
      .trim()
      .split("\n")
      .slice(0, 15)
      .map((line) => {
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

export async function GET() {
  const cpuUsage = getCpuUsage();
  const loadAvg = os.loadavg();
  const totalMB = Math.round(os.totalmem() / 1024 / 1024);
  const freeMB = Math.round(os.freemem() / 1024 / 1024);
  const usedMB = totalMB - freeMB;
  const network = getNetworkDelta();
  const disks = getDiskInfo();
  const processes = getTopProcesses();

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

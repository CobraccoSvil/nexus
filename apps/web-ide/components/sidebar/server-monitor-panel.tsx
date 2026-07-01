"use client";

import { useEffect, useState } from "react";
import { useThemeColors } from "../../lib/theme";

interface SystemMetrics {
  cpu: {
    usagePercent: number;
    loadAvg1: number;
    loadAvg5: number;
    loadAvg15: number;
    coreCount: number;
    model: string;
  };
  memory: { totalMB: number; usedMB: number; freeMB: number; usedPercent: number };
  disks: Array<{ device: string; mountPoint: string; totalMB: number; usedMB: number; availMB: number; usedPercent: number }>;
  network: { rxBytesPerSec: number; txBytesPerSec: number };
  processes: Array<{ pid: number; user: string; cpu: number; mem: number; command: string }>;
  uptime: number;
  timestamp: number;
}

function fmtBytes(n: number): string {
  if (n >= 1_073_741_824) return (n / 1_073_741_824).toFixed(1) + " GB/s";
  if (n >= 1_048_576) return (n / 1_048_576).toFixed(1) + " MB/s";
  if (n >= 1024) return (n / 1024).toFixed(0) + " KB/s";
  return n + " B/s";
}

function fmtMB(mb: number): string {
  if (mb >= 1024) return (mb / 1024).toFixed(1) + " GB";
  return mb + " MB";
}

function fmtUptime(s: number): string {
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

function Bar({ pct }: { pct: number }) {
  const color = pct > 95 ? "#ef4444" : pct > 80 ? "#f59e0b" : "#22c55e";
  return (
    <div style={{ height: 5, background: "rgba(128,128,128,0.2)", borderRadius: 3, overflow: "hidden", margin: "3px 0" }}>
      <div style={{ width: `${Math.min(100, pct)}%`, height: "100%", background: color, borderRadius: 3, transition: "width 0.4s" }} />
    </div>
  );
}

export function ServerMonitorPanel() {
  const tc = useThemeColors();
  const [metrics, setMetrics] = useState<SystemMetrics | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [lastUpdate, setLastUpdate] = useState<number>(0);

  useEffect(() => {
    let active = true;

    const poll = async () => {
      try {
        const res = await fetch("/nexus/system-metrics");
        if (!res.ok) throw new Error(`${res.status}`);
        const data = await res.json() as SystemMetrics;
        if (active) {
          setMetrics(data);
          setLastUpdate(Date.now());
          setError(null);
        }
      } catch (e) {
        if (active) setError(String(e));
      }
    };

    poll();
    const id = setInterval(poll, 2000);
    return () => { active = false; clearInterval(id); };
  }, []);

  const label: React.CSSProperties = {
    fontSize: 10,
    fontWeight: 600,
    color: tc.textMuted,
    textTransform: "uppercase",
    letterSpacing: 0.5,
    marginTop: 10,
    marginBottom: 2,
  };

  const row: React.CSSProperties = {
    display: "flex",
    justifyContent: "space-between",
    alignItems: "center",
    fontSize: 11,
    color: tc.textSecondary,
  };

  const val: React.CSSProperties = { fontVariantNumeric: "tabular-nums", color: tc.text };

  if (error) {
    return (
      <div style={{ padding: "12px 10px", fontSize: 11, color: tc.error }}>
        Errore: {error}
        <br />
        <span className="text-muted">In attesa di /nexus/system-metrics…</span>
      </div>
    );
  }

  if (!metrics) {
    return <div style={{ padding: "12px 10px", fontSize: 11, color: tc.textMuted }}>Caricamento…</div>;
  }

  const { cpu, memory, disks, network, processes, uptime } = metrics;
  const secAgo = Math.round((Date.now() - lastUpdate) / 1000);

  return (
    <div style={{ padding: "6px 10px 16px", fontSize: 12, color: tc.text }}>
      {/* Header aggiornamento */}
      <div style={{ fontSize: 10, color: tc.textMuted, textAlign: "right", marginBottom: 4 }}>
        uptime {fmtUptime(uptime)} · {secAgo}s fa
      </div>

      {/* CPU */}
      <div style={label}>CPU</div>
      <div style={row}>
        <span>Utilizzo</span>
        <span style={val}>{cpu.usagePercent}%</span>
      </div>
      <Bar pct={cpu.usagePercent} />
      <div style={{ ...row, marginTop: 2 }}>
        <span style={{ fontSize: 10, color: tc.textMuted }}>
          load {cpu.loadAvg1} / {cpu.loadAvg5} · {cpu.coreCount} core
        </span>
      </div>

      {/* RAM */}
      <div style={label}>RAM</div>
      <div style={row}>
        <span>Utilizzo</span>
        <span style={val}>{memory.usedPercent}%</span>
      </div>
      <Bar pct={memory.usedPercent} />
      <div style={{ ...row, marginTop: 2 }}>
        <span style={{ fontSize: 10, color: tc.textMuted }}>
          {fmtMB(memory.usedMB)} / {fmtMB(memory.totalMB)}
        </span>
      </div>

      {/* Rete */}
      <div style={label}>Rete</div>
      <div style={row}>
        <span>↓ rx</span>
        <span style={val}>{network.rxBytesPerSec === 0 ? "—" : fmtBytes(network.rxBytesPerSec)}</span>
      </div>
      <div style={row}>
        <span>↑ tx</span>
        <span style={val}>{network.txBytesPerSec === 0 ? "—" : fmtBytes(network.txBytesPerSec)}</span>
      </div>

      {/* Disco */}
      <div style={label}>Disco</div>
      {disks.slice(0, 4).map((d) => (
        <div key={d.mountPoint} style={{ marginBottom: 4 }}>
          <div style={row}>
            <span style={{ fontFamily: "var(--font-mono)", fontSize: 10 }}>{d.mountPoint}</span>
            <span style={val}>{d.usedPercent}%</span>
          </div>
          <Bar pct={d.usedPercent} />
          <div style={{ fontSize: 10, color: tc.textMuted }}>
            {fmtMB(d.usedMB)} / {fmtMB(d.totalMB)}
          </div>
        </div>
      ))}

      {/* Processi */}
      <div style={label}>Processi top CPU</div>
      <div style={{
        fontFamily: "var(--font-mono)",
        fontSize: 10,
        background: tc.bgInput,
        borderRadius: 4,
        padding: "4px 6px",
        overflowX: "auto",
      }}>
        <div style={{ display: "grid", gridTemplateColumns: "1fr 36px 30px", color: tc.textMuted, marginBottom: 2, gap: 4 }}>
          <span>COMANDO</span><span style={{ textAlign: "right" }}>CPU%</span><span style={{ textAlign: "right" }}>MEM%</span>
        </div>
        {processes.slice(0, 12).map((p) => (
          <div key={p.pid} style={{ display: "grid", gridTemplateColumns: "1fr 36px 30px", gap: 4, lineHeight: 1.7 }}>
            <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", color: tc.textSecondary }}
              title={`PID ${p.pid} · ${p.user} · ${p.command}`}>
              {p.command || `[${p.pid}]`}
            </span>
            <span style={{ textAlign: "right", color: p.cpu > 10 ? "#f97316" : tc.text }}>{p.cpu.toFixed(1)}</span>
            <span style={{ textAlign: "right", color: tc.textSecondary }}>{p.mem.toFixed(1)}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

"use client";

import { useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { useEventOfKind } from "../../lib/project-dispatcher/use-events";

interface ServiceMetricsStripProps {
  projectId: string;
}

interface UnitMetrics {
  cpuPct: number;
  rssBytes: number;
  ioReadBytes: number;
  ioWriteBytes: number;
  pid?: number;
  updatedAt: number;
}

interface UnitAlert {
  kind: "anomaly" | "crash";
  label: string;
  severity: string;
  at: number;
}

/**
 * Striscia di osservabilita' runtime dei servizi utente (capacita' 4 + 3 + 1).
 * Consuma gli eventi `ServiceMetrics` / `ServiceAnomaly` / `ServiceCrashDetected`
 * emessi dal worker `service_observer` (backend) via event-stream, senza toccare
 * il reducer dello store (sottoscrizione locale tipizzata, regola L: riusa
 * useEventOfKind). Si auto-nasconde se non arrivano metriche.
 */
export function ServiceMetricsStrip({ projectId: _projectId }: ServiceMetricsStripProps) {
  const tc = useThemeColors();
  const [metrics, setMetrics] = useState<Record<string, UnitMetrics>>({});
  const [alerts, setAlerts] = useState<Record<string, UnitAlert>>({});

  useEventOfKind(
    "ServiceMetrics",
    (env) => {
      const p = env.payload;
      setMetrics((prev) => ({
        ...prev,
        [p.unit]: {
          cpuPct: p.cpu_pct,
          rssBytes: p.rss_bytes,
          ioReadBytes: p.io_read_bytes,
          ioWriteBytes: p.io_write_bytes,
          pid: p.pid,
          updatedAt: Date.now(),
        },
      }));
    },
    [],
  );

  useEventOfKind(
    "ServiceAnomaly",
    (env) => {
      const p = env.payload;
      setAlerts((prev) => ({
        ...prev,
        [p.unit]: {
          kind: "anomaly",
          label: `${p.metric} ${p.value.toFixed(0)} > ${p.threshold.toFixed(0)}`,
          severity: p.severity,
          at: Date.now(),
        },
      }));
    },
    [],
  );

  useEventOfKind(
    "ServiceCrashDetected",
    (env) => {
      const p = env.payload;
      setAlerts((prev) => ({
        ...prev,
        [p.unit]: { kind: "crash", label: p.error_kind, severity: "critical", at: Date.now() },
      }));
    },
    [],
  );

  const units = Object.keys(metrics).sort();
  if (units.length === 0) return null;

  const fmtMb = (b: number) => `${(b / 1_048_576).toFixed(0)} MB`;
  const shortName = (unit: string) => unit.replace(/\.service$/, "");

  return (
    <div
      style={{
        display: "flex",
        flexWrap: "wrap",
        gap: 8,
        padding: "6px 8px",
        borderBottom: `1px solid ${tc.border}`,
        background: tc.bgSidebar,
        fontSize: 11,
      }}
    >
      <span style={{ color: tc.textMuted, alignSelf: "center", fontWeight: 600 }}>
        Osservabilita runtime:
      </span>
      {units.map((unit) => {
        const m = metrics[unit];
        const alert = alerts[unit];
        const cpuHot = m.cpuPct >= 80;
        return (
          <div
            key={unit}
            style={{
              display: "flex",
              alignItems: "center",
              gap: 6,
              padding: "2px 8px",
              borderRadius: 4,
              border: `1px solid ${alert ? "#d9534f" : tc.border}`,
              background: tc.bgCard,
            }}
            title={`PID ${m.pid ?? "?"} - IO r/w ${fmtMb(m.ioReadBytes)}/${fmtMb(m.ioWriteBytes)}`}
          >
            <span style={{ fontWeight: 600, color: tc.text }}>{shortName(unit)}</span>
            <span style={{ color: cpuHot ? "#d9534f" : tc.textMuted }}>
              CPU {m.cpuPct.toFixed(0)}%
            </span>
            <span style={{ color: tc.textMuted }}>RAM {fmtMb(m.rssBytes)}</span>
            {alert && (
              <span
                style={{
                  color: "#fff",
                  background: alert.severity === "critical" ? "#d9534f" : "#f0ad4e",
                  borderRadius: 3,
                  padding: "0 5px",
                  fontSize: 10,
                }}
              >
                {alert.kind === "crash" ? `crash: ${alert.label}` : alert.label}
              </span>
            )}
          </div>
        );
      })}
    </div>
  );
}

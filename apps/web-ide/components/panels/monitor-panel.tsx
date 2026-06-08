"use client";

import { useThemeColors } from "../../lib/theme";
import {
  selectFlags,
  selectMonitors,
  useProjectStore,
} from "../../lib/project-dispatcher";

export interface MonitorPanelProps {
  onSendToChat?: (message: string) => void;
}

export function MonitorPanel(_props: MonitorPanelProps) {
  const tc = useThemeColors();
  const monitors = useProjectStore(selectMonitors);
  const flags = useProjectStore(selectFlags);
  const monitorEntries = Object.entries(monitors);
  const flagEntries = Object.entries(flags);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12, padding: 12, overflow: "auto", height: "100%" }}>
      <section>
        <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 6, textTransform: "uppercase", letterSpacing: 0.5 }}>
          Monitor ({monitorEntries.length})
        </div>
        {monitorEntries.length === 0 ? (
          <div style={{ color: tc.textMuted, fontSize: 12 }}>
            Nessun monitor attivo. L'agente puo' crearne usando il tool <code>dispatcher_update_monitor</code>.
          </div>
        ) : (
          <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(220px, 1fr))", gap: 8 }}>
            {monitorEntries.map(([id, m]) => (
              <div key={id} style={{
                border: `1px solid ${tc.border}`,
                borderRadius: 6,
                background: tc.bgCard,
                padding: "10px 12px",
                minWidth: 0,
                overflow: "hidden",
              }}>
                <div style={{ fontSize: 11, color: tc.textMuted, overflowWrap: "anywhere" }}>{m.label ?? id}</div>
                <div style={{ fontSize: 20, fontWeight: 600, color: tc.text, marginTop: 4, overflowWrap: "anywhere" }}>
                  {formatMonitorValue(m.value)}
                </div>
                {m.updated_at && (
                  <div style={{ fontSize: 10, color: tc.textMuted, marginTop: 4 }}>
                    aggiornato {new Date(m.updated_at).toLocaleTimeString()}
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </section>

      <section>
        <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 6, textTransform: "uppercase", letterSpacing: 0.5 }}>
          Flag ({flagEntries.length})
        </div>
        {flagEntries.length === 0 ? (
          <div style={{ color: tc.textMuted, fontSize: 12 }}>
            Nessun flag impostato.
          </div>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
            {flagEntries.map(([key, value]) => (
              <div key={key} style={{
                border: `1px solid ${tc.border}`,
                borderRadius: 4,
                padding: "6px 10px",
                background: tc.bgCard,
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
                gap: 8,
              }}>
                <code style={{ fontSize: 12, color: tc.text }}>{key}</code>
                <span style={{ fontSize: 12, color: tc.accent }}>{formatMonitorValue(value)}</span>
              </div>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

function formatMonitorValue(v: unknown): string {
  if (v === null || v === undefined) return "—";
  if (typeof v === "object") return JSON.stringify(v);
  return String(v);
}

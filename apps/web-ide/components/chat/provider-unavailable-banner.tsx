"use client";

import type { AgentStep } from "../../lib/api-client";

/**
 * Banner "Nessun provider AI disponibile": appare quando il routing Rust ha
 * rilevato che tutti i provider configurati sono in cooldown (quota/credito
 * esaurito). Lo step e' emesso da `chat_messages.rs::spawn_agent_run` con
 * status `provider_unavailable`. La UI deve fermarsi e dare istruzioni — NON
 * deve far ripartire il run da sola. L'utente clicca "Configurazione provider"
 * per andare all'admin oppure aspetta il reset cooldown.
 */
export function ProviderUnavailableBanner({
  step,
  providersInCooldown,
  tc,
}: {
  step: AgentStep;
  providersInCooldown: string[];
  tc: Record<string, string>;
}) {
  return (
    <div style={{
      background: "rgba(239,68,68,0.10)",
      border: "1px solid rgba(239,68,68,0.50)",
      borderLeft: "4px solid #ef4444",
      borderRadius: 6,
      padding: "10px 14px",
      margin: "0 0 10px 0",
      fontSize: 12,
      color: tc.text,
      display: "flex",
      flexDirection: "column",
      gap: 6,
    }}>
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <span style={{ fontSize: 16 }}>⚠</span>
        <span style={{ fontWeight: 700, color: "#ef4444" }}>
          Nessun provider AI disponibile
        </span>
      </div>
      <div style={{ lineHeight: 1.5 }}>
        {step.toolResult ?? "Tutti i provider configurati sono in cooldown."}
      </div>
      {providersInCooldown.length > 0 && (
        <div style={{ fontSize: 10, color: tc.textMuted }}>
          In cooldown: {providersInCooldown.join(", ")}
        </div>
      )}
      <div style={{ display: "flex", gap: 6, marginTop: 4 }}>
        <button
          type="button"
          onClick={() => {
            if (typeof window !== "undefined") {
              window.open("/admin/settings/providers", "_blank", "noopener");
            }
          }}
          style={{
            background: "rgba(239,68,68,0.18)",
            border: "1px solid rgba(239,68,68,0.55)",
            borderRadius: 4,
            color: "#ef4444",
            cursor: "pointer",
            padding: "3px 10px",
            fontSize: 11,
            fontWeight: 600,
          }}
        >
          Configurazione provider
        </button>
      </div>
    </div>
  );
}

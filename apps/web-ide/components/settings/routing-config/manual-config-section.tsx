"use client";

import { useThemeColors } from "../../../lib/theme";
import {
  PROVIDER_MODELS,
  PROVIDERS,
  ROUTING_INTENTS,
  buttonStyle,
  inputStyle,
  labelProvider,
  moveProvider,
  type RoutingConfigState,
} from "./shared";

interface ManualConfigSectionProps {
  config: RoutingConfigState;
  setConfig: React.Dispatch<React.SetStateAction<RoutingConfigState>>;
}

export function ManualConfigSection({ config, setConfig }: ManualConfigSectionProps) {
  const tc = useThemeColors();

  return (
    <>
      <div className="card-sm" style={{ background: tc.bgHover }}>
        <div className="text-base font-bold" style={{ marginBottom: 6 }}>Gerarchia globale provider</div>
        <div className="text-sm text-muted" style={{ marginBottom: 12 }}>
          Il primo provider pronto viene usato per la chat. Se fallisce, l'orchestratore prova il successivo.
        </div>
        <div className="flex-col-gap-8">
          {config.providerHierarchy.map((provider, index) => (
            <div
              key={provider}
              className="flex-row"
              style={{
                justifyContent: "space-between",
                gap: 12,
                padding: "10px 12px",
                borderRadius: 8,
                background: "var(--color-bgInput)",
                border: "1px solid var(--color-border)",
              }}
            >
              <div>
                <div style={{ fontSize: 13, fontWeight: 700, color: tc.text }}>
                  {index + 1}. {labelProvider(provider)}
                </div>
                <div style={{ fontSize: 12, color: "var(--color-textMuted)" }}>{config.providerModels[provider]}</div>
              </div>
              <div style={{ display: "flex", gap: 6 }}>
                <button
                  onClick={() => setConfig((current) => ({ ...current, providerHierarchy: moveProvider(current.providerHierarchy, provider, -1) }))}
                  disabled={index === 0}
                  style={buttonStyle(tc, index === 0)}
                >
                  Su
                </button>
                <button
                  onClick={() => setConfig((current) => ({ ...current, providerHierarchy: moveProvider(current.providerHierarchy, provider, 1) }))}
                  disabled={index === config.providerHierarchy.length - 1}
                  style={buttonStyle(tc, index === config.providerHierarchy.length - 1)}
                >
                  Giu
                </button>
              </div>
            </div>
          ))}
        </div>
      </div>

      <div
        style={{
          display: "grid",
          gap: 12,
          gridTemplateColumns: "repeat(auto-fit, minmax(220px, 1fr))",
        }}
      >
        {PROVIDERS.map((provider) => (
          <div
            key={provider}
            className="card-sm"
            style={{
              background: "var(--color-bgHover)",
            }}
          >
            <div className="text-base font-bold" style={{ marginBottom: 10 }}>{labelProvider(provider)} model</div>
            <select
              value={config.providerModels[provider]}
              onChange={(event) =>
                setConfig((current) => ({
                  ...current,
                  providerModels: {
                    ...current.providerModels,
                    [provider]: event.target.value,
                  },
                }))
              }
              style={{ ...inputStyle(tc), cursor: "pointer" }}
            >
              <option value="">— auto (routing Nexus) —</option>
              {PROVIDER_MODELS[provider].map((m) => (
                <option key={m} value={m}>{m}</option>
              ))}
            </select>
          </div>
        ))}
      </div>

      <div className="card-sm" style={{ background: tc.bgHover }}>
        <div className="text-base font-bold" style={{ marginBottom: 6 }}>Override per intent</div>
        <div className="text-sm text-muted" style={{ marginBottom: 12 }}>
          Ogni intent puo avere una catena diversa. Se lasci la stessa sequenza della gerarchia globale, il comportamento resta uniforme.
        </div>
        <div style={{ display: "grid", gap: 12 }}>
          {ROUTING_INTENTS.map((intent) => (
            <div
              key={intent.key}
              style={{
                padding: 12,
                borderRadius: 8,
                border: "1px solid var(--color-border)",
                background: "var(--color-bgInput)",
              }}
            >
              <div className="flex-row" style={{ justifyContent: "space-between", gap: 12, marginBottom: 10 }}>
                <div className="text-base font-bold">{intent.label}</div>
                <button
                  onClick={() =>
                    setConfig((current) => ({
                      ...current,
                      intentChains: {
                        ...current.intentChains,
                        [intent.key]: [...current.providerHierarchy],
                      },
                    }))
                  }
                  style={buttonStyle(tc, false)}
                >
                  Allinea al globale
                </button>
              </div>
              <div className="flex-col-gap-8">
                {config.intentChains[intent.key].map((provider, index) => (
                  <div
                    key={`${intent.key}-${provider}`}
                    className="flex-row"
                    style={{
                      justifyContent: "space-between",
                      gap: 12,
                      padding: "8px 10px",
                      borderRadius: 8,
                      border: "1px solid var(--color-border)",
                      background: "var(--color-bgCard)",
                    }}
                  >
                    <span style={{ fontSize: 12, fontWeight: 600 }}>
                      {index + 1}. {labelProvider(provider)}
                    </span>
                    <div style={{ display: "flex", gap: 6 }}>
                      <button
                        onClick={() =>
                          setConfig((current) => ({
                            ...current,
                            intentChains: {
                              ...current.intentChains,
                              [intent.key]: moveProvider(current.intentChains[intent.key], provider, -1),
                            },
                          }))
                        }
                        disabled={index === 0}
                        style={buttonStyle(tc, index === 0)}
                      >
                        Su
                      </button>
                      <button
                        onClick={() =>
                          setConfig((current) => ({
                            ...current,
                            intentChains: {
                              ...current.intentChains,
                              [intent.key]: moveProvider(current.intentChains[intent.key], provider, 1),
                            },
                          }))
                        }
                        disabled={index === config.intentChains[intent.key].length - 1}
                        style={buttonStyle(tc, index === config.intentChains[intent.key].length - 1)}
                      >
                        Giu
                      </button>
                    </div>
                  </div>
                ))}
              </div>
            </div>
          ))}
        </div>
      </div>
    </>
  );
}

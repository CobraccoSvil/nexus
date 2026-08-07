"use client";

import { useThemeColors } from "../../../lib/theme";
import {
  ROUTING_INTENTS,
  buttonStyle,
  labelProvider,
  moveProvider,
  type RoutingConfigState,
} from "./shared";
import { useI18n } from "../../../lib/i18n";

interface ManualConfigSectionProps {
  config: RoutingConfigState;
  setConfig: React.Dispatch<React.SetStateAction<RoutingConfigState>>;
}

export function ManualConfigSection({ config, setConfig }: ManualConfigSectionProps) {
  const { t } = useI18n();
  const tc = useThemeColors();

  return (
    <>
      <div className="card-sm" style={{ background: tc.bgHover }}>
        <div className="text-base font-bold" style={{ marginBottom: 6 }}>{t("settings.gerarchiaGlobaleProvider")}</div>
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
              </div>
              <div style={{ display: "flex", gap: 6 }}>
                <button
                  onClick={() => setConfig((current) => ({ ...current, providerHierarchy: moveProvider(current.providerHierarchy, provider, -1) }))}
                  disabled={index === 0}
                  style={buttonStyle(tc, index === 0)}
                >
                  {t("settings.su")}
                </button>
                <button
                  onClick={() => setConfig((current) => ({ ...current, providerHierarchy: moveProvider(current.providerHierarchy, provider, 1) }))}
                  disabled={index === config.providerHierarchy.length - 1}
                  style={buttonStyle(tc, index === config.providerHierarchy.length - 1)}
                >
                  {t("settings.giu")}
                </button>
              </div>
            </div>
          ))}
        </div>
      </div>

      <div className="card-sm" style={{ background: tc.bgHover }}>
        <div className="text-base font-bold" style={{ marginBottom: 6 }}>{t("settings.overridePerIntent")}</div>
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
                  {t("settings.allineaAlGlobale")}
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
                        {t("settings.su")}
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
                        {t("settings.giu")}
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

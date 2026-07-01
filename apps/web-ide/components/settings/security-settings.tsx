"use client";

import React from "react";
import { useThemeColors } from "../../lib/theme";
import type { SettingEntry } from "./provider-settings";

interface SecuritySettingsProps {
  items: SettingEntry[];
  editValues: Record<string, string>;
  saving: Record<string, boolean>;
  saved: Record<string, boolean>;
  onEditChange: (key: string, value: string) => void;
  onSave: (key: string) => void;
}

/** Metadati leggibili per ogni setting DLP/security */
const SECURITY_META: Record<string, { label: string; description: string; type: "bool" | "text" | "number" }> = {
  dlp_enabled: {
    label: "DLP abilitato",
    description: "Attiva il sistema DLP (Data Loss Prevention). Classifica ogni messaggio per sensibilità e applica le policy provider.",
    type: "bool",
  },
  dlp_allow_cloud_tier2: {
    label: "Consenti dati sensibili (Tier 2) verso cloud",
    description: "Se abilitato, messaggi con dati sensibili (email, variabili d'ambiente con credenziali) possono essere inviati a provider cloud. Se disabilitato, vengono bloccati con suggerimento di usare Ollama o Mistral EU.",
    type: "bool",
  },
  dlp_allow_cloud_tier3: {
    label: "Consenti dati critici (Tier 3) verso cloud",
    description: "⚠️ SCONSIGLIATO. Se abilitato, messaggi con dati critici (chiavi API, JWT, password, PII) possono essere inviati a provider cloud. Mantieni disabilitato per massima sicurezza.",
    type: "bool",
  },
  ollama_enabled: {
    label: "Ollama (provider locale) abilitato",
    description: "Abilita il provider Ollama per eseguire modelli LLM localmente senza inviare dati a provider cloud. Richiede Ollama installato.",
    type: "bool",
  },
  ollama_url: {
    label: "URL Ollama",
    description: "Indirizzo del server Ollama locale (default: http://localhost:11434).",
    type: "text",
  },
};

const TIER_INFO = [
  { tier: 0, color: "#22c55e", label: "Tier 0 — Pubblico", desc: "Dati generici, commenti, domande senza dati aziendali." },
  { tier: 1, color: "#84cc16", label: "Tier 1 — Interno", desc: "Codice sorgente generico, percorsi file, localhost." },
  { tier: 2, color: "#f97316", label: "Tier 2 — Sensibile", desc: "Indirizzi email, variabili d'ambiente con credenziali, URL con password." },
  { tier: 3, color: "#ef4444", label: "Tier 3 — Critico", desc: "Chiavi API, JWT, password in chiaro, PII (Codice Fiscale, carta di credito), chiavi private." },
];

export function SecuritySettings({ items, editValues, saving, saved, onEditChange, onSave }: SecuritySettingsProps) {
  const tc = useThemeColors();

  const securityItems = items.filter((i) => SECURITY_META[i.key]);

  const renderBoolSetting = (item: SettingEntry) => {
    const meta = SECURITY_META[item.key];
    const currentVal = editValues[item.key] ?? item.value ?? "false";
    const isTrue = currentVal !== "false" && currentVal !== "0" && currentVal !== "";
    const isSaving = saving[item.key];
    const isSaved = saved[item.key];

    return (
      <div
        key={item.key}
        style={{
          background: "var(--color-bgCard)",
          border: "1px solid var(--color-border)",
          borderRadius: 8,
          padding: "14px 16px",
          marginBottom: 10,
        }}
      >
        <div className="flex-row-gap-12" style={{ justifyContent: "space-between" }}>
          <div className="flex-1">
            <div className="text-base font-semibold" style={{ color: "var(--color-text)", marginBottom: 4 }}>
              {meta.label}
            </div>
            <div className="text-xs" style={{ color: tc.textMuted, lineHeight: 1.4 }}>
              {meta.description}
            </div>
          </div>
          <div className="flex-row-gap-8 flex-shrink-0">
            <button
              onClick={() => {
                const newVal = isTrue ? "false" : "true";
                onEditChange(item.key, newVal);
                setTimeout(() => onSave(item.key), 50);
              }}
              style={{
                background: isTrue ? "#22c55e" : "var(--color-bgInput)",
                border: `1px solid ${isTrue ? "#16a34a" : "var(--color-border)"}`,
                borderRadius: 20,
                width: 44,
                height: 24,
                cursor: "pointer",
                position: "relative",
                transition: "background 0.2s",
                flexShrink: 0,
              }}
              title={isTrue ? "Clicca per disabilitare" : "Clicca per abilitare"}
            >
              <span
                style={{
                  display: "block",
                  width: 18,
                  height: 18,
                  borderRadius: "50%",
                  background: "#fff",
                  position: "absolute",
                  top: 2,
                  left: isTrue ? 22 : 2,
                  transition: "left 0.2s",
                  boxShadow: "0 1px 3px rgba(0,0,0,0.3)",
                }}
              />
            </button>
            <span style={{ fontSize: 11, color: isTrue ? "#22c55e" : "var(--color-textMuted)", minWidth: 28 }}>
              {isTrue ? "ON" : "OFF"}
            </span>
            {isSaving && <span style={{ fontSize: 10, color: "var(--color-textMuted)" }}>...</span>}
            {isSaved && <span style={{ fontSize: 10, color: "#22c55e" }}>✓</span>}
          </div>
        </div>
      </div>
    );
  };

  const renderTextSetting = (item: SettingEntry) => {
    const meta = SECURITY_META[item.key];
    const currentVal = editValues[item.key] ?? item.value ?? "";
    const isSaving = saving[item.key];
    const isSaved = saved[item.key];

    return (
      <div
        key={item.key}
        style={{
          background: "var(--color-bgCard)",
          border: "1px solid var(--color-border)",
          borderRadius: 8,
          padding: "14px 16px",
          marginBottom: 10,
        }}
      >
        <div style={{ fontWeight: 600, fontSize: 13, color: tc.text, marginBottom: 4 }}>
          {meta.label}
        </div>
        <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 8, lineHeight: 1.4 }}>
          {meta.description}
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <input
            type="text"
            value={currentVal}
            onChange={(e) => onEditChange(item.key, e.target.value)}
            style={{
              flex: 1,
              background: "var(--color-bgInput)",
              border: `1px solid ${tc.border}`,
              borderRadius: 6,
              padding: "6px 10px",
              fontSize: 12,
              color: tc.text,
              fontFamily: "var(--font-mono)",
            }}
          />
          <button
            onClick={() => onSave(item.key)}
            disabled={!!isSaving}
            style={{
              background: "#3b82f6",
              color: "#fff",
              border: "none",
              borderRadius: 6,
              padding: "6px 12px",
              fontSize: 12,
              cursor: "pointer",
            }}
          >
            {isSaving ? "..." : isSaved ? "✓" : "Salva"}
          </button>
        </div>
      </div>
    );
  };

  return (
    <div style={{ maxWidth: 700 }}>
      {/* Header */}
      <div className="mb-24">
        <h2 style={{ fontSize: 18, fontWeight: 700, color: tc.text, margin: 0, marginBottom: 6 }}>
          🔒 Sicurezza & Privacy
        </h2>
        <p style={{ fontSize: 12, color: tc.textMuted, margin: 0 }}>
          Configura il sistema DLP (Data Loss Prevention) e le policy di routing per la riservatezza dei dati.
        </p>
      </div>

      {/* Classificazione tier */}
      <div
        style={{
          background: "var(--color-bgCard)",
          border: "1px solid var(--color-border)",
          borderRadius: 10,
          padding: "14px 16px",
          marginBottom: 20,
        }}
      >
        <div style={{ fontSize: 13, fontWeight: 600, color: tc.text, marginBottom: 10 }}>
          📊 Classificazione Sensibilità (Tier)
        </div>
        <div style={{ display: "grid", gap: 8 }}>
          {TIER_INFO.map((t) => (
            <div key={t.tier} style={{ display: "flex", gap: 10, alignItems: "flex-start" }}>
              <span
                style={{
                  background: t.color,
                  color: "#fff",
                  borderRadius: 4,
                  padding: "2px 6px",
                  fontSize: 10,
                  fontWeight: 700,
                  flexShrink: 0,
                  minWidth: 36,
                  textAlign: "center",
                  marginTop: 1,
                }}
              >
                T{t.tier}
              </span>
              <div>
                <span style={{ fontSize: 11, fontWeight: 600, color: tc.text }}>{t.label}: </span>
                <span style={{ fontSize: 11, color: "var(--color-textSecondary)" }}>{t.desc}</span>
              </div>
            </div>
          ))}
        </div>
      </div>

      {/* Settings DLP */}
      {securityItems.length === 0 ? (
        <div style={{ color: tc.textMuted, fontSize: 12, textAlign: "center", padding: 24 }}>
          Nessuna impostazione di sicurezza trovata. Riavvia il server per inizializzare.
        </div>
      ) : (
        <>
          <div style={{ fontSize: 12, fontWeight: 600, color: tc.textMuted, marginBottom: 10, textTransform: "uppercase", letterSpacing: 1 }}>
            Policy DLP
          </div>
          {securityItems
            .filter((i) => SECURITY_META[i.key]?.type === "bool" && i.key.startsWith("dlp_"))
            .map(renderBoolSetting)}

          <div style={{ fontSize: 12, fontWeight: 600, color: tc.textMuted, marginBottom: 10, marginTop: 20, textTransform: "uppercase", letterSpacing: 1 }}>
            Provider Locale (Ollama)
          </div>
          {securityItems
            .filter((i) => i.key.startsWith("ollama_") && SECURITY_META[i.key]?.type === "bool")
            .map(renderBoolSetting)}
          {securityItems
            .filter((i) => i.key.startsWith("ollama_") && SECURITY_META[i.key]?.type === "text")
            .map(renderTextSetting)}
        </>
      )}
    </div>
  );
}

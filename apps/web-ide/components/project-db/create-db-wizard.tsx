"use client";

import type { Theme } from "../../lib/theme";
import type { ConnFields } from "./db-helpers";

/** Stile input compatto del wizard (regola L, S26): prima ripetuto inline su 8+ input. */
function fieldInputStyle(tc: Theme): React.CSSProperties {
  return {
    padding: "5px 7px",
    fontSize: 11,
    background: tc.bg,
    color: tc.text,
    border: `1px solid ${tc.border}`,
    borderRadius: 4,
  };
}

/** Label + input compatto del wizard, con stile uniforme. */
function WizardField({
  tc,
  label,
  type = "text",
  value,
  onChange,
  placeholder,
  flex,
}: {
  tc: Theme;
  label: string;
  type?: "text" | "password";
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  flex?: number;
}) {
  return (
    <div style={{ flex, display: "flex", flexDirection: "column", gap: 4 }}>
      <label style={{ fontSize: 10, color: tc.textMuted }}>{label}</label>
      <input
        type={type}
        value={value}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
        style={fieldInputStyle(tc)}
      />
    </div>
  );
}

interface Props {
  tc: Theme;
  provStep: "where" | "how";
  setProvStep: (s: "where" | "how") => void;
  provMode: "internal" | "external";
  setProvMode: (m: "internal" | "external") => void;
  provName: string;
  setProvName: (v: string) => void;
  provDbName: string;
  setProvDbName: (v: string) => void;
  provEngine: string;
  setProvEngine: (v: string) => void;
  provExt: ConnFields;
  setProvExt: (updater: (p: ConnFields) => ConnFields) => void;
  provBusy: boolean;
  provResult: { ok: boolean; message: string } | null;
  slugSuggestion: string;
  onClose: () => void;
  onProvision: () => void;
}

/** Wizard "Crea database": guida l'utente su DOVE (internal/external) e COME (nome/engine). */
export function CreateDbWizard({
  tc,
  provStep,
  setProvStep,
  provMode,
  setProvMode,
  provName,
  setProvName,
  provDbName,
  setProvDbName,
  provEngine,
  setProvEngine,
  provExt,
  setProvExt,
  provBusy,
  provResult,
  slugSuggestion,
  onClose,
  onProvision,
}: Props) {
  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 1000,
        background: "rgba(0,0,0,0.55)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: 16,
      }}
      onClick={() => { if (!provBusy) onClose(); }}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          width: 480,
          maxWidth: "100%",
          maxHeight: "90vh",
          overflow: "auto",
          background: tc.bgCard,
          border: `1px solid ${tc.border}`,
          borderRadius: 10,
          padding: 16,
          display: "flex",
          flexDirection: "column",
          gap: 12,
        }}
      >
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
          <div style={{ fontSize: 14, fontWeight: 700, color: tc.text }}>Crea database</div>
          <button
            type="button"
            onClick={() => { if (!provBusy) onClose(); }}
            style={{ border: "none", background: "transparent", color: tc.textMuted, cursor: "pointer", fontSize: 16, lineHeight: 1 }}
          >
            x
          </button>
        </div>

        <div style={{ display: "flex", gap: 8, fontSize: 11 }}>
          <span style={{ color: provStep === "where" ? tc.accent : tc.textMuted, fontWeight: 700 }}>1. Dove</span>
          <span style={{ color: tc.textMuted }}>-</span>
          <span style={{ color: provStep === "how" ? tc.accent : tc.textMuted, fontWeight: 700 }}>2. Come</span>
        </div>

        {provStep === "where" && (
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            <button
              type="button"
              onClick={() => { setProvMode("internal"); setProvEngine("postgres"); }}
              style={{
                textAlign: "left",
                padding: 12,
                borderRadius: 8,
                border: `2px solid ${provMode === "internal" ? tc.accent : tc.border}`,
                background: provMode === "internal" ? `${tc.accent}12` : "transparent",
                color: tc.text,
                cursor: "pointer",
              }}
            >
              <div style={{ fontSize: 12, fontWeight: 700 }}>Container dedicato (Nexus)</div>
              <div style={{ fontSize: 11, color: tc.textSecondary, marginTop: 4 }}>
                Nexus crea un PostgreSQL isolato per questo progetto nel cluster dedicato.
                Nessuna credenziale richiesta: il database e separato da quelli degli altri progetti.
              </div>
            </button>
            <button
              type="button"
              onClick={() => setProvMode("external")}
              style={{
                textAlign: "left",
                padding: 12,
                borderRadius: 8,
                border: `2px solid ${provMode === "external" ? tc.accent : tc.border}`,
                background: provMode === "external" ? `${tc.accent}12` : "transparent",
                color: tc.text,
                cursor: "pointer",
              }}
            >
              <div style={{ fontSize: 12, fontWeight: 700 }}>Database esterno</div>
              <div style={{ fontSize: 11, color: tc.textSecondary, marginTop: 4 }}>
                Usa un database gia esistente fornendo host, porta e credenziali.
                La connessione viene testata prima di essere registrata.
              </div>
            </button>
          </div>
        )}

        {provStep === "how" && (
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            <WizardField
              tc={tc}
              label="Nome connessione logica"
              value={provName}
              onChange={setProvName}
              placeholder="primary"
            />

            {provMode === "internal" && (
              <>
                <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                  <WizardField
                    tc={tc}
                    label="Nome del database"
                    value={provDbName}
                    onChange={setProvDbName}
                    placeholder={slugSuggestion}
                  />
                  <div style={{ fontSize: 10, color: tc.textMuted }}>
                    Suggerito dallo slug del progetto. Caratteri non validi vengono sostituiti con underscore.
                  </div>
                </div>
                <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                  <label style={{ fontSize: 10, color: tc.textMuted }}>Engine</label>
                  <select value="postgres" disabled style={fieldInputStyle(tc)}>
                    <option value="postgres">PostgreSQL</option>
                  </select>
                </div>
              </>
            )}

            {provMode === "external" && (
              <>
                <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                  <label style={{ fontSize: 10, color: tc.textMuted }}>Engine</label>
                  <select
                    value={provEngine}
                    onChange={(e) => setProvEngine(e.target.value)}
                    style={fieldInputStyle(tc)}
                  >
                    <option value="postgres">PostgreSQL</option>
                    <option value="mysql">MySQL</option>
                    <option value="sqlserver">SQL Server</option>
                    <option value="sqlite">SQLite</option>
                  </select>
                </div>
                {provEngine !== "sqlite" && (
                  <div style={{ display: "flex", gap: 6 }}>
                    <WizardField tc={tc} label="Host" value={provExt.host} flex={2}
                      onChange={(v) => setProvExt((p) => ({ ...p, host: v }))} />
                    <WizardField tc={tc} label="Porta" value={provExt.port} flex={1}
                      onChange={(v) => setProvExt((p) => ({ ...p, port: v }))} />
                  </div>
                )}
                <WizardField
                  tc={tc}
                  label={provEngine === "sqlite" ? "Percorso file" : "Nome database"}
                  value={provExt.database}
                  onChange={(v) => setProvExt((p) => ({ ...p, database: v }))}
                />
                {provEngine !== "sqlite" && (
                  <div style={{ display: "flex", gap: 6 }}>
                    <WizardField tc={tc} label="Utente" value={provExt.username} flex={1}
                      onChange={(v) => setProvExt((p) => ({ ...p, username: v }))} />
                    <div style={{ flex: 1 }}>
                    <WizardField tc={tc} label="Password" type="password" value={provExt.password}
                      onChange={(v) => setProvExt((p) => ({ ...p, password: v }))} />
                    </div>
                  </div>
                )}
              </>
            )}
          </div>
        )}

        {provResult && (
          <div
            style={{
              fontSize: 11,
              padding: 8,
              borderRadius: 6,
              border: `1px solid ${provResult.ok ? "#22c55e" : "#ef4444"}`,
              background: provResult.ok ? "#22c55e18" : "#ef444418",
              color: tc.text,
              wordBreak: "break-all",
            }}
          >
            {provResult.message}
          </div>
        )}

        <div style={{ display: "flex", justifyContent: "space-between", gap: 8, marginTop: 4 }}>
          <button
            type="button"
            disabled={provBusy}
            onClick={() => {
              if (provStep === "how") setProvStep("where");
              else onClose();
            }}
            style={{
              padding: "7px 12px",
              borderRadius: 6,
              border: `1px solid ${tc.border}`,
              background: "transparent",
              color: tc.text,
              cursor: provBusy ? "not-allowed" : "pointer",
              fontSize: 12,
            }}
          >
            {provStep === "how" ? "Indietro" : "Annulla"}
          </button>
          {provStep === "where" ? (
            <button
              type="button"
              onClick={() => setProvStep("how")}
              style={{
                padding: "7px 14px",
                borderRadius: 6,
                border: "none",
                background: tc.accent,
                color: "#fff",
                cursor: "pointer",
                fontSize: 12,
                fontWeight: 700,
              }}
            >
              Avanti
            </button>
          ) : (
            <button
              type="button"
              disabled={provBusy}
              onClick={() => onProvision()}
              style={{
                padding: "7px 14px",
                borderRadius: 6,
                border: "none",
                background: tc.accent,
                color: "#fff",
                cursor: provBusy ? "not-allowed" : "pointer",
                fontSize: 12,
                fontWeight: 700,
              }}
            >
              {provBusy ? "Creazione in corso..." : "Crea"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

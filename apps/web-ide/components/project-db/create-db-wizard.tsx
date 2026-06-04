"use client";

import type { Theme } from "../../lib/theme";
import type { ConnFields } from "./db-helpers";

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
            <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
              <label style={{ fontSize: 10, color: tc.textMuted }}>Nome connessione logica</label>
              <input
                type="text"
                value={provName}
                placeholder="primary"
                onChange={(e) => setProvName(e.target.value)}
                style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
              />
            </div>

            {provMode === "internal" && (
              <>
                <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                  <label style={{ fontSize: 10, color: tc.textMuted }}>Nome del database</label>
                  <input
                    type="text"
                    value={provDbName}
                    placeholder={slugSuggestion}
                    onChange={(e) => setProvDbName(e.target.value)}
                    style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
                  />
                  <div style={{ fontSize: 10, color: tc.textMuted }}>
                    Suggerito dallo slug del progetto. Caratteri non validi vengono sostituiti con underscore.
                  </div>
                </div>
                <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                  <label style={{ fontSize: 10, color: tc.textMuted }}>Engine</label>
                  <select
                    value="postgres"
                    disabled
                    style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
                  >
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
                    style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
                  >
                    <option value="postgres">PostgreSQL</option>
                    <option value="mysql">MySQL</option>
                    <option value="sqlserver">SQL Server</option>
                    <option value="sqlite">SQLite</option>
                  </select>
                </div>
                {provEngine !== "sqlite" && (
                  <div style={{ display: "flex", gap: 6 }}>
                    <div style={{ flex: 2, display: "flex", flexDirection: "column", gap: 4 }}>
                      <label style={{ fontSize: 10, color: tc.textMuted }}>Host</label>
                      <input type="text" value={provExt.host} onChange={(e) => setProvExt((p) => ({ ...p, host: e.target.value }))}
                        style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }} />
                    </div>
                    <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: 4 }}>
                      <label style={{ fontSize: 10, color: tc.textMuted }}>Porta</label>
                      <input type="text" value={provExt.port} onChange={(e) => setProvExt((p) => ({ ...p, port: e.target.value }))}
                        style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }} />
                    </div>
                  </div>
                )}
                <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                  <label style={{ fontSize: 10, color: tc.textMuted }}>{provEngine === "sqlite" ? "Percorso file" : "Nome database"}</label>
                  <input type="text" value={provExt.database} onChange={(e) => setProvExt((p) => ({ ...p, database: e.target.value }))}
                    style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }} />
                </div>
                {provEngine !== "sqlite" && (
                  <div style={{ display: "flex", gap: 6 }}>
                    <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: 4 }}>
                      <label style={{ fontSize: 10, color: tc.textMuted }}>Utente</label>
                      <input type="text" value={provExt.username} onChange={(e) => setProvExt((p) => ({ ...p, username: e.target.value }))}
                        style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }} />
                    </div>
                    <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: 4 }}>
                      <label style={{ fontSize: 10, color: tc.textMuted }}>Password</label>
                      <input type="password" value={provExt.password} onChange={(e) => setProvExt((p) => ({ ...p, password: e.target.value }))}
                        style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }} />
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

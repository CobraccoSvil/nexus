"use client";

/**
 * Pagina admin: gestione direttive condivise (nexus_shared_directives).
 *
 * CRUD completo: lista, crea, modifica, toggle attiva/disattiva, elimina.
 * Le direttive condivise vengono iniettate a runtime da prompt_registry.py
 * nei prompt di tutti gli agenti in base allo scope.
 */

import { useCallback, useEffect, useState } from "react";
import { useThemeColors } from "../../../../lib/theme";
import { useGlobalDialog } from "../../../../components/global-dialog-provider";
import {
  listSharedDirectives,
  createSharedDirective,
  updateSharedDirective,
  toggleSharedDirective,
  deleteSharedDirective,
  type SharedDirective,
} from "../../../../lib/api-client";

/* ── Helpers ──────────────────────────────────────────────────────────── */

function ScopeBadge({ scope, tc }: { scope: string; tc: ReturnType<typeof useThemeColors> }) {
  const colors: Record<string, { bg: string; fg: string }> = {
    agent: { bg: "#163a63", fg: "#5ba3e6" },
    system: { bg: "#3a2a10", fg: "#fbbf24" },
    all: { bg: "#1a3a2a", fg: "#4ade80" },
  };
  const c = colors[scope] ?? { bg: tc.bgHover, fg: tc.text };
  return (
    <span
      style={{
        padding: "3px 8px",
        borderRadius: 4,
        fontSize: 11,
        fontWeight: 600,
        background: c.bg,
        color: c.fg,
      }}
    >
      {scope}
    </span>
  );
}

function StatusDot({ active, tc }: { active: boolean; tc: ReturnType<typeof useThemeColors> }) {
  return (
    <span
      title={active ? "Attiva" : "Disattivata"}
      style={{
        display: "inline-block",
        width: 10,
        height: 10,
        borderRadius: "50%",
        background: active ? "#22c55e" : tc.textMuted,
        boxShadow: active ? "0 0 6px rgba(34,197,94,0.4)" : "none",
      }}
    />
  );
}

/* ── Componente principale ────────────────────────────────────────────── */

export default function SharedDirectivesPage() {
  const tc = useThemeColors();
  const { confirmDialog } = useGlobalDialog();

  const [directives, setDirectives] = useState<SharedDirective[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Stato per editing / creazione
  const [editing, setEditing] = useState<string | null>(null); // chiave in modifica
  const [creating, setCreating] = useState(false);
  const [saving, setSaving] = useState(false);

  // Form fields
  const [formKey, setFormKey] = useState("");
  const [formContent, setFormContent] = useState("");
  const [formScope, setFormScope] = useState<"agent" | "system" | "all">("agent");
  const [formPriority, setFormPriority] = useState(100);
  const [formDescription, setFormDescription] = useState("");

  /* ── Caricamento ────────────────────────────────────────────────── */

  const loadData = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const res = await listSharedDirectives();
      setDirectives(res.directives);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Errore nel caricamento");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadData();
  }, [loadData]);

  /* ── Azioni ─────────────────────────────────────────────────────── */

  const handleToggle = async (key: string) => {
    try {
      setError(null);
      const updated = await toggleSharedDirective(key);
      setDirectives((prev) =>
        prev.map((d) => (d.key === key ? updated : d)),
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : "Errore nel toggle");
    }
  };

  const handleDelete = async (key: string) => {
    const ok = await confirmDialog(
      `Eliminare la direttiva "${key}"?\nL'azione e' irreversibile.`,
      "Conferma eliminazione",
    );
    if (!ok) return;
    try {
      setError(null);
      await deleteSharedDirective(key);
      setDirectives((prev) => prev.filter((d) => d.key !== key));
      if (editing === key) {
        setEditing(null);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Errore nell'eliminazione");
    }
  };

  const startEdit = (d: SharedDirective) => {
    setCreating(false);
    setEditing(d.key);
    setFormKey(d.key);
    setFormContent(d.content);
    setFormScope(d.scope);
    setFormPriority(d.priority);
    setFormDescription(d.description ?? "");
  };

  const startCreate = () => {
    setEditing(null);
    setCreating(true);
    setFormKey("");
    setFormContent("");
    setFormScope("agent");
    setFormPriority(100);
    setFormDescription("");
  };

  const cancelForm = () => {
    setEditing(null);
    setCreating(false);
  };

  const handleSave = async () => {
    if (!formKey.trim()) {
      setError("La chiave e' obbligatoria");
      return;
    }
    if (!formContent.trim()) {
      setError("Il contenuto e' obbligatorio");
      return;
    }
    setSaving(true);
    setError(null);
    try {
      if (creating) {
        const created = await createSharedDirective({
          key: formKey.trim(),
          content: formContent,
          scope: formScope,
          priority: formPriority,
          description: formDescription || undefined,
        });
        setDirectives((prev) =>
          [...prev, created].sort((a, b) => a.priority - b.priority),
        );
        setCreating(false);
      } else if (editing) {
        const updated = await updateSharedDirective(editing, {
          content: formContent,
          scope: formScope,
          priority: formPriority,
          description: formDescription || undefined,
        });
        setDirectives((prev) =>
          prev
            .map((d) => (d.key === editing ? updated : d))
            .sort((a, b) => a.priority - b.priority),
        );
        setEditing(null);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : "Errore nel salvataggio");
    } finally {
      setSaving(false);
    }
  };

  /* ── Stili comuni ───────────────────────────────────────────────── */

  const btnStyle = (accent = false): React.CSSProperties => ({
    padding: "6px 14px",
    background: accent ? tc.accent : tc.bgHover,
    border: `1px solid ${accent ? tc.accent : tc.border}`,
    borderRadius: 6,
    fontSize: 12,
    fontWeight: 500,
    cursor: "pointer",
    color: accent ? "#fff" : tc.text,
    transition: "opacity 0.15s",
  });

  const inputStyle: React.CSSProperties = {
    width: "100%",
    padding: "8px 12px",
    borderRadius: 6,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput ?? tc.bg,
    color: tc.text,
    fontSize: 13,
    boxSizing: "border-box",
  };

  /* ── Render ─────────────────────────────────────────────────────── */

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 24, maxWidth: "100%", overflow: "hidden", boxSizing: "border-box" }}>
      {/* Intestazione */}
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", gap: 16 }}>
        <div>
          <h1 style={{ fontSize: 20, fontWeight: 700, color: tc.text, margin: 0 }}>
            Direttive Condivise
          </h1>
          <p style={{ marginTop: 4, fontSize: 13, color: tc.textSecondary }}>
            Regole comuni iniettate a runtime in tutti i prompt agente in base allo scope.
            Gestite dalla tabella <code style={{ fontSize: 12, color: tc.accent }}>nexus_shared_directives</code>.
          </p>
        </div>
        <button
          onClick={startCreate}
          style={btnStyle(true)}
          disabled={creating}
        >
          + Nuova direttiva
        </button>
      </div>

      {/* Errore */}
      {error && (
        <div
          style={{
            padding: "10px 16px",
            borderRadius: 8,
            background: "rgba(248,113,113,0.12)",
            color: "#f87171",
            fontSize: 13,
            border: "1px solid rgba(248,113,113,0.25)",
          }}
        >
          {error}
        </div>
      )}

      {/* Form creazione/modifica */}
      {(creating || editing) && (
        <div
          style={{
            borderRadius: 12,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard ?? tc.bg,
            padding: "20px 24px",
            maxWidth: "100%",
            boxSizing: "border-box",
            overflow: "hidden",
          }}
        >
          <h2 style={{ fontSize: 15, fontWeight: 600, color: tc.text, margin: "0 0 16px" }}>
            {creating ? "Nuova direttiva" : `Modifica: ${editing}`}
          </h2>

          {/* Riga 1: Chiave + Scope + Priorita' su una riga */}
          <div
            style={{
              display: "grid",
              gridTemplateColumns: "2fr 2fr 100px",
              gap: 12,
              marginBottom: 12,
            }}
          >
            <div>
              <label style={{ fontSize: 11, fontWeight: 600, color: tc.textSecondary, display: "block", marginBottom: 4 }}>
                Chiave (identificativo univoco)
              </label>
              <input
                value={formKey}
                onChange={(e) => setFormKey(e.target.value)}
                disabled={!!editing}
                placeholder="es. anti_narration"
                style={{
                  ...inputStyle,
                  opacity: editing ? 0.6 : 1,
                  cursor: editing ? "not-allowed" : "text",
                }}
              />
            </div>
            <div>
              <label style={{ fontSize: 11, fontWeight: 600, color: tc.textSecondary, display: "block", marginBottom: 4 }}>
                Scope (ambito)
              </label>
              <select
                value={formScope}
                onChange={(e) => setFormScope(e.target.value as "agent" | "system" | "all")}
                style={inputStyle}
              >
                <option value="agent">agent</option>
                <option value="system">system</option>
                <option value="all">all</option>
              </select>
            </div>
            <div>
              <label style={{ fontSize: 11, fontWeight: 600, color: tc.textSecondary, display: "block", marginBottom: 4 }}>
                Priorita'
              </label>
              <input
                type="number"
                value={formPriority}
                onChange={(e) => setFormPriority(parseInt(e.target.value) || 100)}
                min={0}
                max={9999}
                style={inputStyle}
              />
            </div>
          </div>

          {/* Riga 2: Descrizione a larghezza piena */}
          <div style={{ marginBottom: 12 }}>
            <label style={{ fontSize: 11, fontWeight: 600, color: tc.textSecondary, display: "block", marginBottom: 4 }}>
              Descrizione (opzionale)
            </label>
            <input
              value={formDescription}
              onChange={(e) => setFormDescription(e.target.value)}
              placeholder="Breve descrizione dello scopo della direttiva"
              style={inputStyle}
            />
          </div>

          {/* Contenuto */}
          <div style={{ marginBottom: 16 }}>
            <label style={{ fontSize: 11, fontWeight: 600, color: tc.textSecondary, display: "block", marginBottom: 4 }}>
              Contenuto (testo iniettato nel prompt)
            </label>
            <textarea
              value={formContent}
              onChange={(e) => setFormContent(e.target.value)}
              rows={8}
              placeholder="<direttiva>&#10;Testo della direttiva...&#10;</direttiva>"
              style={{
                ...inputStyle,
                fontFamily: "monospace",
                fontSize: 12,
                lineHeight: "1.5",
                resize: "vertical",
                minHeight: 120,
              }}
            />
            <div style={{ fontSize: 11, color: tc.textMuted, marginTop: 4 }}>
              {formContent.length.toLocaleString()} caratteri
            </div>
          </div>

          {/* Pulsanti */}
          <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
            <button onClick={cancelForm} style={btnStyle()}>
              Annulla
            </button>
            <button
              onClick={handleSave}
              disabled={saving}
              style={{
                ...btnStyle(true),
                opacity: saving ? 0.6 : 1,
              }}
            >
              {saving ? "Salvataggio..." : creating ? "Crea" : "Salva modifiche"}
            </button>
          </div>
        </div>
      )}

      {/* Tabella */}
      {loading ? (
        <div style={{ textAlign: "center", padding: 40, color: tc.textMuted, fontSize: 13 }}>
          Caricamento...
        </div>
      ) : directives.length === 0 ? (
        <div style={{ textAlign: "center", padding: 40, color: tc.textMuted, fontSize: 13 }}>
          Nessuna direttiva condivisa configurata.
        </div>
      ) : (
        <div
          style={{
            borderRadius: 12,
            border: `1px solid ${tc.border}`,
            overflow: "hidden",
          }}
        >
          <table style={{ width: "100%", borderCollapse: "collapse", tableLayout: "fixed" }}>
            <colgroup>
              <col style={{ width: 40 }} />
              <col style={{ width: "20%" }} />
              <col style={{ width: 64 }} />
              <col style={{ width: 48 }} />
              <col />
              <col style={{ width: 56 }} />
              <col style={{ width: 140 }} />
            </colgroup>
            <thead>
              <tr style={{ background: tc.bgCard ?? tc.bg }}>
                <th style={thStyle(tc)} />
                <th style={thStyle(tc)}>Chiave</th>
                <th style={thStyle(tc)}>Scope</th>
                <th style={{ ...thStyle(tc), textAlign: "center" }}>Prio</th>
                <th style={thStyle(tc)}>Descrizione</th>
                <th style={{ ...thStyle(tc), textAlign: "center" }}>Dim.</th>
                <th style={{ ...thStyle(tc), textAlign: "right" }}>Azioni</th>
              </tr>
            </thead>
            <tbody>
              {directives.map((d) => (
                <tr
                  key={d.key}
                  style={{
                    borderBottom: `1px solid ${tc.border}`,
                    background: editing === d.key ? tc.bgHover : "transparent",
                    opacity: d.isActive ? 1 : 0.55,
                  }}
                >
                  <td style={tdStyle}>
                    <button
                      onClick={() => handleToggle(d.key)}
                      title={d.isActive ? "Disattiva" : "Attiva"}
                      style={{
                        background: "transparent",
                        border: "none",
                        cursor: "pointer",
                        padding: 4,
                      }}
                    >
                      <StatusDot active={d.isActive} tc={tc} />
                    </button>
                  </td>
                  <td style={{ ...tdStyle, fontFamily: "monospace", fontSize: 12, fontWeight: 600, color: tc.text, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                    {d.key}
                  </td>
                  <td style={tdStyle}>
                    <ScopeBadge scope={d.scope} tc={tc} />
                  </td>
                  <td style={{ ...tdStyle, textAlign: "center", fontFamily: "monospace", fontSize: 12, color: tc.textSecondary }}>
                    {d.priority}
                  </td>
                  <td style={{ ...tdStyle, fontSize: 12, color: tc.textSecondary, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
                      title={d.description ?? ""}
                  >
                    {d.description ?? "--"}
                  </td>
                  <td style={{ ...tdStyle, textAlign: "center", fontFamily: "monospace", fontSize: 11, color: tc.textMuted }}>
                    {d.content.length.toLocaleString()}
                  </td>
                  <td style={{ ...tdStyle, textAlign: "right" }}>
                    <div style={{ display: "inline-flex", gap: 6 }}>
                      <button
                        onClick={() => startEdit(d)}
                        style={{
                          ...btnStyle(),
                          padding: "4px 8px",
                          fontSize: 11,
                        }}
                      >
                        Modifica
                      </button>
                      <button
                        onClick={() => handleDelete(d.key)}
                        style={{
                          padding: "4px 8px",
                          background: "rgba(248,113,113,0.1)",
                          border: "1px solid rgba(248,113,113,0.25)",
                          borderRadius: 6,
                          fontSize: 11,
                          fontWeight: 500,
                          cursor: "pointer",
                          color: "#f87171",
                        }}
                      >
                        Elimina
                      </button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {/* Info box */}
      <div
        style={{
          borderRadius: 10,
          border: `1px solid ${tc.border}`,
          background: tc.bgCard ?? tc.bg,
          padding: "14px 18px",
          fontSize: 12,
          color: tc.textSecondary,
          lineHeight: 1.6,
        }}
      >
        <strong style={{ color: tc.text }}>Come funziona:</strong> le direttive attive vengono caricate
        da <code style={{ color: tc.accent }}>prompt_registry.py</code> all&apos;avvio del brain e
        iniettate in coda a ogni prompt agente il cui prefisso corrisponde allo scope configurato.
        L&apos;ordine di iniezione segue la priorita' (valori inferiori = iniettati prima).
        Dopo una modifica, riavviare il brain per applicare le nuove impostazioni.
      </div>
    </div>
  );
}

/* ── Stili tabella ────────────────────────────────────────────────────── */

function thStyle(tc: ReturnType<typeof useThemeColors>): React.CSSProperties {
  return {
    padding: "10px 14px",
    textAlign: "left",
    fontWeight: 600,
    fontSize: 11,
    color: tc.textSecondary,
    textTransform: "uppercase",
    letterSpacing: "0.04em",
    borderBottom: `1px solid ${tc.border}`,
  };
}

const tdStyle: React.CSSProperties = {
  padding: "10px 14px",
  fontSize: 13,
};

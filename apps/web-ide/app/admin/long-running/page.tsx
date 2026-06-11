"use client";

import { useCallback, useEffect, useState } from "react";
import { useThemeColors } from "../../../lib/theme";
import { AdminPageHeader } from "../../../components/admin/AdminPageHeader";
import { ListEditorLayout } from "../../../components/admin/ListEditorLayout";
import { fetchJson } from "../../../lib/api/_shared";


const API = process.env.NEXT_PUBLIC_API_URL || "";

interface Pattern {
  id: string;
  pattern: string;
  description: string;
  enabled: boolean;
  createdAt: string;
}

export default function LongRunningPage() {
  const tc = useThemeColors();
  const [patterns, setPatterns] = useState<Pattern[]>([]);
  const [loading, setLoading] = useState(true);
  const [newPattern, setNewPattern] = useState("");
  const [newDesc, setNewDesc] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  const load = useCallback(async () => {
    try {
      setPatterns(await fetchJson<Pattern[]>(`${API}/api/admin/long-running`));
    } catch {
      /* ignore */
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  const add = async () => {
    if (!newPattern.trim()) return;
    setSaving(true);
    setError("");
    try {
      await fetchJson(`${API}/api/admin/long-running`, {
        method: "POST",
        body: JSON.stringify({ pattern: newPattern.trim(), description: newDesc.trim() }),
      });
      setNewPattern("");
      setNewDesc("");
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore");
    } finally {
      setSaving(false);
    }
  };

  const toggle = async (p: Pattern) => {
    try {
      await fetchJson(`${API}/api/admin/long-running/${p.id}`, {
        method: "PUT",
        body: JSON.stringify({ enabled: !p.enabled }),
      });
    } catch {
      /* fire-and-forget come il fetch precedente: l'esito visibile e' il reload */
    }
    await load();
  };

  const remove = async (id: string) => {
    try {
      await fetchJson(`${API}/api/admin/long-running/${id}`, { method: "DELETE" });
    } catch {
      /* fire-and-forget come il fetch precedente: l'esito visibile e' il reload */
    }
    await load();
  };

  const inputStyle: React.CSSProperties = {
    flex: 1,
    padding: "8px 12px",
    borderRadius: 6,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput || tc.bgCard,
    color: tc.text,
    fontSize: 13,
    outline: "none",
  };

  return (
    <ListEditorLayout
      header={
        <AdminPageHeader
          title="Long-Running Patterns"
          description="Comandi che l'agente AI riconosce automaticamente come processi long-running (server, watcher, ecc.) e avvia nel terminale senza bloccare."
        />
      }
    >
      {/* Add form */}
      <div
        style={{
          display: "flex",
          gap: 8,
          marginBottom: 20,
          flexWrap: "wrap",
        }}
      >
        <input
          value={newPattern}
          onChange={(e) => setNewPattern(e.target.value)}
          placeholder="Pattern (es. dotnet run, ./my-server)"
          style={{ ...inputStyle, minWidth: 200 }}
          onKeyDown={(e) => e.key === "Enter" && add()}
        />
        <input
          value={newDesc}
          onChange={(e) => setNewDesc(e.target.value)}
          placeholder="Descrizione (opzionale)"
          style={{ ...inputStyle, minWidth: 150 }}
          onKeyDown={(e) => e.key === "Enter" && add()}
        />
        <button
          onClick={add}
          disabled={saving || !newPattern.trim()}
          style={{
            padding: "8px 18px",
            borderRadius: 6,
            border: "none",
            background: tc.accent,
            color: "#fff",
            fontSize: 13,
            fontWeight: 600,
            cursor: "pointer",
            opacity: saving || !newPattern.trim() ? 0.5 : 1,
          }}
        >
          {saving ? "..." : "Aggiungi"}
        </button>
      </div>

      {error && (
        <div style={{ color: "#ef4444", fontSize: 13, marginBottom: 12 }}>{error}</div>
      )}

      {/* Table */}
      {loading ? (
        <div style={{ color: tc.textMuted, fontSize: 13 }}>Caricamento...</div>
      ) : (
        <div
          style={{
            border: `1px solid ${tc.border}`,
            borderRadius: 10,
            overflow: "hidden",
          }}
        >
          <table
            style={{
              width: "100%",
              borderCollapse: "collapse",
              fontSize: 13,
            }}
          >
            <thead>
              <tr style={{ background: tc.bgCard }}>
                <th style={{ padding: "10px 14px", textAlign: "left", color: tc.textMuted, fontWeight: 600, borderBottom: `1px solid ${tc.border}` }}>
                  Pattern
                </th>
                <th style={{ padding: "10px 14px", textAlign: "left", color: tc.textMuted, fontWeight: 600, borderBottom: `1px solid ${tc.border}` }}>
                  Descrizione
                </th>
                <th style={{ padding: "10px 14px", textAlign: "center", color: tc.textMuted, fontWeight: 600, borderBottom: `1px solid ${tc.border}`, width: 80 }}>
                  Attivo
                </th>
                <th style={{ padding: "10px 14px", textAlign: "center", color: tc.textMuted, fontWeight: 600, borderBottom: `1px solid ${tc.border}`, width: 60 }}>
                  {/* actions */}
                </th>
              </tr>
            </thead>
            <tbody>
              {patterns.map((p) => (
                <tr
                  key={p.id}
                  style={{
                    borderBottom: `1px solid ${tc.border}`,
                    opacity: p.enabled ? 1 : 0.5,
                  }}
                >
                  <td style={{ padding: "10px 14px", fontFamily: "monospace", color: tc.text }}>
                    {p.pattern}
                  </td>
                  <td style={{ padding: "10px 14px", color: tc.textMuted }}>
                    {p.description}
                  </td>
                  <td style={{ padding: "10px 14px", textAlign: "center" }}>
                    <button
                      onClick={() => toggle(p)}
                      style={{
                        background: p.enabled ? "#22c55e" : tc.border,
                        border: "none",
                        borderRadius: 10,
                        width: 36,
                        height: 20,
                        cursor: "pointer",
                        position: "relative",
                      }}
                    >
                      <div
                        style={{
                          width: 16,
                          height: 16,
                          borderRadius: "50%",
                          background: "#fff",
                          position: "absolute",
                          top: 2,
                          left: p.enabled ? 18 : 2,
                          transition: "left 0.15s",
                        }}
                      />
                    </button>
                  </td>
                  <td style={{ padding: "10px 14px", textAlign: "center" }}>
                    <button
                      onClick={() => remove(p.id)}
                      title="Elimina"
                      style={{
                        background: "transparent",
                        border: "none",
                        color: "#ef4444",
                        cursor: "pointer",
                        fontSize: 15,
                        padding: 4,
                      }}
                    >
                      ✕
                    </button>
                  </td>
                </tr>
              ))}
              {patterns.length === 0 && (
                <tr>
                  <td colSpan={4} style={{ padding: 20, textAlign: "center", color: tc.textMuted }}>
                    Nessun pattern configurato.
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      )}

      <p style={{ fontSize: 11, color: tc.textMuted, marginTop: 16 }}>
        Ogni pattern è una sequenza di token (es. &quot;npm run dev&quot;). Se il comando dell&apos;agente
        contiene questa sequenza, viene avviato nel terminale IDE in modalità fire-and-forget.
        I comandi non riconosciuti vengono comunque intercettati se non terminano entro 10 secondi.
      </p>
    </ListEditorLayout>
  );
}

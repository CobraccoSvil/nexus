"use client";

import { useCallback, useEffect, useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { ModalPortal } from "../modal-portal";
import {
  useProjectStore,
  selectMemoryChangedAt,
} from "../../lib/project-dispatcher/store";
import {
  listProjectMemories,
  toggleProjectMemory,
  type ProjectMemory,
} from "../../lib/api-client";

interface MemoryPanelProps {
  projectId: string;
  onClose: () => void;
}

export function MemoryPanel({ projectId, onClose }: MemoryPanelProps) {
  const tc = useThemeColors();
  const [memories, setMemories] = useState<ProjectMemory[]>([]);
  const [loading, setLoading] = useState(true);
  const [toggling, setToggling] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const data = await listProjectMemories(projectId);
      setMemories(data.memories);
    } finally {
      setLoading(false);
    }
  }, [projectId]);

  useEffect(() => { void load(); }, [load]);

  // Auto-refresh quando l'agente inserisce/aggiorna memorie via dispatcher SSE
  const memoryChangedAt = useProjectStore(selectMemoryChangedAt);
  useEffect(() => {
    if (memoryChangedAt > 0) void load();
  }, [memoryChangedAt, load]);

  const handleToggle = async (id: string) => {
    setToggling(id);
    try {
      const result = await toggleProjectMemory(id);
      setMemories((prev) =>
        prev.map((m) => (m.id === id ? { ...m, active: result.active } : m)),
      );
    } finally {
      setToggling(null);
    }
  };

  const activeCount = memories.filter((m) => m.active).length;

  return (
    <ModalPortal>
    {/* Overlay backdrop */}
    <div
      style={{
        position: "fixed", inset: 0, zIndex: 8000,
        background: "rgba(0,0,0,0.5)",
        display: "flex", alignItems: "center", justifyContent: "center",
      }}
      onMouseDown={onClose}
    >
      {/* Panel */}
      <div
        style={{
          background: tc.bgCard,
          border: `1px solid ${tc.border}`,
          borderRadius: 12,
          width: 520,
          maxHeight: "70vh",
          display: "flex",
          flexDirection: "column",
          boxShadow: "0 8px 40px rgba(0,0,0,0.4)",
          overflow: "hidden",
        }}
        onMouseDown={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div style={{
          padding: "16px 20px",
          borderBottom: `1px solid ${tc.border}`,
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
        }}>
          <div>
            <div style={{ fontSize: 14, fontWeight: 700, color: tc.text }}>
              📚 Memoria del progetto
            </div>
            <div style={{ fontSize: 11, color: tc.textMuted, marginTop: 2 }}>
              {activeCount > 0
                ? `${activeCount} memori${activeCount === 1 ? "a attiva" : "e attive"} nel contesto AI`
                : "Nessuna memoria attiva — il contesto AI usa solo la chat corrente"}
            </div>
          </div>
          <button
            onClick={onClose}
            style={{
              background: "none", border: "none", color: tc.textMuted,
              fontSize: 18, cursor: "pointer", lineHeight: 1,
            }}
          >
            ×
          </button>
        </div>

        {/* Info banner */}
        <div style={{
          margin: "12px 20px 0",
          padding: "10px 14px",
          borderRadius: 8,
          background: `${tc.accent}10`,
          border: `1px solid ${tc.accent}30`,
          fontSize: 12,
          color: tc.textMuted,
          lineHeight: 1.5,
        }}>
          💡 Le memorie sono <strong style={{ color: tc.text }}>riassunti di chat compattate</strong>.
          Quando attive, vengono incluse automaticamente nel contesto dell&apos;AI.
          Attivale solo quando vuoi che l&apos;AI usi quel contesto.
        </div>

        {/* List */}
        <div style={{ flex: 1, overflowY: "auto", padding: "12px 20px 20px" }}>
          {loading && (
            <div style={{ textAlign: "center", padding: 30, color: tc.textMuted, fontSize: 12 }}>
              Caricamento...
            </div>
          )}
          {!loading && memories.length === 0 && (
            <div style={{
              textAlign: "center", padding: 40,
              color: tc.textMuted, fontSize: 13,
            }}>
              <div style={{ fontSize: 28, marginBottom: 10 }}>🔮</div>
              Nessuna memoria salvata.<br />
              <span style={{ fontSize: 11 }}>
                Usa &quot;Compatta e salva in memoria&quot; dal menu di una chat per salvare un riassunto.
              </span>
            </div>
          )}
          {memories.map((memory) => (
            <div
              key={memory.id}
              style={{
                border: `1px solid ${memory.active ? tc.accent + "60" : tc.border}`,
                borderRadius: 8,
                padding: "12px 14px",
                marginBottom: 10,
                background: memory.active ? `${tc.accent}08` : tc.bgInput,
                transition: "all 0.15s",
              }}
            >
              {/* Memory header */}
              <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 8 }}>
                <span style={{ fontSize: 12, fontWeight: 600, color: tc.text, flex: 1 }}>
                  {memory.sessionTitle}
                </span>
                <span style={{
                  fontSize: 10, padding: "2px 7px", borderRadius: 10,
                  background: memory.active ? `${tc.accent}20` : tc.bgCard,
                  color: memory.active ? tc.accent : tc.textMuted,
                  border: `1px solid ${memory.active ? tc.accent + "50" : tc.border}`,
                  fontWeight: 600,
                }}>
                  {memory.active ? "✓ Attiva" : "Inattiva"}
                </span>
                <span style={{ fontSize: 10, color: tc.textMuted }}>
                  {new Date(memory.createdAt).toLocaleDateString("it-IT", {
                    day: "2-digit", month: "short", year: "numeric",
                  })}
                </span>
              </div>

              {/* Summary preview */}
              <p style={{
                fontSize: 11, color: tc.textMuted, margin: "0 0 10px",
                lineHeight: 1.6,
                overflow: "hidden",
                display: "-webkit-box",
                WebkitBoxOrient: "vertical",
                WebkitLineClamp: 3,
              }}>
                {memory.summary}
              </p>

              {/* Toggle button */}
              <button
                onClick={() => void handleToggle(memory.id)}
                disabled={toggling === memory.id}
                style={{
                  fontSize: 11, fontWeight: 600,
                  padding: "4px 12px", borderRadius: 6, cursor: "pointer",
                  background: memory.active ? "transparent" : `${tc.accent}15`,
                  color: memory.active ? tc.textMuted : tc.accent,
                  border: `1px solid ${memory.active ? tc.border : tc.accent + "60"}`,
                  opacity: toggling === memory.id ? 0.5 : 1,
                }}
              >
                {toggling === memory.id
                  ? "..."
                  : memory.active
                  ? "Disattiva dal contesto"
                  : "Attiva nel contesto"}
              </button>
            </div>
          ))}
        </div>
      </div>
    </div>
    </ModalPortal>
  );
}

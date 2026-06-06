"use client";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { useGlobalDialog } from "../global-dialog-provider";
import type { UserProjectDetails } from "../../lib/api-client";
import {
  type FileMutation,
  type MutationDetail,
  getMutationDetail,
  listMutations,
  revertMutation,
} from "../../lib/api/mutations";

interface Props {
  project: UserProjectDetails | null;
  onOpenInEditor?: (relativePath: string) => void;
}

function formatBytes(n: number | null | undefined): string {
  if (n == null) return "—";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function formatRelative(iso: string): string {
  try {
    const d = new Date(iso);
    const diff = (Date.now() - d.getTime()) / 1000;
    if (diff < 60) return `${Math.floor(diff)}s fa`;
    if (diff < 3600) return `${Math.floor(diff / 60)}m fa`;
    if (diff < 86400) return `${Math.floor(diff / 3600)}h fa`;
    return d.toLocaleString();
  } catch {
    return iso;
  }
}

const OP_COLORS: Record<string, string> = {
  created: "#32CD32",
  modified: "#FFA500",
  deleted: "#FF4444",
  reverted: "#9B7EDC",
};

export function MutationsSidebar({ project, onOpenInEditor }: Props) {
  const tc = useThemeColors();
  const { confirmDialog, alertDialog } = useGlobalDialog();
  const [items, setItems] = useState<FileMutation[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [reverting, setReverting] = useState<number | null>(null);
  const [openDetail, setOpenDetail] = useState<MutationDetail | null>(null);
  const [openDetailId, setOpenDetailId] = useState<number | null>(null);

  const refresh = useCallback(async () => {
    if (!project?.id) return;
    setLoading(true);
    setError(null);
    try {
      const r = await listMutations(project.id, 100);
      setItems(r.mutations || []);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Errore caricamento");
    } finally {
      setLoading(false);
    }
  }, [project?.id]);

  useEffect(() => { void refresh(); }, [refresh]);

  // L'evento SSE FileChanged (gia' emesso da write_file / edit_file e dal
  // revert) significa che la lista mutazioni e' cambiata: re-fetch.
  useEffect(() => {
    if (!project?.id) return;
    const handler = () => { void refresh(); };
    window.addEventListener("nexus:file:changed", handler);
    return () => window.removeEventListener("nexus:file:changed", handler);
  }, [project?.id, refresh]);

  const doRevert = useCallback(
    async (item: FileMutation, force = false) => {
      if (!project?.id) return;
      if (!force) {
        const ok = await confirmDialog({
          title: "Ripristina file",
          message: `Annullare la modifica al file "${item.file_path}" del ${formatRelative(item.created_at)}?\n\nIl file verra' sovrascritto con lo stato precedente. L'operazione e' a sua volta annullabile.`,
          confirmLabel: "Ripristina",
          cancelLabel: "Annulla",
        });
        if (!ok) return;
      }
      setReverting(item.id);
      try {
        await revertMutation(project.id, item.id, force);
        await refresh();
      } catch (e: unknown) {
        // Il backend restituisce 409 con corpo {error, conflict} se il file e'
        // stato modificato dopo: chiediamo conferma esplicita per il force.
        const msg = e instanceof Error ? e.message : "Errore ripristino";
        if (msg.toLowerCase().includes("conflict") || msg.includes("409")) {
          const ok = await confirmDialog({
            title: "Conflitto rilevato",
            message: `Il file e' stato modificato dopo questa mutazione. Forzare il ripristino sovrascriverebbe le modifiche successive.\n\nProcedere?`,
            danger: true,
            confirmLabel: "Forza ripristino",
            cancelLabel: "Annulla",
          });
          if (ok) {
            try {
              await revertMutation(project.id, item.id, true);
              await refresh();
            } catch (e2: unknown) {
              await alertDialog(
                e2 instanceof Error ? e2.message : "Errore ripristino",
                "Ripristino fallito",
              );
            }
          }
        } else {
          await alertDialog(msg, "Ripristino fallito");
        }
      } finally {
        setReverting(null);
      }
    },
    [project?.id, refresh, confirmDialog, alertDialog],
  );

  const openDiff = useCallback(
    async (item: FileMutation) => {
      if (!project?.id) return;
      try {
        const d = await getMutationDetail(project.id, item.id);
        setOpenDetail(d);
        setOpenDetailId(item.id);
      } catch (e: unknown) {
        await alertDialog(
          e instanceof Error ? e.message : "Errore caricamento dettaglio",
          "Errore",
        );
      }
    },
    [project?.id, alertDialog],
  );

  const empty = useMemo(() => !loading && items.length === 0 && !error, [loading, items, error]);

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%", minHeight: 0 }}>
      <div
        style={{
          display: "flex", alignItems: "center", justifyContent: "space-between",
          padding: "8px 12px", borderBottom: `1px solid ${tc.border}`, gap: 8,
        }}
      >
        <span style={{ fontSize: 12, fontWeight: 600, color: tc.text, textTransform: "uppercase", letterSpacing: 0.5 }}>
          Modifiche
        </span>
        <button
          type="button"
          onClick={() => void refresh()}
          title="Aggiorna"
          style={{ background: "transparent", border: "none", color: tc.textMuted, cursor: "pointer", fontSize: 14 }}
        >
          {"↻"}
        </button>
      </div>

      <div style={{ flex: 1, minHeight: 0, overflowY: "auto" }}>
        {loading && (
          <div style={{ padding: 12, color: tc.textMuted, fontSize: 12 }}>Caricamento...</div>
        )}
        {error && (
          <div style={{ padding: 12, color: tc.error, fontSize: 12 }}>{error}</div>
        )}
        {empty && (
          <div style={{ padding: 16, color: tc.textMuted, fontSize: 12, lineHeight: 1.5 }}>
            Nessuna modifica registrata. Le modifiche fatte dall'agente (write_file, edit_file) verranno elencate qui e potranno essere ripristinate con un click.
          </div>
        )}
        {items.map((m) => {
          const isReverted = !!m.reverted_at;
          const color = OP_COLORS[m.op] || tc.textMuted;
          const isRevertingThis = reverting === m.id;
          return (
            <div
              key={m.id}
              style={{
                padding: "8px 12px",
                borderBottom: `1px solid ${tc.border}`,
                opacity: isReverted ? 0.55 : 1,
              }}
            >
              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                <span style={{ fontSize: 10, color, fontWeight: 700, textTransform: "uppercase" }}>
                  {m.op}
                </span>
                <span style={{ fontSize: 11, color: tc.textMuted }}>
                  {formatRelative(m.created_at)}
                </span>
                {isReverted && (
                  <span style={{ fontSize: 10, color: tc.textMuted, fontStyle: "italic" }}>
                    annullata
                  </span>
                )}
              </div>
              <div
                style={{
                  fontSize: 12, color: tc.text, marginTop: 4,
                  wordBreak: "break-all", cursor: onOpenInEditor ? "pointer" : "default",
                }}
                onClick={() => onOpenInEditor?.(m.file_path)}
                title={onOpenInEditor ? "Apri nell'editor" : undefined}
              >
                {m.file_path}
              </div>
              <div style={{ fontSize: 10, color: tc.textMuted, marginTop: 2 }}>
                {m.tool_name} · {formatBytes(m.before_size)} → {formatBytes(m.after_size)}
              </div>
              <div style={{ display: "flex", gap: 6, marginTop: 6 }}>
                <button
                  type="button"
                  onClick={() => void openDiff(m)}
                  style={{
                    fontSize: 11, background: "transparent",
                    color: tc.accent, border: `1px solid ${tc.accent}44`,
                    borderRadius: 4, padding: "3px 8px", cursor: "pointer",
                  }}
                >
                  Vedi diff
                </button>
                {m.revertible && !isReverted && (
                  <button
                    type="button"
                    disabled={isRevertingThis}
                    onClick={() => void doRevert(m)}
                    style={{
                      fontSize: 11, background: "transparent",
                      color: tc.warning ?? "#FFA500",
                      border: `1px solid ${(tc.warning ?? "#FFA500")}66`,
                      borderRadius: 4, padding: "3px 8px",
                      cursor: isRevertingThis ? "wait" : "pointer",
                      opacity: isRevertingThis ? 0.6 : 1,
                    }}
                  >
                    {isRevertingThis ? "..." : "Ripristina"}
                  </button>
                )}
              </div>
            </div>
          );
        })}
      </div>

      {openDetail && openDetailId !== null && (
        <DiffModal
          detail={openDetail}
          onClose={() => { setOpenDetail(null); setOpenDetailId(null); }}
        />
      )}
    </div>
  );
}

function DiffModal({ detail, onClose }: { detail: MutationDetail; onClose: () => void }) {
  const tc = useThemeColors();
  const truncatedBefore = detail.before_size != null && detail.before_content == null;
  const truncatedAfter = detail.after_size != null && detail.after_content == null;
  return (
    <div
      onClick={onClose}
      style={{
        position: "fixed", inset: 0, background: "rgba(0,0,0,0.5)",
        zIndex: 1000, display: "flex", alignItems: "center", justifyContent: "center",
      }}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          background: tc.bgCard, border: `1px solid ${tc.border}`, borderRadius: 8,
          width: "min(900px, 90vw)", maxHeight: "85vh", display: "flex", flexDirection: "column",
        }}
      >
        <div style={{ padding: "10px 14px", borderBottom: `1px solid ${tc.border}`, display: "flex", justifyContent: "space-between", alignItems: "center" }}>
          <div>
            <div style={{ fontSize: 13, fontWeight: 600, color: tc.text }}>{detail.file_path}</div>
            <div style={{ fontSize: 11, color: tc.textMuted }}>
              {detail.tool_name} · {detail.op} · mutazione #{detail.id}
            </div>
          </div>
          <button
            type="button" onClick={onClose}
            style={{ background: "transparent", border: "none", color: tc.textMuted, fontSize: 18, cursor: "pointer" }}
          >×</button>
        </div>
        <div style={{ flex: 1, display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8, padding: 8, overflow: "hidden", minHeight: 0 }}>
          <DiffPane title="Prima" content={detail.before_content} truncated={truncatedBefore} tc={tc} />
          <DiffPane title="Dopo" content={detail.after_content} truncated={truncatedAfter} tc={tc} />
        </div>
      </div>
    </div>
  );
}

function DiffPane({
  title, content, truncated, tc,
}: {
  title: string;
  content: string | null;
  truncated: boolean;
  tc: ReturnType<typeof useThemeColors>;
}) {
  return (
    <div style={{ display: "flex", flexDirection: "column", minHeight: 0, border: `1px solid ${tc.border}`, borderRadius: 6 }}>
      <div style={{ padding: "6px 10px", fontSize: 11, fontWeight: 600, color: tc.textMuted, borderBottom: `1px solid ${tc.border}`, textTransform: "uppercase" }}>
        {title}
      </div>
      <pre style={{ flex: 1, minHeight: 0, margin: 0, padding: 10, overflow: "auto", fontSize: 11, color: tc.text, background: tc.bg, whiteSpace: "pre-wrap", wordBreak: "break-all" }}>
        {truncated
          ? "[contenuto non disponibile: file sopra la soglia di tracking]"
          : content == null
            ? "[file non esistente]"
            : content}
      </pre>
    </div>
  );
}

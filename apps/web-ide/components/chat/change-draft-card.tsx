"use client";

import { useState, useEffect } from "react";
import {
  getChangeDraft,
  approveChangeDraft,
  rejectChangeDraft,
  type ChangeDraftDetail,
} from "../../lib/api-client";

export type ChangeDraft = ChangeDraftDetail;

interface Props {
  draftId: string;
  onApplied?: () => void;
  onRejected?: () => void;
}

export function ChangeDraftCard({ draftId, onApplied, onRejected }: Props) {
  const [draft, setDraft] = useState<ChangeDraft | null>(null);
  const [loading, setLoading] = useState(false);
  const [acting, setActing] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    getChangeDraft(draftId)
      .then((d) => {
        if (!cancelled) setDraft(d);
      })
      .catch((e) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [draftId]);

  const handleApprove = async () => {
    setActing(true);
    try {
      await approveChangeDraft(draftId);
      setDraft((d) => (d ? { ...d, status: "approved" } : d));
      onApplied?.();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setActing(false);
    }
  };

  const handleReject = async () => {
    setActing(true);
    try {
      await rejectChangeDraft(draftId);
      setDraft((d) => (d ? { ...d, status: "rejected" } : d));
      onRejected?.();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setActing(false);
    }
  };

  if (loading) {
    return (
      <div style={{ padding: 12, fontSize: 12, color: "#737373" }}>
        Caricamento draft...
      </div>
    );
  }

  if (error) {
    return (
      <div
        style={{
          padding: 12,
          fontSize: 12,
          color: "#dc2626",
          background: "#fef2f2",
          border: "1px solid #fecaca",
          borderRadius: 8,
        }}
      >
        Errore caricamento draft: {error}
      </div>
    );
  }

  if (!draft) return null;

  const isPending = draft.status === "pending";
  const statusColor =
    draft.status === "approved"
      ? "#16a34a"
      : draft.status === "rejected"
      ? "#dc2626"
      : draft.status === "applied"
      ? "#0ea5e9"
      : "#f59e0b";

  return (
    <div
      style={{
        margin: "8px 0",
        border: "1px solid #d4d4d4",
        borderRadius: 10,
        background: "#fafafa",
        overflow: "hidden",
        minWidth: 0,
      }}
    >
      <div
        style={{
          padding: "10px 12px",
          background: "#fff",
          borderBottom: "1px solid #e5e5e5",
          display: "flex",
          alignItems: "center",
          gap: 8,
          minWidth: 0,
        }}
      >
        <span
          style={{
            fontSize: 10,
            fontWeight: 700,
            padding: "2px 6px",
            borderRadius: 4,
            background: statusColor + "22",
            color: statusColor,
            flexShrink: 0,
            textTransform: "uppercase",
          }}
        >
          {draft.status}
        </span>
        <span
          style={{
            fontSize: 11,
            color: "#737373",
            flexShrink: 0,
          }}
        >
          {draft.trigger_kind}
        </span>
        <span
          style={{
            fontWeight: 600,
            fontSize: 13,
            color: "#171717",
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
            minWidth: 0,
            flex: 1,
          }}
          title={draft.summary}
        >
          {draft.summary}
        </span>
      </div>

      {draft.draft.razionale && (
        <div style={{ padding: "8px 12px", fontSize: 12, color: "#171717" }}>
          <strong>Razionale:</strong> {draft.draft.razionale}
        </div>
      )}

      {!expanded && (
        <button
          onClick={() => setExpanded(true)}
          style={{
            background: "transparent",
            border: "none",
            color: "#0ea5e9",
            fontSize: 11,
            cursor: "pointer",
            padding: "0 12px 8px",
          }}
        >
          Mostra dettagli...
        </button>
      )}

      {expanded && (
        <div style={{ padding: "0 12px 8px", fontSize: 12 }}>
          {draft.draft.impact_analysis && (
            <div style={{ marginBottom: 8 }}>
              <div style={{ fontWeight: 600, marginBottom: 4 }}>Impact analysis</div>
              {draft.draft.impact_analysis.files_to_modify && draft.draft.impact_analysis.files_to_modify.length > 0 && (
                <div style={{ fontSize: 11, color: "#525252" }}>
                  Files: {draft.draft.impact_analysis.files_to_modify.join(", ")}
                </div>
              )}
              {draft.draft.impact_analysis.breaking_changes && (
                <div style={{ fontSize: 11, color: "#dc2626", marginTop: 2 }}>
                  Contiene breaking changes
                </div>
              )}
              {draft.draft.impact_analysis.migration_required && (
                <div style={{ fontSize: 11, color: "#f59e0b", marginTop: 2 }}>
                  Richiede migrazione DB
                </div>
              )}
            </div>
          )}

          {draft.draft.diff_proposto && (
            <details style={{ marginBottom: 8 }}>
              <summary style={{ cursor: "pointer", fontSize: 11, fontWeight: 600 }}>
                Diff proposto
              </summary>
              <pre
                style={{
                  margin: "6px 0 0",
                  padding: 8,
                  background: "#1e1e1e",
                  color: "#d4d4d4",
                  fontSize: 11,
                  fontFamily: "Menlo, monospace",
                  borderRadius: 4,
                  overflow: "auto",
                  maxHeight: 280,
                }}
              >
                {draft.draft.diff_proposto}
              </pre>
            </details>
          )}

          {draft.draft.alternative_considerate && draft.draft.alternative_considerate.length > 0 && (
            <div style={{ marginBottom: 8, fontSize: 11, color: "#525252" }}>
              <div style={{ fontWeight: 600 }}>Alternative considerate</div>
              {draft.draft.alternative_considerate.map((a, i) => (
                <div key={i} style={{ marginTop: 2 }}>
                  - <em>{a.opzione}</em>: scartata perche' {a.scartata_perche}
                </div>
              ))}
            </div>
          )}

          {draft.draft.verification_steps && draft.draft.verification_steps.length > 0 && (
            <div style={{ marginBottom: 8, fontSize: 11, color: "#525252" }}>
              <div style={{ fontWeight: 600 }}>Verifica</div>
              <ul style={{ margin: "2px 0", paddingLeft: 16 }}>
                {draft.draft.verification_steps.map((s, i) => (
                  <li key={i}>{s}</li>
                ))}
              </ul>
            </div>
          )}
        </div>
      )}

      {isPending && (
        <div
          style={{
            padding: "8px 12px",
            background: "#f5f5f5",
            display: "flex",
            gap: 6,
            justifyContent: "flex-end",
            borderTop: "1px solid #e5e5e5",
          }}
        >
          <button
            onClick={handleReject}
            disabled={acting}
            style={{
              padding: "4px 12px",
              fontSize: 12,
              background: "transparent",
              border: "1px solid #d4d4d4",
              borderRadius: 6,
              cursor: acting ? "default" : "pointer",
              color: "#525252",
            }}
          >
            Annulla
          </button>
          <button
            onClick={handleApprove}
            disabled={acting}
            style={{
              padding: "4px 12px",
              fontSize: 12,
              background: acting ? "#a3a3a3" : "#16a34a",
              color: "#fff",
              border: "none",
              borderRadius: 6,
              cursor: acting ? "default" : "pointer",
              fontWeight: 600,
            }}
          >
            {acting ? "..." : "Applica"}
          </button>
        </div>
      )}
    </div>
  );
}

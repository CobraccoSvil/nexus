"use client";

import { useState, useEffect, useCallback } from "react";
import {
  listMetaDocs,
  getMetaDoc,
  triggerMetaDocsRefresh,
  type MetaDocSummary,
  type MetaDocDetail,
  type MetaDocKind,
} from "../../lib/api-client";
import { MarkdownBlock } from "../chat/markdown-renderer";

const KIND_LABELS: Record<MetaDocKind, string> = {
  architecture: "Architettura",
  adr: "ADR",
  api: "API",
  schema: "Schema",
  runbook: "Runbook",
  changelog: "Changelog",
  decision: "Decisioni",
  other: "Altro",
};

const KIND_COLORS: Record<MetaDocKind, string> = {
  architecture: "#6366f1",
  adr: "#dc2626",
  api: "#0ea5e9",
  schema: "#7c3aed",
  runbook: "#16a34a",
  changelog: "#f59e0b",
  decision: "#db2777",
  other: "#737373",
};

export function MetaTab() {
  const [items, setItems] = useState<MetaDocSummary[]>([]);
  const [total, setTotal] = useState(0);
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [kindFilter, setKindFilter] = useState<MetaDocKind | "">("");
  const [q, setQ] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [selected, setSelected] = useState<MetaDocDetail | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await listMetaDocs({
        kind: kindFilter || undefined,
        q: q.trim() || undefined,
        limit: 50,
      });
      setItems(res.items);
      setTotal(res.total);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [kindFilter, q]);

  useEffect(() => {
    load();
  }, [load]);

  useEffect(() => {
    if (!selectedId) {
      setSelected(null);
      return;
    }
    getMetaDoc(selectedId)
      .then(setSelected)
      .catch((e) => setError(String(e)));
  }, [selectedId]);

  const handleRefresh = useCallback(async () => {
    setRefreshing(true);
    setError(null);
    try {
      await triggerMetaDocsRefresh();
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setRefreshing(false);
    }
  }, [load]);

  if (selected) {
    return (
      <div style={{ padding: 10, overflow: "hidden", minWidth: 0, height: "100%", display: "flex", flexDirection: "column" }}>
        <button
          onClick={() => {
            setSelectedId(null);
            setSelected(null);
          }}
          style={{
            alignSelf: "flex-start",
            marginBottom: 8,
            padding: "4px 10px",
            fontSize: 12,
            background: "transparent",
            border: "1px solid #d4d4d4",
            borderRadius: 6,
            cursor: "pointer",
          }}
        >
          {"< Indietro"}
        </button>
        <div style={{ display: "flex", gap: 6, alignItems: "center", marginBottom: 6, minWidth: 0 }}>
          <span
            style={{
              fontSize: 10,
              fontWeight: 700,
              padding: "2px 6px",
              borderRadius: 4,
              background: KIND_COLORS[selected.kind] + "22",
              color: KIND_COLORS[selected.kind],
              flexShrink: 0,
            }}
          >
            {KIND_LABELS[selected.kind]}
          </span>
          <h3
            style={{
              margin: 0,
              fontSize: 13,
              fontWeight: 700,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
              minWidth: 0,
              flex: 1,
            }}
            title={selected.title}
          >
            {selected.title}
          </h3>
        </div>
        <div style={{ fontSize: 11, color: "#a3a3a3", marginBottom: 8 }}>
          {selected.vault_file_path} • {selected.auto_generated ? "auto" : "user-curated"}
        </div>
        <div style={{ flex: 1, overflow: "auto", fontSize: 13 }}>
          <MarkdownBlock content={selected.body_md} skipNormalize />
        </div>
        {(selected.incoming_links?.length ?? 0) > 0 && (
          <div style={{ marginTop: 10, paddingTop: 8, borderTop: "1px solid #e5e5e5" }}>
            <div style={{ fontSize: 11, fontWeight: 700, marginBottom: 4 }}>Backlinks</div>
            {selected.incoming_links.map((l, i) => (
              <div key={i} style={{ fontSize: 11, color: "#737373", marginBottom: 2, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                <span style={{ color: "#6366f1" }}>{l.rel_type}</span>: {l.title}
              </div>
            ))}
          </div>
        )}
      </div>
    );
  }

  return (
    <div style={{ padding: 10, overflow: "hidden", minWidth: 0, height: "100%", display: "flex", flexDirection: "column" }}>
      <div style={{ display: "flex", gap: 4, marginBottom: 6, minWidth: 0 }}>
        <input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && load()}
          placeholder="Cerca nel vault..."
          style={{
            flex: 1,
            minWidth: 0,
            padding: "5px 8px",
            fontSize: 12,
            border: "1px solid #d4d4d4",
            borderRadius: 6,
            outline: "none",
          }}
        />
        <button
          onClick={handleRefresh}
          disabled={refreshing}
          title="Esegui tutti i generator e aggiorna il vault"
          style={{
            flexShrink: 0,
            padding: "5px 8px",
            fontSize: 11,
            background: refreshing ? "#a3a3a3" : "#171717",
            color: "#fff",
            border: "none",
            borderRadius: 6,
            cursor: refreshing ? "default" : "pointer",
          }}
        >
          {refreshing ? "..." : "Refresh"}
        </button>
      </div>

      <div style={{ display: "flex", gap: 4, marginBottom: 8, flexWrap: "wrap" }}>
        <button
          onClick={() => setKindFilter("")}
          style={{
            padding: "3px 8px",
            fontSize: 10,
            border: "1px solid " + (kindFilter === "" ? "#171717" : "#d4d4d4"),
            background: kindFilter === "" ? "#171717" : "transparent",
            color: kindFilter === "" ? "#fff" : "#737373",
            borderRadius: 4,
            cursor: "pointer",
          }}
        >
          Tutto
        </button>
        {(Object.keys(KIND_LABELS) as MetaDocKind[]).map((k) => (
          <button
            key={k}
            onClick={() => setKindFilter(k)}
            style={{
              padding: "3px 8px",
              fontSize: 10,
              border: "1px solid " + (kindFilter === k ? KIND_COLORS[k] : "#d4d4d4"),
              background: kindFilter === k ? KIND_COLORS[k] : "transparent",
              color: kindFilter === k ? "#fff" : KIND_COLORS[k],
              borderRadius: 4,
              cursor: "pointer",
              whiteSpace: "nowrap",
            }}
          >
            {KIND_LABELS[k]}
          </button>
        ))}
      </div>

      {error && (
        <div style={{ fontSize: 11, color: "#dc2626", marginBottom: 6, padding: 6, background: "#fef2f2", borderRadius: 4 }}>
          {error}
        </div>
      )}

      <div style={{ fontSize: 10, color: "#a3a3a3", marginBottom: 6 }}>
        {loading ? "Caricamento..." : `${total} doc nel vault`}
      </div>

      <div style={{ flex: 1, overflow: "auto", minHeight: 0 }}>
        {items.map((it) => (
          <div
            key={it.id}
            onClick={() => setSelectedId(it.id)}
            style={{
              padding: "8px 10px",
              marginBottom: 4,
              borderRadius: 6,
              border: "1px solid #e5e5e5",
              cursor: "pointer",
              overflow: "hidden",
              minWidth: 0,
            }}
            onMouseOver={(e) => (e.currentTarget.style.background = "#fafafa")}
            onMouseOut={(e) => (e.currentTarget.style.background = "transparent")}
          >
            <div style={{ display: "flex", gap: 6, alignItems: "center", minWidth: 0 }}>
              <span
                style={{
                  fontSize: 9,
                  fontWeight: 700,
                  padding: "1px 5px",
                  borderRadius: 3,
                  background: KIND_COLORS[it.kind] + "22",
                  color: KIND_COLORS[it.kind],
                  flexShrink: 0,
                }}
              >
                {KIND_LABELS[it.kind]}
              </span>
              <span
                style={{
                  fontWeight: 600,
                  fontSize: 12,
                  color: "#171717",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                  flex: 1,
                  minWidth: 0,
                }}
                title={it.title}
              >
                {it.title}
              </span>
            </div>
            <div
              style={{
                fontSize: 10,
                color: "#a3a3a3",
                marginTop: 2,
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
            >
              {it.vault_file_path}
            </div>
          </div>
        ))}
        {!loading && items.length === 0 && (
          <div style={{ padding: 16, fontSize: 12, color: "#a3a3a3", textAlign: "center" }}>
            Nessuna doc nel vault. Esegui Refresh per generarle.
          </div>
        )}
      </div>
    </div>
  );
}

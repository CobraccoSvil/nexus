"use client";

// TripleBrowser — tabella per il browse delle triple (soggetto, predicato,
// oggetto) della knowledge base. Filtrabile per predicate, source,
// min_confidence. ADR 0017 v2 fase 7.

import * as React from "react";
import {
  listTriples,
  type WikiScope,
  type WikiTriple,
  type WikiTripleSource,
} from "../../lib/wiki-client";
import { useThemeColors } from "../../lib/theme";
import { useI18n } from "../../lib/i18n";

interface Props {
  scope: WikiScope;
  projectId?: string;
  onOpenDoc?: (docId: string) => void;
}

const PREDICATES: string[] = [
  "relates",
  "supersedes",
  "depends_on",
  "illustrates",
  "contradicts",
  "followup",
  "correction_of",
  "refines",
  "duplicate_of",
  "blocks",
  "blocked_by",
  "mentions",
  "implements",
  "tests",
];

const SOURCES: WikiTripleSource[] = [
  "wikilink",
  "semantic",
  "llm",
  "user",
  "agent",
  "external",
];

const SOURCE_COLOR: Record<WikiTripleSource, string> = {
  wikilink: "#0ea5e9",
  semantic: "#7c3aed",
  llm: "#16a34a",
  user: "#f59e0b",
  agent: "#dc2626",
  external: "#737373",
};

export function TripleBrowser({ scope, projectId, onOpenDoc }: Props) {
  const tc = useThemeColors();
  const { t } = useI18n();

  const [predicate, setPredicate] = React.useState<string>("");
  const [source, setSource] = React.useState<string>("");
  const [minConfidence, setMinConfidence] = React.useState(0);
  const [q, setQ] = React.useState("");
  const [items, setItems] = React.useState<WikiTriple[]>([]);
  const [total, setTotal] = React.useState(0);
  const [offset, setOffset] = React.useState(0);
  const [loading, setLoading] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);

  const limit = 50;

  const load = React.useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const r = await listTriples({
        scope,
        project_id: projectId,
        predicate: predicate || undefined,
        source: (source as WikiTripleSource) || undefined,
        min_confidence: minConfidence > 0 ? minConfidence : undefined,
        q: q || undefined,
        limit,
        offset,
      });
      setItems(r.items);
      setTotal(r.total);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [scope, projectId, predicate, source, minConfidence, q, offset]);

  React.useEffect(() => {
    void load();
  }, [load]);

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        height: "100%",
        minWidth: 0,
        background: tc.bg,
      }}
    >
      {/* Toolbar filtri */}
      <div
        style={{
          padding: "10px 14px",
          borderBottom: `1px solid ${tc.border}`,
          display: "flex",
          gap: 10,
          alignItems: "center",
          flexWrap: "wrap",
          background: tc.bgCard,
        }}
      >
        <FilterField label={t("wiki.triples.predicate")}>
          <select
            value={predicate}
            onChange={(e) => {
              setOffset(0);
              setPredicate(e.target.value);
            }}
            style={selectStyle(tc)}
          >
            <option value="">— {t("wiki.filter.any")} —</option>
            {PREDICATES.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        </FilterField>
        <FilterField label={t("wiki.triples.source")}>
          <select
            value={source}
            onChange={(e) => {
              setOffset(0);
              setSource(e.target.value);
            }}
            style={selectStyle(tc)}
          >
            <option value="">— {t("wiki.filter.any")} —</option>
            {SOURCES.map((s) => (
              <option key={s} value={s}>
                {s}
              </option>
            ))}
          </select>
        </FilterField>
        <FilterField label={`${t("wiki.triples.confidence")} >= ${minConfidence.toFixed(2)}`}>
          <input
            type="range"
            min={0}
            max={1}
            step={0.05}
            value={minConfidence}
            onChange={(e) => {
              setOffset(0);
              setMinConfidence(Number(e.target.value));
            }}
            style={{ width: 140 }}
          />
        </FilterField>
        <input
          value={q}
          onChange={(e) => {
            setOffset(0);
            setQ(e.target.value);
          }}
          placeholder={t("wiki.triples.search_placeholder")}
          style={{
            flex: 1,
            minWidth: 180,
            padding: "5px 8px",
            background: tc.bgInput,
            border: `1px solid ${tc.border}`,
            color: tc.text,
            borderRadius: 4,
            fontSize: 12,
          }}
        />
        <span style={{ fontSize: 11, color: tc.textSecondary }}>
          {total} {t("wiki.triples.total")}
        </span>
      </div>

      {error && (
        <div style={{ padding: "6px 14px", color: tc.error, fontSize: 12 }}>{error}</div>
      )}

      {/* Tabella */}
      <div style={{ flex: 1, overflow: "auto", minHeight: 0 }}>
        <table
          style={{
            width: "100%",
            borderCollapse: "collapse",
            fontSize: 12,
            color: tc.text,
          }}
        >
          <thead>
            <tr
              style={{
                position: "sticky",
                top: 0,
                background: tc.bgCard,
                borderBottom: `1px solid ${tc.border}`,
                textAlign: "left",
              }}
            >
              <Th>{t("wiki.triples.subject")}</Th>
              <Th>{t("wiki.triples.predicate")}</Th>
              <Th>{t("wiki.triples.object")}</Th>
              <Th>{t("wiki.triples.source")}</Th>
              <Th>{t("wiki.triples.confidence")}</Th>
              <Th>{t("wiki.triples.created")}</Th>
            </tr>
          </thead>
          <tbody>
            {loading && (
              <tr>
                <td colSpan={6} style={{ padding: 16, color: tc.textSecondary }}>
                  {t("wiki.loading")}
                </td>
              </tr>
            )}
            {!loading && items.length === 0 && (
              <tr>
                <td colSpan={6} style={{ padding: 16, color: tc.textSecondary }}>
                  {t("wiki.triples.empty")}
                </td>
              </tr>
            )}
            {items.map((tr) => (
              <tr
                key={tr.id}
                style={{ borderBottom: `1px solid ${tc.border}` }}
              >
                <Td>
                  <DocLink id={tr.subj_doc_id} onOpenDoc={onOpenDoc} tc={tc} />
                </Td>
                <Td>
                  <code style={predicateChip(tc)}>{tr.predicate}</code>
                </Td>
                <Td>
                  {tr.obj_doc_id ? (
                    <DocLink id={tr.obj_doc_id} onOpenDoc={onOpenDoc} tc={tc} />
                  ) : tr.obj_external ? (
                    <a
                      href={tr.obj_external}
                      target="_blank"
                      rel="noreferrer"
                      style={{ color: tc.accent }}
                    >
                      {tr.obj_external}
                    </a>
                  ) : (
                    <span style={{ color: tc.textSecondary }}>{tr.obj_text}</span>
                  )}
                </Td>
                <Td>
                  <span
                    title={tr.evidence ?? ""}
                    style={{
                      padding: "1px 6px",
                      background: SOURCE_COLOR[tr.source] + "22",
                      color: SOURCE_COLOR[tr.source],
                      borderRadius: 10,
                      fontSize: 10,
                      fontWeight: 600,
                    }}
                  >
                    {tr.source}
                  </span>
                </Td>
                <Td>
                  <ConfidenceBadge value={tr.confidence} tc={tc} />
                </Td>
                <Td>
                  <span style={{ color: tc.textSecondary, fontSize: 11 }}>
                    {new Date(tr.created_at).toLocaleDateString()}
                  </span>
                </Td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {/* Pagination */}
      <div
        style={{
          padding: "8px 14px",
          borderTop: `1px solid ${tc.border}`,
          display: "flex",
          gap: 8,
          alignItems: "center",
          background: tc.bgCard,
        }}
      >
        <button
          type="button"
          disabled={offset === 0}
          onClick={() => setOffset(Math.max(0, offset - limit))}
          style={pagerBtn(tc, offset === 0)}
        >
          ←
        </button>
        <span style={{ fontSize: 12, color: tc.textSecondary }}>
          {offset + 1}–{Math.min(offset + limit, total)} / {total}
        </span>
        <button
          type="button"
          disabled={offset + limit >= total}
          onClick={() => setOffset(offset + limit)}
          style={pagerBtn(tc, offset + limit >= total)}
        >
          →
        </button>
      </div>
    </div>
  );
}

// ──────────────────────── Helper componenti ─────────────────────────────

type ThemeColors = ReturnType<typeof useThemeColors>;

function Th({ children }: { children: React.ReactNode }) {
  return <th style={{ padding: "8px 12px", fontWeight: 600 }}>{children}</th>;
}

function Td({ children }: { children: React.ReactNode }) {
  return <td style={{ padding: "6px 12px", verticalAlign: "top" }}>{children}</td>;
}

function FilterField({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  const tc = useThemeColors();
  return (
    <label style={{ display: "flex", flexDirection: "column", gap: 2 }}>
      <span style={{ fontSize: 10, color: tc.textSecondary, textTransform: "uppercase" }}>
        {label}
      </span>
      {children}
    </label>
  );
}

function DocLink({
  id,
  onOpenDoc,
  tc,
}: {
  id: string;
  onOpenDoc?: (id: string) => void;
  tc: ThemeColors;
}) {
  return (
    <button
      type="button"
      onClick={() => onOpenDoc?.(id)}
      style={{
        background: "transparent",
        border: "none",
        color: tc.accent,
        cursor: "pointer",
        fontFamily: "inherit",
        fontSize: 12,
        padding: 0,
        textAlign: "left",
      }}
      title={id}
    >
      {id.slice(0, 8)}…
    </button>
  );
}

function ConfidenceBadge({ value }: { value: number; tc?: ThemeColors }) {
  const color =
    value >= 0.8 ? "#16a34a" : value >= 0.6 ? "#f59e0b" : "#dc2626";
  return (
    <span
      style={{
        padding: "1px 6px",
        background: color + "22",
        color,
        borderRadius: 10,
        fontSize: 11,
        fontWeight: 600,
      }}
    >
      {value.toFixed(2)}
    </span>
  );
}

function selectStyle(tc: ThemeColors): React.CSSProperties {
  return {
    padding: "4px 6px",
    background: tc.bgInput,
    color: tc.text,
    border: `1px solid ${tc.border}`,
    borderRadius: 4,
    fontSize: 12,
  };
}

function predicateChip(tc: ThemeColors): React.CSSProperties {
  return {
    padding: "1px 6px",
    background: tc.bgInput,
    color: tc.text,
    border: `1px solid ${tc.border}`,
    borderRadius: 10,
    fontSize: 11,
    fontFamily: "monospace",
  };
}

function pagerBtn(tc: ThemeColors, disabled: boolean): React.CSSProperties {
  return {
    padding: "4px 10px",
    background: tc.bgInput,
    color: tc.text,
    border: `1px solid ${tc.border}`,
    borderRadius: 4,
    cursor: disabled ? "not-allowed" : "pointer",
    opacity: disabled ? 0.4 : 1,
    fontSize: 12,
  };
}

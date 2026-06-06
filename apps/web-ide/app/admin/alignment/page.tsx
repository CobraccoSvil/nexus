"use client";

/**
 * Pagina admin (sola lettura): allineamento direttive di prompt engineering.
 *
 * Tre sezioni:
 *  - Conformita' per template (conformance piu' recente per prompt_key)
 *  - Direttive (knowledge base versionata, stato attivazione/approvazione)
 *  - Proposte pending (revisioni SAFELIST, mai auto-applicate)
 *
 * MVP read-only: nessun bottone di scrittura. Dati da admin-service
 * (/api/admin/alignment/*) via fetchJson (punto unico, regola L).
 */

import { useCallback, useState } from "react";

import { AdminPageHeader } from "../../../components/admin/AdminPageHeader";
import {
  listAlignmentConformance,
  listAlignmentGuidelines,
  listAlignmentProposals,
  type AlignmentConformanceRow,
  type AlignmentDimensions,
  type AlignmentGuidelineRow,
  type AlignmentIssue,
  type AlignmentProposalRow,
} from "../../../lib/api/prompts";
import { useListData } from "../../../lib/use-list-data";
import { useThemeColors } from "../../../lib/theme";

const CONFORMANCE_THRESHOLD = 0.75;

type ThemeColors = ReturnType<typeof useThemeColors>;

function formatScore(value: number | undefined): string {
  if (value === undefined || value === null || Number.isNaN(value)) return "—";
  return value.toFixed(3);
}

function formatDate(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return d.toLocaleString("it-IT", {
    day: "2-digit",
    month: "2-digit",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export default function AlignmentPage() {
  const tc = useThemeColors();

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 28, minWidth: 0 }}>
      <AdminPageHeader
        title="Allineamento direttive"
        description="Stato di conformita' dei template prompt rispetto alle direttive di prompt engineering. Vista di sola lettura."
      />

      <ConformanceSection tc={tc} />
      <GuidelinesSection tc={tc} />
      <ProposalsSection tc={tc} />
    </div>
  );
}

// ── Layout helpers ───────────────────────────────────────────────────────────

function SectionShell({
  tc,
  title,
  subtitle,
  loading,
  error,
  empty,
  emptyLabel,
  children,
}: {
  tc: ThemeColors;
  title: string;
  subtitle: string;
  loading: boolean;
  error: string | null;
  empty: boolean;
  emptyLabel: string;
  children: React.ReactNode;
}) {
  return (
    <section style={{ display: "flex", flexDirection: "column", gap: 12, minWidth: 0 }}>
      <div style={{ minWidth: 0 }}>
        <h3 style={{ fontSize: 15, fontWeight: 700, margin: "0 0 2px", color: tc.text }}>{title}</h3>
        <p style={{ fontSize: 12, color: tc.textMuted, margin: 0 }}>{subtitle}</p>
      </div>

      {loading ? (
        <div style={{ fontSize: 13, color: tc.textMuted, padding: "8px 0" }}>Caricamento…</div>
      ) : error ? (
        <div
          style={{
            fontSize: 13,
            color: tc.error,
            padding: "10px 12px",
            borderRadius: 8,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard,
          }}
        >
          Errore: {error}
        </div>
      ) : empty ? (
        <div style={{ fontSize: 13, color: tc.textMuted, padding: "8px 0" }}>{emptyLabel}</div>
      ) : (
        children
      )}
    </section>
  );
}

function tableWrapStyle(tc: ThemeColors): React.CSSProperties {
  return {
    display: "block",
    overflowX: "auto",
    border: `1px solid ${tc.border}`,
    borderRadius: 8,
    minWidth: 0,
  };
}

function thStyle(tc: ThemeColors): React.CSSProperties {
  return {
    textAlign: "left",
    padding: "8px 10px",
    fontSize: 11,
    fontWeight: 600,
    textTransform: "uppercase",
    letterSpacing: "0.04em",
    color: tc.textMuted,
    borderBottom: `1px solid ${tc.border}`,
    whiteSpace: "nowrap",
  };
}

function tdStyle(tc: ThemeColors): React.CSSProperties {
  return {
    padding: "8px 10px",
    fontSize: 12,
    color: tc.text,
    borderBottom: `1px solid ${tc.border}`,
    verticalAlign: "top",
  };
}

function Badge({
  tc,
  label,
  tone,
}: {
  tc: ThemeColors;
  label: string;
  tone: "ok" | "warn" | "muted" | "info";
}) {
  const palette: Record<string, { bg: string; fg: string }> = {
    ok: { bg: "rgba(74,222,128,0.15)", fg: tc.success },
    warn: { bg: "rgba(248,113,113,0.15)", fg: tc.error },
    info: { bg: tc.accentBg, fg: tc.accent },
    muted: { bg: tc.border, fg: tc.textMuted },
  };
  const { bg, fg } = palette[tone];
  return (
    <span
      style={{
        display: "inline-block",
        flexShrink: 0,
        padding: "2px 8px",
        borderRadius: 6,
        fontSize: 11,
        fontWeight: 600,
        background: bg,
        color: fg,
        whiteSpace: "nowrap",
      }}
    >
      {label}
    </span>
  );
}

// ── Sezione 1: Conformita' per template ──────────────────────────────────────

function ScoreBar({ tc, score }: { tc: ThemeColors; score: number }) {
  const below = score < CONFORMANCE_THRESHOLD;
  const pct = Math.max(0, Math.min(1, score)) * 100;
  const color = below ? tc.error : tc.success;
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 8, minWidth: 0 }}>
      <div
        style={{
          flex: 1,
          minWidth: 60,
          height: 6,
          borderRadius: 3,
          background: tc.border,
          overflow: "hidden",
        }}
      >
        <div style={{ width: `${pct}%`, height: "100%", background: color }} />
      </div>
      <span style={{ flexShrink: 0, fontSize: 12, fontWeight: 600, color }}>
        {formatScore(score)}
      </span>
    </div>
  );
}

function DimensionsCell({ tc, dimensions }: { tc: ThemeColors; dimensions: AlignmentDimensions }) {
  const items: Array<[string, number | undefined]> = [
    ["Allin.", dimensions.alignment],
    ["Strut.", dimensions.structure],
    ["Chiar.", dimensions.clarity],
    ["Safety", dimensions.safety_preservation],
  ];
  return (
    <div style={{ display: "flex", flexWrap: "wrap", gap: "2px 10px", minWidth: 0 }}>
      {items.map(([label, val]) => (
        <span key={label} style={{ fontSize: 11, color: tc.textMuted, whiteSpace: "nowrap" }}>
          {label}: <span style={{ color: tc.text, fontWeight: 600 }}>{formatScore(val)}</span>
        </span>
      ))}
    </div>
  );
}

function IssuesCell({ tc, issues }: { tc: ThemeColors; issues: AlignmentIssue[] }) {
  const [open, setOpen] = useState(false);
  if (!issues || issues.length === 0) {
    return <span style={{ fontSize: 12, color: tc.textMuted }}>0</span>;
  }
  return (
    <div style={{ minWidth: 0 }}>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        style={{
          background: "none",
          border: "none",
          padding: 0,
          cursor: "pointer",
          fontSize: 12,
          fontWeight: 600,
          color: tc.accent,
          display: "inline-flex",
          alignItems: "center",
          gap: 4,
        }}
      >
        {issues.length} {issues.length === 1 ? "problema" : "problemi"}
        <span style={{ fontSize: 9 }}>{open ? "▾" : "▸"}</span>
      </button>
      {open && (
        <ul style={{ listStyle: "none", margin: "6px 0 0", padding: 0, display: "flex", flexDirection: "column", gap: 6 }}>
          {issues.map((issue, idx) => (
            <li
              key={`${issue.practice_key ?? "issue"}-${idx}`}
              style={{ display: "flex", gap: 6, alignItems: "flex-start", minWidth: 0 }}
            >
              {issue.severity ? (
                <Badge tc={tc} label={issue.severity} tone={issue.severity === "must" ? "warn" : "muted"} />
              ) : null}
              <span style={{ fontSize: 11, color: tc.text, minWidth: 0, wordBreak: "break-word" }}>
                {issue.practice_key ? (
                  <strong style={{ color: tc.textSecondary }}>{issue.practice_key}: </strong>
                ) : null}
                {issue.detail ?? "(nessun dettaglio)"}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function ConformanceSection({ tc }: { tc: ThemeColors }) {
  const { data, loading, error } = useListData<AlignmentConformanceRow>(
    useCallback(() => listAlignmentConformance(), []),
  );

  return (
    <SectionShell
      tc={tc}
      title="Conformita' per template"
      subtitle={`Esito piu' recente per ciascun template. Soglia sotto-conformita': ${CONFORMANCE_THRESHOLD.toFixed(2)}.`}
      loading={loading}
      error={error}
      empty={data.length === 0}
      emptyLabel="Nessun conformance check registrato."
    >
      <div style={tableWrapStyle(tc)}>
        <table style={{ width: "100%", borderCollapse: "collapse" }}>
          <thead>
            <tr>
              <th style={thStyle(tc)}>Template</th>
              <th style={thStyle(tc)}>Ver.</th>
              <th style={{ ...thStyle(tc), minWidth: 140 }}>Score</th>
              <th style={thStyle(tc)}>Dimensioni</th>
              <th style={thStyle(tc)}>Problemi</th>
              <th style={thStyle(tc)}>Verificato</th>
            </tr>
          </thead>
          <tbody>
            {data.map((row) => {
              const below = row.overall_score < CONFORMANCE_THRESHOLD;
              return (
                <tr
                  key={`${row.prompt_key}-${row.prompt_version}`}
                  style={below ? { background: "rgba(248,113,113,0.07)" } : undefined}
                >
                  <td style={{ ...tdStyle(tc), maxWidth: 220 }}>
                    <span
                      style={{
                        display: "block",
                        overflow: "hidden",
                        textOverflow: "ellipsis",
                        whiteSpace: "nowrap",
                        fontWeight: 600,
                      }}
                      title={row.prompt_key}
                    >
                      {row.prompt_key}
                    </span>
                  </td>
                  <td style={{ ...tdStyle(tc), whiteSpace: "nowrap" }}>v{row.prompt_version}</td>
                  <td style={{ ...tdStyle(tc), minWidth: 140 }}>
                    <ScoreBar tc={tc} score={row.overall_score} />
                  </td>
                  <td style={tdStyle(tc)}>
                    <DimensionsCell tc={tc} dimensions={row.dimensions ?? {}} />
                  </td>
                  <td style={tdStyle(tc)}>
                    <IssuesCell tc={tc} issues={row.issues ?? []} />
                  </td>
                  <td style={{ ...tdStyle(tc), whiteSpace: "nowrap", color: tc.textMuted }}>
                    {formatDate(row.checked_at)}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </SectionShell>
  );
}

// ── Sezione 2: Direttive ─────────────────────────────────────────────────────

function GuidelineStatus({ tc, row }: { tc: ThemeColors; row: AlignmentGuidelineRow }) {
  if (row.is_active && row.approved_by) {
    return <Badge tc={tc} label="Attiva" tone="ok" />;
  }
  return <Badge tc={tc} label="In attesa di approvazione" tone="warn" />;
}

function severityTone(severity: string): "warn" | "info" | "muted" {
  if (severity === "must") return "warn";
  if (severity === "should") return "info";
  return "muted";
}

function GuidelinesSection({ tc }: { tc: ThemeColors }) {
  const { data, loading, error } = useListData<AlignmentGuidelineRow>(
    useCallback(() => listAlignmentGuidelines(), []),
  );

  return (
    <SectionShell
      tc={tc}
      title="Direttive"
      subtitle="Knowledge base versionata delle best practice di prompt engineering."
      loading={loading}
      error={error}
      empty={data.length === 0}
      emptyLabel="Nessuna direttiva registrata."
    >
      <div style={tableWrapStyle(tc)}>
        <table style={{ width: "100%", borderCollapse: "collapse" }}>
          <thead>
            <tr>
              <th style={thStyle(tc)}>Practice</th>
              <th style={thStyle(tc)}>Fonte</th>
              <th style={thStyle(tc)}>Severita'</th>
              <th style={thStyle(tc)}>Ambito</th>
              <th style={thStyle(tc)}>Stato</th>
              <th style={{ ...thStyle(tc), minWidth: 200 }}>Descrizione</th>
            </tr>
          </thead>
          <tbody>
            {data.map((row, idx) => (
              <tr key={`${row.practice_key}-${idx}`}>
                <td style={{ ...tdStyle(tc), maxWidth: 200 }}>
                  <span
                    style={{
                      display: "block",
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                      fontWeight: 600,
                    }}
                    title={row.practice_key}
                  >
                    {row.practice_key}
                  </span>
                </td>
                <td style={{ ...tdStyle(tc), whiteSpace: "nowrap" }}>{row.source}</td>
                <td style={tdStyle(tc)}>
                  <Badge tc={tc} label={row.severity} tone={severityTone(row.severity)} />
                </td>
                <td style={{ ...tdStyle(tc), whiteSpace: "nowrap" }}>{row.applies_to}</td>
                <td style={tdStyle(tc)}>
                  <GuidelineStatus tc={tc} row={row} />
                </td>
                <td style={{ ...tdStyle(tc), minWidth: 200 }}>
                  <span style={{ display: "block", wordBreak: "break-word", color: tc.textSecondary }}>
                    {row.description}
                  </span>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </SectionShell>
  );
}

// ── Sezione 3: Proposte pending ──────────────────────────────────────────────

function ProposalsSection({ tc }: { tc: ThemeColors }) {
  const { data, loading, error } = useListData<AlignmentProposalRow>(
    useCallback(() => listAlignmentProposals(), []),
  );

  return (
    <SectionShell
      tc={tc}
      title="Proposte in attesa"
      subtitle="Revisioni proposte per i prompt protetti (SAFELIST): mai auto-applicate, richiedono approvazione admin."
      loading={loading}
      error={error}
      empty={data.length === 0}
      emptyLabel="Nessuna proposta in attesa."
    >
      <div style={tableWrapStyle(tc)}>
        <table style={{ width: "100%", borderCollapse: "collapse" }}>
          <thead>
            <tr>
              <th style={thStyle(tc)}>Template</th>
              <th style={thStyle(tc)}>Baseline</th>
              <th style={thStyle(tc)}>Origine</th>
              <th style={{ ...thStyle(tc), minWidth: 220 }}>Motivazione</th>
              <th style={thStyle(tc)}>Creata</th>
            </tr>
          </thead>
          <tbody>
            {data.map((row) => (
              <tr key={row.id}>
                <td style={{ ...tdStyle(tc), maxWidth: 220 }}>
                  <span
                    style={{
                      display: "block",
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                      fontWeight: 600,
                    }}
                    title={row.prompt_key}
                  >
                    {row.prompt_key}
                  </span>
                </td>
                <td style={{ ...tdStyle(tc), whiteSpace: "nowrap" }}>v{row.baseline_version}</td>
                <td style={tdStyle(tc)}>
                  <Badge
                    tc={tc}
                    label={row.trigger_source}
                    tone={row.trigger_source === "guideline" ? "info" : "muted"}
                  />
                </td>
                <td style={{ ...tdStyle(tc), minWidth: 220 }}>
                  <span style={{ display: "block", wordBreak: "break-word", color: tc.textSecondary }}>
                    {row.rationale ?? "—"}
                  </span>
                </td>
                <td style={{ ...tdStyle(tc), whiteSpace: "nowrap", color: tc.textMuted }}>
                  {formatDate(row.created_at)}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </SectionShell>
  );
}

"use client";

// WikiShell — orchestratore wiki Confluence-like.
// Layout 3 colonne: WikiTreeNodeView | contenuto/editor/history | WikiSidePanel.
// Riusa MarkdownBlock con anchor heading + wikilink cliccabili. Lo stato
// (selezione, dirty, history) vive qui; i componenti figli sono presentazionali.

import * as React from "react";
import {
  buildTree,
  type WikiDocDetail,
  type WikiDocSummary,
  type WikiScope,
} from "./wiki-scope";
import { MarkdownBlock } from "../chat/markdown-renderer";
import { useThemeColors } from "../../lib/theme";
import { extractHeadings, slugify, type Heading } from "./markdown-wiki-extras";
import { useGlobalDialog } from "../global-dialog-provider";
import type { WikiRevision } from "../../lib/api-client";
import { WikiTreeNodeView } from "./wiki-tree";
import { WikiEditor } from "./wiki-editor";
import { WikiHistory } from "./wiki-history";
import { WikiSidePanel } from "./wiki-side-panel";

interface WikiShellProps {
  scope: WikiScope;
  /** Titolo mostrato nell'header dell'albero. */
  title: string;
  /** Toolbar globale (refresh-all, grafo, ecc.). */
  toolbar?: React.ReactNode;
}

type ViewMode = "view" | "edit" | "history";

export function WikiShell({ scope, title, toolbar }: WikiShellProps) {
  const tc = useThemeColors();
  const dialog = useGlobalDialog();

  // ── Stato ────────────────────────────────────────────────────────────
  const [items, setItems] = React.useState<WikiDocSummary[]>([]);
  const [loadingList, setLoadingList] = React.useState(false);
  const [selectedId, setSelectedId] = React.useState<string | null>(null);
  const [selected, setSelected] = React.useState<WikiDocDetail | null>(null);
  const [loadingDetail, setLoadingDetail] = React.useState(false);
  const [view, setView] = React.useState<ViewMode>("view");
  const [draftBody, setDraftBody] = React.useState("");
  const [draftTitle, setDraftTitle] = React.useState("");
  const [draftTags, setDraftTags] = React.useState("");
  const [saving, setSaving] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [filter, setFilter] = React.useState("");
  const [revisions, setRevisions] = React.useState<WikiRevision[]>([]);
  const [revFrom, setRevFrom] = React.useState<number | null>(null);
  const [revTo, setRevTo] = React.useState<number | null>(null);
  const [diff, setDiff] = React.useState<{
    from: WikiRevision;
    to: WikiRevision;
  } | null>(null);

  // ── Load lista all'avvio (o quando cambia scope) ─────────────────────
  React.useEffect(() => {
    let cancelled = false;
    setLoadingList(true);
    scope
      .list({ limit: 500 })
      .then((r) => {
        if (!cancelled) setItems(r.items);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(asMsg(e));
      })
      .finally(() => {
        if (!cancelled) setLoadingList(false);
      });
    return () => {
      cancelled = true;
    };
  }, [scope]);

  // ── Load dettaglio quando cambia selezione ───────────────────────────
  React.useEffect(() => {
    if (!selectedId) {
      setSelected(null);
      return;
    }
    let cancelled = false;
    setLoadingDetail(true);
    scope
      .get(selectedId)
      .then((d) => {
        if (cancelled) return;
        applyDoc(d);
        setView("view");
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(asMsg(e));
      })
      .finally(() => {
        if (!cancelled) setLoadingDetail(false);
      });
    return () => {
      cancelled = true;
    };
    // applyDoc usa solo setter stabili
  }, [scope, selectedId]);

  const applyDoc = (d: WikiDocDetail) => {
    setSelected(d);
    setDraftBody(d.bodyMd);
    setDraftTitle(d.title);
    setDraftTags(d.tags.join(", "));
  };

  // ── Albero filtrato ──────────────────────────────────────────────────
  const tree = React.useMemo(() => {
    const f = filter.trim().toLowerCase();
    const filtered = f
      ? items.filter(
          (i) =>
            i.title.toLowerCase().includes(f) ||
            (i.path || "").toLowerCase().includes(f),
        )
      : items;
    return buildTree(filtered);
  }, [items, filter]);

  // ── Heading (TOC scroll-spy) ─────────────────────────────────────────
  const headings: Heading[] = React.useMemo(
    () => (selected ? extractHeadings(selected.bodyMd) : []),
    [selected],
  );

  // ── Wikilink: risoluzione tra documenti caricati ─────────────────────
  const onWikiLink = React.useCallback(
    (target: string) => {
      const t = target.toLowerCase();
      const targetSlug = slugify(target);
      const match = items.find(
        (i) =>
          i.title.toLowerCase() === t ||
          slugify(i.title) === targetSlug ||
          (i.path || "").toLowerCase().endsWith(`/${t}.md`),
      );
      if (match) setSelectedId(match.id);
      else setError(`Wikilink non risolto: ${target}`);
    },
    [items],
  );

  // ── Dirty + salvataggio + restore ────────────────────────────────────
  const dirty =
    !!selected &&
    (draftBody !== selected.bodyMd ||
      draftTitle !== selected.title ||
      draftTags !== selected.tags.join(", "));

  const refreshAfterMutation = async (docId: string) => {
    const [d, r] = await Promise.all([scope.get(docId), scope.list({ limit: 500 })]);
    applyDoc(d);
    setItems(r.items);
    return d;
  };

  const onSave = async () => {
    if (!selected || !dirty) return;
    setSaving(true);
    setError(null);
    try {
      const tagList = draftTags
        .split(",")
        .map((t) => t.trim())
        .filter(Boolean);
      await scope.patch(selected.id, {
        title: draftTitle !== selected.title ? draftTitle : undefined,
        body_md: draftBody !== selected.bodyMd ? draftBody : undefined,
        tags: draftTags !== selected.tags.join(", ") ? tagList : undefined,
      });
      await refreshAfterMutation(selected.id);
      setView("view");
    } catch (e: unknown) {
      setError(asMsg(e));
    } finally {
      setSaving(false);
    }
  };

  const onCancelEdit = async () => {
    if (!dirty) {
      setView("view");
      return;
    }
    const ok = await dialog.confirmDialog(
      "Hai modifiche non salvate. Scartare le modifiche?",
    );
    if (!ok || !selected) return;
    applyDoc(selected);
    setView("view");
  };

  const loadHistory = React.useCallback(async () => {
    if (!selected) return;
    try {
      const r = await scope.listRevisions(selected.id);
      setRevisions(r);
      if (r.length >= 2) {
        setRevFrom(r[1].version_no);
        setRevTo(r[0].version_no);
      } else if (r.length === 1) {
        setRevFrom(r[0].version_no);
        setRevTo(r[0].version_no);
      }
    } catch (e: unknown) {
      setError(asMsg(e));
    }
  }, [scope, selected]);

  React.useEffect(() => {
    if (view === "history") void loadHistory();
  }, [view, loadHistory]);

  const loadDiff = async () => {
    if (!selected || revFrom == null || revTo == null) return;
    try {
      const [fromRev, toRev] = await Promise.all([
        scope.getRevision(selected.id, revFrom),
        scope.getRevision(selected.id, revTo),
      ]);
      setDiff({ from: fromRev, to: toRev });
    } catch (e: unknown) {
      setError(asMsg(e));
    }
  };

  const doRestore = async (version: number) => {
    if (!selected) return;
    const ok = await dialog.confirmDialog(
      `Ripristinare la revisione v${version}? Verra' creata una nuova revisione.`,
    );
    if (!ok) return;
    try {
      await scope.restoreRevision(selected.id, version);
      await refreshAfterMutation(selected.id);
      await loadHistory();
      setView("view");
    } catch (e: unknown) {
      setError(asMsg(e));
    }
  };

  // ── Render ────────────────────────────────────────────────────────────
  return (
    <div
      style={{
        display: "flex",
        height: "calc(100vh - 90px)",
        background: tc.bg,
        color: tc.text,
        borderRadius: 8,
        overflow: "hidden",
        border: `1px solid ${tc.border}`,
      }}
    >
      {/* Colonna 1: albero */}
      <aside
        style={{
          width: 300,
          minWidth: 240,
          borderRight: `1px solid ${tc.border}`,
          background: tc.bgCard,
          display: "flex",
          flexDirection: "column",
        }}
      >
        <div style={{ padding: "12px 14px", borderBottom: `1px solid ${tc.border}` }}>
          <div style={{ fontWeight: 700, fontSize: 14, marginBottom: 8 }}>
            {title}
          </div>
          <input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="Filtra documenti..."
            style={{
              width: "100%",
              padding: "6px 8px",
              background: tc.bgInput,
              border: `1px solid ${tc.border}`,
              borderRadius: 4,
              color: tc.text,
              fontSize: 12,
              boxSizing: "border-box",
            }}
          />
          <div style={{ fontSize: 11, color: tc.textSecondary, marginTop: 6 }}>
            {items.length} documenti
          </div>
        </div>
        <div style={{ flex: 1, overflowY: "auto", padding: "6px 0" }}>
          {loadingList ? (
            <div style={{ padding: 12, color: tc.textSecondary }}>Caricamento...</div>
          ) : (
            <WikiTreeNodeView
              node={tree}
              depth={0}
              selectedId={selectedId}
              onSelect={(id) => setSelectedId(id)}
              defaultOpen
            />
          )}
        </div>
      </aside>

      {/* Colonna 2: contenuto */}
      <main style={{ flex: 1, minWidth: 0, display: "flex", flexDirection: "column" }}>
        <ContentHeader
          selected={selected}
          view={view}
          dirty={dirty}
          saving={saving}
          onEdit={() => setView("edit")}
          onHistory={() => setView("history")}
          onSave={onSave}
          onCancelEdit={onCancelEdit}
          onCloseHistory={() => setView("view")}
          toolbar={toolbar}
        />
        {error && (
          <ErrorBar message={error} onClose={() => setError(null)} />
        )}
        <section
          style={{
            flex: 1,
            overflowY: "auto",
            padding: view === "edit" ? 0 : "16px 24px",
          }}
        >
          {loadingDetail && (
            <div style={{ color: tc.textSecondary }}>Caricamento...</div>
          )}
          {!selected && !loadingDetail && (
            <div style={{ color: tc.textSecondary }}>
              Seleziona un documento dall&apos;albero a sinistra per visualizzarlo.
            </div>
          )}
          {selected && view === "view" && !loadingDetail && (
            <DocViewer doc={selected} onWikiLink={onWikiLink} />
          )}
          {selected && view === "edit" && (
            <WikiEditor
              draftTitle={draftTitle}
              setDraftTitle={setDraftTitle}
              draftTags={draftTags}
              setDraftTags={setDraftTags}
              draftBody={draftBody}
              setDraftBody={setDraftBody}
              onWikiLink={onWikiLink}
            />
          )}
          {selected && view === "history" && (
            <WikiHistory
              revisions={revisions}
              revFrom={revFrom}
              revTo={revTo}
              diff={diff}
              setRevFrom={setRevFrom}
              setRevTo={setRevTo}
              onCompare={loadDiff}
              onRestore={doRestore}
            />
          )}
        </section>
      </main>

      {/* Colonna 3: TOC + backlinks */}
      <WikiSidePanel
        headings={headings}
        backlinks={selected?.incoming ?? []}
        outgoing={selected?.outgoing ?? []}
        onNavigate={(id) => setSelectedId(id)}
      />
    </div>
  );
}

// ────────────────────────── Componenti interni ────────────────────────────

function ContentHeader({
  selected,
  view,
  dirty,
  saving,
  onEdit,
  onHistory,
  onSave,
  onCancelEdit,
  onCloseHistory,
  toolbar,
}: {
  selected: WikiDocDetail | null;
  view: ViewMode;
  dirty: boolean;
  saving: boolean;
  onEdit: () => void;
  onHistory: () => void;
  onSave: () => void;
  onCancelEdit: () => void;
  onCloseHistory: () => void;
  toolbar?: React.ReactNode;
}) {
  const tc = useThemeColors();
  return (
    <header
      style={{
        padding: "10px 14px",
        borderBottom: `1px solid ${tc.border}`,
        background: tc.bgCard,
        display: "flex",
        alignItems: "center",
        gap: 10,
        flexWrap: "wrap",
      }}
    >
      {selected ? (
        <>
          <div style={{ fontSize: 12, color: tc.textSecondary }}>
            {selected.path || "(senza path)"}
          </div>
          <div style={{ marginLeft: "auto", display: "flex", gap: 8 }}>
            {view === "view" && (
              <>
                <ToolbarBtn onClick={onEdit}>Modifica</ToolbarBtn>
                <ToolbarBtn onClick={onHistory}>Cronologia</ToolbarBtn>
              </>
            )}
            {view === "edit" && (
              <>
                <ToolbarBtn onClick={onSave} disabled={!dirty || saving} primary>
                  {saving ? "Salvataggio..." : dirty ? "Salva" : "Salvato"}
                </ToolbarBtn>
                <ToolbarBtn onClick={onCancelEdit} disabled={saving}>
                  Annulla
                </ToolbarBtn>
              </>
            )}
            {view === "history" && (
              <ToolbarBtn onClick={onCloseHistory}>Chiudi</ToolbarBtn>
            )}
          </div>
        </>
      ) : (
        <div style={{ color: tc.textSecondary }}>
          Seleziona un documento dall&apos;albero
        </div>
      )}
      {toolbar && <div style={{ width: "100%" }}>{toolbar}</div>}
    </header>
  );
}

function DocViewer({
  doc,
  onWikiLink,
}: {
  doc: WikiDocDetail;
  onWikiLink: (target: string) => void;
}) {
  const tc = useThemeColors();
  return (
    <div>
      <h1 style={{ margin: "0 0 8px", fontSize: 24, fontWeight: 700 }}>
        {doc.title}
      </h1>
      <div
        style={{
          fontSize: 11,
          color: tc.textSecondary,
          marginBottom: 16,
          display: "flex",
          gap: 12,
          flexWrap: "wrap",
        }}
      >
        <span>Categoria: {doc.category}</span>
        <span>Modificato: {new Date(doc.updatedAt).toLocaleString()}</span>
        {doc.autoGenerated && (
          <span
            style={{
              padding: "1px 8px",
              background: tc.bgInput,
              border: `1px solid ${tc.border}`,
              borderRadius: 10,
            }}
            title="Generato dai generatori automatici. Le modifiche manuali sono protette dalla rigenerazione."
          >
            auto-generato
          </span>
        )}
        {doc.tags.map((t) => (
          <span
            key={t}
            style={{
              padding: "1px 8px",
              background: tc.accent + "20",
              color: tc.accent,
              borderRadius: 10,
            }}
          >
            #{t}
          </span>
        ))}
      </div>
      <MarkdownBlock
        content={doc.bodyMd}
        skipNormalize
        enableHeadingAnchors
        slugPrefix="wiki-"
        onWikiLink={onWikiLink}
      />
    </div>
  );
}

function ToolbarBtn({
  children,
  onClick,
  disabled,
  primary,
}: {
  children: React.ReactNode;
  onClick: () => void;
  disabled?: boolean;
  primary?: boolean;
}) {
  const tc = useThemeColors();
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      style={{
        padding: "4px 12px",
        background: primary ? tc.accent : tc.bgInput,
        color: primary ? "#fff" : tc.text,
        border: `1px solid ${primary ? tc.accent : tc.border}`,
        borderRadius: 4,
        cursor: disabled ? "not-allowed" : "pointer",
        opacity: disabled ? 0.5 : 1,
        fontSize: 12,
      }}
    >
      {children}
    </button>
  );
}

function ErrorBar({ message, onClose }: { message: string; onClose: () => void }) {
  const tc = useThemeColors();
  return (
    <div
      style={{
        padding: "8px 14px",
        color: tc.error,
        fontSize: 12,
        borderBottom: `1px solid ${tc.border}`,
      }}
    >
      {message}{" "}
      <button
        onClick={onClose}
        style={{
          marginLeft: 8,
          background: "none",
          border: "none",
          color: tc.accent,
          cursor: "pointer",
        }}
      >
        chiudi
      </button>
    </div>
  );
}

function asMsg(e: unknown): string {
  return String((e as Error)?.message ?? e);
}

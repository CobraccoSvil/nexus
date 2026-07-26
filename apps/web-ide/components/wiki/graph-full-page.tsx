"use client";

// GraphFullPage — visualizza la knowledge graph (nodi=wiki_docs, archi=wiki_links)
// a piena pagina (non modale). Riusa Cytoscape + cose-bilkent come la vecchia
// KnowledgeGraph ma si appoggia sugli endpoint unificati /api/wiki/* (ADR 0017
// v2 fase 7). Permette filtri di confidence/predicate, drill-down su nodo,
// doppio click per aprire il doc, ricerca per titolo.

import * as React from "react";
import cytoscape, { type Core, type ElementDefinition } from "cytoscape";
// @ts-expect-error cytoscape-cose-bilkent non ha tipi TS
import coseBilkent from "cytoscape-cose-bilkent";
import {
  getGraph,
  type WikiGraphData,
  type WikiGraphNode,
  type WikiScope,
} from "../../lib/wiki-client";
import { useThemeColors } from "../../lib/theme";
import { useI18n } from "../../lib/i18n";
import { AutoWidthSelect } from "../auto-width-select";

cytoscape.use(coseBilkent);

interface Props {
  scope: WikiScope;
  projectId?: string;
  onOpenDoc?: (docId: string) => void;
}

const KIND_COLOR: Record<string, string> = {
  architecture: "#6366f1",
  adr: "#dc2626",
  api: "#0ea5e9",
  schema: "#7c3aed",
  runbook: "#16a34a",
  changelog: "#f59e0b",
  decision: "#db2777",
  concept: "#0d9488",
  note: "#737373",
  other: "#a3a3a3",
};

const LAYOUTS = ["cose-bilkent", "breadthfirst", "concentric", "grid"] as const;
type LayoutName = (typeof LAYOUTS)[number];

export function GraphFullPage({ scope, projectId, onOpenDoc }: Props) {
  const tc = useThemeColors();
  const { t } = useI18n();
  const containerRef = React.useRef<HTMLDivElement | null>(null);
  const cyRef = React.useRef<Core | null>(null);

  const [data, setData] = React.useState<WikiGraphData | null>(null);
  const [loading, setLoading] = React.useState(false);
  const [error, setError] = React.useState<string | null>(null);
  const [minConfidence, setMinConfidence] = React.useState(0);
  const [hideAuto, setHideAuto] = React.useState(false);
  const [predicate, setPredicate] = React.useState("");
  const [layout, setLayout] = React.useState<LayoutName>("cose-bilkent");
  const [searchTerm, setSearchTerm] = React.useState("");
  const [centerDocId, setCenterDocId] = React.useState<string | null>(null);
  const [maxHops, setMaxHops] = React.useState(2);

  const load = React.useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const d = await getGraph(scope, projectId, {
        min_confidence: minConfidence > 0 ? minConfidence : undefined,
        predicate: predicate || undefined,
        hide_auto_links: hideAuto || undefined,
        center_doc_id: centerDocId ?? undefined,
        max_hops: centerDocId ? maxHops : undefined,
      });
      setData(d);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [scope, projectId, minConfidence, predicate, hideAuto, centerDocId, maxHops]);

  React.useEffect(() => {
    void load();
  }, [load]);

  React.useEffect(() => {
    if (!data || !containerRef.current) return;

    const elements: ElementDefinition[] = [];
    for (const n of data.nodes) {
      elements.push({
        data: {
          id: n.id,
          label: n.title.length > 35 ? n.title.slice(0, 32) + "..." : n.title,
          fullTitle: n.title,
          color: KIND_COLOR[n.kind] ?? "#737373",
          kind: n.kind,
          scope: n.scope,
        },
      });
    }
    for (const e of data.edges) {
      if (hideAuto && e.created_by === "auto") continue;
      elements.push({
        data: {
          id: `${e.from}__${e.to}__${e.rel_type}`,
          source: e.from,
          target: e.to,
          label: e.rel_type,
          edgeColor: e.created_by === "auto" ? "#a3a3a3" : "#171717",
          edgeWidth: 1 + e.confidence * 2.5,
        },
      });
    }

    if (cyRef.current) cyRef.current.destroy();
    cyRef.current = cytoscape({
      container: containerRef.current,
      elements,
      style: [
        {
          selector: "node",
          style: {
            "background-color": "data(color)",
            label: "data(label)",
            color: tc.text,
            "font-size": 10,
            "text-valign": "bottom",
            "text-halign": "center",
            "text-margin-y": 4,
            width: 16,
            height: 16,
            "border-width": 1.5,
            "border-color": tc.bg,
          },
        },
        {
          selector: "node:selected",
          style: {
            "border-color": "#dc2626",
            "border-width": 3,
            width: 22,
            height: 22,
          },
        },
        {
          selector: "edge",
          style: {
            width: "data(edgeWidth)" as unknown as number,
            "line-color": "data(edgeColor)",
            "target-arrow-color": "data(edgeColor)",
            "target-arrow-shape": "triangle",
            "curve-style": "bezier",
            opacity: 0.7,
            "font-size": 8,
            color: tc.textSecondary,
            "text-rotation": "autorotate",
          },
        },
      ],
      layout: {
        name: layout,
        animate: false,
        randomize: false,
        nodeDimensionsIncludeLabels: true,
        idealEdgeLength: 80,
        nodeRepulsion: 4500,
        numIter: 2500,
        tile: true,
        fit: true,
        padding: 30,
      } as unknown as cytoscape.LayoutOptions,
      minZoom: 0.1,
      maxZoom: 4.0,
      wheelSensitivity: 1,
      userZoomingEnabled: true,
      userPanningEnabled: true,
      boxSelectionEnabled: false,
    });

    const cy = cyRef.current;
    cy?.one("layoutstop", () => {
      cy.stop(true, true);
      cy.fit(undefined, 40);
    });

    // Single tap: centra drill-down. Double tap: apri doc.
    let lastTap = 0;
    let lastNode = "";
    cy?.on("tap", "node", (evt) => {
      const id = evt.target.id();
      const now = Date.now();
      if (id === lastNode && now - lastTap < 350) {
        onOpenDoc?.(id);
      } else {
        setCenterDocId(id);
      }
      lastNode = id;
      lastTap = now;
    });

    return () => {
      cyRef.current?.destroy();
      cyRef.current = null;
    };
  }, [data, hideAuto, layout, onOpenDoc, tc]);

  // Search → centra il viewport su un nodo cui titolo matcha
  const doSearch = () => {
    if (!cyRef.current || !searchTerm.trim()) return;
    const term = searchTerm.trim().toLowerCase();
    const match = (data?.nodes ?? []).find((n: WikiGraphNode) =>
      n.title.toLowerCase().includes(term),
    );
    if (match) {
      const node = cyRef.current.getElementById(match.id);
      if (node && node.length > 0) {
        cyRef.current.center(node);
        cyRef.current.zoom({ level: 2, position: node.position() });
        node.select();
      }
    }
  };

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
      {/* Toolbar */}
      <div
        style={{
          padding: "8px 14px",
          borderBottom: `1px solid ${tc.border}`,
          background: tc.bgCard,
          display: "flex",
          gap: 10,
          alignItems: "center",
          flexWrap: "wrap",
          fontSize: 12,
        }}
      >
        {centerDocId && (
          <button
            type="button"
            onClick={() => setCenterDocId(null)}
            style={{
              padding: "4px 10px",
              background: tc.bgInput,
              border: `1px solid ${tc.border}`,
              borderRadius: 4,
              color: tc.text,
              cursor: "pointer",
              fontSize: 11,
            }}
          >
            ← {t("wiki.graph.show_all")}
          </button>
        )}
        {centerDocId && (
          <label style={{ display: "flex", alignItems: "center", gap: 6 }}>
            {t("wiki.graph.hops")}:
            <input
              type="number"
              min={1}
              max={5}
              value={maxHops}
              onChange={(e) => setMaxHops(Number(e.target.value))}
              style={{
                width: 50,
                padding: "3px 6px",
                background: tc.bgInput,
                color: tc.text,
                border: `1px solid ${tc.border}`,
                borderRadius: 4,
              }}
            />
          </label>
        )}
        <label style={{ display: "flex", alignItems: "center", gap: 6 }}>
          {t("wiki.graph.filter_confidence")}:
          <input
            type="range"
            min={0}
            max={1}
            step={0.05}
            value={minConfidence}
            onChange={(e) => setMinConfidence(Number(e.target.value))}
            style={{ width: 100 }}
          />
          <span style={{ minWidth: 30 }}>{minConfidence.toFixed(2)}</span>
        </label>
        <label style={{ display: "flex", alignItems: "center", gap: 4 }}>
          <input
            type="checkbox"
            checked={hideAuto}
            onChange={(e) => setHideAuto(e.target.checked)}
          />
          {t("wiki.graph.hide_auto")}
        </label>
        <label style={{ display: "flex", alignItems: "center", gap: 6 }}>
          {t("wiki.graph.layout")}:
          <AutoWidthSelect
            value={layout}
            options={LAYOUTS.map((l) => ({ value: l, label: l }))}
            onChange={(value) => setLayout(value as LayoutName)}
            style={{
              padding: "3px 6px",
              background: tc.bgInput,
              color: tc.text,
              border: `1px solid ${tc.border}`,
              borderRadius: 4,
              fontSize: 11,
            }}
          />
        </label>
        <AutoWidthSelect
          value={predicate}
          options={[
            { value: "", label: `— ${t("wiki.graph.all_predicates")} —` },
            ...[
              "relates",
              "supersedes",
              "depends_on",
              "mentions",
              "implements",
              "tests",
              "blocks",
              "duplicate_of",
            ].map((p) => ({ value: p, label: p })),
          ]}
          onChange={(value) => setPredicate(value)}
          style={{
            padding: "3px 6px",
            background: tc.bgInput,
            color: tc.text,
            border: `1px solid ${tc.border}`,
            borderRadius: 4,
            fontSize: 11,
          }}
        />
        <div style={{ flex: 1, minWidth: 0 }} />
        <input
          value={searchTerm}
          onChange={(e) => setSearchTerm(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") doSearch();
          }}
          placeholder={t("wiki.graph.search_node")}
          style={{
            padding: "4px 8px",
            background: tc.bgInput,
            color: tc.text,
            border: `1px solid ${tc.border}`,
            borderRadius: 4,
            fontSize: 11,
            width: 200,
          }}
        />
        <button
          type="button"
          onClick={doSearch}
          style={{
            padding: "4px 10px",
            background: tc.accent,
            color: "#fff",
            border: "none",
            borderRadius: 4,
            cursor: "pointer",
            fontSize: 11,
          }}
        >
          {t("wiki.graph.find")}
        </button>
        <button
          type="button"
          onClick={() => void load()}
          style={{
            padding: "4px 10px",
            background: tc.bgInput,
            color: tc.text,
            border: `1px solid ${tc.border}`,
            borderRadius: 4,
            cursor: "pointer",
            fontSize: 11,
          }}
        >
          {t("wiki.refresh")}
        </button>
      </div>

      {/* Stats */}
      <div
        style={{
          padding: "4px 14px",
          fontSize: 11,
          color: tc.textSecondary,
          borderBottom: `1px solid ${tc.border}`,
          background: tc.bg,
        }}
      >
        {data
          ? `${data.nodes.length} ${t("wiki.graph.nodes")} · ${data.edges.length} ${t("wiki.graph.edges")}`
          : t("wiki.loading")}
      </div>

      {error && (
        <div style={{ padding: "6px 14px", color: tc.error, fontSize: 12 }}>{error}</div>
      )}

      {/* Canvas */}
      <div style={{ flex: 1, position: "relative", overflow: "hidden", minHeight: 0 }}>
        {loading && !data && (
          <div
            style={{
              position: "absolute",
              inset: 0,
              display: "grid",
              placeItems: "center",
              color: tc.textSecondary,
            }}
          >
            {t("wiki.loading")}
          </div>
        )}
        {!loading && data && data.nodes.length === 0 && (
          <div
            style={{
              position: "absolute",
              inset: 0,
              display: "grid",
              placeItems: "center",
              color: tc.textSecondary,
              fontSize: 13,
            }}
          >
            {t("wiki.graph.empty")}
          </div>
        )}
        <div ref={containerRef} style={{ width: "100%", height: "100%", background: tc.bg }} />
      </div>
    </div>
  );
}

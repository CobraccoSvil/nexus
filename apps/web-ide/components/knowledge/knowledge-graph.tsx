"use client";

/**
 * KnowledgeGraph — visualizzazione network di un vault (KB o meta-docs) via Cytoscape.
 *
 * Riusa lo stesso componente per entrambi i contesti:
 *   - `mode="project"` + `projectId` → grafo knowledge per-progetto (status/intent badges)
 *   - `mode="meta"`                  → grafo meta-vault Nexus (kind badges)
 *
 * Il graph e' renderizzato in un dialog modale fullscreen perche' una sidebar
 * stretta (200-300px) non da' spazio sufficiente per layout force-directed.
 */

import { useEffect, useRef, useState, useCallback } from "react";
import cytoscape, { type Core, type ElementDefinition } from "cytoscape";
// @ts-expect-error - cytoscape-cose-bilkent non ha tipi TS
import coseBilkent from "cytoscape-cose-bilkent";
import {
  getKnowledgeGraph,
  getMetaDocsGraph,
  type KnowledgeGraphData,
  type MetaDocsGraphData,
} from "../../lib/api-client";
import { useI18n } from "../../lib/i18n";

cytoscape.use(coseBilkent);

interface CommonProps {
  open: boolean;
  onClose: () => void;
  onNodeClick?: (nodeId: string) => void;
}

type Props =
  | (CommonProps & { mode: "project"; projectId: string })
  | (CommonProps & { mode: "meta" });

const KIND_COLOR: Record<string, string> = {
  architecture: "#6366f1",
  adr: "#dc2626",
  api: "#0ea5e9",
  schema: "#7c3aed",
  runbook: "#16a34a",
  changelog: "#f59e0b",
  decision: "#db2777",
  other: "#737373",
};

const INTENT_COLOR: Record<string, string> = {
  fix: "#dc2626",
  feature: "#16a34a",
  refactor: "#0ea5e9",
  docs: "#f59e0b",
  test: "#7c3aed",
  chat: "#737373",
  architecture: "#6366f1",
  database_schema_change: "#db2777",
};

export function KnowledgeGraph(props: Props) {
  const { t } = useI18n();
  const containerRef = useRef<HTMLDivElement | null>(null);
  const cyRef = useRef<Core | null>(null);
  const [data, setData] = useState<KnowledgeGraphData | MetaDocsGraphData | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hideAutoLinks, setHideAutoLinks] = useState(false);
  const [minConfidence, setMinConfidence] = useState(0);

  const loadData = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      if (props.mode === "project") {
        const d = await getKnowledgeGraph(props.projectId, {
          min_confidence: minConfidence,
        });
        setData(d);
      } else {
        const d = await getMetaDocsGraph();
        setData(d);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [props, minConfidence]);

  useEffect(() => {
    if (!props.open) return;
    void loadData();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.open]);

  // Estraggo i campi rilevanti come variabili stabili: usare `props` come
  // dep dell'effect causa rebuild ad ogni render (identita' nuova),
  // distruggendo lo stato del graph (zoom/pan reset continuo).
  const mode = props.mode;
  const onNodeClickProp = props.onNodeClick;
  const open = props.open;

  useEffect(() => {
    if (!open) return;
    if (!data) return;
    if (!containerRef.current) return;

    // Costruisci elements Cytoscape
    const elements: ElementDefinition[] = [];

    for (const n of data.nodes) {
      const color =
        mode === "meta"
          ? KIND_COLOR[(n as MetaDocsGraphData["nodes"][number]).kind] ?? "#737373"
          : INTENT_COLOR[
              (n as KnowledgeGraphData["nodes"][number]).intent ?? "chat"
            ] ?? "#737373";
      const subtitle =
        mode === "meta"
          ? (n as MetaDocsGraphData["nodes"][number]).kind
          : (n as KnowledgeGraphData["nodes"][number]).intent ?? "";
      elements.push({
        data: {
          id: n.id,
          label: n.title.length > 35 ? n.title.slice(0, 32) + "..." : n.title,
          fullTitle: n.title,
          color,
          subtitle,
        },
      });
    }

    for (const e of data.edges) {
      if (hideAutoLinks && e.created_by === "auto") continue;
      elements.push({
        data: {
          id: e.id,
          source: e.from,
          target: e.to,
          label: e.rel_type,
          confidence: e.confidence,
          edgeColor: e.created_by === "auto" ? "#a3a3a3" : "#171717",
          edgeWidth: 1 + e.confidence * 2.5,
        },
      });
    }

    if (cyRef.current) {
      cyRef.current.destroy();
    }

    cyRef.current = cytoscape({
      container: containerRef.current,
      elements,
      style: [
        {
          selector: "node",
          style: {
            "background-color": "data(color)",
            label: "data(label)",
            color: "#171717",
            "font-size": 10,
            "text-valign": "bottom",
            "text-halign": "center",
            "text-margin-y": 4,
            width: 14,
            height: 14,
            "border-width": 1.5,
            "border-color": "#fff",
          },
        },
        {
          selector: "node:selected",
          style: {
            "border-color": "#dc2626",
            "border-width": 3,
            width: 18,
            height: 18,
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
            color: "#525252",
            "text-rotation": "autorotate",
          },
        },
        {
          selector: "edge:selected",
          style: {
            "line-color": "#dc2626",
            "target-arrow-color": "#dc2626",
            opacity: 1,
          },
        },
      ],
      layout: {
        name: "cose-bilkent",
        animate: false,
        animationDuration: 0,
        randomize: false,
        nodeDimensionsIncludeLabels: true,
        idealEdgeLength: 80,
        nodeRepulsion: 4500,
        numIter: 2500,
        tile: true,
        fit: true,
        padding: 30,
      } as unknown as cytoscape.LayoutOptions,
      // Interazioni utente: zoom rotella, pan trascinamento, drag nodi.
      // wheelSensitivity 1 = default Cytoscape (zoom percepibile).
      minZoom: 0.1,
      maxZoom: 4.0,
      wheelSensitivity: 1,
      userZoomingEnabled: true,
      userPanningEnabled: true,
      boxSelectionEnabled: false,
      autoungrabify: false,
      autounselectify: false,
    });

    // Stop solo animazioni residue del layout iniziale (NON disabilita zoom/pan).
    const cy = cyRef.current;
    if (cy) {
      cy.one("layoutstop", () => {
        cy.stop(true, true);
        cy.nodes().forEach((n) => {
          n.stop(true);
        });
        cy.fit(undefined, 40);
      });
    }

    if (onNodeClickProp) {
      cyRef.current.on("tap", "node", (evt) => {
        const id = evt.target.id();
        onNodeClickProp(id);
      });
    }

    return () => {
      cyRef.current?.destroy();
      cyRef.current = null;
    };
  }, [data, hideAutoLinks, open, mode, onNodeClickProp]);

  if (!props.open) return null;

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0, 0, 0, 0.6)",
        zIndex: 9999,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: 40,
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) props.onClose();
      }}
    >
      <div
        style={{
          background: "#fff",
          borderRadius: 12,
          width: "100%",
          maxWidth: 1400,
          height: "100%",
          maxHeight: 900,
          display: "flex",
          flexDirection: "column",
          overflow: "hidden",
          boxShadow: "0 25px 50px -12px rgba(0, 0, 0, 0.5)",
        }}
      >
        <div
          style={{
            padding: "12px 16px",
            borderBottom: "1px solid #e5e5e5",
            display: "flex",
            alignItems: "center",
            gap: 12,
            flexWrap: "wrap",
          }}
        >
          <h2 style={{ margin: 0, fontSize: 16, fontWeight: 700, flex: 1 }}>
            {props.mode === "meta" ? "Grafo meta-vault Nexus" : "Grafo knowledge progetto"}
          </h2>

          <label style={{ fontSize: 12, color: "#525252", display: "flex", alignItems: "center", gap: 4 }}>
            <input
              type="checkbox"
              checked={hideAutoLinks}
              onChange={(e) => setHideAutoLinks(e.target.checked)}
            />
            {t("knowledge.nascondiLinkAutomatici")}
          </label>

          {props.mode === "project" && (
            <label style={{ fontSize: 12, color: "#525252", display: "flex", alignItems: "center", gap: 4 }}>
              {t("knowledge.confidenceMin")}
              <input
                type="range"
                min={0}
                max={1}
                step={0.05}
                value={minConfidence}
                onChange={(e) => setMinConfidence(Number(e.target.value))}
                style={{ width: 80 }}
              />
              <span style={{ minWidth: 30 }}>{minConfidence.toFixed(2)}</span>
            </label>
          )}

          <button
            onClick={loadData}
            style={{
              padding: "5px 10px",
              fontSize: 11,
              background: "transparent",
              border: "1px solid #d4d4d4",
              borderRadius: 6,
              cursor: "pointer",
            }}
          >
            {t("knowledge.aggiorna")}
          </button>
          <button
            onClick={props.onClose}
            style={{
              padding: "5px 10px",
              fontSize: 11,
              background: "#171717",
              color: "#fff",
              border: "none",
              borderRadius: 6,
              cursor: "pointer",
            }}
          >
            {t("knowledge.chiudi")}
          </button>
        </div>

        {error && (
          <div
            style={{
              padding: 12,
              background: "#fef2f2",
              color: "#dc2626",
              borderBottom: "1px solid #fecaca",
              fontSize: 12,
            }}
          >
            {error}
          </div>
        )}

        <div style={{ flex: 1, position: "relative", overflow: "hidden" }}>
          {loading && !data && (
            <div style={{ padding: 32, textAlign: "center", color: "#a3a3a3", fontSize: 13 }}>
              {t("knowledge.caricamentoGrafo")}
            </div>
          )}
          {!loading && data && data.nodes.length === 0 && (
            <div style={{ padding: 32, textAlign: "center", color: "#a3a3a3", fontSize: 13 }}>
              {t("knowledge.nessunaNotaNelVault")}
            </div>
          )}
          <div
            ref={containerRef}
            style={{ width: "100%", height: "100%", background: "#fafafa" }}
          />
        </div>

        {data && (
          <div
            style={{
              padding: "6px 16px",
              borderTop: "1px solid #e5e5e5",
              fontSize: 11,
              color: "#737373",
              background: "#fafafa",
              display: "flex",
              gap: 16,
            }}
          >
            <span>Nodi: {data.stats.nodes_count}</span>
            <span>Edge: {data.stats.edges_count}</span>
            <span style={{ marginLeft: "auto" }}>
              {t("knowledge.scrollZoomDragPan")}
            </span>
          </div>
        )}
      </div>
    </div>
  );
}

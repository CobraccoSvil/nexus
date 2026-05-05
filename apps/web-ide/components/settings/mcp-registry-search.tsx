"use client";

import { useEffect, useState } from "react";
import type { useThemeColors } from "../../lib/theme";
import type { McpServer } from "../../lib/api-client";
import { MCP_CATALOG, CATALOG_CATEGORIES, type CatalogEntry, type CatalogCategory } from "./mcp-catalog-data";

interface McpRegistrySearchProps {
  tc: ReturnType<typeof useThemeColors>;
  onAddEntry: (entry: CatalogEntry) => void;
  existingServers: McpServer[];
}

function mapRegistryEntry(item: Record<string, unknown>): CatalogEntry | null {
  try {
    const id = (item.id as string) || (item.name as string);
    if (!id) return null;
    // skip if already in embedded catalog
    return {
      id,
      name: (item.display_name as string) || (item.name as string) || id,
      description: (item.description as string) || "",
      category: "dev-tools" as CatalogCategory,
      icon: "🔧",
      transport: "stdio",
      command: (item.package as Record<string, unknown>)?.registry === "npm"
        ? "npx"
        : undefined,
      args: (item.package as Record<string, unknown>)?.name
        ? ["-y", (item.package as Record<string, unknown>).name as string]
        : undefined,
      tags: [],
    };
  } catch {
    return null;
  }
}

function CatalogCard({
  entry,
  tc,
  onAdd,
  alreadyAdded,
}: {
  entry: CatalogEntry;
  tc: ReturnType<typeof useThemeColors>;
  onAdd: () => void;
  alreadyAdded: boolean;
}) {
  return (
    <div
      style={{
        background: alreadyAdded ? "#f59e0b12" : tc.bgCard,
        border: `1px solid ${alreadyAdded ? "#f59e0b66" : tc.border}`,
        borderRadius: 10,
        padding: "14px 16px",
        display: "flex",
        flexDirection: "column",
        gap: 8,
        position: "relative",
      }}
    >
      {/* Header row */}
      <div className="flex-row-gap-8">
        <span style={{ fontSize: 20 }}>{entry.icon}</span>
        <span style={{ fontWeight: 600, fontSize: 13, flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          {entry.name}
        </span>
        <span
          style={{
            fontSize: 10,
            padding: "2px 6px",
            borderRadius: 4,
            background: entry.transport === "http" ? "#1d4ed820" : "#15803d20",
            color: entry.transport === "http" ? "#3b82f6" : "#22c55e",
            border: `1px solid ${entry.transport === "http" ? "#3b82f640" : "#22c55e40"}`,
            fontWeight: 600,
            flexShrink: 0,
          }}
        >
          {entry.transport.toUpperCase()}
        </span>
        {entry.official && (
          <span
            style={{
              fontSize: 10,
              padding: "2px 6px",
              borderRadius: 4,
              background: "#78350f20",
              color: "#f59e0b",
              border: "1px solid #f59e0b40",
              fontWeight: 600,
              flexShrink: 0,
            }}
          >
            ✓ Official
          </span>
        )}
        {alreadyAdded && (
          <span
            style={{
              fontSize: 10,
              padding: "2px 6px",
              borderRadius: 999,
              background: "#f59e0b20",
              color: "#b45309",
              border: "1px solid #f59e0b66",
              fontWeight: 700,
              flexShrink: 0,
              textTransform: "uppercase",
              letterSpacing: "0.02em",
            }}
            title="Server MCP gia presente negli installati"
          >
            Già presente
          </span>
        )}
      </div>

      {/* Description */}
      <p
        style={{
          fontSize: 12,
          color: tc.textMuted,
          margin: 0,
          overflow: "hidden",
          display: "-webkit-box",
          WebkitBoxOrient: "vertical",
          WebkitLineClamp: 2,
          lineHeight: "1.5",
        }}
      >
        {entry.description}
      </p>

      {/* Required env vars */}
      {entry.requiredEnvVars && entry.requiredEnvVars.length > 0 && (
        <div style={{ display: "flex", flexWrap: "wrap", gap: 4 }}>
          {entry.requiredEnvVars.map((k) => (
            <span
              key={k}
              style={{
                fontSize: 10,
                padding: "1px 5px",
                borderRadius: 3,
                background: tc.bgInput,
                color: tc.textMuted,
                fontFamily: "monospace",
                border: `1px solid ${tc.border}`,
              }}
            >
              {k}
            </span>
          ))}
        </div>
      )}

      {/* Footer row */}
      <div style={{ display: "flex", alignItems: "center", justifyContent: "flex-end", marginTop: 2 }}>
        {entry.docsUrl && (
          <a
            href={entry.docsUrl}
            target="_blank"
            rel="noopener noreferrer"
            style={{ fontSize: 11, color: tc.textMuted, textDecoration: "none", marginRight: "auto" }}
          >
            📖 Docs
          </a>
        )}
        <button
          onClick={onAdd}
          disabled={alreadyAdded}
          style={{
            fontSize: 12,
            fontWeight: 600,
            padding: "4px 12px",
            borderRadius: 6,
            border: `1px solid ${alreadyAdded ? tc.border : tc.accent}`,
            background: alreadyAdded ? tc.bgInput : `${tc.accent}15`,
            color: alreadyAdded ? tc.textMuted : tc.accent,
            cursor: alreadyAdded ? "not-allowed" : "pointer",
            opacity: alreadyAdded ? 0.7 : 1,
          }}
        >
          {alreadyAdded ? "Gia aggiunto" : "＋ Aggiungi"}
        </button>
      </div>
    </div>
  );
}

function normalizeLabel(value: string) {
  return value.trim().toLowerCase();
}

export function McpRegistrySearch({ tc, onAddEntry, existingServers }: McpRegistrySearchProps) {
  const [query, setQuery] = useState("");
  const [selectedCategory, setSelectedCategory] = useState<CatalogCategory | "all">("all");
  const [registryEntries, setRegistryEntries] = useState<CatalogEntry[]>([]);
  const [registryLoaded, setRegistryLoaded] = useState(false);

  useEffect(() => {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 5000);

    fetch("https://registry.modelcontextprotocol.io/v0.1/servers?limit=96&version=latest", {
      signal: controller.signal,
    })
      .then((r) => r.json())
      .then((data) => {
        clearTimeout(timer);
        const servers: Record<string, unknown>[] = Array.isArray(data?.servers) ? data.servers : [];
        const mapped = servers
          .map(mapRegistryEntry)
          .filter((e): e is CatalogEntry => e !== null && !MCP_CATALOG.some((c) => c.id === e.id));
        setRegistryEntries(mapped);
        setRegistryLoaded(true);
      })
      .catch(() => {
        clearTimeout(timer);
        // silent fallback — embedded catalog is enough
      });

    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, []);

  const allEntries = [...MCP_CATALOG, ...registryEntries];
  const existingKeys = new Set(
    existingServers.map((server) => `${normalizeLabel(server.name)}::${server.transport}`),
  );
  const filtered = allEntries.filter((entry) => {
    const matchCat = selectedCategory === "all" || entry.category === selectedCategory;
    const q = query.toLowerCase();
    const matchQ =
      !q ||
      [entry.name, entry.description, ...(entry.tags ?? [])].some((s) =>
        s.toLowerCase().includes(q),
      );
    return matchCat && matchQ;
  });

  return (
    <div>
      {/* Search bar row */}
      <div style={{ display: "flex", gap: 10, alignItems: "center", marginBottom: 14 }}>
        <input
          type="text"
          placeholder="🔍  Cerca server MCP..."
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          style={{
            flex: 1,
            padding: "8px 12px",
            borderRadius: 8,
            border: `1px solid ${tc.border}`,
            background: tc.bgInput,
            color: tc.text,
            fontSize: 13,
            outline: "none",
          }}
        />
        {registryLoaded && (
          <span
            style={{
              fontSize: 11,
              padding: "4px 10px",
              borderRadius: 6,
              background: "#15803d20",
              color: "#22c55e",
              border: "1px solid #22c55e40",
              whiteSpace: "nowrap",
              fontWeight: 600,
            }}
          >
            ✓ Catalogo aggiornato
          </span>
        )}
      </div>

      {/* Category pills */}
      <div
        style={{
          display: "flex",
          gap: 6,
          overflowX: "auto",
          flexWrap: "nowrap",
          marginBottom: 20,
          paddingBottom: 4,
        }}
      >
        {CATALOG_CATEGORIES.map((cat) => {
          const active = selectedCategory === cat.value;
          return (
            <button
              key={cat.value}
              onClick={() => setSelectedCategory(cat.value as CatalogCategory | "all")}
              style={{
                fontSize: 12,
                padding: "5px 12px",
                borderRadius: 20,
                border: `1px solid ${active ? tc.accent : tc.border}`,
                background: active ? `${tc.accent}18` : "transparent",
                color: active ? tc.accent : tc.textMuted,
                cursor: "pointer",
                whiteSpace: "nowrap",
                fontWeight: active ? 600 : 400,
                flexShrink: 0,
              }}
            >
              {cat.emoji && `${cat.emoji} `}{cat.label}
            </button>
          );
        })}
      </div>

      {/* Count */}
      <p style={{ fontSize: 12, color: tc.textMuted, marginBottom: 14 }}>
        {filtered.length} server{filtered.length !== 1 ? "" : ""} disponibili
      </p>

      {/* Grid */}
      <div
        style={{
          display: "grid",
          gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))",
          gap: 12,
        }}
      >
        {filtered.map((entry) => (
          <CatalogCard
            key={entry.id}
            entry={entry}
            tc={tc}
            alreadyAdded={existingKeys.has(`${normalizeLabel(entry.name)}::${entry.transport}`)}
            onAdd={() => onAddEntry(entry)}
          />
        ))}
      </div>

      {filtered.length === 0 && (
        <div style={{ textAlign: "center", padding: 40, color: tc.textMuted, fontSize: 13 }}>
          Nessun server trovato per "{query}"
        </div>
      )}
    </div>
  );
}

"use client";

import { useState } from "react";
import { useI18n } from "../../lib/i18n";
import { NotesTab } from "./notes-tab";
import { TagsTab } from "./tags-tab";
import { SearchTab } from "./search-tab";
import { GraphTab } from "./graph-tab";

interface Props {
  project: { id: string };
}

type TabKey = "notes" | "tags" | "search" | "graph";

export function KnowledgePanel({ project }: Props) {
  const { t } = useI18n();
  const [activeTab, setActiveTab] = useState<TabKey>("notes");

  const tabs: Array<{ key: TabKey; label: string }> = [
    { key: "notes", label: t("knowledge.tab.notes") },
    { key: "tags", label: t("knowledge.tab.tags") },
    { key: "search", label: t("knowledge.tab.search") },
    { key: "graph", label: t("knowledge.tab.graph") },
  ];

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%", overflow: "hidden" }}>
      <div style={{ padding: "12px 16px 0", borderBottom: "1px solid #e5e5e5" }}>
        <h3 style={{ margin: 0, fontSize: 15, fontWeight: 700, color: "#171717" }}>
          {t("knowledge.title")}
        </h3>
        <p style={{ margin: "4px 0 8px", fontSize: 12, color: "#737373" }}>
          {t("knowledge.subtitle")}
        </p>
        <div style={{ display: "flex", gap: 0 }}>
          {tabs.map((tab) => (
            <button
              key={tab.key}
              onClick={() => setActiveTab(tab.key)}
              style={{
                padding: "6px 12px",
                fontSize: 12,
                fontWeight: activeTab === tab.key ? 600 : 400,
                color: activeTab === tab.key ? "#171717" : "#737373",
                background: "none",
                border: "none",
                borderBottom: activeTab === tab.key ? "2px solid #171717" : "2px solid transparent",
                cursor: "pointer",
                transition: "all 0.15s",
              }}
            >
              {tab.label}
            </button>
          ))}
        </div>
      </div>
      <div style={{ flex: 1, minHeight: 0, overflow: "auto" }}>
        {activeTab === "notes" && <NotesTab projectId={project.id} />}
        {activeTab === "tags" && <TagsTab projectId={project.id} />}
        {activeTab === "search" && <SearchTab projectId={project.id} />}
        {activeTab === "graph" && <GraphTab />}
      </div>
    </div>
  );
}

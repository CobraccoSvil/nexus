"use client";

import { useState, useEffect } from "react";
import { useI18n } from "../../lib/i18n";
import { listKnowledgeTags, type KnowledgeTag } from "../../lib/api-client";
import { useProjectStore, selectKnowledgeChangedAt } from "../../lib/project-dispatcher/store";

interface Props {
  projectId: string;
}

export function TagsTab({ projectId }: Props) {
  const { t } = useI18n();
  const knowledgeChanged = useProjectStore(selectKnowledgeChangedAt);
  const [tags, setTags] = useState<KnowledgeTag[]>([]);

  useEffect(() => {
    listKnowledgeTags(projectId).then((r) => setTags(r.tags)).catch(() => {});
  }, [projectId, knowledgeChanged]);

  if (tags.length === 0) {
    return (
      <p style={{ padding: 16, fontSize: 13, color: "#a3a3a3", textAlign: "center" }}>
        {t("knowledge.tags.empty")}
      </p>
    );
  }

  const maxCount = Math.max(...tags.map((tg) => tg.noteCount), 1);

  return (
    <div style={{ padding: 16, display: "flex", flexWrap: "wrap", gap: 8, alignContent: "flex-start" }}>
      {tags.map((tg) => {
        const size = 12 + Math.round((tg.noteCount / maxCount) * 6);
        return (
          <span
            key={tg.tag}
            style={{
              fontSize: size,
              padding: "4px 10px",
              borderRadius: 12,
              background: "#f0f0ff",
              color: "#4338ca",
              fontWeight: 500,
              cursor: "default",
            }}
            title={`${tg.noteCount} note`}
          >
            #{tg.tag}
            <span style={{ fontSize: 10, color: "#818cf8", marginLeft: 4 }}>{tg.noteCount}</span>
          </span>
        );
      })}
    </div>
  );
}

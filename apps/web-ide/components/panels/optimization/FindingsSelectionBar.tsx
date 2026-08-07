"use client";

import type { QualityFinding } from "../../../lib/api-client";
import type { Tc, FixQueueItem } from "./types";
import { useI18n } from "../../../lib/i18n";

interface FindingsSelectionBarProps {
  tc: Tc;
  visibleFindings: QualityFinding[];
  selectedFindingIds: Set<string>;
  selectedFindings: QualityFinding[];
  visibleHighFindings: QualityFinding[];
  activeCategory: string;
  onSendToChat?: (message: string) => void;
  fixQueue: FixQueueItem[];
  toggleSelectAllVisible: () => void;
  startFixQueue: (targetFindings: QualityFinding[], autoFix?: boolean) => void;
}

export function FindingsSelectionBar({
  tc,
  visibleFindings,
  selectedFindingIds,
  selectedFindings,
  visibleHighFindings,
  activeCategory,
  onSendToChat,
  fixQueue,
  toggleSelectAllVisible,
  startFixQueue,
}: FindingsSelectionBarProps) {
  const { t } = useI18n();
  return (
    <div style={{
      position: "sticky", top: 0, zIndex: 2,
      display: "flex", alignItems: "center", gap: 8,
      padding: "5px 12px", borderBottom: `1px solid ${tc.border}`,
      background: tc.bgCard, flexWrap: "wrap",
    }}>
      <label style={{ display: "flex", alignItems: "center", gap: 5, cursor: "pointer", fontSize: 11, color: tc.textSecondary }}>
        <input
          type="checkbox"
          checked={visibleFindings.every(f => selectedFindingIds.has(f.id))}
          onChange={toggleSelectAllVisible}
          style={{ cursor: "pointer" }}
        />
        Tutti ({visibleFindings.length})
      </label>
      {selectedFindingIds.size > 0 && (
        <span style={{ fontSize: 11, color: tc.accent, fontWeight: 600 }}>
          {selectedFindingIds.size} selezionati
        </span>
      )}
      {selectedFindingIds.size > 0 && onSendToChat && fixQueue.length === 0 && (
        <>
          <button
            onClick={() => startFixQueue(selectedFindings)}
            title={t("panels.inviaIFindingsSelezionati")}
            style={{
              background: "#f97316", color: "#fff", border: "none", borderRadius: 4,
              padding: "2px 8px", fontSize: 10, cursor: "pointer",
            }}
          >
            {t("panels.fixSel")}
          </button>
          <button
            onClick={() => startFixQueue(selectedFindings, true)}
            title={t("panels.inviaIFindingsSelezionati2")}
            style={{
              background: "#7c3aed", color: "#fff", border: "none", borderRadius: 4,
              padding: "2px 8px", fontSize: 10, cursor: "pointer",
            }}
          >
            {t("panels.autoFixSel")}
          </button>
        </>
      )}
      {/* Fix per categoria corrente (solo se non è "Tutti" e ci sono HIGH) */}
      {activeCategory !== "all" && visibleHighFindings.length > 0 && onSendToChat && fixQueue.length === 0 && selectedFindingIds.size === 0 && (
        <>
          <span style={{ fontSize: 10, color: tc.textMuted, marginLeft: "auto" }}>
            {visibleHighFindings.length} HIGH in sezione
          </span>
          <button
            onClick={() => startFixQueue(visibleHighFindings)}
            title={`Fix tutti i problemi HIGH nella categoria ${activeCategory}`}
            style={{
              background: "#ef4444", color: "#fff", border: "none", borderRadius: 4,
              padding: "2px 8px", fontSize: 10, cursor: "pointer",
            }}
          >
            {t("panels.fixSezione")}
          </button>
          <button
            onClick={() => startFixQueue(visibleHighFindings, true)}
            title={`Auto Fix automatico per i problemi HIGH nella categoria ${activeCategory}`}
            style={{
              background: "#7c3aed", color: "#fff", border: "none", borderRadius: 4,
              padding: "2px 8px", fontSize: 10, cursor: "pointer",
            }}
          >
            {t("panels.autoFixSezione")}
          </button>
        </>
      )}
    </div>
  );
}

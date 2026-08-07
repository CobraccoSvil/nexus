"use client";

import type { QualityFinding } from "../../../lib/api-client";
import type { Tc, FixQueueItem } from "./types";
import { FindingsSelectionBar } from "./FindingsSelectionBar";
import { FindingItem } from "./FindingItem";
import { useI18n } from "../../../lib/i18n";

interface FindingsListProps {
  tc: Tc;
  loading: boolean;
  visibleFindings: QualityFinding[];
  selectedFindingIds: Set<string>;
  selectedFindings: QualityFinding[];
  visibleHighFindings: QualityFinding[];
  activeCategory: string;
  onSendToChat?: (message: string) => void;
  fixQueue: FixQueueItem[];
  toggleSelectAllVisible: () => void;
  startFixQueue: (targetFindings: QualityFinding[], autoFix?: boolean) => void;
  toggleFindingSelection: (id: string) => void;
  handleFix: (finding: QualityFinding) => void;
  handleMarkFixed: (finding: QualityFinding) => void;
  markFalsePositive: (findingId: string, ruleKey?: string) => void;
}

export function FindingsList({
  tc,
  loading,
  visibleFindings,
  selectedFindingIds,
  selectedFindings,
  visibleHighFindings,
  activeCategory,
  onSendToChat,
  fixQueue,
  toggleSelectAllVisible,
  startFixQueue,
  toggleFindingSelection,
  handleFix,
  handleMarkFixed,
  markFalsePositive,
}: FindingsListProps) {
  const { t } = useI18n();
  return (
    <div style={{ flex: 1, minWidth: 0, overflowY: "auto", padding: "4px 0" }}>

      {/* Barra selezione — sticky in cima alla lista */}
      {!loading && visibleFindings.length > 0 && (
        <FindingsSelectionBar
          tc={tc}
          visibleFindings={visibleFindings}
          selectedFindingIds={selectedFindingIds}
          selectedFindings={selectedFindings}
          visibleHighFindings={visibleHighFindings}
          activeCategory={activeCategory}
          onSendToChat={onSendToChat}
          fixQueue={fixQueue}
          toggleSelectAllVisible={toggleSelectAllVisible}
          startFixQueue={startFixQueue}
        />
      )}

      {loading && <div style={{ padding: 12, color: tc.textMuted, fontSize: 12 }}>{t("panels.caricamento")}</div>}
      {!loading && visibleFindings.length === 0 && (
        <div style={{ padding: 12, color: tc.textMuted, fontSize: 12 }}>
          {t("panels.nessunProblemaTrovatoIn")}
        </div>
      )}
      {visibleFindings.map(finding => (
        <FindingItem
          key={finding.id}
          tc={tc}
          finding={finding}
          selected={selectedFindingIds.has(finding.id)}
          onSendToChat={onSendToChat}
          toggleFindingSelection={toggleFindingSelection}
          handleFix={handleFix}
          handleMarkFixed={handleMarkFixed}
          markFalsePositive={markFalsePositive}
        />
      ))}
    </div>
  );
}

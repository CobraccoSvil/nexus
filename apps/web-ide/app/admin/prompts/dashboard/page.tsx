"use client";

/**
 * Pagina admin: Dashboard metriche prompt + Esperimenti A/B.
 *
 * Integra PromptDashboard (overview 7gg) e PromptExperiments (gestione canary).
 */

import { useState } from "react";
import { useThemeColors } from "../../../../lib/theme";
import PromptDashboard from "../../../../components/admin/PromptDashboard";
import PromptExperiments from "../../../../components/admin/PromptExperiments";
import { AdminPageHeader } from "../../../../components/admin/AdminPageHeader";

type Tab = "dashboard" | "esperimenti";

export default function PromptDashboardPage() {
  const tc = useThemeColors();
  const [tab, setTab] = useState<Tab>("dashboard");

  const tabBtn = (t: Tab, label: string): React.ReactElement => (
    <button
      key={t}
      onClick={() => setTab(t)}
      style={{
        paddingBottom: 10,
        fontSize: 13,
        fontWeight: 500,
        color: tab === t ? tc.accent : tc.textSecondary,
        background: "none",
        border: "none",
        borderBottom: `2px solid ${tab === t ? tc.accent : "transparent"}`,
        cursor: "pointer",
        transition: "color 0.15s, border-color 0.15s",
      }}
    >
      {label}
    </button>
  );

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 24 }}>
      <AdminPageHeader
        title="Dashboard Prompt"
        description="Metriche di qualita&apos; e esperimenti A/B canary gestiti dal PromptOptimizerWorker."
      />

      {/* Tab */}
      <div style={{ borderBottom: `1px solid ${tc.border}` }}>
        <nav style={{ display: "flex", gap: 24 }} aria-label="Tabs">
          {tabBtn("dashboard", "Panoramica 7 giorni")}
          {tabBtn("esperimenti", "Esperimenti A/B")}
        </nav>
      </div>

      {tab === "dashboard" ? <PromptDashboard /> : <PromptExperiments />}
    </div>
  );
}

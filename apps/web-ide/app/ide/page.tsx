"use client";

import { Suspense, useEffect, useState } from "react";
import { useSearchParams } from "next/navigation";
import { IdeShell } from "../../components/ide-shell";
import { getDashboardSnapshot, type DashboardSnapshot } from "../../lib/dashboard";

function IdePageContent() {
  const searchParams = useSearchParams();
  const [dashboard, setDashboard] = useState<DashboardSnapshot | null>(null);
  
  useEffect(() => {
    // Load dashboard snapshot on client side
    getDashboardSnapshot().then(setDashboard);
  }, []);

  const projectParam = searchParams.get("project");
  // Basic UUID validation
  const uuidRegex = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
  const initialProjectId = projectParam && uuidRegex.test(projectParam) ? projectParam : undefined;
  
  // Show loading state while dashboard is being fetched
  if (!dashboard) {
    return <div style={{ padding: "20px", color: "#999" }}>Caricamento...</div>;
  }

  return <IdeShell dashboard={dashboard} initialProjectId={initialProjectId} />;
}

export default function Page() {
  return (
    <Suspense fallback={<div style={{ padding: "20px", color: "#999" }}>Caricamento pagina...</div>}>
      <IdePageContent />
    </Suspense>
  );
}

export const dynamic = "force-dynamic";

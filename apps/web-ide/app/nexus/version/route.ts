import { NextResponse } from "next/server";
import { readFileSync } from "fs";
import { join } from "path";

// Endpoint di verifica versione — usato da deploy-nexus.sh per confermare
// che il frontend aggiornato sia effettivamente in esecuzione.
// Percorso: /nexus/version (bypassa il routing nginx /api/admin/ → :4010)
export async function GET() {
  let buildId = "unknown";
  let buildTime: number | null = null;

  try {
    buildId = readFileSync(join(process.cwd(), ".next", "BUILD_ID"), "utf-8").trim();
    // Il buildId ha formato "build-TIMESTAMP" (vedi generateBuildId in next.config.ts)
    const match = buildId.match(/^build-(\d+)$/);
    if (match) buildTime = parseInt(match[1], 10);
  } catch {
    // In sviluppo (.next non presente) restituisce unknown
  }

  return NextResponse.json({
    buildId,
    buildTime,
    buildDate: buildTime ? new Date(buildTime).toISOString() : null,
    uptime: process.uptime(),
  });
}

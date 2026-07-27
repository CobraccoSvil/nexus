/**
 * Statistiche del database Nexus.
 * GET /api/admin/nexus-database-stats -> mcp-core /internal/nexus-database-stats
 *
 * Delega al punto unico `proxyRequest` (regola L), che propaga lo status del
 * backend. Qui c'era una copia locale del proxy con una differenza fatale: su
 * 404 rispondeva **200 con dati inventati** ("dati mock per sviluppo") — sei
 * tabelle con conteggi plausibili, 45,2 MB di database e un cache hit del
 * 94,2%. Numeri indistinguibili da quelli veri: chi guardava il pannello con il
 * backend spento non aveva modo di sapere che stava leggendo una finzione.
 */

import type { NextRequest } from "next/server";
import type { NextResponse } from "next/server";
import { proxyRequest } from "../_proxy";

export async function GET(request: NextRequest): Promise<NextResponse> {
  return proxyRequest(request, "/internal/nexus-database-stats", "GET");
}

import { type NextRequest } from "next/server";
import { proxyRequest } from "../../admin/_proxy";

/** GET /api/projects/mine — lista progetti dell'utente autenticato */
export async function GET(request: NextRequest) {
  return proxyRequest(request, "/api/projects/mine", "GET");
}

import { NextResponse } from "next/server";

const BRAIN_URL = process.env.BRAIN_URL || "http://localhost:8001";

export async function GET() {
  try {
    const targetUrl = `${BRAIN_URL}/health`;
    console.log(`[API] Richiesta al Brain: ${targetUrl}`);

    const response = await fetch(targetUrl, {
      method: "GET",
      headers: { "Content-Type": "application/json" },
    });

    const data = await response.json();
    console.log(`[API] Risposta dal Brain: status=${response.status}`);

    return NextResponse.json(data, { status: response.status });
  } catch (error: any) {
    const errorMsg = error?.message || String(error);
    console.error("Brain health endpoint error:", errorMsg);
    return NextResponse.json(
      { error: `Brain non disponibile: ${errorMsg}` },
      { status: 503 }
    );
  }
}

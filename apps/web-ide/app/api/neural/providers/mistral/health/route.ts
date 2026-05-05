import { NextResponse } from "next/server";

const BRAIN_URL = process.env.BRAIN_URL || "http://localhost:8001";

export async function GET() {
  try {
    const response = await fetch(`${BRAIN_URL}/providers/mistral/health`, {
      method: "GET",
      headers: { "Content-Type": "application/json" },
    });

    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error) {
    return NextResponse.json({ error: "Mistral non disponibile" }, { status: 503 });
  }
}

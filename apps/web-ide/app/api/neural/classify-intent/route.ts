import { NextResponse } from "next/server";

const BRAIN_URL = process.env.BRAIN_URL || "http://localhost:8001";

export async function POST(request: Request) {
  try {
    const body = await request.json();
    const response = await fetch(`${BRAIN_URL}/classify-intent`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });

    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error) {
    console.error("Classify intent error:", error);
    return NextResponse.json(
      { error: "Errore nella classificazione dell'intento" },
      { status: 500 }
    );
  }
}

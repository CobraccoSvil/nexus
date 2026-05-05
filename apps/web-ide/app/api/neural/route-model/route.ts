import { NextResponse } from "next/server";

const BRAIN_URL = process.env.BRAIN_URL || "http://localhost:8001";

export async function POST(request: Request) {
  try {
    const body = await request.json();
    const response = await fetch(`${BRAIN_URL}/route-model`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });

    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error) {
    console.error("Route model error:", error);
    return NextResponse.json(
      { error: "Errore nel routing del modello" },
      { status: 500 }
    );
  }
}

import { NextResponse } from "next/server";

const BRAIN_URL = process.env.BRAIN_URL || "http://localhost:8001";

export async function GET(
  request: Request,
  { params }: { params: Promise<{ provider: string }> }
) {
  const { provider } = await params;

  try {
    const response = await fetch(`${BRAIN_URL}/providers/${provider}/health`, {
      method: "GET",
      headers: { "Content-Type": "application/json" },
    });

    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error) {
    console.error(`Provider ${provider} health endpoint error:`, error);
    return NextResponse.json(
      { error: `Provider ${provider} non disponibile` },
      { status: 503 }
    );
  }
}

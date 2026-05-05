import { NextResponse } from "next/server";

const BRAIN_URL = process.env.BRAIN_URL || "http://localhost:8001";

export async function GET(
  request: Request,
  { params }: { params: Promise<{ paths?: string[] }> }
) {
  const { paths = [] } = await params;
  if (!paths || paths.length === 0) {
    return NextResponse.json({ error: "Path non specificato" }, { status: 400 });
  }

  const pathStr = paths.join("/");
  const targetUrl = `${BRAIN_URL}/providers/${pathStr}`;

  try {
    console.log(`[API Proxy] GET ${targetUrl}`);
    const response = await fetch(targetUrl, {
      method: "GET",
      headers: { "Content-Type": "application/json" },
    });

    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error: any) {
    console.error(`Brain GET /providers/${pathStr} error:`, error?.message);
    return NextResponse.json(
      { error: "Provider non disponibile" },
      { status: 503 }
    );
  }
}

export async function POST(
  request: Request,
  { params }: { params: Promise<{ paths?: string[] }> }
) {
  const { paths = [] } = await params;
  const pathStr = paths.join("/");
  const targetUrl = `${BRAIN_URL}/providers/${pathStr}`;

  try {
    const body = await request.json().catch(() => null);
    const response = await fetch(targetUrl, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: body ? JSON.stringify(body) : undefined,
    });

    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error) {
    console.error(`Brain POST /providers/${pathStr} error:`, error);
    return NextResponse.json(
      { error: "Provider non disponibile" },
      { status: 503 }
    );
  }
}

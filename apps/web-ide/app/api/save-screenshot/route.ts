import { NextRequest, NextResponse } from "next/server";
import { writeFileSync, mkdirSync } from "fs";
import { join } from "path";

// Endpoint temporaneo per salvare screenshot landing
// Rimuovere dopo la cattura
export async function POST(req: NextRequest) {
  try {
    const { name, dataUrl } = await req.json();
    if (!name || !dataUrl) {
      return NextResponse.json({ error: "name e dataUrl richiesti" }, { status: 400 });
    }

    // Estrai base64 dal data URL
    const base64 = dataUrl.replace(/^data:image\/\w+;base64,/, "");
    const buffer = Buffer.from(base64, "base64");

    const dir = join(process.cwd(), "public", "screenshots");
    mkdirSync(dir, { recursive: true });

    const filePath = join(dir, `${name}.webp`);
    writeFileSync(filePath, buffer);

    return NextResponse.json({ ok: true, path: filePath, size: buffer.length });
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    return NextResponse.json({ error: msg }, { status: 500 });
  }
}

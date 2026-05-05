/**
 * POST /api/system/services/[service]/[action]
 *
 * Avvia, stoppa o riavvia un servizio di sistema Nexus tramite systemctl.
 * Non dipende da mcp-core: funziona anche quando l'orchestratore è down.
 *
 * action: "start" | "stop" | "restart"
 * service: nome corto, es. "nexus-core" (senza ".service")
 */
import { NextResponse } from "next/server";
import { exec } from "child_process";
import { promisify } from "util";

export const runtime = "nodejs";

const execAsync = promisify(exec);

// Allowlist di sicurezza: solo i servizi Nexus possono essere controllati.
const ALLOWED_SERVICES = new Set([
  "nexus-core-wsl",
  "nexus-neural-wsl",
  "nexus-gateway",
  "nexus-chat-wsl",
  "nexus-plugin-wsl",
  "nexus-admin-wsl",
  "nexus-billing-wsl",
  "nexus-doc-wsl",
]);

const ALLOWED_ACTIONS = new Set(["start", "stop", "restart"]);

type Params = { service: string; action: string };

export async function POST(
  _request: Request,
  { params }: { params: Promise<Params> }
) {
  const { service, action } = await params;

  if (!ALLOWED_SERVICES.has(service)) {
    return NextResponse.json({ ok: false, error: `Servizio non permesso: ${service}` }, { status: 400 });
  }
  if (!ALLOWED_ACTIONS.has(action)) {
    return NextResponse.json({ ok: false, error: `Azione non valida: ${action}` }, { status: 400 });
  }

  const unit = `${service}.service`;

  try {
    // Prova systemctl --user (sviluppo), poi system (produzione installata)
    let stdout = "";
    let stderr = "";
    try {
      const r = await execAsync(`systemctl --user ${action} "${unit}" 2>&1`);
      stdout = r.stdout;
    } catch (err: unknown) {
      const e = err as { stdout?: string; stderr?: string; message?: string };
      // Se --user fallisce, prova senza (system)
      try {
        const r2 = await execAsync(`systemctl ${action} "${unit}" 2>&1`);
        stdout = r2.stdout;
      } catch (err2: unknown) {
        const e2 = err2 as { stdout?: string; stderr?: string; message?: string };
        stderr = e2.stderr ?? e2.stdout ?? e2.message ?? "Errore sconosciuto";
        return NextResponse.json(
          { ok: false, unit, action, stdout: e.stdout ?? "", stderr },
          { status: 500 }
        );
      }
    }

    return NextResponse.json({ ok: true, unit, action, stdout, stderr });
  } catch (err: unknown) {
    const e = err as { message?: string };
    return NextResponse.json(
      { ok: false, unit, action, stdout: "", stderr: e.message ?? "Errore interno" },
      { status: 500 }
    );
  }
}

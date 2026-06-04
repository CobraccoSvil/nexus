import type { ProjectServiceEntry } from "../../../lib/api-client";
import type { useThemeColors } from "../../../lib/theme";

type ThemeColors = ReturnType<typeof useThemeColors>;

export type ServiceAction = "start" | "stop" | "restart";

// ── Helper stile condivisi tra le sezioni del RunPanel ─────────────────────
export function hdrStyle(tc: ThemeColors, extra?: React.CSSProperties): React.CSSProperties {
  return {
    fontSize:10, color:tc.textMuted, textTransform:"uppercase", letterSpacing:"0.08em",
    padding:"8px 12px 6px", borderBottom:`1px solid ${tc.border}`,
    background:tc.bgSidebar, flexShrink:0,
    display:"flex", justifyContent:"space-between", alignItems:"center",
    ...extra,
  };
}

export function actBtnStyle(tc: ThemeColors, color:string, busy:boolean): React.CSSProperties {
  return {
    background:"transparent", border:`1px solid ${color}`, color:busy?tc.textMuted:color,
    borderRadius:3, padding:"1px 7px", fontSize:10, cursor:busy?"wait":"pointer",
    fontFamily:'"JetBrains Mono", monospace', opacity:busy?0.5:1, transition:"opacity 0.15s",
  };
}

export const STATE_COLOR: Record<string, string> = {
  active:       "#22c55e",
  activating:   "#f59e0b",
  deactivating: "#f59e0b",
  inactive:     "#6b7280",
  failed:       "#ef4444",
};

export function stateColor(s: string, portAlive?: boolean) {
  // Se il servizio risponde sulla porta, e' operativo a tutti gli effetti -> verde
  if ((s === "inactive" || s === "unknown") && portAlive) return "#22c55e";
  return STATE_COLOR[s] ?? "#6b7280";
}

export function stateLabel(state: string, sub: string, portAlive?: boolean) {
  if ((state === "inactive" || state === "unknown") && portAlive) {
    // Servizio attivo ma avviato fuori da systemd (es. via deploy script)
    return "attivo";
  }
  const m: Record<string, string> = { active:"attivo", activating:"avvio…", deactivating:"arresto…", inactive:"inattivo", failed:"errore" };
  const base = m[state] ?? state;
  return sub && sub !== state ? `${base} (${sub})` : base;
}

export const KIND_ICON: Record<string, string> = {
  npm:"📦", pnpm:"📦", dotnet:"🔷", cargo:"🦀", python:"🐍", shell:"⚙️", docker:"🐳",
};

/** Determina la modalita' di esecuzione da kind/command/args. */
export function detectRunMode(kind: string, command: string, args: string[]): "docker" | "native" {
  if (kind === "docker") return "docker";
  const cmd = (command || "").toLowerCase();
  if (cmd === "docker" || cmd.endsWith("/docker") || cmd === "docker-compose" || cmd.endsWith("/docker-compose")) return "docker";
  // shell wrapper "bash -c 'docker start ...'"
  const joined = `${command} ${args.join(" ")}`.toLowerCase();
  if (/\bdocker\s+(start|run|exec|compose|up|stop|restart)\b/.test(joined)) return "docker";
  return "native";
}

// ── Genera prompt diagnostico contestualizzato per la chat AI ─────────────
export function buildDiagnosticPrompt(svc: ProjectServiceEntry): string {
  const base = `Il servizio "${svc.short}" (unit: ${svc.unit}) e' in crash-loop e non riesce ad avviarsi.`;
  const error = svc.last_error ? `\nErrore rilevato: ${svc.last_error}` : "";

  switch (svc.error_kind) {
    case "missing_script":
      return `${base}${error}\nControlla il package.json del sotto-progetto corrispondente, identifica lo script corretto per avviare il servizio in modalita' dev e aggiorna il file .service systemd di conseguenza.`;
    case "missing_directory":
      return `${base}${error}\nLa directory di lavoro configurata nel file .service non esiste. Verifica la struttura del progetto e correggi il percorso WorkingDirectory nel file systemd.`;
    case "missing_dependencies":
      return `${base}${error}\nLe dipendenze Node.js non sono installate. Esegui l'installazione delle dipendenze (npm install / pnpm install) nella directory corretta del progetto.`;
    case "missing_sdk":
      return `${base}${error}\nVerifica che il SDK necessario sia installato e accessibile nel PATH. Se non e' installato, suggerisci i comandi per installarlo.`;
    case "build_failed":
      return `${base}${error}\nLeggi il journal del servizio con 'journalctl --user -u ${svc.unit} -n 50 --no-pager' per estrarre gli errori di compilazione completi, poi analizzali e proponi le correzioni necessarie al codice sorgente.`;
    case "port_in_use":
      return `${base}${error}\nIdentifica quale processo occupa la porta e suggerisci come liberarla oppure come configurare il servizio su una porta alternativa.`;
    case "permission_denied":
      return `${base}${error}\nVerifica i permessi dei file coinvolti e suggerisci i comandi necessari per correggerli.`;
    default:
      return `${base}${error}\nAnalizza i log del servizio con 'journalctl --user -u ${svc.unit} -n 50 --no-pager' per identificare la causa del crash e proponi una soluzione.`;
  }
}

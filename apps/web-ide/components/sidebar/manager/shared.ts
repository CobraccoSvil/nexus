// Tipi, costanti e helper puri condivisi tra i sotto-componenti della sidebar.
// Estratti da sidebar-manager.tsx durante il refactor del god-file (regola H):
// nessun cambiamento di comportamento, solo deduplica in un modulo unico.
import type { useThemeColors } from "../../../lib/theme";
import type { RunConfigItem } from "../../../lib/api-client";

export type ThemeColors = ReturnType<typeof useThemeColors>;

export function iconButton(tc: ThemeColors, disabled = false) {
  return {
    width: 30,
    height: 30,
    border: `1px solid ${tc.border}`,
    background: disabled ? tc.bgInput : tc.bgCard,
    color: disabled ? tc.textMuted : tc.textSecondary,
    borderRadius: 7,
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    cursor: disabled ? "not-allowed" : "pointer",
    fontSize: 13,
    lineHeight: 1,
  } as const;
}

export function inputStyle(tc: ThemeColors) {
  return {
    width: "100%",
    background: tc.bgInput,
    color: tc.text,
    border: `1px solid ${tc.border}`,
    borderRadius: 6,
    padding: "7px 10px",
    fontSize: 12,
    boxSizing: "border-box",
  } as const;
}

export function listRowButton(tc: ThemeColors) {
  return {
    width: "100%",
    textAlign: "left",
    padding: "8px 10px",
    borderRadius: 8,
    border: `1px solid ${tc.border}`,
    background: tc.bgCard,
    cursor: "pointer",
    display: "flex",
    flexDirection: "column",
    gap: 4,
  } as const;
}

export const KIND_OPTIONS = [
  { value: "shell", label: "Shell" },
  { value: "npm", label: "npm" },
  { value: "cargo", label: "Cargo" },
  { value: "python", label: "Python" },
  { value: "node", label: "Node" },
];

export const KIND_ICON: Record<string, string> = {
  shell: "⚡",
  npm: "📦",
  cargo: "🦀",
  python: "🐍",
  node: "🟩",
  playwright: "🐛",
};

export const KIND_PLACEHOLDER: Record<string, string> = {
  shell: "bash script.sh",
  npm: "npm run dev",
  cargo: "cargo run",
  python: "python main.py",
  node: "node index.js",
};

export const ROLE_BADGE: Record<string, { icon: string; label: string; color: string }> = {
  frontend: { icon: "⚛️", label: "frontend", color: "#60a5fa" },
  backend:  { icon: "🔧", label: "backend",  color: "#a78bfa" },
  service:  { icon: "🐳", label: "servizio", color: "#22d3ee" },
  test:     { icon: "🧪", label: "test",     color: "#f59e0b" },
  tool:     { icon: "🛠️", label: "tool",     color: "#9ca3af" },
};

export const GROUP_ICON = (group: string | undefined | null): string => {
  if (!group) return "📁";
  if (group === "docker") return "🐳";
  if (group.startsWith("crates") || group.startsWith("cargo")) return "🦀";
  if (group.startsWith("playwright")) return "🐛";
  if (group === "python") return "🐍";
  if (group === "make") return "🛠️";
  if (group === "go" || group === "dotnet") return "⚙️";
  return "📦";
};

export type EditState = {
  id?: string;
  label: string;
  kind: string;
  command: string;
  args: string;
  cwd: string;
};

export type SuggestedConfig = Omit<RunConfigItem, "id"> & { args?: string[] };

// ── Filtraggio + Categorizzazione Run Configs ────────────────────────────
// L'auto-detect propone tutto quel che trova nel filesystem (npm scripts, make
// targets, ecc.), ma molte voci si sovrappongono ai servizi gestiti del
// progetto (gestiti nel pannello inferiore Run & Debug) e altre sono
// "stop / kill" che non hanno senso come bottone "▶ Avvia".

const STOP_PATTERNS = [
  /-stop\b/i, /:stop\b/i, /^stop[-_:]/i,
  /\bkill\b/i, /^docker\s+stop\b/i, /^docker[-_]?compose\s+down\b/i,
  /\bteardown\b/i, /-down\b/i, /:down\b/i,
];

/** True se il run config è uno script di stop/kill (non andrebbe lanciato col tasto ▶). */
export function isStopScript(c: { label: string; command?: string; args?: string[] }): boolean {
  const haystack = `${c.label} ${c.command ?? ""} ${(c.args ?? []).join(" ")}`;
  return STOP_PATTERNS.some(re => re.test(haystack));
}

/** True se il run config duplica un servizio del progetto già installato (per nome breve). */
export function isDuplicateOfManagedService(
  c: { label: string; command?: string; args?: string[] },
  serviceShorts: Set<string>,
): boolean {
  if (serviceShorts.size === 0) return false;
  const text = `${c.label} ${(c.args ?? []).join(" ")}`.toLowerCase();
  for (const short of serviceShorts) {
    const s = short.toLowerCase();
    if (s.length < 3) continue;
    // Match testuale solo se la parola completa è presente (no "be" che matcha "backend")
    const re = new RegExp(`\\b${s.replace(/[-/\\^$*+?.()|[\]{}]/g, "\\$&")}\\b`);
    if (re.test(text)) return true;
  }
  return false;
}

export type Category = "build" | "test" | "lint" | "db" | "dev" | "altro";

export const CATEGORY_LABEL: Record<Category, string> = {
  build: "Build & compile",
  test:  "Test",
  lint:  "Lint & format",
  db:    "Database",
  dev:   "Dev / serve",
  altro: "Altro",
};

export const CATEGORY_ICON: Record<Category, string> = {
  build: "🔨", test: "🧪", lint: "✨", db: "🗄️", dev: "▶", altro: "⚡",
};

export function categorize(c: { label: string; command?: string; args?: string[] }): Category {
  const haystack = `${c.label} ${c.command ?? ""} ${(c.args ?? []).join(" ")}`.toLowerCase();
  if (/\b(test|spec|jest|vitest|playwright|pytest|cargo test|dotnet test|go test)\b/.test(haystack)) return "test";
  if (/\b(lint|format|fmt|prettier|eslint|stylelint|clippy|tsc|typecheck|check)\b/.test(haystack)) return "lint";
  if (/\b(build|compile|bundle|tsc --build|webpack|rollup|cargo build)\b/.test(haystack)) return "build";
  if (/\b(migrate|seed|ef database|sqlx|alembic|prisma|drizzle|db[-_:](?:up|init|reset|migrate|seed))\b/.test(haystack)) return "db";
  if (/\b(dev|start|serve|preview|watch|hmr|run dev|run start)\b/.test(haystack)) return "dev";
  return "altro";
}

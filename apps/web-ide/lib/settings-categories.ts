"use client";

/**
 * Categorie settings per la navigazione admin — punto unico (regola L).
 *
 * Le voci derivano dai DATI (GET /api/admin/settings-categories, che fa
 * SELECT DISTINCT category dal DB): una categoria nuova introdotta da una
 * migrazione diventa automaticamente navigabile, senza toccare il frontend.
 * Prima del fix esistevano DUE liste hardcoded divergenti (admin-sidebar e
 * CATEGORY_ORDER in settings-panel) e ~160 chiavi in categorie invisibili.
 *
 * KNOWN_CATEGORY_META governa solo ordine e label delle categorie note;
 * le categorie non in lista compaiono in coda, in ordine alfabetico, con
 * il nome grezzo come label.
 */

import { useEffect, useState } from "react";
import { fetchJson } from "./api/_shared";

export interface SettingsCategory {
  key: string;
  label: string;
  count?: number;
}

/**
 * Livello UI di una categoria: "daily" = uso quotidiano (in alto nel menu),
 * "advanced" = configurazione profonda (gruppo collassato). Default advanced.
 * Fase 1 del redesign: la classificazione e' qui (frontend) per categoria; la
 * fase 2 la sposta nel DB per-chiave (settings.ui_level, ADR 0039).
 */
export type CategoryLevel = "daily" | "advanced";

/** Ordine, label e livello delle categorie note (non e' un filtro di visibilita').
 *  Elenco completo delle categorie live: elimina i titoli grezzi "cat.X". */
const KNOWN_CATEGORY_META: ReadonlyArray<{ key: string; label: string; level?: CategoryLevel }> = [
  // Uso quotidiano
  { key: "providers", label: "Provider AI", level: "daily" },
  { key: "routing", label: "Routing", level: "daily" },
  { key: "connectors", label: "Plugin MCP", level: "daily" },
  { key: "security", label: "Sicurezza & DLP", level: "daily" },
  { key: "quality", label: "Qualita", level: "daily" },
  { key: "learning", label: "Learning", level: "daily" },
  { key: "auth", label: "Autenticazione", level: "daily" },
  // Configurazione avanzata
  { key: "agent", label: "Agenti AI", level: "advanced" },
  { key: "agent_tools", label: "Strumenti agente", level: "advanced" },
  { key: "orchestrator", label: "Orchestrator", level: "advanced" },
  { key: "optimizer", label: "Ottimizzatore", level: "advanced" },
  { key: "reflection", label: "Self-Reflection", level: "advanced" },
  { key: "embeddings", label: "Embeddings", level: "advanced" },
  { key: "infrastructure", label: "Infrastruttura", level: "advanced" },
  { key: "gateway", label: "Gateway LLM", level: "advanced" },
  { key: "general", label: "Generale", level: "advanced" },
  { key: "wiki", label: "Wiki", level: "advanced" },
  { key: "knowledge", label: "Conoscenza", level: "advanced" },
  { key: "kb", label: "Knowledge Base", level: "advanced" },
  { key: "database", label: "Database", level: "advanced" },
  { key: "nexus_tools", label: "Strumenti Nexus", level: "advanced" },
  { key: "claude_agents", label: "Agenti Claude", level: "advanced" },
  { key: "prompt_templates", label: "Template prompt", level: "advanced" },
  { key: "project", label: "Progetto", level: "advanced" },
  { key: "system", label: "Sistema", level: "advanced" },
  { key: "chat", label: "Chat", level: "advanced" },
  { key: "media", label: "Media", level: "advanced" },
];

const META_BY_KEY = new Map(KNOWN_CATEGORY_META.map((m) => [m.key, m]));

/** Etichetta leggibile di una categoria (punto unico, regola L): label nota o
 *  fallback capitalizzato. Usato da sidebar E titolo pagina per non mostrare mai
 *  la chiave grezza (fix dei titoli "cat.X" non tradotti). */
export function labelForCategory(category: string): string {
  const meta = META_BY_KEY.get(category);
  if (meta) return meta.label;
  return category.charAt(0).toUpperCase() + category.slice(1).replace(/_/g, " ");
}

/** Livello UI di una categoria (default advanced: una categoria nuova non
 *  classificata finisce nel gruppo protetto, mai in cima). */
export function levelForCategory(category: string): CategoryLevel {
  return META_BY_KEY.get(category)?.level ?? "advanced";
}

function buildList(dbCategories: { category: string; count: number }[]): SettingsCategory[] {
  const byKey = new Map(dbCategories.map((c) => [c.category, c.count]));
  const ordered: SettingsCategory[] = [];
  for (const meta of KNOWN_CATEGORY_META) {
    if (byKey.has(meta.key)) {
      ordered.push({ key: meta.key, label: meta.label ?? meta.key, count: byKey.get(meta.key) });
      byKey.delete(meta.key);
    }
  }
  const rest = Array.from(byKey.entries())
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([key, count]) => ({ key, label: key, count }));
  return [...ordered, ...rest];
}

/** Fallback offline: solo le categorie note, finche' il fetch non risponde. */
const FALLBACK: SettingsCategory[] = KNOWN_CATEGORY_META.map((m) => ({
  key: m.key,
  label: m.label ?? m.key,
}));

export function useSettingsCategories(): SettingsCategory[] {
  const [categories, setCategories] = useState<SettingsCategory[]>(FALLBACK);

  useEffect(() => {
    let cancelled = false;
    fetchJson<{ categories: { category: string; count: number }[] }>(
      "/api/admin/settings-categories",
    )
      .then((data) => {
        if (!cancelled && Array.isArray(data.categories) && data.categories.length > 0) {
          setCategories(buildList(data.categories));
        }
      })
      .catch(() => {
        // Backend irraggiungibile: si resta sul fallback statico.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return categories;
}

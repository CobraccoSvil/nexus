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

/** Ordine e label delle categorie note (non e' un filtro di visibilita'). */
export const KNOWN_CATEGORY_META: ReadonlyArray<{ key: string; label?: string }> = [
  { key: "providers", label: "Provider AI" },
  { key: "routing", label: "Routing" },
  { key: "connectors", label: "Plugin MCP" },
  { key: "security", label: "Sicurezza & DLP" },
  { key: "infrastructure", label: "Infrastruttura" },
  { key: "embeddings", label: "Embeddings" },
  { key: "quality", label: "Qualita" },
  { key: "learning", label: "Learning" },
  { key: "agent", label: "Agenti AI" },
  { key: "orchestrator", label: "Orchestrator" },
  { key: "optimizer", label: "Ottimizzatore" },
  { key: "reflection", label: "Self-Reflection" },
  { key: "auth", label: "Autenticazione" },
];

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

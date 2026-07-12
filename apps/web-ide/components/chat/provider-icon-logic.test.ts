// Unit test del mapping provider -> mark SVG (ADR 0037). Runner: node --test.

import { test } from "node:test";
import assert from "node:assert/strict";
import {
  providerMarkKey,
  providerModelTint,
  providerBaseColor,
  providerLabel,
} from "./provider-icon-logic.ts";

test("providerMarkKey: brand noti mappati sul proprio mark", () => {
  assert.equal(providerMarkKey("anthropic"), "anthropic");
  assert.equal(providerMarkKey("openai"), "openai");
  assert.equal(providerMarkKey("google"), "google");
  assert.equal(providerMarkKey("deepseek"), "deepseek");
  assert.equal(providerMarkKey("mistral"), "mistral");
});

test("providerMarkKey: alias comuni normalizzati al brand", () => {
  assert.equal(providerMarkKey("claude"), "anthropic");
  assert.equal(providerMarkKey("gpt"), "openai");
  assert.equal(providerMarkKey("gemini"), "google");
  assert.equal(providerMarkKey("vertex"), "google");
});

test("providerMarkKey: case-insensitive e trim", () => {
  assert.equal(providerMarkKey("  Anthropic  "), "anthropic");
  assert.equal(providerMarkKey("OpenAI"), "openai");
});

test("providerMarkKey: provider ignoto/vuoto -> generic", () => {
  assert.equal(providerMarkKey("acme-llm"), "generic");
  assert.equal(providerMarkKey(""), "generic");
  assert.equal(providerMarkKey(undefined), "generic");
  assert.equal(providerMarkKey(null), "generic");
});

// ── Consolidamento normalizzazione: colore/label da un solo punto ────────────

test("providerBaseColor: alias del brand ricondotti allo stesso colore", () => {
  assert.equal(providerBaseColor("claude"), providerBaseColor("anthropic"));
  assert.equal(providerBaseColor("gpt"), providerBaseColor("openai"));
  assert.equal(providerBaseColor("gemini"), providerBaseColor("google"));
  assert.equal(providerBaseColor("vertex"), providerBaseColor("google"));
});

test("providerBaseColor/providerLabel: provider ignoto -> fallback stabile", () => {
  assert.equal(providerBaseColor("acme-llm"), "#94a3b8");
  assert.equal(providerBaseColor(null), "#94a3b8");
  assert.equal(providerBaseColor(undefined), "#94a3b8");
  assert.equal(providerLabel("acme-llm"), "Acme-llm");
});

test("providerLabel: alias gemini normalizzato al brand Google", () => {
  assert.equal(providerLabel("gemini"), "Google");
});

// ── providerModelTint (tinta per-modello) ────────────────────────────────────

test("providerModelTint: forma #RRGGBB valida", () => {
  assert.match(
    providerModelTint("anthropic", "claude-sonnet-4-5"),
    /^#[0-9a-f]{6}$/,
  );
});

test("providerModelTint: deterministica (stesso model -> stessa tinta)", () => {
  const a = providerModelTint("anthropic", "claude-sonnet-4-5");
  const b = providerModelTint("anthropic", "claude-sonnet-4-5");
  assert.equal(a, b);
});

test("providerModelTint: senza model -> colore brand esatto (no shift)", () => {
  assert.equal(
    providerModelTint("anthropic", null),
    providerBaseColor("anthropic"),
  );
  assert.equal(
    providerModelTint("anthropic", ""),
    providerBaseColor("anthropic"),
  );
  assert.equal(providerModelTint("acme-llm", null), "#94a3b8");
});

test("providerModelTint: provider ignoto con model -> fallback deterministico", () => {
  const a = providerModelTint("acme-llm", "x-model");
  const b = providerModelTint("acme-llm", "x-model");
  assert.equal(a, b);
  assert.match(a, /^#[0-9a-f]{6}$/);
});

test("providerModelTint: modelli diversi -> tinte percettibilmente diverse", () => {
  const models = Array.from({ length: 24 }, (_, i) => `model-${i}-alpha`);
  const tints = new Set(models.map((m) => providerModelTint("anthropic", m)));
  // Con hashing FNV-1a su model id distinti le collisioni sono rarissime:
  // tolleriamo al piu' 2 collisioni sui 24 campioni.
  assert.ok(
    tints.size >= 22,
    `attese >=22 tinte distinte, ottenute ${tints.size}`,
  );
});

test("providerModelTint: stesso model su provider diversi -> tinte diverse", () => {
  assert.notEqual(
    providerModelTint("anthropic", "shared-model"),
    providerModelTint("openai", "shared-model"),
  );
});

test("providerModelTint: brand riconoscibile (shift di hue contenuto)", () => {
  // Il brand Anthropic e' arancione (rosso dominante): dopo lo shift la tinta
  // deve restare nella stessa famiglia (rosso > blu) su tutto il campione, cioe'
  // non deve virare a un colore freddo.
  const models = Array.from({ length: 16 }, (_, i) => `sonnet-${i}`);
  for (const m of models) {
    const hex = providerModelTint("anthropic", m);
    const r = parseInt(hex.slice(1, 3), 16);
    const b = parseInt(hex.slice(5, 7), 16);
    assert.ok(r > b, `atteso rosso>blu per brand anthropic, ottenuto ${hex}`);
  }
});

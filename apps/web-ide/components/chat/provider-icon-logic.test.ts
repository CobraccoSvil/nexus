// Unit test del mapping provider -> mark SVG (ADR 0037). Runner: node --test.

import { test } from "node:test";
import assert from "node:assert/strict";
import { providerMarkKey } from "./provider-icon-logic.ts";

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

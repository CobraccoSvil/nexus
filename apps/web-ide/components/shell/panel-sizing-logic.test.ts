// Test di regressione per i vincoli del pannello destro (P2b). Runner: node --test.
//
// Copre il tetto piu' generoso sotto la soglia narrow/mobile e la stabilita' dei
// vincoli su viewport ampi (comportamento invariato).

import { test } from "node:test";
import assert from "node:assert/strict";
import { rightSidebarBounds, clampRightWidth } from "./panel-sizing-logic.ts";

test("viewport ampio (>=1280): percentuale 0.6 e cap 620 invariati", () => {
  const b = rightSidebarBounds(1600);
  assert.equal(b.min, 280);
  // 1600 * 0.6 = 960 -> clampato al cap assoluto 620.
  assert.equal(b.max, 620);
});

test("viewport ampio esattamente al confine 1280: regime largo (0.6)", () => {
  const b = rightSidebarBounds(1280);
  // 1280 non e' < 1280 -> non narrow.
  assert.equal(b.min, 280);
  assert.equal(b.max, 620);
});

test("viewport narrow (<1280): tetto piu' generoso (0.72, cap 760)", () => {
  const b = rightSidebarBounds(1200);
  assert.equal(b.min, 280);
  // 1200 * 0.72 = 864 -> clampato al cap narrow 760.
  assert.equal(b.max, 760);
});

test("viewport narrow moderato: la percentuale 0.72 vince sul cap", () => {
  const b = rightSidebarBounds(1000);
  assert.equal(b.min, 280);
  // 1000 * 0.72 = 720, sotto il cap 760.
  assert.equal(b.max, 720);
});

test("viewport mobile (<980): min scende a 240", () => {
  const b = rightSidebarBounds(900);
  assert.equal(b.min, 240);
  // 900 * 0.72 = 648.
  assert.equal(b.max, 648);
});

test("viewport minuscolo: max non scende mai sotto il min", () => {
  const b = rightSidebarBounds(200);
  assert.equal(b.min, 240);
  // 200 * 0.72 = 144 < 240 -> Math.max riporta al min.
  assert.equal(b.max, 240);
});

test("clampRightWidth: default 500 resta valido su viewport tipici", () => {
  // Su viewport ampio 500 e' dentro [280, 620].
  assert.equal(clampRightWidth(500, 1600), 500);
  // Su viewport narrow 500 e' dentro [280, 760].
  assert.equal(clampRightWidth(500, 1200), 500);
});

test("clampRightWidth: valori fuori range riportati ai bordi", () => {
  assert.equal(clampRightWidth(9999, 1600), 620);
  assert.equal(clampRightWidth(10, 1600), 280);
  assert.equal(clampRightWidth(9999, 1200), 760);
});

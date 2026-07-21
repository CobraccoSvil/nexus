// Unit test del formatter del pannello Monitor (node --test, type-stripping).
import { test } from "node:test";
import assert from "node:assert/strict";
import { formatMonitorValue, humanBytes } from "./monitor-format.ts";

test("humanBytes: soglie e separatore italiano", () => {
  assert.equal(humanBytes(128), "128 B");
  assert.equal(humanBytes(6729728), "6,4 MB"); // valore reale dallo screenshot
  assert.equal(humanBytes(1024), "1,0 KB");
  assert.equal(humanBytes(0), "0 B");
});

test("metriche di risorsa: umanizzate, MAI JSON grezzo", () => {
  // Payload REALE dello screenshot (vendita-immobile-backend.service).
  const v = { cpu_pct: 0, rss_bytes: 6729728, io_read_bytes: 3462, io_write_bytes: 0 };
  const out = formatMonitorValue(v);
  assert.ok(!out.includes("{"), `niente JSON grezzo: ${out}`);
  assert.ok(!out.includes("rss_bytes"), `niente chiavi grezze: ${out}`);
  assert.equal(out, "CPU 0% · RAM 6,4 MB · I/O 3,4 KB");
});

test("metriche di risorsa: I/O a zero viene omesso", () => {
  const v = { cpu_pct: 12.34, rss_bytes: 1048576, io_read_bytes: 0, io_write_bytes: 0 };
  assert.equal(formatMonitorValue(v), "CPU 12,3% · RAM 1,0 MB");
});

test("scalari invariati; oggetto generico non e' JSON", () => {
  assert.equal(formatMonitorValue(76), "76");
  assert.equal(formatMonitorValue("mistral/mistral-small-latest"), "mistral/mistral-small-latest");
  assert.equal(formatMonitorValue(null), "—");
  assert.equal(formatMonitorValue({ a: 1, b: "x" }), "a: 1 · b: x");
});

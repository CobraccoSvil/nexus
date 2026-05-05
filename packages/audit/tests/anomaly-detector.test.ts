import { describe, it, expect, beforeEach } from "vitest";
import { AnomalyDetector } from "../src/anomaly-detector.js";

let detector: AnomalyDetector;

beforeEach(() => {
  detector = new AnomalyDetector();
});

const baseParams = {
  tenant_id: "t1",
  request_id: "r1",
  input_tokens: 100,
  output_tokens: 100,
  sensitivity_tier: 1,
  finish_reason: "stop",
  injection_detected: false,
};

describe("AnomalyDetector.analyze", () => {
  it("no anomalies for normal request", () => {
    const events = detector.analyze(baseParams);
    expect(events).toHaveLength(0);
  });

  it("detects token spike", () => {
    const events = detector.analyze({
      ...baseParams,
      input_tokens: 30_000,
      output_tokens: 25_000, // 55k total > 50k threshold
    });
    expect(events.some((e) => e.type === "token_spike")).toBe(true);
  });

  it("detects injection attempt", () => {
    const events = detector.analyze({ ...baseParams, injection_detected: true });
    expect(events.some((e) => e.type === "injection_attempt")).toBe(true);
    expect(events[0].severity).toBe("high");
  });

  it("detects unusual finish reason", () => {
    const events = detector.analyze({ ...baseParams, finish_reason: "content_filter" });
    expect(events.some((e) => e.type === "unusual_finish_reason")).toBe(true);
  });

  it("detects tier escalation after 5 consecutive high-tier requests reaching tier 3", () => {
    const tiers = [1, 2, 2, 3, 3];
    for (const tier of tiers) {
      detector.analyze({ ...baseParams, sensitivity_tier: tier });
    }
    const events = detector.analyze({ ...baseParams, sensitivity_tier: 3 });
    expect(events.some((e) => e.type === "tier_escalation")).toBe(true);
  });

  it("resetTenant clears stats", () => {
    detector.analyze({ ...baseParams, input_tokens: 30_000, output_tokens: 25_000 });
    detector.resetTenant("t1");
    const events = detector.analyze(baseParams);
    expect(events).toHaveLength(0);
  });
});

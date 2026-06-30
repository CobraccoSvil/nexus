export type GuardrailResult = "allowed" | "refused" | "clarify";

export interface ParsedBlock {
  id: string;
  purpose: string;
  filePath: string;
  startLine: number;
  endLine: number;
  inputs: string[];
  outputs: string[];
  dependencies: string[];
  invariants: string[];
  throws: string[];
  sideEffects: string[];
  relatedBlocks: string[];
  steps: string[];
  warnings: string[];
  todos: string[];
  securityNotes: string[];
  performanceNotes: string[];
  codeHash: string;
  lastModified: string;
}

export interface QualityFinding {
  category: string;
  severity: "low" | "medium" | "high" | "critical";
  title: string;
  detail: string;
}

export interface DbFinding {
  objectName: string;
  severity: "low" | "medium" | "high" | "critical";
  detail: string;
}

export interface ValidationReport {
  status: "queued" | "running" | "passed" | "failed";
  scopes: string[];
  summary: string;
}

export interface ChangeReport {
  id: string;
  filesChanged: number;
  testsPassed: number;
  testsFailed: number;
  rollbackCommand: string;
}

export interface ExtractedPattern {
  id: string;
  category: "architecture" | "preference" | "bugfix" | "config" | "domain";
  name: string;
  confidence: number;
  occurrences: number;
  relatedFiles: string[];
}

export interface ProviderHealth {
  provider: string;
  status: string;
  latencyMs?: number;
}

export interface OrchestratorRunAudit {
  projectId: string;
  profileId: string;
  intent: string;
  provider: string;
  model: string;
  tokenBudget: number;
  resources: string[];
  guardrailResult: GuardrailResult;
}

export interface DashboardSnapshot {
  tokenUsage: {
    consumed: number;
    saved: number;
  };
  quality: {
    findings: number;
    shadowDbStatus: string;
  };
}


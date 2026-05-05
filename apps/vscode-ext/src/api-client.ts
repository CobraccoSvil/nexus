import * as http from "node:http";

function fetchJson<T>(url: string, options?: { method?: string; body?: string }): Promise<T> {
  return new Promise((resolve, reject) => {
    const parsed = new URL(url);
    const req = http.request(
      {
        hostname: parsed.hostname,
        port: parsed.port,
        path: parsed.pathname + parsed.search,
        method: options?.method || "GET",
        headers: { "Content-Type": "application/json" },
      },
      (res) => {
        let data = "";
        res.on("data", (chunk) => (data += chunk));
        res.on("end", () => {
          try {
            resolve(JSON.parse(data) as T);
          } catch {
            reject(new Error(`Invalid JSON from ${url}`));
          }
        });
      },
    );
    req.on("error", reject);
    if (options?.body) req.write(options.body);
    req.end();
  });
}

export interface HealthResponse {
  status: string;
  components: { database: boolean; redis: boolean; neural_core: boolean };
}

export interface DashboardResponse {
  total_runs: number;
  tokens_consumed: number;
  tokens_saved: number;
  quality_findings: number;
  active_jobs: number;
}

export interface ChatResponse {
  content: string;
  provider: string;
  model: string;
  tokens_used: number;
}

export interface IntentResponse {
  intent: string;
  confidence: string;
}

export class ApiClient {
  constructor(
    private mcpUrl: string,
    private neuralUrl: string,
  ) {}

  health(): Promise<HealthResponse> {
    return fetchJson(`${this.mcpUrl}/api/health`);
  }

  dashboard(): Promise<DashboardResponse> {
    return fetchJson(`${this.mcpUrl}/api/dashboard`);
  }

  chat(projectId: string, profileId: string, message: string): Promise<ChatResponse> {
    return fetchJson(`${this.mcpUrl}/api/chat`, {
      method: "POST",
      body: JSON.stringify({ project_id: projectId, profile_id: profileId, message }),
    });
  }

  classifyIntent(message: string): Promise<IntentResponse> {
    return fetchJson(`${this.neuralUrl}/classify-intent`, {
      method: "POST",
      body: JSON.stringify({ project_id: "vscode", profile_id: "default", message }),
    });
  }

  neuralHealth(): Promise<Record<string, string>> {
    return fetchJson(`${this.neuralUrl}/health`);
  }
}

import * as vscode from "vscode";
import { ApiClient } from "./api-client";
import { ChatViewProvider } from "./chat-view";
import { MetricsTreeProvider } from "./metrics-view";

export function activate(context: vscode.ExtensionContext) {
  const config = vscode.workspace.getConfiguration("ai-orchestrator");
  const mcpUrl = config.get<string>("mcpCoreUrl", "http://localhost:4000");
  const neuralUrl = config.get<string>("neuralCoreUrl", "http://localhost:8001");
  const api = new ApiClient(mcpUrl, neuralUrl);

  // Chat sidebar
  const chatProvider = new ChatViewProvider(context.extensionUri, api);
  context.subscriptions.push(
    vscode.window.registerWebviewViewProvider("ai-orchestrator.chatView", chatProvider),
  );

  // Metrics tree
  const metricsProvider = new MetricsTreeProvider(api);
  context.subscriptions.push(
    vscode.window.registerTreeDataProvider("ai-orchestrator.metricsView", metricsProvider),
  );

  // Commands
  context.subscriptions.push(
    vscode.commands.registerCommand("ai-orchestrator.chat", async () => {
      const message = await vscode.window.showInputBox({ prompt: "Ask the AI assistant" });
      if (!message) return;

      await vscode.window.withProgress(
        { location: vscode.ProgressLocation.Notification, title: "AI Orchestrator" },
        async (progress) => {
          progress.report({ message: "Thinking..." });
          try {
            const response = await api.chat("vscode", "default", message);
            const doc = await vscode.workspace.openTextDocument({
              content: `# AI Response\n\n**Provider:** ${response.provider}/${response.model}\n**Tokens:** ${response.tokens_used}\n\n---\n\n${response.content}`,
              language: "markdown",
            });
            await vscode.window.showTextDocument(doc, { preview: true });
          } catch (e) {
            vscode.window.showErrorMessage(`AI Chat failed: ${e instanceof Error ? e.message : e}`);
          }
        },
      );
    }),

    vscode.commands.registerCommand("ai-orchestrator.analyzeFile", async () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor) {
        vscode.window.showWarningMessage("No active file to analyze");
        return;
      }

      const text = editor.document.getText();
      const fileName = editor.document.fileName;
      const lines = text.split("\n");
      const todoCount = lines.filter((l) => /TODO|FIXME|HACK/i.test(l)).length;
      const unwrapCount = lines.filter((l) => l.includes(".unwrap()")).length;
      const longestFn = findLongestFunction(lines);

      const report = [
        `# Analysis: ${fileName.split(/[\\/]/).pop()}`,
        "",
        `| Metric | Value |`,
        `|--------|-------|`,
        `| Lines | ${lines.length} |`,
        `| TODO/FIXME/HACK markers | ${todoCount} |`,
        `| .unwrap() calls | ${unwrapCount} |`,
        `| Longest function | ${longestFn} lines |`,
      ].join("\n");

      const doc = await vscode.workspace.openTextDocument({ content: report, language: "markdown" });
      await vscode.window.showTextDocument(doc, { preview: true });
    }),

    vscode.commands.registerCommand("ai-orchestrator.classifyIntent", async () => {
      const message = await vscode.window.showInputBox({ prompt: "Enter a message to classify" });
      if (!message) return;

      try {
        const result = await api.classifyIntent(message);
        vscode.window.showInformationMessage(
          `Intent: ${result.intent} (confidence: ${result.confidence})`,
        );
      } catch (e) {
        vscode.window.showErrorMessage(`Classification failed: ${e instanceof Error ? e.message : e}`);
      }
    }),

    vscode.commands.registerCommand("ai-orchestrator.showDashboard", () => {
      metricsProvider.refresh();
      vscode.commands.executeCommand("ai-orchestrator.metricsView.focus");
    }),

    vscode.commands.registerCommand("ai-orchestrator.healthCheck", async () => {
      try {
        const [mcpHealth, neuralHealth] = await Promise.allSettled([
          api.health(),
          api.neuralHealth(),
        ]);

        const parts: string[] = [];
        if (mcpHealth.status === "fulfilled") {
          const h = mcpHealth.value;
          parts.push(`MCP Core: OK (DB: ${h.components.database ? "up" : "down"}, Redis: ${h.components.redis ? "up" : "down"})`);
        } else {
          parts.push("MCP Core: OFFLINE");
        }

        if (neuralHealth.status === "fulfilled") {
          parts.push(`Neural Core: ${neuralHealth.value.status}`);
        } else {
          parts.push("Neural Core: OFFLINE");
        }

        vscode.window.showInformationMessage(parts.join(" | "));
      } catch (e) {
        vscode.window.showErrorMessage(`Health check failed: ${e instanceof Error ? e.message : e}`);
      }
    }),
  );

  // Status bar
  const statusBar = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
  statusBar.text = "$(hubot) AI Orchestrator";
  statusBar.command = "ai-orchestrator.healthCheck";
  statusBar.tooltip = "Click to check AI Orchestrator health";
  statusBar.show();
  context.subscriptions.push(statusBar);

  // Auto-refresh metrics every 60s
  const timer = setInterval(() => metricsProvider.refresh(), 60_000);
  context.subscriptions.push({ dispose: () => clearInterval(timer) });

  console.log("AI Orchestrator v2 extension activated");
}

export function deactivate() {}

function findLongestFunction(lines: string[]): number {
  const fnRe = /(?:pub\s+)?(?:async\s+)?(?:fn|function|def)\s+\w+/;
  let longest = 0;
  for (let i = 0; i < lines.length; i++) {
    if (fnRe.test(lines[i])) {
      let depth = 0;
      let found = false;
      for (let j = i; j < lines.length; j++) {
        for (const ch of lines[j]) {
          if (ch === "{") { found = true; depth++; }
          else if (ch === "}" && found) {
            depth--;
            if (depth === 0) {
              longest = Math.max(longest, j - i + 1);
              break;
            }
          }
        }
        if (found && depth === 0) break;
      }
    }
  }
  return longest;
}

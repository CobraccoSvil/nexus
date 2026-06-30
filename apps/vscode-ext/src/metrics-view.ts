import * as vscode from "vscode";
import { ApiClient } from "./api-client";

export class MetricsTreeProvider implements vscode.TreeDataProvider<MetricItem> {
  private _onDidChangeTreeData = new vscode.EventEmitter<MetricItem | undefined>();
  readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

  private items: MetricItem[] = [];

  constructor(private readonly _api: ApiClient) {}

  refresh(): void {
    this._fetchMetrics().then(() => this._onDidChangeTreeData.fire(undefined));
  }

  getTreeItem(element: MetricItem): vscode.TreeItem {
    return element;
  }

  getChildren(): MetricItem[] {
    if (this.items.length === 0) {
      this._fetchMetrics().then(() => this._onDidChangeTreeData.fire(undefined));
    }
    return this.items;
  }

  private async _fetchMetrics(): Promise<void> {
    try {
      const dashboard = await this._api.dashboard();
      this.items = [
        new MetricItem("Total Runs", String(dashboard.total_runs), "symbol-event"),
        new MetricItem("Tokens Consumed", dashboard.tokens_consumed.toLocaleString(), "dashboard"),
        new MetricItem("Tokens Saved", dashboard.tokens_saved.toLocaleString(), "arrow-down"),
        new MetricItem("Quality Findings", String(dashboard.quality_findings), "warning"),
        new MetricItem("Active Jobs", String(dashboard.active_jobs), "loading~spin"),
      ];
    } catch {
      this.items = [new MetricItem("Backend Offline", "Connect to see metrics", "error")];
    }
  }
}

class MetricItem extends vscode.TreeItem {
  constructor(label: string, value: string, icon: string) {
    super(`${label}: ${value}`, vscode.TreeItemCollapsibleState.None);
    this.iconPath = new vscode.ThemeIcon(icon);
    this.tooltip = `${label}: ${value}`;
  }
}

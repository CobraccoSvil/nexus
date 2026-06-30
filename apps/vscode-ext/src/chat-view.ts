import * as vscode from "vscode";
import { ApiClient } from "./api-client";

export class ChatViewProvider implements vscode.WebviewViewProvider {
  private _view?: vscode.WebviewView;

  constructor(
    private readonly _extensionUri: vscode.Uri,
    private readonly _api: ApiClient,
  ) {}

  resolveWebviewView(webviewView: vscode.WebviewView) {
    this._view = webviewView;
    webviewView.webview.options = { enableScripts: true };
    webviewView.webview.html = this._getHtml();

    webviewView.webview.onDidReceiveMessage(async (msg) => {
      if (msg.type === "send") {
        try {
          const response = await this._api.chat("vscode", "default", msg.text);
          webviewView.webview.postMessage({
            type: "response",
            content: response.content,
            provider: response.provider,
            model: response.model,
          });
        } catch (e) {
          const errMsg = e instanceof Error ? e.message : "Connection failed";
          webviewView.webview.postMessage({
            type: "response",
            content: `[Error: ${errMsg}]`,
            provider: "error",
            model: "",
          });
        }
      }
    });
  }

  private _getHtml(): string {
    return `<!DOCTYPE html>
<html>
<head>
<style>
  body { font-family: var(--vscode-font-family); font-size: 13px; padding: 8px; color: var(--vscode-foreground); }
  #messages { max-height: 400px; overflow-y: auto; margin-bottom: 8px; }
  .msg { padding: 4px 0; line-height: 1.6; }
  .msg .role { font-weight: 600; }
  .msg .user { color: var(--vscode-textLink-foreground); }
  .msg .ai { color: var(--vscode-testing-iconPassed); }
  .msg .meta { font-size: 11px; opacity: 0.6; margin-left: 8px; }
  #input-area { display: flex; gap: 4px; }
  #input { flex: 1; padding: 6px; background: var(--vscode-input-background); color: var(--vscode-input-foreground); border: 1px solid var(--vscode-input-border); border-radius: 4px; font-size: 13px; }
  #send { padding: 6px 12px; background: var(--vscode-button-background); color: var(--vscode-button-foreground); border: none; border-radius: 4px; cursor: pointer; }
  .loading { opacity: 0.6; font-style: italic; }
</style>
</head>
<body>
  <div id="messages"></div>
  <div id="input-area">
    <input id="input" placeholder="Ask the AI..." />
    <button id="send">Send</button>
  </div>
  <script>
    const vscode = acquireVsCodeApi();
    const messagesEl = document.getElementById('messages');
    const inputEl = document.getElementById('input');
    const sendBtn = document.getElementById('send');

    function addMessage(role, content, meta) {
      const div = document.createElement('div');
      div.className = 'msg';
      const roleClass = role === 'You' ? 'user' : 'ai';
      div.innerHTML = '<span class="role ' + roleClass + '">' + role + ':</span> ' + content +
        (meta ? '<span class="meta">[' + meta + ']</span>' : '');
      messagesEl.appendChild(div);
      messagesEl.scrollTop = messagesEl.scrollHeight;
    }

    function send() {
      const text = inputEl.value.trim();
      if (!text) return;
      addMessage('You', text);
      inputEl.value = '';
      const loading = document.createElement('div');
      loading.className = 'msg loading';
      loading.textContent = 'Thinking...';
      messagesEl.appendChild(loading);
      vscode.postMessage({ type: 'send', text });
    }

    sendBtn.addEventListener('click', send);
    inputEl.addEventListener('keydown', (e) => { if (e.key === 'Enter') send(); });

    window.addEventListener('message', (e) => {
      const msg = e.data;
      if (msg.type === 'response') {
        const loading = messagesEl.querySelector('.loading');
        if (loading) loading.remove();
        const meta = msg.provider !== 'error' ? msg.provider + '/' + msg.model : '';
        addMessage('AI', msg.content, meta);
      }
    });
  </script>
</body>
</html>`;
  }
}

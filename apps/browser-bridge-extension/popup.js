// UI minima per configurare porta + token e gestire attach/detach del tab corrente.

const $ = (id) => document.getElementById(id);

async function load() {
  const v = await chrome.storage.local.get(["port", "token"]);
  if (v.port) $("port").value = v.port;
  if (v.token) $("token").value = v.token;
  await refreshStatus();
}

async function refreshStatus() {
  const port = Number($("port").value) || 4055;
  try {
    const r = await fetch(`http://127.0.0.1:${port}/health`);
    if (r.ok) setStatus(`Daemon raggiungibile su :${port}`, true);
    else setStatus(`daemon ha risposto ${r.status}`, false);
  } catch (e) {
    setStatus(`daemon non raggiungibile: ${e.message}`, false);
  }
}

function setStatus(text, ok) {
  const el = $("status");
  const dot = '<span class="dot"></span>';
  el.innerHTML = `${dot}<span>${text}</span>`;
  el.className = "status " + (ok ? "ok" : "err");
}

$("save").addEventListener("click", async () => {
  const port = Number($("port").value) || 4055;
  const token = $("token").value.trim();
  await chrome.storage.local.set({ port, token });
  setStatus("Salvato. Riconnessione in corso…", true);
  // Forza reload del service worker per riconnettere subito.
  await chrome.runtime.sendMessage({ type: "reconnect" }).catch(() => {});
  await refreshStatus();
});

$("attach").addEventListener("click", async () => {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab) return setStatus("nessun tab attivo", false);
  try {
    await chrome.debugger.attach({ tabId: tab.id }, "1.3");
    setStatus(`Tab ${tab.id} attached`, true);
    await chrome.runtime.sendMessage({ type: "tab_attached", tab_id: tab.id }).catch(() => {});
  } catch (e) {
    setStatus(`attach fallito: ${e.message}`, false);
  }
});

$("detach").addEventListener("click", async () => {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab) return setStatus("nessun tab attivo", false);
  try {
    await chrome.debugger.detach({ tabId: tab.id });
    setStatus(`Tab ${tab.id} detached`, true);
  } catch (e) {
    setStatus(`detach fallito: ${e.message}`, false);
  }
});

const refreshBtn = $("refresh");
if (refreshBtn) {
  refreshBtn.addEventListener("click", async () => {
    await refreshStatus();
  });
}

load();

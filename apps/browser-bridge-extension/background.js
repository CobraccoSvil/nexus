// Service worker MV3 per Nexus Browser Bridge.
//
// Responsabilita`:
//   - Mantenere una connessione WebSocket al daemon browser-bridge-mcp
//     (default 127.0.0.1:4055), autenticata con token letto da
//     http://127.0.0.1:<port>/handshake.
//   - Gestire chrome.debugger per ogni tab attached.
//   - Inoltrare al daemon eventi CDP (Runtime/Network) come BridgeEvent.
//   - Eseguire BridgeRequest in arrivo dal daemon (navigate/click/eval/...).
//
// Note di sicurezza:
//   - Solo loopback (127.0.0.1).
//   - Il token cambia ad ogni avvio del daemon: l'estensione lo rinegozia.
//   - Il service worker viene terminato dopo ~30s di idle: l'auto-reconnect
//     riparte al prossimo evento o quando l'utente apre il popup.

const DEFAULT_PORT = 4055;
const EXT_VERSION = "0.1.1";
const RECONNECT_MIN_MS = 1000;
const RECONNECT_MAX_MS = 30000;

let ws = null;
let reconnectDelay = RECONNECT_MIN_MS;
let attachedTabs = new Set();
let port = DEFAULT_PORT;

async function readStoredPort() {
  try {
    const v = await chrome.storage.local.get("port");
    if (v && v.port) port = Number(v.port) || DEFAULT_PORT;
  } catch (_) {
    /* fallback default */
  }
}

async function fetchToken() {
  // Probe handshake con token vuoto: il daemon risponde 401 sempre, ma
  // confermiamo la presenza con /health.
  const healthUrl = `http://127.0.0.1:${port}/health`;
  const r = await fetch(healthUrl, { method: "GET" });
  if (!r.ok) throw new Error(`daemon non raggiungibile su porta ${port}`);
  // Token deve arrivare dall'utente (popup) — leggiamo da storage.
  const v = await chrome.storage.local.get("token");
  if (!v || !v.token) throw new Error("token non configurato (apri il popup)");
  return v.token;
}

async function connect() {
  await readStoredPort();
  let token;
  try {
    token = await fetchToken();
  } catch (e) {
    console.warn("[bridge] connect fallito:", e.message);
    scheduleReconnect();
    return;
  }
  const url = `ws://127.0.0.1:${port}/ws?token=${encodeURIComponent(token)}`;
  try {
    ws = new WebSocket(url);
  } catch (e) {
    console.warn("[bridge] WebSocket throw:", e);
    scheduleReconnect();
    return;
  }

  ws.onopen = () => {
    reconnectDelay = RECONNECT_MIN_MS;
    sendMessage({ kind: "hello", ext_version: EXT_VERSION });
    console.info("[bridge] connesso al daemon su porta", port);
  };
  ws.onclose = () => {
    ws = null;
    scheduleReconnect();
  };
  ws.onerror = (e) => console.warn("[bridge] ws error", e?.message || e);
  ws.onmessage = (ev) => onWsMessage(ev.data);
}

function scheduleReconnect() {
  setTimeout(connect, reconnectDelay);
  reconnectDelay = Math.min(reconnectDelay * 2, RECONNECT_MAX_MS);
}

function sendMessage(obj) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify(obj));
  }
}

function sendResponse(request_id, ok, data = null, error = null) {
  sendMessage({
    kind: "response",
    request_id,
    ok,
    data,
    error,
  });
}

function sendEvent(event, payload) {
  sendMessage({
    kind: "event",
    event,
    ts_ms: Date.now(),
    ...payload,
  });
}

// ---------- Dispatch BridgeRequest ----------

async function onWsMessage(text) {
  let req;
  try {
    req = JSON.parse(text);
  } catch (e) {
    console.warn("[bridge] payload invalido:", e);
    return;
  }
  const rid = req.request_id;
  try {
    switch (req.kind) {
      case "heartbeat":
        return;
      case "list_tabs":
        return sendResponse(rid, true, await listTabs());
      case "attach_tab":
        await attachTab(req.tab_id);
        return sendResponse(rid, true, { tab_id: req.tab_id });
      case "detach_tab":
        await detachTab(req.tab_id);
        return sendResponse(rid, true, { tab_id: req.tab_id });
      case "navigate":
        return sendResponse(rid, true, await navigate(req));
      case "click":
        return sendResponse(rid, true, await click(req));
      case "fill":
        return sendResponse(rid, true, await fill(req));
      case "scroll":
        return sendResponse(rid, true, await scroll(req));
      case "screenshot":
        return sendResponse(rid, true, await screenshot(req));
      case "snapshot_dom":
        return sendResponse(rid, true, await snapshotDom(req));
      case "eval":
        return sendResponse(rid, true, await evalJs(req));
      default:
        return sendResponse(rid, false, null, `kind sconosciuto: ${req.kind}`);
    }
  } catch (e) {
    sendResponse(rid, false, null, String(e && e.message ? e.message : e));
  }
}

// ---------- Helpers tab + CDP ----------

async function resolveTab(tabId) {
  if (tabId) return tabId;
  if (attachedTabs.size === 1) return [...attachedTabs][0];
  const [active] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!active) throw new Error("nessun tab attivo");
  return active.id;
}

async function listTabs() {
  const all = await chrome.tabs.query({});
  return {
    tabs: all.map((t) => ({
      id: t.id,
      url: t.url,
      title: t.title,
      attached: attachedTabs.has(t.id),
      active: t.active,
    })),
  };
}

async function attachTab(tabId) {
  if (attachedTabs.has(tabId)) return;
  await chrome.debugger.attach({ tabId }, "1.3");
  attachedTabs.add(tabId);
  await chrome.debugger.sendCommand({ tabId }, "Runtime.enable");
  await chrome.debugger.sendCommand({ tabId }, "Network.enable");
  await chrome.debugger.sendCommand({ tabId }, "Page.enable");
}

async function detachTab(tabId) {
  if (!attachedTabs.has(tabId)) return;
  try {
    await chrome.debugger.detach({ tabId });
  } catch (_) {
    /* gia` detached */
  }
  attachedTabs.delete(tabId);
  sendEvent("tab_detached", { tab_id: tabId });
}

async function cdp(tabId, method, params = {}) {
  return await chrome.debugger.sendCommand({ tabId }, method, params);
}

async function navigate(req) {
  const tab = await resolveTab(req.tab_id);
  if (!attachedTabs.has(tab)) await attachTab(tab);
  const r = await cdp(tab, "Page.navigate", { url: req.url });
  return { tab_id: tab, frame_id: r?.frameId || null };
}

async function click(req) {
  const tab = await resolveTab(req.tab_id);
  if (!attachedTabs.has(tab)) await attachTab(tab);
  if (req.selector) {
    const expr = `(() => {
      const el = document.querySelector(${JSON.stringify(req.selector)});
      if (!el) return { ok: false, error: 'selector non trovato' };
      el.scrollIntoView({ block: 'center' });
      const r = el.getBoundingClientRect();
      return { ok: true, x: r.left + r.width/2, y: r.top + r.height/2 };
    })()`;
    const res = await cdp(tab, "Runtime.evaluate", { expression: expr, returnByValue: true });
    const v = res?.result?.value || {};
    if (!v.ok) throw new Error(v.error || "selector non risolto");
    await dispatchMouse(tab, v.x, v.y);
    return { clicked_at: { x: v.x, y: v.y } };
  }
  await dispatchMouse(tab, req.x, req.y);
  return { clicked_at: { x: req.x, y: req.y } };
}

async function dispatchMouse(tabId, x, y) {
  await cdp(tabId, "Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "left", clickCount: 1 });
  await cdp(tabId, "Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "left", clickCount: 1 });
}

async function fill(req) {
  const tab = await resolveTab(req.tab_id);
  if (!attachedTabs.has(tab)) await attachTab(tab);
  const value = atob(req.value_b64);
  const expr = `(() => {
    const el = document.querySelector(${JSON.stringify(req.selector)});
    if (!el) return { ok: false, error: 'selector non trovato' };
    el.focus();
    const setter = Object.getOwnPropertyDescriptor(el.__proto__, 'value')?.set;
    if (setter) setter.call(el, ${JSON.stringify(value)});
    else el.value = ${JSON.stringify(value)};
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
    return { ok: true };
  })()`;
  const res = await cdp(tab, "Runtime.evaluate", { expression: expr, returnByValue: true });
  const v = res?.result?.value || {};
  if (!v.ok) throw new Error(v.error || "fill fallito");
  return { ok: true };
}

async function scroll(req) {
  const tab = await resolveTab(req.tab_id);
  if (!attachedTabs.has(tab)) await attachTab(tab);
  const expr = req.selector
    ? `(() => { const el = document.querySelector(${JSON.stringify(req.selector)});
        if (!el) return { ok:false, error:'selector non trovato' };
        el.scrollBy(${req.dx}, ${req.dy}); return { ok:true }; })()`
    : `(() => { window.scrollBy(${req.dx}, ${req.dy}); return { ok:true }; })()`;
  const res = await cdp(tab, "Runtime.evaluate", { expression: expr, returnByValue: true });
  const v = res?.result?.value || {};
  if (!v.ok) throw new Error(v.error || "scroll fallito");
  return { ok: true };
}

async function screenshot(req) {
  const tab = await resolveTab(req.tab_id);
  if (!attachedTabs.has(tab)) await attachTab(tab);
  const r = await cdp(tab, "Page.captureScreenshot", {
    format: "png",
    captureBeyondViewport: !!req.full_page,
  });
  return { png_base64: r?.data || "" };
}

async function snapshotDom(req) {
  const tab = await resolveTab(req.tab_id);
  if (!attachedTabs.has(tab)) await attachTab(tab);
  if (req.mode === "html") {
    const r = await cdp(tab, "Runtime.evaluate", {
      expression: "document.documentElement.outerHTML",
      returnByValue: true,
    });
    return { html: r?.result?.value || "" };
  }
  // ARIA tree via Accessibility domain.
  await cdp(tab, "Accessibility.enable");
  const tree = await cdp(tab, "Accessibility.getFullAXTree", {});
  return { ax_tree: tree?.nodes || [] };
}

async function evalJs(req) {
  const tab = await resolveTab(req.tab_id);
  if (!attachedTabs.has(tab)) await attachTab(tab);
  const expression = atob(req.expression_b64);
  const r = await cdp(tab, "Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise: !!req.await_promise,
    userGesture: true,
  });
  if (r?.exceptionDetails) {
    throw new Error(r.exceptionDetails.text || "exception in eval");
  }
  return { value: r?.result?.value ?? null, type: r?.result?.type || "undefined" };
}

// ---------- Eventi CDP -> BridgeEvent ----------

chrome.debugger.onEvent.addListener((source, method, params) => {
  const tabId = source.tabId;
  if (!attachedTabs.has(tabId)) return;
  switch (method) {
    case "Runtime.consoleAPICalled": {
      const text = (params.args || []).map(stringifyArg).join(" ");
      sendEvent("console_log", { tab_id: tabId, level: params.type || "log", text });
      break;
    }
    case "Runtime.exceptionThrown": {
      const text = params?.exceptionDetails?.text || JSON.stringify(params);
      sendEvent("exception", { tab_id: tabId, text });
      break;
    }
    case "Network.requestWillBeSent": {
      sendEvent("network_request", {
        tab_id: tabId,
        method: params?.request?.method || "GET",
        url: params?.request?.url || "",
      });
      break;
    }
    case "Network.responseReceived": {
      sendEvent("network_response", {
        tab_id: tabId,
        url: params?.response?.url || "",
        status: params?.response?.status || 0,
      });
      break;
    }
    case "Network.loadingFailed": {
      sendEvent("network_failed", {
        tab_id: tabId,
        url: params?.request?.url || "",
        error: params?.errorText || "unknown",
      });
      break;
    }
  }
});

chrome.debugger.onDetach.addListener((source, _reason) => {
  if (source.tabId && attachedTabs.has(source.tabId)) {
    attachedTabs.delete(source.tabId);
    sendEvent("tab_detached", { tab_id: source.tabId });
  }
});

chrome.tabs.onRemoved.addListener((tabId) => {
  if (attachedTabs.has(tabId)) {
    attachedTabs.delete(tabId);
    sendEvent("tab_detached", { tab_id: tabId });
  }
});

function stringifyArg(arg) {
  if (arg.value !== undefined) return String(arg.value);
  if (arg.description) return arg.description;
  return arg.type || "?";
}

// Boot.
connect();

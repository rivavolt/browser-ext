// browser-ext service worker.
//
// Maintains a persistent native-messaging port to the Rust host. The host
// also listens on a Unix socket for CLI clients and relays their requests
// here as { id, method, params } messages; we answer with { id, result } or
// { id, error }. Each request is dispatched to a handler that wraps the
// chrome.* APIs.

const NATIVE_HOST = 'com.browser_ext.host';

let port = null;
let reconnectTimer = null;

function connect() {
  if (port) return;

  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }

  try {
    port = chrome.runtime.connectNative(NATIVE_HOST);
  } catch (e) {
    console.error(`[browser-ext] connectNative failed: ${e}`);
    scheduleReconnect();
    return;
  }

  console.log('[browser-ext] connected to native host');

  port.onMessage.addListener((msg) => {
    handleRequest(msg).catch((e) => {
      console.error(`[browser-ext] dispatch error: ${e?.stack || e}`);
    });
  });

  port.onDisconnect.addListener(() => {
    console.error(
      `[browser-ext] native host disconnected: ${chrome.runtime.lastError?.message || ''}`,
    );
    port = null;
    scheduleReconnect();
  });
}

function scheduleReconnect() {
  if (!reconnectTimer) {
    reconnectTimer = setTimeout(connect, 3000);
  }
}

function send(msg) {
  if (!port) {
    console.error('[browser-ext] send dropped, no port');
    return;
  }
  try {
    port.postMessage(msg);
  } catch (e) {
    console.error(`[browser-ext] postMessage failed: ${e}`);
  }
}

// --- Request dispatch ---

async function handleRequest(msg) {
  const { id, method, params } = msg;
  if (id === undefined || method === undefined) return;

  const handler = HANDLERS[method];
  if (!handler) {
    send({ id, error: `unknown method: ${method}` });
    return;
  }

  try {
    const result = await handler(params || {});
    send({ id, result });
  } catch (e) {
    send({ id, error: e?.message || String(e) });
  }
}

// --- Handlers ---
//
// Keep each handler small and self-contained so new verbs (eval, activate,
// close, open, navigate, reload, screenshot) drop in as extra entries.

const HANDLERS = {
  'tabs.list': async () => {
    const tabs = await chrome.tabs.query({});
    return tabs.map((t) => ({
      id: t.id,
      windowId: t.windowId,
      title: t.title ?? '',
      url: t.url ?? t.pendingUrl ?? '',
      active: !!t.active,
      pinned: !!t.pinned,
    }));
  },

  'tabs.content': async ({ id }) => {
    const tabId = requireTabId(id);
    const [{ result } = {}] = await chrome.scripting.executeScript({
      target: { tabId },
      func: extractReadableText,
    });
    return { id: tabId, text: result ?? '' };
  },

  'windows.list': async () => {
    const windows = await chrome.windows.getAll({ populate: true });
    return windows.map((w) => ({
      id: w.id,
      focused: !!w.focused,
      tabCount: w.tabs ? w.tabs.length : 0,
    }));
  },
};

function requireTabId(id) {
  const n = Number(id);
  if (!Number.isInteger(n)) {
    throw new Error(`invalid tab id: ${id}`);
  }
  return n;
}

// Runs in the page. Returns the visible text of the document, preferring the
// <body> and collapsing whitespace so the CLI gets something readable.
function extractReadableText() {
  const root = document.body || document.documentElement;
  if (!root) return '';
  const text = root.innerText || root.textContent || '';
  return text.replace(/[ \t]+\n/g, '\n').replace(/\n{3,}/g, '\n\n').trim();
}

// --- Init ---

connect();
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);

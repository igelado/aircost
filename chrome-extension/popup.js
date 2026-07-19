const SIGNATURE_PREFIX = "aircost-plugin-v1";

const setupView = document.querySelector("#setup-view");
const captureView = document.querySelector("#capture-view");
const registerButton = document.querySelector("#register-button");
const submitButton = document.querySelector("#submit-button");
const resetButton = document.querySelector("#reset-button");
const statusOutput = document.querySelector("#status");

document.addEventListener("DOMContentLoaded", refreshView);
registerButton.addEventListener("click", registerPlugin);
submitButton.addEventListener("click", submitCurrentPage);
resetButton.addEventListener("click", resetConfig);

async function refreshView() {
  const config = await loadConfig();
  if (config?.pluginInstallId && config?.privateKeyJwk && config?.serverUrl && config?.username) {
    document.querySelector("#configured-server").textContent = config.serverUrl;
    document.querySelector("#configured-user").textContent = config.username;
    document.querySelector("#configured-install").textContent = String(config.pluginInstallId);
    setupView.classList.add("hidden");
    captureView.classList.remove("hidden");
    setStatus("Ready");
  } else {
    setupView.classList.remove("hidden");
    captureView.classList.add("hidden");
    setStatus("Register this browser first.");
  }
}

async function registerPlugin() {
  try {
    setBusy(registerButton, true);
    const serverUrl = normalizeServerUrl(document.querySelector("#server-url").value);
    const username = document.querySelector("#username").value.trim() || "developer";
    const keyPair = await crypto.subtle.generateKey(
      { name: "ECDSA", namedCurve: "P-256" },
      true,
      ["sign", "verify"],
    );
    const publicKeyRaw = await crypto.subtle.exportKey("raw", keyPair.publicKey);
    const privateKeyJwk = await crypto.subtle.exportKey("jwk", keyPair.privateKey);
    const publicKeyBase64 = arrayBufferToBase64(publicKeyRaw);

    const response = await fetch(`${serverUrl}/api/plugin/register`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-User-Email": username,
      },
      body: JSON.stringify({ public_key_base64: publicKeyBase64 }),
    });
    const payload = await parseJsonResponse(response);
    const pluginInstallId = payload.plugin_install.id;

    await chrome.storage.local.set({
      serverUrl,
      username,
      pluginInstallId,
      privateKeyJwk,
      publicKeyBase64,
    });
    setStatus(`Registered install ${pluginInstallId}`);
    await refreshView();
  } catch (error) {
    setStatus(error.message);
  } finally {
    setBusy(registerButton, false);
  }
}

async function submitCurrentPage() {
  try {
    setBusy(submitButton, true);
    const config = await loadConfig();
    if (!config?.pluginInstallId || !config?.privateKeyJwk) {
      throw new Error("Plugin is not registered.");
    }

    const capture = await captureActiveTab();
    const htmlHash = await sha256Hex(capture.renderedHtml);
    const message = `${SIGNATURE_PREFIX}\n${config.pluginInstallId}\n${capture.sourceUrl}\n${htmlHash}`;
    const privateKey = await crypto.subtle.importKey(
      "jwk",
      config.privateKeyJwk,
      { name: "ECDSA", namedCurve: "P-256" },
      false,
      ["sign"],
    );
    const signature = await crypto.subtle.sign(
      { name: "ECDSA", hash: "SHA-256" },
      privateKey,
      new TextEncoder().encode(message),
    );

    const response = await fetch(`${config.serverUrl}/api/plugin/submissions`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-User-Email": config.username,
      },
      body: JSON.stringify({
        plugin_install_id: config.pluginInstallId,
        source_url: capture.sourceUrl,
        rendered_html: capture.renderedHtml,
        signature: arrayBufferToBase64(signature),
      }),
    });
    const payload = await parseJsonResponse(response);
    const listingId = payload.listing?.id ? `\nlisting: ${payload.listing.id}` : "";
    const error = payload.submission?.extraction_error
      ? `\nextraction: ${payload.submission.extraction_error}`
      : "";
    setStatus(`submitted: ${payload.submission.id}${listingId}${error}`);
  } catch (error) {
    setStatus(error.message);
  } finally {
    setBusy(submitButton, false);
  }
}

async function captureActiveTab() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab?.id) {
    throw new Error("No active tab.");
  }
  const [result] = await chrome.scripting.executeScript({
    target: { tabId: tab.id },
    func: () => ({
      sourceUrl: window.location.href,
      renderedHtml: document.documentElement.outerHTML,
    }),
  });
  if (!result?.result?.renderedHtml) {
    throw new Error("Could not capture rendered HTML.");
  }
  return result.result;
}

async function resetConfig() {
  await chrome.storage.local.clear();
  setStatus("Configuration reset.");
  await refreshView();
}

async function loadConfig() {
  return chrome.storage.local.get([
    "serverUrl",
    "username",
    "pluginInstallId",
    "privateKeyJwk",
    "publicKeyBase64",
  ]);
}

async function parseJsonResponse(response) {
  const payload = await response.json().catch(() => null);
  if (!response.ok) {
    const message = payload?.error?.message || `HTTP ${response.status}`;
    throw new Error(message);
  }
  return payload;
}

async function sha256Hex(value) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return Array.from(new Uint8Array(digest))
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

function arrayBufferToBase64(buffer) {
  const bytes = new Uint8Array(buffer);
  let binary = "";
  for (let index = 0; index < bytes.length; index += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(index, index + 0x8000));
  }
  return btoa(binary);
}

function normalizeServerUrl(value) {
  const trimmed = value.trim().replace(/\/+$/, "");
  if (!trimmed) {
    throw new Error("Server is required.");
  }
  return trimmed;
}

function setBusy(button, busy) {
  button.disabled = busy;
}

function setStatus(message) {
  statusOutput.textContent = message;
}

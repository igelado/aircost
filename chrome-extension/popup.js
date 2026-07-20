const SIGNATURE_PREFIX = "aircost-plugin-v1";

const STAGE_LABELS = {
  capturing_page: "Capturing page",
  signing_upload: "Signing upload",
  sending_upload: "Sending data",
  received_upload: "Received by server",
  verifying_upload: "Verifying data",
  extracting_listing: "Extracting listing",
  verifying_listing: "Checking fields",
  normalizing_aircraft: "Normalizing aircraft",
  normalizing_avionics: "Normalizing avionics",
  saving_listing: "Saving listing",
  refreshing_estimates: "Computing estimates",
  recording_submission: "Recording submission",
  complete: "Complete",
  error: "Error",
};

const setupView = document.querySelector("#setup-view");
const captureView = document.querySelector("#capture-view");
const registerButton = document.querySelector("#register-button");
const submitButton = document.querySelector("#submit-button");
const refreshStatusButton = document.querySelector("#refresh-status-button");
const resetButton = document.querySelector("#reset-button");
const statusOutput = document.querySelector("#status");
const submissionBadge = document.querySelector("#submission-badge");
const currentUrlOutput = document.querySelector("#current-url");
const progressList = document.querySelector("#progress-list");
const listingEditor = document.querySelector("#listing-editor");
const listingEditorForm = document.querySelector("#listing-editor-form");
const listingIdLabel = document.querySelector("#listing-id-label");
const addAvionicsButton = document.querySelector("#add-avionics-button");
const avionicsList = document.querySelector("#avionics-list");

let activeListing = null;
let activeSubmission = null;

document.addEventListener("DOMContentLoaded", refreshView);
registerButton.addEventListener("click", registerPlugin);
submitButton.addEventListener("click", submitCurrentPage);
refreshStatusButton.addEventListener("click", refreshListingStatus);
resetButton.addEventListener("click", resetConfig);
listingEditorForm.addEventListener("submit", saveListingEdits);
addAvionicsButton.addEventListener("click", () => addAvionicsRow());

async function refreshView() {
  const config = await loadConfig();
  if (config?.pluginInstallId && config?.privateKeyJwk && config?.serverUrl && config?.username) {
    document.querySelector("#configured-server").textContent = config.serverUrl;
    document.querySelector("#configured-user").textContent = config.username;
    document.querySelector("#configured-install").textContent = String(config.pluginInstallId);
    setupView.classList.add("hidden");
    captureView.classList.remove("hidden");
    setStatus("Checking current page.");
    await refreshListingStatus();
  } else {
    setupView.classList.remove("hidden");
    captureView.classList.add("hidden");
    clearListingState();
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

async function refreshListingStatus() {
  try {
    setBusy(refreshStatusButton, true);
    const config = await loadConfig();
    if (!config?.serverUrl || !config?.username) {
      return;
    }
    const capture = await captureActiveTab({ includeHtml: false });
    currentUrlOutput.textContent = capture.sourceUrl;
    const url = new URL(`${config.serverUrl}/api/plugin/submissions/status`);
    url.searchParams.set("source_url", capture.sourceUrl);
    const response = await fetch(url.toString(), {
      headers: {
        "X-User-Email": config.username,
      },
    });
    const payload = await parseJsonResponse(response);
    setSubmissionState(payload);
    await setActionBadge(Boolean(payload.submitted), capture.tabId);
    setStatus(payload.submitted ? "This listing has been uploaded." : "This listing has not been uploaded.");
  } catch (error) {
    clearListingState();
    setSubmissionBadge("error", "Status error");
    setStatus(error.message);
  } finally {
    setBusy(refreshStatusButton, false);
  }
}

async function submitCurrentPage() {
  try {
    setBusy(submitButton, true);
    resetProgress();
    const config = await loadConfig();
    if (!config?.pluginInstallId || !config?.privateKeyJwk) {
      throw new Error("Plugin is not registered.");
    }

    recordProgress("capturing_page", "running", "Reading the active tab.");
    const capture = await captureActiveTab({ includeHtml: true });
    currentUrlOutput.textContent = capture.sourceUrl;
    recordProgress("capturing_page", "complete", "Page captured.");

    recordProgress("signing_upload", "running", "Signing the captured document.");
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
    recordProgress("signing_upload", "complete", "Signature ready.");

    recordProgress("sending_upload", "running", "Uploading rendered HTML.");
    const response = await fetch(`${config.serverUrl}/api/plugin/submissions/stream`, {
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
    const payload = await readProgressResponse(response);
    recordProgress("sending_upload", "complete", "Upload delivered.");
    setSubmissionState(payload);
    await setActionBadge(true, capture.tabId);
    const listingId = payload.listing?.id ? ` listing ${payload.listing.id}` : "";
    const error = payload.submission?.extraction_error
      ? ` Extraction issue: ${payload.submission.extraction_error}`
      : "";
    setStatus(`Submitted submission ${payload.submission?.id || "-"}${listingId}.${error}`);
  } catch (error) {
    recordProgress("error", "error", error.message);
    setSubmissionBadge("error", "Upload error");
    setStatus(error.message);
  } finally {
    setBusy(submitButton, false);
  }
}

async function saveListingEdits(event) {
  event.preventDefault();
  if (!activeListing?.id) {
    setStatus("No uploaded listing is available to edit.");
    return;
  }
  try {
    const config = await loadConfig();
    setBusy(document.querySelector("#save-listing-button"), true);
    setStatus("Saving listing edits.");
    const response = await fetch(`${config.serverUrl}/api/listings/${activeListing.id}`, {
      method: "PATCH",
      headers: {
        "Content-Type": "application/json",
        "X-User-Email": config.username,
      },
      body: JSON.stringify({ listing: readListingForm() }),
    });
    const payload = await parseJsonResponse(response);
    setSubmissionState({
      submitted: true,
      submission: activeSubmission,
      listing: payload.listing,
      listing_estimate: payload.listing_estimate,
    });
    setStatus(`Updated listing ${payload.listing.id}.`);
  } catch (error) {
    setStatus(error.message);
  } finally {
    setBusy(document.querySelector("#save-listing-button"), false);
  }
}

async function readProgressResponse(response) {
  if (!response.ok) {
    await parseJsonResponse(response);
  }
  const contentType = response.headers.get("content-type") || "";
  if (!response.body || !contentType.includes("application/x-ndjson")) {
    return parseJsonResponse(response);
  }

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let finalPayload = null;

  while (true) {
    const { value, done } = await reader.read();
    if (done) {
      break;
    }
    buffer += decoder.decode(value, { stream: true });
    const lines = buffer.split("\n");
    buffer = lines.pop() || "";
    for (const line of lines) {
      const event = parseProgressLine(line);
      if (event) {
        handleProgressEvent(event);
        if (event.status === "error") {
          throw new Error(event.message || "Upload failed.");
        }
        if (event.stage === "complete") {
          finalPayload = event;
        }
      }
    }
  }

  const finalEvent = parseProgressLine(buffer);
  if (finalEvent) {
    handleProgressEvent(finalEvent);
    if (finalEvent.status === "error") {
      throw new Error(finalEvent.message || "Upload failed.");
    }
    if (finalEvent.stage === "complete") {
      finalPayload = finalEvent;
    }
  }

  if (!finalPayload) {
    throw new Error("Upload finished without a completion response.");
  }
  return finalPayload;
}

function parseProgressLine(line) {
  const trimmed = line.trim();
  if (!trimmed) {
    return null;
  }
  try {
    return JSON.parse(trimmed);
  } catch (error) {
    throw new Error(`Invalid progress update: ${error.message}`);
  }
}

function handleProgressEvent(event) {
  const stage = event.stage || "upload";
  const status = event.status || "running";
  recordProgress(stage, status, event.message || STAGE_LABELS[stage] || stage);
}

function recordProgress(stage, status, message) {
  if (status === "running") {
    markRunningStagesComplete(stage);
  }
  let item = Array.from(progressList.children).find((node) => node.dataset.stage === stage);
  if (!item) {
    item = document.createElement("li");
    item.dataset.stage = stage;
    const title = document.createElement("span");
    title.className = "progress-title";
    const body = document.createElement("span");
    body.className = "progress-message";
    const content = document.createElement("span");
    content.append(title, body);
    item.append(content);
    progressList.append(item);
  }
  item.className = `is-${status}`;
  item.querySelector(".progress-title").textContent = STAGE_LABELS[stage] || stage;
  item.querySelector(".progress-message").textContent = message || "";
  if (stage === "complete") {
    markRunningStagesComplete(stage);
  }
}

function markRunningStagesComplete(exceptStage) {
  for (const item of progressList.children) {
    if (item.dataset.stage !== exceptStage && item.classList.contains("is-running")) {
      item.className = "is-complete";
    }
  }
}

function resetProgress() {
  progressList.replaceChildren();
}

function setSubmissionState(payload) {
  activeSubmission = payload.submission || null;
  activeListing = payload.listing || null;
  if (payload.submitted || activeSubmission || activeListing) {
    const label = activeSubmission?.id ? `Uploaded #${activeSubmission.id}` : "Uploaded";
    setSubmissionBadge("submitted", label);
  } else {
    activeSubmission = null;
    setSubmissionBadge("new", "Not uploaded");
  }
  if (activeListing) {
    populateListingForm(activeListing);
    listingEditor.classList.remove("hidden");
  } else {
    listingEditor.classList.add("hidden");
  }
}

function clearListingState() {
  activeListing = null;
  activeSubmission = null;
  currentUrlOutput.textContent = "";
  listingEditor.classList.add("hidden");
  resetProgress();
  setSubmissionBadge("unknown", "Checking");
}

function setSubmissionBadge(kind, text) {
  submissionBadge.className = `status-pill is-${kind}`;
  submissionBadge.textContent = text;
}

function populateListingForm(listing) {
  listingIdLabel.textContent = `#${listing.id}`;
  setField("manufacturer", listing.aircraft?.manufacturer || "");
  setField("model", listing.aircraft?.model || "");
  setField("variant", listing.aircraft?.variant || "");
  setField("model_year", listing.model_year);
  setField("asking_price_usd", listing.asking_price_usd);
  setField("currency", listing.currency || "USD");
  setField("status", listing.status || "active");
  setField("registration_number", listing.registration_number || "");
  setField("serial_number", listing.serial_number || "");
  setField("airframe_hours", listing.airframe_hours);
  setField("engine_hours", listing.engine_hours);
  setField("propeller_hours", listing.propeller_hours);
  avionicsList.replaceChildren();
  const avionics = listing.avionics?.length ? listing.avionics : [{}];
  for (const item of avionics) {
    addAvionicsRow(item);
  }
  const disabled = Boolean(listing.is_verified);
  for (const field of listingEditorForm.elements) {
    field.disabled = disabled;
  }
  addAvionicsButton.disabled = disabled;
}

function readListingForm() {
  const data = new FormData(listingEditorForm);
  return {
    manufacturer: requiredText(data, "manufacturer"),
    model: requiredText(data, "model"),
    variant: requiredText(data, "variant"),
    model_year: requiredInteger(data, "model_year"),
    asking_price_usd: requiredNumber(data, "asking_price_usd"),
    currency: requiredText(data, "currency").toUpperCase(),
    status: data.get("status") || "active",
    registration_number: optionalText(data, "registration_number"),
    serial_number: optionalText(data, "serial_number"),
    airframe_hours: requiredNumber(data, "airframe_hours"),
    engine_hours: requiredNumber(data, "engine_hours"),
    propeller_hours: requiredNumber(data, "propeller_hours"),
    avionics: readAvionicsRows(),
  };
}

function addAvionicsRow(item = {}) {
  const row = document.createElement("div");
  row.className = "avionics-row";
  row.append(
    avionicsInput("avionics_manufacturer", "Maker", item.manufacturer),
    avionicsInput("avionics_model", "Model", item.model),
    avionicsTypeSelect(item.type || item.avionics_type),
    avionicsInput("avionics_quantity", "Qty", item.quantity || 1, "number"),
  );
  const remove = document.createElement("button");
  remove.className = "icon-button";
  remove.type = "button";
  remove.textContent = "x";
  remove.setAttribute("aria-label", "Remove avionics");
  remove.title = "Remove avionics";
  remove.addEventListener("click", () => {
    row.remove();
    if (!avionicsList.children.length) {
      addAvionicsRow();
    }
  });
  row.append(remove);
  avionicsList.append(row);
}

function avionicsInput(name, placeholder, value = "", type = "text") {
  const input = document.createElement("input");
  input.name = name;
  input.type = type;
  input.placeholder = placeholder;
  input.setAttribute("aria-label", placeholder);
  input.value = value ?? "";
  if (type === "number") {
    input.min = "1";
    input.step = "1";
  }
  return input;
}

function avionicsTypeSelect(value = "Unknown") {
  const select = document.createElement("select");
  select.name = "avionics_type";
  select.setAttribute("aria-label", "Avionics type");
  for (const optionValue of ["PFD", "MFD", "NAV/COM", "GPS", "Autopilot", "Transponder", "Audio Panel", "Engine Monitor", "Unknown"]) {
    const option = document.createElement("option");
    option.value = optionValue;
    option.textContent = optionValue;
    select.append(option);
  }
  select.value = value || "Unknown";
  return select;
}

function readAvionicsRows() {
  const avionics = [];
  for (const row of Array.from(avionicsList.querySelectorAll(".avionics-row"))) {
    const manufacturer = row.querySelector('[name="avionics_manufacturer"]').value.trim();
    const model = row.querySelector('[name="avionics_model"]').value.trim();
    const type = row.querySelector('[name="avionics_type"]').value;
    const quantity = Number.parseInt(row.querySelector('[name="avionics_quantity"]').value, 10) || 1;
    if (!manufacturer && !model) {
      continue;
    }
    if (!manufacturer || !model) {
      throw new Error("Avionics rows need maker and model.");
    }
    avionics.push({
      manufacturer,
      model,
      type,
      quantity: Math.max(quantity, 1),
    });
  }
  return avionics;
}

function setField(name, value) {
  const field = listingEditorForm.elements[name];
  if (field) {
    field.value = value ?? "";
  }
}

function requiredText(data, name) {
  const value = String(data.get(name) || "").trim();
  if (!value) {
    throw new Error(`${name} is required.`);
  }
  return value;
}

function optionalText(data, name) {
  const value = String(data.get(name) || "").trim();
  return value || null;
}

function requiredInteger(data, name) {
  const value = Number.parseInt(data.get(name), 10);
  if (!Number.isFinite(value)) {
    throw new Error(`${name} must be a number.`);
  }
  return value;
}

function requiredNumber(data, name) {
  const value = Number.parseFloat(data.get(name));
  if (!Number.isFinite(value)) {
    throw new Error(`${name} must be a number.`);
  }
  return value;
}

async function captureActiveTab({ includeHtml } = { includeHtml: true }) {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab?.id) {
    throw new Error("No active tab.");
  }
  const [result] = await chrome.scripting.executeScript({
    target: { tabId: tab.id },
    args: [includeHtml],
    func: (shouldIncludeHtml) => ({
      sourceUrl: window.location.href,
      renderedHtml: shouldIncludeHtml ? document.documentElement.outerHTML : "",
    }),
  });
  if (!result?.result?.sourceUrl) {
    throw new Error("Could not read the active tab URL.");
  }
  if (includeHtml && !result.result.renderedHtml) {
    throw new Error("Could not capture rendered HTML.");
  }
  return { ...result.result, tabId: tab.id };
}

async function resetConfig() {
  await chrome.storage.local.clear();
  clearListingState();
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

async function setActionBadge(submitted, tabId) {
  if (!chrome.action?.setBadgeText) {
    return;
  }
  const details = { tabId, text: submitted ? "OK" : "" };
  await chrome.action.setBadgeText(details).catch(() => {});
  await chrome.action
    .setBadgeBackgroundColor({ tabId, color: submitted ? "#20824d" : "#4f5b66" })
    .catch(() => {});
}

function setBusy(button, busy) {
  if (button) {
    button.disabled = busy;
  }
}

function setStatus(message) {
  statusOutput.textContent = message;
}

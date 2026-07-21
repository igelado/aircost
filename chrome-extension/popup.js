const SIGNATURE_PREFIX = "aircost-plugin-v1";
const START_UPLOAD_MESSAGE = "aircost:start-upload";
const UPLOAD_PROGRESS_MESSAGE = "aircost:upload-progress";
const BACKGROUND_UPLOAD_STATE_KEY = "aircostBackgroundUpload";
const BACKGROUND_UPLOAD_MAX_AGE_MS = 30 * 60 * 1000;

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

const PIPELINE_STAGES = [
  { id: "capture", title: "Capturing this page" },
  { id: "verify", title: "Verifying the upload" },
  { id: "analyze", title: "Analyzing the listing" },
  { id: "normalize", title: "Normalizing aircraft data" },
  { id: "save", title: "Saving to AirCost" },
];

const STAGE_TO_PIPELINE = {
  capturing_page: "capture",
  signing_upload: "verify",
  sending_upload: "verify",
  received_upload: "verify",
  verifying_upload: "verify",
  extracting_listing: "analyze",
  verifying_listing: "analyze",
  normalizing_aircraft: "normalize",
  normalizing_avionics: "normalize",
  saving_listing: "save",
  refreshing_estimates: "save",
  recording_submission: "save",
  complete: "save",
};

const setupView = document.querySelector("#setup-view");
const captureView = document.querySelector("#capture-view");
const registerButton = document.querySelector("#register-button");
const submitButton = document.querySelector("#submit-button");
const refreshPageButton = document.querySelector("#refresh-page-button");
const refreshStatusButton = document.querySelector("#refresh-status-button");
const settingsButton = document.querySelector("#settings-button");
const settingsPanel = document.querySelector("#settings-panel");
const resetButton = document.querySelector("#reset-button");
const setupNotice = document.querySelector("#setup-notice");
const notice = document.querySelector("#notice");
const submissionBadge = document.querySelector("#submission-badge");
const pageHostname = document.querySelector("#page-hostname");
const currentUrlOutput = document.querySelector("#current-url");
const newPageActions = document.querySelector("#new-page-actions");
const existingEntry = document.querySelector("#existing-entry");
const existingEntryTitle = document.querySelector("#existing-entry-title");
const existingEntryMeta = document.querySelector("#existing-entry-meta");
const editListingButton = document.querySelector("#edit-listing-button");
const workflowDetails = document.querySelector("#workflow-details");
const workflowSummary = document.querySelector("#workflow-summary");
const pipeline = document.querySelector("#pipeline");
const activeStageEyebrow = document.querySelector("#active-stage-eyebrow");
const activeStageTitle = document.querySelector("#active-stage-title");
const activeStageMessage = document.querySelector("#active-stage-message");
const technicalProgress = document.querySelector("#technical-progress");
const recoveryActions = document.querySelector("#recovery-actions");
const retryUploadButton = document.querySelector("#retry-upload-button");
const retryStatusButton = document.querySelector("#retry-status-button");
const reprocessButton = document.querySelector("#reprocess-button");
const listingEditor = document.querySelector("#listing-editor");
const listingEditorForm = document.querySelector("#listing-editor-form");
const listingEditorHeading = document.querySelector("#listing-editor-heading");
const listingIdLabel = document.querySelector("#listing-id-label");
const closeEditorButton = document.querySelector("#close-editor-button");
const cancelListingButton = document.querySelector("#cancel-listing-button");
const saveListingButton = document.querySelector("#save-listing-button");
const addAvionicsButton = document.querySelector("#add-avionics-button");
const avionicsList = document.querySelector("#avionics-list");

let activeListing = null;
let activeSubmission = null;
let currentPipelineStage = null;
let backgroundStatusTimer = null;

document.addEventListener("DOMContentLoaded", refreshView);
registerButton.addEventListener("click", registerPlugin);
submitButton.addEventListener("click", submitCurrentPage);
refreshPageButton.addEventListener("click", submitCurrentPage);
refreshStatusButton.addEventListener("click", refreshListingStatus);
retryStatusButton.addEventListener("click", refreshListingStatus);
retryUploadButton.addEventListener("click", submitCurrentPage);
reprocessButton.addEventListener("click", reprocessExtraction);
settingsButton.addEventListener("click", toggleSettings);
resetButton.addEventListener("click", resetConfig);
editListingButton.addEventListener("click", openListingEditor);
closeEditorButton.addEventListener("click", closeListingEditor);
cancelListingButton.addEventListener("click", closeListingEditor);
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
    closeSettings();
    await refreshListingStatus();
  } else {
    setupView.classList.remove("hidden");
    captureView.classList.add("hidden");
    clearListingState();
    setSetupNotice("Connect this browser to begin.", "info");
  }
}

async function registerPlugin() {
  try {
    setBusy(registerButton, true);
    setSetupNotice("Creating a secure browser identity…", "info");
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
    await refreshView();
  } catch (error) {
    setSetupNotice(error.message, "error");
  } finally {
    setBusy(registerButton, false);
  }
}

async function refreshListingStatus() {
  try {
    clearTimeout(backgroundStatusTimer);
    backgroundStatusTimer = null;
    setBusy(refreshStatusButton, true);
    hideRecoveryActions();
    closeListingEditor();
    setSubmissionBadge("unknown", "Checking");
    setNotice("Checking this page in AirCost…", "info");
    const config = await loadConfig();
    if (!config?.serverUrl || !config?.username) {
      return;
    }
    const capture = await captureActiveTab({ includeHtml: false });
    setPageIdentity(capture.sourceUrl);
    const url = new URL(`${config.serverUrl}/api/plugin/submissions/status`);
    url.searchParams.set("source_url", capture.sourceUrl);
    const response = await fetch(url.toString(), {
      headers: { "X-User-Email": config.username },
    });
    const payload = await parseJsonResponse(response);
    const backgroundUpload = await loadBackgroundUpload(capture.sourceUrl);
    if (!payload.submitted && backgroundUpload) {
      showBackgroundUpload(backgroundUpload);
      await setActionBadge(false, capture.tabId);
      return;
    }
    setSubmissionState(payload);
    await setActionBadge(Boolean(payload.submitted), capture.tabId);
    if (payload.submission?.extraction_error) {
      setNotice(`AirCost captured this page, but extraction needs attention: ${payload.submission.extraction_error}`, "error");
      showRecoveryActions(["reprocess", "upload"]);
    } else {
      hideNotice();
    }
  } catch (error) {
    clearListingState({ preservePage: true });
    setSubmissionBadge("error", "Status error");
    setNotice(`Could not check this page: ${error.message}`, "error");
    showRecoveryActions(["status"]);
  } finally {
    setBusy(refreshStatusButton, false);
  }
}

async function submitCurrentPage() {
  let uploadObserver = null;
  try {
    setSubmissionBusy(true);
    hideRecoveryActions();
    setNotice("AirCost is processing this page…", "info");
    resetPipeline();
    const config = await loadConfig();
    if (!config?.pluginInstallId || !config?.privateKeyJwk) {
      throw new Error("Plugin is not registered.");
    }

    recordProgress("capturing_page", "running", "Reading the active tab.");
    const capture = await captureActiveTab({ includeHtml: true });
    setPageIdentity(capture.sourceUrl);
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
    recordProgress("signing_upload", "complete", "Secure signature ready.");

    recordProgress("sending_upload", "running", "Uploading rendered page content.");
    const jobId = crypto.randomUUID();
    uploadObserver = observeBackgroundUpload(jobId);
    const accepted = await chrome.runtime.sendMessage({
      type: START_UPLOAD_MESSAGE,
      jobId,
      tabId: capture.tabId,
      serverUrl: config.serverUrl,
      username: config.username,
      payload: {
        plugin_install_id: config.pluginInstallId,
        source_url: capture.sourceUrl,
        rendered_html: capture.renderedHtml,
        signature: arrayBufferToBase64(signature),
      },
    });
    if (!accepted?.ok) {
      throw new Error(accepted?.error || "Background upload did not start.");
    }
    recordProgress("sending_upload", "complete", "Upload accepted; AirCost now owns processing.");
    const result = await uploadObserver.promise;
    if (result.kind === "detached") {
      setSubmissionBadge("unknown", "Processing");
      setNotice("AirCost is continuing on the server. Reopen or refresh to check the result.", "info");
      showRecoveryActions(["status"]);
      scheduleBackgroundStatusRefresh();
      return;
    }
    if (result.kind === "error") {
      throw new Error(result.event.message || "Upload processing failed.");
    }
    const payload = result.event;
    setSubmissionState(payload);
    await setActionBadge(true, capture.tabId);
    finishPipeline(payload.submission?.extraction_error);
    if (payload.submission?.extraction_error) {
      setNotice(`Page captured, but extraction needs attention: ${payload.submission.extraction_error}`, "error");
      showRecoveryActions(["reprocess", "upload"]);
    } else {
      setNotice("Saved to AirCost.", "success");
    }
  } catch (error) {
    uploadObserver?.cancel();
    recordProgress("error", "error", error.message);
    setSubmissionBadge("error", "Upload error");
    setNotice(`Could not add this page: ${error.message}`, "error");
    showRecoveryActions(["upload", "status"]);
  } finally {
    setSubmissionBusy(false);
  }
}

async function reprocessExtraction() {
  if (!activeSubmission?.id) {
    setNotice("No saved submission is available to reprocess.", "error");
    return;
  }
  try {
    setBusy(reprocessButton, true);
    hideRecoveryActions();
    setNotice("Reprocessing the saved page…", "info");
    resetPipeline("analyze");
    const config = await loadConfig();
    const response = await fetch(
      `${config.serverUrl}/api/plugin/submissions/${activeSubmission.id}/reprocess`,
      {
        method: "POST",
        headers: { "X-User-Email": config.username },
      },
    );
    const payload = await parseJsonResponse(response);
    setSubmissionState(payload);
    if (payload.submission?.extraction_error) {
      setPipelineError("analyze", payload.submission.extraction_error);
      setNotice(`Extraction still needs attention: ${payload.submission.extraction_error}`, "error");
      showRecoveryActions(["reprocess", "upload"]);
    } else {
      finishPipeline();
      setNotice("Listing extracted and saved to AirCost.", "success");
    }
  } catch (error) {
    setPipelineError("analyze", error.message);
    setNotice(`Could not reprocess this submission: ${error.message}`, "error");
    showRecoveryActions(["reprocess", "upload"]);
  } finally {
    setBusy(reprocessButton, false);
  }
}

async function saveListingEdits(event) {
  event.preventDefault();
  if (!activeListing?.id) {
    setNotice("No uploaded listing is available to edit.", "error");
    return;
  }
  try {
    const config = await loadConfig();
    setBusy(saveListingButton, true);
    setNotice("Saving listing changes…", "info");
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
    setNotice(`Listing #${payload.listing.id} updated.`, "success");
  } catch (error) {
    setNotice(`Could not save listing changes: ${error.message}`, "error");
  } finally {
    setBusy(saveListingButton, false);
  }
}

function handleProgressEvent(event) {
  const stage = event.stage || "upload";
  const status = event.status || "running";
  recordProgress(stage, status, event.message || STAGE_LABELS[stage] || stage);
}

function observeBackgroundUpload(jobId) {
  let listener;
  const promise = new Promise((resolve) => {
    listener = (message) => {
      if (message?.type !== UPLOAD_PROGRESS_MESSAGE || message.jobId !== jobId) {
        return;
      }
      handleProgressEvent(message.event || {});
      if (message.lifecycle === "complete") {
        chrome.runtime.onMessage.removeListener(listener);
        resolve({ kind: "complete", event: message.event });
      } else if (message.lifecycle === "error") {
        chrome.runtime.onMessage.removeListener(listener);
        resolve({ kind: "error", event: message.event || {} });
      } else if (message.lifecycle === "detached") {
        chrome.runtime.onMessage.removeListener(listener);
        resolve({ kind: "detached", event: message.event || {} });
      }
    };
    chrome.runtime.onMessage.addListener(listener);
  });
  return {
    promise,
    cancel() {
      if (listener) {
        chrome.runtime.onMessage.removeListener(listener);
      }
    },
  };
}

async function loadBackgroundUpload(sourceUrl) {
  const stored = await chrome.storage.local.get(BACKGROUND_UPLOAD_STATE_KEY);
  const upload = stored[BACKGROUND_UPLOAD_STATE_KEY];
  if (!upload || upload.sourceUrl !== sourceUrl) {
    return null;
  }
  if (Date.now() - Number(upload.updatedAt || upload.startedAt || 0) > BACKGROUND_UPLOAD_MAX_AGE_MS) {
    return null;
  }
  return upload;
}

function showBackgroundUpload(upload) {
  activeSubmission = null;
  activeListing = null;
  closeListingEditor();
  newPageActions.classList.add("hidden");
  existingEntry.classList.add("hidden");
  const pipelineStage = STAGE_TO_PIPELINE[upload.stage] || currentPipelineStage || "verify";
  resetPipeline(pipelineStage);

  if (upload.lifecycle === "error") {
    setSubmissionBadge("error", "Upload error");
    setPipelineError(pipelineStage, upload.message);
    setNotice(`AirCost could not finish this upload: ${upload.message}`, "error");
    showRecoveryActions(["upload", "status"]);
    return;
  }

  recordProgress(upload.stage || "received_upload", "running", upload.message);
  setSubmissionBadge("unknown", "Processing");
  setNotice(
    upload.lifecycle === "detached"
      ? "Live updates ended, but AirCost is continuing on the server."
      : "AirCost is processing this page in the background.",
    "info",
  );
  showRecoveryActions(["status"]);
  scheduleBackgroundStatusRefresh();
}

function scheduleBackgroundStatusRefresh() {
  clearTimeout(backgroundStatusTimer);
  backgroundStatusTimer = setTimeout(refreshListingStatus, 2500);
}

function recordProgress(stage, status, message) {
  appendTechnicalProgress(stage, status, message);
  if (status === "error" || stage === "error") {
    setPipelineError(currentPipelineStage || "capture", message);
    return;
  }

  const pipelineStage = STAGE_TO_PIPELINE[stage] || currentPipelineStage || "capture";
  currentPipelineStage = pipelineStage;
  const currentIndex = PIPELINE_STAGES.findIndex((item) => item.id === pipelineStage);
  for (const [index, item] of PIPELINE_STAGES.entries()) {
    const element = pipeline.querySelector(`[data-pipeline-stage="${item.id}"]`);
    element.className = index < currentIndex || (stage === "complete" && status === "complete")
      ? "is-complete"
      : index === currentIndex
        ? status === "complete" ? "is-complete" : "is-active"
        : "";
    element.removeAttribute("aria-current");
  }
  const activeElement = pipeline.querySelector(`[data-pipeline-stage="${pipelineStage}"]`);
  if (status !== "complete" && stage !== "complete") {
    activeElement.setAttribute("aria-current", "step");
  }
  const stageDefinition = PIPELINE_STAGES[currentIndex];
  activeStageEyebrow.textContent = `${currentIndex + 1} of ${PIPELINE_STAGES.length}`;
  activeStageTitle.textContent = stageDefinition.title;
  activeStageMessage.textContent = message || STAGE_LABELS[stage] || "Processing…";
}

function appendTechnicalProgress(stage, status, message) {
  const item = document.createElement("li");
  const label = STAGE_LABELS[stage] || stage;
  item.textContent = `${status === "complete" ? "✓" : status === "error" ? "!" : "›"} ${label}: ${message || ""}`;
  technicalProgress.append(item);
  technicalProgress.scrollTop = technicalProgress.scrollHeight;
}

function resetPipeline(startAt = "capture") {
  workflowDetails.classList.remove("hidden", "is-success", "is-error");
  workflowDetails.open = true;
  workflowSummary.textContent = "Processing listing";
  technicalProgress.replaceChildren();
  currentPipelineStage = startAt;
  const startIndex = PIPELINE_STAGES.findIndex((item) => item.id === startAt);
  for (const [index, item] of PIPELINE_STAGES.entries()) {
    const element = pipeline.querySelector(`[data-pipeline-stage="${item.id}"]`);
    element.className = index < startIndex ? "is-complete" : index === startIndex ? "is-active" : "";
    element.toggleAttribute("aria-current", index === startIndex);
    if (index === startIndex) {
      element.setAttribute("aria-current", "step");
    }
  }
  activeStageEyebrow.textContent = `${startIndex + 1} of ${PIPELINE_STAGES.length}`;
  activeStageTitle.textContent = PIPELINE_STAGES[startIndex].title;
  activeStageMessage.textContent = startAt === "analyze"
    ? "Running extraction again from the saved page."
    : "Reading the current page from your active tab.";
}

function finishPipeline(extractionError = null) {
  for (const item of PIPELINE_STAGES) {
    const element = pipeline.querySelector(`[data-pipeline-stage="${item.id}"]`);
    element.className = "is-complete";
    element.removeAttribute("aria-current");
  }
  activeStageEyebrow.textContent = "Complete";
  activeStageTitle.textContent = extractionError ? "Page captured" : "Saved to AirCost";
  activeStageMessage.textContent = extractionError
    ? "The upload is stored, but listing extraction needs attention."
    : "The listing is ready in AirCost.";
  workflowDetails.classList.add("is-success");
  workflowSummary.textContent = extractionError ? "Captured by AirCost" : "Saved to AirCost";
  workflowDetails.open = false;
}

function setPipelineError(stage, message) {
  const pipelineStage = STAGE_TO_PIPELINE[stage] || stage || currentPipelineStage || "capture";
  currentPipelineStage = pipelineStage;
  const currentIndex = PIPELINE_STAGES.findIndex((item) => item.id === pipelineStage);
  for (const [index, item] of PIPELINE_STAGES.entries()) {
    const element = pipeline.querySelector(`[data-pipeline-stage="${item.id}"]`);
    element.className = index < currentIndex ? "is-complete" : index === currentIndex ? "is-error" : "";
    element.toggleAttribute("aria-current", index === currentIndex);
    if (index === currentIndex) {
      element.setAttribute("aria-current", "step");
    }
  }
  activeStageEyebrow.textContent = "Needs attention";
  activeStageTitle.textContent = `${PIPELINE_STAGES[currentIndex]?.title || "Processing"} failed`;
  activeStageMessage.textContent = message || "AirCost could not finish this step.";
  workflowDetails.classList.remove("hidden", "is-success");
  workflowDetails.classList.add("is-error");
  workflowDetails.open = true;
  workflowSummary.textContent = "Processing needs attention";
}

function setSubmissionState(payload) {
  activeSubmission = payload.submission || null;
  activeListing = payload.listing || null;
  closeListingEditor();
  if (payload.submitted || activeSubmission || activeListing) {
    setSubmissionBadge("submitted", "In AirCost");
    newPageActions.classList.add("hidden");
    existingEntry.classList.remove("hidden");
    if (activeListing) {
      const aircraft = activeListing.aircraft;
      const aircraftName = [activeListing.model_year, aircraft?.manufacturer, aircraft?.model, aircraft?.variant]
        .filter(Boolean)
        .join(" ");
      existingEntryTitle.textContent = "Already in AirCost";
      existingEntryMeta.textContent = `${aircraftName || "Saved listing"} · Listing #${activeListing.id}`;
      editListingButton.textContent = activeListing.is_verified ? "View details" : "Edit details";
      editListingButton.classList.remove("hidden");
      populateListingForm(activeListing);
    } else {
      existingEntryTitle.textContent = "Captured by AirCost";
      existingEntryMeta.textContent = activeSubmission?.id
        ? `Submission #${activeSubmission.id}`
        : "Awaiting listing details";
      editListingButton.classList.add("hidden");
    }
  } else {
    activeSubmission = null;
    activeListing = null;
    setSubmissionBadge("new", "Not added");
    existingEntry.classList.add("hidden");
    newPageActions.classList.remove("hidden");
  }
}

function clearListingState({ preservePage = false } = {}) {
  activeListing = null;
  activeSubmission = null;
  closeListingEditor();
  existingEntry.classList.add("hidden");
  newPageActions.classList.add("hidden");
  workflowDetails.classList.add("hidden");
  technicalProgress.replaceChildren();
  hideRecoveryActions();
  setSubmissionBadge("unknown", "Checking");
  if (!preservePage) {
    pageHostname.textContent = "Current page";
    currentUrlOutput.textContent = "Checking active tab…";
    currentUrlOutput.removeAttribute("title");
    currentUrlOutput.removeAttribute("aria-label");
  }
}

function setSubmissionBadge(kind, text) {
  submissionBadge.className = `status-pill is-${kind}`;
  submissionBadge.textContent = text;
}

function setPageIdentity(sourceUrl) {
  currentUrlOutput.textContent = sourceUrl;
  currentUrlOutput.title = sourceUrl;
  currentUrlOutput.setAttribute("aria-label", `Current page URL: ${sourceUrl}`);
  try {
    pageHostname.textContent = new URL(sourceUrl).hostname || "Current page";
  } catch {
    pageHostname.textContent = "Current page";
  }
}

function openListingEditor() {
  if (!activeListing) {
    return;
  }
  populateListingForm(activeListing);
  listingEditor.classList.remove("hidden");
  editListingButton.setAttribute("aria-expanded", "true");
  listingEditorHeading.textContent = activeListing.is_verified ? "Listing details" : "Edit listing details";
  const firstField = listingEditorForm.querySelector("input:not(:disabled), select:not(:disabled)");
  firstField?.focus();
}

function closeListingEditor() {
  listingEditor.classList.add("hidden");
  editListingButton.setAttribute("aria-expanded", "false");
  if (activeListing) {
    populateListingForm(activeListing);
  }
}

function populateListingForm(listing) {
  listingIdLabel.textContent = `Listing #${listing.id}${listing.is_verified ? " · Verified" : ""}`;
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
  for (const field of listingEditorForm.querySelectorAll("[name]")) {
    field.disabled = disabled;
  }
  addAvionicsButton.disabled = disabled;
  saveListingButton.classList.toggle("hidden", disabled);
  cancelListingButton.textContent = disabled ? "Close" : "Cancel";
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
  remove.setAttribute("aria-label", "Remove avionics");
  remove.title = "Remove avionics";
  remove.append(createSvgIcon(["M6 6l12 12", "M18 6 6 18"]));
  remove.addEventListener("click", () => {
    row.remove();
    if (!avionicsList.children.length) {
      addAvionicsRow();
    }
  });
  row.append(remove);
  avionicsList.append(row);
}

function createSvgIcon(paths) {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  for (const pathData of paths) {
    const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
    path.setAttribute("d", pathData);
    svg.append(path);
  }
  return svg;
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
    avionics.push({ manufacturer, model, type, quantity: Math.max(quantity, 1) });
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

function toggleSettings() {
  const shouldOpen = settingsPanel.classList.contains("hidden");
  settingsPanel.classList.toggle("hidden", !shouldOpen);
  settingsButton.setAttribute("aria-expanded", String(shouldOpen));
  settingsButton.setAttribute("aria-label", shouldOpen ? "Close connection settings" : "Open connection settings");
}

function closeSettings() {
  settingsPanel.classList.add("hidden");
  settingsButton.setAttribute("aria-expanded", "false");
  settingsButton.setAttribute("aria-label", "Open connection settings");
}

async function resetConfig() {
  await chrome.storage.local.clear();
  clearListingState();
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
    .setBadgeBackgroundColor({ tabId, color: submitted ? "#16754a" : "#566273" })
    .catch(() => {});
}

function setSubmissionBusy(busy) {
  setBusy(submitButton, busy);
  setBusy(refreshPageButton, busy);
  setBusy(retryUploadButton, busy);
}

function setBusy(button, busy) {
  if (button) {
    button.disabled = busy;
  }
}

function setNotice(message, kind = "info") {
  notice.textContent = message;
  notice.className = `notice is-${kind}`;
}

function hideNotice() {
  notice.textContent = "";
  notice.className = "notice hidden";
}

function setSetupNotice(message, kind = "info") {
  setupNotice.textContent = message;
  setupNotice.className = `notice is-${kind}`;
}

function showRecoveryActions(actions) {
  const visibleActions = new Set(actions);
  retryUploadButton.classList.toggle("hidden", !visibleActions.has("upload"));
  retryStatusButton.classList.toggle("hidden", !visibleActions.has("status"));
  reprocessButton.classList.toggle("hidden", !visibleActions.has("reprocess") || !activeSubmission?.id);
  recoveryActions.classList.toggle("hidden", !Array.from(recoveryActions.children).some((button) => !button.classList.contains("hidden")));
}

function hideRecoveryActions() {
  recoveryActions.classList.add("hidden");
  for (const button of recoveryActions.children) {
    button.classList.add("hidden");
  }
}

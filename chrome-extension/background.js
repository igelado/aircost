const START_UPLOAD_MESSAGE = "aircost:start-upload";
const UPLOAD_PROGRESS_MESSAGE = "aircost:upload-progress";
const BACKGROUND_UPLOADS_STATE_KEY = "aircostBackgroundUploads";
const MAX_STORED_UPLOADS = 25;
const MAX_UPLOAD_AGE_MS = 24 * 60 * 60 * 1000;

let stateWriteQueue = Promise.resolve();

chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
  if (message?.type !== START_UPLOAD_MESSAGE) {
    return false;
  }

  void runUpload(message, sendResponse);
  return true;
});

async function runUpload(message, sendResponse) {
  const job = {
    jobId: message.jobId || crypto.randomUUID(),
    sourceUrl: message.payload?.source_url || "",
    tabId: message.tabId,
    startedAt: Date.now(),
  };
  let accepted = false;

  try {
    validateUploadMessage(message);
    await publishProgress(job, {
      stage: "sending_upload",
      status: "running",
      message: "Background upload started.",
    }, "running");
    const response = await fetch(`${message.serverUrl}/api/plugin/submissions/stream`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-User-Email": message.username,
      },
      body: JSON.stringify(message.payload),
    });
    if (!response.ok) {
      throw new Error(await responseError(response));
    }

    // Fetch resolves after the server has received the complete request and
    // returned response headers. At that point the server owns processing.
    accepted = true;
    job.acceptedAt = Date.now();
    safeSendResponse(sendResponse, { ok: true, jobId: job.jobId });
    await publishProgress(job, {
      stage: "sending_upload",
      status: "complete",
      message: "Upload accepted by AirCost.",
    }, "running");
    await consumeProgressResponse(response, job);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (!accepted) {
      safeSendResponse(sendResponse, { ok: false, error: message });
      await publishProgress(job, {
        stage: "error",
        status: "error",
        message,
      }, "error");
      return;
    }

    // Losing the progress body after the server accepts the request does not
    // mean processing failed. Persist a detached state so a reopened popup can
    // query the authoritative server status.
    await publishProgress(job, {
      stage: "detached",
      status: "detached",
      message: "Live progress ended; AirCost is continuing on the server.",
    }, "detached");
  }
}

function safeSendResponse(sendResponse, payload) {
  try {
    sendResponse(payload);
  } catch {
    // The popup may have closed while the service worker was uploading. Its
    // response channel is optional; background and server processing are not.
  }
}

function validateUploadMessage(message) {
  if (!message.serverUrl || !message.username) {
    throw new Error("AirCost connection settings are missing.");
  }
  if (!message.payload?.plugin_install_id || !message.payload?.source_url
      || !message.payload?.rendered_html || !message.payload?.signature) {
    throw new Error("The signed listing upload is incomplete.");
  }
}

async function consumeProgressResponse(response, job) {
  const contentType = response.headers.get("content-type") || "";
  if (!response.body || !contentType.includes("application/x-ndjson")) {
    const payload = await response.json();
    await publishProgress(job, payload, payload?.status === "error" ? "error" : "complete");
    return;
  }

  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let finished = false;

  while (true) {
    const { value, done } = await reader.read();
    if (done) {
      break;
    }
    buffer += decoder.decode(value, { stream: true });
    const lines = buffer.split("\n");
    buffer = lines.pop() || "";
    for (const line of lines) {
      finished = await publishProgressLine(job, line) || finished;
    }
  }

  if (buffer.trim()) {
    finished = await publishProgressLine(job, buffer) || finished;
  }
  if (!finished) {
    throw new Error("Server progress ended before a completion event.");
  }
}

async function publishProgressLine(job, line) {
  const event = parseProgressLine(line);
  if (!event) {
    return false;
  }
  const lifecycle = event.status === "error"
    ? "error"
    : event.stage === "complete"
      ? "complete"
      : "running";
  await publishProgress(job, event, lifecycle);
  return lifecycle === "complete" || lifecycle === "error";
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

async function publishProgress(job, event, lifecycle) {
  const eventStage = event.stage || job.lastStage || "sending_upload";
  if (eventStage !== "detached" && eventStage !== "error") {
    job.lastStage = eventStage;
  }
  const state = {
    ...job,
    lifecycle,
    stage: eventStage === "detached" || eventStage === "error"
      ? job.lastStage || "sending_upload"
      : eventStage,
    status: event.status || "running",
    message: event.message || "AirCost is processing this listing.",
    updatedAt: Date.now(),
  };
  await saveUploadState(state);
  await chrome.runtime.sendMessage({
    type: UPLOAD_PROGRESS_MESSAGE,
    jobId: job.jobId,
    lifecycle,
    event,
  }).catch(() => {});

  if (lifecycle === "complete" && Number.isInteger(job.tabId)) {
    await chrome.action.setBadgeText({ tabId: job.tabId, text: "OK" }).catch(() => {});
    await chrome.action
      .setBadgeBackgroundColor({ tabId: job.tabId, color: "#16754a" })
      .catch(() => {});
  }
}

function saveUploadState(state) {
  stateWriteQueue = stateWriteQueue
    .catch(() => {})
    .then(async () => {
      const stored = await chrome.storage.local.get(BACKGROUND_UPLOADS_STATE_KEY);
      const cutoff = Date.now() - MAX_UPLOAD_AGE_MS;
      const uploads = Object.values(stored[BACKGROUND_UPLOADS_STATE_KEY] || {})
        .filter((upload) => upload.jobId !== state.jobId && upload.updatedAt >= cutoff);
      uploads.push(state);
      uploads.sort((left, right) => right.updatedAt - left.updatedAt);
      const limited = uploads.slice(0, MAX_STORED_UPLOADS);
      await chrome.storage.local.set({
        [BACKGROUND_UPLOADS_STATE_KEY]: Object.fromEntries(
          limited.map((upload) => [upload.jobId, upload]),
        ),
      });
    });
  return stateWriteQueue;
}

async function responseError(response) {
  const payload = await response.json().catch(() => null);
  return payload?.error?.message || `HTTP ${response.status}`;
}

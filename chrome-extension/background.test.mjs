import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const backgroundSource = readFileSync(new URL("./background.js", import.meta.url), "utf8");

function loadBackground(fetchImpl) {
  let listener;
  const storage = {};
  const sentMessages = [];
  const chrome = {
    runtime: {
      onMessage: {
        addListener(nextListener) {
          listener = nextListener;
        },
      },
      async sendMessage(message) {
        sentMessages.push(message);
        throw new Error("Receiving end does not exist.");
      },
    },
    storage: {
      local: {
        async set(values) {
          Object.assign(storage, values);
        },
      },
    },
    action: {
      async setBadgeText() {},
      async setBadgeBackgroundColor() {},
    },
  };
  vm.runInNewContext(backgroundSource, {
    chrome,
    crypto,
    fetch: fetchImpl,
    TextDecoder,
    Error,
    console,
  });
  return { listener, storage, sentMessages };
}

function uploadMessage() {
  return {
    type: "aircost:start-upload",
    jobId: "job-123",
    tabId: 4,
    serverUrl: "http://127.0.0.1:8001",
    username: "developer",
    payload: {
      plugin_install_id: 1,
      source_url: "https://example.com/aircraft/123",
      rendered_html: "<html>listing</html>",
      signature: "signed",
    },
  };
}

function invoke(listener, message) {
  let keepChannelOpen;
  const response = new Promise((resolve) => {
    keepChannelOpen = listener(message, {}, resolve);
  });
  assert.equal(keepChannelOpen, true);
  return response;
}

function invokeAfterPopupClosed(listener, message) {
  const keepChannelOpen = listener(message, {}, () => {
    throw new Error("message port closed");
  });
  assert.equal(keepChannelOpen, true);
}

async function waitFor(check) {
  for (let attempt = 0; attempt < 50; attempt += 1) {
    if (check()) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
  assert.fail("Timed out waiting for background upload state");
}

test("finishes an upload when the popup closes during transport", async () => {
  let request;
  const progress = [
    { stage: "received_upload", status: "complete", message: "Received." },
    { stage: "normalizing_aircraft", status: "running", message: "Normalizing." },
    { stage: "complete", status: "complete", submission: { id: 42 } },
  ].map((event) => JSON.stringify(event)).join("\n") + "\n";
  const harness = loadBackground(async (url, options) => {
    request = { url, options };
    return new Response(progress, {
      status: 200,
      headers: { "content-type": "application/x-ndjson" },
    });
  });

  invokeAfterPopupClosed(harness.listener, uploadMessage());
  await waitFor(() => harness.storage.aircostBackgroundUpload?.lifecycle === "complete");

  assert.equal(request.url, "http://127.0.0.1:8001/api/plugin/submissions/stream");
  assert.equal(JSON.parse(request.options.body).rendered_html, "<html>listing</html>");
  assert.equal(harness.storage.aircostBackgroundUpload.stage, "complete");
  assert.equal(harness.storage.aircostBackgroundUpload.acceptedAt > 0, true);
  assert.equal(harness.sentMessages.some((message) => message.lifecycle === "complete"), true);
});

test("records detached progress without reporting server normalization failure", async () => {
  let reads = 0;
  const body = new ReadableStream({
    pull(controller) {
      if (reads === 0) {
        reads += 1;
        controller.enqueue(new TextEncoder().encode(
          '{"stage":"normalizing_aircraft","status":"running","message":"Normalizing."}\n',
        ));
      } else {
        controller.error(new Error("progress connection closed"));
      }
    },
  });
  const harness = loadBackground(async () => new Response(body, {
    status: 200,
    headers: { "content-type": "application/x-ndjson" },
  }));

  const accepted = await invoke(harness.listener, uploadMessage());
  assert.equal(accepted.ok, true);
  await waitFor(() => harness.storage.aircostBackgroundUpload?.lifecycle === "detached");

  assert.equal(harness.storage.aircostBackgroundUpload.stage, "normalizing_aircraft");
  assert.match(harness.storage.aircostBackgroundUpload.message, /continuing on the server/);
});

test("reports a transport error when the server never accepts the upload", async () => {
  const harness = loadBackground(async () => {
    throw new Error("server unavailable");
  });

  const accepted = await invoke(harness.listener, uploadMessage());
  assert.equal(accepted.ok, false);
  assert.equal(accepted.error, "server unavailable");
  await waitFor(() => harness.storage.aircostBackgroundUpload?.lifecycle === "error");
  assert.equal(harness.storage.aircostBackgroundUpload.acceptedAt, undefined);
});

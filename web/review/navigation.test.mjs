import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";

import {
  createTaskNavigation,
  isPlainPrimaryClick,
  MOBILE_NAVIGATION_QUERY,
} from "../navigation.mjs";

const indexHtml = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const appCss = readFileSync(new URL("../app.css", import.meta.url), "utf8");
const appJs = readFileSync(new URL("../app.js", import.meta.url), "utf8");
const chromiumPath = process.env.AIRCOST_TEST_CHROMIUM_PATH;
const webBinaryPath = process.env.AIRCOST_TEST_WEB_BINARY;

class FakeTarget {
  constructor(name, ownerDocument = null) {
    this.name = name;
    this.ownerDocument = ownerDocument;
    this.listeners = new Map();
    this.attributes = new Map();
    const classes = new Set();
    this.classList = {
      contains: (value) => classes.has(value),
      toggle: (value, force) => {
        const add = force === undefined ? !classes.has(value) : force;
        if (add) {
          classes.add(value);
        } else {
          classes.delete(value);
        }
        return add;
      },
    };
  }

  addEventListener(type, listener) {
    const listeners = this.listeners.get(type) || [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }

  dispatch(type, properties = {}) {
    const event = {
      altKey: false,
      button: 0,
      ctrlKey: false,
      currentTarget: this,
      defaultPrevented: false,
      key: "",
      metaKey: false,
      shiftKey: false,
      target: this,
      preventDefault() {
        this.defaultPrevented = true;
      },
      ...properties,
    };
    for (const listener of this.listeners.get(type) || []) {
      listener(event);
    }
    return event;
  }

  focus() {
    if (this.ownerDocument) {
      this.ownerDocument.activeElement = this;
    }
  }

  getAttribute(name) {
    return this.attributes.get(name) ?? null;
  }

  setAttribute(name, value) {
    this.attributes.set(name, String(value));
  }
}

function fakeMedia(width) {
  const listeners = [];
  return {
    matches: width <= 760,
    addEventListener(type, listener) {
      if (type === "change") {
        listeners.push(listener);
      }
    },
    setWidth(nextWidth) {
      this.matches = nextWidth <= 760;
      for (const listener of listeners) {
        listener({ matches: this.matches });
      }
    },
  };
}

function navigationHarness({ width = 760, navigate = () => true } = {}) {
  const document = new FakeTarget("document");
  document.activeElement = null;
  const root = new FakeTarget("root", document);
  const toggle = new FakeTarget("toggle", document);
  const menu = new FakeTarget("menu", document);
  const links = ["listings", "values", "review", "catalog"].map((name) => {
    const link = new FakeTarget(name, document);
    link.href = `https://aircost.test/#/${name}`;
    return link;
  });
  const descendants = new Set([root, toggle, menu, ...links]);
  root.contains = (target) => descendants.has(target);
  const media = fakeMedia(width);
  let requestedQuery = null;
  const controller = createTaskNavigation({
    root,
    toggle,
    menu,
    links,
    document,
    matchMedia(query) {
      requestedQuery = query;
      return media;
    },
    navigate,
  });
  return {
    controller,
    document,
    links,
    media,
    menu,
    requestedQuery: () => requestedQuery,
    root,
    toggle,
  };
}

async function availablePort() {
  const server = createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const { port } = server.address();
  await new Promise((resolve, reject) => server.close((error) => {
    if (error) {
      reject(error);
    } else {
      resolve();
    }
  }));
  return port;
}

async function waitForHttp(url, child, stderr) {
  for (let attempt = 0; attempt < 600; attempt += 1) {
    if (child.exitCode !== null) {
      throw new Error(`Web server exited before startup (${child.exitCode}): ${stderr()}`);
    }
    try {
      const response = await fetch(url);
      if (response.ok) {
        return;
      }
    } catch {
      // The disposable database performs its first canonical initialization here.
    }
    await delay(100);
  }
  throw new Error(`Timed out waiting for ${url}: ${stderr()}`);
}

async function waitForDevtools(profileDirectory, child, stderr) {
  const portFile = join(profileDirectory, "DevToolsActivePort");
  for (let attempt = 0; attempt < 300; attempt += 1) {
    if (child.exitCode !== null) {
      throw new Error(`Chromium exited before DevTools startup (${child.exitCode}): ${stderr()}`);
    }
    try {
      const [port] = readFileSync(portFile, "utf8").trim().split("\n");
      if (port) {
        return Number(port);
      }
    } catch {
      // Chromium writes the port only after its isolated profile is ready.
    }
    const announcedPort = stderr().match(
      /DevTools listening on ws:\/\/127\.0\.0\.1:(\d+)\//,
    )?.[1];
    if (announcedPort) {
      return Number(announcedPort);
    }
    await delay(100);
  }
  throw new Error(`Timed out waiting for Chromium DevTools: ${stderr()}`);
}

async function connectDevtools(url) {
  const socket = new WebSocket(url);
  await new Promise((resolve, reject) => {
    socket.addEventListener("open", resolve, { once: true });
    socket.addEventListener("error", reject, { once: true });
  });
  let nextId = 0;
  const pending = new Map();
  socket.addEventListener("message", (event) => {
    const message = JSON.parse(event.data);
    if (!message.id || !pending.has(message.id)) {
      return;
    }
    const { resolve, reject } = pending.get(message.id);
    pending.delete(message.id);
    if (message.error) {
      reject(new Error(`${message.error.message}: ${JSON.stringify(message.error.data)}`));
    } else {
      resolve(message.result);
    }
  });
  return {
    close: () => socket.close(),
    send(method, params = {}) {
      nextId += 1;
      const id = nextId;
      const response = new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
      socket.send(JSON.stringify({ id, method, params }));
      return response;
    },
  };
}

async function stopChild(child) {
  if (!child || child.exitCode !== null) {
    return;
  }
  child.kill("SIGTERM");
  await Promise.race([
    new Promise((resolve) => child.once("exit", resolve)),
    delay(2_000),
  ]);
  if (child.exitCode === null) {
    child.kill("SIGKILL");
  }
}

test("keeps WEB-002 task routes and IDs behind one labelled mobile menu", () => {
  assert.match(indexHtml, /<aside class="sidebar" data-task-navigation>/);
  assert.match(
    indexHtml,
    /id="mobile-nav-toggle"[\s\S]*?aria-expanded="false"[\s\S]*?aria-controls="task-navigation"/,
  );
  assert.match(indexHtml, /class="nav-groups" id="task-navigation"/);
  for (const [route, destination, label] of [
    ["listings", "listings", "Listings"],
    ["values", "values", "Aircraft values"],
    ["review", "review", "Review queue"],
    ["catalog", "catalog", "Avionics catalog"],
  ]) {
    assert.match(
      indexHtml,
      new RegExp(`href="/#/${route}"[^>]*data-destination="${destination}"[^>]*>${label}<\\/a>`),
    );
  }
  assert.match(appJs, /createTaskNavigation\(\{[\s\S]*?navigate: \(link\) => navigateRoute\(parseRoute\(link\.href\)\),/);
  assert.match(appJs, /function applyAppRoute\(route, context = \{\}\) \{\s*const destination = destinationForRoute\(route\);\s*taskNavigation\.close\(\);/);
});

test("switches only at 760px and contains the menu at 200 percent zoom", () => {
  const mobileStart = appCss.indexOf("@media (max-width: 760px)");
  assert.ok(mobileStart >= 0);
  const mobileCss = appCss.slice(mobileStart);
  assert.match(appCss, /\.mobile-nav-toggle \{[\s\S]*?display: none;/);
  assert.match(mobileCss, /\.mobile-nav-toggle \{[\s\S]*?display: inline-flex;/);
  assert.match(mobileCss, /\.nav-groups \{[\s\S]*?display: none;[\s\S]*?max-width: 100%;/);
  assert.match(mobileCss, /\.sidebar\.is-menu-open \.nav-groups \{\s*display: grid;/);
  assert.match(mobileCss, /\.nav-tab \{[\s\S]*?min-height: 44px;[\s\S]*?overflow-wrap: anywhere;/);
  assert.match(appCss, /\.mobile-nav-toggle \{[\s\S]*?min-height: 44px;/);
  assert.match(appCss, /\.mobile-nav-toggle:focus-visible,[\s\S]*?outline: 3px solid var\(--accent\);/);

  const at760 = navigationHarness({ width: 760 });
  assert.equal(at760.requestedQuery(), MOBILE_NAVIGATION_QUERY);
  at760.toggle.dispatch("click");
  assert.equal(at760.controller.isOpen(), true);

  const at761 = navigationHarness({ width: 761 });
  at761.toggle.dispatch("click");
  assert.equal(at761.controller.isOpen(), false, "desktop navigation stays persistently visible");

  const atTwoHundredPercent = navigationHarness({ width: 1280 / 2 });
  atTwoHundredPercent.toggle.dispatch("click");
  assert.equal(atTwoHundredPercent.controller.isOpen(), true);
  assert.equal(atTwoHundredPercent.links.length, 4, "every destination remains reachable");
  assert.match(mobileCss, /\.sidebar \{[\s\S]*?width: 100%;[\s\S]*?min-width: 0;[\s\S]*?max-width: 100%;/);
  assert.match(appCss, /\.table-shell \{[\s\S]*?overflow-x: auto;/);
});

test("closes with Escape or outside click and returns focus only when appropriate", () => {
  const escape = navigationHarness();
  escape.toggle.dispatch("click");
  escape.document.activeElement = escape.links[0];
  const keydown = escape.document.dispatch("keydown", { key: "Escape" });
  assert.equal(keydown.defaultPrevented, true);
  assert.equal(escape.controller.isOpen(), false);
  assert.equal(escape.toggle.getAttribute("aria-expanded"), "false");
  assert.equal(escape.document.activeElement, escape.toggle);

  const outside = navigationHarness();
  const outsideControl = new FakeTarget("outside", outside.document);
  outside.toggle.dispatch("click");
  outsideControl.focus();
  outside.document.dispatch("click", { target: outsideControl });
  assert.equal(outside.controller.isOpen(), false);
  assert.equal(outside.document.activeElement, outsideControl, "outside interaction keeps its focus");

  const resized = navigationHarness({ width: 760 });
  resized.toggle.dispatch("click");
  resized.media.setWidth(761);
  assert.equal(resized.controller.isOpen(), false);
  assert.equal(resized.toggle.getAttribute("aria-expanded"), "false");
});

test("closes only after the central router accepts navigation", () => {
  const acceptedLinks = [];
  const accepted = navigationHarness({
    navigate(link) {
      acceptedLinks.push(link.name);
      return undefined;
    },
  });
  accepted.toggle.dispatch("click");
  accepted.links[1].focus();
  const acceptedClick = accepted.links[1].dispatch("click");
  assert.equal(acceptedClick.defaultPrevented, true);
  assert.deepEqual(acceptedLinks, ["values"]);
  assert.equal(accepted.controller.isOpen(), false);
  assert.equal(accepted.document.activeElement, accepted.toggle);

  const rejected = navigationHarness({ navigate: () => false });
  rejected.toggle.dispatch("click");
  rejected.links[2].focus();
  const rejectedClick = rejected.links[2].dispatch("click");
  assert.equal(rejectedClick.defaultPrevented, true);
  assert.equal(rejected.controller.isOpen(), true);
  assert.equal(rejected.toggle.getAttribute("aria-expanded"), "true");
  assert.equal(rejected.document.activeElement, rejected.links[2]);
});

test("leaves modified and non-primary task links to the browser", () => {
  assert.equal(isPlainPrimaryClick({ button: 0 }), true);
  const modifiedCases = [
    { button: 1 },
    { button: 0, metaKey: true },
    { button: 0, ctrlKey: true },
    { button: 0, shiftKey: true },
    { button: 0, altKey: true },
    { button: 0, defaultPrevented: true },
  ];
  for (const properties of modifiedCases) {
    let navigations = 0;
    const harness = navigationHarness({ navigate: () => { navigations += 1; } });
    harness.toggle.dispatch("click");
    const event = harness.links[0].dispatch("click", properties);
    assert.equal(event.defaultPrevented, Boolean(properties.defaultPrevented));
    assert.equal(navigations, 0);
    assert.equal(harness.controller.isOpen(), true);
  }
});

test("native Chromium keeps every task reachable without viewport overflow", {
  skip: chromiumPath === undefined || webBinaryPath === undefined,
  timeout: 120_000,
}, async () => {
  const temporaryRoot = mkdtempSync(join(tmpdir(), "aircost-navigation-smoke-"));
  const profileDirectory = join(temporaryRoot, "chromium-profile");
  const databasePath = join(temporaryRoot, "aircost.sqlite3");
  const port = await availablePort();
  const baseUrl = `http://127.0.0.1:${port}`;
  let server;
  let browser;
  let devtools;
  let serverError = "";
  let browserError = "";
  try {
    server = spawn(webBinaryPath, [
      "--host", "127.0.0.1",
      "--port", String(port),
      "--database", databasePath,
    ], { stdio: ["ignore", "ignore", "pipe"] });
    server.stderr.setEncoding("utf8");
    server.stderr.on("data", (chunk) => { serverError += chunk; });
    await waitForHttp(baseUrl, server, () => serverError);

    browser = spawn(chromiumPath, [
      "--headless=new",
      `--user-data-dir=${profileDirectory}`,
      "--disable-background-networking",
      "--disable-breakpad",
      "--disable-component-update",
      "--disable-crashpad",
      "--disable-crash-reporter",
      "--disable-default-apps",
      "--disable-extensions",
      "--disable-gpu",
      "--disable-sync",
      "--no-default-browser-check",
      "--no-first-run",
      "--remote-debugging-port=0",
      "--remote-allow-origins=*",
      baseUrl,
    ], { stdio: ["ignore", "ignore", "pipe"] });
    browser.stderr.setEncoding("utf8");
    browser.stderr.on("data", (chunk) => { browserError += chunk; });
    const debuggingPort = await waitForDevtools(
      profileDirectory,
      browser,
      () => browserError,
    );
    const targets = await fetch(`http://127.0.0.1:${debuggingPort}/json/list`).then(
      (response) => response.json(),
    );
    const page = targets.find((target) => target.type === "page");
    assert.ok(page?.webSocketDebuggerUrl, browserError);
    devtools = await connectDevtools(page.webSocketDebuggerUrl);
    await devtools.send("Runtime.enable");
    await devtools.send("Page.enable");

    const evaluate = async (expression) => {
      const result = await devtools.send("Runtime.evaluate", {
        expression,
        awaitPromise: true,
        returnByValue: true,
      });
      assert.equal(result.exceptionDetails, undefined, JSON.stringify(result.exceptionDetails));
      return result.result.value;
    };
    const setViewport = async ({ cssWidth, deviceScaleFactor = 1, screenWidth = cssWidth }) => {
      await devtools.send("Emulation.setDeviceMetricsOverride", {
        width: cssWidth,
        height: 900,
        screenWidth,
        screenHeight: 900 * deviceScaleFactor,
        deviceScaleFactor,
        mobile: false,
      });
      await devtools.send("Page.navigate", { url: `${baseUrl}/#/listings` });
      for (let attempt = 0; attempt < 200; attempt += 1) {
        const ready = await evaluate(
          "document.readyState === 'complete' && Boolean(document.querySelector('#mobile-nav-toggle'))",
        );
        if (ready) {
          return;
        }
        await delay(50);
      }
      throw new Error(`Timed out loading navigation at ${cssWidth}px`);
    };
    const layout = () => evaluate(`(() => {
      const toggle = document.querySelector("#mobile-nav-toggle");
      const menu = document.querySelector("#task-navigation");
      const links = [...menu.querySelectorAll(".nav-tab")];
      return {
        devicePixelRatio,
        innerWidth,
        menuDisplay: getComputedStyle(menu).display,
        overflow: document.documentElement.scrollWidth > document.documentElement.clientWidth,
        toggleDisplay: getComputedStyle(toggle).display,
        toggleHeight: toggle.getBoundingClientRect().height,
        links: links.map((link) => ({
          height: link.getBoundingClientRect().height,
          href: link.hash,
          visible: link.getClientRects().length > 0,
        })),
      };
    })()`);

    await setViewport({ cssWidth: 760 });
    let result = await layout();
    assert.equal(result.innerWidth, 760);
    assert.equal(result.toggleDisplay, "flex");
    assert.equal(result.menuDisplay, "none");
    assert.equal(result.overflow, false);
    assert.ok(result.toggleHeight >= 44);

    const interactions = await evaluate(`(async () => {
      const pause = () => new Promise((resolve) => setTimeout(resolve));
      const root = document.querySelector("[data-task-navigation]");
      const toggle = document.querySelector("#mobile-nav-toggle");
      const links = [...document.querySelectorAll("#task-navigation .nav-tab")];
      const states = {};
      toggle.click();
      states.destinations = links.map((link) => ({
        height: link.getBoundingClientRect().height,
        href: link.hash,
        visible: link.getClientRects().length > 0,
      }));
      states.openOverflow = document.documentElement.scrollWidth > document.documentElement.clientWidth;
      links[0].focus();
      const escape = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
      document.dispatchEvent(escape);
      states.escape = {
        closed: !root.classList.contains("is-menu-open"),
        focused: document.activeElement.id,
        prevented: escape.defaultPrevented,
      };
      toggle.click();
      const outside = document.querySelector("#view-title");
      outside.tabIndex = -1;
      outside.focus();
      outside.click();
      states.outside = {
        closed: !root.classList.contains("is-menu-open"),
        focused: document.activeElement.id,
      };
      states.routes = [];
      for (const link of links) {
        toggle.click();
        link.click();
        await pause();
        states.routes.push(location.hash);
      }

      const fixture = document.createElement("aside");
      fixture.className = "sidebar";
      fixture.innerHTML = \`
        <button class="mobile-nav-toggle" id="fixture-toggle" aria-expanded="false">Tasks</button>
        <div class="nav-groups">
          <a class="nav-tab" href="/#/values">Aircraft values</a>
        </div>\`;
      document.body.append(fixture);
      const fixtureToggle = fixture.querySelector("button");
      const fixtureMenu = fixture.querySelector(".nav-groups");
      const fixtureLink = fixture.querySelector("a");
      let allowFixtureNavigation = false;
      const { createTaskNavigation } = await import("/navigation.mjs");
      createTaskNavigation({
        root: fixture,
        toggle: fixtureToggle,
        menu: fixtureMenu,
        links: [fixtureLink],
        document,
        matchMedia: window.matchMedia.bind(window),
        navigate: () => allowFixtureNavigation,
      });
      fixtureToggle.click();
      fixtureLink.focus();
      fixtureLink.click();
      states.rejected = {
        open: fixture.classList.contains("is-menu-open"),
        focused: document.activeElement.textContent.trim(),
      };
      allowFixtureNavigation = true;
      fixtureLink.click();
      states.accepted = {
        closed: !fixture.classList.contains("is-menu-open"),
        focused: document.activeElement.id,
      };
      fixture.remove();
      return states;
    })()`);
    assert.deepEqual(interactions.destinations.map(({ href }) => href), [
      "#/listings", "#/values", "#/review", "#/catalog",
    ]);
    assert.equal(interactions.destinations.every(({ height, visible }) => visible && height >= 44), true);
    assert.equal(interactions.openOverflow, false);
    assert.deepEqual(interactions.routes, [
      "#/listings", "#/values", "#/review", "#/catalog",
    ]);
    assert.deepEqual(interactions.escape, {
      closed: true,
      focused: "mobile-nav-toggle",
      prevented: true,
    });
    assert.deepEqual(interactions.outside, { closed: true, focused: "view-title" });
    assert.deepEqual(interactions.rejected, {
      open: true,
      focused: "Aircraft values",
    });
    assert.deepEqual(interactions.accepted, {
      closed: true,
      focused: "fixture-toggle",
    });

    await setViewport({ cssWidth: 761 });
    result = await layout();
    assert.equal(result.innerWidth, 761);
    assert.equal(result.toggleDisplay, "none");
    assert.equal(result.menuDisplay, "grid");
    assert.equal(result.overflow, false);
    assert.equal(result.links.every(({ visible }) => visible), true);

    await setViewport({ cssWidth: 640, deviceScaleFactor: 2, screenWidth: 1280 });
    await evaluate("document.querySelector('#mobile-nav-toggle').click()");
    result = await layout();
    assert.equal(result.innerWidth, 640, "1280 physical pixels at 200% expose 640 CSS pixels");
    assert.equal(result.devicePixelRatio, 2);
    assert.equal(result.toggleDisplay, "flex");
    assert.equal(result.menuDisplay, "grid");
    assert.equal(result.overflow, false);
    assert.equal(result.links.every(({ height, visible }) => visible && height >= 44), true);
  } finally {
    devtools?.close();
    await stopChild(browser);
    await stopChild(server);
    rmSync(temporaryRoot, { recursive: true, force: true });
  }
});

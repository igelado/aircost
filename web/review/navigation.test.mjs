import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
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
import { createHistoryRouter, parseRoute } from "../routing.mjs";

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
      this.ownerDocument.dispatch("focusin", { target: this });
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
  menu.contains = (target) => target === menu || links.includes(target);
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

function fakeLocation(initialUrl = "/#/listings") {
  const location = { href: "", pathname: "/", search: "", hash: "" };
  setLocation(location, initialUrl);
  return location;
}

function setLocation(location, value) {
  const url = new URL(value, "http://localhost:8001");
  location.href = url.href;
  location.pathname = url.pathname;
  location.search = url.search;
  location.hash = url.hash;
}

function trackedHistory(location, listeners) {
  const entries = [{ state: null, url: `${location.pathname}${location.search}${location.hash}` }];
  let index = 0;
  return {
    pushState(state, _title, url) {
      entries.splice(index + 1, entries.length, { state, url });
      index += 1;
      setLocation(location, url);
    },
    replaceState(state, _title, url) {
      entries[index] = { state, url };
      setLocation(location, url);
    },
    go(delta) {
      index += delta;
      const entry = entries[index];
      setLocation(location, entry.url);
      for (const listener of listeners.get("popstate") || []) {
        listener({ state: entry.state });
      }
    },
    back() {
      this.go(-1);
    },
    forward() {
      this.go(1);
    },
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
    await Promise.race([
      new Promise((resolve) => child.once("exit", resolve)),
      delay(2_000),
    ]);
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
  assert.match(
    appJs,
    /appRouter = createHistoryRouter\([\s\S]*?taskNavigation = createTaskNavigation\([\s\S]*?appRouter\.start\(\);/,
  );
  assert.match(appJs, /function applyAppRoute\(route, context = \{\}\) \{\s*const destination = destinationForRoute\(route\);\s*taskNavigation\.closeForRouteActivation\(\);/);
});

test("switches only at 760px and contains the menu at 200 percent zoom", () => {
  const mobileStart = appCss.indexOf("@media (max-width: 760px)");
  assert.ok(mobileStart >= 0);
  const mobileCss = appCss.slice(mobileStart);
  assert.match(appCss, /\.mobile-nav-toggle \{[\s\S]*?display: none;/);
  assert.match(mobileCss, /\.sidebar\.is-menu-ready \.mobile-nav-toggle \{[\s\S]*?display: inline-flex;/);
  assert.match(mobileCss, /\.nav-groups \{[\s\S]*?max-width: 100%;/);
  assert.match(mobileCss, /\.sidebar\.is-menu-ready \.nav-groups \{\s*display: none;/);
  assert.match(mobileCss, /\.sidebar\.is-menu-open \.nav-groups \{\s*display: grid;/);
  assert.match(appCss, /\.view-panel,[\s\S]*?#review-product-results \{[\s\S]*?min-width: 0;[\s\S]*?max-width: 100%;/);
  assert.match(appCss, /\.table-shell \{[\s\S]*?width: 100%;[\s\S]*?overflow-x: auto;/);
  assert.match(appCss, /@media \(max-width: 1120px\) \{[\s\S]*?\.review-pipeline-overview \{\s*grid-template-columns: 1fr;/);
  assert.match(indexHtml, /class="table-shell review-table-shell review-pipeline-table-shell"[^>]*role="region"[^>]*tabindex="0"/);
  assert.match(mobileCss, /\.nav-tab \{[\s\S]*?min-height: 44px;[\s\S]*?overflow-wrap: anywhere;/);
  assert.match(appCss, /\.mobile-nav-toggle \{[\s\S]*?min-height: 44px;/);
  assert.match(appCss, /\.mobile-nav-toggle:focus-visible,[\s\S]*?outline: 3px solid var\(--accent\);/);

  const at760 = navigationHarness({ width: 760 });
  assert.equal(at760.requestedQuery(), MOBILE_NAVIGATION_QUERY);
  assert.equal(at760.root.classList.contains("is-menu-ready"), true);
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

test("route activation closes and returns focus only after guarded history accepts", () => {
  const listeners = new Map();
  const location = fakeLocation();
  let allowNavigation = true;
  let router;
  const harness = navigationHarness({
    navigate(link) {
      return router.navigate(parseRoute(link.href));
    },
  });
  const history = trackedHistory(location, listeners);
  const listen = (name, listener) => {
    const registered = listeners.get(name) || [];
    registered.push(listener);
    listeners.set(name, registered);
  };
  router = createHistoryRouter({
    location,
    history,
    listen,
    mayNavigate: () => allowNavigation,
    apply: () => harness.controller.closeForRouteActivation(),
  });
  router.start();

  harness.toggle.dispatch("click");
  harness.links[1].focus();
  harness.links[1].dispatch("click");
  assert.equal(location.hash, "#/values");
  assert.equal(harness.controller.isOpen(), false);
  assert.equal(harness.document.activeElement, harness.toggle);

  harness.toggle.dispatch("click");
  harness.links[0].focus();
  history.back();
  assert.equal(location.hash, "#/listings");
  assert.equal(harness.controller.isOpen(), false, "accepted Back activates the route");
  assert.equal(harness.document.activeElement, harness.toggle);

  history.forward();
  assert.equal(location.hash, "#/values");
  harness.toggle.dispatch("click");
  harness.links[0].focus();
  allowNavigation = false;
  history.back();
  assert.equal(location.hash, "#/values", "the router repairs the rejected Back traversal");
  assert.equal(harness.controller.isOpen(), true);
  assert.equal(harness.document.activeElement, harness.links[0]);
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

  const collapsed = navigationHarness({ width: 761 });
  collapsed.links[2].focus();
  collapsed.media.setWidth(760);
  assert.equal(collapsed.controller.isOpen(), false);
  assert.equal(
    collapsed.document.activeElement,
    collapsed.toggle,
    "collapsing desktop navigation returns focus from its newly hidden link",
  );

  const expanded = navigationHarness({ width: 760 });
  expanded.links[2].setAttribute("aria-current", "page");
  expanded.toggle.focus();
  expanded.media.setWidth(761);
  assert.equal(
    expanded.document.activeElement,
    expanded.links[2],
    "expanding navigation moves focus from the hidden toggle to the current task",
  );

  const expandedFallback = navigationHarness({ width: 760 });
  expandedFallback.toggle.focus();
  expandedFallback.media.setWidth(761);
  assert.equal(expandedFallback.document.activeElement, expandedFallback.links[0]);

  const visibleLink = navigationHarness({ width: 760 });
  visibleLink.toggle.dispatch("click");
  visibleLink.links[1].focus();
  visibleLink.media.setWidth(761);
  assert.equal(
    visibleLink.document.activeElement,
    visibleLink.links[1],
    "expanding navigation preserves focus on a link that remains visible",
  );

  const expandedOutside = navigationHarness({ width: 760 });
  const expandedOutsideControl = new FakeTarget("workspace", expandedOutside.document);
  expandedOutsideControl.focus();
  expandedOutside.media.setWidth(761);
  assert.equal(
    expandedOutside.document.activeElement,
    expandedOutsideControl,
    "expanding navigation does not steal focus from the workspace",
  );

  const programmatic = navigationHarness();
  const workspaceControl = new FakeTarget("workspace", programmatic.document);
  programmatic.toggle.dispatch("click");
  workspaceControl.focus();
  programmatic.controller.closeForRouteActivation();
  assert.equal(programmatic.controller.isOpen(), false);
  assert.equal(
    programmatic.document.activeElement,
    workspaceControl,
    "route activation does not steal focus from outside the menu",
  );
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
  const crashDirectory = join(temporaryRoot, "crash-dumps");
  const chromiumTemporaryDirectory = join(temporaryRoot, "tmp");
  const databasePath = join(temporaryRoot, "aircost.sqlite3");
  mkdirSync(crashDirectory);
  mkdirSync(chromiumTemporaryDirectory);
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
      `--crash-dumps-dir=${crashDirectory}`,
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
    ], {
      env: {
        ...process.env,
        TMPDIR: chromiumTemporaryDirectory,
        XDG_CACHE_HOME: join(temporaryRoot, "xdg-cache"),
        XDG_CONFIG_HOME: join(temporaryRoot, "xdg-config"),
        XDG_DATA_HOME: join(temporaryRoot, "xdg-data"),
        XDG_STATE_HOME: join(temporaryRoot, "xdg-state"),
      },
      stdio: ["ignore", "ignore", "pipe"],
    });
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
    const waitForPage = async (cssWidth, route = "listings") => {
      const expectedHash = JSON.stringify(`#/${route}`);
      for (let attempt = 0; attempt < 200; attempt += 1) {
        const ready = await evaluate(
          `document.readyState === 'complete'
            && location.hash === ${expectedHash}
            && Boolean(document.querySelector('#mobile-nav-toggle'))`,
        );
        if (ready) {
          return;
        }
        await delay(50);
      }
      throw new Error(`Timed out loading navigation at ${cssWidth}px`);
    };
    let pageNavigationSequence = 0;
    const navigatePage = async (route, cssWidth) => {
      pageNavigationSequence += 1;
      await devtools.send("Page.navigate", {
        url: `${baseUrl}/?smoke=${pageNavigationSequence}#/${route}`,
      });
      await waitForPage(cssWidth, route);
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
      await navigatePage("listings", cssWidth);
    };
    const layout = () => evaluate(`(() => {
      const toggle = document.querySelector("#mobile-nav-toggle");
      const menu = document.querySelector("#task-navigation");
      const links = [...menu.querySelectorAll(".nav-tab")];
      return {
        devicePixelRatio,
        innerWidth,
        clientWidth: document.documentElement.clientWidth,
        scrollWidth: document.documentElement.scrollWidth,
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

    await setViewport({ cssWidth: 760 });
    const historyInteractions = await evaluate(`(async () => {
      const pause = (milliseconds = 0) => new Promise((resolve) => setTimeout(resolve, milliseconds));
      const root = document.querySelector("[data-task-navigation]");
      const toggle = document.querySelector("#mobile-nav-toggle");
      const menu = document.querySelector("#task-navigation");
      const link = (destination) => menu.querySelector(
        '[data-destination="' + destination + '"]',
      );
      const traverse = (method) => new Promise((resolve) => {
        addEventListener("popstate", () => setTimeout(resolve), { once: true });
        history[method]();
      });

      toggle.click();
      link("values").click();
      await pause();
      toggle.click();
      link("catalog").focus();
      await traverse("back");
      const acceptedBack = {
        closed: !root.classList.contains("is-menu-open"),
        focused: document.activeElement.id,
        hash: location.hash,
      };
      toggle.click();
      link("catalog").focus();
      await traverse("forward");
      const acceptedForward = {
        closed: !root.classList.contains("is-menu-open"),
        focused: document.activeElement.id,
        hash: location.hash,
      };

      toggle.click();
      link("listings").click();
      await pause();
      document.querySelector("#new-listing").click();
      await pause();
      const listingInput = document.querySelector("#listing-form input");
      listingInput.value = "unsaved";
      listingInput.dispatchEvent(new Event("input", { bubbles: true }));
      document.querySelector("#listing-dialog").close();
      let confirmations = 0;
      window.confirm = () => {
        confirmations += 1;
        return false;
      };
      toggle.click();
      link("values").focus();
      history.back();
      await pause(250);
      const rejectedBack = {
        confirmations,
        focused: document.activeElement.textContent.trim(),
        hash: location.hash,
        open: root.classList.contains("is-menu-open"),
      };

      window.confirm = () => true;
      link("values").click();
      await pause();
      await traverse("back");
      const forwardListingInput = document.querySelector("#listing-form input");
      forwardListingInput.value = "another unsaved change";
      forwardListingInput.dispatchEvent(new Event("input", { bubbles: true }));
      document.querySelector("#listing-dialog").close();
      confirmations = 0;
      window.confirm = () => {
        confirmations += 1;
        return false;
      };
      toggle.click();
      link("catalog").focus();
      history.forward();
      await pause(250);
      const rejectedForward = {
        confirmations,
        focused: document.activeElement.textContent.trim(),
        hash: location.hash,
        open: root.classList.contains("is-menu-open"),
      };
      return { acceptedBack, acceptedForward, rejectedBack, rejectedForward };
    })()`);
    assert.deepEqual(historyInteractions.acceptedBack, {
      closed: true,
      focused: "mobile-nav-toggle",
      hash: "#/listings",
    });
    assert.deepEqual(historyInteractions.acceptedForward, {
      closed: true,
      focused: "mobile-nav-toggle",
      hash: "#/values",
    });
    assert.deepEqual(historyInteractions.rejectedBack, {
      confirmations: 1,
      focused: "Aircraft values",
      hash: "#/listings/new",
      open: true,
    });
    assert.deepEqual(historyInteractions.rejectedForward, {
      confirmations: 1,
      focused: "Avionics catalog",
      hash: "#/listings/new",
      open: true,
    });

    await setViewport({ cssWidth: 761 });
    result = await layout();
    assert.equal(result.innerWidth, 761);
    assert.equal(result.toggleDisplay, "none");
    assert.equal(result.menuDisplay, "grid");
    assert.equal(result.overflow, false);
    assert.equal(result.links.every(({ visible }) => visible), true);

    await evaluate("document.querySelector('#task-navigation .nav-tab').focus()");
    await devtools.send("Emulation.setDeviceMetricsOverride", {
      width: 760,
      height: 900,
      screenWidth: 760,
      screenHeight: 900,
      deviceScaleFactor: 1,
      mobile: false,
    });
    await delay(50);
    assert.equal(
      await evaluate("document.activeElement.id"),
      "mobile-nav-toggle",
      "crossing into the mobile breakpoint returns focus from the collapsed menu",
    );
    await devtools.send("Emulation.setDeviceMetricsOverride", {
      width: 761,
      height: 900,
      screenWidth: 761,
      screenHeight: 900,
      deviceScaleFactor: 1,
      mobile: false,
    });
    await delay(50);
    assert.equal(
      await evaluate("document.activeElement.dataset.destination"),
      "listings",
      "crossing out of the mobile breakpoint focuses the current task link",
    );

    await setViewport({ cssWidth: 640, deviceScaleFactor: 2, screenWidth: 1280 });
    await evaluate("document.querySelector('#mobile-nav-toggle').click()");
    result = await layout();
    assert.equal(result.innerWidth, 640, "1280 physical pixels at 200% expose 640 CSS pixels");
    assert.equal(result.devicePixelRatio, 2);
    assert.equal(result.toggleDisplay, "flex");
    assert.equal(result.menuDisplay, "grid");
    assert.equal(result.overflow, false);
    assert.equal(result.links.every(({ height, visible }) => visible && height >= 44), true);

    const routes = [
      "listings",
      "values",
      "review",
      "review/manual",
      "review/products",
      "catalog",
    ];
    for (const cssWidth of [320, 390, 760, 1024, 1120, 1121, 1440]) {
      await setViewport({ cssWidth });
      for (const route of routes) {
        await navigatePage(route, cssWidth);
        if (cssWidth <= 760) {
          await evaluate("document.querySelector('#mobile-nav-toggle').click()");
        }
        const routeLayout = await layout();
        assert.ok(
          routeLayout.scrollWidth <= routeLayout.clientWidth,
          `${route} at ${cssWidth}px overflowed ${routeLayout.scrollWidth}/${routeLayout.clientWidth}`,
        );
        assert.equal(
          routeLayout.links.every(({ height, visible }) => (
            visible && (cssWidth > 760 || height >= 44)
          )),
          true,
          `${route} at ${cssWidth}px keeps every task reachable`,
        );
      }
    }

    await devtools.send("Emulation.setScriptExecutionDisabled", { value: true });
    await devtools.send("Emulation.setDeviceMetricsOverride", {
      width: 760,
      height: 900,
      screenWidth: 760,
      screenHeight: 900,
      deviceScaleFactor: 1,
      mobile: false,
    });
    await devtools.send("Page.navigate", {
      url: `${baseUrl}/?javascript=disabled#/listings`,
    });
    await waitForPage(760);
    result = await layout();
    assert.equal(result.toggleDisplay, "none", "the no-JS menu does not expose an inert toggle");
    assert.equal(result.menuDisplay, "grid", "the no-JS menu keeps all task links visible");
    assert.equal(result.links.every(({ height, visible }) => visible && height >= 44), true);
    assert.equal(result.overflow, false);
    await devtools.send("Emulation.setScriptExecutionDisabled", { value: false });
  } finally {
    devtools?.close();
    await stopChild(browser);
    await stopChild(server);
    rmSync(temporaryRoot, {
      recursive: true,
      force: true,
      maxRetries: 10,
      retryDelay: 100,
    });
  }
});

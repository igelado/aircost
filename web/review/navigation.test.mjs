import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  createTaskNavigation,
  isPlainPrimaryClick,
  MOBILE_NAVIGATION_QUERY,
} from "../navigation.mjs";

const indexHtml = readFileSync(new URL("../index.html", import.meta.url), "utf8");

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
    requestedQuery: () => requestedQuery,
    root,
    toggle,
  };
}

test("keeps the WEB-002 task routes and IDs in one labelled menu", () => {
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
});

test("switches at 760px and initializes progressive enhancement", () => {
  const mobile = navigationHarness({ width: 760 });
  assert.equal(mobile.requestedQuery(), MOBILE_NAVIGATION_QUERY);
  assert.equal(mobile.root.classList.contains("is-menu-ready"), true);
  assert.equal(mobile.toggle.getAttribute("aria-expanded"), "false");
  mobile.toggle.dispatch("click");
  assert.equal(mobile.controller.isOpen(), true);

  const desktop = navigationHarness({ width: 761 });
  desktop.toggle.dispatch("click");
  assert.equal(desktop.controller.isOpen(), false, "desktop navigation remains persistent");

  const twoHundredPercent = navigationHarness({ width: 1280 / 2 });
  twoHundredPercent.toggle.dispatch("click");
  assert.equal(twoHundredPercent.controller.isOpen(), true);
  assert.equal(twoHundredPercent.links.length, 4);
});

test("closes with Escape and outside click without stealing outside focus", () => {
  const escape = navigationHarness();
  escape.toggle.dispatch("click");
  escape.links[0].focus();
  const keydown = escape.document.dispatch("keydown", { key: "Escape" });
  assert.equal(keydown.defaultPrevented, true);
  assert.equal(escape.controller.isOpen(), false);
  assert.equal(escape.toggle.getAttribute("aria-expanded"), "false");
  assert.equal(escape.document.activeElement, escape.toggle);

  const activated = navigationHarness();
  activated.toggle.dispatch("click");
  activated.links[1].focus();
  activated.controller.closeForRouteActivation();
  assert.equal(activated.controller.isOpen(), false);
  assert.equal(activated.document.activeElement, activated.toggle);

  const outside = navigationHarness();
  const outsideControl = new FakeTarget("outside", outside.document);
  outside.toggle.dispatch("click");
  outsideControl.focus();
  outside.document.dispatch("click", { target: outsideControl });
  assert.equal(outside.controller.isOpen(), false);
  assert.equal(outside.document.activeElement, outsideControl);

  outside.toggle.dispatch("click");
  outsideControl.focus();
  outside.controller.closeForRouteActivation();
  assert.equal(outside.controller.isOpen(), false);
  assert.equal(outside.document.activeElement, outsideControl);
});

test("keeps focus visible across both sides of the breakpoint", () => {
  const collapsing = navigationHarness({ width: 761 });
  collapsing.links[2].focus();
  collapsing.media.setWidth(760);
  assert.equal(collapsing.controller.isOpen(), false);
  assert.equal(collapsing.document.activeElement, collapsing.toggle);

  const expanding = navigationHarness({ width: 760 });
  expanding.links[2].setAttribute("aria-current", "page");
  expanding.toggle.focus();
  expanding.media.setWidth(761);
  assert.equal(expanding.document.activeElement, expanding.links[2]);

  const fallback = navigationHarness({ width: 760 });
  fallback.toggle.focus();
  fallback.media.setWidth(761);
  assert.equal(fallback.document.activeElement, fallback.links[0]);

  const visibleLink = navigationHarness({ width: 760 });
  visibleLink.toggle.dispatch("click");
  visibleLink.links[1].focus();
  visibleLink.media.setWidth(761);
  assert.equal(visibleLink.document.activeElement, visibleLink.links[1]);

  const outside = navigationHarness({ width: 760 });
  const workspaceControl = new FakeTarget("workspace", outside.document);
  workspaceControl.focus();
  outside.media.setWidth(761);
  assert.equal(outside.document.activeElement, workspaceControl);
});

test("closes only after navigation is accepted", () => {
  const acceptedDestinations = [];
  const accepted = navigationHarness({
    navigate(link) {
      acceptedDestinations.push(link.name);
      return true;
    },
  });
  accepted.toggle.dispatch("click");
  accepted.links[1].focus();
  const acceptedClick = accepted.links[1].dispatch("click");
  assert.equal(acceptedClick.defaultPrevented, true);
  assert.deepEqual(acceptedDestinations, ["values"]);
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
  for (const properties of [
    { button: 1 },
    { button: 0, metaKey: true },
    { button: 0, ctrlKey: true },
    { button: 0, shiftKey: true },
    { button: 0, altKey: true },
    { button: 0, defaultPrevented: true },
  ]) {
    let navigations = 0;
    const harness = navigationHarness({ navigate: () => { navigations += 1; } });
    harness.toggle.dispatch("click");
    const event = harness.links[0].dispatch("click", properties);
    assert.equal(event.defaultPrevented, Boolean(properties.defaultPrevented));
    assert.equal(navigations, 0);
    assert.equal(harness.controller.isOpen(), true);
  }
});

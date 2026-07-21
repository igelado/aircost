import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const popupHtml = readFileSync(new URL("./popup.html", import.meta.url), "utf8");
const popupSource = readFileSync(new URL("./popup.js", import.meta.url), "utf8");

function fakeElement() {
  const classes = new Set();
  const attributes = new Map();
  const listeners = new Map();
  return {
    classList: {
      add(...names) {
        names.forEach((name) => classes.add(name));
      },
      contains(name) {
        return classes.has(name);
      },
      remove(...names) {
        names.forEach((name) => classes.delete(name));
      },
      toggle(name, force) {
        const enabled = force === undefined ? !classes.has(name) : force;
        if (enabled) {
          classes.add(name);
        } else {
          classes.delete(name);
        }
        return enabled;
      },
    },
    addEventListener(type, listener) {
      listeners.set(type, listener);
    },
    setAttribute(name, value) {
      attributes.set(name, String(value));
    },
    getAttribute(name) {
      return attributes.get(name) ?? null;
    },
    click() {
      listeners.get("click")?.();
    },
  };
}

test("technical details button reveals and hides its panel", () => {
  assert.match(popupHtml, /id="technical-details-toggle"/);
  assert.match(popupHtml, /id="technical-details-panel" class="technical-details-panel hidden"/);

  const elements = new Map();
  const querySelector = (selector) => {
    if (!elements.has(selector)) {
      elements.set(selector, fakeElement());
    }
    return elements.get(selector);
  };
  const document = {
    addEventListener() {},
    querySelector,
  };
  const chrome = {
    runtime: {
      onMessage: {
        addListener() {},
      },
    },
  };
  const panel = querySelector("#technical-details-panel");
  panel.classList.add("hidden");

  vm.runInNewContext(popupSource, { chrome, document, console });
  const toggle = querySelector("#technical-details-toggle");

  toggle.click();
  assert.equal(panel.classList.contains("hidden"), false);
  assert.equal(toggle.getAttribute("aria-expanded"), "true");

  toggle.click();
  assert.equal(panel.classList.contains("hidden"), true);
  assert.equal(toggle.getAttribute("aria-expanded"), "false");
});

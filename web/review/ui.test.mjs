import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const indexHtml = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const reviewJs = readFileSync(new URL("../review.js", import.meta.url), "utf8");

test("provides an accessible render target for pipeline backlog categories", () => {
  assert.match(
    indexHtml,
    /id="review-pipeline-categories"[\s\S]*aria-label="Verification backlog categories"/,
  );
  assert.doesNotMatch(indexHtml, /review-pipeline-(?:aircraft|avionics|gemini)-count/);
});

test("separates catalog identity from reusable product source status", () => {
  assert.match(
    indexHtml,
    /<th>Product<\/th>\s*<th>Catalog identity<\/th>\s*<th>Reusable source<\/th>/,
  );
  assert.match(indexHtml, /Products needing source check/);
  assert.match(indexHtml, /Ready after source check/);
});

test("prepares preserved product references before reading the product queue", () => {
  const prepareCall = reviewJs.indexOf('api("/api/review/avionics/products/prepare"');
  const queueCall = reviewJs.indexOf("api(`/api/review/avionics/products?${params}`)");
  assert.ok(prepareCall >= 0);
  assert.ok(queueCall > prepareCall);
});

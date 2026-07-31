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

test("renders guarded avionics correction controls and uses the revision endpoint", () => {
  assert.match(reviewJs, /Correct extracted values/);
  assert.match(reviewJs, /Manufacturer/);
  assert.match(reviewJs, /Avionics types/);
  assert.match(reviewJs, /Installation action/);
  assert.match(reviewJs, /Save corrected values/);
  assert.match(
    reviewJs,
    /\/api\/review\/listings\/\$\{review\.listing_id\}\/avionics\/revise/,
  );
  assert.match(reviewJs, /avionicsObservationRevisionRequest/);
  assert.doesNotMatch(
    reviewJs,
    /api\(`\/api\/avionics\/\$\{[^}]+\}`[^]*method:\s*"(?:PATCH|PUT|DELETE)"/,
  );
});

test("keeps listing verification disabled while a correction is unsaved", () => {
  assert.match(
    reviewJs,
    /drafts\.some\(\(draft\) => draft\.correction\.dirty \|\| draft\.correction\.saving\)/,
  );
  assert.match(reviewJs, /review_payload_sha256: review\.review_payload_sha256/);
  assert.match(reviewJs, /catalog_revision_sha256: review\.catalog_revision_sha256/);
});

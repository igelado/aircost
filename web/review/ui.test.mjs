import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const indexHtml = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const appCss = readFileSync(new URL("../app.css", import.meta.url), "utf8");
const appJs = readFileSync(new URL("../app.js", import.meta.url), "utf8");
const reviewJs = readFileSync(new URL("../review.js", import.meta.url), "utf8");

test("uses a compact multi-capability dropdown in listing avionics rows", () => {
  assert.match(appJs, /function avionicsTypeDropdown\(values = \[\]\)/);
  assert.match(appJs, /querySelectorAll\('\[name="avionics_types"\]:checked'\)/);
  assert.match(appJs, /checkbox\.type = "checkbox"/);
  assert.doesNotMatch(appJs, /select\.multiple = true/);
  assert.match(appCss, /\.avionics-type-dropdown > summary/);
  assert.match(appCss, /\.avionics-type-menu/);
});

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

test("names the three workflows by acceptance and residual-review purpose", () => {
  assert.match(indexHtml, /id="review-mode-pipeline"[^>]*>Automatic acceptance<\/button>/);
  assert.match(indexHtml, /id="review-mode-product"[^>]*>Known avionics products<\/button>/);
  assert.match(indexHtml, /id="review-mode-listing"[^>]*>Manual review<\/button>/);
  assert.match(
    indexHtml,
    /Each card is one retained occurrence\.[\s\S]*repeated cards are never merged automatically\./,
  );
});

test("keeps aircraft and avionics review controls in their own panels", () => {
  const aircraftPanel = indexHtml.match(
    /id="review-aircraft-panel"[\s\S]*?<\/section>/,
  )?.[0] ?? "";
  const avionicsPanel = indexHtml.match(
    /id="review-avionics-panel"[\s\S]*?<\/section>/,
  )?.[0] ?? "";
  assert.match(aircraftPanel, /id="review-aircraft-summary"/);
  assert.doesNotMatch(aircraftPanel, /rebuild-avionics-review/);
  assert.match(avionicsPanel, /id="rebuild-avionics-review"/);
  assert.match(avionicsPanel, /id="review-avionics-aspects"/);
});

test("shows quantity and installation for known-product occurrences", () => {
  assert.match(
    indexHtml,
    /<th>Observed text<\/th>\s*<th>Quantity<\/th>\s*<th>Installation<\/th>/,
  );
  assert.match(reviewJs, /reviewQuantity\(association\.quantity\)/);
  assert.match(reviewJs, /displayLabel\(association\.configuration_action\)/);
});

test("blocks duplicate canonical selections before submitting manual review", () => {
  assert.match(reviewJs, /canonicalProductSelectionConflicts/);
  assert.match(reviewJs, /has-product-conflict/);
  assert.match(
    reviewJs,
    /Two retained occurrences select the same canonical avionics product/,
  );
  assert.doesNotMatch(reviewJs, /review\.aspects\.length === 0/);
});

test("saves a verified product decision on its own aspect card", () => {
  assert.match(reviewJs, /Save verified product for this entry/);
  assert.match(
    reviewJs,
    /\/api\/review\/listings\/\$\{review\.listing_id\}\/avionics\/use-existing/,
  );
  assert.match(reviewJs, /useExistingProductRequest/);
  assert.match(reviewJs, /const preservedDrafts = new Map\(state\.drafts\)/);
  assert.match(reviewJs, /Could not save this entry: \$\{draft\.decisionError\}/);
  assert.match(appCss, /\.review-aspect-save-controls/);
  assert.match(appCss, /\.review-aspect-save-result\.error/);
});

test("reports card-by-card completion only after final listing verification", () => {
  assert.match(reviewJs, /outcome\?\.listing_ready === true/);
  assert.match(reviewJs, /outcome\?\.listing_verified === true/);
  assert.match(reviewJs, /outcome\?\.finalization_error/);
  assert.match(reviewJs, /final avionics decision[^]*was saved, but the listing could not be verified/);
  assert.match(reviewJs, /state\.savingAspectKey !== null/);
});

test("routes aspect-scoped whole-review failures back to the affected card", () => {
  assert.match(reviewJs, /function showAspectResolutionError\(error\)/);
  assert.match(reviewJs, /draft\.decisionError = detail/);
  assert.match(reviewJs, /The exact error is shown on that card/);
  assert.match(reviewJs, /showAspectResolutionError\(error\)/);
});

test("prevents selecting an explicitly non-reusable approved catalog product", () => {
  assert.match(reviewJs, /value\.catalog\?\.reuse_eligible \?\? value\.reuse_eligible/);
  assert.match(reviewJs, /product\.reuseEligible === false/);
  assert.match(reviewJs, /Reusable source verification required/);
  assert.match(reviewJs, /Known avionics products before selection/);
  assert.match(appCss, /\.review-catalog-result\.not-reusable/);
});

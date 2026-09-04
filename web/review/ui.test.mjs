import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const indexHtml = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const appCss = readFileSync(new URL("../app.css", import.meta.url), "utf8");
const appJs = readFileSync(new URL("../app.js", import.meta.url), "utf8");
const avionicsJs = readFileSync(new URL("../avionics.js", import.meta.url), "utf8");
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

test("labels global OEM source work as automation maintenance", () => {
  assert.match(
    indexHtml,
    /<th>Product<\/th>\s*<th>Catalog identity<\/th>\s*<th>OEM automation source<\/th>/,
  );
  assert.match(indexHtml, /OEM source automation/);
  assert.match(indexHtml, /not a prerequisite for a reviewer to approve an individual listing association/);
  assert.match(indexHtml, /Verify OEM source for automation/);
  assert.doesNotMatch(reviewJs, /Review product/);
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
  assert.match(indexHtml, /id="review-mode-product"[^>]*>OEM source automation<\/button>/);
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

test("saves only eligible raw observation discards from their aspect card", () => {
  assert.match(reviewJs, /canSaveAvionicsDiscardIndividually/);
  assert.match(reviewJs, /Save discarded observation/);
  assert.match(
    reviewJs,
    /\/api\/review\/listings\/\$\{review\.listing_id\}\/avionics\/discard/,
  );
  assert.match(reviewJs, /discardAvionicsObservationRequest\(/);
  assert.match(reviewJs, /const preservedDrafts = new Map\(state\.drafts\)/);
  assert.match(reviewJs, /exact error is shown on its avionics card/);
  assert.match(reviewJs, /Its discard must be saved with the complete listing review/);
  const handler = reviewJs.match(
    /async function saveIndividualAspectDecision\(key\) \{[\s\S]*?\n\}\n\nasync function validateExistingAssociation/,
  )?.[0] ?? "";
  assert.notEqual(handler, "");
  assert.match(
    handler,
    /discardAvionicsObservationRequest\(\s*review\.review_payload_sha256,\s*draft\.aspect\.id,\s*draft\.discardReason/,
  );
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

test("allows accountable source-free human selection of every approved product", () => {
  assert.match(reviewJs, /Approved catalog product selected\. Saving this association records the accountable human verification\./);
  assert.match(reviewJs, /status: "approved"/);
  assert.match(reviewJs, /Save this human-verified match now/);
  assert.doesNotMatch(reviewJs, /product\.reuseEligible === false/);
  assert.doesNotMatch(appCss, /\.review-catalog-result\.not-reusable/);
});

test("creates and saves a source-free human-verified product from one card", () => {
  assert.match(
    reviewJs,
    /\/api\/review\/listings\/\$\{review\.listing_id\}\/avionics\/create/,
  );
  assert.match(reviewJs, /createHumanVerifiedProductRequest\(/);
  assert.match(reviewJs, /Create and use product for this entry/);
  assert.match(reviewJs, /Stable identifier kind \(optional\)/);
  assert.doesNotMatch(reviewJs, /draft\.create\.identitySourceUrl/);
  assert.doesNotMatch(reviewJs, /draft\.create\.identityEvidenceText/);
});

test("distinguishes integrated suites from units without double-valuing components", () => {
  assert.match(reviewJs, /\["integrated_suite", "Integrated suite"\]/);
  assert.match(reviewJs, /The suite is valued once/);
  assert.match(reviewJs, /Catalog component rows are descriptive and are not valued again/);
  assert.match(reviewJs, /Suites and individual units remain distinct catalog products/);
  assert.match(reviewJs, /An integrated suite cannot contain another suite/);
  assert.match(appCss, /\.review-suite-component/);
});

test("edits an existing approved product structure behind an optimistic reviewer boundary", () => {
  assert.match(indexHtml, /id="review-product-structure-editor"/);
  assert.match(indexHtml, /This is a source-free human catalog decision/);
  assert.match(indexHtml, /separate from both OEM automation and approval of any listing association/);
  assert.match(reviewJs, /function renderExistingProductStructureEditor\(\)/);
  assert.match(reviewJs, /function searchExistingProductStructureComponents/);
  assert.match(
    reviewJs,
    /\/api\/review\/avionics\/products\/\$\{selected\.id\}\/structure/,
  );
  assert.match(reviewJs, /catalog_revision_sha256: selected\.catalogRevision/);
  assert.match(reviewJs, /valuation_scope: draft\.valuationScope/);
  assert.match(reviewJs, /suite_components: draft\.valuationScope === "integrated_suite"/);
  assert.match(reviewJs, /G1000 and G1000 NXi remain separate products/);
  assert.match(appCss, /\.review-existing-structure-form/);
});

test("brings the product review workspace into view from a queue row action", () => {
  assert.match(
    indexHtml,
    /id="review-product-workspace"[^>]*tabindex="-1"/,
  );
  const openProductReview = reviewJs.match(
    /async function openProductReview\(productId\) \{[\s\S]*?\n\}/,
  )?.[0] ?? "";
  assert.match(openProductReview, /classList\.remove\("is-hidden"\)/);
  assert.match(openProductReview, /focus\(\{ preventScroll: true \}\)/);
  assert.match(openProductReview, /scrollIntoView\(\{/);
  assert.match(openProductReview, /behavior: "smooth"/);
});

test("offers a guarded destructive avionics product action and refreshes its consumers", () => {
  assert.match(
    indexHtml,
    /id="delete-avionics-product"[^>]*>Delete product<\/button>/,
  );
  assert.match(avionicsJs, /Delete product \"\$\{productName\}\"\?/);
  assert.match(avionicsJs, /every direct and pending listing occurrence/);
  assert.match(avionicsJs, /Additional pending review occurrences may also be removed/);
  assert.match(
    avionicsJs,
    /api\(`\/api\/avionics\/\$\{productId\}`[^]*method: "DELETE"/,
  );
  assert.match(avionicsJs, /affectedListingCount/);
  assert.match(avionicsJs, /refreshListings\(\)/);
  assert.match(avionicsJs, /refreshReview\(\)/);
  assert.match(avionicsJs, /Could not delete \$\{productName\}: \$\{error\.message\}/);
});

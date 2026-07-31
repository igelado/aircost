import { displayLabel, renderAvionicsChips, safeDetailLink } from "/avionics.js";
import {
  filterPipelineRows,
  pipelineAutomaticEligibility,
  pipelineBacklogCategories,
  pipelineCheckpoint,
  pipelineProviderPlan,
  pipelineRowsFromResponse,
  pipelineServiceStatus,
  pipelineSummary,
  verificationRunIdempotencyKey,
  verificationRunRequest,
  verificationRunState,
  verificationRunStatusView,
} from "/review/automation.mjs";
import {
  REVIEW_AREAS,
  REVIEW_PRODUCT_IDENTITY_LIMITS,
  aircraftIdentityIsVerified,
  associationsNeedingSourceRecovery,
  authoritativeIdentityUrl,
  autoVerifiableProductAssociations,
  characterLimitState,
  describeAircraftIdentity,
  describeProductAssociationOutcome,
  describeResolvedListingOutcome,
  describeReviewReasons,
  existingProductVerificationRequest,
  isAircraftIdentityStatus,
  isCompletedReviewMaintenanceResponse,
  listingAssociationCanValidateLocally,
  preselectedReviewAction,
  productAssociationEvidenceDisplay,
  productAssociationEligibilityOutcomeForAttestation,
  productActionContextIsCurrent,
  productAttestationDraft,
  productDetailRequestMayCommit,
  reviewAreaForAspect,
  reviewProductIdentitySourceValidation,
  runProductAssociationWorkers,
  summarizeProductAssociations,
  summarizeProductReviewGroups,
} from "/review/domain.mjs";

const REVIEW_LISTING_PARAM = "review_listing";
const REVIEW_AREA_PARAM = "review_area";
const QUEUE_LIMIT = 100;
const CATALOG_RESULT_LIMIT = 8;
const CATALOG_SEARCH_DELAY_MS = 250;
const VERIFICATION_RUN_POLL_MS = 2000;
const VERIFICATION_RUN_STORAGE_KEY = "aircost.current-verification-run-id";
const SUPPORTED_ACTIONS = Object.freeze([
  "use_verified_product",
  "create_verified_product",
  "discard",
]);

let activatePanel;
let api;
let formatDate;
let formatNumber;
let refreshAvionics;
let refreshListings;
let setButtonBusy;

const state = {
  reviews: [],
  total: 0,
  limit: QUEUE_LIMIT,
  offset: 0,
  queueLoaded: false,
  queueMode: "pipeline",
  queueRequestSequence: 0,
  pipelineRequestSequence: 0,
  pipelineLoaded: false,
  pipelineResponses: [],
  pipelineRows: [],
  pipelineFilter: "all",
  pipelineSearch: "",
  pipelineSelectedListingIds: new Set(),
  verificationRunRequestSequence: 0,
  verificationRunPollTimer: null,
  activeVerificationRunId: null,
  activeVerificationRun: null,
  activeVerificationRunItems: [],
  activeVerificationRunItemByListing: new Map(),
  reconciledVerificationRunId: null,
  verificationRunCreating: false,
  verificationRunCancelling: false,
  productRequestSequence: 0,
  productDetailRequestSequence: 0,
  productGroups: [],
  selectedProduct: null,
  productAssociations: [],
  productOutcomes: new Map(),
  productBusy: false,
  productBusyProductId: null,
  productActionSequence: 0,
  detailRequestSequence: 0,
  currentReview: null,
  drafts: new Map(),
  aspectViews: new Map(),
  catalogSearchTimers: new Map(),
  catalogSearchSequences: new Map(),
  activeArea: "avionics",
  stale: false,
  resolving: false,
  automating: false,
  automationControlStates: new Map(),
};

const elements = {};
let initialized = false;

export function initializeReviewWorkspace(shared) {
  if (initialized) {
    throw new Error("The listing review workspace is already initialized.");
  }
  ({
    activatePanel,
    api,
    formatDate,
    formatNumber,
    refreshAvionics,
    refreshListings,
    setButtonBusy,
  } = shared);
  collectElements();
  bindEvents();
  setQueueMode(state.queueMode, { load: false });
  state.activeVerificationRunId = storedVerificationRunId();
  initialized = true;

  return Object.freeze({
    activate() {
      const listingId = reviewListingIdFromLocation();
      if (listingId !== null) {
        setQueueMode("listing", { load: false });
        state.activeArea = reviewAreaFromLocation() ?? "avionics";
        const queueLoad = state.queueLoaded ? Promise.resolve() : loadQueue({ quiet: true });
        const pipelineLoad = state.pipelineLoaded
          ? Promise.resolve()
          : loadPipelineQueue({ quiet: true });
        const detailLoad = openReview(listingId, { historyMode: "none", discardDraft: true });
        const runLoad = state.activeVerificationRunId === null
          ? Promise.resolve()
          : resumeVerificationRun(state.activeVerificationRunId);
        return Promise.allSettled([queueLoad, pipelineLoad, detailLoad, runLoad]);
      }
      showQueue({ historyMode: "none", discardDraft: true });
      const pipelineLoad = state.pipelineLoaded
        ? Promise.resolve()
        : loadPipelineQueue();
      const runLoad = state.activeVerificationRunId === null
        ? Promise.resolve()
        : resumeVerificationRun(state.activeVerificationRunId);
      return Promise.allSettled([pipelineLoad, runLoad]);
    },
    refresh() {
      return refreshActiveQueue();
    },
    restoreFromLocation() {
      if (reviewListingIdFromLocation() !== null) {
        activatePanel("review-panel");
      }
    },
  });
}

function collectElements() {
  for (const [key, selector] of Object.entries({
    reviewPanel: "#review-panel",
    reviewPendingCount: "#review-pending-count",
    reviewPendingLabel: "#review-pending-label",
    reviewAspectCount: "#review-aspect-count",
    reviewAspectLabel: "#review-aspect-label",
    reviewReasonCount: "#review-reason-count",
    reviewReasonLabel: "#review-reason-label",
    reviewAttestationCount: "#review-attestation-count",
    reviewAttestationLabel: "#review-attestation-label",
    reviewManualCount: "#review-manual-count",
    reviewManualLabel: "#review-manual-label",
    reviewQueueView: "#review-queue-view",
    reviewQueueTitle: "#review-queue-title",
    reviewQueueDescription: "#review-queue-description",
    reviewModePipeline: "#review-mode-pipeline",
    reviewModeProduct: "#review-mode-product",
    reviewModeListing: "#review-mode-listing",
    refreshReviews: "#refresh-reviews",
    reviewQueueMessage: "#review-queue-message",
    reviewPipelineResults: "#review-pipeline-results",
    reviewPipelineSearch: "#review-pipeline-search",
    reviewPipelineFilter: "#review-pipeline-filter",
    reviewPipelineVisibleCount: "#review-pipeline-visible-count",
    reviewPipelinePlan: "#review-pipeline-plan",
    reviewPipelineTableBody: "#review-pipeline-table-body",
    emptyReviewPipeline: "#empty-review-pipeline",
    reviewPipelineCategories: "#review-pipeline-categories",
    reviewPipelineSelectAll: "#review-pipeline-select-all",
    reviewPipelineVerify: "#review-pipeline-verify",
    reviewPipelineSelectionCount: "#review-pipeline-selection-count",
    reviewRun: "#review-run",
    reviewRunTitle: "#review-run-title",
    reviewRunStatus: "#review-run-status",
    reviewRunCancel: "#review-run-cancel",
    reviewRunProgress: "#review-run-progress",
    reviewRunProgressLabel: "#review-run-progress-label",
    reviewRunCurrent: "#review-run-current",
    reviewRunCounts: "#review-run-counts",
    reviewRunItemsBody: "#review-run-items-body",
    reviewResults: "#review-results",
    reviewTableBody: "#review-table-body",
    emptyReviews: "#empty-reviews",
    reviewProductResults: "#review-product-results",
    reviewProductTableBody: "#review-product-table-body",
    emptyReviewProducts: "#empty-review-products",
    reviewProductWorkspace: "#review-product-workspace",
    reviewProductTitle: "#review-product-title",
    reviewProductSummary: "#review-product-summary",
    reviewProductStatus: "#review-product-status",
    reviewProductAttestationForm: "#review-product-attestation-form",
    reviewProductTitleCount: "#review-product-title-count",
    reviewProductEvidenceCount: "#review-product-evidence-count",
    reviewProductAttest: "#review-product-attest",
    reviewProductRecover: "#review-product-recover",
    reviewProductValidate: "#review-product-validate",
    reviewProductActionMessage: "#review-product-action-message",
    reviewProductAssociationBody: "#review-product-association-body",
    reviewProductTotalCount: "#review-product-total-count",
    reviewProductReadyCount: "#review-product-ready-count",
    reviewProductRecoveryCount: "#review-product-recovery-count",
    reviewProductAttestationCount: "#review-product-attestation-count",
    reviewProductManualCount: "#review-product-manual-count",
    reviewWorkspace: "#review-workspace",
    reviewWorkspaceTitle: "#review-workspace-title",
    reviewWorkspaceSubtitle: "#review-workspace-subtitle",
    reviewWorkspaceMessage: "#review-workspace-message",
    reviewBack: "#review-back",
    reviewNext: "#review-next",
    reviewReload: "#review-reload",
    reviewStale: "#review-stale",
    reviewSourceLabel: "#review-source-label",
    reviewSourceLink: "#review-source-link",
    reviewAircraftTab: "#review-aircraft-tab",
    reviewAircraftTabCount: "#review-aircraft-tab-count",
    reviewAircraftPanel: "#review-aircraft-panel",
    reviewAircraftSummary: "#review-aircraft-summary",
    reviewAvionicsTab: "#review-avionics-tab",
    reviewAvionicsTabCount: "#review-avionics-tab-count",
    reviewAvionicsPanel: "#review-avionics-panel",
    reviewAvionicsReasons: "#review-avionics-reasons",
    reviewAvionicsAspects: "#review-avionics-aspects",
    reviewProgress: "#review-progress",
    reviewProgressLabel: "#review-progress-label",
    automaticallyVerifyListing: "#automatically-verify-listing",
    verifyListing: "#verify-listing",
  })) {
    elements[key] = document.querySelector(selector);
    if (!elements[key]) {
      throw new Error(`Missing listing review element: ${selector}`);
    }
  }
}

function bindEvents() {
  elements.refreshReviews.addEventListener("click", () => refreshActiveQueue());
  elements.reviewModePipeline.addEventListener("click", () => setQueueMode("pipeline"));
  elements.reviewModeProduct.addEventListener("click", () => setQueueMode("product"));
  elements.reviewModeListing.addEventListener("click", () => setQueueMode("listing"));
  elements.reviewPipelineSearch.addEventListener("input", () => {
    state.pipelineSearch = elements.reviewPipelineSearch.value;
    renderPipelineTable();
  });
  elements.reviewPipelineFilter.addEventListener("change", () => {
    state.pipelineFilter = elements.reviewPipelineFilter.value;
    renderPipelineTable();
  });
  elements.reviewPipelineSelectAll.addEventListener("click", selectAllActionablePipelineRows);
  elements.reviewPipelineVerify.addEventListener("click", () => {
    startVerificationRun(Array.from(state.pipelineSelectedListingIds));
  });
  elements.reviewRunCancel.addEventListener("click", cancelActiveVerificationRun);
  elements.reviewPipelineTableBody.addEventListener("click", (event) => {
    const checkbox = event.target.closest("input[data-pipeline-listing-id]");
    const selectedListingId = positiveInteger(checkbox?.dataset.pipelineListingId);
    if (selectedListingId !== null) {
      if (checkbox.checked) {
        state.pipelineSelectedListingIds.add(selectedListingId);
      } else {
        state.pipelineSelectedListingIds.delete(selectedListingId);
      }
      renderPipelineSelection();
      return;
    }
    const button = event.target.closest("button[data-review-listing-id]");
    const listingId = positiveInteger(button?.dataset.reviewListingId);
    if (listingId !== null) {
      setQueueMode("listing", { load: false });
      openReview(listingId, { historyMode: "push" });
    }
  });
  elements.reviewRunItemsBody.addEventListener("click", (event) => {
    const button = event.target.closest("button[data-review-listing-id]");
    const listingId = positiveInteger(button?.dataset.reviewListingId);
    const row = pipelineRowForListing(listingId);
    if (listingId !== null && row?.hasPendingReview) {
      setQueueMode("listing", { load: false });
      openReview(listingId, { historyMode: "push", force: true });
    }
  });
  elements.reviewProductTableBody.addEventListener("click", (event) => {
    if (state.productBusy) {
      return;
    }
    const button = event.target.closest("button[data-review-product-id]");
    const productId = positiveInteger(button?.dataset.reviewProductId);
    if (productId !== null) {
      openProductReview(productId);
    }
  });
  elements.reviewProductAttestationForm.addEventListener(
    "submit",
    attestSelectedProduct,
  );
  elements.reviewProductValidate.addEventListener(
    "click",
    validateSelectedProductAssociations,
  );
  elements.reviewProductRecover.addEventListener(
    "click",
    recoverSelectedProductEvidence,
  );
  for (const [name, counter, limit] of [
    ["identity_source_title", elements.reviewProductTitleCount,
      REVIEW_PRODUCT_IDENTITY_LIMITS.sourceTitle],
    ["identity_evidence_text", elements.reviewProductEvidenceCount,
      REVIEW_PRODUCT_IDENTITY_LIMITS.evidenceText],
  ]) {
    const input = elements.reviewProductAttestationForm.elements.namedItem(name);
    input.addEventListener("input", () => {
      const value = characterLimitState(input.value, limit);
      counter.textContent = `${value.count} / ${value.limit}`;
      counter.classList.toggle("over-limit", value.overLimit);
    });
  }
  elements.reviewTableBody.addEventListener("click", (event) => {
    const button = event.target.closest("button[data-review-listing-id]");
    if (!button) {
      return;
    }
    const listingId = positiveInteger(button.dataset.reviewListingId);
    if (listingId !== null) {
      openReview(listingId, { historyMode: "push" });
    }
  });
  elements.reviewBack.addEventListener("click", () => {
    if (confirmDiscardDraft()) {
      showQueue({ historyMode: "push", discardDraft: true });
    }
  });
  elements.reviewNext.addEventListener("click", () => {
    const listingId = nextPendingListingId();
    if (listingId !== null && confirmDiscardDraft()) {
      openReview(listingId, { historyMode: "push", discardDraft: true });
    }
  });
  elements.reviewReload.addEventListener("click", () => {
    const listingId = currentListingId();
    if (listingId !== null) {
      openReview(listingId, { historyMode: "none", discardDraft: true, force: true });
    }
  });
  for (const area of REVIEW_AREAS) {
    const tab = reviewAreaElements(area).tab;
    tab.addEventListener("click", () => {
      setActiveReviewArea(area, { updateLocation: true });
    });
    tab.addEventListener("keydown", (event) => handleReviewTabKeydown(event, area));
  }
  elements.automaticallyVerifyListing.addEventListener(
    "click",
    automaticallyVerifyListing,
  );
  elements.verifyListing.addEventListener("click", resolveReview);
  window.addEventListener("popstate", () => {
    if (!elements.reviewPanel.classList.contains("is-active")) {
      return;
    }
    const openListingId = positiveInteger(state.currentReview?.listing_id);
    if (!confirmDiscardDraft()) {
      updateReviewLocation(openListingId, "push");
      return;
    }
    const listingId = reviewListingIdFromLocation();
    if (listingId === null) {
      showQueue({ historyMode: "none", discardDraft: true });
    } else {
      state.activeArea = reviewAreaFromLocation() ?? "avionics";
      openReview(listingId, { historyMode: "none", discardDraft: true, force: true });
    }
  });
}

function refreshActiveQueue() {
  if (state.queueMode === "pipeline") {
    return loadPipelineQueue();
  }
  return state.queueMode === "product" ? loadProductQueue() : loadQueue();
}

function setQueueMode(mode, { load = true } = {}) {
  state.queueMode = ["pipeline", "product", "listing"].includes(mode)
    ? mode
    : "pipeline";
  const pipelineMode = state.queueMode === "pipeline";
  const productMode = state.queueMode === "product";
  const listingMode = state.queueMode === "listing";
  for (const [candidate, button] of [
    ["pipeline", elements.reviewModePipeline],
    ["product", elements.reviewModeProduct],
    ["listing", elements.reviewModeListing],
  ]) {
    const active = state.queueMode === candidate;
    button.classList.toggle("is-active", active);
    button.classList.toggle("subtle", !active);
    button.setAttribute("aria-pressed", String(active));
  }
  elements.reviewPipelineResults.classList.toggle("is-hidden", !pipelineMode);
  elements.reviewProductResults.classList.toggle("is-hidden", !productMode);
  elements.reviewResults.classList.toggle("is-hidden", !listingMode);
  elements.reviewQueueTitle.textContent = pipelineMode
    ? "Verification pipeline"
    : productMode
      ? "Avionics needing verification"
      : "Listings needing verification";
  elements.reviewQueueDescription.textContent = pipelineMode
    ? "Provider-free status across every non-ready listing."
    : productMode
      ? "Verify each catalog identity and reusable source independently, then validate eligible listing associations locally."
      : "Review every unresolved aspect of one listing at a time.";
  for (const metric of document.querySelectorAll(".review-product-only-metric")) {
    metric.classList.toggle("is-hidden", !productMode);
  }
  if (pipelineMode) {
    elements.reviewPendingLabel.textContent = "Non-ready listings";
    elements.reviewAspectLabel.textContent = "Manual review";
    elements.reviewReasonLabel.textContent = "Reference pending";
    renderPipelineMetrics();
  } else if (productMode) {
    elements.reviewPendingLabel.textContent = "Total pending";
    elements.reviewAspectLabel.textContent = "Ready locally";
    elements.reviewReasonLabel.textContent = "Needs source recovery";
    elements.reviewAttestationLabel.textContent = "Products needing source check";
    elements.reviewManualLabel.textContent = "Manual or ambiguous";
    if (state.productGroups.length) {
      renderProductQueue();
    }
  } else {
    elements.reviewPendingLabel.textContent = "Pending listings";
    elements.reviewAspectLabel.textContent = "Pending checks";
    elements.reviewReasonLabel.textContent = "Issue types";
    if (state.queueLoaded) {
      renderQueue();
    }
  }
  if (!load) {
    return;
  }
  if (pipelineMode) {
    loadPipelineQueue();
  } else if (productMode) {
    loadProductQueue();
  } else {
    loadQueue();
  }
}

async function loadPipelineQueue({ quiet = false } = {}) {
  const sequence = ++state.pipelineRequestSequence;
  if (!quiet) {
    setQueueMessage("Running provider-free verification preflight…");
  }
  elements.reviewPipelineResults.setAttribute("aria-busy", "true");
  setButtonBusy(elements.refreshReviews, true);
  try {
    const responses = [];
    const seenCheckpoints = new Set();
    let afterListingId = null;
    do {
      const params = new URLSearchParams({ limit: String(QUEUE_LIMIT) });
      if (afterListingId !== null) {
        params.set("after_listing_id", String(afterListingId));
      }
      const payload = await api(
        `/api/review/verification/preflight?${params}`,
      );
      if (sequence !== state.pipelineRequestSequence) {
        return false;
      }
      responses.push(payload);
      const checkpoint = pipelineCheckpoint(payload);
      if (!checkpoint.valid) {
        throw new Error(
          "The server reported another pipeline page without a usable resume checkpoint.",
        );
      }
      if (!checkpoint.hasMore) {
        break;
      }
      if (seenCheckpoints.has(checkpoint.resumeAfterListingId)) {
        throw new Error("The server repeated a verification pipeline checkpoint.");
      }
      seenCheckpoints.add(checkpoint.resumeAfterListingId);
      afterListingId = checkpoint.resumeAfterListingId;
    } while (true);

    if (sequence !== state.pipelineRequestSequence) {
      return false;
    }
    state.pipelineResponses = responses;
    state.pipelineRows = responses.flatMap(pipelineRowsFromResponse);
    const actionableIds = new Set(
      state.pipelineRows
        .filter((row) => pipelineAutomaticEligibility(row).eligible)
        .map((row) => row.listingId),
    );
    state.pipelineSelectedListingIds = new Set(
      Array.from(state.pipelineSelectedListingIds)
        .filter((listingId) => actionableIds.has(listingId)),
    );
    state.pipelineLoaded = true;
    if (state.queueMode === "pipeline") {
      renderPipeline();
    } else {
      updateProgress();
    }
    if (!quiet) {
      setQueueMessage(
        `${state.pipelineRows.length} non-ready `
          + `${pluralize(state.pipelineRows.length, "listing")} checked locally. `
          + "No provider calls or domain writes were made.",
      );
    }
    return true;
  } catch (error) {
    if (sequence === state.pipelineRequestSequence) {
      setQueueMessage(
        `Could not load verification pipeline: ${error.message}`,
        true,
      );
    }
    return false;
  } finally {
    if (sequence === state.pipelineRequestSequence) {
      elements.reviewPipelineResults.setAttribute("aria-busy", "false");
      setButtonBusy(elements.refreshReviews, false);
    }
  }
}

function renderPipeline() {
  renderPipelineMetrics();
  renderPipelinePlan();
  renderPipelineTable();
  renderPipelineSelection();
  renderVerificationRun();
}

function renderPipelineMetrics() {
  const summary = pipelineSummary(state.pipelineRows);
  elements.reviewPendingCount.textContent = formatNumber(summary.total, 0);
  elements.reviewAspectCount.textContent = formatNumber(summary.manualReview, 0);
  elements.reviewReasonCount.textContent =
    formatNumber(summary.referencePending, 0);
  elements.reviewPipelineCategories.replaceChildren(
    ...pipelineBacklogCategories(state.pipelineRows).map(
      pipelineBacklogCategoryCard,
    ),
  );
}

function pipelineBacklogCategoryCard(category) {
  const card = document.createElement("article");
  card.className = "review-pipeline-category";
  card.dataset.category = category.key;
  const heading = document.createElement("div");
  const label = document.createElement("h3");
  label.textContent = category.label;
  const count = document.createElement("strong");
  count.textContent = formatNumber(category.count, 0);
  count.setAttribute(
    "aria-label",
    `${category.count} ${pluralize(category.count, "listing")}`,
  );
  heading.append(label, count);
  const description = document.createElement("p");
  description.textContent = category.description;
  card.append(heading, description);
  return card;
}

function renderPipelinePlan() {
  const plan = pipelineProviderPlan(state.pipelineResponses);
  const services = pipelineServiceStatus(state.pipelineResponses, plan);
  const heading = document.createElement("strong");
  heading.textContent = "Automatic verification request plan";
  const detail = document.createElement("p");
  detail.textContent = [
    `${plan.verifiedLocalIdentityComponents} identity `
      + `${pluralize(plan.verifiedLocalIdentityComponents, "component")} reusable locally`,
    `${plan.aircraftGroundingCandidates} aircraft grounding `
      + `${pluralize(plan.aircraftGroundingCandidates, "candidate")}`,
    `${plan.minimumBaselineRequests} minimum baseline Gemini requests`,
    `${plan.allPositiveBaselineRequests} if all identities are positive`,
    `${plan.validationEnvelopeMaximum} maximum validation envelope`,
  ].join(" · ");
  const note = document.createElement("p");
  note.className = "review-pipeline-plan-note";
  note.textContent = [
    "This preflight made no Gemini calls and wrote no data.",
    ...(!plan.includesFinalizationEnrichment
      ? ["Finalization enrichment is not included in these request counts."]
      : []),
  ].join(" ");
  const serviceList = document.createElement("div");
  serviceList.className = "review-pipeline-services";
  serviceList.append(
    pipelineServiceBadge("Gemini", services.geminiConfigured),
    pipelineServiceBadge("FAA DRS", services.faaDrsConfigured),
  );
  const warnings = document.createElement("div");
  warnings.className = "review-pipeline-warnings";
  warnings.replaceChildren(...services.warnings.map((warning) => {
    const item = document.createElement("p");
    item.textContent = warning;
    return item;
  }));
  elements.reviewPipelinePlan.replaceChildren(
    heading,
    detail,
    note,
    serviceList,
    warnings,
  );
}

function pipelineServiceBadge(label, configured) {
  const badge = document.createElement("span");
  badge.className = `review-pipeline-service ${configured ? "is-ready" : "is-missing"}`;
  badge.textContent = `${label}: ${configured ? "configured" : "not configured"}`;
  return badge;
}

function renderPipelineTable() {
  const rows = filterPipelineRows(
    state.pipelineRows,
    state.pipelineFilter,
    state.pipelineSearch,
  );
  elements.reviewPipelineTableBody.replaceChildren(
    ...rows.map(pipelineTableRow),
  );
  elements.emptyReviewPipeline.classList.toggle("is-hidden", rows.length > 0);
  elements.reviewPipelineVisibleCount.textContent =
    `${rows.length} of ${state.pipelineRows.length} `
      + pluralize(state.pipelineRows.length, "listing");
}

function pipelineTableRow(item) {
  const row = document.createElement("tr");
  const eligibility = pipelineAutomaticEligibility(item);
  const selection = document.createElement("td");
  selection.dataset.label = "Select";
  if (eligibility.eligible) {
    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    checkbox.dataset.pipelineListingId = String(item.listingId);
    checkbox.checked = state.pipelineSelectedListingIds.has(item.listingId);
    checkbox.disabled = activeVerificationRunIsBusy();
    checkbox.setAttribute(
      "aria-label",
      `Select ${item.label} for automatic verification`,
    );
    selection.append(checkbox);
  } else {
    const unavailable = document.createElement("span");
    unavailable.className = "review-pipeline-no-action";
    unavailable.textContent = "—";
    unavailable.title = eligibility.reason;
    unavailable.setAttribute("aria-label", `Not selectable: ${eligibility.reason}`);
    selection.append(unavailable);
  }
  const listing = document.createElement("td");
  listing.dataset.label = "Listing";
  const label = document.createElement("strong");
  label.textContent = item.label;
  const metadata = document.createElement("span");
  metadata.className = "review-pipeline-listing-meta";
  metadata.textContent = [
    item.registrationNumber,
    item.modelYear,
    `#${item.listingId}`,
  ].filter((value) => value !== null && value !== "").join(" · ");
  listing.append(label, metadata);

  const aircraft = pipelineStageCell("Aircraft", item.aircraft);
  const avionics = pipelineStageCell("Avionics", item.avionics);
  const reference = pipelineStageCell("Reference", item.reference);
  const gemini = document.createElement("td");
  gemini.dataset.label = "Gemini";
  const geminiStatus = document.createElement("span");
  geminiStatus.className = `review-pipeline-gemini is-${item.gemini.kind}`;
  geminiStatus.textContent = item.gemini.label;
  geminiStatus.title = item.gemini.detail;
  gemini.append(geminiStatus);

  const reason = queueTextCell("What remains", item.reason);
  reason.classList.add("review-pipeline-reason");
  const runResult = document.createElement("td");
  runResult.dataset.label = "Run result";
  const runItem = state.activeVerificationRunItemByListing.get(item.listingId);
  if (runItem) {
    const view = verificationRunStatusView(runItem.status);
    const status = document.createElement("span");
    status.className = `review-pipeline-stage is-${view.tone}`;
    status.textContent = view.label;
    status.title = runItem.reason || view.detail;
    runResult.append(status);
  } else {
    runResult.textContent = "—";
  }
  const action = document.createElement("td");
  action.dataset.label = "Actions";
  if (
    item.hasPendingReview
    && !(
      activeVerificationRunIsBusy()
      && verificationRunIncludesListing(item.listingId)
    )
  ) {
    const open = document.createElement("button");
    open.type = "button";
    open.className = "button review-open-button";
    open.dataset.reviewListingId = String(item.listingId);
    open.textContent = "Open manual review";
    open.setAttribute("aria-label", `Open manual review for ${item.label}`);
    action.append(open);
  } else {
    const unavailable = document.createElement("span");
    unavailable.className = "review-pipeline-no-action";
    unavailable.textContent = activeVerificationRunIsBusy()
      && verificationRunIncludesListing(item.listingId)
      ? "Automatic verification running"
      : item.reference.status === "pending_reference"
        ? "Identity review complete"
        : "No manual review available";
    action.append(unavailable);
  }
  row.append(
    selection,
    listing,
    aircraft,
    avionics,
    reference,
    gemini,
    reason,
    runResult,
    action,
  );
  return row;
}

function pipelineStageCell(label, value) {
  const cell = document.createElement("td");
  cell.dataset.label = label;
  const status = document.createElement("span");
  status.className = `review-pipeline-stage is-${value.tone}`;
  status.textContent = value.label;
  if (value.reason) {
    status.title = value.reason;
  }
  cell.append(status);
  return cell;
}

function pipelineRowForListing(listingId) {
  return state.pipelineRows.find((row) => row.listingId === listingId) || null;
}

function selectAllActionablePipelineRows() {
  if (activeVerificationRunIsBusy()) {
    return;
  }
  const actionable = state.pipelineRows
    .filter((row) => pipelineAutomaticEligibility(row).eligible)
    .map((row) => row.listingId);
  const allSelected = actionable.length > 0
    && actionable.every((listingId) => (
      state.pipelineSelectedListingIds.has(listingId)
    ));
  state.pipelineSelectedListingIds = new Set(allSelected ? [] : actionable);
  renderPipelineTable();
  renderPipelineSelection();
}

function renderPipelineSelection() {
  const selectedCount = state.pipelineSelectedListingIds.size;
  const actionable = state.pipelineRows.filter(
    (row) => pipelineAutomaticEligibility(row).eligible,
  );
  const busy = activeVerificationRunIsBusy();
  elements.reviewPipelineSelectionCount.textContent =
    `${selectedCount} ${pluralize(selectedCount, "listing")} selected`;
  elements.reviewPipelineVerify.textContent = selectedCount > 0
    ? `Automatically verify ${selectedCount} selected`
    : "Automatically verify selected";
  elements.reviewPipelineVerify.disabled = busy || selectedCount === 0;
  elements.reviewPipelineSelectAll.disabled = busy || actionable.length === 0;
  const allSelected = actionable.length > 0
    && actionable.every((row) => (
      state.pipelineSelectedListingIds.has(row.listingId)
    ));
  elements.reviewPipelineSelectAll.textContent = allSelected
    ? "Clear selection"
    : "Select all automatic candidates";
}

function activeVerificationRunIsBusy() {
  return state.verificationRunCreating
    || (
      state.activeVerificationRun !== null
      && !state.activeVerificationRun.terminal
    );
}

function verificationRunIncludesListing(listingId) {
  return state.activeVerificationRunItems.some(
    (item) => item.listingId === listingId,
  );
}

function synchronizeOpenedListingAutomationBusy(
  run = state.activeVerificationRun,
) {
  const listingId = currentListingId();
  setAutomaticVerificationBusy(Boolean(
    state.currentReview
    && listingId !== null
    && run
    && !run.terminal
    && verificationRunIncludesListing(listingId)
  ));
}

async function startVerificationRun(listingIds, { openedListing = false } = {}) {
  const request = verificationRunRequest(listingIds);
  if (
    request.listing_ids.length === 0
    || state.verificationRunCreating
    || activeVerificationRunIsBusy()
  ) {
    return;
  }
  const rows = request.listing_ids
    .map(pipelineRowForListing)
    .filter(Boolean);
  if (
    rows.length > 0
    && rows.some((row) => !pipelineAutomaticEligibility(row).eligible)
  ) {
    const message = "Refresh the Pipeline before starting automatic verification.";
    if (openedListing) {
      setWorkspaceMessage(message, true);
    } else {
      setQueueMessage(message, true);
    }
    return;
  }
  const plan = pipelineProviderPlan(state.pipelineResponses);
  const unsavedWarning = openedListing && hasDraftDecisions()
    ? "This will discard the unsaved decisions in this review. "
    : "";
  const confirmed = window.confirm(
    `${unsavedWarning}Automatically verify ${request.listing_ids.length} `
      + `${pluralize(request.listing_ids.length, "listing")}? `
      + "Local FAA and catalog checks run first. Unresolved identities may use paid Gemini calls. "
      + `The current full Pipeline plan includes ${plan.aircraftGroundingCandidates} aircraft `
      + `grounding ${pluralize(plan.aircraftGroundingCandidates, "candidate")} and estimates `
      + `${plan.minimumBaselineRequests} minimum avionics baseline requests, `
      + `${plan.allPositiveBaselineRequests} if all avionics identities are positive, `
      + `and a ${plan.validationEnvelopeMaximum}-request avionics validation envelope; `
      + "finalization enrichment is additional. There is no hard budget. Continue?",
  );
  if (!confirmed) {
    return;
  }

  state.verificationRunCreating = true;
  if (openedListing) {
    setAutomaticVerificationBusy(true);
    setWorkspaceMessage("Creating a durable automatic verification run…");
  } else {
    setQueueMessage("Creating a durable automatic verification run…");
  }
  renderPipelineSelection();
  try {
    const payload = await api("/api/review/verification-runs", {
      method: "POST",
      headers: { "Idempotency-Key": verificationRunIdempotencyKey() },
      body: JSON.stringify(request),
    });
    const runId = positiveInteger(payload?.run?.id);
    if (runId === null) {
      throw new Error("The server did not return a verification run ID.");
    }
    rememberVerificationRunId(runId);
    state.reconciledVerificationRunId = null;
    state.pipelineSelectedListingIds.clear();
    await resumeVerificationRun(runId);
  } catch (error) {
    const activeRunId = error?.status === 409
      ? positiveInteger(error?.payload?.error?.active_run_id)
      : null;
    if (activeRunId !== null) {
      rememberVerificationRunId(activeRunId);
      await resumeVerificationRun(activeRunId);
    } else if (openedListing) {
      setWorkspaceMessage(
        `Could not start automatic verification: ${error.message}`,
        true,
      );
    } else {
      setQueueMessage(
        `Could not start automatic verification: ${error.message}`,
        true,
      );
    }
  } finally {
    state.verificationRunCreating = false;
    if (openedListing) {
      synchronizeOpenedListingAutomationBusy();
    }
    renderPipelineTable();
    renderPipelineSelection();
    updateProgress();
  }
}

function rememberVerificationRunId(runId) {
  if (state.activeVerificationRunId !== runId) {
    state.reconciledVerificationRunId = null;
  }
  state.activeVerificationRunId = runId;
  try {
    window.localStorage.setItem(
      VERIFICATION_RUN_STORAGE_KEY,
      String(runId),
    );
  } catch {
    // A disabled local store only removes reload recovery; server state remains authoritative.
  }
}

function storedVerificationRunId() {
  try {
    return positiveInteger(
      Number(window.localStorage.getItem(VERIFICATION_RUN_STORAGE_KEY)),
    );
  } catch {
    return null;
  }
}

function forgetVerificationRunId(runId) {
  if (state.activeVerificationRunId !== runId) {
    return;
  }
  state.activeVerificationRunId = null;
  state.activeVerificationRun = null;
  state.activeVerificationRunItems = [];
  state.activeVerificationRunItemByListing.clear();
  state.reconciledVerificationRunId = null;
  try {
    window.localStorage.removeItem(VERIFICATION_RUN_STORAGE_KEY);
  } catch {
    // The server remains authoritative even when local recovery state is unavailable.
  }
}

async function resumeVerificationRun(runId) {
  const normalizedRunId = positiveInteger(runId);
  if (normalizedRunId === null) {
    return false;
  }
  if (state.verificationRunPollTimer !== null) {
    window.clearTimeout(state.verificationRunPollTimer);
    state.verificationRunPollTimer = null;
  }
  const sequence = ++state.verificationRunRequestSequence;
  try {
    const [runPayload, items] = await Promise.all([
      api(`/api/review/verification-runs/${normalizedRunId}`),
      loadVerificationRunItems(normalizedRunId, sequence),
    ]);
    if (sequence !== state.verificationRunRequestSequence) {
      return false;
    }
    const view = verificationRunState(runPayload?.run, items);
    if (view.id !== normalizedRunId || view.status === "unknown") {
      throw new Error("The server returned an invalid verification run.");
    }
    rememberVerificationRunId(normalizedRunId);
    state.activeVerificationRun = view;
    state.activeVerificationRunItems = view.items;
    state.activeVerificationRunItemByListing = new Map(
      view.items.map((item) => [item.listingId, item]),
    );
    if (!view.terminal) {
      synchronizeOpenedListingAutomationBusy(view);
    }
    renderVerificationRun();
    renderPipelineTable();
    renderPipelineSelection();
    updateProgress();
    updateOpenedListingRunProgress(view);

    if (view.terminal) {
      await reconcileCompletedVerificationRun(view, sequence);
      if (sequence === state.verificationRunRequestSequence) {
        synchronizeOpenedListingAutomationBusy(view);
      }
    } else {
      state.verificationRunPollTimer = window.setTimeout(
        () => resumeVerificationRun(normalizedRunId),
        VERIFICATION_RUN_POLL_MS,
      );
    }
    return true;
  } catch (error) {
    if (sequence !== state.verificationRunRequestSequence) {
      return false;
    }
    if (error?.status === 404) {
      forgetVerificationRunId(normalizedRunId);
      synchronizeOpenedListingAutomationBusy();
      renderVerificationRun();
      renderPipelineTable();
      renderPipelineSelection();
    }
    const message = `Could not refresh automatic verification run: ${error.message}`;
    if (state.currentReview) {
      setWorkspaceMessage(message, true);
    } else {
      setQueueMessage(message, true);
    }
    return false;
  }
}

async function loadVerificationRunItems(runId, sequence) {
  const items = [];
  const seenCheckpoints = new Set();
  let afterItemId = null;
  do {
    const params = new URLSearchParams({ limit: String(QUEUE_LIMIT) });
    if (afterItemId !== null) {
      params.set("after_item_id", String(afterItemId));
    }
    const payload = await api(
      `/api/review/verification-runs/${runId}/items?${params}`,
    );
    if (sequence !== state.verificationRunRequestSequence) {
      return [];
    }
    items.push(...(Array.isArray(payload?.items) ? payload.items : []));
    const hasMore = payload?.checkpoint?.has_more === true;
    if (!hasMore) {
      break;
    }
    const resumeAfterItemId = positiveInteger(
      payload?.checkpoint?.resume_after_item_id,
    );
    if (
      resumeAfterItemId === null
      || seenCheckpoints.has(resumeAfterItemId)
    ) {
      throw new Error("The server returned an invalid run-item checkpoint.");
    }
    seenCheckpoints.add(resumeAfterItemId);
    afterItemId = resumeAfterItemId;
  } while (true);
  return items;
}

async function cancelActiveVerificationRun() {
  const runId = state.activeVerificationRun?.id;
  if (
    !activeVerificationRunIsBusy()
    || positiveInteger(runId) === null
    || state.verificationRunCancelling
  ) {
    return;
  }
  state.verificationRunCancelling = true;
  elements.reviewRunCancel.disabled = true;
  elements.reviewRunStatus.textContent =
    "Requesting a stop after the current listing…";
  try {
    await api(`/api/review/verification-runs/${runId}/cancel`, {
      method: "POST",
    });
    await resumeVerificationRun(runId);
  } catch (error) {
    setQueueMessage(`Could not stop verification run: ${error.message}`, true);
  } finally {
    state.verificationRunCancelling = false;
    renderVerificationRun();
  }
}

function renderVerificationRun() {
  const run = state.activeVerificationRun;
  if (!run) {
    elements.reviewRun.classList.add("is-hidden");
    return;
  }
  elements.reviewRun.classList.remove("is-hidden");
  const status = verificationRunStatusView(run.status);
  elements.reviewRunTitle.textContent = `Verification run #${run.id}`;
  elements.reviewRunStatus.textContent = run.status === "cancelled"
    ? "Stopped. The run stopped after its current listing."
    : `${status.label}. ${status.detail}`;
  elements.reviewRunProgress.max = Math.max(run.total, 1);
  elements.reviewRunProgress.value = Math.min(run.completed, run.total);
  elements.reviewRunProgressLabel.textContent =
    `${run.completed} of ${run.total} complete`;
  const current = pipelineRowForListing(run.currentListingId);
  elements.reviewRunCurrent.textContent = run.currentListingId === null
    ? ""
    : `Currently processing ${current?.label || `listing #${run.currentListingId}`}`;
  elements.reviewRunCancel.classList.toggle(
    "is-hidden",
    !["queued", "running"].includes(run.status),
  );
  elements.reviewRunCancel.disabled = state.verificationRunCancelling;

  const countViews = [
    ["Queued", run.counts.queued],
    ["Running", run.counts.running],
    ["Verified", run.counts.verified],
    ["Manual review", run.counts.pendingReview],
    ["Reference pending", run.counts.pendingReference],
    ["Blocked", run.counts.blocked],
    ["Failed", run.counts.failed],
    ["Cancelled", run.counts.cancelled],
  ];
  elements.reviewRunCounts.replaceChildren(...countViews.map(([label, count]) => {
    const item = document.createElement("span");
    item.textContent = `${label}: ${count}`;
    return item;
  }));
  elements.reviewRunItemsBody.replaceChildren(
    ...run.items.map(verificationRunItemRow),
  );
}

function verificationRunItemRow(item) {
  const row = document.createElement("tr");
  const pipelineRow = pipelineRowForListing(item.listingId);
  const statusView = verificationRunStatusView(item.status);
  const listing = queueTextCell(
    "Listing",
    pipelineRow?.label || `Listing #${item.listingId}`,
  );
  const result = document.createElement("td");
  result.dataset.label = "Result";
  const status = document.createElement("span");
  status.className = `review-pipeline-stage is-${statusView.tone}`;
  status.textContent = statusView.label;
  result.append(status);
  const detail = queueTextCell(
    "Detail",
    verificationRunItemDetail(item, statusView.detail),
  );
  const action = document.createElement("td");
  action.dataset.label = "Actions";
  if (
    item.status === "pending_review"
    && state.reconciledVerificationRunId === state.activeVerificationRun?.id
    && pipelineRow?.hasPendingReview
  ) {
    const open = document.createElement("button");
    open.type = "button";
    open.className = "button";
    open.dataset.reviewListingId = String(item.listingId);
    open.textContent = "Open manual review";
    open.setAttribute(
      "aria-label",
      `Open manual review for ${pipelineRow.label}`,
    );
    action.append(open);
  } else {
    action.textContent = "—";
  }
  row.append(listing, result, detail, action);
  return row;
}

function verificationRunItemDetail(item, fallback) {
  return item.reason
    || optionalText(item.outcome?.finalization?.reason)
    || optionalText(item.outcome?.avionics?.reason)
    || optionalText(item.outcome?.aircraft?.reason)
    || fallback;
}

function updateOpenedListingRunProgress(run) {
  const listingId = currentListingId();
  if (
    listingId === null
    || !verificationRunIncludesListing(listingId)
  ) {
    return;
  }
  const item = state.activeVerificationRunItemByListing.get(listingId);
  const status = verificationRunStatusView(item?.status || run.status);
  setWorkspaceMessage(
    `${status.label}: ${verificationRunItemDetail(item || {}, status.detail)} `
      + `Run progress: ${run.completed} of ${run.total} complete.`,
    item?.status === "failed",
  );
}

async function reconcileCompletedVerificationRun(run, sequence) {
  await Promise.allSettled([
    loadPipelineQueue({ quiet: true }),
    loadQueue({ quiet: true }),
    Promise.resolve(refreshListings?.()),
    Promise.resolve(refreshAvionics?.()),
  ]);
  if (sequence !== state.verificationRunRequestSequence) {
    return;
  }
  state.reconciledVerificationRunId = run.id;
  if (state.queueMode === "pipeline") {
    renderPipelineMetrics();
    renderPipelinePlan();
  }
  renderVerificationRun();
  renderPipelineTable();
  renderPipelineSelection();
  const listingId = currentListingId();
  const item = state.activeVerificationRunItemByListing.get(listingId);
  if (!item) {
    setQueueMessage(
      `Verification run #${run.id} ${run.status}. Review the terminal results below.`,
    );
    return;
  }
  const status = verificationRunStatusView(item.status);
  if (item.status === "verified" || item.status === "pending_reference") {
    await leaveAutomaticallyVerifiedReview(listingId, state.reviews.slice(), status.label);
    return;
  }
  const refreshedRow = pipelineRowForListing(listingId);
  if (refreshedRow?.hasPendingReview) {
    await openReview(listingId, {
      historyMode: "none",
      discardDraft: true,
      force: true,
    });
  }
  setWorkspaceMessage(
    `${status.label}: ${verificationRunItemDetail(item, status.detail)}`,
    item.status === "failed",
  );
}

async function loadProductQueue({ quiet = false, commitGuard = null } = {}) {
  const sequence = ++state.productRequestSequence;
  const mayCommit = () => (
    sequence === state.productRequestSequence
    && (commitGuard === null || commitGuard())
  );
  if (!quiet) {
    setQueueMessage("Loading product review queue…");
  }
  elements.reviewProductResults.setAttribute("aria-busy", "true");
  setButtonBusy(elements.refreshReviews, true);
  try {
    const prepared = await api("/api/review/avionics/products/prepare", {
      method: "POST",
    });
    if (!mayCommit()) {
      return false;
    }
    const preparedRevision = nonBlank(prepared?.catalog_revision_sha256)
      ? prepared.catalog_revision_sha256
      : null;
    if (preparedRevision === null) {
      throw new Error("The server returned an invalid product preparation result.");
    }
    const groups = [];
    let cursor = null;
    let catalogRevision = null;
    do {
      const params = new URLSearchParams({ limit: String(QUEUE_LIMIT) });
      if (cursor) {
        params.set("cursor", cursor);
      }
      const page = await api(`/api/review/avionics/products?${params}`);
      if (!mayCommit()) {
        return false;
      }
      if (
        page.catalog_revision_sha256 !== preparedRevision
        || catalogRevision !== null && page.catalog_revision_sha256 !== catalogRevision
      ) {
        throw new Error("The avionics catalog changed while the product queue was loading.");
      }
      catalogRevision = page.catalog_revision_sha256;
      groups.push(...(Array.isArray(page?.items) ? page.items : []));
      cursor = nonBlank(page?.next_cursor) ? page.next_cursor : null;
    } while (cursor);
    if (!mayCommit()) {
      return false;
    }
    state.productGroups = groups.sort((left, right) => {
      const statusOrder = Number(left?.attestation_status !== "current")
        - Number(right?.attestation_status !== "current");
      return statusOrder
        || (positiveInteger(left?.product?.id) ?? 0)
          - (positiveInteger(right?.product?.id) ?? 0);
    });
    renderProductQueue();
    if (!quiet) {
      setQueueMessage(
        `${groups.length} ${pluralize(groups.length, "product")} with pending associations. `
          + `Prepared ${prepared.restaged_listing_count ?? 0} of `
          + `${prepared.inspected_listing_count ?? 0} inspected listings.`,
      );
    }
    return true;
  } catch (error) {
    if (mayCommit()) {
      setQueueMessage(`Could not load product review queue: ${error.message}`, true);
    }
    return false;
  } finally {
    if (sequence === state.productRequestSequence) {
      elements.reviewProductResults.setAttribute("aria-busy", "false");
      setButtonBusy(elements.refreshReviews, false);
    }
  }
}

function renderProductQueue() {
  elements.reviewProductTableBody.replaceChildren(
    ...state.productGroups.map(productQueueRow),
  );
  elements.emptyReviewProducts.classList.toggle(
    "is-hidden",
    state.productGroups.length > 0,
  );
  const summary = summarizeProductReviewGroups(state.productGroups);
  elements.reviewPendingCount.textContent = formatNumber(summary.total, 0);
  elements.reviewAspectCount.textContent = formatNumber(summary.readyLocal, 0);
  elements.reviewReasonCount.textContent = formatNumber(summary.needsSourceRecovery, 0);
  elements.reviewAttestationCount.textContent =
    formatNumber(summary.productsNeedingSourceCheck, 0);
  elements.reviewManualCount.textContent = formatNumber(summary.manualOrAmbiguous, 0);
}

function productQueueRow(group) {
  const row = document.createElement("tr");
  const product = group?.product || {};
  const productId = positiveInteger(product.id);
  const title = [product.manufacturer, product.model].filter(nonBlank).join(" ")
    || `Catalog product ${productId ?? "-"}`;
  const identity = product.stable_identifier;
  const productCell = document.createElement("td");
  productCell.dataset.label = "Product";
  const name = document.createElement("strong");
  name.textContent = title;
  productCell.append(name, renderAvionicsChips(product.capabilities || []));
  const identityCell = queueTextCell("Catalog identity", "Verified");
  identityCell.classList.add("review-status-current");
  const sourceCell = queueTextCell(
    "Reusable source",
    group?.attestation_status === "current"
      ? "Current"
      : "Source check required",
  );
  sourceCell.classList.add(
    group?.attestation_status === "current" ? "review-status-current" : "review-status-required",
  );
  const pendingCell = queueTextCell(
    "Pending",
    productGroupPendingSummary(group),
  );
  const actionCell = document.createElement("td");
  actionCell.dataset.label = "Actions";
  const button = document.createElement("button");
  button.type = "button";
  button.className = "button";
  button.dataset.reviewProductId = String(productId ?? "");
  button.disabled = productId === null || state.productBusy;
  button.textContent = group?.attestation_status === "current"
    ? "Validate associations"
    : "Review product";
  actionCell.append(button);
  row.append(
    productCell,
    identityCell,
    sourceCell,
    pendingCell,
    actionCell,
  );
  if (identity?.value) {
    name.title = `${displayLabel(identity.kind)}: ${identity.value}`;
  }
  return row;
}

function productGroupPendingSummary(group) {
  const summary = summarizeProductReviewGroups([group]);
  const breakdown = [
    summary.readyLocal ? `${summary.readyLocal} ready` : null,
    summary.needsSourceRecovery ? `${summary.needsSourceRecovery} need source text` : null,
    summary.productAttestationRequired
      ? `${summary.productAttestationRequired} ready after source check`
      : null,
    summary.manualOrAmbiguous ? `${summary.manualOrAmbiguous} manual` : null,
  ].filter(nonBlank).join(" · ");
  const listings = `${group?.pending_listing_count ?? 0} `
    + pluralize(group?.pending_listing_count ?? 0, "listing");
  return `${summary.total} across ${listings}${breakdown ? ` · ${breakdown}` : ""}`;
}

async function loadAllProductAssociations(productId, sequence) {
  const associations = [];
  let cursor = null;
  let product = null;
  let attestationStatus = null;
  let catalogRevision = null;
  do {
    if (sequence !== state.productDetailRequestSequence) {
      throw new Error("Product detail request was superseded.");
    }
    const params = new URLSearchParams({ limit: String(QUEUE_LIMIT) });
    if (cursor) {
      params.set("cursor", cursor);
    }
    const page = await api(
      `/api/review/avionics/products/${productId}/associations?${params}`,
    );
    if (sequence !== state.productDetailRequestSequence) {
      throw new Error("Product detail request was superseded.");
    }
    if (
      positiveInteger(page?.product?.id) !== productId
      || product !== null && JSON.stringify(page.product) !== JSON.stringify(product)
      || attestationStatus !== null && page.attestation_status !== attestationStatus
      || catalogRevision !== null && page.catalog_revision_sha256 !== catalogRevision
    ) {
      throw new Error(
        "The product, attestation, or catalog revision changed between association pages.",
      );
    }
    product = page.product;
    attestationStatus = page.attestation_status;
    catalogRevision = page.catalog_revision_sha256;
    associations.push(...(Array.isArray(page?.associations) ? page.associations : []));
    cursor = nonBlank(page?.next_cursor) ? page.next_cursor : null;
  } while (cursor);
  return { product, attestationStatus, catalogRevision, associations };
}

async function openProductReview(productId) {
  if (state.productBusy && state.productBusyProductId !== productId) {
    return { status: "busy" };
  }
  const sequence = ++state.productDetailRequestSequence;
  elements.reviewProductWorkspace.classList.remove("is-hidden");
  elements.reviewProductActionMessage.textContent = "Loading product associations…";
  try {
    const detail = await loadAllProductAssociations(productId, sequence);
    if (!productDetailRequestMayCommit(productId, sequence, state)) {
      return { status: "superseded" };
    }
    state.selectedProduct = {
      id: productId,
      product: detail.product,
      attestationStatus: detail.attestationStatus,
      catalogRevision: detail.catalogRevision,
    };
    state.productAssociations = detail.associations;
    renderSelectedProduct();
    return { status: "loaded" };
  } catch (error) {
    if (!productDetailRequestMayCommit(productId, sequence, state)) {
      return { status: "superseded" };
    }
    if (error?.status === 404) {
      if (state.selectedProduct?.id === productId) {
        state.selectedProduct = null;
        state.productAssociations = [];
      }
      elements.reviewProductWorkspace.classList.add("is-hidden");
      elements.reviewProductActionMessage.textContent = "";
      return { status: "absent" };
    }
    elements.reviewProductActionMessage.textContent =
      `Could not load product review: ${error.message}`;
    return { status: "error", error };
  }
}

function renderSelectedProduct() {
  const selected = state.selectedProduct;
  if (!selected) {
    elements.reviewProductWorkspace.classList.add("is-hidden");
    return;
  }
  const product = selected.product || {};
  elements.reviewProductTitle.textContent =
    [product.manufacturer, product.model].filter(nonBlank).join(" ")
      || `Catalog product ${selected.id}`;
  elements.reviewProductSummary.textContent = [
    `Catalog ID ${selected.id}`,
    "Catalog identity verified",
    product.stable_identifier?.value,
    `${state.productAssociations.length} pending `
      + pluralize(state.productAssociations.length, "association"),
  ].filter(nonBlank).join(" · ");
  const current = selected.attestationStatus === "current";
  elements.reviewProductStatus.textContent = current
    ? "Reusable source current"
    : "Reusable source check required";
  elements.reviewProductStatus.classList.toggle("is-current", current);
  elements.reviewProductAttestationForm.classList.toggle("is-hidden", current);
  const autoVerifiable = autoVerifiableProductAssociations(
    state.productAssociations,
    selected.attestationStatus,
  );
  const summary = summarizeProductAssociations(
    state.productAssociations,
    selected.attestationStatus,
  );
  elements.reviewProductTotalCount.textContent = formatNumber(summary.total, 0);
  elements.reviewProductReadyCount.textContent = formatNumber(summary.readyLocal, 0);
  elements.reviewProductRecoveryCount.textContent =
    formatNumber(summary.needsSourceRecovery, 0);
  elements.reviewProductAttestationCount.textContent =
    formatNumber(summary.productAttestationRequired, 0);
  elements.reviewProductManualCount.textContent =
    formatNumber(summary.manualOrAmbiguous, 0);
  elements.reviewProductValidate.disabled = !current
    || autoVerifiable.length === 0
    || state.productBusy;
  elements.reviewProductRecover.disabled = !current
    || summary.needsSourceRecovery === 0
    || state.productBusy;
  if (!current) {
    const form = elements.reviewProductAttestationForm.elements;
    const draft = productAttestationDraft(product);
    form.namedItem("identity_source_url").value = draft.sourceUrl;
    form.namedItem("identity_source_title").value = draft.sourceTitle;
    form.namedItem("identity_evidence_text").value = draft.evidenceText;
    form.namedItem("identity_source_title").dispatchEvent(new Event("input"));
    form.namedItem("identity_evidence_text").dispatchEvent(new Event("input"));
  }
  renderProductAssociationRows();
  elements.reviewProductActionMessage.textContent = current
    ? productAssociationActionSummary(summary)
    : "The catalog identity is verified. Complete one reusable manufacturer-source check before validating ready associations.";
}

function productAssociationActionSummary(summary) {
  return [
    `${summary.total} pending`,
    `${summary.readyLocal} ready locally`,
    `${summary.needsSourceRecovery} need source recovery`,
    `${summary.manualOrAmbiguous} manual or ambiguous`,
  ].join(" · ");
}

function associationOutcomeKey(association, productId = state.selectedProduct?.id) {
  return `${productId}:${association?.listing_id}:${aspectKey(association?.aspect_id)}`;
}

function renderProductAssociationRows() {
  elements.reviewProductAssociationBody.replaceChildren(
    ...state.productAssociations.map((association) => {
      const row = document.createElement("tr");
      const listing = document.createElement("td");
      listing.dataset.label = "Listing";
      const label = document.createElement("strong");
      label.textContent = association.listing_label || `Listing ${association.listing_id}`;
      listing.append(label);
      const source = safeDetailLink(association.source_url, "Open listing");
      if (source) {
        source.className = "review-association-source";
        listing.append(source);
      }
      const evidenceDisplay = productAssociationEvidenceDisplay(association);
      const observed = queueTextCell(
        "Observed text",
        evidenceDisplay.observedText,
      );
      const retainedEvidence = queueTextCell(
        "Retained source evidence",
        evidenceDisplay.sourceEvidenceText,
      );
      const eligibilityOutcome = productAssociationEligibilityOutcomeForAttestation(
        association,
        state.selectedProduct?.attestationStatus,
      );
      const outcome = state.productOutcomes.get(associationOutcomeKey(association))
        || eligibilityOutcome;
      const result = queueTextCell(
        "Result",
        outcome.label,
      );
      result.title = outcome.detail;
      result.classList.add(`review-outcome-${outcome.kind}`);
      row.append(listing, observed, retainedEvidence, result);
      return row;
    }),
  );
}

async function attestSelectedProduct(event) {
  event.preventDefault();
  const selected = state.selectedProduct;
  if (!selected || selected.attestationStatus === "current" || state.productBusy) {
    return;
  }
  const authorization = state.productAssociations.find((association) => (
    positiveInteger(association?.listing_id) !== null
    && nonBlank(association?.review_payload_sha256)
    && aspectKey(association?.aspect_id)
  ));
  if (!authorization) {
    elements.reviewProductActionMessage.textContent =
      "Reload this product to obtain a current pending association authorization.";
    return;
  }
  const form = elements.reviewProductAttestationForm.elements;
  const sourceUrl = form.namedItem("identity_source_url").value.trim();
  const sourceTitle = form.namedItem("identity_source_title").value.trim();
  const evidenceText = form.namedItem("identity_evidence_text").value.trim();
  const validation = reviewProductIdentitySourceValidation(sourceTitle, evidenceText);
  if (!authoritativeIdentityUrl(sourceUrl) || !validation.valid) {
    elements.reviewProductActionMessage.textContent = !authoritativeIdentityUrl(sourceUrl)
      ? "Provide an authoritative HTTPS OEM source URL."
      : validation.message;
    return;
  }
  const action = beginProductAction(selected);
  elements.reviewProductActionMessage.textContent =
    "Fetching the OEM source and checking the immutable product identity…";
  try {
    const result = await api(
      `/api/review/avionics/products/${selected.id}/attest`,
      {
        method: "POST",
        body: JSON.stringify({
          listing_id: authorization.listing_id,
          review_payload_sha256: authorization.review_payload_sha256,
          aspect_id: authorization.aspect_id,
          catalog_revision_sha256: selected.catalogRevision,
          identity_source_url: sourceUrl,
          identity_source_title: sourceTitle,
          identity_evidence_text: evidenceText,
        }),
      },
    );
    if (!productActionContextIsCurrent(action, state)) {
      return;
    }
    state.selectedProduct.attestationStatus = "current";
    elements.reviewProductActionMessage.textContent = result?.reused
      ? "The reusable product source was already current; no source fetch was needed."
      : "Reusable product source verified from the guarded manufacturer source without Gemini.";
    await loadProductQueue({
      quiet: true,
      commitGuard: () => productActionContextIsCurrent(action, state),
    });
    if (!productActionContextIsCurrent(action, state)) {
      return;
    }
    await openProductReview(action.productId);
  } catch (error) {
    if (!productActionContextIsCurrent(action, state)) {
      return;
    }
    const outcome = describeProductAssociationOutcome(error);
    elements.reviewProductActionMessage.textContent =
      `${outcome.label}: ${outcome.detail}`;
  } finally {
    finishProductAction(action);
  }
}

async function validateSelectedProductAssociations() {
  const selected = state.selectedProduct;
  if (!selected || selected.attestationStatus !== "current" || state.productBusy) {
    return;
  }
  const initialAssociations = autoVerifiableProductAssociations(
    state.productAssociations,
    selected.attestationStatus,
  );
  const manualAssociationsBeforeRun = state.productAssociations.length
    - initialAssociations.length;
  if (initialAssociations.length === 0) {
    elements.reviewProductActionMessage.textContent =
      "No pending associations currently pass the complete local preflight.";
    return;
  }
  const action = beginProductAction(selected);
  elements.reviewProductActionMessage.textContent =
    `Validating ${initialAssociations.length} associations locally…`;
  try {
    const results = await runProductAssociationWorkers(
      initialAssociations,
      async (listingId, associations) => {
        let pending = associations.slice();
        const failedAspectKeys = new Set();
        let accepted = 0;
        let failures = 0;
        let current = pending[0];
        while (current && productActionContextIsCurrent(action, state)) {
          try {
            const response = await api(
              `/api/review/listings/${listingId}/avionics/verify-existing`,
              {
                method: "POST",
                body: JSON.stringify(existingProductVerificationRequest(
                  current.review_payload_sha256,
                  selected.catalogRevision,
                  current.aspect_id,
                )),
              },
            );
            if (!productActionContextIsCurrent(action, state)) {
              return { accepted, failures, superseded: true };
            }
            state.productOutcomes.set(associationOutcomeKey(current, action.productId), {
              kind: "accepted",
              label: "Accepted locally",
              detail: "The retained listing text uniquely matched the attested product.",
            });
            accepted += 1;
            renderProductAssociationRows();
            const review = response?.review;
            const initialByAspect = new Map(
              associations.map((association) => [
                aspectKey(association.aspect_id),
                association,
              ]),
            );
            pending = Array.isArray(review?.aspects)
              ? review.aspects
                .filter(
                  (aspect) => (
                    positiveInteger(aspect?.reuse_attestation_target?.id) === action.productId
                    && initialByAspect.has(aspectKey(aspect.id))
                  ),
                )
                .map((aspect) => {
                  const initial = initialByAspect.get(aspectKey(aspect.id));
                  return {
                    ...initial,
                    review_payload_sha256: review.review_payload_sha256,
                    observed_text: aspect.observed_text,
                    source_evidence_text: aspect.source_evidence_text,
                  };
                })
              : [];
          } catch (error) {
            if (!productActionContextIsCurrent(action, state)) {
              return { accepted, failures, superseded: true };
            }
            failedAspectKeys.add(aspectKey(current.aspect_id));
            failures += 1;
            state.productOutcomes.set(
              associationOutcomeKey(current, action.productId),
              describeProductAssociationOutcome(error),
            );
            renderProductAssociationRows();
            pending = pending.filter(
              (association) => aspectKey(association.aspect_id) !== aspectKey(current.aspect_id),
            );
          }
          current = pending.find(
            (association) => !failedAspectKeys.has(aspectKey(association.aspect_id)),
          ) || null;
        }
        return {
          accepted,
          failures,
          superseded: !productActionContextIsCurrent(action, state),
        };
      },
      4,
    );
    if (!productActionContextIsCurrent(action, state)) {
      return;
    }
    const accepted = results
      .filter((result) => result.status === "fulfilled")
      .reduce((sum, result) => sum + (result.value?.accepted || 0), 0);
    const failedAssociations = results.reduce(
      (sum, result) => sum + (
        result.status === "fulfilled" ? result.value?.failures || 0 : 1
      ),
      0,
    );
    const manualAssociations = manualAssociationsBeforeRun + failedAssociations;
    const manualListingIds = new Set(
      state.productAssociations
        .filter((association) => (
          association?.verification_eligibility?.status !== "auto_verifiable"
        ))
        .map((association) => association.listing_id),
    );
    for (const result of results) {
      if (result.status === "rejected" || (result.value?.failures || 0) > 0) {
        manualListingIds.add(result.listingId);
      }
    }
    const manualListings = manualListingIds.size;
    elements.reviewProductActionMessage.textContent =
      `${accepted} accepted locally; ${manualAssociations} `
        + `${pluralize(manualAssociations, "association")} across ${manualListings} `
        + `${pluralize(manualListings, "listing")} need manual review or refresh.`;
    await loadProductQueue({
      quiet: true,
      commitGuard: () => productActionContextIsCurrent(action, state),
    });
    if (!productActionContextIsCurrent(action, state)) {
      return;
    }
    await openProductReview(action.productId);
  } finally {
    finishProductAction(action);
  }
}

async function recoverSelectedProductEvidence() {
  const selected = state.selectedProduct;
  if (!selected || selected.attestationStatus !== "current" || state.productBusy) {
    return;
  }
  const recoverable = associationsNeedingSourceRecovery(
    state.productAssociations,
    selected.attestationStatus,
  );
  if (recoverable.length === 0) {
    elements.reviewProductActionMessage.textContent =
      "No pending association currently needs listing source recovery.";
    return;
  }
  const listingCount = new Set(recoverable.map((association) => association.listing_id)).size;
  const before = summarizeProductAssociations(
    state.productAssociations,
    selected.attestationStatus,
  );
  const action = beginProductAction(selected);
  elements.reviewProductActionMessage.textContent =
    `Recovering exact source text from ${listingCount} `
      + `${pluralize(listingCount, "listing")}…`;
  try {
    const results = await runProductAssociationWorkers(
      recoverable,
      async (listingId) => api(`/api/review/listings/${listingId}/restage`, {
        method: "POST",
      }),
      4,
    );
    if (!productActionContextIsCurrent(action, state)) {
      return;
    }
    const succeeded = results.filter((result) => result.status === "fulfilled").length;
    const failed = results.length - succeeded;
    await loadProductQueue({
      quiet: true,
      commitGuard: () => productActionContextIsCurrent(action, state),
    });
    if (!productActionContextIsCurrent(action, state)) {
      return;
    }
    const detailResult = await openProductReview(action.productId);
    if (detailResult.status !== "loaded" || state.selectedProduct?.id !== action.productId) {
      return;
    }
    const after = summarizeProductAssociations(
      state.productAssociations,
      state.selectedProduct.attestationStatus,
    );
    const newlyReady = Math.max(0, after.readyLocal - before.readyLocal);
    const stillNeedsRecovery = after.needsSourceRecovery;
    elements.reviewProductActionMessage.textContent = [
      `${succeeded} ${pluralize(succeeded, "listing")} checked`,
      `${newlyReady} ${pluralize(newlyReady, "association")} newly ready`,
      `${stillNeedsRecovery} still need source recovery`,
      failed ? `${failed} ${pluralize(failed, "listing")} could not be refreshed` : null,
    ].filter(nonBlank).join(" · ");
  } finally {
    finishProductAction(action);
  }
}

function beginProductAction(selected) {
  const action = {
    productId: selected.id,
    detailSequence: state.productDetailRequestSequence,
    actionSequence: state.productActionSequence + 1,
  };
  state.productActionSequence = action.actionSequence;
  state.productBusyProductId = action.productId;
  setProductBusy(true);
  return action;
}

function finishProductAction(action) {
  if (
    state.productActionSequence !== action.actionSequence
    || state.productBusyProductId !== action.productId
  ) {
    return;
  }
  state.productBusyProductId = null;
  setProductBusy(false);
}

function setProductBusy(busy) {
  state.productBusy = busy;
  setButtonBusy(elements.reviewProductAttest, busy);
  setButtonBusy(elements.reviewProductRecover, busy);
  setButtonBusy(elements.reviewProductValidate, busy);
  const summary = summarizeProductAssociations(
    state.productAssociations,
    state.selectedProduct?.attestationStatus,
  );
  elements.reviewProductRecover.disabled =
    busy
    || state.selectedProduct?.attestationStatus !== "current"
    || summary.needsSourceRecovery === 0;
  elements.reviewProductValidate.disabled =
    busy
    || state.selectedProduct?.attestationStatus !== "current"
    || autoVerifiableProductAssociations(
      state.productAssociations,
      state.selectedProduct?.attestationStatus,
    ).length === 0;
  renderProductQueue();
}

async function loadQueue({ quiet = false } = {}) {
  const sequence = ++state.queueRequestSequence;
  if (!quiet) {
    setQueueMessage("Loading review queue…");
  }
  elements.reviewResults.setAttribute("aria-busy", "true");
  setButtonBusy(elements.refreshReviews, true);
  try {
    const payload = await api(`/api/review/listings?limit=${QUEUE_LIMIT}&offset=0`);
    if (sequence !== state.queueRequestSequence) {
      return false;
    }
    const reviews = Array.isArray(payload?.reviews) ? payload.reviews.slice() : [];
    const total = nonNegativeInteger(payload?.total) ?? reviews.length;
    while (reviews.length < total) {
      const page = await api(
        `/api/review/listings?limit=${QUEUE_LIMIT}&offset=${reviews.length}`,
      );
      if (sequence !== state.queueRequestSequence) {
        return false;
      }
      const items = Array.isArray(page?.reviews) ? page.reviews : [];
      if (!items.length) {
        break;
      }
      const known = new Set(reviews.map((item) => positiveInteger(item?.listing_id)));
      const additions = items.filter(
        (item) => !known.has(positiveInteger(item?.listing_id)),
      );
      if (!additions.length) {
        break;
      }
      reviews.push(...additions);
    }
    state.reviews = reviews;
    state.total = total;
    state.limit = positiveInteger(payload?.limit) ?? QUEUE_LIMIT;
    state.offset = nonNegativeInteger(payload?.offset) ?? 0;
    state.queueLoaded = true;
    renderQueue();
    if (!quiet) {
      const suffix = state.total > state.reviews.length
        ? ` Showing ${state.reviews.length} of ${state.total}.`
        : "";
      setQueueMessage(`${state.total} ${pluralize(state.total, "listing")} pending.${suffix}`);
    }
    return true;
  } catch (error) {
    if (sequence === state.queueRequestSequence) {
      setQueueMessage(`Could not load review queue: ${error.message}`, true);
    }
    return false;
  } finally {
    if (sequence === state.queueRequestSequence) {
      elements.reviewResults.setAttribute("aria-busy", "false");
      setButtonBusy(elements.refreshReviews, false);
    }
  }
}

function renderQueue() {
  elements.reviewTableBody.replaceChildren(...state.reviews.map(reviewQueueRow));
  elements.emptyReviews.classList.toggle("is-hidden", state.reviews.length > 0);
  elements.reviewPendingCount.textContent = formatNumber(state.total, 0);
  const aspectCount = state.reviews.reduce(
    (sum, item) => sum + (nonNegativeInteger(item?.pending_aspect_count) ?? 0),
    0,
  );
  const reasons = new Set(
    state.reviews.flatMap((item) => (
      describeReviewReasons(item?.reason_codes).map((reason) => reason.label)
    )),
  );
  elements.reviewAspectCount.textContent = formatNumber(aspectCount, 0);
  elements.reviewReasonCount.textContent = formatNumber(reasons.size, 0);
  updateNextButton();
}

function reviewQueueRow(item) {
  const row = document.createElement("tr");
  const aircraft = item?.aircraft || {};
  const identity = [aircraft.manufacturer, aircraft.variant || aircraft.model]
    .filter(nonBlank)
    .join(" ") || item?.label || `Listing ${item?.listing_id ?? "-"}`;

  const aircraftCell = document.createElement("td");
  aircraftCell.dataset.label = "Aircraft";
  aircraftCell.className = "aircraft-cell";
  const aircraftName = document.createElement("strong");
  aircraftName.textContent = identity;
  aircraftCell.append(aircraftName);

  const pendingCell = document.createElement("td");
  pendingCell.dataset.label = "Checks";
  const pendingCount = nonNegativeInteger(item?.pending_aspect_count) ?? 0;
  const pendingPill = document.createElement("span");
  pendingPill.className = "review-count-pill";
  pendingPill.textContent = `${pendingCount} ${pluralize(pendingCount, "check")}`;
  pendingCell.append(pendingPill);

  const reasonsCell = document.createElement("td");
  reasonsCell.dataset.label = "Why review is needed";
  reasonsCell.append(reviewReasonChips(item?.reason_codes));

  const actionCell = document.createElement("td");
  actionCell.dataset.label = "Actions";
  const inspect = document.createElement("button");
  inspect.type = "button";
  inspect.className = "button review-open-button";
  inspect.textContent = "Review";
  inspect.dataset.reviewListingId = String(item?.listing_id ?? "");
  inspect.disabled = positiveInteger(item?.listing_id) === null;
  inspect.setAttribute("aria-label", `Review ${identity}`);
  actionCell.append(inspect);

  row.append(
    aircraftCell,
    queueTextCell("Tail", item?.registration_number || "-"),
    queueTextCell("Year", item?.model_year ?? "-"),
    pendingCell,
    reasonsCell,
    queueTextCell("Updated", formatDate(item?.updated_at)),
    actionCell,
  );
  return row;
}

function queueTextCell(label, value) {
  const cell = document.createElement("td");
  cell.dataset.label = label;
  cell.textContent = value === null || value === undefined || value === "" ? "-" : String(value);
  return cell;
}

function reviewReasonChips(values) {
  const container = document.createElement("div");
  container.className = "review-reason-list";
  const reasons = describeReviewReasons(values);
  for (const reason of reasons.slice(0, 3)) {
    const chip = document.createElement("span");
    chip.className = "review-reason-chip";
    chip.textContent = reason.label;
    chip.title = reason.detail;
    container.append(chip);
  }
  if (reasons.length > 3) {
    const overflow = document.createElement("span");
    overflow.className = "review-reason-chip overflow";
    overflow.textContent = `+${reasons.length - 3}`;
    overflow.title = reasons.slice(3).map((reason) => reason.label).join(", ");
    container.append(overflow);
  }
  if (!reasons.length) {
    container.textContent = "Pending verification";
  }
  return container;
}

async function openReview(
  listingId,
  { historyMode = "push", discardDraft = false, force = false } = {},
) {
  if (!discardDraft && !confirmDiscardDraft()) {
    return;
  }
  if (
    !force
    && currentListingId() === listingId
    && state.currentReview
    && !state.stale
  ) {
    showWorkspace();
    updateReviewLocation(listingId, historyMode);
    return;
  }

  state.activeArea = historyMode === "none"
    ? reviewAreaFromLocation() ?? "avionics"
    : "avionics";
  cancelCatalogSearches();
  state.currentReview = null;
  state.drafts.clear();
  state.aspectViews.clear();
  state.stale = false;
  state.resolving = false;
  showWorkspace();
  updateReviewLocation(listingId, historyMode);
  setWorkspaceLoading(listingId);

  const sequence = ++state.detailRequestSequence;
  try {
    const payload = await api(`/api/review/listings/${listingId}/restage`, {
      method: "POST",
    });
    if (sequence !== state.detailRequestSequence) {
      return;
    }
    const review = payload?.review;
    if (!isReviewDetail(review, listingId)) {
      throw new Error("The server returned an invalid listing review.");
    }
    state.currentReview = review;
    initializeDrafts(review);
    renderReview();
  } catch (error) {
    if (sequence !== state.detailRequestSequence) {
      return;
    }
    const stale = isStaleError(error);
    if (stale) {
      markStale(error.message);
    } else {
      renderReviewLoadError(listingId, error);
    }
  }
}

function isReviewDetail(review, listingId) {
  if (
    !review
    || typeof review !== "object"
    || positiveInteger(review.listing_id) !== listingId
    || !Array.isArray(review.allowed_capabilities)
    || !Array.isArray(review.aspects)
    || review.aspects.length === 0
    || !isAircraftIdentityStatus(review.aircraft_identity)
    || !nonBlank(review.review_payload_sha256)
    || !nonBlank(review.catalog_revision_sha256)
  ) {
    return false;
  }
  const keys = review.aspects.map((aspect) => aspectKey(aspect?.id));
  return keys.every(nonBlank)
    && new Set(keys).size === keys.length
    && review.aspects.every((aspect) => (
      reviewAreaForAspect(aspect) === "avionics"
      && Array.isArray(aspect.allowed_actions)
      && allowedActions(aspect).length > 0
    ));
}

function initializeDrafts(review) {
  for (const aspect of review.aspects) {
    const key = aspectKey(aspect.id);
    const sourceProduct = aspect.reuse_attestation_target ?? aspect.proposed_product;
    const proposed = normalizedProduct(sourceProduct);
    state.drafts.set(key, {
      aspect,
      // This is only a local draft default. Resolution still requires the
      // reviewer to submit the complete decision set to the server.
      action: preselectedReviewAction(aspect),
      catalogProduct: normalizedProduct(aspect.suggested_product),
      create: {
        manufacturer: proposed?.manufacturer || "",
        model: proposed?.model || "",
        capabilities: proposed?.capabilities || [],
        manufacturerIdentifierKind: optionalText(
          sourceProduct?.stable_identifier?.kind
            ?? sourceProduct?.manufacturer_identifier_kind,
        ),
        manufacturerIdentifier: optionalText(
          sourceProduct?.stable_identifier?.value
            ?? sourceProduct?.manufacturer_identifier,
        ),
        identitySourceUrl: optionalText(sourceProduct?.identity_source_url),
        identitySourceTitle: optionalText(sourceProduct?.identity_source_title),
        identityEvidenceText: optionalText(sourceProduct?.identity_evidence_text),
      },
      discardReason: "",
    });
  }
}

function renderReview() {
  const review = state.currentReview;
  if (!review) {
    return;
  }
  elements.reviewWorkspaceTitle.textContent = review.label || `Listing ${review.listing_id}`;
  elements.reviewWorkspaceSubtitle.textContent = [
    `Listing ${review.listing_id}`,
    `${review.aspects.length} pending ${pluralize(review.aspects.length, "avionics check")}`,
    ...(!aircraftIdentityIsVerified(review.aircraft_identity)
      ? ["Aircraft curation required"]
      : []),
  ].join(" · ");
  renderSource(review);
  renderAircraftSummary(review);
  state.aspectViews.clear();
  const avionicsAspects = review.aspects.filter(
    (aspect) => reviewAreaForAspect(aspect) === "avionics",
  );
  renderListingReasons(avionicsAspects);
  elements.reviewAvionicsAspects.replaceChildren(
    ...avionicsAspects.map(
      (aspect, index) => renderAspect(aspect, index, avionicsAspects.length),
    ),
  );
  elements.reviewAvionicsAspects.setAttribute("aria-busy", "false");
  const aircraftBlockerCount = aircraftIdentityIsVerified(review.aircraft_identity) ? 0 : 1;
  elements.reviewAircraftTabCount.textContent = String(aircraftBlockerCount);
  elements.reviewAvionicsTabCount.textContent = String(avionicsAspects.length);
  const requestedArea = reviewAreaFromLocation();
  setActiveReviewArea(
    requestedArea ?? (aircraftBlockerCount ? "aircraft" : "avionics"),
    { updateLocation: requestedArea === null },
  );
  elements.reviewStale.classList.add("is-hidden");
  setWorkspaceMessage("");
  updateProgress();
  updateNextButton();
  synchronizeOpenedListingAutomationBusy();
}

function renderSource(review) {
  elements.reviewSourceLabel.textContent = review.source_url || "No source recorded";
  const link = safeDetailLink(review.source_url, "Open source");
  if (link) {
    elements.reviewSourceLink.href = link.href;
    elements.reviewSourceLink.classList.remove("is-hidden");
  } else {
    elements.reviewSourceLink.removeAttribute("href");
    elements.reviewSourceLink.classList.add("is-hidden");
  }
}

function reviewAreaElements(area) {
  if (area === "aircraft") {
    return {
      tab: elements.reviewAircraftTab,
      count: elements.reviewAircraftTabCount,
      panel: elements.reviewAircraftPanel,
    };
  }
  return {
    tab: elements.reviewAvionicsTab,
    count: elements.reviewAvionicsTabCount,
    panel: elements.reviewAvionicsPanel,
  };
}

function setActiveReviewArea(area, { focus = false, updateLocation = false } = {}) {
  const selectedArea = REVIEW_AREAS.includes(area) ? area : "avionics";
  state.activeArea = selectedArea;
  for (const candidate of REVIEW_AREAS) {
    const { tab, panel } = reviewAreaElements(candidate);
    const selected = candidate === selectedArea;
    tab.setAttribute("aria-selected", String(selected));
    tab.tabIndex = selected ? 0 : -1;
    panel.hidden = !selected;
  }
  if (focus) {
    reviewAreaElements(selectedArea).tab.focus();
  }
  if (updateLocation && currentListingId() !== null) {
    updateReviewLocation(currentListingId(), "replace");
  }
}

function handleReviewTabKeydown(event, area) {
  const currentIndex = REVIEW_AREAS.indexOf(area);
  let nextIndex = null;
  if (event.key === "ArrowLeft") {
    nextIndex = (currentIndex - 1 + REVIEW_AREAS.length) % REVIEW_AREAS.length;
  } else if (event.key === "ArrowRight") {
    nextIndex = (currentIndex + 1) % REVIEW_AREAS.length;
  } else if (event.key === "Home") {
    nextIndex = 0;
  } else if (event.key === "End") {
    nextIndex = REVIEW_AREAS.length - 1;
  }
  if (nextIndex === null) {
    return;
  }
  event.preventDefault();
  setActiveReviewArea(REVIEW_AREAS[nextIndex], {
    focus: true,
    updateLocation: true,
  });
}

function renderAircraftSummary(review) {
  const aircraft = review.aircraft || {};
  const identity = review.aircraft_identity;
  const identityDescription = describeAircraftIdentity(identity);
  const identityVerified = aircraftIdentityIsVerified(identity);
  const card = document.createElement("article");
  card.className = "review-aircraft-card";
  const header = document.createElement("header");
  const heading = document.createElement("div");
  const eyebrow = document.createElement("span");
  eyebrow.className = "review-eyebrow";
  eyebrow.textContent = "FAA-backed aircraft identity";
  const title = document.createElement("h3");
  title.textContent = review.label || `Listing ${review.listing_id}`;
  heading.append(eyebrow, title);
  const status = document.createElement("span");
  status.className = `review-decision-status ${identityVerified ? "decided" : "pending"}`;
  status.textContent = identityDescription.label;
  header.append(heading, status);

  const metadata = document.createElement("dl");
  metadata.className = "review-aircraft-metadata";
  metadata.append(
    reviewMetadataItem("Registration", review.registration_number || "Not recorded"),
    reviewMetadataItem("Model year", review.model_year ?? "Not recorded"),
    reviewMetadataItem("Manufacturer", aircraft.manufacturer || "Not recorded"),
    reviewMetadataItem("Model", aircraft.model || "Not recorded"),
    reviewMetadataItem("Variant", aircraft.variant || "Not recorded"),
    reviewMetadataItem("FAA N-number", identity.faa_n_number || "Not verified"),
    reviewMetadataItem("FAA snapshot", identity.faa_snapshot_id ?? "Not verified"),
  );
  const notice = document.createElement("p");
  notice.className = "review-aircraft-notice";
  notice.textContent = identityDescription.detail;
  card.append(header, metadata, notice);
  elements.reviewAircraftSummary.replaceChildren(card);
}

function renderListingReasons(aspects) {
  const reasons = [];
  const seen = new Set();
  for (const aspect of aspects) {
    for (const reason of describeReviewReasons(aspect.reason)) {
      if (!reason.isListingLevel || seen.has(reason.label)) {
        continue;
      }
      seen.add(reason.label);
      reasons.push(reason);
    }
  }
  if (!reasons.length) {
    elements.reviewAvionicsReasons.replaceChildren();
    return;
  }
  const notice = document.createElement("section");
  notice.className = "review-listing-reason";
  const title = document.createElement("strong");
  title.textContent = reasons.length === 1
    ? reasons[0].label
    : "Listing-wide equipment issues need confirmation";
  const list = document.createElement("ul");
  for (const reason of reasons) {
    const item = document.createElement("li");
    item.textContent = reason.detail;
    list.append(item);
  }
  notice.append(title, list);
  elements.reviewAvionicsReasons.replaceChildren(notice);
}

function renderAspect(aspect, index, total) {
  const key = aspectKey(aspect.id);
  const draft = state.drafts.get(key);
  const article = document.createElement("article");
  article.className = "review-aspect-card";
  article.dataset.aspectId = key;

  const header = document.createElement("header");
  header.className = "review-aspect-header";
  const headingGroup = document.createElement("div");
  const eyebrow = document.createElement("span");
  eyebrow.className = "review-eyebrow";
  eyebrow.textContent = [
    displayLabel(aspect.kind || "aspect"),
    displayLabel(aspect.configuration_action || "installed"),
    `${index + 1} of ${total}`,
  ].join(" · ");
  const title = document.createElement("h3");
  title.textContent = aspect.label || `Aspect ${index + 1}`;
  headingGroup.append(eyebrow, title);
  const status = document.createElement("span");
  status.className = "review-decision-status pending";
  status.textContent = "Needs decision";
  header.append(headingGroup, status);

  const context = document.createElement("div");
  context.className = "review-aspect-context";
  const reviewReasons = describeReviewReasons(aspect.reason).filter(
    (reason) => !reason.isListingLevel,
  );
  if (reviewReasons.length) {
    const reason = document.createElement("div");
    reason.className = "review-aspect-reason";
    const reasonTitle = document.createElement("strong");
    reasonTitle.textContent = "Why this needs review";
    const reasonList = document.createElement("ul");
    for (const item of reviewReasons) {
      const listItem = document.createElement("li");
      const label = document.createElement("strong");
      label.textContent = item.label;
      const detail = document.createElement("span");
      detail.textContent = item.detail;
      listItem.append(label, detail);
      reasonList.append(listItem);
    }
    reason.append(reasonTitle, reasonList);
    context.append(reason);
  }
  const observation = document.createElement("div");
  observation.className = "review-observation";
  const observationLabel = document.createElement("span");
  observationLabel.className = "review-eyebrow";
  observationLabel.textContent = "Observed in listing";
  const observationText = document.createElement("p");
  observationText.textContent = aspect.observed_text || "No source observation recorded.";
  const observationMetadata = document.createElement("dl");
  observationMetadata.className = "review-observation-metadata";
  observationMetadata.append(
    reviewMetadataItem("Quantity", reviewQuantity(aspect.quantity)),
    reviewMetadataItem(
      "Source confidence",
      nonBlank(aspect.source_confidence)
        ? displayLabel(aspect.source_confidence)
        : "Not recorded",
    ),
  );
  const evidence = document.createElement("p");
  evidence.className = "review-source-evidence";
  if (nonBlank(aspect.source_evidence_text)) {
    evidence.append(
      strongText("Source evidence: "),
      document.createTextNode(aspect.source_evidence_text),
    );
  } else {
    evidence.classList.add("empty");
    evidence.textContent = "No retained source evidence was recorded.";
  }
  observation.append(observationLabel, observationText, observationMetadata, evidence);
  context.append(observation);

  if (aspect.suggested_product) {
    const suggestion = productSummary(aspect.suggested_product, "Suggested verified match");
    suggestion.classList.add("review-suggested-product");
    context.append(suggestion);
  }
  if (aspect.reuse_attestation_target) {
    const target = productSummary(
      aspect.reuse_attestation_target,
      "Catalog identity verified",
    );
    target.classList.add("review-suggested-product");
    context.append(target);
    const sourceCurrent = listingAssociationCanValidateLocally(aspect);
    const sourceStatus = document.createElement("p");
    sourceStatus.className = `review-catalog-message ${sourceCurrent ? "review-status-current" : "review-status-required"}`;
    sourceStatus.textContent = sourceCurrent
      ? "Reusable manufacturer source is current."
      : "Reusable manufacturer source check is still required in By product.";
    const associationAction = document.createElement("button");
    associationAction.type = "button";
    associationAction.className = "button";
    associationAction.textContent = "Validate this listing association locally";
    associationAction.disabled = !sourceCurrent;
    associationAction.addEventListener("click", () => {
      validateExistingAssociation(key, associationAction);
    });
    const associationHint = document.createElement("p");
    associationHint.className = "review-catalog-message";
    associationHint.textContent = sourceCurrent
      ? "This operation uses only retained listing text and the verified local catalog."
      : "Local association validation stays unavailable until the product source check is complete.";
    context.append(sourceStatus, associationAction, associationHint);
  }
  const suggestedId = positiveInteger(aspect.suggested_product?.id);
  const candidateId = positiveInteger(aspect.proposed_product?.id);
  if (candidateId !== null && candidateId !== suggestedId) {
    const candidate = productSummary(
      aspect.proposed_product,
      "Existing unreviewed catalog candidate",
    );
    candidate.classList.add("review-suggested-product");
    context.append(candidate);
  }
  if (aspect.replacement_product) {
    context.append(productSummary(
      aspect.replacement_product,
      replacementProductHeading(aspect.configuration_action),
    ));
  } else if (aspect.replacement_aspect_id !== null && aspect.replacement_aspect_id !== undefined) {
    const target = state.currentReview.aspects.find(
      (candidate) => aspectKey(candidate.id) === aspectKey(aspect.replacement_aspect_id),
    );
    const relationship = document.createElement("div");
    relationship.className = "review-product-summary";
    const relationshipLabel = document.createElement("span");
    relationshipLabel.className = "review-eyebrow";
    relationshipLabel.textContent = "Replacement target observation";
    const relationshipTitle = document.createElement("strong");
    relationshipTitle.textContent = target?.label || `Aspect ${aspectKey(aspect.replacement_aspect_id)}`;
    const relationshipMetadata = document.createElement("span");
    relationshipMetadata.textContent = target
      ? [
        `Aspect ${aspectKey(target.id)}`,
        `Quantity ${reviewQuantity(target.quantity)}`,
      ].join(" · ")
      : `Aspect ${aspectKey(aspect.replacement_aspect_id)}`;
    relationship.append(relationshipLabel, relationshipTitle, relationshipMetadata);
    if (nonBlank(target?.observed_text)) {
      const relationshipObservation = document.createElement("p");
      relationshipObservation.className = "review-related-observation";
      relationshipObservation.textContent = target.observed_text;
      relationship.append(relationshipObservation);
    }
    context.append(relationship);
  }

  const decision = document.createElement("fieldset");
  decision.className = "review-decision-fieldset";
  const legend = document.createElement("legend");
  legend.textContent = aspect.required === false ? "Decision (optional aspect)" : "Decision";
  decision.append(legend);

  const actionList = document.createElement("div");
  actionList.className = "review-action-list";
  const panels = new Map();
  for (const action of allowedActions(aspect)) {
    const option = actionOption(aspect, action, key);
    actionList.append(option.label);
    const panel = actionPanel(aspect, draft, action, key);
    panels.set(action, panel);
  }
  decision.append(actionList, ...panels.values());

  const validation = document.createElement("p");
  validation.className = "review-decision-validation";
  validation.textContent = "Choose how this observation should be resolved.";
  decision.append(validation);

  if (!allowedActions(aspect).length) {
    validation.classList.add("error");
    validation.textContent = "The server did not provide an allowed review action for this aspect.";
  }

  article.append(header, context, decision);
  state.aspectViews.set(key, { article, status, panels, validation });
  syncAspectView(key);
  return article;
}

function actionOption(aspect, action, key) {
  const label = document.createElement("label");
  label.className = "review-action-option";
  const input = document.createElement("input");
  input.type = "radio";
  input.name = `review-action-${safeDomToken(key)}`;
  input.value = action;
  input.checked = state.drafts.get(key)?.action === action;
  input.addEventListener("change", () => {
    if (!input.checked) {
      return;
    }
    const draft = state.drafts.get(key);
    draft.action = action;
    syncAllAspectViews();
    updateProgress();
  });
  const copy = document.createElement("span");
  const title = document.createElement("strong");
  title.textContent = actionTitle(action);
  const description = document.createElement("small");
  description.textContent = actionDescription(action, aspect);
  copy.append(title, description);
  label.append(input, copy);
  return { label, input };
}

function actionPanel(aspect, draft, action, key) {
  const panel = document.createElement("div");
  panel.className = "review-action-panel is-hidden";
  panel.dataset.reviewAction = action;
  if (action === "use_verified_product") {
    panel.append(...catalogSelectionControls(aspect, draft, key));
  } else if (action === "create_verified_product") {
    panel.append(...createProductControls(draft, key));
  } else if (action === "discard") {
    panel.append(discardControls(draft, key));
  }
  return panel;
}

function catalogSelectionControls(aspect, draft, key) {
  const selected = document.createElement("div");
  selected.className = "review-selected-product";
  renderSelectedCatalogProduct(selected, draft.catalogProduct);

  const searchLabel = document.createElement("label");
  const searchCaption = document.createElement("span");
  searchCaption.textContent = "Search approved avionics";
  const search = document.createElement("input");
  search.type = "search";
  search.placeholder = "Manufacturer, model, or identifier";
  search.autocomplete = "off";
  search.value = initialCatalogSearch(aspect);
  searchLabel.append(searchCaption, search);

  const message = document.createElement("p");
  message.className = "review-catalog-message";
  message.textContent = draft.catalogProduct
    ? "The suggested verified product is selected. Search to replace it."
    : "Search for and select one approved catalog product.";
  const results = document.createElement("div");
  results.className = "review-catalog-results";

  search.addEventListener("input", () => {
    scheduleCatalogSearch(key, search.value, results, selected, message);
  });
  if (search.value.trim().length >= 2) {
    scheduleCatalogSearch(key, search.value, results, selected, message);
  }
  return [selected, searchLabel, message, results];
}

function createProductControls(draft, key) {
  const grid = document.createElement("div");
  grid.className = "review-create-product-grid";
  grid.append(
    draftInput("Manufacturer", draft.create.manufacturer, (value) => {
      draft.create.manufacturer = value;
      draftChanged(key);
    }),
    draftInput("Model", draft.create.model, (value) => {
      draft.create.model = value;
      draftChanged(key);
    }),
    draftInput("Capabilities (comma-separated)", draft.create.capabilities.join(", "), (value) => {
      draft.create.capabilities = commaSeparatedValues(value);
      draftChanged(key);
    }, "text", true),
    draftSelect(
      "Manufacturer identifier kind",
      draft.create.manufacturerIdentifierKind,
      [
        ["", "Select identifier kind"],
        ["manufacturer_part_number", "Manufacturer part number"],
        ["manufacturer_model_number", "Manufacturer model number"],
        ["sku", "SKU"],
      ],
      (value) => {
        draft.create.manufacturerIdentifierKind = value;
        draftChanged(key);
      },
    ),
    draftInput("Manufacturer identifier", draft.create.manufacturerIdentifier, (value) => {
      draft.create.manufacturerIdentifier = value;
      draftChanged(key);
    }),
    draftInput("Identity source URL", draft.create.identitySourceUrl, (value) => {
      draft.create.identitySourceUrl = value;
      draftChanged(key);
    }, "url"),
    draftInput("Identity source title", draft.create.identitySourceTitle, (value) => {
      draft.create.identitySourceTitle = value;
      draftChanged(key);
    }, "text", false, REVIEW_PRODUCT_IDENTITY_LIMITS.sourceTitle),
    draftTextarea("Identity evidence", draft.create.identityEvidenceText, (value) => {
      draft.create.identityEvidenceText = value;
      draftChanged(key);
    }, REVIEW_PRODUCT_IDENTITY_LIMITS.evidenceText),
  );
  const capabilityHint = document.createElement("p");
  capabilityHint.className = "review-catalog-message";
  capabilityHint.textContent = `Allowed capabilities: ${allowedCapabilities().join(", ")}.`;
  return [grid, capabilityHint];
}

function discardControls(draft, key) {
  return draftTextarea(
    "Reason for discarding this observation",
    draft.discardReason,
    (value) => {
      draft.discardReason = value;
      draftChanged(key);
    },
  );
}

function draftInput(
  labelText,
  value,
  onInput,
  type = "text",
  fullWidth = false,
  characterLimit = null,
) {
  const label = document.createElement("label");
  if (fullWidth) {
    label.className = "review-control-wide";
  }
  const caption = document.createElement("span");
  caption.textContent = labelText;
  const input = document.createElement("input");
  input.type = type;
  input.value = value || "";
  input.addEventListener("input", () => onInput(input.value.trim()));
  label.append(caption, input);
  appendCharacterCounter(label, input, characterLimit);
  return label;
}

function draftSelect(labelText, value, options, onChange) {
  const label = document.createElement("label");
  const caption = document.createElement("span");
  caption.textContent = labelText;
  const select = document.createElement("select");
  for (const [optionValue, optionLabel] of options) {
    const option = document.createElement("option");
    option.value = optionValue;
    option.textContent = optionLabel;
    option.selected = optionValue === value;
    select.append(option);
  }
  select.addEventListener("change", () => onChange(select.value));
  label.append(caption, select);
  return label;
}

function draftTextarea(labelText, value, onInput, characterLimit = null) {
  const label = document.createElement("label");
  label.className = "review-control-wide";
  const caption = document.createElement("span");
  caption.textContent = labelText;
  const input = document.createElement("textarea");
  input.rows = 3;
  input.value = value || "";
  input.addEventListener("input", () => onInput(input.value.trim()));
  label.append(caption, input);
  appendCharacterCounter(label, input, characterLimit);
  return label;
}

function appendCharacterCounter(label, input, characterLimit) {
  if (!Number.isSafeInteger(characterLimit) || characterLimit <= 0) {
    return;
  }
  input.maxLength = characterLimit;
  const counter = document.createElement("small");
  counter.className = "review-character-counter";
  const update = () => {
    const limitState = characterLimitState(input.value, characterLimit);
    counter.textContent = `${limitState.count} / ${limitState.limit} characters`;
    counter.classList.toggle("over-limit", limitState.overLimit);
  };
  update();
  input.addEventListener("input", update);
  label.append(counter);
}

function draftChanged(key) {
  syncAllAspectViews();
  updateProgress();
}

function syncAllAspectViews() {
  for (const key of state.drafts.keys()) {
    syncAspectView(key);
  }
}

function syncAspectView(key) {
  const view = state.aspectViews.get(key);
  const draft = state.drafts.get(key);
  if (!view || !draft) {
    return;
  }
  for (const [action, panel] of view.panels) {
    panel.classList.toggle("is-hidden", draft.action !== action);
  }
  const validation = validateDraft(draft);
  view.article.classList.toggle("is-decided", validation.valid);
  view.status.className = `review-decision-status ${validation.valid ? "decided" : "pending"}`;
  view.status.textContent = validation.valid ? "Decided" : "Needs decision";
  view.validation.classList.toggle("error", draft.action !== null && !validation.valid);
  view.validation.textContent = validation.message;
}

function validateDraft(draft) {
  if (!draft.action) {
    return { valid: false, message: "Choose how this observation should be resolved." };
  }
  if (!allowedActions(draft.aspect).includes(draft.action)) {
    return { valid: false, message: "That action is not allowed for this aspect." };
  }
  if (draft.action === "use_verified_product") {
    if (positiveInteger(draft.catalogProduct?.id) === null) {
      return { valid: false, message: "Select one approved avionics catalog product." };
    }
    return validDraftResult(draft, "Verified catalog product selected.");
  }
  if (draft.action === "create_verified_product") {
    if (!nonBlank(draft.create.manufacturer) || !nonBlank(draft.create.model)) {
      return { valid: false, message: "Manufacturer and model are required." };
    }
    if (!draft.create.capabilities.length) {
      return { valid: false, message: "Provide at least one capability." };
    }
    const supportedCapabilities = new Set(allowedCapabilities());
    const unsupported = draft.create.capabilities.filter(
      (capability) => !supportedCapabilities.has(capability),
    );
    if (unsupported.length) {
      return {
        valid: false,
        message: `Use exact canonical capability names; unsupported: ${unsupported.join(", ")}.`,
      };
    }
    if (![
      "manufacturer_part_number",
      "manufacturer_model_number",
      "sku",
    ].includes(draft.create.manufacturerIdentifierKind)) {
      return { valid: false, message: "Select the manufacturer's identifier kind." };
    }
    if (!nonBlank(draft.create.manufacturerIdentifier)) {
      return { valid: false, message: "Provide the manufacturer's stable identifier." };
    }
    if (!nonBlank(draft.create.identitySourceUrl)) {
      return { valid: false, message: "An authoritative identity source URL is required." };
    }
    if (!authoritativeIdentityUrl(draft.create.identitySourceUrl)) {
      return {
        valid: false,
        message: "Identity source must be an absolute authoritative HTTPS URL, not a sale listing.",
      };
    }
    const identitySourceValidation = reviewProductIdentitySourceValidation(
      draft.create.identitySourceTitle,
      draft.create.identityEvidenceText,
    );
    if (!identitySourceValidation.valid) {
      return identitySourceValidation;
    }
    return validDraftResult(draft, "New verified product details are complete.");
  }
  if (draft.action === "discard") {
    if (!nonBlank(draft.discardReason)) {
      return { valid: false, message: "Explain why this listing observation should be discarded." };
    }
    return validDraftResult(draft, "Discard rationale recorded.");
  }
  return { valid: false, message: "Unsupported review action." };
}

function validDraftResult(draft, message) {
  const currentKey = aspectKey(draft.aspect.id);
  for (const parent of state.drafts.values()) {
    const replacementKey = parent.aspect?.replacement_aspect_id === null
      || parent.aspect?.replacement_aspect_id === undefined
      ? null
      : aspectKey(parent.aspect.replacement_aspect_id);
    if (replacementKey === null) {
      continue;
    }
    const parentKey = aspectKey(parent.aspect.id);
    if (currentKey !== parentKey && currentKey !== replacementKey) {
      continue;
    }
    const replacement = state.drafts.get(replacementKey);
    if (!parent.action || !replacement?.action) {
      continue;
    }
    const parentDiscarded = parent.action === "discard";
    const replacementDiscarded = replacement.action === "discard";
    if (parentDiscarded !== replacementDiscarded) {
      return {
        valid: false,
        message: "A product and its replacement target must either both be accepted or both be discarded.",
      };
    }
  }
  return { valid: true, message };
}

function updateProgress() {
  const drafts = Array.from(state.drafts.values());
  const decided = drafts.filter((draft) => validateDraft(draft).valid).length;
  const total = drafts.length;
  const aircraftVerified = aircraftIdentityIsVerified(
    state.currentReview?.aircraft_identity,
  );
  elements.reviewProgress.max = Math.max(total, 1);
  elements.reviewProgress.value = decided;
  elements.reviewProgressLabel.textContent = [
    `${decided} of ${total} avionics decided`,
    ...(!aircraftVerified ? ["aircraft curation required"] : []),
  ].join(" · ");
  elements.reviewAircraftTabCount.textContent = aircraftVerified ? "0" : "1";
  elements.reviewAircraftTab.setAttribute(
    "aria-label",
    aircraftVerified
      ? "Aircraft, FAA identity verified"
      : "Aircraft, curation required before listing verification",
  );
  elements.reviewAvionicsTabCount.textContent = String(total);
  elements.reviewAvionicsTab.setAttribute(
    "aria-label",
    `Avionics, ${total} ${pluralize(total, "decision")}, `
      + `${decided} decided, ${total - decided} remaining`,
  );
  const hashesPresent = nonBlank(state.currentReview?.review_payload_sha256)
    && nonBlank(state.currentReview?.catalog_revision_sha256);
  elements.verifyListing.disabled = state.resolving
    || state.automating
    || state.stale
    || !state.currentReview
    || decided !== total
    || !hashesPresent
    || !aircraftVerified;
  elements.automaticallyVerifyListing.disabled = state.resolving
    || state.automating
    || state.stale
    || !state.currentReview
    || !state.pipelineLoaded
    || pipelineRowForListing(currentListingId()) === null
    || (
      activeVerificationRunIsBusy()
      && verificationRunIncludesListing(currentListingId())
    )
    || !pipelineAutomaticEligibility(
      pipelineRowForListing(currentListingId()),
    ).eligible;
}

async function automaticallyVerifyListing() {
  const review = state.currentReview;
  if (!review || state.stale || state.resolving || state.automating) {
    return;
  }
  await startVerificationRun([review.listing_id], { openedListing: true });
}

async function leaveAutomaticallyVerifiedReview(listingId, previousQueue, label) {
  state.currentReview = null;
  state.drafts.clear();
  state.aspectViews.clear();
  const queueRefreshed = await loadQueue({ quiet: true });
  if (!queueRefreshed) {
    state.reviews = previousQueue.filter(
      (item) => positiveInteger(item?.listing_id) !== listingId,
    );
    state.total = Math.max(0, state.total - 1);
    renderQueue();
  }
  await Promise.allSettled([
    Promise.resolve(refreshListings?.()),
    Promise.resolve(refreshAvionics?.()),
  ]);
  const nextId = nextAfterResolved(previousQueue, listingId);
  if (nextId !== null) {
    await openReview(nextId, {
      historyMode: "replace",
      discardDraft: true,
      force: true,
    });
    setWorkspaceMessage(`${label}. Loaded the next pending review.`);
    return;
  }
  showQueue({ historyMode: "replace", discardDraft: true });
  setQueueMessage(
    state.total === 0
      ? `${label}. The review queue is clear.`
      : `${label}.`,
  );
}

function setAutomaticVerificationBusy(busy) {
  if (state.automating === busy) {
    return;
  }
  state.automating = busy;
  elements.reviewWorkspace.setAttribute("aria-busy", String(busy));
  if (busy) {
    state.automationControlStates.clear();
    for (const control of elements.reviewWorkspace.querySelectorAll(
      "button, input, select, textarea",
    )) {
      state.automationControlStates.set(control, control.disabled);
      control.disabled = true;
    }
    return;
  }
  for (const [control, disabled] of state.automationControlStates) {
    if (control.isConnected) {
      control.disabled = disabled;
    }
  }
  state.automationControlStates.clear();
  updateProgress();
  updateNextButton();
}

async function resolveReview() {
  const review = state.currentReview;
  if (!review || state.stale || state.resolving || state.automating) {
    return;
  }
  if (!aircraftIdentityIsVerified(review.aircraft_identity)) {
    setWorkspaceMessage(
      "Aircraft catalog curation and FAA verification must be completed before this listing can be verified.",
      true,
    );
    setActiveReviewArea("aircraft", { updateLocation: true });
    updateProgress();
    return;
  }
  const drafts = Array.from(state.drafts.values());
  if (drafts.some((draft) => !validateDraft(draft).valid)) {
    setWorkspaceMessage("Resolve every pending check before verifying the listing.", true);
    setActiveReviewArea("avionics", { updateLocation: true });
    const firstInvalid = drafts.find((draft) => !validateDraft(draft).valid);
    state.aspectViews.get(aspectKey(firstInvalid?.aspect?.id))?.article.scrollIntoView({
      behavior: "smooth",
      block: "start",
    });
    updateProgress();
    return;
  }

  const request = {
    review_payload_sha256: review.review_payload_sha256,
    catalog_revision_sha256: review.catalog_revision_sha256,
    finalize_listing: true,
    decisions: drafts.map(decisionFromDraft),
  };
  const previousQueue = state.reviews.slice();
  const resolvedListingId = review.listing_id;
  state.resolving = true;
  updateProgress();
  setWorkspaceMessage("Saving review decisions and completing final enrichment…");
  setButtonBusy(elements.verifyListing, true);
  try {
    const payload = await api(`/api/review/listings/${resolvedListingId}/resolve`, {
      method: "POST",
      body: JSON.stringify(request),
    });
    const outcome = describeResolvedListingOutcome(payload, resolvedListingId);
    if (!outcome.terminal) {
      throw new Error(outcome.detail);
    }
    state.currentReview = null;
    state.drafts.clear();
    state.aspectViews.clear();
    const queueRefreshed = await loadQueue({ quiet: true });
    if (!queueRefreshed) {
      state.reviews = previousQueue.filter(
        (item) => positiveInteger(item?.listing_id) !== resolvedListingId,
      );
      state.total = Math.max(0, state.total - 1);
      renderQueue();
    }
    await Promise.allSettled([
      Promise.resolve(refreshListings?.()),
      Promise.resolve(refreshAvionics?.()),
    ]);
    const nextId = nextAfterResolved(previousQueue, resolvedListingId);
    if (nextId !== null) {
      await openReview(nextId, {
        historyMode: "replace",
        discardDraft: true,
        force: true,
      });
      setWorkspaceMessage(`${outcome.label}. Loaded the next pending review.`);
    } else {
      showQueue({ historyMode: "replace", discardDraft: true });
      setQueueMessage(
        state.total === 0
          ? `${outcome.label}. The review queue is clear.`
          : `${outcome.label}.`,
      );
    }
  } catch (error) {
    if (isAvionicsCatalogConsolidated(error)) {
      state.resolving = false;
      await Promise.allSettled([
        loadQueue({ quiet: true }),
        Promise.resolve(refreshAvionics?.()),
      ]);
      await openReview(resolvedListingId, {
        historyMode: "none",
        discardDraft: true,
        force: true,
      });
      if (
        positiveInteger(state.currentReview?.listing_id) === resolvedListingId
        && !state.stale
      ) {
        setWorkspaceMessage(
          "Duplicate avionics catalog records were consolidated into a verified product. The review was refreshed with the corrected catalog identity; confirm the updated selection and verify again.",
        );
      }
    } else if (isStaleError(error)) {
      markStale(error.message);
    } else if (isFinalizationError(error)) {
      await recoverCommittedResolution(
        resolvedListingId,
        `Review decisions were saved, but listing ${resolvedListingId} was quarantined during final enrichment: ${error.message}`,
      );
    } else if (shouldReconcileResolution(error)) {
      const stillPending = await pendingReviewStatus(resolvedListingId);
      if (stillPending === false) {
        await recoverCommittedResolution(
          resolvedListingId,
          `Review decisions were saved, but the response was interrupted. Inspect listing ${resolvedListingId} to confirm its final enrichment state.`,
        );
      } else {
        setWorkspaceMessage(`Could not verify listing: ${error.message}`, true);
      }
    } else {
      setWorkspaceMessage(`Could not verify listing: ${error.message}`, true);
    }
  } finally {
    state.resolving = false;
    setButtonBusy(elements.verifyListing, false);
    updateProgress();
  }
}

async function validateExistingAssociation(key, button) {
  const review = state.currentReview;
  const draft = state.drafts.get(key);
  const targetId = positiveInteger(draft?.aspect?.reuse_attestation_target?.id);
  if (
    !review
    || !draft
    || targetId === null
    || !listingAssociationCanValidateLocally(draft.aspect)
    || state.stale
  ) {
    return;
  }
  button.disabled = true;
  setWorkspaceMessage(`Validating this listing association against catalog product ${targetId}…`);
  try {
    const payload = await api(
      `/api/review/listings/${review.listing_id}/avionics/verify-existing`,
      {
        method: "POST",
        body: JSON.stringify(existingProductVerificationRequest(
          review.review_payload_sha256,
          review.catalog_revision_sha256,
          draft.aspect.id,
        )),
      },
    );
    const refreshed = payload?.review;
    if (isCompletedReviewMaintenanceResponse(payload)) {
      await leaveCompletedOneByOneReview(review.listing_id);
      return;
    }
    if (!isReviewDetail(refreshed, review.listing_id)) {
      throw new Error("The server returned an invalid refreshed listing review.");
    }
    state.currentReview = refreshed;
    state.drafts.clear();
    state.aspectViews.clear();
    initializeDrafts(refreshed);
    renderReview();
    setWorkspaceMessage(
      `The listing text matched catalog product ${targetId}. Review the remaining aspects.`,
    );
  } catch (error) {
    if (isStaleError(error)) {
      markStale(error.message);
    } else {
      const outcome = describeProductAssociationOutcome(error);
      setWorkspaceMessage(`${outcome.label}: ${outcome.detail}`, true);
      button.disabled = false;
    }
  }
}

async function leaveCompletedOneByOneReview(listingId) {
  state.currentReview = null;
  state.drafts.clear();
  state.aspectViews.clear();
  state.reviews = state.reviews.filter(
    (item) => positiveInteger(item?.listing_id) !== listingId,
  );
  state.total = Math.max(0, state.total - 1);
  renderQueue();
  await loadQueue({ quiet: true });
  await Promise.allSettled([
    Promise.resolve(refreshListings?.()),
    Promise.resolve(refreshAvionics?.()),
  ]);
  showQueue({ historyMode: "replace", discardDraft: true });
  setQueueMessage(
    state.total === 0
      ? `Listing ${listingId} review completed. The review queue is clear.`
      : `Listing ${listingId} review completed.`,
  );
}

async function pendingReviewStatus(listingId) {
  try {
    await api(`/api/review/listings/${listingId}`);
    return true;
  } catch (error) {
    if (error?.status === 404) {
      return false;
    }
    return null;
  }
}

async function recoverCommittedResolution(listingId, message) {
  state.currentReview = null;
  state.drafts.clear();
  state.aspectViews.clear();
  state.reviews = state.reviews.filter(
    (item) => positiveInteger(item?.listing_id) !== listingId,
  );
  state.total = Math.max(0, state.total - 1);
  renderQueue();
  await loadQueue({ quiet: true });
  await Promise.allSettled([
    Promise.resolve(refreshListings?.()),
    Promise.resolve(refreshAvionics?.()),
  ]);
  showQueue({ historyMode: "replace", discardDraft: true });
  setQueueMessage(message, true);
}

function decisionFromDraft(draft) {
  if (draft.action === "use_verified_product") {
    return {
      aspect_id: draft.aspect.id,
      action: draft.action,
      avionics_model_id: positiveInteger(draft.catalogProduct.id),
    };
  }
  if (draft.action === "create_verified_product") {
    const decision = {
      aspect_id: draft.aspect.id,
      action: draft.action,
      manufacturer: draft.create.manufacturer.trim(),
      model: draft.create.model.trim(),
      capabilities: draft.create.capabilities.slice(),
      manufacturer_identifier_kind: draft.create.manufacturerIdentifierKind,
      manufacturer_identifier: draft.create.manufacturerIdentifier.trim(),
      identity_source_url: draft.create.identitySourceUrl.trim(),
      identity_source_title: draft.create.identitySourceTitle.trim(),
      identity_evidence_text: draft.create.identityEvidenceText.trim(),
    };
    return decision;
  }
  return {
    aspect_id: draft.aspect.id,
    action: "discard",
    reason: draft.discardReason.trim(),
  };
}

function scheduleCatalogSearch(key, query, results, selected, message) {
  const previous = state.catalogSearchTimers.get(key);
  if (previous) {
    window.clearTimeout(previous);
  }
  const normalized = query.trim();
  message.classList.remove("error");
  if (normalized.length < 2) {
    state.catalogSearchSequences.set(
      key,
      (state.catalogSearchSequences.get(key) || 0) + 1,
    );
    results.replaceChildren();
    message.textContent = "Enter at least two characters to search the approved catalog.";
    state.catalogSearchTimers.delete(key);
    return;
  }
  message.textContent = "Waiting to search…";
  const timer = window.setTimeout(() => {
    state.catalogSearchTimers.delete(key);
    searchCatalog(key, normalized, results, selected, message);
  }, CATALOG_SEARCH_DELAY_MS);
  state.catalogSearchTimers.set(key, timer);
}

async function searchCatalog(key, query, results, selected, message) {
  const sequence = (state.catalogSearchSequences.get(key) || 0) + 1;
  state.catalogSearchSequences.set(key, sequence);
  message.textContent = "Searching approved avionics…";
  results.setAttribute("aria-busy", "true");
  try {
    const params = new URLSearchParams({
      search: query,
      status: "approved",
      limit: String(CATALOG_RESULT_LIMIT),
      offset: "0",
    });
    const payload = await api(`/api/avionics?${params}`);
    if (state.catalogSearchSequences.get(key) !== sequence || !state.drafts.has(key)) {
      return;
    }
    const items = Array.isArray(payload?.catalog?.items) ? payload.catalog.items : [];
    results.replaceChildren(
      ...items.map((item) => catalogResult(item, () => {
        const draft = state.drafts.get(key);
        if (!draft) {
          return;
        }
        draft.catalogProduct = normalizedProduct(item);
        renderSelectedCatalogProduct(selected, draft.catalogProduct);
        message.textContent = `${draft.catalogProduct.displayName} selected.`;
        message.classList.remove("error");
        syncAllAspectViews();
        updateProgress();
        loadSelectedProductEvidence(key, draft.catalogProduct.id, selected, message);
      })),
    );
    message.classList.remove("error");
    message.textContent = items.length
      ? `${items.length} approved ${pluralize(items.length, "match")} found.`
      : "No approved avionics matched this search.";
  } catch (error) {
    if (state.catalogSearchSequences.get(key) === sequence) {
      results.replaceChildren();
      message.textContent = `Catalog search failed: ${error.message}`;
      message.classList.add("error");
    }
  } finally {
    if (state.catalogSearchSequences.get(key) === sequence) {
      results.setAttribute("aria-busy", "false");
    }
  }
}

function catalogResult(item, onSelect) {
  const product = normalizedProduct(item);
  const button = document.createElement("button");
  button.type = "button";
  button.className = "review-catalog-result";
  button.disabled = !product || positiveInteger(product.id) === null;
  const title = document.createElement("strong");
  title.textContent = product?.displayName || "Unknown catalog product";
  const metadata = document.createElement("span");
  metadata.textContent = [
    product?.stableIdentifier,
    product?.capabilities.join(", "),
  ].filter(nonBlank).join(" · ") || "Approved catalog entry";
  button.append(title, metadata);
  button.addEventListener("click", onSelect);
  return button;
}

function renderSelectedCatalogProduct(container, product) {
  container.replaceChildren();
  if (!product || positiveInteger(product.id) === null) {
    const empty = document.createElement("p");
    empty.className = "review-selection-empty";
    empty.textContent = "No verified product selected.";
    container.append(empty);
    return;
  }
  const eyebrow = document.createElement("span");
  eyebrow.className = "review-eyebrow";
  eyebrow.textContent = "Selected verified product";
  const title = document.createElement("strong");
  title.textContent = product.displayName;
  const metadata = document.createElement("span");
  metadata.textContent = [
    `Catalog ID ${product.id}`,
    product.stableIdentifier,
  ].filter(nonBlank).join(" · ");
  container.append(eyebrow, title, metadata, renderAvionicsChips(product.capabilities));
  appendProductIdentityEvidence(container, product);
}

async function loadSelectedProductEvidence(key, productId, selected, message) {
  if (positiveInteger(productId) === null) {
    return;
  }
  message.textContent = "Loading authoritative identity evidence…";
  try {
    const payload = await api(`/api/avionics/${productId}`);
    const detail = payload?.avionics;
    const summary = detail?.summary;
    if (!summary || positiveInteger(summary.id) !== productId) {
      throw new Error("The server returned an invalid avionics record.");
    }
    const draft = state.drafts.get(key);
    if (positiveInteger(draft?.catalogProduct?.id) !== productId) {
      return;
    }
    const identity = detail.identity_evidence || {};
    draft.catalogProduct = normalizedProduct({
      ...summary,
      identity_source_url: identity.source_url,
      identity_source_title: identity.source_title,
      identity_evidence_text: identity.evidence_text,
    });
    renderSelectedCatalogProduct(selected, draft.catalogProduct);
    const hasAuthoritativeEvidence = authoritativeIdentityUrl(
      draft.catalogProduct.identitySourceUrl,
    ) && nonBlank(draft.catalogProduct.identityEvidenceText);
    message.classList.toggle("error", !hasAuthoritativeEvidence);
    message.textContent = hasAuthoritativeEvidence
      ? `${draft.catalogProduct.displayName} selected with authoritative identity evidence.`
      : `${draft.catalogProduct.displayName} selected, but its catalog identity evidence is incomplete.`;
  } catch (error) {
    const draft = state.drafts.get(key);
    if (positiveInteger(draft?.catalogProduct?.id) !== productId) {
      return;
    }
    message.classList.add("error");
    message.textContent = `Product selected, but its identity evidence could not be loaded: ${error.message}`;
  }
}

function productSummary(value, heading) {
  const product = normalizedProduct(value);
  const container = document.createElement("div");
  container.className = "review-product-summary";
  const eyebrow = document.createElement("span");
  eyebrow.className = "review-eyebrow";
  eyebrow.textContent = heading;
  const title = document.createElement("strong");
  title.textContent = product?.displayName || "Unknown product";
  const metadata = document.createElement("span");
  metadata.textContent = [
    product?.id && `Catalog ID ${product.id}`,
    product?.stableIdentifier,
    product?.catalogStatus && displayLabel(product.catalogStatus),
  ].filter(nonBlank).join(" · ");
  container.append(eyebrow, title);
  if (metadata.textContent) {
    container.append(metadata);
  }
  if (product?.capabilities.length) {
    container.append(renderAvionicsChips(product.capabilities));
  }
  appendProductIdentityEvidence(container, product);
  return container;
}

function appendProductIdentityEvidence(container, product) {
  const source = authoritativeIdentityUrl(product?.identitySourceUrl)
    ? safeDetailLink(
      product.identitySourceUrl,
      product.identitySourceTitle || "Open authoritative identity source",
    )
    : null;
  if (source) {
    source.className = "review-product-source";
    container.append(source);
  } else {
    const missingSource = document.createElement("span");
    missingSource.className = "review-product-evidence-missing";
    missingSource.textContent = "No authoritative identity source recorded.";
    container.append(missingSource);
  }
  const evidence = document.createElement("p");
  evidence.className = "review-product-evidence";
  if (nonBlank(product?.identityEvidenceText)) {
    evidence.append(
      strongText("Identity evidence: "),
      document.createTextNode(product.identityEvidenceText),
    );
  } else {
    evidence.classList.add("empty");
    evidence.textContent = "No authoritative identity evidence recorded.";
  }
  container.append(evidence);
}

function normalizedProduct(value) {
  if (!value || typeof value !== "object") {
    return null;
  }
  const id = positiveInteger(value.id ?? value.avionics_model_id);
  const manufacturer = productManufacturer(value);
  const model = optionalText(value.model ?? value.name);
  const capabilities = normalizedTextList(value.capabilities ?? value.types);
  const stableIdentifierValue = value.stable_identifier?.value
    ?? value.manufacturer_identifier
    ?? value.identifier;
  const stableIdentifierKind = value.stable_identifier?.kind
    ?? value.manufacturer_identifier_kind
    ?? value.identifier_kind;
  const stableIdentifier = nonBlank(stableIdentifierValue)
    ? [displayLabel(stableIdentifierKind), stableIdentifierValue].filter(nonBlank).join(" ")
    : "";
  return {
    id,
    manufacturer,
    model,
    displayName: value.display_name
      || [manufacturer, model].filter(nonBlank).join(" ")
      || (id !== null ? `Avionics ${id}` : "Proposed avionics"),
    capabilities,
    stableIdentifier,
    catalogStatus: value.catalog?.status ?? value.catalog_status,
    identitySourceUrl: optionalText(value.identity_source_url),
    identitySourceTitle: optionalText(value.identity_source_title),
    identityEvidenceText: optionalText(value.identity_evidence_text),
  };
}

function productManufacturer(value) {
  const manufacturer = value?.manufacturer;
  if (manufacturer && typeof manufacturer === "object") {
    return optionalText(manufacturer.name);
  }
  return optionalText(manufacturer ?? value?.manufacturer_name);
}

function initialCatalogSearch(aspect) {
  const proposed = normalizedProduct(aspect.proposed_product);
  if (proposed) {
    return [proposed.manufacturer, proposed.model].filter(nonBlank).join(" ");
  }
  return aspect.observed_text || "";
}

function allowedActions(aspect) {
  if (!Array.isArray(aspect?.allowed_actions)) {
    return [];
  }
  const allowed = new Set(aspect.allowed_actions);
  return SUPPORTED_ACTIONS.filter((action) => allowed.has(action));
}

function actionTitle(action) {
  return {
    use_verified_product: "Use verified product",
    create_verified_product: "Create verified product",
    discard: "Discard observation",
  }[action] || displayLabel(action);
}

function actionDescription(action, aspect) {
  if (action === "use_verified_product") {
    return "Map the listing text to one approved avionics catalog identity.";
  }
  if (action === "create_verified_product") {
    const proposedId = positiveInteger(aspect.proposed_product?.id);
    const suggestedId = positiveInteger(aspect.suggested_product?.id);
    if (proposedId !== null && proposedId !== suggestedId) {
      return "Curate the matched candidate, or enter a corrected identity as a separate product.";
    }
    return suggestedId !== null
      ? "Enter a corrected identity as a separate verified product."
      : "Approve the proposed identity as a new catalog product.";
  }
  if (aspect.required === false) {
    return "Exclude this optional observation and record why.";
  }
  return "Reject this observation as unusable and record why.";
}

function reviewMetadataItem(label, value) {
  const wrapper = document.createElement("div");
  const term = document.createElement("dt");
  term.textContent = label;
  const description = document.createElement("dd");
  description.textContent = value;
  wrapper.append(term, description);
  return wrapper;
}

function reviewQuantity(value) {
  const quantity = positiveInteger(value);
  return quantity === null ? "Not recorded" : formatNumber(quantity, 0);
}

function replacementProductHeading(configurationAction) {
  if (configurationAction === "removes") {
    return "Product being removed";
  }
  if (configurationAction === "replaces") {
    return "Product being replaced";
  }
  return "Replacement target product";
}

function setWorkspaceLoading(listingId) {
  elements.reviewWorkspaceTitle.textContent = `Listing ${listingId}`;
  elements.reviewWorkspaceSubtitle.textContent = "Loading review details…";
  elements.reviewSourceLabel.textContent = "Loading source…";
  elements.reviewSourceLink.classList.add("is-hidden");
  elements.reviewStale.classList.add("is-hidden");
  elements.reviewAircraftSummary.replaceChildren(
    workspaceState("Loading aircraft context…"),
  );
  elements.reviewAvionicsReasons.replaceChildren();
  elements.reviewAvionicsAspects.setAttribute("aria-busy", "true");
  elements.reviewAvionicsAspects.replaceChildren(
    workspaceState("Loading every pending avionics check…"),
  );
  elements.reviewAircraftTabCount.textContent = "0";
  elements.reviewAvionicsTabCount.textContent = "0";
  setActiveReviewArea(state.activeArea);
  setWorkspaceMessage("");
  elements.reviewProgress.max = 1;
  elements.reviewProgress.value = 0;
  elements.reviewProgressLabel.textContent = "Loading decisions";
  elements.automaticallyVerifyListing.disabled = true;
  elements.verifyListing.disabled = true;
  updateNextButton(listingId);
}

function renderReviewLoadError(listingId, error) {
  state.currentReview = null;
  state.drafts.clear();
  elements.reviewWorkspaceSubtitle.textContent = "Review unavailable";
  elements.reviewAircraftSummary.replaceChildren();
  elements.reviewAvionicsReasons.replaceChildren();
  elements.reviewAvionicsAspects.setAttribute("aria-busy", "false");
  const errorState = workspaceState(`Could not load listing ${listingId}: ${error.message}`, true);
  const retry = document.createElement("button");
  retry.type = "button";
  retry.className = "button";
  retry.textContent = "Try again";
  retry.addEventListener("click", () => {
    openReview(listingId, { historyMode: "none", discardDraft: true, force: true });
  });
  errorState.append(retry);
  elements.reviewAvionicsAspects.replaceChildren(errorState);
  elements.reviewAircraftTabCount.textContent = "0";
  elements.reviewAvionicsTabCount.textContent = "0";
  setActiveReviewArea("avionics", { updateLocation: true });
  elements.automaticallyVerifyListing.disabled = true;
  elements.verifyListing.disabled = true;
  elements.reviewProgressLabel.textContent = "Review unavailable";
  setWorkspaceMessage("Review details could not be loaded.", true);
}

function workspaceState(message, isError = false) {
  const container = document.createElement("div");
  container.className = `review-workspace-state${isError ? " error" : ""}`;
  container.setAttribute("role", isError ? "alert" : "status");
  const text = document.createElement("p");
  text.textContent = message;
  container.append(text);
  return container;
}

function markStale(message) {
  state.stale = true;
  state.resolving = false;
  elements.reviewStale.classList.remove("is-hidden");
  setWorkspaceMessage(
    message
      ? `The review is stale: ${message}`
      : "The listing or avionics catalog changed while this review was open.",
    true,
  );
  updateProgress();
}

function isStaleError(error) {
  return error?.payload?.error?.code === "review_stale"
    || error?.status === 412;
}

function isAvionicsCatalogConsolidated(error) {
  return error?.payload?.error?.code === "avionics_catalog_consolidated";
}

function isFinalizationError(error) {
  return error?.payload?.error?.code === "listing_finalization_failed";
}

function shouldReconcileResolution(error) {
  return !Number.isInteger(error?.status) || error.status >= 500;
}

function showWorkspace() {
  elements.reviewQueueView.classList.add("is-hidden");
  elements.reviewWorkspace.classList.remove("is-hidden");
}

function showQueue({ historyMode = "push", discardDraft = false } = {}) {
  if (!discardDraft && !confirmDiscardDraft()) {
    return false;
  }
  cancelCatalogSearches();
  state.detailRequestSequence += 1;
  state.currentReview = null;
  state.drafts.clear();
  state.aspectViews.clear();
  state.stale = false;
  state.resolving = false;
  state.automating = false;
  state.automationControlStates.clear();
  elements.reviewWorkspace.setAttribute("aria-busy", "false");
  elements.automaticallyVerifyListing.disabled = true;
  elements.reviewWorkspace.classList.add("is-hidden");
  elements.reviewQueueView.classList.remove("is-hidden");
  updateReviewLocation(null, historyMode);
  if (state.queueMode === "pipeline" && !state.pipelineLoaded) {
    loadPipelineQueue();
  } else if (state.queueMode === "product" && !state.productGroups.length) {
    loadProductQueue();
  } else if (state.queueMode === "listing" && !state.queueLoaded) {
    loadQueue();
  }
  return true;
}

function updateReviewLocation(listingId, mode) {
  if (mode === "none") {
    return;
  }
  const url = new URL(window.location.href);
  if (listingId === null) {
    url.searchParams.delete(REVIEW_LISTING_PARAM);
    url.searchParams.delete(REVIEW_AREA_PARAM);
  } else {
    url.searchParams.set(REVIEW_LISTING_PARAM, String(listingId));
    url.searchParams.set(REVIEW_AREA_PARAM, state.activeArea);
  }
  const method = mode === "replace" ? "replaceState" : "pushState";
  window.history[method](
    { reviewListingId: listingId, reviewArea: listingId === null ? null : state.activeArea },
    "",
    url,
  );
}

function reviewListingIdFromLocation() {
  const value = new URL(window.location.href).searchParams.get(REVIEW_LISTING_PARAM);
  return positiveInteger(value);
}

function reviewAreaFromLocation() {
  const value = new URL(window.location.href).searchParams.get(REVIEW_AREA_PARAM);
  return REVIEW_AREAS.includes(value) ? value : null;
}

function currentListingId() {
  return positiveInteger(state.currentReview?.listing_id) ?? reviewListingIdFromLocation();
}

function updateNextButton(listingId = currentListingId()) {
  const nextId = nextPendingListingId(listingId);
  elements.reviewNext.disabled = nextId === null;
  elements.reviewNext.title = nextId === null ? "No other pending listing" : `Open listing ${nextId}`;
}

function nextPendingListingId(listingId = currentListingId()) {
  if (!state.reviews.length) {
    return null;
  }
  const index = state.reviews.findIndex((item) => positiveInteger(item?.listing_id) === listingId);
  if (index === -1) {
    return positiveInteger(state.reviews[0]?.listing_id);
  }
  for (let offset = 1; offset < state.reviews.length; offset += 1) {
    const candidate = state.reviews[(index + offset) % state.reviews.length];
    const candidateId = positiveInteger(candidate?.listing_id);
    if (candidateId !== null && candidateId !== listingId) {
      return candidateId;
    }
  }
  return null;
}

function nextAfterResolved(previousQueue, resolvedListingId) {
  if (!state.reviews.length) {
    return null;
  }
  const currentIds = new Set(
    state.reviews.map((item) => positiveInteger(item?.listing_id)).filter((id) => id !== null),
  );
  const previousIndex = previousQueue.findIndex(
    (item) => positiveInteger(item?.listing_id) === resolvedListingId,
  );
  if (previousIndex !== -1) {
    for (let offset = 1; offset <= previousQueue.length; offset += 1) {
      const candidate = previousQueue[(previousIndex + offset) % previousQueue.length];
      const candidateId = positiveInteger(candidate?.listing_id);
      if (candidateId !== null && currentIds.has(candidateId)) {
        return candidateId;
      }
    }
  }
  return positiveInteger(state.reviews[0]?.listing_id);
}

function hasDraftDecisions() {
  return Array.from(state.drafts.values()).some((draft) => draft.action !== null);
}

function confirmDiscardDraft() {
  return !hasDraftDecisions()
    || window.confirm("Discard the decisions made for this listing review?");
}

function cancelCatalogSearches() {
  for (const timer of state.catalogSearchTimers.values()) {
    window.clearTimeout(timer);
  }
  state.catalogSearchTimers.clear();
  state.catalogSearchSequences.clear();
}

function setQueueMessage(message, isError = false) {
  elements.reviewQueueMessage.textContent = message || "";
  elements.reviewQueueMessage.classList.toggle("error", isError);
}

function setWorkspaceMessage(message, isError = false) {
  elements.reviewWorkspaceMessage.textContent = message || "";
  elements.reviewWorkspaceMessage.classList.toggle("error", isError);
}

function strongText(value) {
  const strong = document.createElement("strong");
  strong.textContent = value;
  return strong;
}

function normalizedTextList(values) {
  const list = Array.isArray(values) ? values : values ? [values] : [];
  const normalized = list
    .map((value) => {
      if (typeof value === "string" || typeof value === "number") {
        return String(value).trim();
      }
      return optionalText(value?.name ?? value?.value ?? value?.code ?? value?.capability);
    })
    .filter(nonBlank);
  return Array.from(new Set(normalized));
}

function commaSeparatedValues(value) {
  return Array.from(new Set(
    String(value || "")
      .split(",")
      .map((item) => item.trim())
      .filter(Boolean),
  ));
}

function allowedCapabilities() {
  return normalizedTextList(state.currentReview?.allowed_capabilities);
}

function aspectKey(value) {
  return String(value ?? "");
}

function safeDomToken(value) {
  return String(value).replace(/[^a-zA-Z0-9_-]/g, "-");
}

function optionalText(value) {
  return value === null || value === undefined ? "" : String(value).trim();
}

function nonBlank(value) {
  return value !== null && value !== undefined && String(value).trim().length > 0;
}

function positiveInteger(value) {
  if (value === null || value === undefined || value === "") {
    return null;
  }
  const numeric = Number(value);
  return Number.isSafeInteger(numeric) && numeric > 0 ? numeric : null;
}

function nonNegativeInteger(value) {
  if (value === null || value === undefined || value === "") {
    return null;
  }
  const numeric = Number(value);
  return Number.isSafeInteger(numeric) && numeric >= 0 ? numeric : null;
}

function pluralize(count, word) {
  return count === 1 ? word : `${word}s`;
}

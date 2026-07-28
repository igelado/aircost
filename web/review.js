import { displayLabel, renderAvionicsChips, safeDetailLink } from "/avionics.js";
import {
  REVIEW_AREAS,
  aircraftIdentityIsVerified,
  describeAircraftIdentity,
  describeReviewReasons,
  isAircraftIdentityStatus,
  isCompletedReviewMaintenanceResponse,
  preselectedReviewAction,
  reviewAreaForAspect,
} from "/review/domain.mjs";

const REVIEW_LISTING_PARAM = "review_listing";
const REVIEW_AREA_PARAM = "review_area";
const QUEUE_LIMIT = 100;
const CATALOG_RESULT_LIMIT = 8;
const CATALOG_SEARCH_DELAY_MS = 250;
const SUPPORTED_ACTIONS = Object.freeze([
  "use_verified_product",
  "verify_existing_product",
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
  queueRequestSequence: 0,
  detailRequestSequence: 0,
  currentReview: null,
  drafts: new Map(),
  aspectViews: new Map(),
  catalogSearchTimers: new Map(),
  catalogSearchSequences: new Map(),
  activeArea: "avionics",
  stale: false,
  resolving: false,
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
  initialized = true;

  return Object.freeze({
    activate() {
      const listingId = reviewListingIdFromLocation();
      if (listingId !== null) {
        state.activeArea = reviewAreaFromLocation() ?? "avionics";
        const queueLoad = state.queueLoaded ? Promise.resolve() : loadQueue({ quiet: true });
        const detailLoad = openReview(listingId, { historyMode: "none", discardDraft: true });
        return Promise.allSettled([queueLoad, detailLoad]);
      }
      showQueue({ historyMode: "none", discardDraft: true });
      return state.queueLoaded ? Promise.resolve() : loadQueue();
    },
    refresh() {
      return loadQueue();
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
    reviewAspectCount: "#review-aspect-count",
    reviewReasonCount: "#review-reason-count",
    reviewQueueView: "#review-queue-view",
    refreshReviews: "#refresh-reviews",
    reviewQueueMessage: "#review-queue-message",
    reviewResults: "#review-results",
    reviewTableBody: "#review-table-body",
    emptyReviews: "#empty-reviews",
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
    verifyListing: "#verify-listing",
  })) {
    elements[key] = document.querySelector(selector);
    if (!elements[key]) {
      throw new Error(`Missing listing review element: ${selector}`);
    }
  }
}

function bindEvents() {
  elements.refreshReviews.addEventListener("click", () => loadQueue());
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
      "Existing product requiring fresh verification",
    );
    target.classList.add("review-suggested-product");
    context.append(target);
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
  } else if (action === "verify_existing_product") {
    panel.append(...verifyExistingProductControls(draft, key));
  } else if (action === "create_verified_product") {
    panel.append(...createProductControls(draft, key));
  } else if (action === "discard") {
    panel.append(discardControls(draft, key));
  }
  return panel;
}

function verifyExistingProductControls(draft, key) {
  const controls = document.createElement("div");
  controls.className = "review-create-product-grid";
  controls.append(
    draftInput("Authoritative identity source URL", draft.create.identitySourceUrl, (value) => {
      draft.create.identitySourceUrl = value;
      draftChanged(key);
    }, "url", true),
    draftInput("Authoritative identity source title", draft.create.identitySourceTitle, (value) => {
      draft.create.identitySourceTitle = value;
      draftChanged(key);
    }, "text", true),
    draftTextarea("Exact identity evidence", draft.create.identityEvidenceText, (value) => {
      draft.create.identityEvidenceText = value;
      draftChanged(key);
    }),
  );
  const button = document.createElement("button");
  button.type = "button";
  button.className = "button";
  button.textContent = "Verify this product now";
  button.addEventListener("click", () => verifyExistingProduct(key, button));
  const hint = document.createElement("p");
  hint.className = "review-catalog-message";
  hint.textContent = "This grounds only this product. A successful verification is saved before the listing is resolved.";
  return [controls, button, hint];
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
    }),
    draftTextarea("Identity evidence", draft.create.identityEvidenceText, (value) => {
      draft.create.identityEvidenceText = value;
      draftChanged(key);
    }),
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

function draftInput(labelText, value, onInput, type = "text", fullWidth = false) {
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

function draftTextarea(labelText, value, onInput) {
  const label = document.createElement("label");
  label.className = "review-control-wide";
  const caption = document.createElement("span");
  caption.textContent = labelText;
  const input = document.createElement("textarea");
  input.rows = 3;
  input.value = value || "";
  input.addEventListener("input", () => onInput(input.value.trim()));
  label.append(caption, input);
  return label;
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
  if (draft.action === "verify_existing_product") {
    if (!nonBlank(draft.create.identitySourceUrl)
      || !authoritativeIdentityUrl(draft.create.identitySourceUrl)) {
      return { valid: false, message: "Provide an authoritative HTTPS product source URL." };
    }
    if (!nonBlank(draft.create.identitySourceTitle)
      || !nonBlank(draft.create.identityEvidenceText)) {
      return { valid: false, message: "Provide the source title and exact identity evidence." };
    }
    return {
      valid: false,
      message: "Verify this product now; after success it will become a selectable verified product.",
    };
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
    if (!nonBlank(draft.create.identitySourceTitle)) {
      return { valid: false, message: "An authoritative identity source title is required." };
    }
    if (!nonBlank(draft.create.identityEvidenceText)) {
      return { valid: false, message: "Quote the authoritative evidence for this identity." };
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
    || state.stale
    || !state.currentReview
    || decided !== total
    || !hashesPresent
    || !aircraftVerified;
}

async function resolveReview() {
  const review = state.currentReview;
  if (!review || state.stale || state.resolving) {
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
    await api(`/api/review/listings/${resolvedListingId}/resolve`, {
      method: "POST",
      body: JSON.stringify(request),
    });
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
      setWorkspaceMessage(`Listing ${resolvedListingId} verified. Loaded the next pending review.`);
    } else {
      showQueue({ historyMode: "replace", discardDraft: true });
      setQueueMessage(
        state.total === 0
          ? `Listing ${resolvedListingId} verified. The review queue is clear.`
          : `Listing ${resolvedListingId} verified.`,
      );
    }
  } catch (error) {
    if (isStaleError(error)) {
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

async function verifyExistingProduct(key, button) {
  const review = state.currentReview;
  const draft = state.drafts.get(key);
  const targetId = positiveInteger(draft?.aspect?.reuse_attestation_target?.id);
  if (!review || !draft || targetId === null || state.stale) {
    return;
  }
  if (!nonBlank(draft.create.identitySourceUrl)
    || !authoritativeIdentityUrl(draft.create.identitySourceUrl)
    || !nonBlank(draft.create.identitySourceTitle)
    || !nonBlank(draft.create.identityEvidenceText)) {
    setWorkspaceMessage(
      "Provide an authoritative source URL, title, and exact identity evidence before verification.",
      true,
    );
    return;
  }
  button.disabled = true;
  setWorkspaceMessage(`Grounding catalog product ${targetId} for this aspect only…`);
  try {
    const payload = await api(
      `/api/review/listings/${review.listing_id}/avionics/verify-existing`,
      {
        method: "POST",
        body: JSON.stringify({
          review_payload_sha256: review.review_payload_sha256,
          catalog_revision_sha256: review.catalog_revision_sha256,
          aspect_id: draft.aspect.id,
          identity_source_url: draft.create.identitySourceUrl.trim(),
          identity_source_title: draft.create.identitySourceTitle.trim(),
          identity_evidence_text: draft.create.identityEvidenceText.trim(),
        }),
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
      `Catalog product ${targetId} is freshly verified. Review the remaining aspects.`,
    );
  } catch (error) {
    if (isStaleError(error)) {
      markStale(error.message);
    } else {
      setWorkspaceMessage(`Could not verify this avionics product: ${error.message}`, true);
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
  if (draft.action === "verify_existing_product") {
    throw new Error("Existing products must be verified before resolving the listing.");
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
    verify_existing_product: "Verify source and keep product",
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
  if (action === "verify_existing_product") {
    return "Run the one-time source check for this exact approved product without reviewing any other aspect.";
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
  return error?.payload?.error?.code === "review_stale" || error?.status === 412;
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
  elements.reviewWorkspace.classList.add("is-hidden");
  elements.reviewQueueView.classList.remove("is-hidden");
  updateReviewLocation(null, historyMode);
  if (!state.queueLoaded) {
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

function authoritativeIdentityUrl(value) {
  try {
    const parsed = new URL(String(value || "").trim());
    if (parsed.protocol !== "https:") {
      return false;
    }
    const lower = parsed.href.toLowerCase();
    return ![
      "/listing/",
      "/listings/",
      "/aircraft-for-sale/",
      "/classifieds/",
    ].some((marker) => lower.includes(marker));
  } catch {
    return false;
  }
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

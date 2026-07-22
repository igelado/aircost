let api;
let finiteNumber;
let formatCurrency;
let formatDate;
let formatNumber;
let formatPercent;
let selectOption;
let setButtonBusy;

const state = {
  avionicsItems: [],
  avionicsTotal: 0,
  avionicsLimit: 20,
  avionicsOffset: 0,
  avionicsLoaded: false,
  avionicsOptionsLoaded: false,
  avionicsRequestSequence: 0,
  avionicsDetailRequestSequence: 0,
  avionicsSearchTimer: null,
  avionicsDetailTrigger: null,
};

const elements = {};
let initialized = false;

export function initializeAvionicsInspector(shared) {
  if (initialized) {
    throw new Error("The avionics inspector is already initialized.");
  }
  ({
    api,
    finiteNumber,
    formatCurrency,
    formatDate,
    formatNumber,
    formatPercent,
    selectOption,
    setButtonBusy,
  } = shared);
  collectElements();
  bindEvents();
  initialized = true;

  return Object.freeze({
    activate() {
      if (!state.avionicsLoaded) {
        return loadAvionicsWorkspace();
      }
      return Promise.resolve();
    },
    refresh() {
      cancelAvionicsSearch();
      state.avionicsOffset = 0;
      return loadAvionicsWorkspace(true);
    },
  });
}

function collectElements() {
  for (const [key, selector] of Object.entries({
    avionicsSearch: "#avionics-search",
    avionicsStatusFilter: "#avionics-status-filter",
    avionicsCapabilityFilter: "#avionics-capability-filter",
    avionicsCompletenessFilter: "#avionics-completeness-filter",
    refreshAvionics: "#refresh-avionics",
    avionicsMessage: "#avionics-message",
    avionicsResults: "#avionics-results",
    avionicsTableBody: "#avionics-table-body",
    emptyAvionics: "#empty-avionics",
    avionicsTotalCount: "#avionics-total-count",
    avionicsCompleteCount: "#avionics-complete-count",
    avionicsUsedCount: "#avionics-used-count",
    avionicsPageSummary: "#avionics-page-summary",
    avionicsPreviousPage: "#avionics-previous-page",
    avionicsNextPage: "#avionics-next-page",
    avionicsDetailDialog: "#avionics-detail-dialog",
    avionicsDetailTitle: "#avionics-detail-title",
    avionicsDetailSubtitle: "#avionics-detail-subtitle",
    avionicsDetailBody: "#avionics-detail-body",
    closeAvionicsDetail: "#close-avionics-detail",
  })) {
    elements[key] = document.querySelector(selector);
    if (!elements[key]) {
      throw new Error(`Missing avionics inspector element: ${selector}`);
    }
  }
}

function bindEvents() {
  elements.refreshAvionics.addEventListener("click", () => {
    cancelAvionicsSearch();
    state.avionicsOffset = 0;
    loadAvionicsWorkspace(true);
  });
  elements.avionicsSearch.addEventListener("input", scheduleAvionicsSearch);
  for (const filter of [
    elements.avionicsStatusFilter,
    elements.avionicsCapabilityFilter,
    elements.avionicsCompletenessFilter,
  ]) {
    filter.addEventListener("change", () => {
      cancelAvionicsSearch();
      state.avionicsOffset = 0;
      loadAvionics();
    });
  }
  elements.avionicsPreviousPage.addEventListener("click", () => {
    cancelAvionicsSearch();
    state.avionicsOffset = Math.max(0, state.avionicsOffset - state.avionicsLimit);
    loadAvionics();
  });
  elements.avionicsNextPage.addEventListener("click", () => {
    cancelAvionicsSearch();
    if (state.avionicsOffset + state.avionicsLimit < state.avionicsTotal) {
      state.avionicsOffset += state.avionicsLimit;
      loadAvionics();
    }
  });
  elements.avionicsTableBody.addEventListener("click", handleAvionicsTableClick);
  elements.closeAvionicsDetail.addEventListener("click", closeAvionicsDetail);
  elements.avionicsDetailDialog.addEventListener("click", (event) => {
    if (event.target === elements.avionicsDetailDialog) {
      closeAvionicsDetail();
    }
  });
  elements.avionicsDetailDialog.addEventListener("close", () => {
    state.avionicsDetailRequestSequence += 1;
    state.avionicsDetailTrigger?.focus();
    state.avionicsDetailTrigger = null;
  });
}

async function loadAvionicsWorkspace(forceOptions = false) {
  if (forceOptions || !state.avionicsOptionsLoaded) {
    await loadAvionicsOptions();
  }
  await loadAvionics();
}

async function loadAvionicsOptions() {
  try {
    const payload = await api("/api/avionics/options");
    updateAvionicsFilterOptions(payload?.options || {});
    state.avionicsOptionsLoaded = true;
  } catch (error) {
    state.avionicsOptionsLoaded = false;
    setAvionicsMessage(`Catalog filters unavailable: ${error.message}`, true);
  }
}

function updateAvionicsFilterOptions(options) {
  const statuses = catalogFilterOptions(options.statuses);
  const capabilities = catalogFilterOptions(options.capabilities);
  replaceCatalogSelectOptions(
    elements.avionicsStatusFilter,
    "All statuses",
    statuses,
  );
  replaceCatalogSelectOptions(
    elements.avionicsCapabilityFilter,
    "All capabilities",
    capabilities,
  );
}

function catalogFilterOptions(raw) {
  const values = Array.isArray(raw) ? raw : [];
  const byValue = new Map();
  for (const entry of values) {
    const value = typeof entry === "string" || typeof entry === "number"
      ? String(entry)
      : String(entry?.value || "");
    if (!value || byValue.has(value)) {
      continue;
    }
    byValue.set(value, {
      value,
      label: typeof entry === "object" && entry?.label
        ? String(entry.label)
        : displayLabel(value),
      count: typeof entry === "object" ? finiteNumber(entry?.count) : Number.NaN,
    });
  }
  return Array.from(byValue.values()).sort((left, right) => left.label.localeCompare(right.label));
}

function replaceCatalogSelectOptions(select, allLabel, options) {
  const previous = select.value;
  select.replaceChildren(
    selectOption("", allLabel),
    ...options.map((option) => selectOption(
      option.value,
      Number.isFinite(option.count)
        ? `${option.label} (${formatNumber(option.count, 0)})`
        : option.label,
    )),
  );
  select.value = options.some((option) => option.value === previous) ? previous : "";
}

function scheduleAvionicsSearch() {
  cancelAvionicsSearch();
  state.avionicsSearchTimer = window.setTimeout(() => {
    state.avionicsSearchTimer = null;
    state.avionicsOffset = 0;
    loadAvionics();
  }, 250);
}

function cancelAvionicsSearch() {
  if (state.avionicsSearchTimer) {
    window.clearTimeout(state.avionicsSearchTimer);
    state.avionicsSearchTimer = null;
  }
}

async function loadAvionics() {
  const requestSequence = ++state.avionicsRequestSequence;
  setAvionicsLoading(true);
  setAvionicsMessage("Loading avionics catalog...");
  try {
    const query = new URLSearchParams({
      limit: String(state.avionicsLimit),
      offset: String(state.avionicsOffset),
    });
    appendQueryValue(query, "search", elements.avionicsSearch.value.trim());
    appendQueryValue(query, "status", elements.avionicsStatusFilter.value);
    appendQueryValue(query, "capability", elements.avionicsCapabilityFilter.value);
    appendQueryValue(query, "completeness", elements.avionicsCompletenessFilter.value);
    const payload = await api(`/api/avionics?${query}`);
    if (requestSequence !== state.avionicsRequestSequence) {
      return;
    }
    const page = payload?.catalog || payload;
    state.avionicsItems = Array.isArray(page?.items) ? page.items : [];
    state.avionicsTotal = nonnegativeInteger(page?.total, state.avionicsItems.length);
    state.avionicsLimit = positiveInteger(page?.limit, state.avionicsLimit);
    state.avionicsOffset = nonnegativeInteger(page?.offset, state.avionicsOffset);
    state.avionicsLoaded = true;
    renderAvionicsCatalog();
    setAvionicsMessage(
      state.avionicsTotal
        ? `${state.avionicsTotal} catalog ${state.avionicsTotal === 1 ? "entry" : "entries"} found.`
        : "No avionics match these filters.",
    );
  } catch (error) {
    if (requestSequence !== state.avionicsRequestSequence) {
      return;
    }
    state.avionicsItems = [];
    state.avionicsTotal = 0;
    state.avionicsLoaded = false;
    renderAvionicsCatalog();
    elements.emptyAvionics.classList.add("is-hidden");
    setAvionicsMessage(`Could not load avionics: ${error.message}`, true);
  } finally {
    if (requestSequence === state.avionicsRequestSequence) {
      setAvionicsLoading(false);
    }
  }
}

function appendQueryValue(query, key, value) {
  if (value) {
    query.set(key, value);
  }
}

function renderAvionicsCatalog() {
  elements.avionicsTableBody.replaceChildren(...state.avionicsItems.map(avionicsCatalogRow));
  elements.emptyAvionics.classList.toggle("is-hidden", state.avionicsItems.length > 0);
  elements.avionicsTotalCount.textContent = formatNumber(state.avionicsTotal, 0);
  elements.avionicsCompleteCount.textContent = formatNumber(
    state.avionicsItems.filter((item) => avionicsView(item).complete).length,
    0,
  );
  elements.avionicsUsedCount.textContent = formatNumber(
    state.avionicsItems.filter((item) => avionicsView(item).listingCount > 0).length,
    0,
  );
  renderAvionicsPagination();
}

function avionicsCatalogRow(item) {
  const view = avionicsView(item);
  const row = document.createElement("tr");
  const identity = document.createElement("td");
  identity.dataset.label = "Manufacturer / model";
  identity.className = "avionics-identity-cell";
  const name = document.createElement("strong");
  name.textContent = item.display_name || [view.manufacturer, view.model].filter(Boolean).join(" ") || `Avionics ${item.id ?? "-"}`;
  const meta = document.createElement("small");
  meta.textContent = item.introduced_year ? `Introduced ${item.introduced_year}` : `Catalog ID ${item.id ?? "-"}`;
  identity.append(name, meta);

  const identifier = document.createElement("td");
  identifier.dataset.label = "Identifier";
  identifier.className = "avionics-identifier-cell";
  const identifierValue = document.createElement("strong");
  identifierValue.textContent = displayValue(view.identifier);
  const identifierKind = document.createElement("small");
  identifierKind.textContent = displayLabel(view.identifierKind);
  identifier.append(identifierValue, identifierKind);

  const capabilities = document.createElement("td");
  capabilities.dataset.label = "Capabilities";
  capabilities.append(renderAvionicsChips(item.capabilities, 3));

  const status = document.createElement("td");
  status.dataset.label = "Status";
  status.className = "avionics-status-cell";
  status.append(catalogStatusPill(view.catalogStatus));
  const confidence = document.createElement("small");
  confidence.textContent = `Confidence ${formatConfidence(view.identityConfidence)}`;
  status.append(confidence);
  const blockers = avionicsBlockers(item);
  if (blockers.length) {
    const incomplete = document.createElement("small");
    incomplete.className = "catalog-incomplete";
    incomplete.textContent = `${blockers.length} completeness ${blockers.length === 1 ? "blocker" : "blockers"}`;
    status.append(incomplete);
  }

  const value = document.createElement("td");
  value.dataset.label = "Value";
  value.className = "avionics-value-cell";
  const installed = document.createElement("strong");
  installed.textContent = formatCurrency(view.installedValue, "USD");
  const replacement = document.createElement("small");
  replacement.textContent = view.replacementCost === null || view.replacementCost === undefined
    ? "Replacement -"
    : `Replacement ${formatCurrency(view.replacementCost, "USD")}`;
  value.append(installed, replacement);

  const usage = document.createElement("td");
  usage.dataset.label = "Usage";
  usage.className = "avionics-usage-cell";
  const listingUsage = document.createElement("strong");
  listingUsage.textContent = `${formatNumber(view.listingCount, 0)} observed`;
  const catalogUsage = document.createElement("small");
  catalogUsage.textContent = [
    `${formatNumber(view.eligibleListingCount, 0)} eligible`,
    `${formatNumber(view.defaultUsageCount, 0)} default`,
    `${formatNumber(view.referenceUsageCount, 0)} reference`,
    `${formatNumber(view.suiteUsageCount, 0)} suite`,
  ].join(" · ");
  usage.append(listingUsage, catalogUsage);

  const action = document.createElement("td");
  action.dataset.label = "Actions";
  const inspect = document.createElement("button");
  inspect.type = "button";
  inspect.className = "button catalog-inspect-button";
  inspect.textContent = "Inspect";
  inspect.dataset.avionicsId = String(item.id ?? "");
  inspect.setAttribute("aria-label", `Inspect ${name.textContent}`);
  inspect.disabled = item.id === null || item.id === undefined;
  action.append(inspect);

  row.append(identity, identifier, capabilities, status, value, usage, action);
  return row;
}

function renderAvionicsChips(values, limit = Number.POSITIVE_INFINITY) {
  const container = document.createElement("div");
  container.className = "catalog-chip-list";
  const normalized = normalizedTextValues(values);
  for (const value of normalized.slice(0, limit)) {
    const chip = document.createElement("span");
    chip.className = "catalog-chip";
    chip.textContent = value;
    container.append(chip);
  }
  if (normalized.length > limit) {
    const overflow = document.createElement("span");
    overflow.className = "catalog-chip catalog-chip-overflow";
    overflow.textContent = `+${normalized.length - limit}`;
    overflow.title = normalized.slice(limit).join(", ");
    container.append(overflow);
  }
  if (!normalized.length) {
    container.textContent = "-";
  }
  return container;
}

function normalizedTextValues(values) {
  if (!Array.isArray(values)) {
    return values ? [String(values)] : [];
  }
  return values
    .map((value) => {
      if (typeof value === "string" || typeof value === "number") {
        return String(value);
      }
      return value?.name || value?.capability || value?.type || value?.code || value?.model || "";
    })
    .filter(Boolean);
}

function avionicsBlockers(item) {
  return normalizedTextValues(item?.completeness?.blockers);
}

function avionicsView(item) {
  const blockers = avionicsBlockers(item);
  return {
    manufacturer: item?.manufacturer?.name,
    model: item?.name,
    identifierKind: item?.stable_identifier?.kind,
    identifier: item?.stable_identifier?.value,
    catalogStatus: item?.catalog?.status,
    identityConfidence: item?.catalog?.identity_confidence,
    installedValue: item?.valuation?.installed_contribution_usd,
    replacementCost: item?.valuation?.replacement_cost_usd,
    listingCount: nonnegativeInteger(item?.usage?.visible_listings, 0),
    eligibleListingCount: nonnegativeInteger(item?.usage?.valuation_eligible_listings, 0),
    defaultUsageCount: nonnegativeInteger(item?.usage?.legacy_defaults, 0),
    referenceUsageCount: nonnegativeInteger(item?.usage?.reference_configurations, 0),
    suiteUsageCount: nonnegativeInteger(item?.usage?.suite_relationships, 0),
    complete: typeof item?.completeness?.complete === "boolean"
      ? item.completeness.complete
      : blockers.length === 0,
  };
}

function catalogStatusPill(status) {
  const pill = document.createElement("span");
  pill.className = `status-pill catalog-status-${cssToken(status)}`;
  pill.textContent = displayLabel(status) || "Unknown";
  return pill;
}

function cssToken(value) {
  return String(value || "unknown").toLowerCase().replace(/[^a-z0-9-]+/g, "-");
}

function formatConfidence(value) {
  const numeric = finiteNumber(value);
  if (Number.isFinite(numeric)) {
    return formatPercent(numeric > 1 ? numeric / 100 : numeric, 0);
  }
  return displayLabel(value);
}

function renderAvionicsPagination() {
  const total = state.avionicsTotal;
  const start = total ? state.avionicsOffset + 1 : 0;
  const end = Math.min(state.avionicsOffset + state.avionicsItems.length, total);
  elements.avionicsPageSummary.textContent = total
    ? `${formatNumber(start, 0)}–${formatNumber(end, 0)} of ${formatNumber(total, 0)}`
    : "0 results";
  elements.avionicsPreviousPage.disabled = state.avionicsOffset <= 0;
  elements.avionicsNextPage.disabled = state.avionicsOffset + state.avionicsLimit >= total;
}

function setAvionicsLoading(loading) {
  elements.avionicsResults.setAttribute("aria-busy", String(loading));
  setButtonBusy(elements.refreshAvionics, loading);
  elements.avionicsPreviousPage.disabled = loading || state.avionicsOffset <= 0;
  elements.avionicsNextPage.disabled =
    loading || state.avionicsOffset + state.avionicsLimit >= state.avionicsTotal;
}

function setAvionicsMessage(message, isError = false) {
  elements.avionicsMessage.textContent = message;
  elements.avionicsMessage.classList.toggle("error", isError);
}

function nonnegativeInteger(value, fallback) {
  const numeric = Number.parseInt(value, 10);
  return Number.isInteger(numeric) && numeric >= 0 ? numeric : fallback;
}

function positiveInteger(value, fallback) {
  const numeric = Number.parseInt(value, 10);
  return Number.isInteger(numeric) && numeric > 0 ? numeric : fallback;
}

function handleAvionicsTableClick(event) {
  const button = event.target.closest("button[data-avionics-id]");
  if (!button) {
    return;
  }
  const id = Number.parseInt(button.dataset.avionicsId, 10);
  if (Number.isInteger(id)) {
    openAvionicsDetail(id, button);
  }
}

async function openAvionicsDetail(id, trigger) {
  const requestSequence = ++state.avionicsDetailRequestSequence;
  state.avionicsDetailTrigger = trigger || document.activeElement;
  const summary = state.avionicsItems.find((item) => Number(item.id) === id);
  elements.avionicsDetailTitle.textContent = summary?.display_name || `Avionics ${id}`;
  elements.avionicsDetailSubtitle.textContent = "Loading catalog identity and usage...";
  elements.avionicsDetailBody.replaceChildren(detailState("Loading avionics details..."));
  if (!elements.avionicsDetailDialog.open) {
    elements.avionicsDetailDialog.showModal();
  }
  elements.closeAvionicsDetail.focus();
  try {
    const payload = await api(`/api/avionics/${id}`);
    if (
      requestSequence !== state.avionicsDetailRequestSequence
      || !elements.avionicsDetailDialog.open
    ) {
      return;
    }
    const detail = payload?.avionics;
    if (!detail || typeof detail !== "object") {
      throw new Error("The server returned an empty avionics record.");
    }
    renderAvionicsDetail(detail);
    elements.avionicsDetailBody.focus();
  } catch (error) {
    if (
      requestSequence !== state.avionicsDetailRequestSequence
      || !elements.avionicsDetailDialog.open
    ) {
      return;
    }
    elements.avionicsDetailSubtitle.textContent = "Details unavailable";
    elements.avionicsDetailBody.replaceChildren(
      detailState(`Could not load avionics details: ${error.message}`, true),
    );
    elements.avionicsDetailBody.focus();
  }
}

function closeAvionicsDetail() {
  if (elements.avionicsDetailDialog.open) {
    elements.avionicsDetailDialog.close();
  }
}

function detailState(message, isError = false) {
  const stateElement = document.createElement("p");
  stateElement.className = `detail-state${isError ? " error" : ""}`;
  stateElement.setAttribute("role", isError ? "alert" : "status");
  stateElement.textContent = message;
  return stateElement;
}

function renderAvionicsDetail(detail) {
  const summary = detail.summary || {};
  const identity = detail.identity_evidence || {};
  const view = avionicsView(summary);
  elements.avionicsDetailTitle.textContent = summary.display_name || `Avionics ${summary.id ?? "-"}`;
  elements.avionicsDetailSubtitle.textContent = [
    displayLabel(view.catalogStatus),
    `Catalog ID ${summary.id ?? "-"}`,
  ].filter(Boolean).join(" · ");

  const sourceLink = safeDetailLink(
    identity.source_url,
    identity.source_title || identity.source_url,
  );
  const overview = detailDefinitionSection("Catalog record", [
    ["Manufacturer", view.manufacturer],
    ["Model", view.model],
    ["Catalog status", displayLabel(view.catalogStatus)],
    ["Identity confidence", formatConfidence(view.identityConfidence)],
    ["Introduced", summary.introduced_year],
    ["Discontinued", summary.discontinued_year],
    ["Identifier kind", displayLabel(view.identifierKind)],
    ["Manufacturer identifier", view.identifier],
    ["Reviewed", formatDate(summary.catalog?.reviewed_at || identity.reviewed_at)],
    ["Identity source", sourceLink || identity.source_title],
  ]);

  const capabilities = detailSection("Capabilities");
  capabilities.append(renderAvionicsChips(summary.capabilities));

  const valuation = detailDefinitionSection("Valuation", [
    ["Installed contribution", formatCurrency(summary.valuation?.installed_contribution_usd, "USD")],
    ["Replacement cost", formatCurrency(summary.valuation?.replacement_cost_usd, "USD")],
    ["Reference year", summary.valuation?.reference_year],
    ["Basis", displayLabel(summary.valuation?.basis)],
    ["Scope", displayLabel(summary.valuation?.scope)],
    ["Source", summary.valuation?.source],
  ]);

  const evidence = detailSection("Identity evidence");
  const evidenceBody = document.createElement("p");
  evidenceBody.className = "detail-evidence";
  evidenceBody.textContent = identity.evidence_text || "No identity evidence recorded.";
  evidence.append(evidenceBody);
  evidence.append(detailDefinitionList([
    ["Evidence kind", displayLabel(identity.evidence_kind)],
    ["Confidence", formatConfidence(identity.confidence)],
  ]));

  const completeness = detailSection("Completeness");
  const blockers = avionicsBlockers(summary);
  if (blockers.length) {
    const list = document.createElement("ul");
    list.className = "detail-list";
    for (const blocker of blockers) {
      const item = document.createElement("li");
      item.textContent = displayLabel(blocker);
      list.append(item);
    }
    completeness.append(list);
  } else {
    completeness.append(detailState("No completeness blockers recorded."));
  }

  const collections = [
    detailCollectionSection("Suite components", detail.suite_components, describeSuiteComponent),
    detailCollectionSection("Suite memberships", detail.suite_memberships, describeSuiteMembership),
    detailCollectionSection("Listing occurrences", detail.listing_occurrences, describeListingOccurrence),
    detailCollectionSection("Legacy default configurations", detail.legacy_defaults, describeLegacyDefault),
    detailCollectionSection("Reference configurations", detail.reference_configurations, describeReferenceConfiguration),
  ];

  elements.avionicsDetailBody.replaceChildren(
    overview,
    capabilities,
    valuation,
    evidence,
    completeness,
    ...collections,
  );
}

function detailSection(title) {
  const section = document.createElement("section");
  section.className = "avionics-detail-section";
  const heading = document.createElement("h3");
  heading.textContent = title;
  section.append(heading);
  return section;
}

function detailDefinitionSection(title, entries) {
  const section = detailSection(title);
  section.append(detailDefinitionList(entries));
  return section;
}

function detailDefinitionList(entries) {
  const list = document.createElement("dl");
  list.className = "detail-definition-list";
  for (const [label, value] of entries) {
    const term = document.createElement("dt");
    term.textContent = label;
    const definition = document.createElement("dd");
    if (value instanceof Node) {
      definition.append(value);
    } else {
      definition.textContent = displayValue(value);
    }
    list.append(term, definition);
  }
  return list;
}

function detailCollectionSection(title, values, describe) {
  const section = detailSection(title);
  const items = Array.isArray(values) ? values : [];
  if (!items.length) {
    section.append(detailState("None recorded."));
    return section;
  }
  const list = document.createElement("ul");
  list.className = "detail-record-list";
  for (const value of items) {
    const description = describe(value || {});
    const item = document.createElement("li");
    const primary = document.createElement("strong");
    primary.textContent = description.primary || "Catalog usage";
    item.append(primary);
    if (description.secondary) {
      const secondary = document.createElement("span");
      secondary.textContent = description.secondary;
      item.append(secondary);
    }
    const link = safeDetailLink(description.url, "Open source");
    if (link) {
      item.append(link);
    }
    list.append(item);
  }
  section.append(list);
  return section;
}

function describeSuiteComponent(item) {
  return {
    primary: item.display_name || `Component ${item.model_id || "-"}`,
    secondary: detailMetadata([
      quantityMetadata(item.quantity),
      item.stable_identifier && `${displayLabel(item.stable_identifier.kind)} ${item.stable_identifier.value}`,
    ]),
  };
}

function describeSuiteMembership(item) {
  return {
    primary: item.display_name || `Suite ${item.model_id || "-"}`,
    secondary: detailMetadata([
      quantityMetadata(item.quantity),
      item.stable_identifier && `${displayLabel(item.stable_identifier.kind)} ${item.stable_identifier.value}`,
    ]),
  };
}

function describeListingOccurrence(item) {
  const aircraft = [
    item.registration_number,
    item.aircraft,
  ].filter(Boolean).join(" · ");
  const valuationBlockers = normalizedTextValues(item.valuation_blockers)
    .map(displayLabel)
    .join(", ");
  return {
    primary: aircraft || `Listing ${item.listing_id || "-"}`,
    secondary: detailMetadata([
      item.model_year,
      quantityMetadata(item.quantity),
      item.ingestion_state && `Ingestion ${displayLabel(item.ingestion_state)}`,
      item.valuation_eligible ? "Valuation eligible" : "Not valuation eligible",
      valuationBlockers && `Blocked by ${valuationBlockers}`,
      displayLabel(item.occurrence_role),
      displayLabel(item.configuration_action),
      item.is_verified ? "Verified listing" : "Unverified listing",
      item.serial_number && `Serial ${item.serial_number}`,
      item.source && `Source ${displayLabel(item.source)}`,
      item.source_notes,
      item.source_confidence && `Confidence ${displayLabel(item.source_confidence)}`,
      item.ingestion_error,
    ]),
    url: item.source_url,
  };
}

function describeLegacyDefault(item) {
  return {
    primary: item.aircraft || `Legacy default ${item.id || "-"}`,
    secondary: detailMetadata([
      item.model_year,
      quantityMetadata(item.quantity),
      item.source_title,
      item.source_notes,
      item.source_confidence && `Confidence ${displayLabel(item.source_confidence)}`,
    ]),
    url: item.source_url,
  };
}

function describeReferenceConfiguration(item) {
  const aircraft = [
    item.aircraft_make,
    item.aircraft_family,
    item.aircraft_designation,
    item.aircraft_generation,
    item.tier_package,
  ].filter(Boolean).join(" ");
  return {
    primary: item.display_name || aircraft || `Reference configuration ${item.configuration_id || "-"}`,
    secondary: detailMetadata([
      item.model_year,
      item.configuration_kind && displayLabel(item.configuration_kind),
      `Revision ${item.revision ?? "-"}`,
      item.publication_state && displayLabel(item.publication_state),
      item.equipment_role && displayLabel(item.equipment_role),
      quantityMetadata(item.quantity),
      item.evidence_validation_status && `Evidence ${displayLabel(item.evidence_validation_status)}`,
      item.evidence_source_title,
      item.evidence_source_tier && `Source tier ${displayLabel(item.evidence_source_tier)}`,
      item.immutable ? "Immutable" : "Mutable",
    ]),
    url: item.evidence_source_url,
  };
}

function quantityMetadata(quantity) {
  const numeric = finiteNumber(quantity);
  return Number.isFinite(numeric) ? `Quantity ${formatNumber(numeric, 0)}` : "";
}

function detailMetadata(values) {
  return values.filter(Boolean).map(String).join(" · ");
}

function safeDetailLink(value, label) {
  if (!value) {
    return null;
  }
  try {
    const url = new URL(String(value), window.location.origin);
    if (url.protocol !== "http:" && url.protocol !== "https:") {
      return null;
    }
    const link = document.createElement("a");
    link.href = url.href;
    link.textContent = label || url.href;
    link.target = "_blank";
    link.rel = "noopener noreferrer";
    return link;
  } catch (error) {
    return null;
  }
}

function displayLabel(value) {
  if (value === null || value === undefined || value === "") {
    return "-";
  }
  return String(value)
    .replaceAll("_", " ")
    .replace(/\b\w/g, (character) => character.toUpperCase());
}

function displayValue(value) {
  return value === null || value === undefined || value === "" ? "-" : String(value);
}

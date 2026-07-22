const USER_HEADER = "developer";
const VIEW_TITLES = {
  "listings-panel": ["Listings", "Sale listings and aircraft details"],
  "aircraft-panel": ["Aircraft", "Model parameters and depreciation curves"],
  "comparisons-panel": ["Comparisons", "Purchase, rental, and investment runs"],
  "rentals-panel": ["Rentals", "Club and rental aircraft profiles"],
};
const AVIONICS_TYPES = [
  "GPS",
  "NAV",
  "COM",
  "Transponder",
  "Autopilot",
  "Flight Director",
  "Integrated Flight Deck",
  "Audio Panel",
  "Flight Display",
  "Navigation Indicator",
  "Traffic",
  "Datalink",
  "Weather Radar",
  "Lightning Detection",
  "Terrain Awareness",
  "Engine Monitor",
  "Standby Instrument",
  "ELT",
  "ADF",
  "DME",
  "AHRS",
  "Air Data Computer",
  "Radar Altimeter",
  "Magnetometer",
  "Clock/Timer",
];
const SVG_NS = "http://www.w3.org/2000/svg";
const ICONS = {
  edit: ["M12 20h9", "M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z"],
  trash: ["M3 6h18", "M8 6V4h8v2", "M19 6l-1 14H6L5 6", "M10 11v5", "M14 11v5"],
  remove: ["M18 6 6 18", "M6 6l12 12"],
};

const state = {
  listings: [],
  aircraftOptions: [],
  aircraftDetail: null,
  aircraftAnnualHours: null,
  aircraftAnnualHoursVariantId: null,
  aircraftAnnualHoursTimer: null,
  editingListingId: null,
  valuationStatus: null,
};

const elements = {};

document.addEventListener("DOMContentLoaded", () => {
  collectElements();
  bindEvents();
  addAvionicsRow();
  loadValuationStatus();
  loadCurrentUser();
  loadListings();
  loadAircraftOptions();
});

function collectElements() {
  for (const [key, selector] of Object.entries({
    navTabs: ".nav-tab",
    viewPanels: ".view-panel",
    viewTitle: "#view-title",
    viewSubtitle: "#view-subtitle",
    currentUser: "#current-user",
    valuationStatus: "#valuation-status",
    listingSearch: "#listing-search",
    manufacturerFilter: "#manufacturer-filter",
    modelFilter: "#model-filter",
    variantFilter: "#variant-filter",
    statusFilter: "#status-filter",
    verifiedFilter: "#verified-filter",
    yearMinFilter: "#year-min-filter",
    yearMaxFilter: "#year-max-filter",
    priceMinFilter: "#price-min-filter",
    priceMaxFilter: "#price-max-filter",
    clearFilters: "#clear-filters",
    refreshListings: "#refresh-listings",
    newListing: "#new-listing",
    listingTableBody: "#listing-table-body",
    emptyListings: "#empty-listings",
    listMessage: "#list-message",
    visibleCount: "#visible-count",
    verifiedCount: "#verified-count",
    medianAsk: "#median-ask",
    listingDialog: "#listing-dialog",
    listingForm: "#listing-form",
    listingFormTitle: "#listing-form-title",
    formModeStatus: "#form-mode-status",
    closeListingDialog: "#close-listing-dialog",
    resetForm: "#reset-form",
    deleteListing: "#delete-listing",
    saveListing: "#save-listing",
    formMessage: "#form-message",
    avionicsList: "#avionics-list",
    addAvionics: "#add-avionics",
    aircraftManufacturer: "#aircraft-manufacturer",
    aircraftModel: "#aircraft-model",
    aircraftVariant: "#aircraft-variant",
    aircraftAnnualHours: "#aircraft-annual-hours",
    aircraftAnnualHoursValue: "#aircraft-annual-hours-value",
    refreshAircraft: "#refresh-aircraft",
    aircraftMessage: "#aircraft-message",
    aircraftParams: "#aircraft-params",
    aircraftChart: "#aircraft-value-chart",
    aircraftValueTableBody: "#aircraft-value-table-body",
    emptyAircraftValues: "#empty-aircraft-values",
  })) {
    elements[key] = selector.startsWith(".")
      ? Array.from(document.querySelectorAll(selector))
      : document.querySelector(selector);
  }
}

async function loadValuationStatus() {
  try {
    const payload = await api("/api/valuation/status");
    state.valuationStatus = payload.valuation || null;
    renderValuationStatus();
  } catch (error) {
    state.valuationStatus = {
      state: "unavailable",
      calibrated: false,
      warnings: [`Could not load valuation status: ${error.message}`],
    };
    renderValuationStatus();
  }
}

function renderValuationStatus() {
  const status = state.valuationStatus;
  const badge = elements.valuationStatus;
  badge.className = "valuation-status";
  if (!status || status.state === "unavailable") {
    badge.classList.add("is-unavailable");
    badge.textContent = "Valuation unavailable";
  } else if (status.state === "comparable_fallback") {
    badge.classList.add("is-comparable");
    badge.textContent = `Comparable fallback · snapshot ${status.snapshot_id ?? "-"}`;
  } else {
    badge.classList.add("is-calibrated");
    const version = status.model_version_id ? ` v${status.model_version_id}` : "";
    const snapshot = status.snapshot_id ? ` · snapshot ${status.snapshot_id}` : "";
    badge.textContent = `Calibrated ${status.model_kind || "model"}${version}${snapshot}`;
  }
  if ((status?.warnings || []).length) {
    badge.classList.add("has-warning");
    badge.textContent += " · warning";
  }
  badge.title = (status?.warnings || []).join("\n");
  if (state.aircraftDetail) {
    renderAircraftDetail();
  }
}

function bindEvents() {
  for (const tab of elements.navTabs) {
    tab.addEventListener("click", () => activatePanel(tab.dataset.panel));
  }
  elements.refreshListings.addEventListener("click", loadListings);
  elements.newListing.addEventListener("click", () => {
    resetListingForm();
    openListingDialog();
  });
  elements.resetForm.addEventListener("click", resetListingForm);
  elements.closeListingDialog.addEventListener("click", closeListingDialog);
  elements.listingDialog.addEventListener("click", (event) => {
    if (event.target === elements.listingDialog) {
      closeListingDialog();
    }
  });
  elements.manufacturerFilter.addEventListener("change", () => {
    populateModelFilter();
    populateVariantFilter();
    renderListings();
  });
  elements.modelFilter.addEventListener("change", () => {
    populateVariantFilter();
    renderListings();
  });
  for (const filter of [
    elements.listingSearch,
    elements.variantFilter,
    elements.statusFilter,
    elements.verifiedFilter,
    elements.yearMinFilter,
    elements.yearMaxFilter,
    elements.priceMinFilter,
    elements.priceMaxFilter,
  ]) {
    filter.addEventListener("input", renderListings);
    filter.addEventListener("change", renderListings);
  }
  elements.clearFilters.addEventListener("click", clearFilters);
  elements.addAvionics.addEventListener("click", () => addAvionicsRow());
  elements.listingForm.addEventListener("submit", saveListing);
  elements.deleteListing.addEventListener("click", deleteCurrentListing);
  elements.listingTableBody.addEventListener("click", handleTableClick);
  elements.refreshAircraft.addEventListener("click", loadAircraftOptions);
  elements.aircraftManufacturer.addEventListener("change", () => {
    resetAircraftAnnualHours();
    populateAircraftModelSelect();
    populateAircraftVariantSelect();
    loadSelectedAircraftDetail();
  });
  elements.aircraftModel.addEventListener("change", () => {
    resetAircraftAnnualHours();
    populateAircraftVariantSelect();
    loadSelectedAircraftDetail();
  });
  elements.aircraftVariant.addEventListener("change", () => {
    resetAircraftAnnualHours();
    loadSelectedAircraftDetail();
  });
  elements.aircraftAnnualHours.addEventListener("input", () => {
    state.aircraftAnnualHours = sliderAnnualHoursValue();
    renderAircraftAnnualHoursValue();
    scheduleSelectedAircraftDetailLoad();
  });
}

function activatePanel(panelId) {
  for (const tab of elements.navTabs) {
    tab.classList.toggle("is-active", tab.dataset.panel === panelId);
  }
  for (const panel of elements.viewPanels) {
    panel.classList.toggle("is-active", panel.id === panelId);
  }
  const [title, subtitle] = VIEW_TITLES[panelId] || VIEW_TITLES["listings-panel"];
  elements.viewTitle.textContent = title;
  elements.viewSubtitle.textContent = subtitle;
}

async function loadCurrentUser() {
  try {
    const payload = await api("/api/users/current");
    const user = payload.user;
    elements.currentUser.textContent = user.display_name || user.email || USER_HEADER;
  } catch (error) {
    elements.currentUser.textContent = USER_HEADER;
  }
}

async function loadListings() {
  setListMessage("Loading listings...");
  setButtonBusy(elements.refreshListings, true);
  try {
    const payload = await api("/api/listings");
    state.listings = payload.listings || [];
    populateFilterOptions();
    renderListings();
  } catch (error) {
    setListMessage(error.message, true);
  } finally {
    setButtonBusy(elements.refreshListings, false);
  }
}

async function loadAircraftOptions() {
  setAircraftMessage("Loading aircraft...");
  setButtonBusy(elements.refreshAircraft, true);
  try {
    const payload = await api("/api/aircraft/options");
    state.aircraftOptions = payload.options || [];
    populateAircraftManufacturerSelect();
    populateAircraftModelSelect();
    populateAircraftVariantSelect();
    await loadSelectedAircraftDetail();
  } catch (error) {
    state.aircraftOptions = [];
    state.aircraftDetail = null;
    clearAircraftDetail();
    setAircraftMessage(error.message, true);
  } finally {
    setButtonBusy(elements.refreshAircraft, false);
  }
}

async function loadSelectedAircraftDetail() {
  const variantId = selectedInteger(elements.aircraftVariant);
  if (!variantId) {
    state.aircraftDetail = null;
    clearAircraftDetail();
    return;
  }
  setAircraftMessage("Loading model...");
  try {
    const annualHours = selectedAircraftAnnualHoursForRequest(variantId);
    const query = Number.isFinite(annualHours) ? `?annual_hours=${annualHours}` : "";
    const payload = await api(`/api/aircraft/variants/${variantId}${query}`);
    state.aircraftDetail = payload.aircraft || null;
    syncAircraftAnnualHoursControl(state.aircraftDetail);
    renderAircraftDetail();
  } catch (error) {
    state.aircraftDetail = null;
    clearAircraftDetail();
    setAircraftMessage(error.message, true);
  }
}

function resetAircraftAnnualHours() {
  state.aircraftAnnualHours = null;
  state.aircraftAnnualHoursVariantId = null;
  if (state.aircraftAnnualHoursTimer) {
    window.clearTimeout(state.aircraftAnnualHoursTimer);
    state.aircraftAnnualHoursTimer = null;
  }
}

function scheduleSelectedAircraftDetailLoad() {
  if (state.aircraftAnnualHoursTimer) {
    window.clearTimeout(state.aircraftAnnualHoursTimer);
  }
  state.aircraftAnnualHoursTimer = window.setTimeout(() => {
    state.aircraftAnnualHoursTimer = null;
    loadSelectedAircraftDetail();
  }, 180);
}

function sliderAnnualHoursValue() {
  const value = Number(elements.aircraftAnnualHours.value);
  return Number.isFinite(value) ? value : null;
}

function selectedAircraftAnnualHoursForRequest(variantId) {
  if (state.aircraftAnnualHoursVariantId !== variantId) {
    return null;
  }
  return state.aircraftAnnualHours;
}

function syncAircraftAnnualHoursControl(detail) {
  const variantId = detail?.option?.variant_id || selectedInteger(elements.aircraftVariant);
  if (state.aircraftAnnualHoursVariantId !== variantId || state.aircraftAnnualHours === null) {
    state.aircraftAnnualHours = 200;
    state.aircraftAnnualHoursVariantId = variantId;
  }
  elements.aircraftAnnualHours.disabled = !detail?.spec;
  elements.aircraftAnnualHours.value = String(state.aircraftAnnualHours);
  renderAircraftAnnualHoursValue();
}

function renderAircraftAnnualHoursValue() {
  const value = sliderAnnualHoursValue();
  elements.aircraftAnnualHoursValue.textContent = Number.isFinite(value)
    ? formatUnit(value, "h", 0)
    : "-";
}

function populateAircraftManufacturerSelect() {
  replaceEntityOptions(
    elements.aircraftManufacturer,
    uniqueEntityOptions(state.aircraftOptions, "manufacturer_id", "manufacturer"),
    "Make",
  );
}

function populateAircraftModelSelect() {
  const manufacturerId = selectedInteger(elements.aircraftManufacturer);
  const options = state.aircraftOptions.filter(
    (option) => !manufacturerId || option.manufacturer_id === manufacturerId,
  );
  replaceEntityOptions(
    elements.aircraftModel,
    uniqueEntityOptions(options, "model_id", "model"),
    "Model",
  );
}

function populateAircraftVariantSelect() {
  const manufacturerId = selectedInteger(elements.aircraftManufacturer);
  const modelId = selectedInteger(elements.aircraftModel);
  const options = state.aircraftOptions.filter(
    (option) =>
      (!manufacturerId || option.manufacturer_id === manufacturerId) &&
      (!modelId || option.model_id === modelId),
  );
  replaceEntityOptions(
    elements.aircraftVariant,
    options.map((option) => ({
      value: option.variant_id,
      label: `${option.variant} (${option.listing_count})`,
    })),
    "Variant",
  );
}

function uniqueEntityOptions(items, idKey, labelKey) {
  const seen = new Map();
  for (const item of items) {
    if (!seen.has(item[idKey])) {
      seen.set(item[idKey], {
        value: item[idKey],
        label: item[labelKey],
      });
    }
  }
  return Array.from(seen.values()).sort((left, right) =>
    left.label.localeCompare(right.label),
  );
}

function replaceEntityOptions(select, options, emptyLabel) {
  const previous = select.value;
  if (!options.length) {
    select.replaceChildren(selectOption("", emptyLabel));
    select.value = "";
    select.disabled = true;
    return;
  }
  select.replaceChildren(
    ...options.map((option) => selectOption(String(option.value), option.label)),
  );
  select.disabled = false;
  select.value = options.some((option) => String(option.value) === previous)
    ? previous
    : String(options[0].value);
}

function renderAircraftDetail() {
  const detail = state.aircraftDetail;
  if (!detail) {
    clearAircraftDetail();
    return;
  }
  renderAircraftParams(detail);
  renderAircraftChart(detail);
  renderAircraftValueTable(detail);
  const listingOnly = (detail.listings || []).some((listing) => listing.valuation_model_kind);
  const valuationUnavailable = state.valuationStatus?.state === "unavailable";
  elements.aircraftAnnualHours.disabled = listingOnly || valuationUnavailable;
  elements.aircraftAnnualHours.title = valuationUnavailable
    ? "Market valuation is unavailable until an approved model or eligible snapshot is loaded."
    : listingOnly
      ? "Future utilization is learned from the frozen listing snapshot."
      : "Set projected annual airframe hours.";
  const listingCount = detail.listings?.length || 0;
  setAircraftMessage(detail.message || `${listingCount} listing values modeled.`);
}

function clearAircraftDetail() {
  elements.aircraftParams.replaceChildren();
  elements.aircraftValueTableBody.replaceChildren();
  elements.emptyAircraftValues.classList.remove("is-hidden");
  renderEmptyChart("No aircraft selected.");
}

function renderAircraftParams(detail) {
  const valuation = (detail.listings || []).find((listing) => listing.valuation_model_kind);
  if (valuation) {
    const breakdown = valuation.valuation_breakdown || {};
    elements.aircraftParams.replaceChildren(
      paramRow("Model", valuation.valuation_model_kind),
      paramRow(
        "Model version",
        valuation.valuation_model_version_id > 0
          ? String(valuation.valuation_model_version_id)
          : "snapshot fallback",
      ),
      paramRow("Snapshot", String(valuation.valuation_snapshot_id ?? "-")),
      paramRow("Calibration", valuation.valuation_calibrated ? "Calibrated" : "Uncalibrated fallback"),
      paramRow("Support", titleCase(valuation.valuation_support || "low")),
      paramRow("Estimated range", formatEstimateRange(valuation)),
      paramRow("Global anchor", formatCurrency(breakdown.global_anchor_usd, "USD")),
      paramRow("Age factor", formatPercent(breakdown.age_factor, 1)),
      paramRow("Expected hours", formatUnit(breakdown.expected_airframe_hours, "h", 0)),
      paramRow("Hours factor", formatPercent(breakdown.hours_factor, 1)),
      paramRow("Manufacturer factor", formatPercent(breakdown.manufacturer_factor, 1)),
      paramRow("Model factor", formatPercent(breakdown.model_factor, 1)),
      paramRow("Variant factor", formatPercent(breakdown.variant_factor, 1)),
    );
    return;
  }
  if (state.valuationStatus?.state === "unavailable") {
    elements.aircraftParams.replaceChildren(
      paramRow("Valuation", "Unavailable"),
      paramRow("Reason", state.valuationStatus.warnings?.at(-1) || "No approved serving model"),
    );
    return;
  }
  const spec = detail.spec;
  if (!spec) {
    elements.aircraftParams.replaceChildren(paramRow("Status", "Spec metadata missing"));
    return;
  }
  const profile = spec.depreciation_profile_detail || {};
  elements.aircraftParams.replaceChildren(
    paramRow("Spec scope", "Variant"),
    paramRow("Profile", spec.depreciation_profile),
    paramRow("Fit scope", formatFitScope(profile)),
    paramRow("Fit samples", profile.sample_count ?? "-"),
    paramRow("Fit MAE", formatPercent(profile.mae_fraction, 1)),
    paramRow("Age decay", formatPercent(profile.age_decay_rate, 1)),
    paramRow("Residual floor", formatPercent(profile.long_run_residual_fraction, 1)),
    paramRow("New-used discount", formatPercent(profile.new_to_used_discount_fraction, 1)),
    paramRow("Hour discount", formatPercent(profile.airframe_doubling_discount, 1)),
    paramRow("Low-time cap", formatPercent(profile.max_airframe_premium, 1)),
    paramRow("High-time cap", formatPercent(profile.max_airframe_discount, 1)),
    paramRow("High-time threshold", formatUnit(profile.high_time_threshold_hours, "h", 0)),
    paramRow(
      "Threshold discount",
      formatPercent(profile.high_time_discount_at_double_threshold, 1),
    ),
    paramRow("Inflation", formatPercent(spec.average_inflation_rate)),
    paramRow("Fuel burn", formatUnit(spec.fuel_burn_gph, "gph", 1)),
    paramRow("Oil burn", formatUnit(spec.oil_quarts_per_hour, "qt/hr", 2)),
    paramRow("Oil price", formatCurrency(spec.oil_price_per_quart_usd, "USD")),
    paramRow("Engine count", String(spec.engine_count)),
    paramRow("Engine TBO", formatUnit(spec.engine_tbo_hours, "h", 0)),
    paramRow("Engine overhaul", formatCurrency(spec.engine_overhaul_cost_usd, "USD")),
    paramRow("Prop count", String(spec.propeller_count)),
    paramRow("Prop TBO", formatUnit(spec.propeller_tbo_hours, "h", 0)),
    paramRow("Prop overhaul", formatCurrency(spec.propeller_overhaul_cost_usd, "USD")),
    paramRow("Annual inspection", formatCurrency(spec.annual_inspection_usd, "USD")),
    paramRow("Maintenance/hr", formatCurrency(spec.other_maintenance_per_hour, "USD")),
    paramRow("Effective", spec.effective_from || "-"),
  );
}

function paramRow(label, value) {
  const row = document.createElement("div");
  row.className = "param-row";
  const labelElement = document.createElement("span");
  labelElement.className = "param-label";
  labelElement.textContent = label;
  const valueElement = document.createElement("span");
  valueElement.className = "param-value";
  valueElement.textContent = value || "-";
  valueElement.title = value || "-";
  row.append(labelElement, valueElement);
  return row;
}

function renderListings() {
  const filters = readFilters();
  const rows = state.listings.filter((listing) => {
    const text = [
      listing.aircraft?.manufacturer,
      listing.aircraft?.model,
      listing.aircraft?.variant,
      listing.registration_number,
      listing.serial_number,
    ]
      .filter(Boolean)
      .join(" ")
      .toLowerCase();
    const year = Number(listing.model_year);
    const price = Number(listing.asking_price_usd);
    const manufacturer = listing.aircraft?.manufacturer || "";
    const model = listing.aircraft?.model || "";
    const variant = listing.aircraft?.variant || "";
    const statusMatches = filters.status === "all" || listing.status === filters.status;
    const manufacturerMatches =
      filters.manufacturer === "all" || manufacturer === filters.manufacturer;
    const modelMatches = filters.model === "all" || model === filters.model;
    const variantMatches = filters.variant === "all" || variant === filters.variant;
    const verifiedMatches =
      filters.verified === "all" ||
      (filters.verified === "verified" && listing.is_verified) ||
      (filters.verified === "unverified" && !listing.is_verified);
    const yearMatches =
      (!Number.isFinite(filters.yearMin) || year >= filters.yearMin) &&
      (!Number.isFinite(filters.yearMax) || year <= filters.yearMax);
    const priceMatches =
      (!Number.isFinite(filters.priceMin) || price >= filters.priceMin) &&
      (!Number.isFinite(filters.priceMax) || price <= filters.priceMax);
    return (
      statusMatches &&
      manufacturerMatches &&
      modelMatches &&
      variantMatches &&
      verifiedMatches &&
      yearMatches &&
      priceMatches &&
      (!filters.query || text.includes(filters.query))
    );
  });

  elements.listingTableBody.replaceChildren(...rows.map(listingRow));
  elements.emptyListings.classList.toggle("is-hidden", rows.length > 0);
  renderMetrics(rows);
  setListMessage(`${rows.length} of ${state.listings.length} listings visible.`);
}

function readFilters() {
  return {
    query: elements.listingSearch.value.trim().toLowerCase(),
    manufacturer: elements.manufacturerFilter.value,
    model: elements.modelFilter.value,
    variant: elements.variantFilter.value,
    status: elements.statusFilter.value,
    verified: elements.verifiedFilter.value,
    yearMin: optionalNumber(elements.yearMinFilter.value),
    yearMax: optionalNumber(elements.yearMaxFilter.value),
    priceMin: optionalNumber(elements.priceMinFilter.value),
    priceMax: optionalNumber(elements.priceMaxFilter.value),
  };
}

function populateFilterOptions() {
  replaceSelectOptions(
    elements.manufacturerFilter,
    "All makes",
    uniqueValues(state.listings.map((listing) => listing.aircraft?.manufacturer)),
  );
  populateModelFilter();
  populateVariantFilter();
}

function populateModelFilter() {
  const manufacturer = elements.manufacturerFilter.value;
  const listings = manufacturer === "all"
    ? state.listings
    : state.listings.filter((listing) => listing.aircraft?.manufacturer === manufacturer);
  replaceSelectOptions(
    elements.modelFilter,
    "All models",
    uniqueValues(listings.map((listing) => listing.aircraft?.model)),
  );
}

function populateVariantFilter() {
  const manufacturer = elements.manufacturerFilter.value;
  const model = elements.modelFilter.value;
  const listings = state.listings.filter((listing) => {
    const manufacturerMatches =
      manufacturer === "all" || listing.aircraft?.manufacturer === manufacturer;
    const modelMatches = model === "all" || listing.aircraft?.model === model;
    return manufacturerMatches && modelMatches;
  });
  replaceSelectOptions(
    elements.variantFilter,
    "All variants",
    uniqueValues(listings.map((listing) => listing.aircraft?.variant)),
  );
}

function replaceSelectOptions(select, allLabel, values) {
  const previous = select.value || "all";
  select.replaceChildren(selectOption("all", allLabel), ...values.map((value) => selectOption(value, value)));
  select.value = values.includes(previous) ? previous : "all";
}

function selectOption(value, label) {
  const option = document.createElement("option");
  option.value = value;
  option.textContent = label;
  return option;
}

function uniqueValues(values) {
  return Array.from(new Set(values.filter(Boolean))).sort((left, right) =>
    left.localeCompare(right),
  );
}

function clearFilters() {
  elements.listingSearch.value = "";
  elements.manufacturerFilter.value = "all";
  populateModelFilter();
  elements.modelFilter.value = "all";
  populateVariantFilter();
  elements.variantFilter.value = "all";
  elements.statusFilter.value = "all";
  elements.verifiedFilter.value = "all";
  elements.yearMinFilter.value = "";
  elements.yearMaxFilter.value = "";
  elements.priceMinFilter.value = "";
  elements.priceMaxFilter.value = "";
  renderListings();
}

function listingRow(listing) {
  const row = document.createElement("tr");

  const aircraftCell = document.createElement("td");
  aircraftCell.className = "aircraft-cell";
  const aircraftName = document.createElement("strong");
  aircraftName.textContent = [
    listing.aircraft?.manufacturer,
    listing.aircraft?.variant || listing.aircraft?.model,
  ]
    .filter(Boolean)
    .join(" ");
  aircraftCell.title = [
    listing.aircraft?.manufacturer,
    listing.aircraft?.model,
    listing.aircraft?.variant,
  ]
    .filter(Boolean)
    .join(" / ");
  aircraftCell.append(aircraftName);

  row.append(
    aircraftCell,
    textCell(listing.registration_number || "-"),
    textCell(String(listing.model_year || "-")),
    textCell(formatHours(listing.airframe_hours)),
    textCell(formatCurrency(listing.asking_price_usd, listing.currency)),
    statusCell(
      listing.status,
      listing.is_verified,
      listing.ingestion_state,
      listing.ingestion_error,
    ),
    textCell(formatDate(listing.added_at)),
    actionsCell(listing),
  );

  return row;
}

function textCell(value) {
  const cell = document.createElement("td");
  cell.textContent = value;
  return cell;
}

function statusCell(status, verified, ingestionState, ingestionError) {
  const cell = document.createElement("td");
  cell.className = "listing-status-cell";
  const stack = document.createElement("div");
  stack.className = "status-stack";
  const pill = document.createElement("span");
  pill.className = `status-pill ${status || "unknown"}${verified ? " verified" : ""}`;
  pill.textContent = status || "unknown";
  pill.title = verified ? `${status || "unknown"} verified` : status || "unknown";
  stack.append(pill);

  const ingestionPill = document.createElement("span");
  const normalizedState = ingestionState || "unknown";
  ingestionPill.className = `ingestion-pill ${normalizedState}`;
  ingestionPill.textContent = normalizedState;
  ingestionPill.title = ingestionError
    ? `Ingestion ${normalizedState}: ${ingestionError}`
    : `Ingestion ${normalizedState}`;
  stack.append(ingestionPill);
  cell.append(stack);
  return cell;
}

function actionsCell(listing) {
  const cell = document.createElement("td");
  const actions = document.createElement("div");
  actions.className = "row-actions";

  const edit = document.createElement("button");
  edit.className = "icon-button";
  edit.type = "button";
  edit.title = "Edit";
  edit.setAttribute("aria-label", "Edit listing");
  edit.append(iconSvg("edit"));
  edit.dataset.action = "edit";
  edit.dataset.id = String(listing.id);
  edit.disabled = listing.is_verified;

  const remove = document.createElement("button");
  remove.className = "icon-button danger";
  remove.type = "button";
  remove.title = "Delete";
  remove.setAttribute("aria-label", "Delete listing");
  remove.append(iconSvg("trash"));
  remove.dataset.action = "delete";
  remove.dataset.id = String(listing.id);
  remove.disabled = listing.is_verified;

  actions.append(edit, remove);
  cell.append(actions);
  return cell;
}

function renderMetrics(rows) {
  elements.visibleCount.textContent = String(rows.length);
  elements.verifiedCount.textContent = String(rows.filter((listing) => listing.is_verified).length);
  const prices = rows
    .map((listing) => Number(listing.asking_price_usd))
    .filter((value) => Number.isFinite(value))
    .sort((left, right) => left - right);
  const median = prices.length ? prices[Math.floor(prices.length / 2)] : 0;
  elements.medianAsk.textContent = formatCurrency(median, "USD");
}

function renderAircraftValueTable(detail) {
  const rows = detail.listings || [];
  elements.aircraftValueTableBody.replaceChildren(
    ...rows.map((listing) => aircraftValueRow(listing, detail)),
  );
  elements.emptyAircraftValues.classList.toggle("is-hidden", rows.length > 0);
}

function aircraftValueRow(listing, detail) {
  const row = document.createElement("tr");
  const age = listingAgeYears(listing, detail);
  const estimate = finiteNumber(listing.estimated_value_usd);
  const ask = finiteNumber(listing.asking_price_usd);
  const delta = Number.isFinite(estimate) && Number.isFinite(ask) ? ask - estimate : Number.NaN;
  row.append(
    textCell(listing.registration_number || listing.serial_number || "-"),
    textCell(String(listing.model_year || "-")),
    textCell(Number.isFinite(age) ? `${formatNumber(age, 1)} yr` : "-"),
    textCell(formatHours(listing.airframe_hours)),
    textCell(formatCurrency(listing.asking_price_usd, listing.currency)),
    estimateCell(listing),
    deltaCell(delta),
  );
  row.title = listing.estimate_error || "";
  return row;
}

function estimateCell(listing) {
  if (Number.isFinite(finiteNumber(listing.estimated_value_usd))) {
    const cell = document.createElement("td");
    cell.className = "estimate-cell";
    const value = document.createElement("strong");
    value.textContent = formatCurrency(listing.estimated_value_usd, "USD");
    cell.append(value);
    if (
      Number.isFinite(finiteNumber(listing.estimated_value_low_usd)) &&
      Number.isFinite(finiteNumber(listing.estimated_value_high_usd))
    ) {
      const range = document.createElement("small");
      range.textContent = formatEstimateRange(listing);
      cell.append(range);
    }
    if (listing.valuation_support) {
      const support = document.createElement("span");
      support.className = `support-pill support-${listing.valuation_support}`;
      support.textContent = titleCase(listing.valuation_support);
      support.title = `${titleCase(listing.valuation_support)} comparable support`;
      cell.append(support);
    }
    return cell;
  }
  const unavailable = listing.estimate_error?.includes("Listing-only valuation unavailable");
  const cell = textCell(listing.estimate_error ? (unavailable ? "Unavailable" : "Error") : "-");
  if (listing.estimate_error) {
    cell.className = "estimate-error";
    cell.title = listing.estimate_error;
  }
  return cell;
}

function deltaCell(delta) {
  const cell = document.createElement("td");
  cell.textContent = formatSignedCurrency(delta);
  if (Number.isFinite(delta)) {
    cell.className = delta >= 0 ? "delta-positive" : "delta-negative";
  }
  return cell;
}

function renderAircraftChart(detail) {
  const svg = elements.aircraftChart;
  svg.replaceChildren();
  const listings = detail.listings || [];
  const curvePoints = listings.flatMap((listing) =>
    (listing.value_curve || [])
      .map((point) => ({
        listing,
        age: finiteNumber(point.age_years),
        year: finiteNumber(point.valuation_year),
        value: finiteNumber(point.estimated_value_usd),
      }))
      .filter(
        (point) =>
          Number.isFinite(point.year) &&
          Number.isFinite(point.value) &&
          Number.isFinite(point.age),
      ),
  );
  const valuationYear = aircraftValuationYear(detail);
  const listingOnly = listings.some((listing) => listing.valuation_model_kind);
  const askPoints = listings
    .map((listing) => ({
      listing,
      age: listingAgeYears(listing, detail),
      year: listingObservationYear(listing, detail),
      value: finiteNumber(listing.asking_price_usd),
    }))
    .filter((point) => Number.isFinite(point.year) && Number.isFinite(point.value));
  const estimatePoints = listings
    .map((listing) => ({
      listing,
      age: listingAgeYears(listing, detail),
      year: listingObservationYear(listing, detail),
      value: finiteNumber(listing.estimated_value_usd),
    }))
    .filter((point) => Number.isFinite(point.year) && Number.isFinite(point.value));
  const maxValue = Math.max(
    0,
    ...curvePoints.map((point) => point.value),
    ...askPoints.map((point) => point.value),
    ...estimatePoints.map((point) => point.value),
  );
  if (!maxValue || !askPoints.length) {
    renderEmptyChart(detail.message || "No chart data available.");
    return;
  }

  const width = 920;
  const height = 360;
  const margin = { top: 24, right: 26, bottom: 42, left: 76 };
  const plotWidth = width - margin.left - margin.right;
  const plotHeight = height - margin.top - margin.bottom;
  const xMin = Math.min(
    ...curvePoints.map((point) => point.year),
    ...askPoints.map((point) => point.year),
    ...estimatePoints.map((point) => point.year),
    ...(listingOnly
      ? []
      : listings.map((listing) => finiteNumber(listing.model_year)).filter(Number.isFinite)),
    valuationYear,
  );
  const xMax = Math.max(
    valuationYear + 30,
    ...curvePoints.map((point) => point.year),
    ...askPoints.map((point) => point.year),
    ...estimatePoints.map((point) => point.year),
  );
  const yMin = 0;
  const yMax = niceMax(maxValue * 1.08);
  const xScale = (year) => margin.left + ((year - xMin) / (xMax - xMin)) * plotWidth;
  const yScale = (value) => margin.top + plotHeight - ((value - yMin) / (yMax - yMin)) * plotHeight;

  drawGrid(svg, width, height, margin, plotWidth, plotHeight, xScale, yScale, xMin, xMax, yMax);

  for (const listing of listings) {
    const points = (listing.value_curve || [])
      .map((point) => ({
        year: finiteNumber(point.valuation_year),
        value: finiteNumber(point.estimated_value_usd),
      }))
      .filter((point) => Number.isFinite(point.year) && Number.isFinite(point.value))
      .map((point) => [xScale(point.year), yScale(point.value)]);
    if (points.length > 1) {
      svg.append(svgPath(points, "chart-curve chart-curve-listing"));
    }
  }
  for (const askPoint of askPoints) {
    const estimatePoint = estimatePoints.find(
      (point) => point.listing.listing_id === askPoint.listing.listing_id,
    );
    if (estimatePoint) {
      svg.append(
        svgLine(
          xScale(askPoint.year),
          yScale(askPoint.value),
          xScale(estimatePoint.year),
          yScale(estimatePoint.value),
          "chart-connector",
        ),
      );
    }
  }

  for (const point of askPoints) {
    svg.append(chartCircle(xScale(point.year), yScale(point.value), 5, "chart-dot-ask", chartPointLabel(point, "Ask")));
  }
  for (const point of estimatePoints) {
    svg.append(
      chartCircle(
        xScale(point.year),
        yScale(point.value),
        4.5,
        "chart-dot-estimate",
        chartPointLabel(point, "Estimate"),
      ),
    );
  }

  drawLegend(svg, margin.left + 8, margin.top + 8);
}

function formatEstimateRange(listing) {
  const low = finiteNumber(listing.estimated_value_low_usd);
  const high = finiteNumber(listing.estimated_value_high_usd);
  if (!Number.isFinite(low) || !Number.isFinite(high)) {
    return "-";
  }
  return `${formatCurrency(low, "USD")} – ${formatCurrency(high, "USD")}`;
}

function titleCase(value) {
  const text = String(value || "");
  return text ? `${text[0].toUpperCase()}${text.slice(1)}` : "";
}

function renderEmptyChart(message) {
  const svg = elements.aircraftChart;
  svg.replaceChildren();
  const text = svgText(460, 180, message, "chart-empty");
  text.setAttribute("text-anchor", "middle");
  svg.append(text);
}

function drawGrid(svg, width, height, margin, plotWidth, plotHeight, xScale, yScale, xMin, xMax, yMax) {
  for (const year of yearTicks(xMin, xMax)) {
    const x = xScale(year);
    svg.append(svgLine(x, margin.top, x, margin.top + plotHeight, "chart-grid"));
    const label = svgText(x, height - 14, String(year), "chart-label");
    label.setAttribute("text-anchor", "middle");
    svg.append(label);
  }
  const yTicks = [0, 0.25, 0.5, 0.75, 1].map((fraction) => yMax * fraction);
  for (const value of yTicks) {
    const y = yScale(value);
    svg.append(svgLine(margin.left, y, margin.left + plotWidth, y, "chart-grid"));
    const label = svgText(margin.left - 10, y + 4, compactCurrency(value), "chart-label");
    label.setAttribute("text-anchor", "end");
    svg.append(label);
  }
  svg.append(svgLine(margin.left, margin.top, margin.left, margin.top + plotHeight, "chart-axis"));
  svg.append(
    svgLine(
      margin.left,
      margin.top + plotHeight,
      margin.left + plotWidth,
      margin.top + plotHeight,
      "chart-axis",
    ),
  );
}

function drawLegend(svg, x, y) {
  svg.append(chartCircle(x, y, 5, "chart-dot-ask", ""));
  svg.append(svgText(x + 10, y + 4, "Ask", "chart-label"));
  svg.append(chartCircle(x + 72, y, 4.5, "chart-dot-estimate", ""));
  svg.append(svgText(x + 82, y + 4, "Estimate", "chart-label"));
}

function chartPointLabel(point, label) {
  const tail = point.listing.registration_number || point.listing.serial_number || `listing ${point.listing.listing_id}`;
  return `${label}: ${tail}, observed ${point.year}, age ${formatNumber(point.age, 1)}, ${formatCurrency(point.value, "USD")}`;
}

function handleTableClick(event) {
  const button = event.target.closest("button[data-action]");
  if (!button) {
    return;
  }
  const listing = state.listings.find((item) => item.id === Number(button.dataset.id));
  if (!listing) {
    return;
  }
  if (button.dataset.action === "edit") {
    editListing(listing);
  } else if (button.dataset.action === "delete") {
    deleteListing(listing);
  }
}

function editListing(listing) {
  state.editingListingId = listing.id;
  elements.listingFormTitle.textContent = `Edit listing ${listing.id}`;
  elements.formModeStatus.textContent = listing.is_verified
    ? "Verified listings cannot be changed here."
    : "Changes update this unverified listing.";
  elements.deleteListing.classList.toggle("is-hidden", listing.is_verified);
  elements.deleteListing.disabled = listing.is_verified;
  elements.saveListing.disabled = listing.is_verified;

  setField("manufacturer", listing.aircraft?.manufacturer);
  setField("model", listing.aircraft?.model);
  setField("variant", listing.aircraft?.variant);
  setField("model_year", listing.model_year);
  setField("asking_price_usd", listing.asking_price_usd);
  setField("currency", listing.currency || "USD");
  setField("status", listing.status || "active");
  setField("registration_number", listing.registration_number);
  setField("serial_number", listing.serial_number);
  setField("airframe_hours", listing.airframe_hours);
  setField("engine_hours", listing.engine_hours);
  setField("propeller_hours", listing.propeller_hours);

  elements.avionicsList.replaceChildren();
  const avionics = listing.avionics?.length ? listing.avionics : [{}];
  for (const item of avionics) {
    addAvionicsRow(item);
  }
  setFormMessage("");
  openListingDialog();
}

function resetListingForm() {
  state.editingListingId = null;
  elements.listingForm.reset();
  elements.listingFormTitle.textContent = "New aircraft";
  elements.formModeStatus.textContent = "Manual entries are saved as unverified.";
  elements.deleteListing.classList.add("is-hidden");
  elements.deleteListing.disabled = false;
  elements.saveListing.disabled = false;
  setField("currency", "USD");
  setField("status", "active");
  elements.avionicsList.replaceChildren();
  addAvionicsRow();
  setFormMessage("");
}

function openListingDialog() {
  if (!elements.listingDialog.open) {
    elements.listingDialog.showModal();
  }
  const firstInput = elements.listingForm.querySelector("input, select, textarea, button");
  firstInput?.focus();
}

function closeListingDialog() {
  if (elements.listingDialog.open) {
    elements.listingDialog.close();
  }
}

async function saveListing(event) {
  event.preventDefault();
  setFormMessage("Saving listing...");
  setButtonBusy(elements.saveListing, true);
  try {
    const listing = readListingForm();
    const isEditing = state.editingListingId !== null;
    const path = isEditing ? `/api/listings/${state.editingListingId}` : "/api/listings";
    const method = isEditing ? "PATCH" : "POST";
    const response = await api(path, {
      method,
      body: JSON.stringify({ listing }),
    });
    await loadListings();
    await refreshAircraftAfterEstimateResponse(response);
    resetListingForm();
    closeListingDialog();
    setListMessage(isEditing ? "Listing updated." : "Listing created.");
  } catch (error) {
    setFormMessage(error.message, true);
  } finally {
    setButtonBusy(elements.saveListing, false);
  }
}

async function deleteCurrentListing() {
  if (state.editingListingId === null) {
    return;
  }
  const listing = state.listings.find((item) => item.id === state.editingListingId);
  if (listing) {
    await deleteListing(listing);
  }
}

async function deleteListing(listing) {
  const label =
    listing.registration_number || listing.aircraft?.variant || listing.aircraft?.model || `listing ${listing.id}`;
  if (!window.confirm(`Delete ${label}?`)) {
    return;
  }
  if (elements.listingDialog.open) {
    setFormMessage("Deleting listing...");
  } else {
    setListMessage("Deleting listing...");
  }
  try {
    await api(`/api/listings/${listing.id}`, { method: "DELETE" });
    resetListingForm();
    closeListingDialog();
    await loadListings();
    await loadAircraftOptions();
    setListMessage("Listing deleted.");
  } catch (error) {
    if (elements.listingDialog.open) {
      setFormMessage(error.message, true);
    } else {
      setListMessage(error.message, true);
    }
  }
}

async function refreshAircraftAfterEstimateResponse(response) {
  const hasEstimateResult = Object.prototype.hasOwnProperty.call(response || {}, "listing_estimate");
  if (hasEstimateResult) {
    await loadAircraftOptions();
    return;
  }
  if (response?.listing) {
    await loadAircraftOptions();
  }
}

function readListingForm() {
  const data = new FormData(elements.listingForm);
  const listing = {
    manufacturer: requiredText(data, "manufacturer"),
    model: requiredText(data, "model"),
    variant: requiredText(data, "variant"),
    model_year: requiredInteger(data, "model_year"),
    asking_price_usd: requiredNumber(data, "asking_price_usd"),
    currency: requiredText(data, "currency").toUpperCase(),
    status: data.get("status") || "active",
    registration_number: optionalText(data, "registration_number"),
    serial_number: optionalText(data, "serial_number"),
    airframe_hours: requiredNumber(data, "airframe_hours"),
    engine_hours: requiredNumber(data, "engine_hours"),
    propeller_hours: requiredNumber(data, "propeller_hours"),
    avionics: readAvionicsRows(),
  };
  return listing;
}

function readAvionicsRows() {
  const rows = Array.from(elements.avionicsList.querySelectorAll(".avionics-row"));
  const avionics = [];
  for (const row of rows) {
    const manufacturer = row.querySelector('[name="avionics_manufacturer"]').value.trim();
    const model = row.querySelector('[name="avionics_model"]').value.trim();
    const types = Array.from(
      row.querySelector('[name="avionics_types"]').selectedOptions,
      (option) => option.value,
    );
    const quantity = Number.parseInt(row.querySelector('[name="avionics_quantity"]').value, 10) || 1;
    const hasAnyValue = manufacturer || model;
    if (!hasAnyValue) {
      continue;
    }
    if (!manufacturer || !model) {
      throw new Error("Avionics rows need manufacturer and model.");
    }
    if (!types.length) {
      throw new Error("Avionics rows need at least one capability.");
    }
    avionics.push({
      manufacturer,
      model,
      types,
      quantity: Math.max(quantity, 1),
    });
  }
  return avionics;
}

function addAvionicsRow(item = {}) {
  const row = document.createElement("div");
  row.className = "avionics-row";
  row.append(
    avionicsInput("avionics_manufacturer", "Manufacturer", item.manufacturer),
    avionicsInput("avionics_model", "Model", item.model),
    avionicsTypeSelect(item.types || item.avionics_types),
    avionicsInput("avionics_quantity", "Qty", item.quantity || 1, "number"),
    removeAvionicsButton(row),
  );
  elements.avionicsList.append(row);
}

function avionicsInput(name, placeholder, value = "", type = "text") {
  const input = document.createElement("input");
  input.name = name;
  input.placeholder = placeholder;
  input.value = value ?? "";
  input.type = type;
  if (type === "number") {
    input.min = "1";
    input.step = "1";
  }
  return input;
}

function avionicsTypeSelect(values = []) {
  const select = document.createElement("select");
  select.name = "avionics_types";
  select.multiple = true;
  select.size = 3;
  select.title = "Select one or more avionics capabilities";
  select.setAttribute("aria-label", "Avionics capabilities");
  const selectedTypes = new Set(Array.isArray(values) ? values : []);
  for (const type of AVIONICS_TYPES) {
    const option = document.createElement("option");
    option.value = type;
    option.textContent = type;
    option.selected = selectedTypes.has(type);
    select.append(option);
  }
  return select;
}

function removeAvionicsButton(row) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "icon-button";
  button.title = "Remove";
  button.setAttribute("aria-label", "Remove avionics");
  button.append(iconSvg("remove"));
  button.addEventListener("click", () => {
    row.remove();
    if (!elements.avionicsList.children.length) {
      addAvionicsRow();
    }
  });
  return button;
}

function iconSvg(name) {
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  for (const pathData of ICONS[name] || []) {
    const path = document.createElementNS(SVG_NS, "path");
    path.setAttribute("d", pathData);
    svg.append(path);
  }
  return svg;
}

function svgLine(x1, y1, x2, y2, className) {
  const line = document.createElementNS(SVG_NS, "line");
  line.setAttribute("x1", String(x1));
  line.setAttribute("y1", String(y1));
  line.setAttribute("x2", String(x2));
  line.setAttribute("y2", String(y2));
  line.setAttribute("class", className);
  return line;
}

function svgPath(points, className) {
  const path = document.createElementNS(SVG_NS, "path");
  const d = points
    .map(([x, y], index) => `${index === 0 ? "M" : "L"} ${x.toFixed(2)} ${y.toFixed(2)}`)
    .join(" ");
  path.setAttribute("d", d);
  path.setAttribute("class", className);
  return path;
}

function svgText(x, y, text, className) {
  const label = document.createElementNS(SVG_NS, "text");
  label.setAttribute("x", String(x));
  label.setAttribute("y", String(y));
  label.setAttribute("class", className);
  label.textContent = text;
  return label;
}

function chartCircle(x, y, radius, className, title) {
  const circle = document.createElementNS(SVG_NS, "circle");
  circle.setAttribute("cx", String(x));
  circle.setAttribute("cy", String(y));
  circle.setAttribute("r", String(radius));
  circle.setAttribute("class", className);
  if (title) {
    const titleElement = document.createElementNS(SVG_NS, "title");
    titleElement.textContent = title;
    circle.append(titleElement);
  }
  return circle;
}

async function api(path, options = {}) {
  const headers = {
    "Content-Type": "application/json",
    "X-User-Email": USER_HEADER,
    ...(options.headers || {}),
  };
  const response = await fetch(path, { ...options, headers });
  if (response.status === 204) {
    return null;
  }
  const payload = await response.json().catch(() => null);
  if (!response.ok) {
    const message = payload?.error?.message || `HTTP ${response.status}`;
    throw new Error(message);
  }
  return payload;
}

function setField(name, value) {
  const field = elements.listingForm.elements.namedItem(name);
  if (field) {
    field.value = value ?? "";
  }
}

function requiredText(data, name) {
  const value = optionalText(data, name);
  if (!value) {
    throw new Error(`${labelFor(name)} is required.`);
  }
  return value;
}

function optionalText(data, name) {
  const value = data.get(name);
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

function requiredNumber(data, name) {
  const value = Number.parseFloat(data.get(name));
  if (!Number.isFinite(value)) {
    throw new Error(`${labelFor(name)} is required.`);
  }
  return value;
}

function requiredInteger(data, name) {
  const value = Number.parseInt(data.get(name), 10);
  if (!Number.isInteger(value)) {
    throw new Error(`${labelFor(name)} is required.`);
  }
  return value;
}

function labelFor(name) {
  return name.replaceAll("_", " ");
}

function setButtonBusy(button, busy) {
  button.disabled = busy;
}

function setFormMessage(message, isError = false) {
  elements.formMessage.textContent = message;
  elements.formMessage.classList.toggle("error", isError);
}

function setListMessage(message, isError = false) {
  elements.listMessage.textContent = message;
  elements.listMessage.classList.toggle("error", isError);
}

function setAircraftMessage(message, isError = false) {
  elements.aircraftMessage.textContent = message;
  elements.aircraftMessage.classList.toggle("error", isError);
}

function optionalNumber(value) {
  const trimmed = String(value || "").trim();
  if (!trimmed) {
    return Number.NaN;
  }
  const parsed = Number.parseFloat(trimmed);
  return Number.isFinite(parsed) ? parsed : Number.NaN;
}

function finiteNumber(value) {
  if (value === null || value === undefined || value === "") {
    return Number.NaN;
  }
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : Number.NaN;
}

function selectedInteger(select) {
  const value = Number.parseInt(select.value, 10);
  return Number.isInteger(value) ? value : null;
}

function formatFitScope(profile) {
  if (!profile || !profile.fit_scope) {
    return "-";
  }
  const scope = String(profile.fit_scope);
  if (scope === "global") {
    return "Global";
  }
  if (scope === "model") {
    return `Model ${profile.fit_scope_key || ""}`.trim();
  }
  if (scope === "category") {
    return `Category ${profile.fit_scope_key || ""}`.trim();
  }
  return scope;
}

function aircraftValuationYear(detail) {
  const year = detail?.spec?.effective_from?.slice(0, 4);
  const parsed = Number.parseInt(year, 10);
  return Number.isInteger(parsed) ? parsed : new Date().getFullYear();
}

function listingAgeYears(listing, detail) {
  const modelYear = Number(listing.model_year);
  if (!Number.isFinite(modelYear)) {
    return Number.NaN;
  }
  return Math.max(0, aircraftValuationYear(detail) - modelYear);
}

function listingObservationYear(listing, detail) {
  const addedAt = typeof listing.added_at === "string" ? listing.added_at : "";
  const parsed = Number.parseInt(addedAt.slice(0, 4), 10);
  return Number.isInteger(parsed) ? parsed : aircraftValuationYear(detail);
}

function niceMax(value) {
  if (!Number.isFinite(value) || value <= 0) {
    return 1;
  }
  const exponent = Math.floor(Math.log10(value));
  const base = 10 ** exponent;
  const normalized = value / base;
  const step = normalized <= 2 ? 2 : normalized <= 5 ? 5 : 10;
  return step * base;
}

function yearTicks(minYear, maxYear) {
  const span = Math.max(1, maxYear - minYear);
  const step = span <= 35 ? 5 : span <= 80 ? 10 : 20;
  const first = Math.ceil(minYear / step) * step;
  const ticks = [];
  if (first > minYear) {
    ticks.push(minYear);
  }
  for (let year = first; year <= maxYear; year += step) {
    ticks.push(year);
  }
  if (!ticks.includes(maxYear)) {
    ticks.push(maxYear);
  }
  return ticks;
}

function formatCurrency(value, currency = "USD") {
  if (value === null || value === undefined || value === "") {
    return "-";
  }
  const numeric = Number(value);
  if (!Number.isFinite(numeric)) {
    return "-";
  }
  return new Intl.NumberFormat("en-US", {
    style: "currency",
    currency: currency || "USD",
    maximumFractionDigits: 0,
  }).format(numeric);
}

function formatSignedCurrency(value) {
  if (!Number.isFinite(value)) {
    return "-";
  }
  const formatted = formatCurrency(Math.abs(value), "USD");
  return `${value >= 0 ? "+" : "-"}${formatted}`;
}

function compactCurrency(value) {
  const numeric = Number(value);
  if (!Number.isFinite(numeric)) {
    return "-";
  }
  return new Intl.NumberFormat("en-US", {
    style: "currency",
    currency: "USD",
    notation: "compact",
    maximumFractionDigits: 1,
  }).format(numeric);
}

function formatNumber(value, maximumFractionDigits = 1) {
  if (value === null || value === undefined || value === "") {
    return "-";
  }
  const numeric = Number(value);
  if (!Number.isFinite(numeric)) {
    return "-";
  }
  return new Intl.NumberFormat("en-US", {
    maximumFractionDigits,
  }).format(numeric);
}

function formatPercent(value, maximumFractionDigits = 1) {
  const numeric = Number(value);
  if (!Number.isFinite(numeric)) {
    return "-";
  }
  return new Intl.NumberFormat("en-US", {
    style: "percent",
    maximumFractionDigits,
  }).format(numeric);
}

function formatUnit(value, suffix, maximumFractionDigits = 1) {
  const formatted = formatNumber(value, maximumFractionDigits);
  return formatted === "-" ? "-" : `${formatted} ${suffix}`;
}

function formatHours(value) {
  const numeric = Number(value);
  if (!Number.isFinite(numeric)) {
    return "-";
  }
  return `${new Intl.NumberFormat("en-US", { maximumFractionDigits: 1 }).format(numeric)} h`;
}

function formatDate(value) {
  if (!value) {
    return "-";
  }
  const isoValue = value.includes("T") ? value : value.replace(" ", "T");
  const date = new Date(isoValue);
  if (Number.isNaN(date.getTime())) {
    return value;
  }
  return new Intl.DateTimeFormat("en-US", {
    month: "short",
    day: "numeric",
    year: "numeric",
  }).format(date);
}

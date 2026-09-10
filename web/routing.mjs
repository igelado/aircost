const LISTING_STATUSES = new Set(["active", "pending", "sold", "unknown"]);
const VERIFIED_FILTERS = new Set(["verified", "unverified"]);
const REVIEW_FILTERS = new Set([
  "all",
  "manual",
  "aircraft",
  "avionics",
  "reference",
  "gemini",
]);
const REVIEW_AREAS = new Set(["aircraft", "avionics"]);
const COMPLETENESS_FILTERS = new Set(["complete", "incomplete"]);

export const DESTINATIONS = Object.freeze({
  listings: Object.freeze({
    panelId: "listings-panel",
    title: "Listings",
    subtitle: "Sale listings and aircraft details",
    documentTitle: "Listings · AirCost",
  }),
  values: Object.freeze({
    panelId: "values-panel",
    title: "Aircraft values",
    subtitle: "Model parameters and depreciation curves",
    documentTitle: "Aircraft values · AirCost",
  }),
  review: Object.freeze({
    panelId: "review-panel",
    title: "Review queue",
    subtitle: "Accept unambiguous evidence automatically; review only the residual exceptions",
    documentTitle: "Review queue · AirCost",
  }),
  catalog: Object.freeze({
    panelId: "catalog-panel",
    title: "Avionics catalog",
    subtitle: "Catalog identities, capabilities, values, and usage",
    documentTitle: "Avionics catalog · AirCost",
  }),
});

export function defaultRoute() {
  return { name: "listings", filters: listingFilters(new URLSearchParams()) };
}

export function parseRoute(value) {
  const url = routeSourceUrl(value);
  const hash = url.hash.startsWith("#") ? url.hash.slice(1) : url.hash;
  if (!hash.startsWith("/")) {
    return defaultRoute();
  }

  let routeUrl;
  try {
    routeUrl = new URL(hash, "https://aircost.invalid");
  } catch {
    return defaultRoute();
  }
  const segments = routeUrl.pathname.split("/").filter(Boolean);
  const params = routeUrl.searchParams;
  if (segments[0] === "listings") {
    if (segments.length > 2) {
      return { name: "listings", filters: listingFilters(params) };
    }
    const selected = segments[1];
    if (selected === undefined) {
      return { name: "listings", filters: listingFilters(params) };
    }
    if (selected === "new") {
      return { name: "listings", selected: "new", filters: listingFilters(params) };
    }
    const listingId = positiveInteger(selected);
    return listingId === null
      ? { name: "listings", filters: listingFilters(params) }
      : { name: "listings", listingId, filters: listingFilters(params) };
  }
  if (segments[0] === "values") {
    if (segments.length > 2) {
      return { name: "values" };
    }
    const variantId = segments.length === 2 ? positiveInteger(segments[1]) : null;
    if (segments.length === 2 && variantId === null) {
      return { name: "values" };
    }
    return variantId === null ? { name: "values" } : { name: "values", variantId };
  }
  if (segments[0] === "review") {
    return parseReviewRoute(segments, params);
  }
  if (segments[0] === "catalog") {
    if (segments.length > 2) {
      return { name: "catalog", filters: catalogFilters(params) };
    }
    const productId = segments.length === 2 ? positiveInteger(segments[1]) : null;
    if (segments.length === 2 && productId === null) {
      return { name: "catalog", filters: catalogFilters(params) };
    }
    const route = { name: "catalog", filters: catalogFilters(params) };
    if (productId !== null) {
      route.productId = productId;
    }
    return route;
  }
  return defaultRoute();
}

export function formatRoute(value) {
  const route = normalizeRoute(value);
  const params = new URLSearchParams();
  let path;
  if (route.name === "listings") {
    path = route.selected === "new"
      ? "/listings/new"
      : route.listingId
        ? `/listings/${route.listingId}`
        : "/listings";
    appendListingFilters(params, route.filters);
  } else if (route.name === "values") {
    path = route.variantId ? `/values/${route.variantId}` : "/values";
  } else if (route.name === "review") {
    if (route.view === "products") {
      path = route.productId ? `/review/products/${route.productId}` : "/review/products";
    } else if (route.view === "manual") {
      path = "/review/manual";
    } else if (route.view === "listing") {
      path = `/review/listings/${route.listingId}`;
      if (route.area) {
        params.set("area", route.area);
      }
    } else {
      path = "/review";
      if (route.search) {
        params.set("search", route.search);
      }
      if (route.filter !== "all") {
        params.set("filter", route.filter);
      }
    }
  } else {
    path = route.productId ? `/catalog/${route.productId}` : "/catalog";
    appendCatalogFilters(params, route.filters);
  }
  const query = params.toString();
  return `/#${path}${query ? `?${query}` : ""}`;
}

export function normalizeRoute(value) {
  const route = value && typeof value === "object" ? value : defaultRoute();
  if (route.name === "listings") {
    const normalized = {
      name: "listings",
      filters: normalizeListingFilters(route.filters),
    };
    if (route.selected === "new") {
      normalized.selected = "new";
    } else {
      const listingId = positiveInteger(route.listingId);
      if (listingId !== null) {
        normalized.listingId = listingId;
      }
    }
    return normalized;
  }
  if (route.name === "values") {
    const variantId = positiveInteger(route.variantId);
    return variantId === null ? { name: "values" } : { name: "values", variantId };
  }
  if (route.name === "review") {
    return normalizeReviewRoute(route);
  }
  if (route.name === "catalog") {
    const normalized = {
      name: "catalog",
      filters: normalizeCatalogFilters(route.filters),
    };
    const productId = positiveInteger(route.productId);
    if (productId !== null) {
      normalized.productId = productId;
    }
    return normalized;
  }
  return defaultRoute();
}

export function destinationForRoute(route) {
  return DESTINATIONS[normalizeRoute(route).name];
}

export function reviewListingIdForRoute(route) {
  const normalized = normalizeRoute(route);
  return normalized.name === "review" && normalized.view === "listing"
    ? normalized.listingId
    : null;
}

export function reviewAreaForRoute(route, fallback = null) {
  const normalized = normalizeRoute(route);
  return normalized.name === "review"
      && normalized.view === "listing"
      && normalized.area !== null
    ? normalized.area
    : fallback;
}

export function reviewMutationInProgress({
  productBatch = false,
  resolution = false,
  aspectSave = false,
  correctionSave = false,
  automation = false,
  associationValidation = false,
} = {}) {
  return productBatch
    || resolution
    || aspectSave
    || correctionSave
    || automation
    || associationValidation;
}

export function routeActivationIsCurrent(owner, current) {
  return owner === current;
}

export function preserveLiveRouteInput(source, focused) {
  return (source === "replace" || source === "refresh") && focused;
}

export function listingEditorRouteIsSame(current, next) {
  const owner = normalizeRoute(current);
  const candidate = normalizeRoute(next);
  if (owner.name !== "listings" || candidate.name !== "listings") {
    return false;
  }
  return owner.selected === "new"
    ? candidate.selected === "new"
    : owner.listingId !== undefined && candidate.listingId === owner.listingId;
}

export function reviewProductRouteIsSame(current, next) {
  const owner = normalizeRoute(current);
  const candidate = normalizeRoute(next);
  return owner.name === "review"
    && owner.view === "products"
    && owner.productId !== undefined
    && candidate.name === "review"
    && candidate.view === "products"
    && candidate.productId === owner.productId;
}

export function confirmDirtyRouteChange(dirty, sameOwner, confirmDiscard) {
  return !dirty || sameOwner || confirmDiscard();
}

export function reviewProductQueueNeedsLoad(route, hasCachedGroups) {
  const normalized = normalizeRoute(route);
  return normalized.name === "review"
    && normalized.view === "products"
    && (normalized.productId === undefined || !hasCachedGroups);
}

export function reviewProductFallbackForResult(result, owner, current) {
  if (result?.status !== "absent" || !routeActivationIsCurrent(owner, current)) {
    return null;
  }
  const route = normalizeRoute(current);
  return route.name === "review"
      && route.view === "products"
      && route.productId !== undefined
    ? { name: "review", view: "products" }
    : null;
}

export function catalogPageFallbackForResult(owner, current, total, limit) {
  if (!routeActivationIsCurrent(owner, current)) {
    return null;
  }
  const route = normalizeRoute(current);
  const resultTotal = Number(total);
  const resultLimit = positiveInteger(limit);
  if (
    route.name !== "catalog"
    || !Number.isSafeInteger(resultTotal)
    || resultTotal < 0
    || resultLimit === null
  ) {
    return null;
  }
  const lastPage = resultTotal === 0 ? 1 : Math.ceil(resultTotal / resultLimit);
  if (route.filters.page <= lastPage) {
    return null;
  }
  return {
    ...route,
    filters: {
      ...route.filters,
      page: lastPage,
    },
  };
}

export function reviewListingRouteOwner(route, generation) {
  const listingId = reviewListingIdForRoute(route);
  return listingId === null ? null : { generation, listingId };
}

export function reviewListingRouteOwnerIsCurrent(owner, route, generation) {
  return owner !== null
    && owner.generation === generation
    && reviewListingIdForRoute(route) === owner.listingId;
}

export function createHistoryRouter({ location, history, listen, apply, mayNavigate }) {
  if (!location || !history || typeof listen !== "function" || typeof apply !== "function") {
    throw new Error("The history router requires location, history, listen, and apply.");
  }
  const navigationAllowed = typeof mayNavigate === "function" ? mayNavigate : () => true;
  let current = null;
  let currentPosition = null;
  let suppressedPosition = null;

  function positionFromState(value) {
    if (
      value?.aircostRouterVersion !== 1
      || value.aircostRoute === null
      || typeof value.aircostRoute !== "object"
    ) {
      return null;
    }
    const position = value?.aircostPosition;
    return Number.isSafeInteger(position) && position >= 0 ? position : null;
  }

  function routeState(route, position) {
    return {
      aircostRouterVersion: 1,
      aircostRoute: route,
      aircostPosition: position,
    };
  }

  function write(route, mode, source) {
    const next = normalizeRoute(route);
    if (current !== null && !navigationAllowed(next, current, source)) {
      return false;
    }
    const method = mode === "replace" ? "replaceState" : "pushState";
    const nextPosition = mode === "replace" ? currentPosition : currentPosition + 1;
    history[method](routeState(next, nextPosition), "", formatRoute(next));
    current = next;
    currentPosition = nextPosition;
    return apply(next, { source });
  }

  function restore(event) {
    const next = parseRoute(location.href);
    let targetPosition = positionFromState(event?.state);
    if (targetPosition === null) {
      // Startup and every router-owned write are tagged. A state-less
      // same-document target is therefore a newly created fragment entry
      // adjacent to the active entry; adopt it without appending history.
      targetPosition = currentPosition + 1;
      history.replaceState(
        routeState(next, targetPosition),
        "",
        formatRoute(next),
      );
    }
    if (suppressedPosition !== null && targetPosition === suppressedPosition) {
      suppressedPosition = null;
      return true;
    }
    suppressedPosition = null;
    if (current !== null && !navigationAllowed(next, current, "popstate")) {
      if (
        targetPosition !== null
        && targetPosition !== currentPosition
        && typeof history.go === "function"
      ) {
        suppressedPosition = currentPosition;
        history.go(currentPosition - targetPosition);
      }
      return false;
    }
    const nextPosition = targetPosition ?? currentPosition;
    const canonicalUrl = formatRoute(next);
    if (`${location.pathname}${location.search}${location.hash}` !== canonicalUrl) {
      history.replaceState(routeState(next, nextPosition), "", canonicalUrl);
    }
    current = next;
    currentPosition = nextPosition;
    apply(next, { source: "popstate" });
    return true;
  }

  return Object.freeze({
    start() {
      const initial = parseRoute(location.href);
      current = initial;
      currentPosition = positionFromState(history.state) ?? 0;
      const canonicalUrl = formatRoute(initial);
      history.replaceState(routeState(initial, currentPosition), "", canonicalUrl);
      listen("popstate", restore);
      apply(initial, { source: "startup" });
      return initial;
    },
    navigate(route, { replace = false } = {}) {
      return write(route, replace ? "replace" : "push", replace ? "replace" : "push");
    },
    current() {
      return current;
    },
    restore,
  });
}

function parseReviewRoute(segments, params) {
  if (segments.length === 1) {
    return {
      name: "review",
      view: "pipeline",
      search: textParam(params, "search"),
      filter: enumParam(params, "filter", REVIEW_FILTERS, "all"),
    };
  }
  if (segments[1] === "products") {
    if (segments.length > 3) {
      return { name: "review", view: "products" };
    }
    const productId = segments.length === 3 ? positiveInteger(segments[2]) : null;
    if (segments.length === 3 && productId === null) {
      return { name: "review", view: "products" };
    }
    return productId === null
      ? { name: "review", view: "products" }
      : { name: "review", view: "products", productId };
  }
  if (segments[1] === "manual") {
    return { name: "review", view: "manual" };
  }
  if (segments[1] === "listings") {
    if (segments.length !== 3) {
      return { name: "review", view: "manual" };
    }
    const listingId = positiveInteger(segments[2]);
    if (listingId !== null) {
      return {
        name: "review",
        view: "listing",
        listingId,
        area: enumParam(params, "area", REVIEW_AREAS, null),
      };
    }
    return { name: "review", view: "manual" };
  }
  return {
    name: "review",
    view: "pipeline",
    search: textParam(params, "search"),
    filter: enumParam(params, "filter", REVIEW_FILTERS, "all"),
  };
}

function normalizeReviewRoute(route) {
  if (route.view === "products") {
    const productId = positiveInteger(route.productId);
    return productId === null
      ? { name: "review", view: "products" }
      : { name: "review", view: "products", productId };
  }
  if (route.view === "manual") {
    return { name: "review", view: "manual" };
  }
  if (route.view === "listing") {
    const listingId = positiveInteger(route.listingId);
    if (listingId === null) {
      return { name: "review", view: "manual" };
    }
    return {
      name: "review",
      view: "listing",
      listingId,
      area: REVIEW_AREAS.has(route.area) ? route.area : null,
    };
  }
  return {
    name: "review",
    view: "pipeline",
    search: cleanText(route.search),
    filter: REVIEW_FILTERS.has(route.filter) ? route.filter : "all",
  };
}

function listingFilters(params) {
  return normalizeListingFilters({
    search: textParam(params, "search"),
    manufacturer: textParam(params, "manufacturer"),
    model: textParam(params, "model"),
    variant: textParam(params, "variant"),
    status: enumParam(params, "status", LISTING_STATUSES, "all"),
    verified: enumParam(params, "verified", VERIFIED_FILTERS, "all"),
    yearMin: integerParam(params, "year_min"),
    yearMax: integerParam(params, "year_max"),
    priceMin: numberParam(params, "price_min"),
    priceMax: numberParam(params, "price_max"),
  });
}

function normalizeListingFilters(value = {}) {
  return {
    search: cleanText(value.search),
    manufacturer: cleanText(value.manufacturer),
    model: cleanText(value.model),
    variant: cleanText(value.variant),
    status: LISTING_STATUSES.has(value.status) ? value.status : "all",
    verified: VERIFIED_FILTERS.has(value.verified) ? value.verified : "all",
    yearMin: safeInteger(value.yearMin),
    yearMax: safeInteger(value.yearMax),
    priceMin: safeNumber(value.priceMin),
    priceMax: safeNumber(value.priceMax),
  };
}

function appendListingFilters(params, filters) {
  appendText(params, "search", filters.search);
  appendText(params, "manufacturer", filters.manufacturer);
  appendText(params, "model", filters.model);
  appendText(params, "variant", filters.variant);
  appendNonDefault(params, "status", filters.status, "all");
  appendNonDefault(params, "verified", filters.verified, "all");
  appendNumber(params, "year_min", filters.yearMin);
  appendNumber(params, "year_max", filters.yearMax);
  appendNumber(params, "price_min", filters.priceMin);
  appendNumber(params, "price_max", filters.priceMax);
}

function catalogFilters(params) {
  return normalizeCatalogFilters({
    search: textParam(params, "search"),
    status: textParam(params, "status"),
    capability: textParam(params, "capability"),
    completeness: enumParam(params, "completeness", COMPLETENESS_FILTERS, ""),
    page: positiveInteger(params.get("page")) ?? 1,
  });
}

function normalizeCatalogFilters(value = {}) {
  return {
    search: cleanText(value.search),
    status: cleanText(value.status),
    capability: cleanText(value.capability),
    completeness: COMPLETENESS_FILTERS.has(value.completeness) ? value.completeness : "",
    page: positiveInteger(value.page) ?? 1,
  };
}

function appendCatalogFilters(params, filters) {
  appendText(params, "search", filters.search);
  appendText(params, "status", filters.status);
  appendText(params, "capability", filters.capability);
  appendText(params, "completeness", filters.completeness);
  appendNonDefault(params, "page", filters.page, 1);
}

function routeSourceUrl(value) {
  if (value instanceof URL) {
    return value;
  }
  try {
    return new URL(String(value ?? ""), "https://aircost.invalid/");
  } catch {
    return new URL("https://aircost.invalid/");
  }
}

function textParam(params, key) {
  return cleanText(params.get(key));
}

function enumParam(params, key, allowed, fallback) {
  const value = textParam(params, key);
  return allowed.has(value) ? value : fallback;
}

function integerParam(params, key) {
  const value = params.get(key);
  if (value === null || !/^-?\d+$/.test(value)) {
    return null;
  }
  return safeInteger(Number(value));
}

function numberParam(params, key) {
  const value = params.get(key);
  return value === null || value.trim() === "" ? null : safeNumber(Number(value));
}

function positiveInteger(value) {
  if (value === null || value === undefined || value === "") {
    return null;
  }
  if (typeof value === "string" && !/^\d+$/.test(value)) {
    return null;
  }
  const number = Number(value);
  return Number.isSafeInteger(number) && number > 0 ? number : null;
}

function safeInteger(value) {
  if (value === null || value === undefined || value === "") {
    return null;
  }
  const number = Number(value);
  return Number.isSafeInteger(number) ? number : null;
}

function safeNumber(value) {
  if (value === null || value === undefined || value === "") {
    return null;
  }
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

function cleanText(value) {
  return value === null || value === undefined ? "" : String(value).trim();
}

function appendText(params, key, value) {
  if (value) {
    params.set(key, value);
  }
}

function appendNonDefault(params, key, value, fallback) {
  if (value !== fallback) {
    params.set(key, String(value));
  }
}

function appendNumber(params, key, value) {
  if (value !== null) {
    params.set(key, String(value));
  }
}

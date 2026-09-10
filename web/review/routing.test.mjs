import assert from "node:assert/strict";
import test from "node:test";

import {
  DESTINATIONS,
  catalogPageFallbackForResult,
  createHistoryRouter,
  formatRoute,
  parseRoute,
  preserveLiveRouteInput,
  reviewAreaForRoute,
  reviewListingIdForRoute,
  reviewListingRouteOwner,
  reviewListingRouteOwnerIsCurrent,
  reviewMutationInProgress,
  reviewProductFallbackForResult,
  reviewProductQueueNeedsLoad,
  routeActivationIsCurrent,
} from "../routing.mjs";

test("publishes the four frozen task destinations and panel IDs", () => {
  assert.deepEqual(
    Object.fromEntries(Object.entries(DESTINATIONS).map(([name, value]) => [name, value.panelId])),
    {
      listings: "listings-panel",
      values: "values-panel",
      review: "review-panel",
      catalog: "catalog-panel",
    },
  );
});

test("round trips every addressable task and selected record", () => {
  const urls = [
    "/#/listings",
    "/#/listings/new?manufacturer=Beechcraft",
    "/#/listings/42?search=bonanza&status=active&verified=unverified&year_min=1960&price_max=275000",
    "/#/values",
    "/#/values/17",
    "/#/review",
    "/#/review?search=N12345&filter=aircraft",
    "/#/review/products",
    "/#/review/products/28",
    "/#/review/manual",
    "/#/review/listings/42",
    "/#/review/listings/42?area=aircraft",
    "/#/catalog",
    "/#/catalog/28?search=GNS+430W&status=approved&capability=GPS&completeness=complete&page=3",
  ];
  for (const url of urls) {
    assert.equal(formatRoute(parseRoute(`http://localhost:8001${url}`)), url);
  }
});

test("canonicalizes defaults and rejects unknown paths and invalid IDs", () => {
  assert.equal(
    formatRoute(parseRoute("http://localhost:8001/?review_listing=42#/catalog?page=1&completeness=bogus")),
    "/#/catalog",
  );
  for (const url of [
    "http://localhost:8001/",
    "http://localhost:8001/#/unknown",
  ]) {
    assert.equal(formatRoute(parseRoute(url)), "/#/listings");
  }
  assert.equal(formatRoute(parseRoute("/#/listings/nope?status=active")), "/#/listings?status=active");
  assert.equal(formatRoute(parseRoute("/#/values/0")), "/#/values");
  assert.equal(formatRoute(parseRoute("/#/values/1e2")), "/#/values");
  assert.equal(formatRoute(parseRoute("/#/review/products/nope")), "/#/review/products");
  assert.equal(formatRoute(parseRoute("/#/review/listings/nope")), "/#/review/manual");
  assert.equal(formatRoute(parseRoute("/#/catalog/-9?page=3")), "/#/catalog?page=3");
  assert.equal(formatRoute(parseRoute("/#/listings/42/extra?status=active")), "/#/listings?status=active");
  assert.equal(formatRoute(parseRoute("/#/values/42/extra")), "/#/values");
  assert.equal(formatRoute(parseRoute("/#/review/products/42/extra")), "/#/review/products");
  assert.equal(formatRoute(parseRoute("/#/review/listings/42/extra")), "/#/review/manual");
  assert.equal(formatRoute(parseRoute("/#/review/unrecognized?filter=aircraft")), "/#/review?filter=aircraft");
  assert.equal(formatRoute(parseRoute("/#/catalog/42/extra?page=3")), "/#/catalog?page=3");
});

test("keeps review area selection in router state instead of ambient URL helpers", () => {
  const defaultAreaRoute = parseRoute("/#/review/listings/42");
  const aircraftRoute = parseRoute("/#/review/listings/42?area=aircraft");
  assert.deepEqual(defaultAreaRoute, {
    name: "review",
    view: "listing",
    listingId: 42,
    area: null,
  });
  assert.deepEqual(aircraftRoute, {
    name: "review",
    view: "listing",
    listingId: 42,
    area: "aircraft",
  });
  assert.equal(reviewListingIdForRoute(aircraftRoute), 42);
  assert.equal(reviewListingIdForRoute(parseRoute("/#/review/manual")), null);
  assert.equal(reviewAreaForRoute(defaultAreaRoute, "avionics"), "avionics");
  assert.equal(reviewAreaForRoute(aircraftRoute, "avionics"), "aircraft");
  assert.equal(
    formatRoute({ name: "review", view: "listing", listingId: 42, area: "avionics" }),
    "/#/review/listings/42?area=avionics",
  );
});

test("locks routing for active review mutations but not passive loading", () => {
  for (const owner of [
    "productBatch",
    "resolution",
    "aspectSave",
    "correctionSave",
    "automation",
    "associationValidation",
  ]) {
    assert.equal(reviewMutationInProgress({ [owner]: true }), true, owner);
  }
  assert.equal(reviewMutationInProgress(), false);
  assert.equal(reviewMutationInProgress({ queueLoading: true }), false);
});

test("keeps the active review history entry while a product batch owns mutation", () => {
  const location = fakeLocation();
  setLocation(location, "/#/review/products/28");
  let mutation = { productBatch: true };
  const writes = [];
  const router = createHistoryRouter({
    location,
    history: {
      pushState(_state, _title, url) {
        writes.push(url);
        setLocation(location, url);
      },
      replaceState() {},
    },
    listen: () => {},
    apply: () => {},
    mayNavigate: () => !reviewMutationInProgress(mutation),
  });
  router.start();

  assert.equal(router.navigate(parseRoute("/#/catalog")), false);
  assert.equal(location.hash, "#/review/products/28");
  assert.deepEqual(writes, []);

  mutation = {};
  router.navigate(parseRoute("/#/catalog"));
  assert.equal(location.hash, "#/catalog");
  assert.deepEqual(writes, ["/#/catalog"]);
});

test("invalidates asynchronous review completion when route ownership changes", () => {
  const listing = parseRoute("/#/review/listings/42?area=aircraft");
  const owner = reviewListingRouteOwner(listing, 7);
  assert.deepEqual(owner, { generation: 7, listingId: 42 });
  assert.equal(reviewListingRouteOwnerIsCurrent(owner, listing, 7), true);
  assert.equal(
    reviewListingRouteOwnerIsCurrent(
      owner,
      parseRoute("/#/review/listings/42?area=avionics"),
      8,
    ),
    false,
  );
  assert.equal(
    reviewListingRouteOwnerIsCurrent(owner, parseRoute("/#/listings"), 8),
    false,
  );
  assert.equal(reviewListingRouteOwner(parseRoute("/#/review/manual"), 8), null);
});

test("reconciles shared cache before dropping a stale route continuation", async () => {
  const firstRoute = parseRoute("/#/review/products/28");
  let currentRoute = firstRoute;
  let release;
  const pending = new Promise((resolve) => {
    release = resolve;
  });
  let cacheReconciled = false;
  let committed = false;
  const continuation = pending.then(() => {
    cacheReconciled = true;
    if (routeActivationIsCurrent(firstRoute, currentRoute)) {
      committed = true;
    }
  });

  currentRoute = parseRoute("/#/review/products/29");
  release();
  await continuation;

  assert.equal(cacheReconciled, true);
  assert.equal(committed, false);
  assert.equal(routeActivationIsCurrent(currentRoute, currentRoute), true);
});

test("preserves focused raw input only for its replace activation", () => {
  assert.equal(preserveLiveRouteInput("replace", true), true);
  assert.equal(preserveLiveRouteInput("replace", false), false);
  for (const source of ["startup", "push", "popstate", undefined]) {
    assert.equal(
      preserveLiveRouteInput(source, true),
      false,
      `${source || "missing"} activation remains route-authoritative`,
    );
  }
});

test("keeps raw input through replace but restores route text through browser history", () => {
  const listeners = new Map();
  const location = fakeLocation();
  setLocation(location, "/#/catalog?search=Garmin");
  const input = { focused: true, value: "" };
  const history = {
    pushState(_state, _title, url) {
      setLocation(location, url);
    },
    replaceState(_state, _title, url) {
      setLocation(location, url);
    },
  };
  const router = createHistoryRouter({
    location,
    history,
    listen: (name, listener) => listeners.set(name, listener),
    apply: (route, { source }) => {
      if (!preserveLiveRouteInput(source, input.focused)) {
        input.value = route.filters.search;
      }
    },
  });

  router.start();
  assert.equal(input.value, "Garmin");
  input.value = "GNS ";
  router.navigate({ name: "catalog", filters: { search: input.value } }, { replace: true });
  assert.equal(location.hash, "#/catalog?search=GNS");
  assert.equal(input.value, "GNS ", "the active keystroke keeps its trailing space");

  setLocation(location, "/#/catalog?search=King");
  listeners.get("popstate")();
  assert.equal(input.value, "King", "history restores its route even while input stays focused");
});

test("replaces an absent product detail only while its exact route owns activation", async () => {
  const owner = parseRoute("/#/review/products/28");
  assert.deepEqual(
    reviewProductFallbackForResult({ status: "absent" }, owner, owner),
    { name: "review", view: "products" },
  );
  assert.equal(
    reviewProductFallbackForResult({ status: "loaded" }, owner, owner),
    null,
  );
  assert.equal(
    reviewProductFallbackForResult(
      { status: "absent" },
      owner,
      parseRoute("/#/review/products/29"),
    ),
    null,
    "a late 404 cannot replace a newer product route",
  );
  assert.equal(
    reviewProductFallbackForResult(
      { status: "absent" },
      owner,
      parseRoute("/#/review/products/28"),
    ),
    null,
    "equal route values do not substitute for the initiating activation object",
  );
  let release;
  let current = owner;
  const pending = new Promise((resolve) => {
    release = resolve;
  }).then((result) => reviewProductFallbackForResult(result, owner, current));
  current = parseRoute("/#/review/products/29");
  release({ status: "absent" });
  assert.equal(await pending, null, "a deferred 404 cannot replace the newer route");
  const collection = parseRoute("/#/review/products");
  assert.equal(
    reviewProductFallbackForResult({ status: "absent" }, collection, collection),
    null,
  );
});

test("reloads product collections while reusing a usable detail-route cache", async () => {
  const collection = parseRoute("/#/review/products");
  assert.equal(reviewProductQueueNeedsLoad(collection, true), true);
  assert.equal(reviewProductQueueNeedsLoad(parseRoute("/#/review/products"), false), true);
  assert.equal(reviewProductQueueNeedsLoad(parseRoute("/#/review/products/28"), true), false);
  assert.equal(reviewProductQueueNeedsLoad(parseRoute("/#/review/products/28"), false), true);
  assert.equal(reviewProductQueueNeedsLoad(parseRoute("/#/review/manual"), false), false);

  let release;
  let current = collection;
  const pending = new Promise((resolve) => {
    release = resolve;
  }).then(() => routeActivationIsCurrent(collection, current));
  current = parseRoute("/#/review/manual");
  release();
  assert.equal(await pending, false, "a late collection load cannot commit after route exit");
});

test("clamps catalog pages from authoritative result bounds without losing detail", async () => {
  const emptyOwner = parseRoute("/#/catalog?page=999");
  assert.equal(
    formatRoute(catalogPageFallbackForResult(emptyOwner, emptyOwner, 0, 20)),
    "/#/catalog",
  );

  const detailOwner = parseRoute("/#/catalog/28?search=Garmin&page=999");
  assert.equal(
    formatRoute(catalogPageFallbackForResult(detailOwner, detailOwner, 45, 20)),
    "/#/catalog/28?search=Garmin&page=3",
  );
  const validLastPage = parseRoute("/#/catalog?page=3");
  assert.equal(
    catalogPageFallbackForResult(validLastPage, validLastPage, 45, 20),
    null,
  );

  let release;
  let current = detailOwner;
  const pending = new Promise((resolve) => {
    release = resolve;
  }).then(({ total, limit }) => (
    catalogPageFallbackForResult(detailOwner, current, total, limit)
  ));
  current = parseRoute("/#/catalog?page=2");
  release({ total: 45, limit: 20 });
  assert.equal(await pending, null, "a late page result cannot replace the newer route");
});

test("restores complete route snapshots through Back and Forward without recursive writes", () => {
  const listeners = new Map();
  const applied = [];
  const entries = ["/#/listings?manufacturer=Cessna"];
  let index = 0;
  const location = fakeLocation();
  setLocation(location, entries[index]);
  const history = {
    pushCount: 0,
    replaceCount: 0,
    pushState(_state, _title, url) {
      this.pushCount += 1;
      entries.splice(index + 1, entries.length, url);
      index += 1;
      setLocation(location, url);
    },
    replaceState(_state, _title, url) {
      this.replaceCount += 1;
      entries[index] = url;
      setLocation(location, url);
    },
    back() {
      index -= 1;
      setLocation(location, entries[index]);
      listeners.get("popstate")();
    },
    forward() {
      index += 1;
      setLocation(location, entries[index]);
      listeners.get("popstate")();
    },
  };
  const router = createHistoryRouter({
    location,
    history,
    listen: (name, listener) => listeners.set(name, listener),
    apply: (route, context) => applied.push({ url: formatRoute(route), source: context.source }),
  });

  router.start();
  router.navigate(parseRoute("/#/review/listings/42?area=aircraft"));
  router.navigate(parseRoute("/#/review/listings/42?area=avionics"));
  router.navigate(parseRoute("/#/catalog?search=Garmin&page=3"));
  router.navigate(parseRoute("/#/catalog/28?search=Garmin&page=3"));
  history.back();
  history.back();
  history.back();
  history.back();
  history.forward();
  history.forward();

  assert.deepEqual(applied, [
    { url: "/#/listings?manufacturer=Cessna", source: "startup" },
    { url: "/#/review/listings/42?area=aircraft", source: "push" },
    { url: "/#/review/listings/42?area=avionics", source: "push" },
    { url: "/#/catalog?search=Garmin&page=3", source: "push" },
    { url: "/#/catalog/28?search=Garmin&page=3", source: "push" },
    { url: "/#/catalog?search=Garmin&page=3", source: "popstate" },
    { url: "/#/review/listings/42?area=avionics", source: "popstate" },
    { url: "/#/review/listings/42?area=aircraft", source: "popstate" },
    { url: "/#/listings?manufacturer=Cessna", source: "popstate" },
    { url: "/#/review/listings/42?area=aircraft", source: "popstate" },
    { url: "/#/review/listings/42?area=avionics", source: "popstate" },
  ]);
  assert.equal(history.pushCount, 4);
  assert.equal(history.replaceCount, 0);
});

test("repairs a cancelled popstate without applying the rejected route", () => {
  const listeners = new Map();
  const applied = [];
  const location = fakeLocation();
  setLocation(location, "/#/review/listings/42?area=aircraft");
  const pushed = [];
  const history = {
    pushState(_state, _title, url) {
      pushed.push(url);
      setLocation(location, url);
    },
    replaceState() {},
  };
  const router = createHistoryRouter({
    location,
    history,
    listen: (name, listener) => listeners.set(name, listener),
    apply: (route) => applied.push(formatRoute(route)),
    mayNavigate: (next, _current, source) => (
      source !== "popstate" || next.name === "review"
    ),
  });
  router.start();
  setLocation(location, "/#/catalog");
  listeners.get("popstate")();

  assert.deepEqual(applied, ["/#/review/listings/42?area=aircraft"]);
  assert.deepEqual(pushed, ["/#/review/listings/42?area=aircraft"]);
});

test("canonicalizes malformed URLs reached through browser history", () => {
  const listeners = new Map();
  const location = fakeLocation();
  setLocation(location, "/#/listings");
  const replaced = [];
  const history = {
    pushState() {},
    replaceState(_state, _title, url) {
      replaced.push(url);
      setLocation(location, url);
    },
  };
  const router = createHistoryRouter({
    location,
    history,
    listen: (name, listener) => listeners.set(name, listener),
    apply: () => {},
  });
  router.start();
  setLocation(location, "/#/values/0");
  listeners.get("popstate")();

  assert.equal(location.hash, "#/values");
  assert.deepEqual(replaced, ["/#/values"]);
});

function fakeLocation() {
  return { href: "", pathname: "/", search: "", hash: "" };
}

function setLocation(location, value) {
  const url = new URL(value, "http://localhost:8001");
  location.href = url.href;
  location.pathname = url.pathname;
  location.search = url.search;
  location.hash = url.hash;
}

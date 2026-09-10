import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const indexHtml = readFileSync(new URL("../index.html", import.meta.url), "utf8");
const appCss = readFileSync(new URL("../app.css", import.meta.url), "utf8");
const appJs = readFileSync(new URL("../app.js", import.meta.url), "utf8");
const avionicsJs = readFileSync(new URL("../avionics.js", import.meta.url), "utf8");
const reviewJs = readFileSync(new URL("../review.js", import.meta.url), "utf8");

test("keeps listing form reset explicit without creating a duplicate route entry", () => {
  assert.match(
    appJs,
    /elements\.resetForm\.addEventListener\("click", \(\) => resetListingForm\(\)\)/,
  );
  const guardStart = appJs.indexOf("function confirmListingRouteChange");
  const guardEnd = appJs.indexOf("\nfunction markListingDraftDirty", guardStart);
  assert.doesNotMatch(
    appJs.slice(guardStart, guardEnd),
    /listingDraftDirty = false/,
    "checking one guard must not consume state before later guards accept",
  );
});

test("resetting an edited listing replaces its route with creation", () => {
  const start = appJs.indexOf("function resetListingForm");
  const end = appJs.indexOf("\nfunction openListingDialog", start);
  assert.ok(start >= 0 && end > start);

  const state = { editingListingId: 41, listingDraftDirty: true };
  const navigations = [];
  const elements = {
    listingForm: { reset() {} },
    listingFormTitle: {},
    formModeStatus: {},
    deleteListing: { classList: { add() {} } },
    saveListing: {},
    avionicsList: { replaceChildren() {} },
  };
  const reset = Function(
    "state",
    "elements",
    "setField",
    "addAvionicsRow",
    "setFormMessage",
    "navigateRoute",
    "listingFiltersFromControls",
    `${appJs.slice(start, end)}\nreturn resetListingForm;`,
  )(
    state,
    elements,
    () => {},
    () => {},
    () => {},
    (route, options) => navigations.push({ route, options }),
    () => ({ search: "Cessna" }),
  );

  reset();
  assert.equal(state.editingListingId, null);
  assert.equal(state.listingDraftDirty, false);
  assert.deepEqual(navigations, [{
    route: {
      name: "listings",
      selected: "new",
      filters: { search: "Cessna" },
    },
    options: { replace: true },
  }]);

  reset();
  assert.equal(navigations.length, 1, "resetting creation mode must not write history again");
});

test("organizes four addressable task links without unfinished destinations", () => {
  assert.match(indexHtml, /id="market-navigation-title">Market analysis<\/h2>/);
  assert.match(indexHtml, /id="operations-navigation-title">Operations<\/h2>/);
  for (const [route, destination, label] of [
    ["listings", "listings", "Listings"],
    ["values", "values", "Aircraft values"],
    ["review", "review", "Review queue"],
    ["catalog", "catalog", "Avionics catalog"],
  ]) {
    assert.match(
      indexHtml,
      new RegExp(`href="/#/${route}"[^>]*data-destination="${destination}"[^>]*>${label}<\\/a>`),
    );
  }
  assert.doesNotMatch(indexHtml, /comparisons-panel|rentals-panel/);
  assert.match(indexHtml, /id="values-panel"/);
  assert.match(indexHtml, /id="catalog-panel"/);
});

test("uses one application history owner for task and review routing", () => {
  assert.match(appJs, /createHistoryRouter/);
  assert.doesNotMatch(reviewJs, /window\.(?:history|location)|popstate|FromLocation/);
});

test("keeps live search text only for focused owned refresh activations", () => {
  assert.match(appJs, /function applyAppRoute\(route, context = \{\}\)/);
  assert.match(
    appJs,
    /preserveLiveRouteInput\(\s*source,\s*document\.activeElement === elements\.listingSearch,/,
  );
  assert.match(
    reviewJs,
    /preserveLiveRouteInput\(\s*source,\s*document\.activeElement === elements\.reviewPipelineSearch,/,
  );
  const guardStart = reviewJs.indexOf("    confirmRouteChange(next) {");
  const guardEnd = reviewJs.indexOf("\n    },", guardStart);
  assert.doesNotMatch(
    reviewJs.slice(guardStart, guardEnd),
    /resetProductDraftDirty/,
    "checking one guard must not consume state before later guards accept",
  );
  assert.match(
    avionicsJs,
    /preserveLiveRouteInput\(\s*source,\s*document\.activeElement === elements\.avionicsSearch,/,
  );
});

test("guards dirty listing editors before closing or changing their route", () => {
  assert.match(
    appJs,
    /mayNavigate: \(next, current\) => \(\s*confirmListingRouteChange\(next, current\)/,
  );
  assert.match(
    appJs,
    /elements\.listingForm\.addEventListener\("input", markListingDraftDirty\);\s*elements\.listingForm\.addEventListener\("change", markListingDraftDirty\);/,
  );
  assert.match(
    appJs,
    /function confirmListingRouteChange\(next, current\) \{[\s\S]*?listingEditorRouteIsSame\(current, next\)[\s\S]*?Discard the unsaved listing changes\?/,
  );
  assert.match(
    appJs,
    /function closeListingDialog\(\{ navigate = true \} = \{\}\) \{[\s\S]*?return navigateRoute\([\s\S]*?if \(elements\.listingDialog\.open\) \{\s*elements\.listingDialog\.close\(\);/,
    "a rejected route change must return before the dialog is closed",
  );
  assert.match(appJs, /function editListing\([\s\S]*?state\.listingDraftDirty = false;/);
  assert.match(appJs, /function resetListingForm\([\s\S]*?state\.listingDraftDirty = false;/);
});

test("guards real product drafts while preserving same-product activations", () => {
  assert.match(
    reviewJs,
    /const sameProduct = reviewProductRouteIsSame\(state\.route, next\);[\s\S]*?Discard the unsaved product review changes\?/,
  );
  assert.match(
    reviewJs,
    /reviewProductAttestationForm\.addEventListener\(\s*"input",\s*markProductAttestationDirty,/,
  );
  assert.match(reviewJs, /scope\.addEventListener\("change", \(\) => \{[\s\S]*?markProductStructureDirty\(\);/);
  assert.match(reviewJs, /quantity\.addEventListener\("input", \(\) => \{[\s\S]*?markProductStructureDirty\(\);/);
  assert.match(
    reviewJs,
    /const previousRoute = state\.route;[\s\S]*?const sameProduct = reviewProductRouteIsSame\(previousRoute, route\);\s*if \(!sameProduct\) \{\s*closeProductReview\(\);/,
    "an accepted product switch invalidates the prior draft before awaiting new data",
  );
  assert.match(
    reviewJs,
    /if \(state\.selectedProduct\?\.id === route\.productId\) \{\s*return \{ status: "loaded" \};/,
    "a same-product activation must not reload and discard its drafts",
  );
  assert.match(reviewJs, /state\.productStructureDirty = false;\s*renderSelectedProduct\(\);/);
  assert.match(reviewJs, /state\.selectedProduct\.attestationStatus = "current";\s*state\.productAttestationDirty = false;/);
});

test("preserves stale listing drafts across review area routes", async () => {
  const start = reviewJs.indexOf("function activateReviewRoute");
  const end = reviewJs.indexOf("\nfunction activeReviewMutation", start);
  assert.ok(start >= 0 && end > start);

  const listingId = 41;
  const draft = {
    action: "discard",
    correction: { dirty: true },
  };
  const aircraftRoute = {
    name: "review",
    view: "listing",
    listingId,
    area: "aircraft",
  };
  const avionicsRoute = { ...aircraftRoute, area: "avionics" };
  const state = {
    route: aircraftRoute,
    routeGeneration: 0,
    currentReview: { listing_id: listingId },
    drafts: new Map([["aspect-1", draft]]),
    stale: true,
    activeArea: "aircraft",
    queueLoaded: true,
    pipelineLoaded: true,
    activeVerificationRunId: null,
  };
  const opened = [];
  const selectedAreas = [];
  const activate = Function(
    "state",
    "closeProductReview",
    "setQueueMode",
    "loadQueue",
    "loadPipelineQueue",
    "openReview",
    "resumeVerificationRun",
    "reviewListingIdForRoute",
    "positiveInteger",
    "showWorkspace",
    "setActiveReviewArea",
    `${reviewJs.slice(start, end)}\nreturn activateReviewRoute;`,
  )(
    state,
    () => {},
    () => {},
    () => Promise.resolve(),
    () => Promise.resolve(),
    (nextListingId) => {
      opened.push(nextListingId);
      state.currentReview = { listing_id: nextListingId };
      state.drafts.clear();
      state.stale = false;
      return Promise.resolve();
    },
    () => Promise.resolve(),
    (route) => route?.name === "review" && route.view === "listing"
      ? route.listingId
      : null,
    (value) => Number.isInteger(value) && value > 0 ? value : null,
    () => {},
    (area) => selectedAreas.push(area),
  );

  await activate(avionicsRoute, { source: "popstate" });
  assert.deepEqual(opened, []);
  assert.equal(state.stale, true);
  assert.equal(state.drafts.get("aspect-1"), draft);
  assert.deepEqual(selectedAreas, ["avionics"]);

  await activate({ ...aircraftRoute, listingId: 42 });
  assert.deepEqual(opened, [42], "a different listing must load instead of reusing drafts");
  assert.equal(state.stale, false);
  assert.equal(state.drafts.size, 0);
});

test("cold detail preloads commit only while their listing route owns activation", async () => {
  const start = reviewJs.indexOf("function activateReviewRoute");
  const end = reviewJs.indexOf("\nfunction activeReviewMutation", start);
  assert.ok(start >= 0 && end > start);

  const state = {
    route: null,
    routeGeneration: 0,
    currentReview: null,
    queueLoaded: false,
    pipelineLoaded: false,
    activeVerificationRunId: null,
  };
  const pending = [];
  const commits = [];
  const deferredLoad = (name) => (options) => new Promise((resolve) => {
    pending.push(() => {
      const owned = options.commitGuard();
      if (owned) {
        commits.push(name);
      }
      resolve(owned);
    });
  });
  const activate = Function(
    "state",
    "closeProductReview",
    "setQueueMode",
    "loadQueue",
    "loadPipelineQueue",
    "openReview",
    "resumeVerificationRun",
    "routeActivationIsCurrent",
    "reviewListingIdForRoute",
    "positiveInteger",
    "showWorkspace",
    "setActiveReviewArea",
    `${reviewJs.slice(start, end)}\nreturn activateReviewRoute;`,
  )(
    state,
    () => {},
    () => {},
    deferredLoad("manual"),
    deferredLoad("pipeline"),
    () => Promise.resolve(),
    () => Promise.resolve(),
    (owner, current) => owner === current,
    (route) => route?.name === "review" && route.view === "listing"
      ? route.listingId
      : null,
    (value) => Number.isInteger(value) && value > 0 ? value : null,
    () => {},
    () => {},
  );
  const firstRoute = {
    name: "review",
    view: "listing",
    listingId: 41,
    area: "avionics",
  };

  const departedLoad = activate(firstRoute);
  assert.equal(pending.length, 2);
  assert.equal(pending.every((release) => typeof release === "function"), true);
  state.route = { name: "review", view: "products" };
  pending.splice(0).forEach((release) => release());
  await departedLoad;
  assert.deepEqual(commits, [], "late detail preloads cannot repaint another Review view");

  const secondRoute = { ...firstRoute, listingId: 42 };
  const ownedLoad = activate(secondRoute);
  pending.splice(0).forEach((release) => release());
  await ownedLoad;
  assert.deepEqual(commits, ["manual", "pipeline"]);
});

test("reloads pipeline only on startup and true collection re-entry", async () => {
  const start = reviewJs.indexOf("function deactivateReviewRoute");
  const end = reviewJs.indexOf("\nfunction activeReviewMutation", start);
  assert.ok(start >= 0 && end > start);

  const state = {
    route: { name: "review", view: "manual" },
    routeGeneration: 0,
    pipelineSearch: "",
    pipelineFilter: "all",
    pipelineLoaded: true,
    activeVerificationRunId: 7,
  };
  const elements = {
    reviewPipelineSearch: { value: "" },
    reviewPipelineFilter: { value: "all" },
  };
  const requests = [];
  const resumedRuns = [];
  const loadPipelineQueue = (options) => {
    requests.push(options);
    return Promise.resolve(true);
  };
  const samePipeline = (left, right) => left?.name === "review"
    && left.view === "pipeline"
    && right?.name === "review"
    && right.view === "pipeline";
  const lifecycle = Function(
    "state",
    "elements",
    "document",
    "closeProductReview",
    "showQueue",
    "setQueueMode",
    "loadQueue",
    "loadPipelineQueue",
    "renderPipelineTable",
    "preserveLiveRouteInput",
    "reviewPipelineRouteIsSame",
    "resumeVerificationRun",
    `${reviewJs.slice(start, end)}\nreturn { activateReviewRoute, deactivateReviewRoute };`,
  )(
    state,
    elements,
    { activeElement: null },
    () => {},
    () => {},
    () => {},
    () => Promise.resolve(true),
    loadPipelineQueue,
    () => {},
    () => false,
    samePipeline,
    (runId) => {
      resumedRuns.push(runId);
      return Promise.resolve();
    },
  );
  const activate = lifecycle.activateReviewRoute;

  const pipeline = { name: "review", view: "pipeline", search: "", filter: "all" };
  await activate(pipeline, { source: "push" });
  assert.equal(requests.length, 1, "entering Pipeline refreshes stale cached rows");
  assert.deepEqual(resumedRuns, [7]);

  const searched = { ...pipeline, search: "GNS" };
  await activate(searched, { source: "replace" });
  await activate({ ...searched, filter: "manual" }, { source: "replace" });
  assert.equal(requests.length, 1, "local controls reuse the active collection");
  assert.deepEqual(resumedRuns, [7], "local controls issue no run-status request");
  assert.equal(requests[0].commitGuard(), true, "local controls retain request ownership");

  lifecycle.deactivateReviewRoute();
  assert.equal(state.route, null, "deactivation records that Review no longer owns the page");
  assert.equal(requests[0].commitGuard(), false, "leaving Pipeline invalidates its response");
  await activate(pipeline, { source: "popstate" });
  assert.equal(requests.length, 2, "returning to Pipeline refreshes again");
  assert.deepEqual(resumedRuns, [7, 7]);
});

test("review tab selection changes only after accepted navigation", () => {
  const start = reviewJs.indexOf("function navigateReviewArea");
  const end = reviewJs.indexOf("\nfunction handleReviewTabKeydown", start);
  assert.ok(start >= 0 && end > start);

  let accepted = false;
  let visibleArea = "aircraft";
  let focused = 0;
  const navigations = [];
  const navigateArea = Function(
    "REVIEW_AREAS",
    "currentListingId",
    "navigate",
    "reviewListingRoute",
    "reviewAreaElements",
    `${reviewJs.slice(start, end)}\nreturn navigateReviewArea;`,
  )(
    ["aircraft", "avionics"],
    () => 41,
    (route, options) => {
      navigations.push({ route, options });
      if (!accepted) {
        return false;
      }
      visibleArea = route.area;
      return true;
    },
    (listingId, area) => ({ name: "review", view: "listing", listingId, area }),
    () => ({ tab: { focus: () => { focused += 1; } } }),
  );

  assert.equal(navigateArea("avionics", { focus: true }), false);
  assert.equal(visibleArea, "aircraft");
  assert.equal(focused, 0);

  accepted = true;
  assert.equal(navigateArea("avionics", { focus: true }), true);
  assert.equal(visibleArea, "avionics");
  assert.equal(focused, 1);
  assert.deepEqual(navigations.at(-1), {
    route: { name: "review", view: "listing", listingId: 41, area: "avionics" },
    options: { replace: true },
  });
  assert.match(
    reviewJs,
    /tab\.addEventListener\("click", \(\) => \{\s*navigateReviewArea\(area\);/,
  );
});

test("falls back from absent product detail only through its route owner", () => {
  assert.match(
    reviewJs,
    /const result = await openProductReview\(route\.productId\);\s*await replaceAbsentProductRoute\(result, route\);/,
  );
  assert.match(reviewJs, /routeOwner: state\.route/);
  assert.match(
    reviewJs,
    /if \(finishProductAction\(action\)\) \{\s*await replaceAbsentProductRoute\(detailResult, action\.routeOwner\);/,
  );
});

test("reloads product collections but reuses detail caches under route ownership", () => {
  assert.match(
    reviewJs,
    /reviewProductQueueNeedsLoad\(\s*route,\s*state\.productGroups\.length > 0,\s*\)\s*\? loadProductQueue\(\{\s*commitGuard: \(\) => routeActivationIsCurrent\(route, state\.route\),/,
  );
});

test("canonicalizes catalog result pages before continuing detail activation", () => {
  assert.match(
    avionicsJs,
    /const pageFallback = state\.avionicsLoaded[\s\S]*?catalogPageFallbackForResult\(\s*route,\s*state\.route,\s*state\.avionicsTotal,\s*state\.avionicsLimit,[\s\S]*?const navigated = await navigate\(pageFallback, \{ replace: true \}\);\s*if \(navigated !== false\) \{\s*return;/,
  );
});

test("catalog deactivation cancels a pending routed search", () => {
  const deactivateStart = avionicsJs.indexOf("function deactivateAvionicsInspector");
  const deactivateEnd = avionicsJs.indexOf("\nasync function applyCatalogRoute", deactivateStart);
  const searchStart = avionicsJs.indexOf("function scheduleAvionicsSearch");
  const searchEnd = avionicsJs.indexOf("\nasync function loadAvionics", searchStart);
  assert.ok(deactivateStart >= 0 && deactivateEnd > deactivateStart);
  assert.ok(searchStart >= 0 && searchEnd > searchStart);

  let nextTimer = 1;
  const timers = new Map();
  const navigations = [];
  const state = {
    avionicsSearchTimer: null,
    route: { name: "catalog", filters: { page: 1 } },
  };
  const window = {
    setTimeout(callback) {
      const timer = nextTimer;
      nextTimer += 1;
      timers.set(timer, callback);
      return timer;
    },
    clearTimeout(timer) {
      timers.delete(timer);
    },
  };
  const lifecycle = Function(
    "state",
    "window",
    "navigate",
    "catalogRouteFromControls",
    "closeAvionicsDetail",
    `${avionicsJs.slice(deactivateStart, deactivateEnd)}\n`
      + `${avionicsJs.slice(searchStart, searchEnd)}\n`
      + "return { deactivateAvionicsInspector, scheduleAvionicsSearch };",
  )(
    state,
    window,
    (route, options) => navigations.push({ route, options }),
    () => ({ name: "catalog", filters: { search: "GNS", page: 1 } }),
    () => {},
  );
  const runTimers = () => {
    const pending = Array.from(timers.values());
    timers.clear();
    pending.forEach((callback) => callback());
  };

  lifecycle.scheduleAvionicsSearch();
  assert.equal(timers.size, 1);
  lifecycle.deactivateAvionicsInspector();
  runTimers();
  assert.equal(state.avionicsSearchTimer, null);
  assert.deepEqual(navigations, [], "a deactivated search cannot route back to Catalog");

  lifecycle.scheduleAvionicsSearch();
  runTimers();
  assert.deepEqual(navigations, [{
    route: { name: "catalog", filters: { search: "GNS", page: 1 } },
    options: { replace: true },
  }]);
});

test("catalog deactivation suppresses hidden trigger focus", () => {
  const deactivateStart = avionicsJs.indexOf("function deactivateAvionicsInspector");
  const deactivateEnd = avionicsJs.indexOf("\nasync function applyCatalogRoute", deactivateStart);
  const closeStart = avionicsJs.indexOf("function closeAvionicsDetail");
  const closeEnd = avionicsJs.indexOf("\nfunction detailState", closeStart);
  assert.ok(deactivateStart >= 0 && deactivateEnd > deactivateStart);
  assert.ok(closeStart >= 0 && closeEnd > closeStart);

  let closeHandler = () => {};
  let hiddenFocus = 0;
  let visibleFocus = 0;
  const state = {
    route: { name: "catalog", filters: { page: 1 } },
    avionicsDetailTrigger: { focus: () => { hiddenFocus += 1; } },
    avionicsDetailRequestSequence: 0,
    avionicsDetail: {},
    avionicsDeleting: false,
  };
  const elements = {
    avionicsDetailDialog: {
      open: true,
      close() {
        this.open = false;
        closeHandler();
      },
    },
    deleteAvionicsProduct: {},
  };
  const lifecycle = Function(
    "state",
    "elements",
    "cancelAvionicsSearch",
    "navigate",
    "catalogRouteFromControls",
    `${avionicsJs.slice(deactivateStart, deactivateEnd)}\n`
      + `${avionicsJs.slice(closeStart, closeEnd)}\n`
      + "return { deactivateAvionicsInspector, finishAvionicsDetailClose, closeAvionicsDetail };",
  )(
    state,
    elements,
    () => {},
    () => {},
    () => ({ name: "catalog", filters: { page: 1 } }),
  );
  closeHandler = lifecycle.finishAvionicsDetailClose;

  lifecycle.deactivateAvionicsInspector();
  assert.equal(hiddenFocus, 0);
  assert.equal(state.avionicsDetailTrigger, null);

  elements.avionicsDetailDialog.open = true;
  state.avionicsDetailTrigger = { focus: () => { visibleFocus += 1; } };
  lifecycle.closeAvionicsDetail(false, { updateRoute: false });
  assert.equal(visibleFocus, 1, "an ordinary user close restores its visible trigger");
  assert.equal(state.avionicsDetailTrigger, null);
});

test("hands catalog deletion cleanup to the exact operation owner", () => {
  assert.match(
    avionicsJs,
    /const deletionOwner = \{ productId \};[\s\S]*?state\.avionicsDeletionOwner = deletionOwner;\s*state\.avionicsDeleting = true;/,
  );
  assert.match(
    avionicsJs,
    /closeAvionicsDetail\(true, \{ updateRoute: false \}\);\s*if \(!ownsDeletion\(\)\) \{\s*return;\s*\}\s*state\.avionicsDeleting = false;\s*const navigation = navigate\(catalogRouteFromControls\([\s\S]*?const routeOwner = state\.route;\s*await navigation;\s*if \(!ownsDeletion\(\) \|\| !routeActivationIsCurrent\(routeOwner, state\.route\)\) \{\s*return;\s*\}/,
    "the owning delete releases route navigation only after the response is committed",
  );
  assert.match(
    avionicsJs,
    /await Promise\.allSettled\([\s\S]*?if \(!ownsDeletion\(\) \|\| !routeActivationIsCurrent\(routeOwner, state\.route\)\) \{\s*return;\s*\}\s*const listingIds/,
    "a later Catalog activation owns the deletion follow-up message",
  );
  assert.match(
    avionicsJs,
    /finally \{\s*if \(ownsDeletion\(\)\) \{\s*state\.avionicsDeletionOwner = null;\s*state\.avionicsDeleting = false;/,
    "an older finally block cannot clear a newer delete operation",
  );
});

test("preserves focused listing search text during its own asynchronous refresh", () => {
  assert.match(
    appJs,
    /const route = appRouter\.current\(\);\s*if \(route\?\.name === "listings"\) \{\s*applyListingsRoute\(route, \{ source: "refresh" \}\);/,
  );
});

test("keeps usable listing cache state across a failed refresh", () => {
  const start = appJs.indexOf("async function loadListings()");
  const end = appJs.indexOf("\nasync function loadAircraftOptions()", start);
  assert.ok(start >= 0 && end > start);
  const loader = appJs.slice(start, end);
  assert.match(loader, /state\.listingsLoaded = true;/);
  assert.doesNotMatch(loader, /catch \(error\) \{\s*state\.listingsLoaded = false;/);
});

test("reloads the manual review collection on every route re-entry", () => {
  assert.match(
    reviewJs,
    /if \(route\.view === "manual"\) \{\s*setQueueMode\("listing", \{ load: false \}\);\s*const queueLoad = loadQueue\(\{\s*commitGuard: \(\) => routeActivationIsCurrent\(route, state\.route\),/,
  );
});

test("reloads the pipeline collection under destination ownership", () => {
  assert.match(
    reviewJs,
    /const samePipelineActivation = source !== "startup"\s*&& reviewPipelineRouteIsSame\(previousRoute, route\);\s*const pipelineLoad = samePipelineActivation[\s\S]*?commitGuard: \(\) => reviewPipelineRouteIsSame\(route, state\.route\),/,
  );
  assert.match(
    reviewJs,
    /async function loadPipelineQueue\(\{ quiet = false, commitGuard = null \} = \{\}\) \{\s*const sequence = \+\+state\.pipelineRequestSequence;\s*const uiGeneration = \+\+state\.reviewLoadUiGeneration;\s*const mayCommit = \(\) => \(/,
  );
});

test("route-invalidated manual and pipeline loads release only their own busy state", async () => {
  for (const [name, endMarker, sequenceKey, resultKey] of [
    ["loadQueue", "\nfunction renderQueue()", "queueRequestSequence", "reviewResults"],
    ["loadPipelineQueue", "\nfunction renderPipeline()", "pipelineRequestSequence", "reviewPipelineResults"],
  ]) {
    const start = reviewJs.indexOf(`async function ${name}`);
    const end = reviewJs.indexOf(endMarker, start);
    assert.ok(start >= 0 && end > start);
    const state = { [sequenceKey]: 0, reviewLoadUiGeneration: 0 };
    const attributes = [];
    const elements = {
      [resultKey]: {
        setAttribute(attribute, value) {
          attributes.push([attribute, value]);
        },
      },
      refreshReviews: {},
    };
    const busy = [];
    const pending = [];
    const api = () => new Promise((resolve) => pending.push(resolve));
    const loader = Function(
      "state",
      "elements",
      "api",
      "setQueueMessage",
      "setButtonBusy",
      "QUEUE_LIMIT",
      `${reviewJs.slice(start, end)}\nreturn ${name};`,
    )(
      state,
      elements,
      api,
      () => {},
      (_button, value) => busy.push(value),
      50,
    );
    let routeCurrent = true;
    const first = loader({ commitGuard: () => routeCurrent });
    const second = loader({ commitGuard: () => routeCurrent });
    pending[0]({});
    await first;
    assert.equal(busy.at(-1), true, `${name} stale sequence keeps newer load busy`);

    routeCurrent = false;
    pending[1]({});
    assert.equal(await second, false);
    assert.equal(busy.at(-1), false, `${name} current sequence releases shared refresh`);
    assert.deepEqual(attributes.at(-1), ["aria-busy", "false"]);
  }
});

test("an older manual load cannot release a newer pipeline refresh", async () => {
  const loaderSource = (name, endMarker) => {
    const start = reviewJs.indexOf(`async function ${name}`);
    const end = reviewJs.indexOf(endMarker, start);
    assert.ok(start >= 0 && end > start);
    return reviewJs.slice(start, end);
  };
  const state = {
    queueRequestSequence: 0,
    pipelineRequestSequence: 0,
    reviewLoadUiGeneration: 0,
  };
  const attributes = new Map();
  const result = (name) => ({
    setAttribute(attribute, value) {
      attributes.set(name, [attribute, value]);
    },
  });
  const elements = {
    reviewResults: result("manual"),
    reviewPipelineResults: result("pipeline"),
    refreshReviews: {},
  };
  const busy = [];
  const pending = [];
  const api = () => new Promise((resolve) => pending.push(resolve));
  const compile = (name, source) => Function(
    "state",
    "elements",
    "api",
    "setQueueMessage",
    "setButtonBusy",
    "QUEUE_LIMIT",
    `${source}\nreturn ${name};`,
  )(
    state,
    elements,
    api,
    () => {},
    (_button, value) => busy.push(value),
    50,
  );
  const loadManual = compile(
    "loadQueue",
    loaderSource("loadQueue", "\nfunction renderQueue()"),
  );
  const loadPipeline = compile(
    "loadPipelineQueue",
    loaderSource("loadPipelineQueue", "\nfunction renderPipeline()"),
  );
  let manualRouteCurrent = true;
  let pipelineRouteCurrent = true;
  const manual = loadManual({ commitGuard: () => manualRouteCurrent });
  const pipeline = loadPipeline({ commitGuard: () => pipelineRouteCurrent });

  manualRouteCurrent = false;
  pending[0]({});
  assert.equal(await manual, false);
  assert.equal(busy.at(-1), true, "manual completion leaves newer pipeline busy");
  assert.deepEqual(attributes.get("manual"), ["aria-busy", "false"]);

  pipelineRouteCurrent = false;
  pending[1]({});
  assert.equal(await pipeline, false);
  assert.equal(busy.at(-1), false, "newest pipeline completion releases Refresh");
  assert.deepEqual(attributes.get("pipeline"), ["aria-busy", "false"]);
});

test("guards review mutations and stale asynchronous completion at the route boundary", () => {
  assert.match(
    reviewJs,
    /confirmRouteChange\(next\) \{\s*if \(activeReviewMutation\(\)\) \{\s*return false;/,
  );
  for (const owner of [
    "productBusy",
    "resolving",
    "savingAspectKey",
    "correctionSave",
    "automating",
    "validatingAspectKey",
  ]) {
    assert.match(reviewJs, new RegExp(owner));
  }
  assert.match(reviewJs, /function listingRouteOwnerIsCurrent\(owner\)/);
  assert.match(
    reviewJs,
    /finally \{\s*state\.resolving = false;\s*if \(listingRouteOwnerIsCurrent\(routeOwner\)\)/,
  );
  assert.match(
    reviewJs,
    /setAutomaticVerificationBusy\(false, \{\s*updateView: listingRouteOwnerIsCurrent\(routeOwner\),\s*\}\)/,
  );
  assert.match(
    reviewJs,
    /finally \{\s*if \(state\.validatingAspectKey === key\) \{\s*state\.validatingAspectKey = null;/,
  );
  assert.match(
    reviewJs,
    /shouldReconcileResolution\(error\)[\s\S]*?recoverCommittedResolution\(\s*resolvedListingId,\s*`Review decisions were saved, but the response was interrupted\.[\s\S]*?`,\s*routeOwner,\s*\)/,
  );
});

test("commits post-navigation review messages only for the accepted activation", async () => {
  const start = reviewJs.indexOf("async function navigateAndCommitReviewRoute");
  const end = reviewJs.indexOf("\nfunction handleReviewTabKeydown", start);
  assert.ok(start >= 0 && end > start);

  const state = {
    route: { name: "review", view: "listing", listingId: 41 },
    routeGeneration: 1,
  };
  const pending = [];
  const navigate = (route) => {
    state.route = route;
    state.routeGeneration += 1;
    return new Promise((resolve) => pending.push(resolve));
  };
  const navigateAndCommit = Function(
    "state",
    "navigate",
    "routeActivationIsCurrent",
    `${reviewJs.slice(start, end)}\nreturn navigateAndCommitReviewRoute;`,
  )(
    state,
    navigate,
    (owner, current) => owner === current,
  );
  for (const helper of ["automatic verification", "manual resolution"]) {
    const messages = [];
    const superseded = navigateAndCommit(
      { name: "review", view: "listing", listingId: 42 },
      { replace: true },
      () => messages.push(`${helper} next listing`),
    );
    state.route = { name: "review", view: "listing", listingId: 43 };
    state.routeGeneration += 1;
    pending.shift()();
    assert.equal(await superseded, false);
    assert.deepEqual(
      messages,
      [],
      `${helper} cannot overwrite a superseding listing activation`,
    );

    const owned = navigateAndCommit(
      { name: "review", view: "manual" },
      { replace: true },
      () => messages.push(`${helper} queue fallback`),
    );
    pending.shift()();
    assert.equal(await owned, true);
    assert.deepEqual(messages, [`${helper} queue fallback`]);
  }

  for (const [name, endMarker] of [
    ["leaveAutomaticallyVerifiedReview", "\nfunction setAutomaticVerificationBusy"],
    ["resolveReview", "\nfunction showAspectResolutionError"],
  ]) {
    const helperStart = reviewJs.indexOf(`async function ${name}`);
    const helperEnd = reviewJs.indexOf(endMarker, helperStart);
    const source = reviewJs.slice(helperStart, helperEnd);
    assert.ok(helperStart >= 0 && helperEnd > helperStart);
    assert.match(source, /navigateAndCommitReviewRoute\(\s*reviewListingRoute\(nextId\)/);
    assert.match(source, /navigateAndCommitReviewRoute\(\s*reviewQueueRoute\("listing"\)/);
    assert.doesNotMatch(source, /await navigate\(/);
  }
});

test("drops stale listing, product-review, and catalog activation continuations", () => {
  assert.match(
    appJs,
    /const routeOwner = appRouter\.current\(\);\s*const ownsRoute = \(\) => routeActivationIsCurrent\(routeOwner, appRouter\.current\(\)\);/,
  );
  assert.match(
    appJs,
    /const response = await api\([\s\S]*?state\.listings = reconcileSavedListingCache\(state\.listings, response\?\.listing\);\s*if \(!ownsRoute\(\)\) \{\s*return;\s*\}\s*state\.listingDraftDirty = false;\s*await loadListings\(\);\s*if \(!ownsRoute\(\)\) \{\s*return;\s*\}\s*await refreshAircraftAfterEstimateResponse\(response\);\s*if \(!ownsRoute\(\)\) \{\s*return;\s*\}\s*navigateRoute\(/,
  );
  assert.match(
    appJs,
    /await api\(`\/api\/listings\/\$\{listing\.id\}`[\s\S]*?state\.listings = reconcileDeletedListingCache\(state\.listings, listing\.id\);\s*if \(!ownsRoute\(\)\) \{\s*return;\s*\}\s*state\.listingDraftDirty = false;\s*await loadListings\(\);\s*if \(!ownsRoute\(\)\) \{\s*return;\s*\}\s*await loadAircraftOptions\(\);\s*if \(!ownsRoute\(\)\) \{\s*return;\s*\}\s*navigateRoute\(/,
  );
  assert.match(
    reviewJs,
    /Promise\.resolve\(queueLoad\)\.then\(async \(loaded\) => \{\s*if \(!loaded \|\| !routeActivationIsCurrent\(route, state\.route\)\)/,
  );
  assert.match(
    avionicsJs,
    /await loadAvionicsOptions\(\);\s*\}\s*if \(!routeActivationIsCurrent\(route, state\.route\)\) \{\s*return;/,
  );
});

test("reconciles stale listing saves and deletes without changing load semantics", () => {
  const start = appJs.indexOf("function reconcileSavedListingCache");
  const end = appJs.indexOf("\nasync function refreshAircraftAfterEstimateResponse", start);
  assert.ok(start >= 0 && end > start);
  const helpers = Function(
    `${appJs.slice(start, end)}\nreturn { reconcileSavedListingCache, reconcileDeletedListingCache };`,
  )();
  const original = [
    { id: 2, label: "existing two" },
    { id: 1, label: "old one" },
  ];

  const updated = helpers.reconcileSavedListingCache(
    original,
    { id: 1, label: "fresh one" },
  );
  assert.deepEqual(updated, [
    { id: 2, label: "existing two" },
    { id: 1, label: "fresh one" },
  ]);
  assert.deepEqual(original, [
    { id: 2, label: "existing two" },
    { id: 1, label: "old one" },
  ]);

  const created = helpers.reconcileSavedListingCache(
    updated,
    { id: 3, label: "created three" },
  );
  assert.deepEqual(created.map((listing) => listing.id), [3, 2, 1]);
  assert.deepEqual(
    helpers.reconcileDeletedListingCache(created, 2).map((listing) => listing.id),
    [3, 1],
  );
  assert.doesNotMatch(
    appJs.slice(start, end),
    /listingsLoaded|renderListings|navigateRoute/,
  );
});

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

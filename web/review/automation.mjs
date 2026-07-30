const AIRCRAFT_COMPLETE = new Set([
  "already_verified",
  "assigned",
  "curated",
  "current",
  "reused",
  "verified",
]);

const AIRCRAFT_LOCAL = new Set([
  ...AIRCRAFT_COMPLETE,
  "locally_assignable",
  "not_required",
]);

const AVIONICS_COMPLETE = new Set([
  "already_complete",
  "already_verified",
  "verified",
]);

const RUN_STATUSES = new Set([
  "queued",
  "running",
  "cancelling",
  "completed",
  "cancelled",
]);

const RUN_ITEM_STATUSES = new Set([
  "queued",
  "running",
  "verified",
  "pending_review",
  "pending_reference",
  "blocked",
  "failed",
  "cancelled",
]);

const REASON_COPY = Object.freeze({
  aircraft_assignment_ready:
    "The FAA-backed aircraft identity can be assigned from the local catalog.",
  aircraft_curation_required:
    "The FAA record is valid, but the aircraft still needs a canonical catalog identity.",
  aircraft_verification_remaining:
    "The aircraft still needs FAA-backed verification.",
  automatic_verification_available:
    "The retained listing is ready for automatic equipment checks.",
  automatic_verification_blocked:
    "The retained listing cannot be resolved safely without manual review.",
  automatic_verification_failed:
    "The provider-free verification check failed.",
  avionics_review_remaining:
    "One or more avionics observations still need verification.",
  canonical_identity_assignment_missing:
    "FAA verification succeeded, but the aircraft has no current canonical catalog assignment.",
  canonical_identity_assignment_mismatch:
    "The canonical aircraft assignment no longer agrees with the FAA record.",
  faa_rejected:
    "The aircraft did not pass mandatory FAA admission.",
  factory_reference_pending:
    "Aircraft and avionics identity review is complete; valuation-grade factory reference data is still pending.",
  manual_review_required:
    "One or more observations need a person to verify or discard them.",
  missing_registration:
    "No U.S. N-number is recorded for this listing.",
  non_n_registration:
    "The recorded registration is not a U.S. N-number.",
  registration_not_found:
    "The N-number was not found in the current FAA registry snapshot.",
  registry_snapshot_unavailable:
    "No current FAA registry snapshot is available for this aircraft.",
  serial_conflict:
    "The listing serial number conflicts with the current FAA record.",
  source_unavailable:
    "The retained listing source is unavailable.",
});

export const PIPELINE_FILTERS = Object.freeze([
  "all",
  "manual",
  "aircraft",
  "avionics",
  "reference",
  "gemini",
]);

export function pipelineRowsFromResponse(payload) {
  const verification = payload?.verification;
  const listings = Array.isArray(verification?.listings)
    ? verification.listings
    : [];
  const contexts = new Map(
    (Array.isArray(payload?.listing_contexts) ? payload.listing_contexts : [])
      .filter((context) => positiveInteger(context?.listing_id) !== null)
      .map((context) => [positiveInteger(context.listing_id), context]),
  );
  return listings.flatMap((listing) => {
    const listingId = positiveInteger(listing?.listing_id);
    if (listingId === null) {
      return [];
    }
    const context = contexts.get(listingId) || {};
    const aircraft = stageView("aircraft", listing?.aircraft);
    const avionics = stageView("avionics", listing?.avionics);
    const reference = stageView("reference", listing?.finalization);
    const gemini = geminiRequirement(listing);
    const reason = listingReason(listing, aircraft, avionics, reference);
    return [{
      listingId,
      label: nonBlank(context.label) || `Listing #${listingId}`,
      registrationNumber: nonBlank(context.registration_number) || "",
      modelYear: integer(context.model_year),
      hasPendingReview: context.has_pending_review === true,
      status: nonBlank(listing?.status) || "unknown",
      finalIngestionState: nonBlank(listing?.final_ingestion_state) || "",
      aircraft,
      avionics,
      reference,
      gemini,
      reason,
    }];
  });
}

export function pipelineCheckpoint(payload) {
  const checkpoint = payload?.verification?.checkpoint;
  const hasMore = checkpoint?.has_more === true;
  const resumeAfterListingId = positiveInteger(
    checkpoint?.resume_after_listing_id,
  );
  return {
    hasMore,
    resumeAfterListingId,
    valid: !hasMore || resumeAfterListingId !== null,
  };
}

export function pipelineSummary(rows) {
  const values = Array.isArray(rows) ? rows : [];
  return {
    total: values.length,
    aircraftComplete: values.filter((row) => row?.aircraft?.complete).length,
    avionicsComplete: values.filter((row) => row?.avionics?.complete).length,
    manualReview: values.filter((row) => row?.hasPendingReview === true).length,
    referencePending: values.filter(
      (row) => row?.reference?.status === "pending_reference",
    ).length,
    geminiExpected: values.filter(
      (row) => row?.gemini?.kind === "required",
    ).length,
    geminiPossible: values.filter(
      (row) => row?.gemini?.kind === "possible",
    ).length,
  };
}

export function pipelineProviderPlan(responses) {
  const pages = Array.isArray(responses) ? responses : [];
  const result = {
    aircraftGroundingCandidates: 0,
    verifiedLocalIdentityComponents: 0,
    minimumBaselineRequests: 0,
    allPositiveBaselineRequests: 0,
    validationEnvelopeMaximum: 0,
    includesFinalizationEnrichment: false,
    notes: [],
  };
  for (const payload of pages) {
    const plan = payload?.verification?.provider_request_plan;
    const avionics = plan?.avionics;
    result.aircraftGroundingCandidates += nonnegativeInteger(
      plan?.aircraft_grounding_candidates,
    );
    result.verifiedLocalIdentityComponents += nonnegativeInteger(
      avionics?.verified_local_identity_components,
    );
    result.minimumBaselineRequests += nonnegativeInteger(
      avionics?.known_total_provider_requests_minimum_baseline,
    );
    result.allPositiveBaselineRequests += nonnegativeInteger(
      avionics?.known_total_provider_requests_all_positive_baseline,
    );
    result.validationEnvelopeMaximum += nonnegativeInteger(
      avionics?.known_total_provider_requests_validation_envelope_maximum,
    );
    result.includesFinalizationEnrichment ||= (
      plan?.finalization_enrichment_requests_included === true
    );
    const note = nonBlank(plan?.finalization_note);
    if (note && !result.notes.includes(note)) {
      result.notes.push(note);
    }
  }
  return result;
}

export function pipelineServiceStatus(responses, providerPlan = null) {
  const pages = Array.isArray(responses) ? responses : [];
  const plan = providerPlan || pipelineProviderPlan(pages);
  const geminiConfigured = pages.length > 0
    && pages.every((payload) => payload?.services?.gemini_configured === true);
  const faaDrsConfigured = pages.length > 0
    && pages.every((payload) => payload?.services?.faa_drs_configured === true);
  const warnings = [];
  if (
    !faaDrsConfigured
    && plan.aircraftGroundingCandidates > 0
  ) {
    warnings.push(
      `${plan.aircraftGroundingCandidates} aircraft grounding `
        + `${plan.aircraftGroundingCandidates === 1 ? "candidate needs" : "candidates need"} `
        + "FAA DRS, but FAA_DRS_API_KEY is not configured.",
    );
  }
  if (
    !geminiConfigured
    && (
      plan.aircraftGroundingCandidates > 0
      || plan.validationEnvelopeMaximum > 0
    )
  ) {
    warnings.push(
      "Gemini is not configured, so provider-backed identity work cannot run.",
    );
  }
  return { geminiConfigured, faaDrsConfigured, warnings };
}

export function filterPipelineRows(rows, filter = "all", query = "") {
  const selectedFilter = PIPELINE_FILTERS.includes(filter) ? filter : "all";
  const search = String(query || "").trim().toLocaleLowerCase();
  return (Array.isArray(rows) ? rows : []).filter((row) => {
    const matchesFilter = selectedFilter === "all"
      || selectedFilter === "manual" && row?.hasPendingReview === true
      || selectedFilter === "aircraft" && !row?.aircraft?.complete
      || selectedFilter === "avionics" && !row?.avionics?.complete
      || selectedFilter === "reference"
        && row?.reference?.status === "pending_reference"
      || selectedFilter === "gemini" && row?.gemini?.kind !== "none";
    if (!matchesFilter || !search) {
      return matchesFilter;
    }
    return [
      row?.listingId,
      row?.label,
      row?.registrationNumber,
      row?.modelYear,
      row?.reason,
      row?.aircraft?.label,
      row?.avionics?.label,
      row?.reference?.label,
      row?.gemini?.label,
    ].some((value) => String(value ?? "").toLocaleLowerCase().includes(search));
  });
}

export function pipelineAutomaticEligibility(row) {
  if (!row || typeof row !== "object") {
    return { eligible: false, reason: "Listing status is unavailable." };
  }
  if (
    row.status === "verified"
    || row.status === "already_verified"
    || row.finalIngestionState === "ready"
  ) {
    return { eligible: false, reason: "The listing is already verified." };
  }
  if (
    row.reference?.status === "pending_reference"
    || row.status === "pending_reference"
  ) {
    return {
      eligible: false,
      reason: "Identity review is complete; factory reference publication is the remaining work.",
    };
  }
  if (
    row.aircraft?.status === "rejected"
    || row.avionics?.status === "faa_rejected"
  ) {
    return {
      eligible: false,
      reason: "The aircraft was deterministically rejected by mandatory FAA admission.",
    };
  }
  return {
    eligible: true,
    reason: "The listing has automatic identity or readiness work available.",
  };
}

export function verificationRunRequest(listingIds) {
  const normalized = Array.from(new Set(
    (Array.isArray(listingIds) ? listingIds : [])
      .map(positiveInteger)
      .filter((value) => value !== null),
  )).sort((left, right) => left - right);
  return { listing_ids: normalized };
}

export function verificationRunIdempotencyKey(cryptoApi = globalThis.crypto) {
  if (typeof cryptoApi?.randomUUID === "function") {
    return cryptoApi.randomUUID();
  }
  if (typeof cryptoApi?.getRandomValues !== "function") {
    throw new Error("Secure random values are unavailable in this browser.");
  }
  const bytes = new Uint8Array(16);
  cryptoApi.getRandomValues(bytes);
  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const hex = Array.from(bytes, (value) => value.toString(16).padStart(2, "0"));
  return [
    hex.slice(0, 4).join(""),
    hex.slice(4, 6).join(""),
    hex.slice(6, 8).join(""),
    hex.slice(8, 10).join(""),
    hex.slice(10).join(""),
  ].join("-");
}

export function verificationRunState(run, items = []) {
  const runId = positiveInteger(run?.id);
  const status = RUN_STATUSES.has(run?.status) ? run.status : "unknown";
  const normalizedItems = (Array.isArray(items) ? items : []).flatMap((item) => {
    const id = positiveInteger(item?.id);
    const listingId = positiveInteger(item?.listing_id);
    if (
      id === null
      || listingId === null
      || !RUN_ITEM_STATUSES.has(item?.status)
    ) {
      return [];
    }
    return [{
      id,
      listingId,
      status: item.status,
      outcome: item?.outcome && typeof item.outcome === "object"
        ? item.outcome
        : null,
      reason: nonBlank(item?.reason),
    }];
  });
  const derived = Object.fromEntries(
    Array.from(RUN_ITEM_STATUSES, (itemStatus) => [
      itemStatus,
      normalizedItems.filter((item) => item.status === itemStatus).length,
    ]),
  );
  const total = nonnegativeInteger(run?.total_items) || normalizedItems.length;
  const counts = {
    queued: runCount(run, "queued_items", derived.queued),
    running: runCount(run, "running_items", derived.running),
    verified: runCount(run, "verified_items", derived.verified),
    pendingReview: runCount(
      run,
      "pending_review_items",
      derived.pending_review,
    ),
    pendingReference: runCount(
      run,
      "pending_reference_items",
      derived.pending_reference,
    ),
    blocked: runCount(run, "blocked_items", derived.blocked),
    failed: runCount(run, "failed_items", derived.failed),
    cancelled: runCount(run, "cancelled_items", derived.cancelled),
  };
  const completed = counts.verified
    + counts.pendingReview
    + counts.pendingReference
    + counts.blocked
    + counts.failed
    + counts.cancelled;
  const currentListingId = positiveInteger(run?.current_listing_id)
    ?? normalizedItems.find((item) => item.status === "running")?.listingId
    ?? null;
  return {
    id: runId,
    status,
    terminal: status === "completed"
      || (
        status === "cancelled"
        && counts.running === 0
        && counts.queued === 0
      ),
    total,
    completed,
    currentListingId,
    counts,
    items: normalizedItems,
  };
}

export function verificationRunStatusView(status) {
  const views = {
    queued: ["Queued", "The run is waiting to start.", "pending"],
    running: ["Running", "Automatic verification is processing listings.", "running"],
    cancelling: [
      "Stopping",
      "The current listing will finish before the run stops.",
      "pending",
    ],
    completed: ["Completed", "Every selected listing reached a terminal result.", "complete"],
    cancelled: ["Cancelled", "The work was not started because the run was stopped.", "cancelled"],
    verified: ["Verified", "The listing passed identity and readiness checks.", "complete"],
    pending_review: ["Manual review", "Automatic checks left a current manual review.", "pending"],
    pending_reference: [
      "Reference pending",
      "Identity review is complete; factory reference publication remains.",
      "reference",
    ],
    blocked: ["Blocked", "Automatic verification could not safely advance this listing.", "blocked"],
    failed: ["Failed", "Automatic verification failed for this listing.", "blocked"],
  };
  const [label, detail, tone] = views[status]
    || ["Status unavailable", "Reload this verification run.", "blocked"];
  return { label, detail, tone };
}

function runCount(run, key, fallback) {
  return Number.isSafeInteger(run?.[key]) && run[key] >= 0
    ? run[key]
    : fallback;
}

function stageView(area, value) {
  const status = nonBlank(value?.status) || "unknown";
  const reasonCode = nonBlank(value?.reason_code);
  const suppliedReason = nonBlank(value?.reason);
  const reason = REASON_COPY[reasonCode] || suppliedReason || "";
  if (area === "aircraft") {
    if (AIRCRAFT_COMPLETE.has(status)) {
      return stage(status, "Verified", reason, true, "complete");
    }
    if (status === "locally_assignable") {
      return stage(status, "Ready locally", reason, false, "ready");
    }
    if (status === "rejected") {
      return stage(status, "FAA rejected", reason, false, "blocked");
    }
    return stage(status, "Verification needed", reason, false, "pending");
  }
  if (area === "avionics") {
    if (AVIONICS_COMPLETE.has(status)) {
      return stage(status, "Complete", reason, true, "complete");
    }
    if (status === "ready_retained_observations") {
      return stage(status, "Ready to check", reason, false, "ready");
    }
    if (status === "ready_legacy_reextraction") {
      return stage(status, "Re-extraction needed", reason, false, "pending");
    }
    if (status === "skipped" || status === "faa_rejected") {
      return stage(status, "Waiting for aircraft", reason, false, "blocked");
    }
    return stage(status, "Review needed", reason, false, "pending");
  }
  if (status === "ready") {
    return stage(status, "Ready", reason, true, "complete");
  }
  if (status === "pending_reference") {
    return stage(status, "Reference pending", reason, false, "reference");
  }
  if (status === "failed") {
    return stage(status, "Finalization failed", reason, false, "blocked");
  }
  return stage(status, "Waiting on identities", reason, false, "waiting");
}

function stage(status, label, reason, complete, tone) {
  return { status, label, reason, complete, tone };
}

function geminiRequirement(listing) {
  const aircraftStatus = nonBlank(listing?.aircraft?.status) || "unknown";
  const avionicsStatus = nonBlank(listing?.avionics?.status) || "unknown";
  if (aircraftStatus === "rejected" || avionicsStatus === "faa_rejected") {
    return {
      kind: "none",
      label: "Not applicable",
      detail: "FAA admission is blocked, so provider-backed identity work will not run.",
    };
  }
  if (
    !AIRCRAFT_LOCAL.has(aircraftStatus)
    || avionicsStatus === "ready_legacy_reextraction"
  ) {
    return {
      kind: "required",
      label: "Expected",
      detail: "The preflight found identity work that requires grounded provider calls.",
    };
  }
  if (
    avionicsStatus === "ready_retained_observations"
    || (
      !AVIONICS_COMPLETE.has(avionicsStatus)
      && nonnegativeInteger(listing?.avionics?.remaining_review_aspects) > 0
    )
  ) {
    return {
      kind: "possible",
      label: "Possible",
      detail: "Local catalog checks run first; unresolved identities may require Gemini.",
    };
  }
  return {
    kind: "none",
    label: "Not expected",
    detail: "The remaining provider-free stages do not currently require Gemini.",
  };
}

function listingReason(listing, aircraft, avionics, reference) {
  if (reference.status === "pending_reference") {
    return reference.reason || REASON_COPY.factory_reference_pending;
  }
  if (!aircraft.complete) {
    return aircraft.reason || "The aircraft identity still needs verification.";
  }
  if (!avionics.complete) {
    return avionics.reason || "One or more avionics observations still need verification.";
  }
  const status = nonBlank(listing?.status);
  if (status === "failed") {
    return "The provider-free verification check failed.";
  }
  return reference.reason || "The listing is waiting for final readiness checks.";
}

function nonBlank(value) {
  return typeof value === "string" && value.trim() ? value.trim() : "";
}

function positiveInteger(value) {
  return Number.isSafeInteger(value) && value > 0 ? value : null;
}

function nonnegativeInteger(value) {
  return Number.isSafeInteger(value) && value >= 0 ? value : 0;
}

function integer(value) {
  return Number.isSafeInteger(value) ? value : null;
}

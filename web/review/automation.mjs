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

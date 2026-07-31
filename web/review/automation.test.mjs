import assert from "node:assert/strict";
import test from "node:test";

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
} from "./automation.mjs";

function response(listings, contexts = []) {
  return {
    verification: {
      checkpoint: {
        has_more: false,
        resume_after_listing_id: listings.at(-1)?.listing_id ?? null,
      },
      provider_request_plan: {
        aircraft_grounding_candidates: 1,
        avionics: {
          verified_local_identity_components: 4,
          known_total_provider_requests_minimum_baseline: 3,
          known_total_provider_requests_all_positive_baseline: 6,
          known_total_provider_requests_validation_envelope_maximum: 14,
        },
        finalization_enrichment_requests_included: false,
        finalization_note: "Finalization requests are excluded.",
      },
      listings,
    },
    listing_contexts: contexts,
    services: {
      gemini_configured: true,
      faa_drs_configured: true,
    },
  };
}

const currentAircraft = {
  status: "current",
  reason_code: null,
  reason: null,
  gemini_used: false,
  catalog_writes: 0,
};

const completeAvionics = {
  status: "already_complete",
  reason_code: null,
  reason: null,
  accepted: 0,
  safely_discarded: 0,
  remaining_review_aspects: 0,
  gemini_used: false,
};

test("joins listing contexts and keeps reference-only work visible", () => {
  const rows = pipelineRowsFromResponse(response([{
    listing_id: 73,
    status: "pending_reference",
    final_ingestion_state: "incomplete",
    aircraft: currentAircraft,
    avionics: completeAvionics,
    finalization: {
      status: "pending_reference",
      reason_code: "factory_reference_pending",
      reason: "Raw backend detail",
      gemini_used: false,
      catalog_writes: 0,
    },
  }], [{
    listing_id: 73,
    label: "2007 Cessna 182T",
    registration_number: "N123AB",
    model_year: 2007,
    has_pending_review: false,
  }]));

  assert.equal(rows.length, 1);
  assert.equal(rows[0].label, "2007 Cessna 182T");
  assert.equal(rows[0].registrationNumber, "N123AB");
  assert.equal(rows[0].reference.label, "Reference pending");
  assert.equal(rows[0].hasPendingReview, false);
  assert.match(rows[0].reason, /factory reference data/);
  assert.equal(rows[0].gemini.kind, "none");
});

test("falls back safely when listing context is absent", () => {
  const rows = pipelineRowsFromResponse(response([{
    listing_id: 20,
    status: "blocked",
    final_ingestion_state: "pending_review",
    aircraft: {
      ...currentAircraft,
      status: "pending",
      reason_code: "canonical_identity_assignment_missing",
    },
    avionics: {
      ...completeAvionics,
      status: "skipped",
      remaining_review_aspects: 3,
    },
    finalization: {
      ...currentAircraft,
      status: "not_attempted",
    },
  }]));

  assert.equal(rows[0].label, "Listing #20");
  assert.equal(rows[0].aircraft.label, "Verification needed");
  assert.equal(rows[0].gemini.kind, "required");
  assert.equal(rows[0].hasPendingReview, false);
});

test("classifies retained avionics as possibly using Gemini and manual only from context", () => {
  const rows = pipelineRowsFromResponse(response([{
    listing_id: 21,
    status: "pending_review",
    final_ingestion_state: "pending_review",
    aircraft: currentAircraft,
    avionics: {
      ...completeAvionics,
      status: "ready_retained_observations",
      reason_code: "automatic_verification_available",
      remaining_review_aspects: 4,
    },
    finalization: {
      ...currentAircraft,
      status: "not_attempted",
    },
  }], [{
    listing_id: 21,
    label: "Listing 21",
    has_pending_review: true,
  }]));

  assert.equal(rows[0].gemini.kind, "possible");
  assert.equal(rows[0].hasPendingReview, true);
  assert.equal(pipelineSummary(rows).manualReview, 1);
});

test("does not claim Gemini work for an FAA-rejected aircraft", () => {
  const rows = pipelineRowsFromResponse(response([{
    listing_id: 10,
    status: "blocked",
    final_ingestion_state: "pending_review",
    aircraft: {
      ...currentAircraft,
      status: "rejected",
      reason_code: "faa_rejected",
    },
    avionics: {
      ...completeAvionics,
      status: "skipped",
      reason_code: "faa_rejected",
      remaining_review_aspects: 4,
    },
    finalization: {
      ...currentAircraft,
      status: "not_attempted",
    },
  }]));

  assert.equal(rows[0].gemini.kind, "none");
  assert.equal(rows[0].gemini.label, "Not applicable");
});

test("groups preflight rows into four exclusive operator backlog categories", () => {
  const rows = pipelineRowsFromResponse(response([
    {
      listing_id: 31,
      status: "pending_review",
      final_ingestion_state: "pending_review",
      aircraft: currentAircraft,
      avionics: {
        ...completeAvionics,
        status: "ready_retained_observations",
        remaining_review_aspects: 2,
      },
      finalization: { ...currentAircraft, status: "not_attempted" },
    },
    {
      listing_id: 32,
      status: "pending_review",
      final_ingestion_state: "pending_review",
      aircraft: currentAircraft,
      avionics: {
        ...completeAvionics,
        status: "ready_legacy_reextraction",
        remaining_review_aspects: 1,
      },
      finalization: { ...currentAircraft, status: "not_attempted" },
    },
    {
      listing_id: 33,
      status: "blocked",
      final_ingestion_state: "pending_review",
      aircraft: { ...currentAircraft, status: "rejected" },
      avionics: {
        ...completeAvionics,
        status: "ready_retained_observations",
        remaining_review_aspects: 3,
      },
      finalization: { ...currentAircraft, status: "not_attempted" },
    },
    {
      listing_id: 34,
      status: "pending_reference",
      final_ingestion_state: "incomplete",
      aircraft: currentAircraft,
      avionics: completeAvionics,
      finalization: { ...currentAircraft, status: "pending_reference" },
    },
    {
      listing_id: 35,
      status: "blocked",
      final_ingestion_state: "pending_review",
      aircraft: currentAircraft,
      avionics: { ...completeAvionics, status: "skipped" },
      finalization: { ...currentAircraft, status: "not_attempted" },
    },
    {
      listing_id: 36,
      status: "pending_reference",
      final_ingestion_state: "incomplete",
      aircraft: currentAircraft,
      avionics: { ...completeAvionics, status: "already_verified" },
      finalization: { ...currentAircraft, status: "pending_reference" },
    },
  ]));

  assert.deepEqual(
    pipelineBacklogCategories(rows).map(({ key, count }) => ({ key, count })),
    [
      { key: "currentAvionicsReview", count: 1 },
      { key: "legacyReextraction", count: 1 },
      { key: "faaBlocked", count: 2 },
      { key: "referencePending", count: 1 },
    ],
  );
});

test("provides plain-language next steps for every backlog category", () => {
  const categories = pipelineBacklogCategories([]);
  assert.deepEqual(categories.map(({ label }) => label), [
    "Current avionics review",
    "One-time avionics re-extraction",
    "FAA admission blocked",
    "Factory reference pending",
  ]);
  for (const category of categories) {
    assert.ok(category.description.length > 40);
    assert.doesNotMatch(category.description, /[_`]/);
  }
});

test("filters by stage, Gemini expectation, and free text", () => {
  const rows = [
    {
      listingId: 10,
      label: "Cessna 182",
      registrationNumber: "N10",
      hasPendingReview: true,
      aircraft: { complete: false, label: "Verification needed" },
      avionics: { complete: false, label: "Waiting" },
      reference: { status: "not_attempted", label: "Waiting" },
      gemini: { kind: "required", label: "Expected" },
      reason: "Serial conflict",
    },
    {
      listingId: 73,
      label: "Cirrus SR22",
      registrationNumber: "N73",
      hasPendingReview: false,
      aircraft: { complete: true, label: "Verified" },
      avionics: { complete: true, label: "Complete" },
      reference: { status: "pending_reference", label: "Reference pending" },
      gemini: { kind: "none", label: "Not expected" },
      reason: "Factory reference pending",
    },
  ];

  assert.deepEqual(filterPipelineRows(rows, "manual").map((row) => row.listingId), [10]);
  assert.deepEqual(filterPipelineRows(rows, "reference").map((row) => row.listingId), [73]);
  assert.deepEqual(filterPipelineRows(rows, "gemini").map((row) => row.listingId), [10]);
  assert.deepEqual(filterPipelineRows(rows, "all", "sr22").map((row) => row.listingId), [73]);
});

test("sums provider plans across checkpoint pages without claiming provider work occurred", () => {
  const first = response([]);
  const second = response([]);
  second.verification.provider_request_plan.aircraft_grounding_candidates = 2;
  second.verification.provider_request_plan.avionics.verified_local_identity_components = 5;
  second.verification.provider_request_plan.avionics.known_total_provider_requests_minimum_baseline = 7;
  second.verification.provider_request_plan.avionics.known_total_provider_requests_all_positive_baseline = 8;
  second.verification.provider_request_plan.avionics.known_total_provider_requests_validation_envelope_maximum = 20;

  assert.deepEqual(pipelineProviderPlan([first, second]), {
    aircraftGroundingCandidates: 3,
    verifiedLocalIdentityComponents: 9,
    minimumBaselineRequests: 10,
    allPositiveBaselineRequests: 14,
    validationEnvelopeMaximum: 34,
    includesFinalizationEnrichment: false,
    notes: ["Finalization requests are excluded."],
  });
});

test("requires a usable numeric checkpoint when another page exists", () => {
  const valid = response([]);
  valid.verification.checkpoint = {
    has_more: true,
    resume_after_listing_id: 100,
  };
  assert.deepEqual(pipelineCheckpoint(valid), {
    hasMore: true,
    resumeAfterListingId: 100,
    valid: true,
  });

  const invalid = structuredClone(valid);
  invalid.verification.checkpoint.resume_after_listing_id = null;
  assert.deepEqual(pipelineCheckpoint(invalid), {
    hasMore: true,
    resumeAfterListingId: null,
    valid: false,
  });
});

test("surfaces missing provider services only when the plan needs them", () => {
  const unavailable = response([]);
  unavailable.services = {
    gemini_configured: false,
    faa_drs_configured: false,
  };
  const status = pipelineServiceStatus([unavailable]);
  assert.equal(status.geminiConfigured, false);
  assert.equal(status.faaDrsConfigured, false);
  assert.equal(status.warnings.length, 2);
  assert.match(status.warnings[0], /FAA_DRS_API_KEY/);

  const local = response([]);
  local.services = {
    gemini_configured: false,
    faa_drs_configured: false,
  };
  local.verification.provider_request_plan.aircraft_grounding_candidates = 0;
  local.verification.provider_request_plan.avionics.known_total_provider_requests_validation_envelope_maximum = 0;
  assert.deepEqual(pipelineServiceStatus([local]).warnings, []);
});

test("submits only unique sorted positive listing IDs", () => {
  assert.deepEqual(verificationRunRequest([73, 10, 73, 0, "20", null]), {
    listing_ids: [10, 73],
  });
});

test("creates a secure idempotency key with an insecure-context fallback", () => {
  assert.equal(
    verificationRunIdempotencyKey({
      randomUUID: () => "00000000-0000-4000-8000-000000000001",
    }),
    "00000000-0000-4000-8000-000000000001",
  );
  const fallback = verificationRunIdempotencyKey({
    getRandomValues(bytes) {
      bytes.fill(0xab);
      return bytes;
    },
  });
  assert.match(
    fallback,
    /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
  );
  assert.throws(
    () => verificationRunIdempotencyKey({}),
    /Secure random values/,
  );
});

test("excludes reference-only and deterministically FAA-rejected rows", () => {
  const base = {
    status: "pending_review",
    finalIngestionState: "pending_review",
    aircraft: { status: "current" },
    avionics: { status: "ready_retained_observations" },
    reference: { status: "not_attempted" },
  };
  assert.equal(pipelineAutomaticEligibility(base).eligible, true);
  assert.equal(pipelineAutomaticEligibility({
    ...base,
    status: "pending_reference",
    reference: { status: "pending_reference" },
  }).eligible, false);
  assert.equal(pipelineAutomaticEligibility({
    ...base,
    aircraft: { status: "rejected" },
  }).eligible, false);
});

test("normalizes durable run progress and terminal item outcomes", () => {
  const view = verificationRunState({
    id: 9,
    status: "running",
    total_items: 4,
    queued_items: 1,
    running_items: 1,
    verified_items: 1,
    pending_review_items: 1,
    pending_reference_items: 0,
    blocked_items: 0,
    failed_items: 0,
    cancelled_items: 0,
    current_listing_id: 21,
  }, [
    { id: 1, listing_id: 10, status: "verified", outcome: { status: "verified" } },
    { id: 2, listing_id: 20, status: "pending_review" },
    { id: 3, listing_id: 21, status: "running" },
    { id: 4, listing_id: 22, status: "queued" },
  ]);
  assert.equal(view.id, 9);
  assert.equal(view.terminal, false);
  assert.equal(view.completed, 2);
  assert.equal(view.currentListingId, 21);
  assert.equal(view.counts.pendingReview, 1);
  assert.equal(view.items[0].outcome.status, "verified");
});

test("recognizes stopped runs and provides accessible status copy", () => {
  const view = verificationRunState({
    id: 11,
    status: "cancelled",
    total_items: 2,
    cancelled_items: 1,
  }, [
    { id: 1, listing_id: 10, status: "blocked", reason: "FAA mismatch" },
    { id: 2, listing_id: 20, status: "cancelled" },
  ]);
  assert.equal(view.terminal, true);
  assert.equal(view.counts.cancelled, 1);
  assert.equal(verificationRunStatusView("pending_reference").label, "Reference pending");
  assert.equal(verificationRunStatusView("future").label, "Status unavailable");
});

test("keeps a cancelling run nonterminal until the current listing finishes", () => {
  const view = verificationRunState({
    id: 12,
    status: "cancelling",
    total_items: 2,
    queued_items: 0,
    running_items: 1,
    cancelled_items: 1,
    current_listing_id: 73,
  }, [
    { id: 1, listing_id: 73, status: "running" },
    { id: 2, listing_id: 74, status: "cancelled" },
  ]);

  assert.equal(view.terminal, false);
  assert.equal(view.currentListingId, 73);
  assert.deepEqual(verificationRunStatusView(view.status), {
    label: "Stopping",
    detail: "The current listing will finish before the run stops.",
    tone: "pending",
  });
});

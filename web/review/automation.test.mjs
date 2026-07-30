import assert from "node:assert/strict";
import test from "node:test";

import {
  filterPipelineRows,
  pipelineCheckpoint,
  pipelineProviderPlan,
  pipelineRowsFromResponse,
  pipelineServiceStatus,
  pipelineSummary,
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

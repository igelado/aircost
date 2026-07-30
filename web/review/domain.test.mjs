import assert from "node:assert/strict";
import test from "node:test";

import {
  REVIEW_AREAS,
  REVIEW_PRODUCT_IDENTITY_LIMITS,
  aircraftIdentityIsVerified,
  associationsNeedingSourceRecovery,
  automaticListingVerificationRequest,
  authoritativeIdentityUrl,
  autoVerifiableProductAssociations,
  characterLimitState,
  describeAircraftIdentity,
  describeAutomaticListingVerificationError,
  describeAutomaticListingVerificationOutcome,
  describeProductAssociationOutcome,
  describeResolvedListingOutcome,
  describeReviewReasons,
  existingProductVerificationRequest,
  groupProductAssociationsByListing,
  isAircraftIdentityStatus,
  isCompletedReviewMaintenanceResponse,
  preselectedReviewAction,
  productAssociationEvidenceDisplay,
  productAssociationEligibilityOutcome,
  productAssociationEligibilityOutcomeForAttestation,
  productAssociationReviewBucket,
  productActionContextIsCurrent,
  productAttestationDraft,
  productDetailRequestMayCommit,
  reviewAreaForAspect,
  reviewProductIdentitySourceValidation,
  runProductAssociationWorkers,
  summarizeProductAssociations,
  summarizeProductReviewGroups,
} from "./domain.mjs";

const PRODUCTION_REASON_CODES = [
  "listing_action_graph_invalid",
  "raw_observation_unlinked",
  "catalog_product_unverified",
  "catalog_collision_consolidated_pending_identity_verification",
  "listing_link_confidence_not_high",
  "configuration_action_mismatch",
  "quantity_mismatch",
  "capability_mismatch_or_unknown",
  "replacement_identity_mismatch",
  "raw_observation_ambiguous",
  "approved_high_confidence_link_unmatched_by_retained_observation",
  "catalog_product_unverified_without_raw_observation",
  "approved_link_confidence_not_high",
  "replacement_association_requires_review",
  "replacement_product_unverified",
  "approved_high_confidence_replacement_unmatched_by_retained_observation",
  "replacement_association_confidence_not_high",
  "raw_observation_not_an_object",
  "raw_observation_identity_unusable",
  "raw_observation_quantity_invalid",
  "raw_observation_configuration_action_invalid",
  "installed_observation_has_replacement",
  "replacement_identity_missing",
  "raw_observation_source_confidence_invalid",
  "replacement_identity_unusable",
  "raw_observation_types_member_invalid",
  "raw_observation_types_not_array",
  "raw_observation_legacy_type_invalid",
  "raw_observation_capability_missing",
  "raw_observation_capability_unrecognized",
];

const LIVE_REASON_CODES = [
  "listing_action_graph_invalid",
  "catalog_product_unverified",
  "listing_link_confidence_not_high",
  "raw_observation_unlinked",
  "catalog_product_unverified_without_raw_observation",
  "capability_mismatch_or_unknown",
  "raw_observation_capability_unrecognized",
  "raw_observation_ambiguous",
  "raw_observation_identity_unusable",
];

test("builds one canonical source-free request for every existing-product aspect", () => {
  const expected = {
    review_payload_sha256: "a".repeat(64),
    catalog_revision_sha256: "b".repeat(64),
    aspect_id: "observation-17",
  };
  assert.deepEqual(
    existingProductVerificationRequest(
      expected.review_payload_sha256,
      expected.catalog_revision_sha256,
      expected.aspect_id,
    ),
    expected,
  );
  assert.deepEqual(Object.keys(expected).sort(), [
    "aspect_id",
    "catalog_revision_sha256",
    "review_payload_sha256",
  ]);
});

test("builds the exact automatic listing verification request", () => {
  assert.deepEqual(
    automaticListingVerificationRequest("a".repeat(64), "b".repeat(64)),
    {
      review_payload_sha256: "a".repeat(64),
      catalog_revision_sha256: "b".repeat(64),
    },
  );
});

test("accepts automatic success only when the listing is reported ready", () => {
  const verified = {
    verification: {
      listing_id: 23,
      status: "verified",
      initial_ingestion_state: "pending_review",
      final_ingestion_state: "ready",
      aircraft: { status: "reused", gemini_used: false },
      avionics: {
        status: "verified",
        accepted: 2,
        safely_discarded: 1,
        remaining_review_aspects: 0,
        gemini_used: true,
      },
      finalization: { status: "verified" },
    },
  };
  assert.deepEqual(
    describeAutomaticListingVerificationOutcome(verified, 23),
    {
      kind: "verified",
      label: "Listing verified automatically",
      detail: "The aircraft and avionics checks passed, and the listing is now verified.",
      listingId: 23,
      terminal: true,
      stale: false,
      focusArea: null,
    },
  );

  const incomplete = structuredClone(verified);
  incomplete.verification.final_ingestion_state = "incomplete";
  const conservative = describeAutomaticListingVerificationOutcome(incomplete, 23);
  assert.equal(conservative.terminal, false);
  assert.equal(conservative.kind, "blocked");
  assert.match(conservative.detail, /remains unverified/);
});

test("describes residual automatic aircraft and avionics blockers", () => {
  const aircraft = describeAutomaticListingVerificationOutcome({
    verification: {
      listing_id: 20,
      status: "blocked",
      initial_ingestion_state: "pending_review",
      final_ingestion_state: "pending_review",
      aircraft: {
        status: "blocked",
        reason_code: "canonical_identity_assignment_missing",
        gemini_used: true,
      },
      avionics: {
        status: "not_run",
        accepted: 0,
        safely_discarded: 0,
        remaining_review_aspects: 3,
        gemini_used: false,
      },
      finalization: { status: "not_run" },
    },
  }, 20);
  assert.equal(aircraft.kind, "blocked");
  assert.equal(aircraft.focusArea, "aircraft");
  assert.match(aircraft.detail, /canonical catalog assignment/);

  const avionics = describeAutomaticListingVerificationOutcome({
    verification: {
      listing_id: 21,
      status: "pending_review",
      initial_ingestion_state: "pending_review",
      final_ingestion_state: "pending_review",
      aircraft: { status: "current", gemini_used: false },
      avionics: {
        status: "pending_review",
        reason_code: "source_evidence_missing",
        accepted: 2,
        safely_discarded: 1,
        remaining_review_aspects: 4,
        gemini_used: true,
      },
      finalization: { status: "not_run" },
    },
  }, 21);
  assert.equal(avionics.kind, "pending");
  assert.equal(avionics.focusArea, "avionics");
  assert.match(avionics.detail, /exact source evidence/);

  const finalization = describeAutomaticListingVerificationOutcome({
    verification: {
      listing_id: 22,
      status: "failed",
      initial_ingestion_state: "incomplete",
      final_ingestion_state: "quarantined",
      aircraft: { status: "current", gemini_used: false },
      avionics: {
        status: "already_complete",
        accepted: 0,
        safely_discarded: 0,
        remaining_review_aspects: 0,
        gemini_used: false,
      },
      finalization: {
        status: "failed",
        reason_code: "listing_finalization_failed",
      },
    },
  }, 22);
  assert.equal(finalization.kind, "failed");
  assert.match(finalization.detail, /valuation enrichment/);
});

test("treats pending factory reference work as terminal for the manual review queue", () => {
  const outcome = describeAutomaticListingVerificationOutcome({
    verification: {
      listing_id: 22,
      status: "pending_reference",
      initial_ingestion_state: "pending_review",
      final_ingestion_state: "incomplete",
      aircraft: { status: "current", gemini_used: false },
      avionics: {
        status: "already_complete",
        accepted: 2,
        safely_discarded: 1,
        remaining_review_aspects: 0,
        gemini_used: false,
      },
      finalization: {
        status: "pending_reference",
        reason_code: "factory_reference_pending",
        reason: "No published model-year factory configuration is available.",
      },
    },
  }, 22);

  assert.deepEqual(outcome, {
    kind: "pending_reference",
    label: "Review complete — factory reference pending",
    detail: "The aircraft and avionics review is complete, but factory reference data still needs automated curation before valuation is available.",
    listingId: 22,
    terminal: true,
    stale: false,
    focusArea: null,
  });
});

test("describes manual resolution from the returned listing state", () => {
  assert.deepEqual(
    describeResolvedListingOutcome({
      listing: { id: 23, ingestion_state: "ready", is_verified: true },
    }, 23),
    {
      kind: "verified",
      label: "Listing 23 verified",
      detail: "The review and valuation-readiness checks are complete.",
      listingId: 23,
      terminal: true,
    },
  );
  assert.deepEqual(
    describeResolvedListingOutcome({
      listing: { id: 24, ingestion_state: "incomplete", is_verified: false },
    }, 24),
    {
      kind: "pending_reference",
      label: "Listing 24 review complete; factory reference pending before valuation",
      detail: "The aircraft and avionics review is complete, but factory reference data still needs automated curation before valuation is available.",
      listingId: 24,
      terminal: true,
    },
  );
  for (const payload of [
    {},
    { listing: { id: 24, ingestion_state: "quarantined", is_verified: false } },
    { listing: { id: 25, ingestion_state: "ready", is_verified: true } },
  ]) {
    const outcome = describeResolvedListingOutcome(payload, 24);
    assert.equal(outcome.kind, "invalid");
    assert.equal(outcome.terminal, false);
  }
});

test("never treats stale, mismatched, or unknown automatic results as success", () => {
  const stale = describeAutomaticListingVerificationOutcome({
    verification: {
      listing_id: 23,
      status: "stale",
      initial_ingestion_state: "pending_review",
      final_ingestion_state: "pending_review",
      aircraft: { status: "current" },
      avionics: { status: "stale", remaining_review_aspects: 2 },
      finalization: { status: "not_run" },
    },
  }, 23);
  assert.equal(stale.stale, true);
  assert.equal(stale.terminal, false);

  for (const malformed of [
    null,
    {},
    { verification: null },
    { verification: { listing_id: 23, status: "future_status" } },
    {
      verification: {
        listing_id: 24,
        status: "verified",
        final_ingestion_state: "ready",
      },
    },
  ]) {
    const outcome = describeAutomaticListingVerificationOutcome(malformed, 23);
    assert.equal(outcome.terminal, false);
    assert.equal(outcome.kind, "blocked");
  }
});

test("maps automatic verification API errors to safe user-facing outcomes", () => {
  assert.equal(describeAutomaticListingVerificationError({
    status: 412,
    payload: { error: { code: "automatic_verification_stale" } },
  }).kind, "stale");
  assert.equal(describeAutomaticListingVerificationError({
    status: 409,
    payload: { error: { code: "automatic_verification_in_progress" } },
  }).kind, "in_progress");
  assert.equal(describeAutomaticListingVerificationError({
    status: 503,
    payload: { error: { code: "automatic_verification_unavailable" } },
  }).kind, "unavailable");
  const unknown = describeAutomaticListingVerificationError(
    new Error("internal database secret"),
  );
  assert.equal(unknown.detail.includes("secret"), false);
  assert.match(unknown.detail, /not reported as verified/);
});

test("publishes the review product identity character limits", () => {
  assert.deepEqual(REVIEW_PRODUCT_IDENTITY_LIMITS, {
    sourceTitle: 200,
    evidenceText: 128,
  });
});

test("counts review field characters and exposes prefilled over-limit state", () => {
  assert.deepEqual(characterLimitState("é😀", 3), {
    count: 2,
    limit: 3,
    remaining: 1,
    overLimit: false,
  });
  assert.deepEqual(characterLimitState("x".repeat(129), 128), {
    count: 129,
    limit: 128,
    remaining: -1,
    overLimit: true,
  });
});

test("validates review identity source fields at the exact server boundaries", () => {
  assert.deepEqual(reviewProductIdentitySourceValidation(" ", "identity"), {
    valid: false,
    message: "An authoritative identity source title is required.",
  });
  assert.deepEqual(reviewProductIdentitySourceValidation("identity", " "), {
    valid: false,
    message: "Exact identity evidence is required.",
  });
  assert.deepEqual(
    reviewProductIdentitySourceValidation("x".repeat(200), "é".repeat(128)),
    {
      valid: true,
      message: "Identity source fields are within their character limits.",
    },
  );
  assert.deepEqual(
    reviewProductIdentitySourceValidation("x".repeat(201), "identity"),
    {
      valid: false,
      message: "Authoritative identity source title must contain at most 200 characters.",
    },
  );
  assert.deepEqual(
    reviewProductIdentitySourceValidation("identity", "x".repeat(129)),
    {
      valid: false,
      message: "Exact identity evidence must contain at most 128 characters.",
    },
  );
});

test("prepares fresh product attestation fields without replaying catalog evidence", () => {
  const draft = productAttestationDraft({
    identity_source_url: "https://www.garmin.com/aviation/product",
    identity_source_title: "Garmin G1000 NXi product identity",
    identity_evidence_text: "x".repeat(158),
  });
  assert.deepEqual(draft, {
    sourceUrl: "https://www.garmin.com/aviation/product",
    sourceTitle: "Garmin G1000 NXi product identity",
    evidenceText: "",
  });
  assert.equal(authoritativeIdentityUrl("http://www.garmin.com/product"), false);
  assert.deepEqual(productAttestationDraft({
    identity_source_url: "https://market.example/listings/unit",
    identity_source_title: "x".repeat(201),
    identity_evidence_text: "short historical evidence",
  }), {
    sourceUrl: "",
    sourceTitle: "",
    evidenceText: "",
  });
});

test("describes and filters server-supplied product association eligibility", () => {
  const associations = [
    {
      listing_id: 1,
      verification_eligibility: { status: "auto_verifiable" },
    },
    {
      listing_id: 2,
      verification_eligibility: {
        status: "product_attestation_required",
        reason_code: "product_attestation_required",
        reason: "Attest this product first.",
      },
    },
    {
      listing_id: 3,
      verification_eligibility: {
        status: "manual_review_required",
        reason_code: "catalog_identity_ambiguous",
        reason: "The exact product is ambiguous.",
      },
    },
  ];
  assert.deepEqual(autoVerifiableProductAssociations(associations), [associations[0]]);
  assert.deepEqual(productAssociationEligibilityOutcome(associations[0]), {
    kind: "ready",
    label: "Ready for local validation",
    detail: "The retained listing proof uniquely identifies the current approved product.",
  });
  assert.deepEqual(productAssociationEligibilityOutcome(associations[1]), {
    kind: "required",
    label: "OEM attestation required",
    detail: "Attest this product once from an OEM source before validating its listing associations.",
  });
  assert.deepEqual(productAssociationEligibilityOutcome(associations[2]), {
    kind: "manual",
    label: "Manual or ambiguous",
    detail: "The retained listing evidence matches more than one possible product.",
  });
});

test("summarizes source recovery separately from ambiguous manual work", () => {
  const associations = [
    {
      listing_id: 1,
      source_evidence_text: "Garmin GTX 345",
      verification_eligibility: { status: "auto_verifiable" },
    },
    {
      listing_id: 2,
      source_evidence_text: null,
      verification_eligibility: {
        status: "manual_review_required",
        reason_code: "source_evidence_missing",
      },
    },
    {
      listing_id: 3,
      source_evidence_text: "Garmin GTX 33 ADS-B",
      verification_eligibility: {
        status: "manual_review_required",
        reason_code: "identity_or_capability_qualifier_unresolved",
      },
    },
  ];
  assert.deepEqual(summarizeProductAssociations(associations, "current"), {
    total: 3,
    readyLocal: 1,
    needsSourceRecovery: 1,
    productAttestationRequired: 0,
    manualOrAmbiguous: 1,
  });
  assert.deepEqual(associationsNeedingSourceRecovery(associations, "current"), [
    associations[1],
  ]);
  assert.equal(
    productAssociationEligibilityOutcomeForAttestation(
      associations[1],
      "current",
    ).label,
    "Needs source recovery",
  );
  assert.equal(
    productAssociationEligibilityOutcomeForAttestation(
      associations[2],
      "current",
    ).detail,
    "The listing includes a model variant or capability qualifier that may identify a different product.",
  );
});

test("requires product attestation before classifying blank source evidence for recovery", () => {
  const association = {
    source_evidence_text: null,
    verification_eligibility: {
      status: "manual_review_required",
      reason_code: "source_evidence_missing",
    },
  };
  assert.equal(
    productAssociationReviewBucket(association, "required"),
    "product_attestation_required",
  );
  assert.deepEqual(summarizeProductAssociations([association], "required"), {
    total: 1,
    readyLocal: 0,
    needsSourceRecovery: 0,
    productAttestationRequired: 1,
    manualOrAmbiguous: 0,
  });
});

test("sums queue eligibility counts and degrades missing breakdowns safely", () => {
  assert.deepEqual(summarizeProductReviewGroups([
    {
      pending_association_count: 6,
      attestation_status: "current",
      eligibility_counts: {
        ready_local: 1,
        source_evidence_missing: 2,
        product_attestation_required: 0,
        manual_review_required: 3,
      },
    },
    {
      pending_association_count: 2,
      attestation_status: "required",
    },
    {
      pending_association_count: 4,
      attestation_status: "current",
    },
  ]), {
    total: 12,
    readyLocal: 1,
    needsSourceRecovery: 2,
    productAttestationRequired: 2,
    manualOrAmbiguous: 7,
  });
});

test("keeps observed text distinct from retained product-association evidence", () => {
  assert.deepEqual(productAssociationEvidenceDisplay({
    observed_text: "Garmin GTN 750",
    source_evidence_text: null,
  }), {
    observedText: "Garmin GTN 750",
    sourceEvidenceText: "No retained source evidence",
  });
  assert.deepEqual(productAssociationEvidenceDisplay({
    observed_text: "Garmin GTN 750",
    source_evidence_text: "Dual Garmin GTN 750 navigators",
  }), {
    observedText: "Garmin GTN 750",
    sourceEvidenceText: "Dual Garmin GTN 750 navigators",
  });
});

test("describes a consolidated catalog collision as pending identity verification", () => {
  assert.deepEqual(
    describeReviewReasons(
      "catalog_collision_consolidated_pending_identity_verification",
    ),
    [
      {
        code: "catalog_collision_consolidated_pending_identity_verification",
        label: "Product identity still needs verification",
        detail:
          "Duplicate catalog entries were consolidated, but the surviving product identity has not been verified yet.",
        isListingLevel: false,
      },
    ],
  );
});

test("exports the two exact review areas and classifies only known aspect kinds", () => {
  assert.deepEqual(REVIEW_AREAS, ["aircraft", "avionics"]);
  assert.equal(reviewAreaForAspect({ kind: "aircraft" }), "aircraft");
  assert.equal(reviewAreaForAspect({ kind: "avionics" }), "avionics");
  assert.equal(reviewAreaForAspect({ kind: "avionics_identity" }), "avionics");
  assert.equal(reviewAreaForAspect({ kind: "avionics_candidate" }), "avionics");
  assert.equal(reviewAreaForAspect({ kind: "Avionics" }), null);
  assert.equal(reviewAreaForAspect({ kind: "avionic" }), null);
  assert.equal(reviewAreaForAspect({}), null);
  assert.equal(reviewAreaForAspect(null), null);
});

test("preselects use-verified only for an explicit positive suggested product", () => {
  assert.equal(
    preselectedReviewAction({
      allowed_actions: [
        "use_verified_product",
        "create_verified_product",
        "discard",
      ],
      suggested_product: { id: 239, manufacturer: "Garmin", model: "GIA 63W" },
    }),
    "use_verified_product",
  );
  for (const aspect of [
    {
      allowed_actions: ["create_verified_product", "discard"],
      suggested_product: { id: 239 },
    },
    {
      allowed_actions: ["use_verified_product"],
      proposed_product: { id: 239 },
    },
    {
      allowed_actions: ["use_verified_product"],
      suggested_product: { id: "239" },
    },
    {
      allowed_actions: ["use_verified_product"],
      suggested_product: { id: 0 },
    },
    null,
  ]) {
    assert.equal(preselectedReviewAction(aspect), null);
  }
});

test("validates strict aircraft identity status and distinguishes verified reviews", () => {
  const verified = {
    status: "verified",
    faa_n_number: "N89225",
    faa_snapshot_id: 2,
  };
  const pending = {
    status: "curation_required",
    reason_code: "canonical_identity_assignment_missing",
    faa_n_number: "N89225",
    faa_snapshot_id: 2,
  };

  assert.equal(isAircraftIdentityStatus(verified), true);
  assert.equal(aircraftIdentityIsVerified(verified), true);
  assert.equal(isAircraftIdentityStatus(pending), true);
  assert.equal(aircraftIdentityIsVerified(pending), false);
  assert.equal(isAircraftIdentityStatus({ status: "verified", reason_code: "mismatch" }), false);
  assert.equal(isAircraftIdentityStatus({ status: "future_status" }), false);
  assert.equal(isAircraftIdentityStatus(null), false);
});

test("recognizes only the terminal review-maintenance response", () => {
  assert.equal(
    isCompletedReviewMaintenanceResponse({
      review: null,
      review_complete: true,
    }),
    true,
  );
  for (const response of [
    { review: { listing_id: 23 }, review_complete: false },
    { review: null, review_complete: false },
    { review: null },
    { review_complete: true },
    { review: {}, review_complete: true },
    null,
    [],
  ]) {
    assert.equal(isCompletedReviewMaintenanceResponse(response), false);
  }
});

test("describes canonical and FAA aircraft blockers without exposing unknown backend codes", () => {
  const missing = describeAircraftIdentity({
    status: "curation_required",
    reason_code: "canonical_identity_assignment_missing",
    faa_n_number: "N89225",
    faa_snapshot_id: 2,
  });
  assert.equal(missing.label, "Curation required");
  assert.equal(missing.isBlocking, true);
  assert.match(missing.detail, /canonical catalog assignment/);
  assert.match(missing.detail, /before this listing can be verified/);

  const rawFaaBlocker = describeAircraftIdentity({
    status: "curation_required",
    reason_code: "serial_conflict",
  });
  assert.match(rawFaaBlocker.detail, /serial number conflicts/);

  const secret = "database_failure_with_secret_481";
  const unknown = describeAircraftIdentity({
    status: "curation_required",
    reason_code: secret,
  });
  assert.equal(unknown.detail.includes(secret), false);
  assert.match(unknown.detail, /could not be verified automatically/);

  assert.deepEqual(describeAircraftIdentity({
    status: "verified",
    faa_n_number: "N89225",
    faa_snapshot_id: 2,
  }), {
    label: "FAA identity verified",
    detail: "The listing has a current FAA-backed canonical aircraft assignment.",
    isBlocking: false,
  });
});

test("describes the current multi-code sample with safe user-facing copy", () => {
  assert.deepEqual(
    describeReviewReasons(
      "listing_action_graph_invalid,catalog_product_unverified,listing_link_confidence_not_high",
    ),
    [
      {
        code: "listing_action_graph_invalid",
        label: "Equipment relationships are inconsistent",
        detail: "The stored installed, replaced, or removed equipment relationships conflict.",
        isListingLevel: true,
      },
      {
        code: "catalog_product_unverified",
        label: "Product is not verified",
        detail: "The matched catalog product has not been verified yet.",
        isListingLevel: false,
      },
      {
        code: "listing_link_confidence_not_high",
        label: "Match needs confirmation",
        detail: "The existing product match is not supported with high confidence.",
        isListingLevel: false,
      },
    ],
  );
});

test("maps every production code, including every code present in the live database", () => {
  for (const code of PRODUCTION_REASON_CODES) {
    const [description] = describeReviewReasons(code);
    assert.ok(description, `missing description for ${code}`);
    assert.equal(description.code, code);
    assert.notEqual(description.label, "Manual review required", `unmapped code: ${code}`);
    assert.ok(description.detail.length > 0, `blank detail for ${code}`);
  }
  for (const code of LIVE_REASON_CODES) {
    assert.notEqual(describeReviewReasons(code)[0].label, "Manual review required");
  }
});

test("does not split mixed prose and never reflects unknown backend text into UI copy", () => {
  const secret = "Database timeout for catalog id 481, retry with internal token SECRET";
  const [prose] = describeReviewReasons(secret);
  assert.deepEqual(prose, {
    code: "unclassified_review_reason",
    label: "Manual review required",
    detail: "The automated review could not verify this item with enough confidence.",
    isListingLevel: false,
  });
  assert.equal(JSON.stringify(prose).includes("SECRET"), false);
  assert.equal(JSON.stringify(prose).includes("481"), false);

  const mixed = describeReviewReasons(
    "catalog_product_unverified,Internal validation failed",
  );
  assert.deepEqual(mixed, [prose]);

  const [unknownCode] = describeReviewReasons("future_backend_reason");
  assert.equal(unknownCode.code, "future_backend_reason");
  assert.equal(unknownCode.label, "Manual review required");
  assert.equal(unknownCode.detail.includes("future_backend_reason"), false);
});

test("supports arrays and deduplicates repeated and equivalent reasons", () => {
  assert.deepEqual(
    describeReviewReasons([
      "listing_link_confidence_not_high",
      "approved_link_confidence_not_high",
      "listing_link_confidence_not_high,raw_observation_unlinked",
      ["raw_observation_unlinked"],
    ]),
    [
      {
        code: "listing_link_confidence_not_high",
        label: "Match needs confirmation",
        detail: "The existing product match is not supported with high confidence.",
        isListingLevel: false,
      },
      {
        code: "raw_observation_unlinked",
        label: "No catalog match",
        detail: "No catalog product is linked to this listing item.",
        isListingLevel: false,
      },
    ],
  );
});

test("marks only the listing action graph reason as listing-level", () => {
  for (const code of PRODUCTION_REASON_CODES) {
    const [description] = describeReviewReasons(code);
    assert.equal(
      description.isListingLevel,
      code === "listing_action_graph_invalid",
      code,
    );
  }
});

test("ignores blank and unsupported values", () => {
  assert.deepEqual(describeReviewReasons(""), []);
  assert.deepEqual(describeReviewReasons([" ", null, 42, {}]), []);
});

test("groups product associations by listing while preserving association order", () => {
  const associations = [
    { listing_id: 8, aspect_id: "a" },
    { listing_id: 4, aspect_id: "b" },
    { listing_id: 8, aspect_id: "c" },
    { listing_id: 0, aspect_id: "ignored" },
  ];
  assert.deepEqual(groupProductAssociationsByListing(associations), [
    { listingId: 8, items: [associations[0], associations[2]] },
    { listingId: 4, items: [associations[1]] },
  ]);
});

test("rapid product switches cannot commit over an active product action", () => {
  const action = {
    productId: 11,
    detailSequence: 4,
    actionSequence: 2,
  };
  const active = {
    productBusy: true,
    productBusyProductId: 11,
    productActionSequence: 2,
    productDetailRequestSequence: 4,
    selectedProduct: { id: 11 },
  };
  assert.equal(productActionContextIsCurrent(action, active), true);
  assert.equal(productDetailRequestMayCommit(11, 4, active), true);

  const rapidSwitch = {
    ...active,
    productDetailRequestSequence: 5,
  };
  assert.equal(productActionContextIsCurrent(action, rapidSwitch), false);
  assert.equal(
    productDetailRequestMayCommit(22, 5, rapidSwitch),
    false,
    "a product B request that started before A became busy cannot replace A",
  );
  assert.equal(
    productDetailRequestMayCommit(22, 5, {
      ...rapidSwitch,
      productBusy: false,
      productBusyProductId: null,
    }),
    true,
    "product B can commit after the active action is finished",
  );
});

test("runs listing-owned association loops with bounded cross-listing concurrency", async () => {
  const associations = Array.from({ length: 6 }, (_, index) => ({
    listing_id: index + 1,
    aspect_id: `aspect-${index}`,
  }));
  let active = 0;
  let maximumActive = 0;
  const results = await runProductAssociationWorkers(
    associations,
    async (listingId, items) => {
      active += 1;
      maximumActive = Math.max(maximumActive, active);
      await new Promise((resolve) => setTimeout(resolve, 2));
      active -= 1;
      return { listingId, count: items.length };
    },
    3,
  );
  assert.equal(maximumActive, 3);
  assert.equal(results.length, 6);
  assert.ok(results.every((result) => result.status === "fulfilled"));
});

test("gives precise product-association failure outcomes", () => {
  assert.equal(describeProductAssociationOutcome({
    status: 409,
    payload: { error: { code: "review_stale" } },
  }).kind, "stale");
  assert.equal(describeProductAssociationOutcome({
    payload: { error: { code: "avionics_association_unresolved" } },
  }).kind, "manual");
  assert.equal(describeProductAssociationOutcome({
    payload: { error: { code: "avionics_identity_mismatch" } },
  }).kind, "mismatch");
  assert.equal(describeProductAssociationOutcome(new Error("network unavailable")).detail,
    "network unavailable");
});

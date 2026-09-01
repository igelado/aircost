import assert from "node:assert/strict";
import test from "node:test";

import {
  REVIEW_AREAS,
  REVIEW_PRODUCT_IDENTITY_LIMITS,
  aircraftIdentityIsVerified,
  avionicsRebuildBlockMessage,
  avionicsObservationCorrectionDraft,
  avionicsObservationRevisionRequest,
  associationsNeedingSourceRecovery,
  authoritativeIdentityUrl,
  autoVerifiableProductAssociations,
  canonicalProductSelectionConflicts,
  characterLimitState,
  describeAircraftIdentity,
  describeProductAssociationOutcome,
  describeResolvedListingOutcome,
  describeReviewReasons,
  existingProductVerificationRequest,
  groupProductAssociationsByListing,
  isAircraftIdentityStatus,
  isAircraftRepairPreflight,
  isCompletedReviewMaintenanceResponse,
  listingAssociationCanValidateLocally,
  preselectedReviewAction,
  productAssociationEvidenceDisplay,
  productAssociationEligibilityOutcome,
  productAssociationEligibilityOutcomeForAttestation,
  productAssociationReviewBucket,
  productActionContextIsCurrent,
  productAttestationDraft,
  productDetailRequestMayCommit,
  reviewAreaForAspect,
  reviewPresentationSummary,
  reviewProductIdentitySourceValidation,
  runProductAssociationWorkers,
  summarizeProductAssociations,
  summarizeProductReviewGroups,
  useExistingProductRequest,
  validateAvionicsObservationCorrection,
} from "./domain.mjs";

const PRODUCTION_REASON_CODES = [
  "listing_action_graph_invalid",
  "raw_observation_unlinked",
  "catalog_product_unresolved",
  "catalog_product_unverified",
  "catalog_product_reuse_attestation_missing",
  "catalog_product_or_listing_corroboration_missing",
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
  "catalog_product_unresolved",
  "catalog_product_unverified",
  "listing_link_confidence_not_high",
  "raw_observation_unlinked",
  "catalog_product_unverified_without_raw_observation",
  "capability_mismatch_or_unknown",
  "raw_observation_capability_unrecognized",
  "raw_observation_ambiguous",
  "raw_observation_identity_unusable",
];

test("avionics rebuild block reasons use fixed non-reflective copy", () => {
  const expected = new Map([
    [
      "retained_source_missing",
      "The retained listing source is unavailable. Capture the listing again before rebuilding its avionics review.",
    ],
    [
      "extraction_not_current",
      "The retained extraction does not satisfy the current avionics schema. Run a validated re-extraction before rebuilding its review.",
    ],
    [
      "occurrence_disposition_unknown",
      "At least one retained avionics occurrence has no current review or listing-link disposition. Run a validated re-extraction before rebuilding its review.",
    ],
    [
      "unsupported_review_state",
      "This review includes state outside the avionics workflow. No review state was changed.",
    ],
  ]);

  for (const [reasonCode, message] of expected) {
    assert.equal(avionicsRebuildBlockMessage(reasonCode), message);
  }

  const untrustedDetail = "secret parser detail: quantity failed at avionics[7]";
  const fallback = avionicsRebuildBlockMessage(untrustedDetail);
  assert.equal(
    fallback,
    "The avionics cards could not be rebuilt safely. No review state was changed.",
  );
  assert.equal(fallback.includes(untrustedDetail), false);
  assert.equal(avionicsRebuildBlockMessage("toString"), fallback);
});

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

test("builds one hash-bound request for an independently saved product decision", () => {
  assert.deepEqual(
    useExistingProductRequest(
      "a".repeat(64),
      "b".repeat(64),
      "observation-17",
      28,
    ),
    {
      review_payload_sha256: "a".repeat(64),
      catalog_revision_sha256: "b".repeat(64),
      aspect_id: "observation-17",
      avionics_model_id: 28,
    },
  );
});

test("builds a hash-bound avionics correction without replacing source evidence", () => {
  const aspect = {
    id: "avionics:2:primary",
    quantity: 1,
    configuration_action: "installed",
    configuration_action_editable: true,
    observed_text: "Garmin standby display",
    source_evidence_text: "Garmin standby display",
    proposed_product: {
      manufacturer: "Garmin",
      model: "GI 275",
      capabilities: ["Flight Display"],
    },
  };
  const correction = avionicsObservationCorrectionDraft(aspect);
  correction.quantity = 2;
  correction.configurationAction = "replaces";
  correction.replacementTargetKind = "catalog_product";
  correction.replacementProduct = { id: 17, manufacturer: "King", model: "KI 208" };
  const review = {
    review_payload_sha256: "a".repeat(64),
    catalog_revision_sha256: "b".repeat(64),
    allowed_capabilities: ["Flight Display", "NAV"],
  };
  assert.deepEqual(
    avionicsObservationRevisionRequest(review, aspect, correction),
    {
      review_payload_sha256: "a".repeat(64),
      catalog_revision_sha256: "b".repeat(64),
      aspect_id: "avionics:2:primary",
      manufacturer: "Garmin",
      model: "GI 275",
      capabilities: ["Flight Display"],
      quantity: 2,
      configuration_action: "replaces",
      replacement_target: {
        kind: "catalog_product",
        avionics_model_id: 17,
      },
    },
  );
  assert.equal("observed_text" in correction, false);
  assert.equal("source_evidence_text" in correction, false);
});

test("validates canonical correction values and requires complete replacement semantics", () => {
  const allowed = ["GPS", "NAV"];
  const valid = {
    manufacturer: "Garmin",
    model: "GNS 430W",
    capabilities: ["GPS", "NAV"],
    quantity: 1,
    configurationAction: "installed",
    replacementTargetKind: "none",
    replacementProduct: null,
    replacementAspectId: null,
  };
  assert.equal(validateAvionicsObservationCorrection(valid, allowed).valid, true);
  assert.match(
    validateAvionicsObservationCorrection(
      { ...valid, capabilities: ["Mystery"] },
      allowed,
    ).message,
    /unsupported: Mystery/,
  );
  assert.match(
    validateAvionicsObservationCorrection(
      {
        ...valid,
        configurationAction: "replaces",
        replacementTargetKind: "catalog_product",
      },
      allowed,
    ).message,
    /Select the approved product/,
  );
  assert.equal(
    validateAvionicsObservationCorrection(
      {
        ...valid,
        configurationAction: "removes",
        replacementTargetKind: "review_aspect",
        replacementAspectId: "avionics:2:replacement",
      },
      allowed,
    ).valid,
    true,
  );
});

test("describes manual resolution from the returned listing state", () => {
  assert.deepEqual(
    describeResolvedListingOutcome({
      listing: { id: 23, ingestion_state: "ready", is_verified: true },
    }, 23),
    {
      kind: "verified",
      label: "Listing 23 verified",
      detail: "The aircraft identity and listing review checks are complete.",
      listingId: 23,
      terminal: true,
    },
  );
  for (const payload of [
    {},
    { listing: { id: 24, ingestion_state: "incomplete", is_verified: false } },
    { listing: { id: 24, ingestion_state: "quarantined", is_verified: false } },
    { listing: { id: 25, ingestion_state: "ready", is_verified: true } },
  ]) {
    const outcome = describeResolvedListingOutcome(payload, 24);
    assert.equal(outcome.kind, "invalid");
    assert.equal(outcome.terminal, false);
  }
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
    label: "Ready after product source check",
    detail: "The listing match passed local checks, but the verified catalog product still needs one reusable manufacturer-source check.",
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

test("keeps listing eligibility visible while the product source check is pending", () => {
  const association = {
    source_evidence_text: null,
    verification_eligibility: {
      status: "manual_review_required",
      reason_code: "source_evidence_missing",
    },
  };
  assert.equal(
    productAssociationReviewBucket(association, "required"),
    "source_evidence_missing",
  );
  assert.deepEqual(summarizeProductAssociations([association], "required"), {
    total: 1,
    readyLocal: 0,
    needsSourceRecovery: 1,
    productAttestationRequired: 0,
    manualOrAmbiguous: 0,
  });
});

test("prevents listing-local validation until the reusable product source is current", () => {
  const aspect = {
    reuse_attestation_target: { id: 28 },
    reuse_attestation_status: "required",
  };
  assert.equal(listingAssociationCanValidateLocally(aspect), false);
  assert.equal(autoVerifiableProductAssociations([
    { verification_eligibility: { status: "auto_verifiable" } },
  ], "required").length, 0);
  aspect.reuse_attestation_status = "current";
  assert.equal(listingAssociationCanValidateLocally(aspect), true);
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
    productsNeedingSourceCheck: 1,
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

test("summarizes an aircraft-only review without inventing avionics work", () => {
  const summary = reviewPresentationSummary({
    listing_id: 23,
    aircraft_identity: {
      status: "curation_required",
      reason_code: "canonical_identity_assignment_missing",
    },
    aspects: [],
    review_payload_sha256: "a".repeat(64),
    catalog_revision_sha256: "b".repeat(64),
  });

  assert.deepEqual(summary.aircraft, { verified: false, blocking: true });
  assert.deepEqual(summary.avionics, {
    total: 0,
    decided: 0,
    remaining: 0,
    hasDirtyCorrections: false,
  });
  assert.equal(summary.defaultArea, "aircraft");
  assert.equal(
    summary.subtitle,
    "Listing 23 · No avionics decisions · Aircraft curation required",
  );
  assert.equal(
    summary.progress,
    "No avionics decisions remain · aircraft curation required",
  );
  assert.deepEqual(summary.manualReviewEligibility, {
    eligible: false,
    gates: {
      aircraftVerified: false,
      allAvionicsDecided: true,
      correctionsSaved: true,
      reviewPayloadPresent: true,
      catalogRevisionPresent: true,
    },
  });
});

test("keeps aircraft blocking separate from mixed avionics decision progress", () => {
  const summary = reviewPresentationSummary({
    listing_id: 41,
    aircraft_identity: {
      status: "curation_required",
      reason_code: "aircraft_model_mismatch",
    },
    aspects: [
      { id: "avionics:0:primary", kind: "avionics" },
      { id: "avionics:1:primary", kind: "avionics_candidate" },
      { id: "aircraft:identity", kind: "aircraft" },
    ],
    review_payload_sha256: "a".repeat(64),
    catalog_revision_sha256: "b".repeat(64),
  }, [
    { aspectId: "avionics:0:primary", valid: true, dirty: false },
    { aspectId: "avionics:1:primary", valid: false, dirty: false },
    { aspectId: "aircraft:identity", valid: true, dirty: false },
  ]);

  assert.deepEqual(summary.aircraft, { verified: false, blocking: true });
  assert.deepEqual(summary.avionics, {
    total: 2,
    decided: 1,
    remaining: 1,
    hasDirtyCorrections: false,
  });
  assert.equal(summary.defaultArea, "aircraft");
  assert.equal(
    summary.subtitle,
    "Listing 41 · 2 avionics decisions · Aircraft curation required",
  );
  assert.equal(
    summary.progress,
    "1 of 2 avionics decision remains · aircraft curation required",
  );
  assert.equal(summary.manualReviewEligibility.eligible, false);
});

test("allows final manual review only when every independent gate passes", () => {
  const review = {
    listing_id: 52,
    aircraft_identity: {
      status: "verified",
      reason_code: null,
      faa_n_number: "N123AB",
      faa_snapshot_id: 7,
    },
    aspects: [
      { id: "avionics:0:primary", kind: "avionics" },
      { id: "avionics:1:primary", kind: "avionics_identity" },
    ],
    review_payload_sha256: "a".repeat(64),
    catalog_revision_sha256: "b".repeat(64),
  };
  const decided = [
    { aspectId: "avionics:0:primary", valid: true, dirty: false },
    { aspectId: "avionics:1:primary", valid: true, dirty: false },
  ];
  const complete = reviewPresentationSummary(review, decided);

  assert.deepEqual(complete.aircraft, { verified: true, blocking: false });
  assert.deepEqual(complete.avionics, {
    total: 2,
    decided: 2,
    remaining: 0,
    hasDirtyCorrections: false,
  });
  assert.equal(complete.defaultArea, "avionics");
  assert.equal(complete.progress, "All 2 avionics decisions complete");
  assert.equal(complete.manualReviewEligibility.eligible, true);
  assert.deepEqual(complete.manualReviewEligibility.gates, {
    aircraftVerified: true,
    allAvionicsDecided: true,
    correctionsSaved: true,
    reviewPayloadPresent: true,
    catalogRevisionPresent: true,
  });

  const dirty = reviewPresentationSummary(review, [
    decided[0],
    { ...decided[1], dirty: true },
  ]);
  assert.equal(dirty.avionics.hasDirtyCorrections, true);
  assert.equal(dirty.manualReviewEligibility.eligible, false);
  assert.equal(dirty.manualReviewEligibility.gates.correctionsSaved, false);

  const missingHashes = reviewPresentationSummary({
    ...review,
    review_payload_sha256: " ",
    catalog_revision_sha256: null,
  }, decided);
  assert.equal(missingHashes.manualReviewEligibility.eligible, false);
  assert.equal(missingHashes.manualReviewEligibility.gates.reviewPayloadPresent, false);
  assert.equal(missingHashes.manualReviewEligibility.gates.catalogRevisionPresent, false);
});

test("detects one canonical product selected for distinct occurrences", () => {
  assert.deepEqual(canonicalProductSelectionConflicts([
    { aspectId: "avionics:0:primary", productId: 375, quantity: 1 },
    { aspectId: "avionics:1:primary", productId: 375, quantity: 1 },
    { aspectId: "avionics:2:primary", productId: 345, quantity: 1 },
  ]), [{
    productId: 375,
    aspectIds: ["avionics:0:primary", "avionics:1:primary"],
  }]);
});

test("does not confuse repeated input or explicit quantity with duplicate occurrences", () => {
  assert.deepEqual(canonicalProductSelectionConflicts([
    { aspectId: "avionics:0:primary", productId: 375, quantity: 2 },
    { aspectId: "avionics:0:primary", productId: 375, quantity: 2 },
    { aspectId: "avionics:1:primary", productId: 345, quantity: 1 },
  ]), []);
  assert.deepEqual(canonicalProductSelectionConflicts([
    { aspectId: "avionics:0:primary", productId: 375, quantity: 1 },
    { aspectId: "avionics:0:primary", productId: 345, quantity: 1 },
  ]), []);
  assert.deepEqual(canonicalProductSelectionConflicts([
    { aspectId: "ignored", productId: 0, quantity: 1 },
    { aspectId: "", productId: 375, quantity: 1 },
  ]), []);
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

test("accepts only hash-bound typed aircraft repair preflight", () => {
  const repair = {
    status: "available",
    listing_id: 20,
    expected_state_sha256: "a".repeat(64),
    reason_code: "missing_registration",
    actions: ["visual_identifier"],
    visual_assets: [{
      asset_id: "11002235856",
      media_url: "https://media.sandhills.com/example.jpg",
      label: "Aircraft photo",
    }],
  };
  assert.equal(isAircraftRepairPreflight(repair), true);
  assert.equal(isAircraftIdentityStatus({
    status: "curation_required",
    reason_code: "missing_registration",
    repair,
  }), true);
  assert.equal(isAircraftRepairPreflight({ ...repair, expected_state_sha256: "stale" }), false);
  assert.equal(isAircraftRepairPreflight({ ...repair, actions: ["legacy_repair"] }), false);
  assert.equal(isAircraftRepairPreflight({
    ...repair,
    visual_assets: [{ asset_id: "1", media_url: "http://unsafe.test/image.jpg" }],
  }), false);
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

test("explains a rebuilt observation with no verified product match", () => {
  assert.deepEqual(describeReviewReasons("catalog_product_unresolved"), [{
    code: "catalog_product_unresolved",
    label: "No verified product match",
    detail: "Select an existing verified product, create the confirmed product, or discard this listing item.",
    isListingLevel: false,
  }]);
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

import assert from "node:assert/strict";
import test from "node:test";

import {
  REVIEW_AREAS,
  REVIEW_PRODUCT_IDENTITY_LIMITS,
  aircraftIdentityIsVerified,
  characterLimitState,
  describeAircraftIdentity,
  describeProductAssociationOutcome,
  describeReviewReasons,
  existingProductVerificationRequest,
  groupProductAssociationsByListing,
  isAircraftIdentityStatus,
  isCompletedReviewMaintenanceResponse,
  preselectedReviewAction,
  productActionContextIsCurrent,
  productDetailRequestMayCommit,
  reviewAreaForAspect,
  reviewProductIdentitySourceValidation,
  runProductAssociationWorkers,
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

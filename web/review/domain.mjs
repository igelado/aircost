export const REVIEW_AREAS = Object.freeze(["aircraft", "avionics"]);
export const REVIEW_PRODUCT_IDENTITY_LIMITS = Object.freeze({
  sourceTitle: 200,
  evidenceText: 128,
});

// Build the single source-free request contract used for both preserved
// associations and ordinary hash-bound extraction aspects.
export function existingProductVerificationRequest(
  reviewPayloadSha256,
  catalogRevisionSha256,
  aspectId,
) {
  return {
    review_payload_sha256: reviewPayloadSha256,
    catalog_revision_sha256: catalogRevisionSha256,
    aspect_id: aspectId,
  };
}

export function automaticListingVerificationRequest(
  reviewPayloadSha256,
  catalogRevisionSha256,
) {
  return {
    review_payload_sha256: reviewPayloadSha256,
    catalog_revision_sha256: catalogRevisionSha256,
  };
}

const MACHINE_REASON_CODE = /^[a-z0-9_]+$/;
const UNCLASSIFIED_REASON_CODE = "unclassified_review_reason";
const AIRCRAFT_IDENTITY_STATES = new Set(["verified", "curation_required"]);
const AUTOMATIC_VERIFICATION_STATUSES = new Set([
  "already_verified",
  "verified",
  "pending_review",
  "blocked",
  "stale",
  "failed",
]);
const AUTOMATIC_AIRCRAFT_COMPLETE_STATES = new Set([
  "already_verified",
  "verified",
  "current",
  "reused",
  "curated",
  "assigned",
  "not_required",
]);

const AIRCRAFT_IDENTITY_REASONS = Object.freeze({
  missing_registration:
    "No U.S. N-number is recorded for this listing.",
  non_n_registration:
    "The recorded registration is not a U.S. N-number.",
  invalid_n_number:
    "The recorded N-number is not valid.",
  registry_snapshot_unavailable:
    "No current FAA registry snapshot is available for this aircraft.",
  registration_not_found:
    "The N-number was not found in the current FAA registry snapshot.",
  registration_not_covered:
    "The current FAA snapshot does not include this N-number.",
  ambiguous_registration:
    "The FAA registry data contains more than one possible record for this N-number.",
  serial_conflict:
    "The listing serial number conflicts with the current FAA record.",
  registry_aircraft_identity_unavailable:
    "The current FAA record does not include a usable aircraft identity.",
  aircraft_manufacturer_mismatch:
    "The stored manufacturer conflicts with the current FAA record.",
  aircraft_model_mismatch:
    "The stored model conflicts with the current FAA record.",
  canonical_identity_assignment_missing:
    "FAA verification succeeded, but this aircraft does not yet have a current canonical catalog assignment.",
  canonical_identity_assignment_mismatch:
    "The current canonical aircraft assignment no longer matches the FAA record or its valuation identity.",
});

const AUTOMATIC_VERIFICATION_REASONS = Object.freeze({
  ...AIRCRAFT_IDENTITY_REASONS,
  aircraft_curation_required:
    "The FAA record is valid, but the aircraft still needs a canonical catalog assignment.",
  faa_rejected:
    "The aircraft did not pass the mandatory FAA admission check.",
  source_unavailable:
    "The retained listing source needed for automatic verification is unavailable.",
  source_evidence_missing:
    "The retained listing does not contain exact source evidence for every remaining avionics item.",
  product_attestation_required:
    "One or more avionics products need a current manufacturer-source attestation.",
  catalog_identity_ambiguous:
    "The retained listing evidence matches more than one possible avionics product.",
  identity_or_capability_qualifier_unresolved:
    "A model variant or capability qualifier may identify a different avionics product.",
  manual_review_required:
    "One or more observations still require a person to confirm or discard them.",
  listing_finalization_failed:
    "The identities were checked, but required valuation enrichment did not complete.",
});

const FALLBACK_REASON = Object.freeze({
  label: "Manual review required",
  detail: "The automated review could not verify this item with enough confidence.",
  isListingLevel: false,
  group: "unclassified",
});

const REASON_DESCRIPTIONS = Object.freeze({
  listing_action_graph_invalid: reason(
    "Equipment relationships are inconsistent",
    "The stored installed, replaced, or removed equipment relationships conflict.",
    { isListingLevel: true },
  ),
  raw_observation_unlinked: reason(
    "No catalog match",
    "No catalog product is linked to this listing item.",
  ),
  catalog_product_unverified: reason(
    "Product is not verified",
    "The matched catalog product has not been verified yet.",
  ),
  catalog_product_reuse_attestation_missing: reason(
    "One-time source check required",
    "This verified product needs one current manufacturer-source check before it can be kept on the listing.",
  ),
  catalog_collision_consolidated_pending_identity_verification: reason(
    "Product identity still needs verification",
    "Duplicate catalog entries were consolidated, but the surviving product identity has not been verified yet.",
  ),
  listing_link_confidence_not_high: reason(
    "Match needs confirmation",
    "The existing product match is not supported with high confidence.",
    { group: "existing_match_confidence" },
  ),
  configuration_action_mismatch: reason(
    "Installation status conflicts",
    "The stored installed, replaced, or removed status conflicts with the listing.",
  ),
  quantity_mismatch: reason(
    "Quantity conflicts",
    "The stored quantity conflicts with the quantity observed in the listing.",
  ),
  capability_mismatch_or_unknown: reason(
    "Capabilities need confirmation",
    "The catalog product capabilities do not match the capabilities observed in the listing.",
  ),
  replacement_identity_mismatch: reason(
    "Replaced product conflicts",
    "The stored replaced product does not match the replacement observed in the listing.",
  ),
  raw_observation_ambiguous: reason(
    "Multiple possible matches",
    "This listing item matches more than one stored equipment link.",
  ),
  approved_high_confidence_link_unmatched_by_retained_observation: reason(
    "Stored product is not in the retained listing data",
    "A previously approved product could not be confirmed in the retained listing data.",
  ),
  catalog_product_unverified_without_raw_observation: reason(
    "Unverified product lacks a listing observation",
    "An unverified stored product has no matching observation in the retained listing data.",
  ),
  approved_link_confidence_not_high: reason(
    "Match needs confirmation",
    "The existing product match is not supported with high confidence.",
    { group: "existing_match_confidence" },
  ),
  replacement_association_requires_review: reason(
    "Replacement relationship needs confirmation",
    "The relationship between this product and the product it replaces needs confirmation.",
  ),
  replacement_product_unverified: reason(
    "Replaced product is not verified",
    "The product identified as being replaced has not been verified yet.",
  ),
  approved_high_confidence_replacement_unmatched_by_retained_observation: reason(
    "Replaced product is not in the retained listing data",
    "A previously approved replaced product could not be confirmed in the retained listing data.",
  ),
  replacement_association_confidence_not_high: reason(
    "Replacement match needs confirmation",
    "The replacement relationship is not supported with high confidence.",
  ),
  raw_observation_not_an_object: reason(
    "Equipment entry is malformed",
    "The retained equipment entry does not have the expected structure.",
  ),
  raw_observation_identity_unusable: reason(
    "Manufacturer or model is missing",
    "The listing item does not contain a usable manufacturer and model.",
  ),
  raw_observation_quantity_invalid: reason(
    "Quantity is invalid",
    "The quantity observed in the listing is not a positive number.",
  ),
  raw_observation_configuration_action_invalid: reason(
    "Installation status is invalid",
    "The listing item does not have a valid installed, replaced, or removed status.",
  ),
  installed_observation_has_replacement: reason(
    "Installed item has a replacement target",
    "An item marked as installed unexpectedly identifies another product as being replaced.",
  ),
  replacement_identity_missing: reason(
    "Replacement identity is incomplete",
    "The listing does not identify the product being replaced.",
    { group: "replacement_identity_incomplete" },
  ),
  raw_observation_source_confidence_invalid: reason(
    "Source confidence is invalid",
    "The retained source confidence is not a supported value.",
  ),
  replacement_identity_unusable: reason(
    "Replacement identity is incomplete",
    "The listing does not provide a usable manufacturer and model for the product being replaced.",
    { group: "replacement_identity_incomplete" },
  ),
  raw_observation_types_member_invalid: reason(
    "Capability data is malformed",
    "One or more retained capability values do not have the expected format.",
    { group: "malformed_capability_data" },
  ),
  raw_observation_types_not_array: reason(
    "Capability data is malformed",
    "One or more retained capability values do not have the expected format.",
    { group: "malformed_capability_data" },
  ),
  raw_observation_legacy_type_invalid: reason(
    "Capability data is malformed",
    "One or more retained capability values do not have the expected format.",
    { group: "malformed_capability_data" },
  ),
  raw_observation_capability_missing: reason(
    "Capabilities are missing",
    "No avionics capability was recorded for this listing item.",
  ),
  raw_observation_capability_unrecognized: reason(
    "Capability is not recognized",
    "A capability observed in the listing does not match the supported avionics categories.",
  ),
});

function reason(label, detail, { isListingLevel = false, group = null } = {}) {
  return Object.freeze({ label, detail, isListingLevel, group });
}

export function reviewAreaForAspect(aspect) {
  const kind = aspect?.kind;
  if (kind === "aircraft") {
    return "aircraft";
  }
  return typeof kind === "string" && kind.startsWith("avionics") ? "avionics" : null;
}

export function preselectedReviewAction(aspect) {
  if (
    !aspect
    || typeof aspect !== "object"
    || !Array.isArray(aspect.allowed_actions)
    || !aspect.allowed_actions.includes("use_verified_product")
  ) {
    return null;
  }
  const suggestedId = aspect.suggested_product?.id;
  return Number.isSafeInteger(suggestedId) && suggestedId > 0
    ? "use_verified_product"
    : null;
}

export function isAircraftIdentityStatus(value) {
  if (
    !value
    || typeof value !== "object"
    || !AIRCRAFT_IDENTITY_STATES.has(value.status)
  ) {
    return false;
  }
  if (value.status === "verified" && value.reason_code != null) {
    return false;
  }
  return (value.reason_code == null || typeof value.reason_code === "string")
    && (value.faa_n_number == null || typeof value.faa_n_number === "string")
    && (
      value.faa_snapshot_id == null
      || (Number.isInteger(value.faa_snapshot_id) && value.faa_snapshot_id > 0)
    );
}

export function aircraftIdentityIsVerified(value) {
  return isAircraftIdentityStatus(value) && value.status === "verified";
}

export function isCompletedReviewMaintenanceResponse(value) {
  return value !== null
    && typeof value === "object"
    && value.review === null
    && value.review_complete === true;
}

export function characterLimitState(value, limit) {
  const count = Array.from(typeof value === "string" ? value : "").length;
  return {
    count,
    limit,
    remaining: limit - count,
    overLimit: count > limit,
  };
}

export function reviewProductIdentitySourceValidation(sourceTitle, evidenceText) {
  const sourceTitleText = typeof sourceTitle === "string" ? sourceTitle.trim() : "";
  if (!sourceTitleText) {
    return {
      valid: false,
      message: "An authoritative identity source title is required.",
    };
  }
  if (
    characterLimitState(
      sourceTitleText,
      REVIEW_PRODUCT_IDENTITY_LIMITS.sourceTitle,
    ).overLimit
  ) {
    return {
      valid: false,
      message:
        `Authoritative identity source title must contain at most ${REVIEW_PRODUCT_IDENTITY_LIMITS.sourceTitle} characters.`,
    };
  }

  const evidenceTextValue = typeof evidenceText === "string" ? evidenceText.trim() : "";
  if (!evidenceTextValue) {
    return {
      valid: false,
      message: "Exact identity evidence is required.",
    };
  }
  if (
    characterLimitState(
      evidenceTextValue,
      REVIEW_PRODUCT_IDENTITY_LIMITS.evidenceText,
    ).overLimit
  ) {
    return {
      valid: false,
      message:
        `Exact identity evidence must contain at most ${REVIEW_PRODUCT_IDENTITY_LIMITS.evidenceText} characters.`,
    };
  }
  return {
    valid: true,
    message: "Identity source fields are within their character limits.",
  };
}

export function authoritativeIdentityUrl(value) {
  try {
    const parsed = new URL(String(value || "").trim());
    if (parsed.protocol !== "https:") {
      return false;
    }
    const lower = parsed.href.toLowerCase();
    return ![
      "/listing/",
      "/listings/",
      "/aircraft-for-sale/",
      "/classifieds/",
    ].some((marker) => lower.includes(marker));
  } catch {
    return false;
  }
}

/// Prepare suggestions for a fresh OEM attestation without treating historical
/// catalog evidence as a new exact publisher excerpt.
export function productAttestationDraft(product) {
  const sourceUrl = typeof product?.identity_source_url === "string"
    && authoritativeIdentityUrl(product.identity_source_url)
    ? product.identity_source_url.trim()
    : "";
  const sourceTitle = typeof product?.identity_source_title === "string"
    && product.identity_source_title.trim()
    && !characterLimitState(
      product.identity_source_title.trim(),
      REVIEW_PRODUCT_IDENTITY_LIMITS.sourceTitle,
    ).overLimit
    ? product.identity_source_title.trim()
    : "";
  return {
    sourceUrl,
    sourceTitle,
    evidenceText: "",
  };
}

export function productAssociationEligibilityOutcome(association) {
  return productAssociationEligibilityOutcomeForAttestation(association, null);
}

export function productAssociationEligibilityOutcomeForAttestation(
  association,
  attestationStatus,
) {
  const bucket = productAssociationReviewBucket(association, attestationStatus);
  const eligibility = association?.verification_eligibility;
  if (bucket === "ready_local") {
    return {
      kind: "ready",
      label: "Ready for local validation",
      detail: "The retained listing proof uniquely identifies the current approved product.",
    };
  }
  if (bucket === "source_evidence_missing") {
    return {
      kind: "recovery",
      label: "Needs source recovery",
      detail: "The retained listing does not yet contain an exact source excerpt for this product.",
    };
  }
  if (bucket === "product_attestation_required") {
    return {
      kind: "required",
      label: "OEM attestation required",
      detail: "Attest this product once from an OEM source before validating its listing associations.",
    };
  }
  if (bucket === "manual_review_required") {
    return {
      kind: "manual",
      label: "Manual or ambiguous",
      detail: manualAssociationExplanation(eligibility?.reason_code),
    };
  }
  return {
    kind: "error",
    label: "Eligibility unavailable",
    detail: "Reload the product review before attempting this association.",
  };
}

function manualAssociationExplanation(reasonCode) {
  if (reasonCode === "identity_or_capability_qualifier_unresolved") {
    return "The listing includes a model variant or capability qualifier that may identify a different product.";
  }
  if (reasonCode === "different_product_detected") {
    return "The listing evidence appears to identify a different approved product.";
  }
  if (reasonCode === "catalog_identity_ambiguous") {
    return "The retained listing evidence matches more than one possible product.";
  }
  if (reasonCode === "listing_restage_required") {
    return "The listing changed and must be reloaded before it can be checked again.";
  }
  return "This association needs a person to confirm the product identity or listing relationship.";
}

export function productAssociationReviewBucket(association, attestationStatus = null) {
  if (attestationStatus === "required") {
    return "product_attestation_required";
  }
  const eligibility = association?.verification_eligibility;
  if (eligibility?.status === "auto_verifiable") {
    return "ready_local";
  }
  if (eligibility?.status === "product_attestation_required") {
    return "product_attestation_required";
  }
  if (
    eligibility?.status === "manual_review_required"
    && (
      eligibility?.reason_code === "source_evidence_missing"
      || (
        attestationStatus === "current"
        && !(typeof association?.source_evidence_text === "string"
          && association.source_evidence_text.trim())
      )
    )
  ) {
    return "source_evidence_missing";
  }
  if (eligibility?.status === "manual_review_required") {
    return "manual_review_required";
  }
  return "eligibility_unavailable";
}

export function summarizeProductAssociations(associations, attestationStatus = null) {
  const summary = emptyProductAssociationSummary();
  for (const association of Array.isArray(associations) ? associations : []) {
    summary.total += 1;
    const bucket = productAssociationReviewBucket(association, attestationStatus);
    if (bucket === "ready_local") {
      summary.readyLocal += 1;
    } else if (bucket === "source_evidence_missing") {
      summary.needsSourceRecovery += 1;
    } else if (bucket === "product_attestation_required") {
      summary.productAttestationRequired += 1;
    } else {
      summary.manualOrAmbiguous += 1;
    }
  }
  return summary;
}

export function summarizeProductReviewGroups(groups) {
  const summary = emptyProductAssociationSummary();
  for (const group of Array.isArray(groups) ? groups : []) {
    const total = nonnegativeCount(group?.pending_association_count);
    summary.total += total;
    const counts = group?.eligibility_counts;
    if (!counts || typeof counts !== "object") {
      if (group?.attestation_status === "required") {
        summary.productAttestationRequired += total;
      } else {
        summary.manualOrAmbiguous += total;
      }
      continue;
    }
    const ready = nonnegativeCount(counts.ready_local);
    const recovery = nonnegativeCount(counts.source_evidence_missing);
    const attestation = nonnegativeCount(counts.product_attestation_required);
    const manual = nonnegativeCount(counts.manual_review_required);
    const classified = ready + recovery + attestation + manual;
    summary.readyLocal += ready;
    summary.needsSourceRecovery += recovery;
    summary.productAttestationRequired += attestation;
    summary.manualOrAmbiguous += manual + Math.max(0, total - classified);
  }
  return summary;
}

export function associationsNeedingSourceRecovery(associations, attestationStatus = null) {
  return (Array.isArray(associations) ? associations : []).filter(
    (association) => (
      productAssociationReviewBucket(association, attestationStatus)
      === "source_evidence_missing"
    ),
  );
}

function emptyProductAssociationSummary() {
  return {
    total: 0,
    readyLocal: 0,
    needsSourceRecovery: 0,
    productAttestationRequired: 0,
    manualOrAmbiguous: 0,
  };
}

function nonnegativeCount(value) {
  return Number.isSafeInteger(value) && value >= 0 ? value : 0;
}

export function productAssociationEvidenceDisplay(association) {
  const observedText = typeof association?.observed_text === "string"
    && association.observed_text.trim()
    ? association.observed_text.trim()
    : "No observed text";
  const sourceEvidenceText = typeof association?.source_evidence_text === "string"
    && association.source_evidence_text.trim()
    ? association.source_evidence_text.trim()
    : "No retained source evidence";
  return { observedText, sourceEvidenceText };
}

export function autoVerifiableProductAssociations(associations) {
  return (Array.isArray(associations) ? associations : []).filter(
    (association) => association?.verification_eligibility?.status === "auto_verifiable",
  );
}

export function groupProductAssociationsByListing(associations) {
  const groups = new Map();
  for (const association of Array.isArray(associations) ? associations : []) {
    const listingId = association?.listing_id;
    if (!Number.isSafeInteger(listingId) || listingId <= 0) {
      continue;
    }
    if (!groups.has(listingId)) {
      groups.set(listingId, []);
    }
    groups.get(listingId).push(association);
  }
  return [...groups.entries()].map(([listingId, items]) => ({ listingId, items }));
}

export function productDetailRequestMayCommit(productId, requestSequence, viewState) {
  return Number.isSafeInteger(productId)
    && productId > 0
    && Number.isSafeInteger(requestSequence)
    && requestSequence === viewState?.productDetailRequestSequence
    && (
      viewState?.productBusy !== true
      || viewState?.productBusyProductId === productId
    );
}

export function productActionContextIsCurrent(context, viewState) {
  return Number.isSafeInteger(context?.productId)
    && context.productId > 0
    && Number.isSafeInteger(context?.detailSequence)
    && Number.isSafeInteger(context?.actionSequence)
    && viewState?.productBusy === true
    && viewState?.productBusyProductId === context.productId
    && viewState?.productActionSequence === context.actionSequence
    && viewState?.productDetailRequestSequence === context.detailSequence
    && viewState?.selectedProduct?.id === context.productId;
}

/// Run one serial task per listing with bounded concurrency across listings.
///
/// The task owns the complete listing loop so it can refresh the review hash
/// after every successful mutation. This prevents two association writes for
/// one listing from racing with the same optimistic-lock token.
export async function runProductAssociationWorkers(
  associations,
  verifyListing,
  concurrency = 4,
) {
  if (typeof verifyListing !== "function") {
    throw new TypeError("verifyListing must be a function");
  }
  const groups = groupProductAssociationsByListing(associations);
  const workerCount = Math.min(
    groups.length,
    Math.max(1, Number.isSafeInteger(concurrency) ? concurrency : 4),
  );
  const results = new Array(groups.length);
  let nextIndex = 0;
  await Promise.all(Array.from({ length: workerCount }, async () => {
    while (nextIndex < groups.length) {
      const index = nextIndex;
      nextIndex += 1;
      const group = groups[index];
      try {
        results[index] = {
          listingId: group.listingId,
          status: "fulfilled",
          value: await verifyListing(group.listingId, group.items),
        };
      } catch (error) {
        results[index] = {
          listingId: group.listingId,
          status: "rejected",
          reason: error,
        };
      }
    }
  }));
  return results;
}

export function describeProductAssociationOutcome(error) {
  const code = typeof error?.payload?.error?.code === "string"
    ? error.payload.error.code
    : typeof error?.payload?.code === "string"
      ? error.payload.code
      : "";
  if (code === "review_stale" || error?.status === 409 && /stale|changed/i.test(error?.message)) {
    return {
      kind: "stale",
      label: "Stale — refresh required",
      detail: "The listing or catalog changed while it was being checked.",
    };
  }
  if (code === "avionics_association_unresolved") {
    return {
      kind: "manual",
      label: "Manual review required",
      detail: "The retained listing text does not uniquely identify this exact product.",
    };
  }
  if (code === "avionics_identity_mismatch") {
    return {
      kind: "mismatch",
      label: "Different product detected",
      detail: "Local matching selected a different approved product.",
    };
  }
  return {
    kind: "error",
    label: "Check failed",
    detail: typeof error?.message === "string" && error.message.trim()
      ? error.message
      : "The association could not be checked.",
  };
}

export function describeAutomaticListingVerificationOutcome(
  payload,
  expectedListingId = null,
) {
  const verification = payload?.verification;
  const listingId = positiveInteger(verification?.listing_id);
  const expectedId = expectedListingId == null
    ? null
    : positiveInteger(expectedListingId);
  if (
    !verification
    || typeof verification !== "object"
    || listingId === null
    || (expectedId !== null && listingId !== expectedId)
    || !AUTOMATIC_VERIFICATION_STATUSES.has(verification.status)
  ) {
    return unknownAutomaticVerificationOutcome();
  }

  if (
    (verification.status === "verified" || verification.status === "already_verified")
    && verification.final_ingestion_state === "ready"
  ) {
    return {
      kind: "verified",
      label: verification.status === "already_verified"
        ? "Listing already verified"
        : "Listing verified automatically",
      detail: verification.status === "already_verified"
        ? "The listing was already fully verified; no additional review was applied."
        : "The aircraft and avionics checks passed, and the listing is now verified.",
      listingId,
      terminal: true,
      stale: false,
      focusArea: null,
    };
  }
  if (
    verification.status === "verified"
    || verification.status === "already_verified"
  ) {
    return unknownAutomaticVerificationOutcome(
      listingId,
      automaticVerificationFocusArea(verification),
    );
  }

  if (verification.status === "stale") {
    return {
      kind: "stale",
      label: "Review changed — reload required",
      detail: "The listing, FAA data, or catalog changed while automatic verification was running.",
      listingId,
      terminal: false,
      stale: true,
      focusArea: automaticVerificationFocusArea(verification),
    };
  }

  const focusArea = automaticVerificationFocusArea(verification);
  const reasonCode = automaticVerificationReasonCode(verification, focusArea);
  const knownReason = AUTOMATIC_VERIFICATION_REASONS[reasonCode];
  const remaining = nonnegativeCount(
    verification?.avionics?.remaining_review_aspects,
  );
  const detail = knownReason || (
    focusArea === "aircraft"
      ? "The aircraft identity still needs FAA-backed catalog review."
      : remaining > 0
        ? `${remaining} ${pluralize(remaining, "avionics observation")} still need review.`
        : "The automatic checks could not safely resolve every required listing detail."
  );

  if (verification.status === "failed") {
    return {
      kind: "failed",
      label: "Automatic verification failed",
      detail,
      listingId,
      terminal: false,
      stale: false,
      focusArea,
    };
  }

  if (
    verification.status === "pending_review"
    || verification.status === "blocked"
  ) {
    return {
      kind: verification.status === "pending_review" ? "pending" : "blocked",
      label: verification.status === "pending_review"
        ? "Review still required"
        : "Automatic verification stopped",
      detail,
      listingId,
      terminal: false,
      stale: false,
      focusArea,
    };
  }

  return unknownAutomaticVerificationOutcome(listingId, focusArea);
}

export function describeAutomaticListingVerificationError(error) {
  const code = typeof error?.payload?.error?.code === "string"
    ? error.payload.error.code
    : "";
  if (
    code === "automatic_verification_stale"
    || code === "review_stale"
    || error?.status === 412
  ) {
    return {
      kind: "stale",
      label: "Review changed — reload required",
      detail: "The listing, FAA data, or catalog changed before automatic verification could finish.",
    };
  }
  if (code === "automatic_verification_in_progress") {
    return {
      kind: "in_progress",
      label: "Verification already running",
      detail: "Another automatic verification is already checking this listing.",
    };
  }
  if (code === "automatic_verification_unavailable" || error?.status === 503) {
    return {
      kind: "unavailable",
      label: "Automatic verification unavailable",
      detail: "Automatic verification is not configured on this server.",
    };
  }
  return {
    kind: "failed",
    label: "Automatic verification failed",
    detail: "The listing was not reported as verified. Reload it before trying again.",
  };
}

function automaticVerificationFocusArea(verification) {
  const aircraftStatus = typeof verification?.aircraft?.status === "string"
    ? verification.aircraft.status
    : "";
  if (!AUTOMATIC_AIRCRAFT_COMPLETE_STATES.has(aircraftStatus)) {
    return "aircraft";
  }
  return "avionics";
}

function automaticVerificationReasonCode(verification, focusArea) {
  const preferred = focusArea === "aircraft"
    ? verification?.aircraft?.reason_code
    : verification?.avionics?.reason_code;
  if (typeof preferred === "string" && MACHINE_REASON_CODE.test(preferred)) {
    return preferred;
  }
  const fallback = verification?.finalization?.reason_code;
  return typeof fallback === "string" && MACHINE_REASON_CODE.test(fallback)
    ? fallback
    : "";
}

function unknownAutomaticVerificationOutcome(listingId = null, focusArea = "avionics") {
  return {
    kind: "blocked",
    label: "Verification result unavailable",
    detail: "The server did not return a complete automatic verification result. The listing remains unverified.",
    listingId,
    terminal: false,
    stale: false,
    focusArea,
  };
}

function positiveInteger(value) {
  const parsed = typeof value === "string" && value.trim()
    ? Number(value)
    : value;
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : null;
}

function pluralize(count, singular, plural = `${singular}s`) {
  return count === 1 ? singular : plural;
}

export function describeAircraftIdentity(value) {
  if (aircraftIdentityIsVerified(value)) {
    return {
      label: "FAA identity verified",
      detail: "The listing has a current FAA-backed canonical aircraft assignment.",
      isBlocking: false,
    };
  }
  const reason = typeof value?.reason_code === "string"
    ? AIRCRAFT_IDENTITY_REASONS[value.reason_code]
    : null;
  return {
    label: "Curation required",
    detail: [
      reason || "The aircraft identity could not be verified automatically.",
      "Aircraft catalog curation and FAA verification must be completed before this listing can be verified.",
    ].join(" "),
    isBlocking: true,
  };
}

export function describeReviewReasons(value) {
  const descriptions = [];
  const seenGroups = new Set();

  for (const rawCode of reasonCodes(value)) {
    const known = REASON_DESCRIPTIONS[rawCode];
    const description = known || FALLBACK_REASON;
    const code = known
      ? rawCode
      : MACHINE_REASON_CODE.test(rawCode)
        ? rawCode
        : UNCLASSIFIED_REASON_CODE;
    const group = description.group || `code:${code}`;
    if (seenGroups.has(group)) {
      continue;
    }
    seenGroups.add(group);
    descriptions.push({
      code,
      label: description.label,
      detail: description.detail,
      isListingLevel: description.isListingLevel,
    });
  }

  return descriptions;
}

function reasonCodes(value) {
  if (Array.isArray(value)) {
    return value.flatMap(reasonCodes);
  }
  if (typeof value !== "string") {
    return [];
  }
  const text = value.trim();
  if (!text) {
    return [];
  }
  const parts = text.split(",").map((part) => part.trim());
  if (parts.length > 1 && parts.every(isMachineReasonCode)) {
    return parts;
  }
  return [text];
}

function isMachineReasonCode(value) {
  return value.length > 0 && MACHINE_REASON_CODE.test(value);
}

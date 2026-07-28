export const REVIEW_AREAS = Object.freeze(["aircraft", "avionics"]);

const MACHINE_REASON_CODE = /^[a-z0-9_]+$/;
const UNCLASSIFIED_REASON_CODE = "unclassified_review_reason";
const AIRCRAFT_IDENTITY_STATES = new Set(["verified", "curation_required"]);

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

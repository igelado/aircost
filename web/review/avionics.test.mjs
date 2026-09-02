import assert from "node:assert/strict";
import test from "node:test";

import { avionicsDeletionOutcome } from "../avionics.js";

test("accepts a canonical avionics deletion result with every affected listing", () => {
  assert.deepEqual(
    avionicsDeletionOutcome({
      deleted_product_id: 28,
      deleted_product_name: "Garmin GNS 430W",
      affected_listing_count: 2,
      affected_listing_ids: [7, 19],
    }, 28),
    {
      productId: 28,
      productName: "Garmin GNS 430W",
      affectedListingCount: 2,
      affectedListingIds: [7, 19],
    },
  );
});

test("rejects deletion results that omit or miscount affected listings", () => {
  assert.throws(
    () => avionicsDeletionOutcome({
      deleted_product_id: 28,
      deleted_product_name: "Garmin GNS 430W",
      affected_listing_count: 2,
      affected_listing_ids: [7],
    }, 28),
    /invalid avionics deletion result/,
  );
  assert.throws(
    () => avionicsDeletionOutcome({
      deleted_product_id: 29,
      deleted_product_name: "Garmin GNS 430W",
      affected_listing_count: 0,
      affected_listing_ids: [],
    }, 28),
    /invalid avionics deletion result/,
  );
});

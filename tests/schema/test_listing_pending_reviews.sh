#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
test_database="$(mktemp /tmp/aircost-listing-review-schema.XXXXXX.sqlite3)"
trap 'rm -f "$test_database"' EXIT

sqlite3 -bail "$test_database" \
  ".read $repository_root/schema/sqlite.sql" \
  "DELETE FROM sqlite_sequence WHERE name = 'aircraft_sale_listings'" \
  "INSERT INTO sqlite_sequence (name, seq) VALUES ('aircraft_sale_listings', 999)" \
  ".read $repository_root/migrations/20260724_listing_pending_reviews.sqlite.sql"

first_sequence="$(sqlite3 "$test_database" \
  "SELECT seq FROM sqlite_sequence WHERE name = 'aircraft_sale_listings'")"
test "$first_sequence" = "999"

sqlite3 -bail "$test_database" \
  ".read $repository_root/migrations/20260724_listing_pending_reviews.sqlite.sql"

second_sequence="$(sqlite3 "$test_database" \
  "SELECT seq FROM sqlite_sequence WHERE name = 'aircraft_sale_listings'")"
test "$second_sequence" = "999"

foreign_key_issues="$(sqlite3 "$test_database" "PRAGMA foreign_key_check")"
test -z "$foreign_key_issues"

integrity_check="$(sqlite3 "$test_database" "PRAGMA integrity_check")"
test "$integrity_check" = "ok"

listing_contract="$(sqlite3 "$test_database" \
  "SELECT sql FROM sqlite_schema WHERE type='table' AND name='aircraft_sale_listings'")"
[[ "$listing_contract" == *"pending_review"* ]]

review_columns="$(sqlite3 "$test_database" \
  "SELECT group_concat(name, ',') FROM pragma_table_info('aircraft_sale_listing_pending_reviews')")"
test "$review_columns" = "id,listing_id,plugin_submission_id,extraction_sha256,catalog_revision_sha256,pending_aspect_count,review_payload_json,review_payload_sha256,created_at,updated_at"

echo "Listing pending-review SQLite schema contract passed"

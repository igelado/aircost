#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
database="$(mktemp /tmp/aircost-avionics-provenance.XXXXXX.sqlite3)"
trap 'rm -f "$database"' EXIT

expect_failure() {
  local statement="$1"
  local description="$2"
  if sqlite3 -bail "$database" "PRAGMA foreign_keys=ON" "$statement" \
      >/dev/null 2>&1; then
    echo "Expected failure: $description" >&2
    exit 1
  fi
}

sqlite3 -bail "$database" ".read $repository_root/schema/sqlite.sql"

sqlite3 -bail "$database" <<'SQL'
PRAGMA foreign_keys = ON;
INSERT INTO users (email, display_name, auth_subject)
VALUES ('catalog-reviewer@example.test', 'Catalog Reviewer', 'catalog-reviewer');
INSERT INTO avionics_manufacturers (name, normalized_name)
VALUES ('Human Avionics', 'human avionics');
SQL

expect_failure \
  "INSERT INTO avionics_manufacturer_identities (canonical_name,normalized_identity_key,verification_method) VALUES ('Unsupported Automation','unsupportedautomation','automated')" \
  "automated manufacturer identity without authoritative evidence"

sqlite3 -bail "$database" <<'SQL'
PRAGMA foreign_keys = ON;
INSERT INTO avionics_manufacturer_identities (
  canonical_name, normalized_identity_key,
  verification_method, verified_by_user_id
) VALUES (
  'Human Avionics', 'humanavionics', 'human',
  (SELECT id FROM users WHERE auth_subject = 'catalog-reviewer')
);
INSERT INTO avionics_manufacturer_identity_memberships (
  avionics_manufacturer_id, avionics_manufacturer_identity_id,
  membership_basis, normalized_name_key,
  verification_method, verified_by_user_id
)
SELECT manufacturer.id, identity.id,
       'deterministic_exact', 'humanavionics', 'human', reviewer.id
FROM avionics_manufacturers manufacturer
JOIN avionics_manufacturer_identities identity
  ON identity.normalized_identity_key = 'humanavionics'
JOIN users reviewer ON reviewer.auth_subject = 'catalog-reviewer'
WHERE manufacturer.normalized_name = 'human avionics';

INSERT INTO avionics_manufacturers (name, normalized_name)
VALUES ('Human Alias', 'human alias');
SQL

expect_failure \
  "INSERT INTO avionics_manufacturer_identity_memberships (avionics_manufacturer_id,avionics_manufacturer_identity_id,membership_basis,normalized_name_key,verification_method,verified_by_user_id) SELECT manufacturer.id,identity.id,'deterministic_exact','humanalias','human',reviewer.id FROM avionics_manufacturers manufacturer JOIN avionics_manufacturer_identities identity ON identity.normalized_identity_key='humanavionics' JOIN users reviewer ON reviewer.auth_subject='catalog-reviewer' WHERE manufacturer.normalized_name='human alias'" \
  "source-free human semantic manufacturer alias"

sqlite3 -bail "$database" <<'SQL'
PRAGMA foreign_keys = ON;
INSERT INTO avionics_types (name, normalized_name)
VALUES ('Navigation', 'navigation');

INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
)
SELECT id, 'Missing Provenance', 'missing provenance',
       'manufacturer_model_number', 'Missing Provenance', 'missing provenance'
FROM avionics_manufacturers WHERE normalized_name = 'human avionics';
INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
SELECT model.id, type.id
FROM avionics_models model, avionics_types type
WHERE model.normalized_name = 'missing provenance'
  AND type.normalized_name = 'navigation';

INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
)
SELECT id, 'Unsupported Automation', 'unsupported automation',
       'manufacturer_model_number', 'Unsupported Automation', 'unsupported automation'
FROM avionics_manufacturers WHERE normalized_name = 'human avionics';
INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
SELECT model.id, type.id
FROM avionics_models model, avionics_types type
WHERE model.normalized_name = 'unsupported automation'
  AND type.normalized_name = 'navigation';
SQL

expect_failure \
  "UPDATE avionics_models SET catalog_status='approved',catalog_reviewed_at=CURRENT_TIMESTAMP WHERE normalized_name='missing provenance'" \
  "approved product without a verification method"
expect_failure \
  "UPDATE avionics_models SET catalog_status='approved',verification_method='automated',catalog_reviewed_at=CURRENT_TIMESTAMP WHERE normalized_name='unsupported automation'" \
  "automated product approval without authoritative evidence"

sqlite3 -bail "$database" <<'SQL'
PRAGMA foreign_keys = ON;
UPDATE avionics_models
SET catalog_status = 'approved',
    verification_method = 'automated',
    identity_source_url = 'https://manufacturer.example/products/supported-automation',
    identity_source_title = 'Supported Automation Product',
    identity_evidence_text = 'Human Avionics Supported Automation manufacturer model number Supported Automation',
    identity_evidence_kind = 'authoritative_reference',
    identity_confidence = 'very_high',
    catalog_reviewed_at = CURRENT_TIMESTAMP
WHERE normalized_name = 'unsupported automation';

INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, valuation_scope
)
SELECT id, 'Reviewed Unit', 'reviewed unit',
       'manufacturer_model_number', 'Reviewed Unit', 'reviewed unit', 'unit'
FROM avionics_manufacturers WHERE normalized_name = 'human avionics';
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, valuation_scope
)
SELECT id, 'Reviewed Suite', 'reviewed suite',
       'manufacturer_model_number', 'Reviewed Suite', 'reviewed suite',
       'integrated_suite'
FROM avionics_manufacturers WHERE normalized_name = 'human avionics';
INSERT INTO avionics_models (
  avionics_manufacturer_id, name, normalized_name,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, valuation_scope
)
SELECT id, 'Nested Suite', 'nested suite',
       'manufacturer_model_number', 'Nested Suite', 'nested suite',
       'integrated_suite'
FROM avionics_manufacturers WHERE normalized_name = 'human avionics';

INSERT INTO avionics_model_types (avionics_model_id, avionics_type_id)
SELECT model.id, type.id
FROM avionics_models model, avionics_types type
WHERE model.normalized_name IN ('reviewed unit', 'reviewed suite', 'nested suite')
  AND type.normalized_name = 'navigation';

UPDATE avionics_models
SET catalog_status = 'approved',
    verification_method = 'human',
    verified_by_user_id = (
      SELECT id FROM users WHERE auth_subject = 'catalog-reviewer'
    ),
    catalog_reviewed_at = CURRENT_TIMESTAMP
WHERE normalized_name IN ('reviewed unit', 'reviewed suite', 'nested suite');
SQL

expect_failure \
  "UPDATE avionics_models SET structure_verified_by_user_id=(SELECT id FROM users WHERE auth_subject='catalog-reviewer'),structure_reviewed_at=CURRENT_TIMESTAMP WHERE normalized_name='reviewed suite'" \
  "final suite structure provenance without any component"
expect_failure \
  "INSERT INTO avionics_suite_components (suite_model_id,component_model_id,quantity) SELECT suite.id,component.id,1 FROM avionics_models suite,avionics_models component WHERE suite.normalized_name='reviewed suite' AND component.normalized_name='nested suite'" \
  "integrated suite nested as a component"

sqlite3 -bail "$database" <<'SQL'
PRAGMA foreign_keys = ON;
INSERT INTO avionics_suite_components (
  suite_model_id, component_model_id, quantity
)
SELECT suite.id, component.id, 2
FROM avionics_models suite, avionics_models component
WHERE suite.normalized_name = 'reviewed suite'
  AND component.normalized_name = 'reviewed unit';
UPDATE avionics_models
SET structure_verified_by_user_id = (
      SELECT id FROM users WHERE auth_subject = 'catalog-reviewer'
    ),
    structure_reviewed_at = CURRENT_TIMESTAMP
WHERE normalized_name = 'reviewed suite';
SQL

test "$(sqlite3 "$database" \
  "SELECT verification_method || ':' || verified_by_user_id FROM avionics_models WHERE normalized_name='reviewed unit'")" = "human:1"
test "$(sqlite3 "$database" \
  "SELECT verification_method || ':' || coalesce(verified_by_user_id,'none') FROM avionics_models WHERE normalized_name='unsupported automation'")" = "automated:none"
test "$(sqlite3 "$database" \
  "SELECT count(*) FROM avionics_suite_components WHERE quantity=2")" = "1"
test "$(sqlite3 "$database" \
  "SELECT count(*) FROM avionics_models WHERE normalized_name='reviewed suite' AND structure_verified_by_user_id=1 AND structure_reviewed_at IS NOT NULL")" = "1"
test -z "$(sqlite3 "$database" "PRAGMA foreign_key_check")"

echo "Avionics automated/human verification provenance checks passed."

-- Bind each exact listing-association corroboration to the target-scoped
-- active catalog collision closure used by the local resolver. Older,
-- unbound corroborations remain safely stale.

PRAGMA foreign_keys = ON;
BEGIN IMMEDIATE;

CREATE TABLE IF NOT EXISTS schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL,
  contract_fingerprint TEXT NOT NULL,
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(migration_name)) > 0),
  CHECK (typeof(contract_version) = 'integer' AND contract_version > 0),
  CHECK (length(contract_fingerprint) = 64),
  CHECK (contract_fingerprint = lower(contract_fingerprint)),
  CHECK (contract_fingerprint NOT GLOB '*[^0-9a-f]*')
);

DROP TABLE IF EXISTS temp.listing_avionics_collision_closure_migration_guard;
CREATE TEMP TABLE listing_avionics_collision_closure_migration_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO listing_avionics_collision_closure_migration_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260806_listing_avionics_collision_closure'
  ) THEN 1
  WHEN EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260806_listing_avionics_collision_closure'
      AND contract_version = 1
      AND contract_fingerprint =
        '363fd039068667cca351c0009c0621e55942186a5d63804cf0e7da8212fa26b3'
  ) THEN 1
  ELSE 0
END;
DROP TABLE listing_avionics_collision_closure_migration_guard;

CREATE TABLE IF NOT EXISTS
  aircraft_sale_listing_avionics_corroboration_scopes (
    listing_link_id INTEGER NOT NULL,
    association_role TEXT NOT NULL
      CHECK (association_role IN ('installed', 'replacement')),
    collision_closure_sha256 TEXT NOT NULL,
    policy_version TEXT NOT NULL
      CHECK (policy_version = 'listing_avionics_collision_closure_v1'),
    bound_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (listing_link_id, association_role),
    FOREIGN KEY (listing_link_id, association_role)
      REFERENCES aircraft_sale_listing_avionics_corroborations (
        listing_link_id, association_role
      )
      ON DELETE CASCADE,
    CHECK (length(collision_closure_sha256) = 64),
    CHECK (collision_closure_sha256 = lower(collision_closure_sha256)),
    CHECK (collision_closure_sha256 NOT GLOB '*[^0-9a-f]*')
  );

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_corroboration_scopes_immutable_update
BEFORE UPDATE ON aircraft_sale_listing_avionics_corroboration_scopes
BEGIN
  SELECT RAISE(
    ABORT,
    'listing avionics corroboration collision scopes are replaced, never updated'
  );
END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260806_listing_avionics_collision_closure',
  1,
  '363fd039068667cca351c0009c0621e55942186a5d63804cf0e7da8212fa26b3',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;

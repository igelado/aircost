-- Invalidate derived manufacturer-reuse receipts that may have crossed from
-- the predecessor corroboration hash domain. Listing links and catalog rows
-- are retained; the normal local review workflow can issue current receipts.

PRAGMA foreign_keys = ON;
BEGIN IMMEDIATE;

DROP TABLE IF EXISTS temp.listing_avionics_authorization_hash_reset_guard;
CREATE TEMP TABLE listing_avionics_authorization_hash_reset_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO listing_avionics_authorization_hash_reset_guard (accepted)
SELECT CASE
  WHEN EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name =
            '20260818_listing_avionics_association_authorizations'
      AND contract_version = 1
      AND contract_fingerprint =
        'bbb76c8535647f2ecaab3179d5ef483bdef9ca23a0e14e3fd0888912fc3d90f9'
  ) AND (
    NOT EXISTS (
      SELECT 1 FROM schema_migration_contracts
      WHERE migration_name =
              '20260818_listing_avionics_authorization_hash_domain_reset'
    )
    OR EXISTS (
      SELECT 1 FROM schema_migration_contracts
      WHERE migration_name =
              '20260818_listing_avionics_authorization_hash_domain_reset'
        AND contract_version = 1
        AND contract_fingerprint =
          'cd0c1e10c508017f7053d0ab418e627ef993029ab7523a045eb7b66b802d5033'
    )
  ) THEN 1
  ELSE 0
END;
DROP TABLE listing_avionics_authorization_hash_reset_guard;

-- The original transition copied the predecessor digest while changing its
-- domain and policy label. SQL cannot prove which manufacturer-reuse rows were
-- subsequently reissued, so fail closed for this derived receipt class. The
-- same-case grounded class was never copied and remains valid.
DELETE FROM aircraft_sale_listing_avionics_authorizations
WHERE authorization_kind = 'manufacturer_reuse'
  AND NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name =
            '20260818_listing_avionics_authorization_hash_domain_reset'
      AND contract_version = 1
      AND contract_fingerprint =
        'cd0c1e10c508017f7053d0ab418e627ef993029ab7523a045eb7b66b802d5033'
  );

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260818_listing_avionics_authorization_hash_domain_reset',
  1,
  'cd0c1e10c508017f7053d0ab418e627ef993029ab7523a045eb7b66b802d5033',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO UPDATE SET
  contract_version = excluded.contract_version,
  contract_fingerprint = excluded.contract_fingerprint,
  installed_at = excluded.installed_at;

COMMIT;
PRAGMA foreign_key_check;

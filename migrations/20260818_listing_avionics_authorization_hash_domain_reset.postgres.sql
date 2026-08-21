-- Invalidate derived manufacturer-reuse receipts that may have crossed from
-- the predecessor corroboration hash domain. Listing links and catalog rows
-- are retained; the normal local review workflow can issue current receipts.

BEGIN;

SET LOCAL search_path = public, pg_catalog, pg_temp;

LOCK TABLE ONLY public.schema_migration_contracts
IN SHARE ROW EXCLUSIVE MODE;

-- Exclude old-binary authorization writers until both invalidation and the
-- reset contract commit. SHARE ROW EXCLUSIVE conflicts with INSERT's ROW
-- EXCLUSIVE lock while still allowing ordinary reads.
LOCK TABLE aircraft_sale_listing_avionics_authorizations
IN SHARE ROW EXCLUSIVE MODE;

DO $migration_guard$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM ONLY public.schema_migration_contracts
    WHERE migration_name =
            '20260818_listing_avionics_association_authorizations'
      AND contract_version = 1
      AND contract_fingerprint =
        'bbb76c8535647f2ecaab3179d5ef483bdef9ca23a0e14e3fd0888912fc3d90f9'
  ) OR EXISTS (
    SELECT 1 FROM ONLY public.schema_migration_contracts
    WHERE migration_name =
            '20260818_listing_avionics_authorization_hash_domain_reset'
      AND (
        contract_version IS DISTINCT FROM 1
        OR contract_fingerprint IS DISTINCT FROM
          'cd0c1e10c508017f7053d0ab418e627ef993029ab7523a045eb7b66b802d5033'
      )
  ) THEN
    RAISE EXCEPTION
      'listing avionics authorization hash reset is already installed with a different contract or is missing its predecessor';
  END IF;
END
$migration_guard$;

-- The original transition copied the predecessor digest while changing its
-- domain and policy label. SQL cannot prove which manufacturer-reuse rows were
-- subsequently reissued, so fail closed for this derived receipt class. The
-- same-case grounded class was never copied and remains valid.
DELETE FROM aircraft_sale_listing_avionics_authorizations
WHERE authorization_kind = 'manufacturer_reuse'
  AND NOT EXISTS (
    SELECT 1 FROM ONLY public.schema_migration_contracts
    WHERE migration_name =
            '20260818_listing_avionics_authorization_hash_domain_reset'
      AND contract_version = 1
      AND contract_fingerprint =
        'cd0c1e10c508017f7053d0ab418e627ef993029ab7523a045eb7b66b802d5033'
  );

INSERT INTO public.schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260818_listing_avionics_authorization_hash_domain_reset',
  1,
  'cd0c1e10c508017f7053d0ab418e627ef993029ab7523a045eb7b66b802d5033',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;

-- Bind each exact listing-association corroboration to the target-scoped
-- active catalog collision closure used by the local resolver. Older,
-- unbound corroborations remain safely stale.

BEGIN;

SET LOCAL search_path = public, pg_catalog, pg_temp;

CREATE TABLE IF NOT EXISTS public.schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version BIGINT NOT NULL CHECK (contract_version > 0),
  contract_fingerprint TEXT NOT NULL
    CHECK (contract_fingerprint ~ '^[0-9a-f]{64}$'),
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(BTRIM(migration_name)) > 0)
);

LOCK TABLE ONLY public.schema_migration_contracts
IN SHARE ROW EXCLUSIVE MODE;

DO $migration_guard$
BEGIN
  IF EXISTS (
    SELECT 1
    FROM ONLY public.schema_migration_contracts
    WHERE migration_name = '20260806_listing_avionics_collision_closure'
      AND (
        contract_version IS DISTINCT FROM 1
        OR contract_fingerprint IS DISTINCT FROM
          '363fd039068667cca351c0009c0621e55942186a5d63804cf0e7da8212fa26b3'
      )
  ) THEN
    RAISE EXCEPTION
      'installed listing-avionics collision-closure migration has a different contract';
  END IF;
END
$migration_guard$;

CREATE TABLE IF NOT EXISTS
  aircraft_sale_listing_avionics_corroboration_scopes (
    listing_link_id BIGINT NOT NULL,
    association_role TEXT NOT NULL
      CHECK (association_role IN ('installed', 'replacement')),
    collision_closure_sha256 TEXT NOT NULL
      CHECK (collision_closure_sha256 ~ '^[0-9a-f]{64}$'),
    policy_version TEXT NOT NULL
      CHECK (policy_version = 'listing_avionics_collision_closure_v1'),
    bound_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (listing_link_id, association_role),
    FOREIGN KEY (listing_link_id, association_role)
      REFERENCES aircraft_sale_listing_avionics_corroborations (
        listing_link_id, association_role
      )
      ON DELETE CASCADE
  );

CREATE OR REPLACE FUNCTION preserve_listing_avionics_corroboration_scope()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $function$
BEGIN
  RAISE EXCEPTION
    'listing avionics corroboration collision scopes are replaced, never updated';
  RETURN NEW;
END;
$function$;

DROP TRIGGER IF EXISTS
  listing_avionics_corroboration_scopes_immutable_update
ON aircraft_sale_listing_avionics_corroboration_scopes;
CREATE TRIGGER listing_avionics_corroboration_scopes_immutable_update
BEFORE UPDATE ON aircraft_sale_listing_avionics_corroboration_scopes
FOR EACH ROW EXECUTE FUNCTION preserve_listing_avionics_corroboration_scope();

INSERT INTO public.schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260806_listing_avionics_collision_closure',
  1,
  '363fd039068667cca351c0009c0621e55942186a5d63804cf0e7da8212fa26b3',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;

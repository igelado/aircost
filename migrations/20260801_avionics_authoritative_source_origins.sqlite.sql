-- Exact, evidence-backed web origins for avionics authority.
--
-- A manufacturer identity authorizes only the exact HTTPS origins recorded
-- here. Subdomains, parent domains, and sibling domains are never inferred.
-- Manufacturer aliases inherit origins only through the approved effective
-- identity graph. The regulator subject is reserved for reviewed regulator
-- origins without conflating them with a manufacturer namespace.

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

DROP TABLE IF EXISTS temp.avionics_source_origin_migration_guard;
CREATE TEMP TABLE avionics_source_origin_migration_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO avionics_source_origin_migration_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260801_avionics_authoritative_source_origins'
  ) THEN 1
  WHEN EXISTS (
    SELECT 1
    FROM schema_migration_contracts
    WHERE migration_name = '20260801_avionics_authoritative_source_origins'
      AND contract_version = 2
      AND contract_fingerprint =
        'f78087f6354d93d78dc8cebc895f285e38a91ca6f72dc2351acaaa88b49f9620'
  ) THEN 1
  ELSE 0
END;
DROP TABLE avionics_source_origin_migration_guard;

CREATE TABLE IF NOT EXISTS avionics_authoritative_source_origins (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  authority_kind TEXT NOT NULL CHECK (authority_kind IN (
    'manufacturer_primary', 'regulator_primary'
  )),
  avionics_manufacturer_identity_id INTEGER
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  regulator_key TEXT,
  https_origin TEXT NOT NULL,
  evidence_source_url TEXT NOT NULL,
  evidence_source_title TEXT NOT NULL,
  evidence_text TEXT NOT NULL,
  approval_basis TEXT NOT NULL CHECK (approval_basis IN (
    'curated_bootstrap', 'human_review'
  )),
  approved_by_user_id INTEGER REFERENCES users(id) ON DELETE RESTRICT,
  approval_reason TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (
    (
      authority_kind = 'manufacturer_primary'
      AND avionics_manufacturer_identity_id IS NOT NULL
      AND regulator_key IS NULL
    )
    OR (
      authority_kind = 'regulator_primary'
      AND avionics_manufacturer_identity_id IS NULL
      AND regulator_key IS NOT NULL
      AND length(regulator_key) > 0
      AND regulator_key = lower(regulator_key)
      AND regulator_key NOT GLOB '*[^a-z0-9_]*'
    )
  ),
  CHECK (https_origin = lower(trim(https_origin))),
  CHECK (substr(https_origin, 1, 8) = 'https://'),
  CHECK (length(substr(https_origin, 9)) >= 3),
  CHECK (instr(substr(https_origin, 9), '.') > 1),
  CHECK (substr(https_origin, 9) NOT GLOB '*[^a-z0-9.-]*'),
  CHECK (instr(substr(https_origin, 9), '..') = 0),
  CHECK (instr(substr(https_origin, 9), '.-') = 0),
  CHECK (instr(substr(https_origin, 9), '-.') = 0),
  CHECK (substr(substr(https_origin, 9), 1, 1) NOT IN ('.', '-')),
  CHECK (substr(https_origin, -1, 1) NOT IN ('.', '-')),
  CHECK (
    evidence_source_url = https_origin
    OR (
      substr(evidence_source_url, 1, length(https_origin)) = https_origin
      AND substr(evidence_source_url, length(https_origin) + 1, 1) = '/'
    )
  ),
  CHECK (length(trim(evidence_source_title)) >= 4),
  CHECK (length(trim(evidence_text)) >= 20),
  CHECK (
    (approval_basis = 'curated_bootstrap' AND approved_by_user_id IS NULL)
    OR (approval_basis = 'human_review' AND approved_by_user_id IS NOT NULL)
  ),
  CHECK (length(trim(approval_reason)) >= 10)
);

CREATE UNIQUE INDEX IF NOT EXISTS
  idx_avionics_authoritative_origin_manufacturer
ON avionics_authoritative_source_origins (
  avionics_manufacturer_identity_id, https_origin
)
WHERE authority_kind = 'manufacturer_primary';

CREATE UNIQUE INDEX IF NOT EXISTS
  idx_avionics_authoritative_origin_regulator
ON avionics_authoritative_source_origins (regulator_key, https_origin)
WHERE authority_kind = 'regulator_primary';

CREATE INDEX IF NOT EXISTS idx_avionics_authoritative_origin_lookup
  ON avionics_authoritative_source_origins (
    authority_kind, https_origin, avionics_manufacturer_identity_id
  );

CREATE TRIGGER IF NOT EXISTS
  avionics_authoritative_source_origins_immutable_update
BEFORE UPDATE ON avionics_authoritative_source_origins
BEGIN
  SELECT RAISE(ABORT, 'avionics authoritative source origins are immutable');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_authoritative_source_origins_immutable_delete
BEFORE DELETE ON avionics_authoritative_source_origins
BEGIN
  SELECT RAISE(ABORT, 'avionics authoritative source-origin approvals are permanent audit records');
END;

CREATE TABLE IF NOT EXISTS avionics_authoritative_source_origin_revocations (
  avionics_authoritative_source_origin_id INTEGER PRIMARY KEY
    REFERENCES avionics_authoritative_source_origins(id) ON DELETE RESTRICT,
  revoked_by_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
  reason TEXT NOT NULL,
  revoked_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(reason)) >= 10)
);

CREATE TRIGGER IF NOT EXISTS
  avionics_authoritative_source_origin_revocations_immutable_update
BEFORE UPDATE ON avionics_authoritative_source_origin_revocations
BEGIN
  SELECT RAISE(ABORT, 'avionics authoritative source-origin revocations are immutable');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_authoritative_source_origin_revocations_immutable_delete
BEFORE DELETE ON avionics_authoritative_source_origin_revocations
BEGIN
  SELECT RAISE(ABORT, 'avionics authoritative source-origin revocations are permanent audit records');
END;

CREATE VIEW IF NOT EXISTS avionics_active_authoritative_source_origins AS
SELECT source_origin.*
FROM avionics_authoritative_source_origins source_origin
WHERE NOT EXISTS (
  SELECT 1
  FROM avionics_authoritative_source_origin_revocations revocation
  WHERE revocation.avionics_authoritative_source_origin_id = source_origin.id
);

-- Fresh databases have no curated manufacturer identities while the schema is
-- first installed. When the one reviewed Garmin identity is later inserted,
-- provision only these two fixed origins. This is a Garmin-specific bootstrap,
-- never a rule that derives authority from arbitrary identity evidence URLs.
CREATE TRIGGER IF NOT EXISTS
  avionics_garmin_authoritative_source_origins_bootstrap
AFTER INSERT ON avionics_manufacturer_identities
WHEN NEW.normalized_identity_key = 'garmin'
  AND lower(trim(NEW.canonical_name)) = 'garmin'
  AND NEW.identity_evidence_kind = 'authoritative_reference'
  AND NEW.identity_confidence = 'very_high'
  AND substr(NEW.identity_source_url, 1, 23) =
    'https://www.garmin.com/'
BEGIN
  INSERT INTO avionics_authoritative_source_origins (
    authority_kind,
    avionics_manufacturer_identity_id,
    regulator_key,
    https_origin,
    evidence_source_url,
    evidence_source_title,
    evidence_text,
    approval_basis,
    approved_by_user_id,
    approval_reason
  ) VALUES (
    'manufacturer_primary',
    NEW.id,
    NULL,
    'https://www.garmin.com',
    'https://www.garmin.com/en-US/p/588901/',
    'Garmin G1000 NXi | Integrated Flight Deck',
    'The Garmin G1000 NXi is an advanced integrated flight deck family designed and manufactured by Garmin',
    'curated_bootstrap',
    NULL,
    'Reviewed first-party Garmin origin bootstrap installed by the 20260801 migration'
  )
  ON CONFLICT DO NOTHING;

  INSERT INTO avionics_authoritative_source_origins (
    authority_kind,
    avionics_manufacturer_identity_id,
    regulator_key,
    https_origin,
    evidence_source_url,
    evidence_source_title,
    evidence_text,
    approval_basis,
    approved_by_user_id,
    approval_reason
  ) VALUES (
    'manufacturer_primary',
    NEW.id,
    NULL,
    'https://static.garmin.com',
    'https://static.garmin.com/pumac/GIA63_GIA63W_InstallationManual.pdf',
    'Garmin GIA 63/GIA 63W Installation Manual',
    'GIA 63W Unit Only, (011-01105-00) 010-00386-00',
    'curated_bootstrap',
    NULL,
    'Reviewed first-party Garmin origin bootstrap installed by the 20260801 migration'
  )
  ON CONFLICT DO NOTHING;
END;

-- Curated bootstrap. The www and static hosts are independent exact origins;
-- neither row authorizes any other garmin.com subdomain.
INSERT INTO avionics_authoritative_source_origins (
  authority_kind,
  avionics_manufacturer_identity_id,
  regulator_key,
  https_origin,
  evidence_source_url,
  evidence_source_title,
  evidence_text,
  approval_basis,
  approved_by_user_id,
  approval_reason
)
SELECT
  'manufacturer_primary',
  identity.id,
  NULL,
  seed.https_origin,
  seed.evidence_source_url,
  seed.evidence_source_title,
  seed.evidence_text,
  'curated_bootstrap',
  NULL,
  'Reviewed first-party Garmin origin bootstrap installed by the 20260801 migration'
FROM avionics_manufacturer_identities identity
JOIN (
  SELECT
    'https://www.garmin.com' AS https_origin,
    'https://www.garmin.com/en-US/p/588901/' AS evidence_source_url,
    'Garmin G1000 NXi | Integrated Flight Deck' AS evidence_source_title,
    'The Garmin G1000 NXi is an advanced integrated flight deck family designed and manufactured by Garmin'
      AS evidence_text
  UNION ALL
  SELECT
    'https://static.garmin.com',
    'https://static.garmin.com/pumac/GIA63_GIA63W_InstallationManual.pdf',
    'Garmin GIA 63/GIA 63W Installation Manual',
    'GIA 63W Unit Only, (011-01105-00) 010-00386-00'
) seed
WHERE identity.normalized_identity_key = 'garmin'
  AND lower(trim(identity.canonical_name)) = 'garmin'
  AND substr(identity.identity_source_url, 1, 23) =
    'https://www.garmin.com/'
ON CONFLICT DO NOTHING;

DROP TABLE IF EXISTS temp.avionics_source_origin_seed_guard;
CREATE TEMP TABLE avionics_source_origin_seed_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO avionics_source_origin_seed_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1 FROM avionics_manufacturer_identities
    WHERE normalized_identity_key = 'garmin'
  ) THEN 1
  WHEN (
    SELECT count(*)
    FROM avionics_manufacturer_identities identity
    WHERE identity.normalized_identity_key = 'garmin'
      AND lower(trim(identity.canonical_name)) = 'garmin'
      AND substr(identity.identity_source_url, 1, 23) =
        'https://www.garmin.com/'
  ) = 1
  AND (
    SELECT count(*)
    FROM avionics_authoritative_source_origins source_origin
    JOIN avionics_manufacturer_identities identity
      ON identity.id = source_origin.avionics_manufacturer_identity_id
    WHERE identity.normalized_identity_key = 'garmin'
      AND source_origin.authority_kind = 'manufacturer_primary'
      AND source_origin.approval_basis = 'curated_bootstrap'
      AND source_origin.approved_by_user_id IS NULL
      AND source_origin.approval_reason =
        'Reviewed first-party Garmin origin bootstrap installed by the 20260801 migration'
      AND (
        (
          source_origin.https_origin = 'https://www.garmin.com'
          AND source_origin.evidence_source_url =
            'https://www.garmin.com/en-US/p/588901/'
          AND source_origin.evidence_source_title =
            'Garmin G1000 NXi | Integrated Flight Deck'
          AND source_origin.evidence_text =
            'The Garmin G1000 NXi is an advanced integrated flight deck family designed and manufactured by Garmin'
        )
        OR (
          source_origin.https_origin = 'https://static.garmin.com'
          AND source_origin.evidence_source_url =
            'https://static.garmin.com/pumac/GIA63_GIA63W_InstallationManual.pdf'
          AND source_origin.evidence_source_title =
            'Garmin GIA 63/GIA 63W Installation Manual'
          AND source_origin.evidence_text =
            'GIA 63W Unit Only, (011-01105-00) 010-00386-00'
        )
      )
  ) = 2
  THEN 1
  ELSE 0
END;
DROP TABLE avionics_source_origin_seed_guard;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260801_avionics_authoritative_source_origins',
  2,
  'f78087f6354d93d78dc8cebc895f285e38a91ca6f72dc2351acaaa88b49f9620',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
PRAGMA foreign_key_check;

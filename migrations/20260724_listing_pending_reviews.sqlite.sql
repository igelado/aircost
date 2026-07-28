-- Add a durable listing pending-review handoff after the aircraft
-- reference-catalog migrations. Back up the database first and invoke sqlite3
-- with -bail. The migration is intentionally idempotent: SQLite requires
-- rebuilding aircraft_sale_listings to extend its ingestion-state CHECK, and
-- the same rebuild is safe on a subsequent run.

PRAGMA foreign_keys = OFF;
PRAGMA legacy_alter_table = ON;
BEGIN IMMEDIATE;

DROP TABLE IF EXISTS temp.aircraft_sale_listings_pending_review_sequence;
CREATE TEMP TABLE aircraft_sale_listings_pending_review_sequence (
  seq INTEGER NOT NULL
);
INSERT INTO temp.aircraft_sale_listings_pending_review_sequence (seq)
SELECT COALESCE((
  SELECT seq
  FROM sqlite_sequence
  WHERE name = 'aircraft_sale_listings'
), 0);

DROP TABLE IF EXISTS aircraft_sale_listings_pending_review_rebuild;

CREATE TABLE aircraft_sale_listings_pending_review_rebuild (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_variant_id INTEGER NOT NULL REFERENCES aircraft_model_variants(id),
  created_by_user_id INTEGER NOT NULL REFERENCES users(id),
  is_verified INTEGER NOT NULL DEFAULT 0 CHECK (is_verified IN (0, 1)),
  source_url TEXT,
  model_year INTEGER NOT NULL,
  asking_price_usd REAL NOT NULL,
  currency TEXT NOT NULL DEFAULT 'USD',
  added_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  status TEXT NOT NULL DEFAULT 'active',
  ingestion_state TEXT NOT NULL DEFAULT 'incomplete'
    CHECK (ingestion_state IN (
      'incomplete', 'pending_review', 'ready', 'quarantined'
    )),
  ingestion_error TEXT,
  ingestion_completed_at TEXT,
  registration_number TEXT,
  serial_number TEXT,
  airframe_hours REAL NOT NULL,
  engine_hours REAL,
  engine_time_basis TEXT NOT NULL DEFAULT 'unknown'
    CHECK (engine_time_basis IN ('SNEW', 'SMOH', 'SFOH', 'SPOH', 'unknown')),
  engine_time_evidence TEXT,
  engine_time_confidence TEXT
    CHECK (engine_time_confidence IS NULL OR engine_time_confidence IN ('high', 'medium', 'low')),
  propeller_hours REAL,
  propeller_time_basis TEXT NOT NULL DEFAULT 'unknown'
    CHECK (propeller_time_basis IN ('SNEW', 'SMOH', 'SFOH', 'SPOH', 'unknown')),
  propeller_time_evidence TEXT,
  propeller_time_confidence TEXT
    CHECK (propeller_time_confidence IS NULL OR propeller_time_confidence IN ('high', 'medium', 'low')),
  installed_engine_model_id INTEGER REFERENCES engine_models(id),
  installed_engine_source_url TEXT,
  installed_engine_evidence_text TEXT,
  installed_engine_confidence TEXT
    CHECK (installed_engine_confidence IS NULL OR installed_engine_confidence IN ('high', 'medium', 'low')),
  installed_propeller_model_id INTEGER REFERENCES propeller_models(id),
  installed_propeller_source_url TEXT,
  installed_propeller_evidence_text TEXT,
  installed_propeller_confidence TEXT
    CHECK (installed_propeller_confidence IS NULL OR installed_propeller_confidence IN ('high', 'medium', 'low')),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (source_url IS NOT NULL OR is_verified = 0),
  CHECK (
    ingestion_state = 'quarantined'
    OR asking_price_usd BETWEEN 1000 AND 250000000
  ),
  CHECK (airframe_hours >= 0 AND airframe_hours <= 100000),
  CHECK (engine_hours IS NULL OR (engine_hours >= 0 AND engine_hours <= 100000)),
  CHECK (propeller_hours IS NULL OR (propeller_hours >= 0 AND propeller_hours <= 100000)),
  CHECK (engine_hours IS NOT NULL OR engine_time_basis = 'unknown'),
  CHECK (propeller_hours IS NOT NULL OR propeller_time_basis = 'unknown'),
  CHECK (
    (installed_engine_model_id IS NULL
      AND installed_engine_source_url IS NULL
      AND installed_engine_evidence_text IS NULL
      AND installed_engine_confidence IS NULL)
    OR
    (installed_engine_model_id IS NOT NULL
      AND installed_engine_source_url IS NOT NULL
      AND installed_engine_evidence_text IS NOT NULL
      AND installed_engine_confidence IS NOT NULL)
  ),
  CHECK (
    (installed_propeller_model_id IS NULL
      AND installed_propeller_source_url IS NULL
      AND installed_propeller_evidence_text IS NULL
      AND installed_propeller_confidence IS NULL)
    OR
    (installed_propeller_model_id IS NOT NULL
      AND installed_propeller_source_url IS NOT NULL
      AND installed_propeller_evidence_text IS NOT NULL
      AND installed_propeller_confidence IS NOT NULL)
  ),
  CHECK (
    ingestion_state <> 'ready'
    OR (ingestion_error IS NULL AND ingestion_completed_at IS NOT NULL)
  ),
  CHECK (ingestion_state <> 'quarantined' OR ingestion_error IS NOT NULL)
);

INSERT INTO aircraft_sale_listings_pending_review_rebuild (
  id, aircraft_model_variant_id, created_by_user_id, is_verified, source_url,
  model_year, asking_price_usd, currency, added_at, status,
  ingestion_state, ingestion_error, ingestion_completed_at,
  registration_number, serial_number, airframe_hours,
  engine_hours, engine_time_basis, engine_time_evidence, engine_time_confidence,
  propeller_hours, propeller_time_basis, propeller_time_evidence,
  propeller_time_confidence, installed_engine_model_id,
  installed_engine_source_url, installed_engine_evidence_text,
  installed_engine_confidence, installed_propeller_model_id,
  installed_propeller_source_url, installed_propeller_evidence_text,
  installed_propeller_confidence, created_at, updated_at
)
SELECT
  id, aircraft_model_variant_id, created_by_user_id, is_verified, source_url,
  model_year, asking_price_usd, currency, added_at, status,
  ingestion_state, ingestion_error, ingestion_completed_at,
  registration_number, serial_number, airframe_hours,
  engine_hours, engine_time_basis, engine_time_evidence, engine_time_confidence,
  propeller_hours, propeller_time_basis, propeller_time_evidence,
  propeller_time_confidence, installed_engine_model_id,
  installed_engine_source_url, installed_engine_evidence_text,
  installed_engine_confidence, installed_propeller_model_id,
  installed_propeller_source_url, installed_propeller_evidence_text,
  installed_propeller_confidence, created_at, updated_at
FROM aircraft_sale_listings;

DROP TABLE aircraft_sale_listings;
ALTER TABLE aircraft_sale_listings_pending_review_rebuild
  RENAME TO aircraft_sale_listings;

UPDATE sqlite_sequence
SET seq = MAX(
  seq,
  (SELECT seq FROM temp.aircraft_sale_listings_pending_review_sequence)
)
WHERE name = 'aircraft_sale_listings';

INSERT INTO sqlite_sequence (name, seq)
SELECT 'aircraft_sale_listings', preserved.seq
FROM temp.aircraft_sale_listings_pending_review_sequence preserved
WHERE preserved.seq > 0
  AND NOT EXISTS (
    SELECT 1
    FROM sqlite_sequence current_sequence
    WHERE current_sequence.name = 'aircraft_sale_listings'
  );

CREATE INDEX IF NOT EXISTS idx_aircraft_sale_listings_variant
  ON aircraft_sale_listings (aircraft_model_variant_id, is_verified, added_at);
CREATE INDEX IF NOT EXISTS idx_aircraft_sale_listings_user
  ON aircraft_sale_listings (created_by_user_id);
CREATE INDEX IF NOT EXISTS idx_aircraft_sale_listings_ingestion
  ON aircraft_sale_listings (ingestion_state, status, added_at);

CREATE TABLE IF NOT EXISTS aircraft_sale_listing_pending_reviews (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  listing_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  plugin_submission_id INTEGER
    REFERENCES plugin_submissions(id) ON DELETE SET NULL,
  extraction_sha256 TEXT NOT NULL,
  catalog_revision_sha256 TEXT NOT NULL,
  pending_aspect_count INTEGER NOT NULL CHECK (pending_aspect_count >= 1),
  review_payload_json TEXT NOT NULL,
  review_payload_sha256 TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(extraction_sha256) = 64
    AND extraction_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(catalog_revision_sha256) = 64
    AND catalog_revision_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(trim(review_payload_json)) > 0),
  CHECK (length(review_payload_sha256) = 64
    AND review_payload_sha256 NOT GLOB '*[^0-9a-f]*')
);

CREATE INDEX IF NOT EXISTS idx_aircraft_sale_listing_pending_reviews_submission
  ON aircraft_sale_listing_pending_reviews (plugin_submission_id);

DROP TABLE temp.aircraft_sale_listings_pending_review_sequence;

COMMIT;
PRAGMA legacy_alter_table = OFF;
PRAGMA foreign_keys = ON;
PRAGMA foreign_key_check;

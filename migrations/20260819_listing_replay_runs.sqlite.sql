-- Add resumable coordination for trusted-capture listing replay.

BEGIN IMMEDIATE;

-- A marker-present rerun must prove the exact canonical replay contract before
-- any replay DDL is attempted. The duplicate sentinel rows deliberately violate
-- the migration-name primary key when neither a pristine install nor an exact
-- installed contract is present; the statement and enclosing transaction then
-- leave the hostile schema untouched.
WITH replay_contract_guard(accepted) AS (
  SELECT
  (
    NOT EXISTS (
      SELECT 1 FROM schema_migration_contracts
      WHERE migration_name = '20260819_listing_replay_runs'
    )
    AND NOT EXISTS (
      SELECT 1 FROM sqlite_schema
      WHERE name IN (
        'listing_replay_runs', 'listing_replay_run_items',
        'plugin_submission_materialization_receipts',
        'idx_listing_replay_runs_one_running',
        'idx_listing_replay_run_items_phase',
        'uq_aircraft_sale_listings_owner_source',
        'listing_replay_run_items_checkpoint_exact_insert',
        'listing_replay_run_items_checkpoint_exact_update',
        'listing_replay_run_items_completed_immutable_update',
        'listing_replay_run_items_completed_immutable_delete',
        'plugin_submission_materialization_receipts_immutable_update',
        'plugin_submission_materialization_receipts_immutable_delete',
        'plugin_submissions_replay_checkpoint_immutable',
        'plugin_installs_replay_identity_immutable'
      )
    )
  )
  OR
  (
    EXISTS (
      SELECT 1 FROM schema_migration_contracts
      WHERE migration_name = '20260819_listing_replay_runs'
        AND contract_version = 1
        AND contract_fingerprint =
          'ef344cdb9cf9a7ffcd0ae66e1c9cb3979afa07c1155377cee5dc1031dd0d47c1'
    )
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'table' AND name = 'listing_replay_runs'
           AND sql = 'CREATE TABLE listing_replay_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  manifest_version INTEGER NOT NULL CHECK (manifest_version > 0),
  manifest_sha256 TEXT NOT NULL UNIQUE,
  manifest_capture_count INTEGER NOT NULL CHECK (manifest_capture_count > 0),
  status TEXT NOT NULL DEFAULT ''queued''
    CHECK (status IN (''queued'', ''running'', ''completed'')),
  active_phase TEXT CHECK (active_phase IN (''extraction'', ''materialization'')),
  owner_token TEXT,
  heartbeat_at_epoch_seconds INTEGER,
  started_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  CHECK (length(manifest_sha256) = 64),
  CHECK (manifest_sha256 = lower(manifest_sha256)),
  CHECK (manifest_sha256 NOT GLOB ''*[^0-9a-f]*''),
  CHECK (owner_token IS NULL OR length(trim(owner_token)) BETWEEN 1 AND 200),
  CHECK (
    (status = ''running'' AND active_phase IS NOT NULL AND owner_token IS NOT NULL
      AND heartbeat_at_epoch_seconds IS NOT NULL AND started_at IS NOT NULL
      AND completed_at IS NULL)
    OR
    (status = ''queued'' AND active_phase IS NULL AND owner_token IS NULL
      AND heartbeat_at_epoch_seconds IS NULL AND completed_at IS NULL)
    OR
    (status = ''completed'' AND active_phase IS NULL AND owner_token IS NULL
      AND heartbeat_at_epoch_seconds IS NULL AND completed_at IS NOT NULL)
  )
)') = 1
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'index' AND name = 'idx_listing_replay_runs_one_running'
           AND sql = 'CREATE UNIQUE INDEX idx_listing_replay_runs_one_running
  ON listing_replay_runs (status) WHERE status = ''running''') = 1
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'table' AND name = 'listing_replay_run_items'
           AND sql = 'CREATE TABLE listing_replay_run_items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL REFERENCES listing_replay_runs(id) ON DELETE CASCADE,
  plugin_submission_id INTEGER NOT NULL
    REFERENCES plugin_submissions(id) ON DELETE RESTRICT,
  position INTEGER NOT NULL CHECK (position >= 0),
  expected_rendered_html_sha256 TEXT NOT NULL,
  extracted_listing_sha256 TEXT,
  extracted_listing_json TEXT,
  extraction_state TEXT NOT NULL DEFAULT ''queued''
    CHECK (extraction_state IN (''queued'', ''running'', ''succeeded'', ''rejected'', ''failed'')),
  materialization_state TEXT NOT NULL DEFAULT ''blocked''
    CHECK (materialization_state IN (''blocked'', ''queued'', ''running'', ''succeeded'', ''rejected'', ''failed'')),
  resulting_listing_id INTEGER
    REFERENCES aircraft_sale_listings(id) ON DELETE RESTRICT,
  terminal_rejection_phase TEXT
    CHECK (terminal_rejection_phase IN (''extraction'', ''materialization'')),
  terminal_rejection_stage TEXT CHECK (terminal_rejection_stage IN (
    ''capture_admission'', ''faa_aircraft_admission''
  )),
  terminal_rejection_reason_code TEXT CHECK (terminal_rejection_reason_code IN (
    ''capture_authentication_failed'', ''capture_not_found'', ''capture_validation_failed'',
    ''missing_registration'', ''non_n_registration'',
    ''invalid_n_number'', ''serial_conflict''
  )),
  last_failure_phase TEXT CHECK (last_failure_phase IN (''extraction'', ''materialization'')),
  last_failure_reason_code TEXT
    CHECK (last_failure_reason_code IN (
      ''database_error'', ''operation_failed'', ''faa_lookup_failed'', ''faa_listing_not_found'',
      ''faa_registry_snapshot_unavailable'', ''faa_registration_not_found'',
      ''faa_registration_not_covered'', ''faa_ambiguous_registration'',
      ''faa_registry_aircraft_identity_unavailable'', ''faa_aircraft_manufacturer_mismatch'',
      ''faa_aircraft_model_mismatch'', ''faa_canonical_identity_assignment_missing'',
      ''faa_canonical_identity_assignment_mismatch''
    )),
  extraction_attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (extraction_attempt_count >= 0),
  materialization_attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (materialization_attempt_count >= 0),
  extraction_started_at TEXT,
  extraction_completed_at TEXT,
  materialization_started_at TEXT,
  materialization_completed_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (run_id, position),
  UNIQUE (run_id, plugin_submission_id),
  CHECK (length(expected_rendered_html_sha256) = 64),
  CHECK (expected_rendered_html_sha256 = lower(expected_rendered_html_sha256)),
  CHECK (expected_rendered_html_sha256 NOT GLOB ''*[^0-9a-f]*''),
  CHECK (extracted_listing_sha256 IS NULL OR (
    length(extracted_listing_sha256) = 64
    AND extracted_listing_sha256 = lower(extracted_listing_sha256)
    AND extracted_listing_sha256 NOT GLOB ''*[^0-9a-f]*''
  )),
  CHECK (
    (extraction_state = ''rejected'' AND materialization_state = ''blocked''
      AND terminal_rejection_phase = ''extraction''
      AND terminal_rejection_stage = ''capture_admission''
      AND terminal_rejection_reason_code IN (
        ''capture_authentication_failed'', ''capture_not_found'', ''capture_validation_failed''
      ))
    OR
    (extraction_state = ''succeeded'' AND materialization_state = ''rejected''
      AND terminal_rejection_phase = ''materialization''
      AND (
        (terminal_rejection_stage = ''capture_admission''
          AND terminal_rejection_reason_code IN (
            ''capture_authentication_failed'', ''capture_not_found'', ''capture_validation_failed''
          ))
        OR
        (terminal_rejection_stage = ''faa_aircraft_admission''
          AND terminal_rejection_reason_code IN (
            ''missing_registration'', ''non_n_registration'', ''invalid_n_number'', ''serial_conflict''
          ))
      ))
    OR
    (extraction_state <> ''rejected'' AND materialization_state <> ''rejected''
      AND terminal_rejection_phase IS NULL AND terminal_rejection_stage IS NULL
      AND terminal_rejection_reason_code IS NULL)
  ),
  CHECK (
    (extraction_state = ''failed'' AND materialization_state = ''blocked''
      AND last_failure_phase = ''extraction''
      AND last_failure_reason_code IN (''database_error'', ''operation_failed''))
    OR
    (extraction_state = ''succeeded'' AND materialization_state = ''failed''
      AND last_failure_phase = ''materialization'' AND last_failure_reason_code IS NOT NULL)
    OR
    (extraction_state <> ''failed'' AND materialization_state <> ''failed''
      AND last_failure_phase IS NULL AND last_failure_reason_code IS NULL)
  ),
  CHECK ((materialization_state = ''succeeded'') = (resulting_listing_id IS NOT NULL)),
  CHECK ((extraction_state = ''succeeded'') = (extracted_listing_sha256 IS NOT NULL)),
  CHECK ((extraction_state = ''succeeded'') = (extracted_listing_json IS NOT NULL)),
  CHECK (extraction_state = ''succeeded'' OR materialization_state = ''blocked''),
  CHECK (extraction_state <> ''running'' OR extraction_started_at IS NOT NULL),
  CHECK (materialization_state <> ''running'' OR materialization_started_at IS NOT NULL)
)') = 1
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'index' AND name = 'idx_listing_replay_run_items_phase'
           AND sql = 'CREATE INDEX idx_listing_replay_run_items_phase
  ON listing_replay_run_items (run_id, extraction_state, materialization_state, position)') = 1
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'table' AND name = 'plugin_submission_materialization_receipts'
           AND sql = 'CREATE TABLE plugin_submission_materialization_receipts (
  plugin_submission_id INTEGER PRIMARY KEY
    REFERENCES plugin_submissions(id) ON DELETE CASCADE,
  aircraft_sale_listing_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_sale_listings(id) ON DELETE RESTRICT,
  rendered_html_sha256 TEXT NOT NULL,
  extracted_listing_sha256 TEXT NOT NULL,
  completed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(rendered_html_sha256) = 64),
  CHECK (rendered_html_sha256 = lower(rendered_html_sha256)),
  CHECK (rendered_html_sha256 NOT GLOB ''*[^0-9a-f]*''),
  CHECK (length(extracted_listing_sha256) = 64),
  CHECK (extracted_listing_sha256 = lower(extracted_listing_sha256)),
  CHECK (extracted_listing_sha256 NOT GLOB ''*[^0-9a-f]*'')
)') = 1
    AND NOT EXISTS (
      SELECT 1 FROM sqlite_schema
      WHERE tbl_name IN (
        'listing_replay_runs', 'listing_replay_run_items',
        'plugin_submission_materialization_receipts'
      )
        AND (
          (
            type = 'trigger'
            AND name NOT IN (
              'listing_replay_run_items_checkpoint_exact_insert',
              'listing_replay_run_items_checkpoint_exact_update',
              'listing_replay_run_items_completed_immutable_update',
              'listing_replay_run_items_completed_immutable_delete',
              'plugin_submission_materialization_receipts_immutable_update',
              'plugin_submission_materialization_receipts_immutable_delete'
            )
          )
          OR (
            type = 'index'
            AND name NOT IN (
              'idx_listing_replay_runs_one_running',
              'idx_listing_replay_run_items_phase',
              'sqlite_autoindex_listing_replay_runs_1',
              'sqlite_autoindex_listing_replay_run_items_1',
              'sqlite_autoindex_listing_replay_run_items_2',
              'sqlite_autoindex_plugin_submission_materialization_receipts_1'
            )
          )
        )
    )
    AND NOT EXISTS (
      SELECT 1 FROM sqlite_schema
      WHERE type = 'trigger'
        AND tbl_name IN ('plugin_submissions', 'plugin_installs')
        AND NOT (
          (
            tbl_name = 'plugin_submissions'
            AND name IN (
              'plugin_submissions_replay_checkpoint_immutable',
              'listing_avionics_authorizations_invalidate_capture_delete',
              'listing_avionics_authorizations_invalidate_capture_update'
            )
          )
          OR (
            tbl_name = 'plugin_installs'
            AND name = 'plugin_installs_replay_identity_immutable'
          )
        )
    )
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'index' AND tbl_name IN (
           'listing_replay_runs', 'listing_replay_run_items',
           'plugin_submission_materialization_receipts'
         )) = 6
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE name IN (
           'uq_aircraft_sale_listings_owner_source',
           'listing_replay_run_items_checkpoint_exact_insert',
           'listing_replay_run_items_checkpoint_exact_update',
           'listing_replay_run_items_completed_immutable_update',
           'listing_replay_run_items_completed_immutable_delete',
           'plugin_submission_materialization_receipts_immutable_update',
           'plugin_submission_materialization_receipts_immutable_delete',
           'plugin_submissions_replay_checkpoint_immutable',
           'plugin_installs_replay_identity_immutable'
         )) = 9
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE name IN (
           'listing_replay_runs', 'listing_replay_run_items',
           'plugin_submission_materialization_receipts',
           'idx_listing_replay_runs_one_running',
           'idx_listing_replay_run_items_phase'
         )) = 5
  )
),
duplicate_guard_rows(row_number) AS (
  VALUES (1), (2)
)
INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
)
SELECT '__listing_replay_runs_contract_guard__', 1,
       '0000000000000000000000000000000000000000000000000000000000000000',
       'contract-guard'
FROM replay_contract_guard
CROSS JOIN duplicate_guard_rows
WHERE NOT accepted;

CREATE TABLE IF NOT EXISTS listing_replay_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  manifest_version INTEGER NOT NULL CHECK (manifest_version > 0),
  manifest_sha256 TEXT NOT NULL UNIQUE,
  manifest_capture_count INTEGER NOT NULL CHECK (manifest_capture_count > 0),
  status TEXT NOT NULL DEFAULT 'queued'
    CHECK (status IN ('queued', 'running', 'completed')),
  active_phase TEXT CHECK (active_phase IN ('extraction', 'materialization')),
  owner_token TEXT,
  heartbeat_at_epoch_seconds INTEGER,
  started_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  CHECK (length(manifest_sha256) = 64),
  CHECK (manifest_sha256 = lower(manifest_sha256)),
  CHECK (manifest_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (owner_token IS NULL OR length(trim(owner_token)) BETWEEN 1 AND 200),
  CHECK (
    (status = 'running' AND active_phase IS NOT NULL AND owner_token IS NOT NULL
      AND heartbeat_at_epoch_seconds IS NOT NULL AND started_at IS NOT NULL
      AND completed_at IS NULL)
    OR
    (status = 'queued' AND active_phase IS NULL AND owner_token IS NULL
      AND heartbeat_at_epoch_seconds IS NULL AND completed_at IS NULL)
    OR
    (status = 'completed' AND active_phase IS NULL AND owner_token IS NULL
      AND heartbeat_at_epoch_seconds IS NULL AND completed_at IS NOT NULL)
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_listing_replay_runs_one_running
  ON listing_replay_runs (status) WHERE status = 'running';

CREATE TABLE IF NOT EXISTS listing_replay_run_items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL REFERENCES listing_replay_runs(id) ON DELETE CASCADE,
  plugin_submission_id INTEGER NOT NULL
    REFERENCES plugin_submissions(id) ON DELETE RESTRICT,
  position INTEGER NOT NULL CHECK (position >= 0),
  expected_rendered_html_sha256 TEXT NOT NULL,
  extracted_listing_sha256 TEXT,
  extracted_listing_json TEXT,
  extraction_state TEXT NOT NULL DEFAULT 'queued'
    CHECK (extraction_state IN ('queued', 'running', 'succeeded', 'rejected', 'failed')),
  materialization_state TEXT NOT NULL DEFAULT 'blocked'
    CHECK (materialization_state IN ('blocked', 'queued', 'running', 'succeeded', 'rejected', 'failed')),
  resulting_listing_id INTEGER
    REFERENCES aircraft_sale_listings(id) ON DELETE RESTRICT,
  terminal_rejection_phase TEXT
    CHECK (terminal_rejection_phase IN ('extraction', 'materialization')),
  terminal_rejection_stage TEXT CHECK (terminal_rejection_stage IN (
    'capture_admission', 'faa_aircraft_admission'
  )),
  terminal_rejection_reason_code TEXT CHECK (terminal_rejection_reason_code IN (
    'capture_authentication_failed', 'capture_not_found', 'capture_validation_failed',
    'missing_registration', 'non_n_registration',
    'invalid_n_number', 'serial_conflict'
  )),
  last_failure_phase TEXT CHECK (last_failure_phase IN ('extraction', 'materialization')),
  last_failure_reason_code TEXT
    CHECK (last_failure_reason_code IN (
      'database_error', 'operation_failed', 'faa_lookup_failed', 'faa_listing_not_found',
      'faa_registry_snapshot_unavailable', 'faa_registration_not_found',
      'faa_registration_not_covered', 'faa_ambiguous_registration',
      'faa_registry_aircraft_identity_unavailable', 'faa_aircraft_manufacturer_mismatch',
      'faa_aircraft_model_mismatch', 'faa_canonical_identity_assignment_missing',
      'faa_canonical_identity_assignment_mismatch'
    )),
  extraction_attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (extraction_attempt_count >= 0),
  materialization_attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (materialization_attempt_count >= 0),
  extraction_started_at TEXT,
  extraction_completed_at TEXT,
  materialization_started_at TEXT,
  materialization_completed_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (run_id, position),
  UNIQUE (run_id, plugin_submission_id),
  CHECK (length(expected_rendered_html_sha256) = 64),
  CHECK (expected_rendered_html_sha256 = lower(expected_rendered_html_sha256)),
  CHECK (expected_rendered_html_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (extracted_listing_sha256 IS NULL OR (
    length(extracted_listing_sha256) = 64
    AND extracted_listing_sha256 = lower(extracted_listing_sha256)
    AND extracted_listing_sha256 NOT GLOB '*[^0-9a-f]*'
  )),
  CHECK (
    (extraction_state = 'rejected' AND materialization_state = 'blocked'
      AND terminal_rejection_phase = 'extraction'
      AND terminal_rejection_stage = 'capture_admission'
      AND terminal_rejection_reason_code IN (
        'capture_authentication_failed', 'capture_not_found', 'capture_validation_failed'
      ))
    OR
    (extraction_state = 'succeeded' AND materialization_state = 'rejected'
      AND terminal_rejection_phase = 'materialization'
      AND (
        (terminal_rejection_stage = 'capture_admission'
          AND terminal_rejection_reason_code IN (
            'capture_authentication_failed', 'capture_not_found', 'capture_validation_failed'
          ))
        OR
        (terminal_rejection_stage = 'faa_aircraft_admission'
          AND terminal_rejection_reason_code IN (
            'missing_registration', 'non_n_registration', 'invalid_n_number', 'serial_conflict'
          ))
      ))
    OR
    (extraction_state <> 'rejected' AND materialization_state <> 'rejected'
      AND terminal_rejection_phase IS NULL AND terminal_rejection_stage IS NULL
      AND terminal_rejection_reason_code IS NULL)
  ),
  CHECK (
    (extraction_state = 'failed' AND materialization_state = 'blocked'
      AND last_failure_phase = 'extraction'
      AND last_failure_reason_code IN ('database_error', 'operation_failed'))
    OR
    (extraction_state = 'succeeded' AND materialization_state = 'failed'
      AND last_failure_phase = 'materialization' AND last_failure_reason_code IS NOT NULL)
    OR
    (extraction_state <> 'failed' AND materialization_state <> 'failed'
      AND last_failure_phase IS NULL AND last_failure_reason_code IS NULL)
  ),
  CHECK ((materialization_state = 'succeeded') = (resulting_listing_id IS NOT NULL)),
  CHECK ((extraction_state = 'succeeded') = (extracted_listing_sha256 IS NOT NULL)),
  CHECK ((extraction_state = 'succeeded') = (extracted_listing_json IS NOT NULL)),
  CHECK (extraction_state = 'succeeded' OR materialization_state = 'blocked'),
  CHECK (extraction_state <> 'running' OR extraction_started_at IS NOT NULL),
  CHECK (materialization_state <> 'running' OR materialization_started_at IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_listing_replay_run_items_phase
  ON listing_replay_run_items (run_id, extraction_state, materialization_state, position);

CREATE TRIGGER IF NOT EXISTS listing_replay_run_items_checkpoint_exact_insert
BEFORE INSERT ON listing_replay_run_items
WHEN NEW.extraction_state = 'succeeded' AND NOT EXISTS (
  SELECT 1
  FROM plugin_submissions submission
  JOIN plugin_installs install ON install.id = submission.plugin_install_id
  WHERE submission.id = NEW.plugin_submission_id
    AND submission.rendered_html_sha256 = NEW.expected_rendered_html_sha256
    AND submission.extracted_listing_json IS NEW.extracted_listing_json
    AND submission.extraction_error IS NULL
    AND julianday(submission.submitted_at) IS NOT NULL
    AND (
      install.revoked_at IS NULL
      OR (
        julianday(install.revoked_at) IS NOT NULL
        AND julianday(submission.submitted_at) <= julianday(install.revoked_at)
      )
    )
)
BEGIN
  SELECT RAISE(ABORT, 'replay extraction transition does not match its exact checkpoint');
END;

CREATE TRIGGER IF NOT EXISTS listing_replay_run_items_checkpoint_exact_update
BEFORE UPDATE ON listing_replay_run_items
WHEN NEW.extraction_state = 'succeeded' AND NOT EXISTS (
  SELECT 1
  FROM plugin_submissions submission
  JOIN plugin_installs install ON install.id = submission.plugin_install_id
  WHERE submission.id = NEW.plugin_submission_id
    AND submission.rendered_html_sha256 = NEW.expected_rendered_html_sha256
    AND submission.extracted_listing_json IS NEW.extracted_listing_json
    AND submission.extraction_error IS NULL
    AND julianday(submission.submitted_at) IS NOT NULL
    AND (
      install.revoked_at IS NULL
      OR (
        julianday(install.revoked_at) IS NOT NULL
        AND julianday(submission.submitted_at) <= julianday(install.revoked_at)
      )
    )
)
BEGIN
  SELECT RAISE(ABORT, 'replay extraction transition does not match its exact checkpoint');
END;

CREATE TRIGGER IF NOT EXISTS listing_replay_run_items_completed_immutable_update
BEFORE UPDATE ON listing_replay_run_items
WHEN OLD.materialization_state = 'succeeded'
BEGIN
  SELECT RAISE(ABORT, 'completed replay item is immutable');
END;

CREATE TRIGGER IF NOT EXISTS listing_replay_run_items_completed_immutable_delete
BEFORE DELETE ON listing_replay_run_items
WHEN OLD.materialization_state = 'succeeded'
BEGIN
  SELECT RAISE(ABORT, 'completed replay item is immutable');
END;

CREATE TABLE IF NOT EXISTS plugin_submission_materialization_receipts (
  plugin_submission_id INTEGER PRIMARY KEY
    REFERENCES plugin_submissions(id) ON DELETE CASCADE,
  aircraft_sale_listing_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_sale_listings(id) ON DELETE RESTRICT,
  rendered_html_sha256 TEXT NOT NULL,
  extracted_listing_sha256 TEXT NOT NULL,
  completed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(rendered_html_sha256) = 64),
  CHECK (rendered_html_sha256 = lower(rendered_html_sha256)),
  CHECK (rendered_html_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(extracted_listing_sha256) = 64),
  CHECK (extracted_listing_sha256 = lower(extracted_listing_sha256)),
  CHECK (extracted_listing_sha256 NOT GLOB '*[^0-9a-f]*')
);

CREATE TRIGGER IF NOT EXISTS plugin_submission_materialization_receipts_immutable_update
BEFORE UPDATE ON plugin_submission_materialization_receipts
BEGIN
  SELECT RAISE(ABORT, 'replay materialization receipt is immutable');
END;

CREATE TRIGGER IF NOT EXISTS plugin_submission_materialization_receipts_immutable_delete
BEFORE DELETE ON plugin_submission_materialization_receipts
BEGIN
  SELECT RAISE(ABORT, 'replay materialization receipt is immutable');
END;

CREATE UNIQUE INDEX IF NOT EXISTS uq_aircraft_sale_listings_owner_source
  ON aircraft_sale_listings (created_by_user_id, source_url)
  WHERE source_url IS NOT NULL AND length(trim(source_url)) > 0;

CREATE TRIGGER IF NOT EXISTS plugin_submissions_replay_checkpoint_immutable
BEFORE UPDATE ON plugin_submissions
WHEN (
  EXISTS (
    SELECT 1 FROM listing_replay_run_items item
    WHERE item.plugin_submission_id = OLD.id
      AND item.extraction_state = 'succeeded'
  )
  OR EXISTS (
    SELECT 1 FROM plugin_submission_materialization_receipts receipt
    WHERE receipt.plugin_submission_id = OLD.id
  )
) AND (
  NOT (NEW.id IS OLD.id)
  OR NOT (NEW.user_id IS OLD.user_id)
  OR NOT (NEW.plugin_install_id IS OLD.plugin_install_id)
  OR NOT (NEW.source_url IS OLD.source_url)
  OR NOT (NEW.submitted_at IS OLD.submitted_at)
  OR NOT (NEW.rendered_html IS OLD.rendered_html)
  OR NOT (NEW.rendered_html_sha256 IS OLD.rendered_html_sha256)
  OR NOT (NEW.signature_base64 IS OLD.signature_base64)
  OR NOT (NEW.extracted_listing_json IS OLD.extracted_listing_json)
  OR NOT (NEW.extraction_error IS OLD.extraction_error)
  OR NOT (
    NEW.canonical_listing_id IS OLD.canonical_listing_id
    OR (
      OLD.canonical_listing_id IS NULL
      AND NEW.canonical_listing_id IS NOT NULL
      AND NOT EXISTS (
        SELECT 1 FROM plugin_submission_materialization_receipts receipt
        WHERE receipt.plugin_submission_id = OLD.id
      )
      AND EXISTS (
        SELECT 1 FROM aircraft_sale_listings listing
        WHERE listing.id = NEW.canonical_listing_id
          AND listing.created_by_user_id = OLD.user_id
          AND listing.source_url = OLD.source_url
      )
    )
  )
)
BEGIN
  SELECT RAISE(ABORT, 'replay checkpoint capture is immutable');
END;

CREATE TRIGGER IF NOT EXISTS plugin_installs_replay_identity_immutable
BEFORE UPDATE ON plugin_installs
WHEN EXISTS (
  SELECT 1
  FROM plugin_submissions submission
  WHERE submission.plugin_install_id = OLD.id
    AND (
      EXISTS (
        SELECT 1 FROM listing_replay_run_items item
        WHERE item.plugin_submission_id = submission.id
          AND item.extraction_state = 'succeeded'
      )
      OR EXISTS (
        SELECT 1 FROM plugin_submission_materialization_receipts receipt
        WHERE receipt.plugin_submission_id = submission.id
      )
    )
) AND (
  NOT (NEW.id IS OLD.id)
  OR NOT (NEW.user_id IS OLD.user_id)
  OR NOT (NEW.public_key_base64 IS OLD.public_key_base64)
  OR NOT (NEW.created_at IS OLD.created_at)
  OR NOT (
    NEW.revoked_at IS OLD.revoked_at
    OR (
      OLD.revoked_at IS NULL
      AND NEW.revoked_at IS NOT NULL
      AND julianday(NEW.revoked_at) IS NOT NULL
      AND NOT EXISTS (
        SELECT 1
        FROM plugin_submissions submission
        WHERE submission.plugin_install_id = OLD.id
          AND (
            EXISTS (
              SELECT 1 FROM listing_replay_run_items item
              WHERE item.plugin_submission_id = submission.id
                AND item.extraction_state = 'succeeded'
            )
            OR EXISTS (
              SELECT 1 FROM plugin_submission_materialization_receipts receipt
              WHERE receipt.plugin_submission_id = submission.id
            )
          )
          AND (
            julianday(submission.submitted_at) IS NULL
            OR julianday(submission.submitted_at) > julianday(NEW.revoked_at)
          )
      )
    )
  )
)
BEGIN
  SELECT RAISE(ABORT, 'replay checkpoint plugin identity is immutable');
END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260819_listing_replay_runs', 1,
  'ef344cdb9cf9a7ffcd0ae66e1c9cb3979afa07c1155377cee5dc1031dd0d47c1',
  CURRENT_TIMESTAMP
) ON CONFLICT (migration_name) DO NOTHING;

COMMIT;

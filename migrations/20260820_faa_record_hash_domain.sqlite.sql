PRAGMA foreign_keys = OFF;
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

DROP TABLE IF EXISTS temp.faa_record_hash_domain_guard;
CREATE TEMP TABLE faa_record_hash_domain_guard (
  accepted INTEGER NOT NULL CHECK (accepted = 1)
);
INSERT INTO faa_record_hash_domain_guard (accepted)
SELECT CASE
  WHEN NOT EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260820_faa_record_hash_domain'
  )
  AND NOT EXISTS (
    SELECT 1 FROM pragma_table_xinfo('faa_registry_snapshots')
    WHERE name = 'record_hash_domain'
  )
  AND NOT EXISTS (SELECT 1 FROM faa_registry_snapshots)
  THEN 1
  WHEN EXISTS (
    SELECT 1 FROM schema_migration_contracts
    WHERE migration_name = '20260820_faa_record_hash_domain'
      AND contract_version = 1
      AND contract_fingerprint =
        'f124f573bf705da6c1e4b0a5c7a8df45ea5a4a5dc009a28eee012be42c691502'
  )
  AND (
    SELECT count(*) FROM pragma_table_xinfo('faa_registry_snapshots')
    WHERE hidden = 0
  ) = 15
  AND EXISTS (
    SELECT 1 FROM pragma_table_xinfo('faa_registry_snapshots')
    WHERE cid = 14
      AND name = 'record_hash_domain'
      AND upper(type) = 'TEXT'
      AND [notnull] = 1
      AND dflt_value IS NULL
      AND pk = 0
      AND hidden = 0
  )
  AND EXISTS (
    SELECT 1 FROM sqlite_schema
    WHERE type = 'table'
      AND name = 'faa_registry_snapshots'
      AND instr(
        replace(replace(lower(sql), char(10), ''), ' ', ''),
        'record_hash_domaintextnotnullcheck(record_hash_domain=''aircost-faa-master-retained-aircraft-projection-v1'')'
      ) > 0
  )
  THEN 1
  ELSE 0
END
WHERE (
  SELECT group_concat(
    cid || ':' || name || ':' || type || ':' || [notnull] || ':' ||
    coalesce(dflt_value, '') || ':' || pk || ':' || hidden,
    '|'
  )
  FROM (
    SELECT * FROM pragma_table_xinfo('faa_registry_snapshots')
    WHERE name <> 'record_hash_domain'
    ORDER BY cid
  )
) = '0:id:INTEGER:0::1:0|1:evidence_source_id:INTEGER:1::0:0|2:snapshot_date:TEXT:1::0:0|3:source_url:TEXT:1::0:0|4:archive_sha256:TEXT:1::0:0|5:source_manifest_sha256:TEXT:1::0:0|6:target_set_sha256:TEXT:1::0:0|7:master_member_name:TEXT:1::0:0|8:master_member_sha256:TEXT:1::0:0|9:aircraft_member_name:TEXT:1::0:0|10:aircraft_member_sha256:TEXT:1::0:0|11:engine_member_name:TEXT:1::0:0|12:engine_member_sha256:TEXT:1::0:0|13:imported_at:TEXT:1:CURRENT_TIMESTAMP:0:0'
AND NOT EXISTS (
  SELECT 1 FROM pragma_integrity_check('faa_registry_snapshots')
  WHERE integrity_check <> 'ok'
)
AND (
  SELECT group_concat(
    name || ':' || [unique] || ':' || origin || ':' || partial,
    '|'
  )
  FROM (
    SELECT * FROM pragma_index_list('faa_registry_snapshots') ORDER BY name
  )
) = 'idx_faa_registry_snapshots_current:0:c:0|sqlite_autoindex_faa_registry_snapshots_1:1:u:0'
AND (
  SELECT group_concat(index_name || ':' || seqno || ':' || column_name, '|')
  FROM (
    SELECT indexes.name AS index_name, columns.seqno, columns.name AS column_name
    FROM pragma_index_list('faa_registry_snapshots') indexes
    JOIN pragma_index_info(indexes.name) columns
    ORDER BY indexes.name, columns.seqno
  )
) = 'idx_faa_registry_snapshots_current:0:snapshot_date|idx_faa_registry_snapshots_current:1:id|sqlite_autoindex_faa_registry_snapshots_1:0:archive_sha256|sqlite_autoindex_faa_registry_snapshots_1:1:target_set_sha256'
AND (
  SELECT group_concat(
    id || ':' || seq || ':' || [table] || ':' || [from] || ':' || [to] || ':' ||
    on_update || ':' || on_delete || ':' || match,
    '|'
  )
  FROM pragma_foreign_key_list('faa_registry_snapshots')
) = '0:0:curation_evidence_sources:evidence_source_id:id:NO ACTION:RESTRICT:NONE'
AND (
  SELECT count(*) FROM sqlite_schema
  WHERE tbl_name = 'faa_registry_snapshots'
) = 6
AND (
  SELECT replace(replace(replace(lower(sql), char(10), ''), char(9), ''), ' ', '')
  FROM sqlite_schema
  WHERE type = 'index' AND name = 'idx_faa_registry_snapshots_current'
) = 'createindexidx_faa_registry_snapshots_currentonfaa_registry_snapshots(snapshot_datedesc,iddesc)'
AND (
  SELECT group_concat(name || '|' || normalized_sql, char(10))
  FROM (
    SELECT name,
      replace(replace(replace(lower(sql), char(10), ''), char(9), ''), ' ', '')
        AS normalized_sql
    FROM sqlite_schema
    WHERE type = 'trigger' AND tbl_name = 'faa_registry_snapshots'
    ORDER BY name
  )
) = 'faa_registry_snapshots_immutable_delete|createtriggerfaa_registry_snapshots_immutable_deletebeforedeleteonfaa_registry_snapshotsbeginselectraise(abort,''faaregistrysnapshotsareimmutable'');end
faa_registry_snapshots_immutable_update|createtriggerfaa_registry_snapshots_immutable_updatebeforeupdateonfaa_registry_snapshotsbeginselectraise(abort,''faaregistrysnapshotsareimmutable'');end
faa_registry_snapshots_require_exact_evidence|createtriggerfaa_registry_snapshots_require_exact_evidencebeforeinsertonfaa_registry_snapshotswhennotexists(select1fromcuration_evidence_sourcessourcewheresource.id=new.evidence_source_idandsource.source_domain=''faa.gov''andsource.source_tier=''regulator_primary''andsource.source_url=new.source_urlandsource.content_sha256=new.archive_sha256)beginselectraise(abort,''faasnapshotrequiresexactregulatorevidenceprovenance'');end'
AND (
  WITH normalized(sql) AS (
    SELECT replace(replace(replace(lower(sql), char(10), ''), char(9), ''), ' ', '')
    FROM sqlite_schema
    WHERE type = 'table' AND name = 'faa_registry_snapshots'
  )
  SELECT
    (length(sql) - length(replace(sql, 'check(', ''))) / 6
      = CASE WHEN instr(sql, 'record_hash_domaintextnotnull') > 0 THEN 12 ELSE 11 END
    AND instr(sql, 'master_member_nametextnotnullcheck(master_member_name=''master.txt'')') > 0
    AND instr(sql, 'aircraft_member_nametextnotnullcheck(aircraft_member_name=''acftref.txt'')') > 0
    AND instr(sql, 'engine_member_nametextnotnullcheck(engine_member_name=''engine.txt'')') > 0
    AND instr(sql, 'unique(archive_sha256,target_set_sha256)') > 0
    AND instr(sql, 'check(snapshot_dateglob''[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'')') > 0
    AND instr(sql, 'check(source_urllike''https://faa.gov/%''orsource_urllike''https://%.faa.gov/%'')') > 0
    AND instr(sql, 'check(length(archive_sha256)=64andarchive_sha256notglob''*[^0-9a-f]*'')') > 0
    AND instr(sql, 'check(length(source_manifest_sha256)=64andsource_manifest_sha256notglob''*[^0-9a-f]*'')') > 0
    AND instr(sql, 'check(length(target_set_sha256)=64andtarget_set_sha256notglob''*[^0-9a-f]*'')') > 0
    AND instr(sql, 'check(length(master_member_sha256)=64andmaster_member_sha256notglob''*[^0-9a-f]*'')') > 0
    AND instr(sql, 'check(length(aircraft_member_sha256)=64andaircraft_member_sha256notglob''*[^0-9a-f]*'')') > 0
    AND instr(sql, 'check(length(engine_member_sha256)=64andengine_member_sha256notglob''*[^0-9a-f]*'')') > 0
  FROM normalized
);
INSERT INTO faa_record_hash_domain_guard (accepted)
SELECT CASE WHEN count(*) = 1 THEN 1 ELSE 0 END
FROM faa_record_hash_domain_guard;
DROP TABLE faa_record_hash_domain_guard;

-- The first transition is legal only for an empty legacy projection. A rerun
-- may rebuild an already attested current table, preserving every logical
-- value and the original migration timestamp.
DROP TABLE IF EXISTS temp.faa_registry_snapshot_domain_copy;
CREATE TEMP TABLE faa_registry_snapshot_domain_copy AS
SELECT
  id, evidence_source_id, snapshot_date, source_url, archive_sha256,
  source_manifest_sha256, target_set_sha256, master_member_name,
  master_member_sha256, aircraft_member_name, aircraft_member_sha256,
  engine_member_name, engine_member_sha256, imported_at
FROM faa_registry_snapshots;

DROP INDEX idx_faa_registry_snapshots_current;
DROP TRIGGER faa_registry_snapshots_require_exact_evidence;
DROP TRIGGER faa_registry_snapshots_immutable_update;
DROP TRIGGER faa_registry_snapshots_immutable_delete;
DROP TABLE faa_registry_snapshots;

CREATE TABLE faa_registry_snapshots (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  evidence_source_id INTEGER NOT NULL
    REFERENCES curation_evidence_sources(id) ON DELETE RESTRICT,
  snapshot_date TEXT NOT NULL,
  source_url TEXT NOT NULL,
  archive_sha256 TEXT NOT NULL,
  source_manifest_sha256 TEXT NOT NULL,
  target_set_sha256 TEXT NOT NULL,
  master_member_name TEXT NOT NULL CHECK (master_member_name = 'MASTER.txt'),
  master_member_sha256 TEXT NOT NULL,
  aircraft_member_name TEXT NOT NULL CHECK (aircraft_member_name = 'ACFTREF.txt'),
  aircraft_member_sha256 TEXT NOT NULL,
  engine_member_name TEXT NOT NULL CHECK (engine_member_name = 'ENGINE.txt'),
  engine_member_sha256 TEXT NOT NULL,
  imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  record_hash_domain TEXT NOT NULL CHECK (
    record_hash_domain = 'aircost-faa-master-retained-aircraft-projection-v1'
  ),
  UNIQUE (archive_sha256, target_set_sha256),
  CHECK (snapshot_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
  CHECK (source_url LIKE 'https://faa.gov/%' OR source_url LIKE 'https://%.faa.gov/%'),
  CHECK (length(archive_sha256) = 64 AND archive_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(source_manifest_sha256) = 64 AND source_manifest_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(target_set_sha256) = 64 AND target_set_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(master_member_sha256) = 64 AND master_member_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(aircraft_member_sha256) = 64 AND aircraft_member_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(engine_member_sha256) = 64 AND engine_member_sha256 NOT GLOB '*[^0-9a-f]*')
);

INSERT INTO faa_registry_snapshots (
  id, evidence_source_id, snapshot_date, source_url, archive_sha256,
  source_manifest_sha256, target_set_sha256, master_member_name,
  master_member_sha256, aircraft_member_name, aircraft_member_sha256,
  engine_member_name, engine_member_sha256, imported_at, record_hash_domain
)
SELECT
  id, evidence_source_id, snapshot_date, source_url, archive_sha256,
  source_manifest_sha256, target_set_sha256, master_member_name,
  master_member_sha256, aircraft_member_name, aircraft_member_sha256,
  engine_member_name, engine_member_sha256, imported_at,
  'aircost-faa-master-retained-aircraft-projection-v1'
FROM temp.faa_registry_snapshot_domain_copy;
DROP TABLE temp.faa_registry_snapshot_domain_copy;

CREATE INDEX idx_faa_registry_snapshots_current
  ON faa_registry_snapshots (snapshot_date DESC, id DESC);

CREATE TRIGGER faa_registry_snapshots_require_exact_evidence
BEFORE INSERT ON faa_registry_snapshots
WHEN NOT EXISTS (
  SELECT 1 FROM curation_evidence_sources source
  WHERE source.id = NEW.evidence_source_id
    AND source.source_domain = 'faa.gov'
    AND source.source_tier = 'regulator_primary'
    AND source.source_url = NEW.source_url
    AND source.content_sha256 = NEW.archive_sha256
)
BEGIN SELECT RAISE(ABORT, 'FAA snapshot requires exact regulator evidence provenance'); END;

CREATE TRIGGER faa_registry_snapshots_immutable_update
BEFORE UPDATE ON faa_registry_snapshots
BEGIN SELECT RAISE(ABORT, 'FAA registry snapshots are immutable'); END;
CREATE TRIGGER faa_registry_snapshots_immutable_delete
BEFORE DELETE ON faa_registry_snapshots
BEGIN SELECT RAISE(ABORT, 'FAA registry snapshots are immutable'); END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260820_faa_record_hash_domain',
  1,
  'f124f573bf705da6c1e4b0a5c7a8df45ea5a4a5dc009a28eee012be42c691502',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
PRAGMA foreign_key_check;
PRAGMA foreign_keys = ON;

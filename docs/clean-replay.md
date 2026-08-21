# Clean listing replay

Clean replay rebuilds derived listing state from an explicit set of retained,
signed plugin captures. It never copies listings, catalogs, reviews, Gemini
responses, or other derived rows from the source database.

The source must be a file-backed SQLite database. Replay commands open it with
SQLite read-only mode and do not run schema initialization, migrations, or seed
writes. The target must be a different database.

FAA registry dry runs have the same boundary: they require an existing target
database and use a dedicated read-only diagnostic connection. They do not
create a SQLite file or upgrade either backend. Before materialization, import
the exact retained FAA ZIP into the shadow database with `--apply`; projection
hashes cannot be recovered by relabeling or mechanically rehashing legacy FAA
rows.

## Frozen legacy source preparation

One historical SQLite source predates the versioned FAA retained-record hash
domain and the resumable replay ledger. It is not opened through ordinary
application startup and must never be repaired in place. The temporary
`prepare-legacy-replay-source` command is the only admitted conversion:

```text
aircost-admin prepare-legacy-replay-source \
  --source-database FROZEN_SOURCE \
  --source-database-sha256 3468cd90ff2799d3640764ed0097dd07aa28164b249a4a9134e646e98158f8fc \
  --manifest captures.json \
  --manifest-sha256 345b1566ec491488d3ba4d1db2855eb9ea8e9b1258a7fc799418c581581b5d00 \
  --faa-archive ReleasableAircraft.zip \
  --faa-archive-sha256 14885735825e5f46babdac8bf851c77c7ce7b104ae0f86395ef594e6e467c724 \
  --output PREPARED_SOURCE
aircost-admin prepare-legacy-replay-source \
  --source-database FROZEN_SOURCE \
  --source-database-sha256 3468cd90ff2799d3640764ed0097dd07aa28164b249a4a9134e646e98158f8fc \
  --manifest captures.json \
  --manifest-sha256 345b1566ec491488d3ba4d1db2855eb9ea8e9b1258a7fc799418c581581b5d00 \
  --faa-archive ReleasableAircraft.zip \
  --faa-archive-sha256 14885735825e5f46babdac8bf851c77c7ce7b104ae0f86395ef594e6e467c724 \
  --output PREPARED_SOURCE --apply
```

Dry run is the default and creates no output file. Both modes require the exact
reviewed source-database byte digest and semantic capture-manifest fingerprint,
the fixed legacy schema and migration-receipt inventories, and the exact
retained FAA ZIP. The source may not have a `-wal`, `-shm`, or `-journal`
sidecar. The bridge opens the original file once, copies and hashes those same
bytes into a private `0600` snapshot, and opens only that snapshot as immutable
and read-only. It then revalidates every selected signed capture and timestamp
and parses the FAA ZIP through the current privacy-minimizing importer. A
different source byte, schema, receipt set, archive, manifest member,
signature, owner, install, key, revocation instant, capture hash, or retained
non-PII FAA fact is rejected.

The output is a new current canonical-schema SQLite file. It contains only the
selected users, installs, signed captures with all derived fields reset, the
current-domain FAA projection rebuilt by the normal importer, and a typed
provider-free projection of the reusable verified aircraft and avionics
catalog closure. It does not copy listings, reviews, valuation data, provider
usage, Gemini responses, or legacy FAA projection rows. FAA-bound claims,
observations, resolution cases, decisions, and catalog bindings are rebuilt
against the parsed current-domain records. An exhaustive final scan rejects
any retained legacy FAA record or source-manifest digest.

Apply builds a sibling temporary file and publishes it with a no-replace
atomic rename only after current diagnostic startup, integrity, foreign-key,
taint, and zero-provider-usage checks pass. It synchronizes both the published
file and its parent directory before reporting success. Failures before the
rename remove the temporary file and leave the requested output absent. A
directory-synchronization failure occurs after publication and can therefore
leave a complete output whose crash durability is uncertain; inspect that file
before deciding whether to retain or remove it. The frozen source is never
modified. This command is a one-time administrative bridge, not runtime
compatibility; remove it after the reviewed source has been prepared and the
clean rebuild has cut over.

## Phases

All operational commands are dry-run unless `--apply` is supplied.

1. Export an explicit capture manifest:

   ```text
   aircost-admin export-replay-manifest --database SOURCE --all-bound \
     --output captures.json --apply
   ```

   `--all-bound` fails unless each selected listing has exactly one retained
   capture. `--submission-id` can instead select exact capture IDs. Export
   recomputes every HTML hash, verifies capture ownership and install ownership,
   and verifies the P-256 signature.

2. Import into an empty shadow target:

   ```text
   aircost-admin import-replay-manifest --source-database SOURCE \
     --database SHADOW --manifest captures.json --apply
   ```

   Import revalidates the live source against the manifest, imports only the
   selected users, installs, and capture bytes, preserves original IDs and
   submission timestamps, and resets extraction, error, and canonical-listing
   fields.

3. Create and inspect the extraction checkpoint:

   ```text
   aircost-admin replay-extraction --database SHADOW --submission-id ID
   aircost-admin replay-extraction --database SHADOW --submission-id ID --apply
   ```

   Dry-run is provider-free and fails closed on corrupt signatures or invalid
   existing checkpoints. Apply performs only current-schema listing extraction.
   It stops before FAA admission, catalog resolution, listing insertion, and
   finalization. The checkpoint retains the pinned visual-identity report when
   visual recovery was used.

4. Materialize the exact checkpoint:

   ```text
   aircost-admin replay-listing --database SHADOW --submission-id ID
   aircost-admin replay-listing --database SHADOW --submission-id ID --apply
   ```

   This does not repeat listing or visual extraction. It uses the ordinary FAA,
   aircraft, avionics, review, and finalization workflow in create-only mode, so
   it cannot refresh or repair a preexisting listing. The listing observation
   timestamp is restored from the capture's `submitted_at`. FAA rejection is a
   typed result and leaves the checkpoint unbound. Only immutable source-policy
   outcomes (missing, invalid, or non-N registration and serial conflict) are
   terminal. Registry lookup failures, unavailable or insufficient snapshots,
   and missing or mismatched canonical assignments remain retryable. For a
   narrow FAA serial correction, listing insertion and checkpoint binding are
   one transaction.
   A later failure retains that private receipt-gated pair, and an exact retry
   deterministically resumes child projections, writes one correction receipt,
   and finalizes the same listing. Before binding, any listing-creation failure
   rolls back the insert and exact capture CAS in their shared transaction; no
   replay path scans for or deletes listings as compensation.
   Install revocation does not invalidate an older signed capture: replay and
   reprocess accept it only when its retained `submitted_at` is at or before
   the retained install `revoked_at`. A capture timestamp after revocation is
   rejected as an impossible signed-source history. The exact key and
   revocation timestamp are pinned again by the atomic bind CAS.

For a manifest-sized replay, use the durable batch coordinator instead of a
shell loop:

```text
aircost-admin replay-captures --database SHADOW --manifest captures.json \
  --phase extraction
aircost-admin replay-captures --database SHADOW --manifest captures.json \
  --phase extraction --apply
aircost-admin replay-captures --database SHADOW --manifest captures.json \
  --phase materialization --apply
```

Fatal database, heartbeat, or final-ledger errors intentionally leave the
owner-fenced run in `running` state. After confirming that worker has stopped
and the conservative heartbeat threshold has elapsed, resume it explicitly
with `--recover-stale`; this requeues only the fenced in-flight item state.

The default is provider-free and read-only. `--submission-id ID` restricts one
invocation without changing the manifest-backed run membership. Apply records
independent extraction and materialization states, attempts, and timestamps.
Dry-run opens the installed target through a no-initialize diagnostic
connection, attests its schema contracts, and performs no seed, schema,
migration-contract timestamp, or ledger writes.
Before any provider-backed retry, it derives an already-committed checkpoint or
exact materialization-completion receipt from the authoritative stores and
completes the ledger without repeating that work. A capture binding alone is
only a resumable ownership anchor: if its receipt is absent, replay rebuilds
the deterministic child projections and records completion last. Each
successful extraction also pins both the exact normalized extraction JSON and
its checkpoint SHA-256 in the run member; materialization refuses a different
payload even when the signed capture itself is unchanged.

Only one replay run may own mutations at a time. Its opaque owner token is
heartbeated during long operations and fences every item completion. There is
no automatic time-only takeover. If a worker was killed, first confirm it is no
longer running; after one hour without a heartbeat, repeat the apply command
with `--recover-stale`. The displaced token cannot commit a later ledger
transition or replace the first committed extraction checkpoint. Loss of the
heartbeat/owner lease cancels the in-flight provider operation promptly; resume
then reconciles any authoritative checkpoint or completion receipt before
retrying.

Each report includes `gemini_usage` with the explicit
`manifest_phase_cumulative` scope. A stable correlation ID is derived from the
manifest fingerprint and phase, while each accounting row retains its exact
submission source ID. The totals therefore include every request across
resumptions of that phase: logical requests, transport attempts, retries,
provider token/search counters, and estimated cost when the provider supplied
complete billable usage. A retry that only reconciles an already-committed
checkpoint makes no new request, but still reports the phase's earlier cost
instead of losing durable attribution.

The ledger stores no HTML, raw provider response envelope, or raw rejection
message. For a successful extraction, it intentionally stores the exact
normalized extraction JSON alongside its SHA-256. That immutable pair lets a
resumed materialization prove it is using the checkpoint that the extraction
phase committed. Terminal rejection stage and reason use a closed vocabulary;
FAA source-policy rejections retain a stable policy reason code. Transient FAA
lookup and mutable catalog/snapshot readiness failures use a separate closed
retry-failure vocabulary; database and other operation failures likewise remain
distinct from terminal rejection.

## Avionics terminal state

Each current extraction occurrence component is exactly one of:

- linked to a verified avionics product;
- discarded with a bounded decision reason; or
- pending in the listing review bundle.

Automatic terminal receipts are written atomically from the resolver's exact
occurrence/action graph. They do not rematch raw listing typography to canonical
catalog labels. Pending components receive no terminal receipt. Manual review
writes the corresponding immutable receipt when it resolves the aspect.

`reconcile-replay-avionics` is a provider-free audit/backfill for already-bound
captures. It records only unambiguous, provable retained associations and never
infers a discard because a listing link is absent.

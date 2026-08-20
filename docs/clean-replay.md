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
   and finalizes the same listing. Uncorrected replay failures still compensate
   the newly created listing and binding.

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

The default is provider-free and read-only. `--submission-id ID` restricts one
invocation without changing the manifest-backed run membership. Apply records
independent extraction and materialization states, attempts, and timestamps.
Before any provider-backed retry, it derives an already-committed checkpoint or
capture binding from the authoritative submission and completes the ledger
without repeating that work. Each successful extraction also pins the exact
checkpoint SHA-256 in the run member; materialization refuses a different
payload even when the signed capture itself is unchanged.

Only one replay run may own mutations at a time. Its opaque owner token is
heartbeated during long operations and fences every item completion. There is
no automatic time-only takeover. If a worker was killed, first confirm it is no
longer running; after one hour without a heartbeat, repeat the apply command
with `--recover-stale`. The displaced token cannot commit a later ledger
transition or replace the first committed extraction checkpoint. Loss of the
heartbeat/owner lease cancels the in-flight provider operation promptly; resume
then reconciles any authoritative checkpoint or listing commit before retrying.

Each report includes `gemini_usage` with the explicit
`manifest_phase_cumulative` scope. A stable correlation ID is derived from the
manifest fingerprint and phase, while each accounting row retains its exact
submission source ID. The totals therefore include every request across
resumptions of that phase: logical requests, transport attempts, retries,
provider token/search counters, and estimated cost when the provider supplied
complete billable usage. A retry that only reconciles an already-committed
checkpoint makes no new request, but still reports the phase's earlier cost
instead of losing durable attribution.

The ledger stores no HTML, extraction JSON, Gemini response, or raw rejection
message. Terminal rejection stage and reason use a closed vocabulary; FAA
source-policy rejections retain a stable policy reason code. Transient FAA
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

# Clean listing replay

Clean replay rebuilds derived listing state from an explicit set of retained,
signed plugin captures. It never copies listings, catalogs, reviews, Gemini
responses, or other derived rows from the source database.

The source must be a current-schema, verified SQLite or PostgreSQL database.
Replay commands open it diagnostically without schema initialization,
migrations, or seed writes. SQLite sources must be existing file-backed
databases and are opened read-only; PostgreSQL diagnostic connections enforce
read-only transactions. The target must be a different database.

## Phases

All operational commands are dry-run unless `--apply` is supplied.

1. Export an explicit capture manifest:

   ```text
   aircost-admin export-replay-manifest --database SOURCE --all-bound \
     --expected-capture-count 70 --output captures.json \
     --readiness-output capture-readiness.json
   aircost-admin export-replay-manifest --database SOURCE --all-bound \
     --expected-capture-count 70 --output captures.json \
     --readiness-output capture-readiness.json --apply
   ```

   Export evaluates the complete source inventory and builds the manifest from
   one read transaction, so readiness, exclusions, capture count, and manifest
   fingerprint all describe the same database snapshot. `--all-bound` includes
   `--expected-capture-count` when the operator knows the reviewed inventory;
   a count change makes the source not ready instead of silently shrinking or
   expanding the replay. `--submission-id` can instead select exact capture IDs
   and cannot be combined with the expected-count option.

   The readiness report contains the closed database checks, source inventory,
   per-capture results, excluded submission IDs, and manifest fingerprint. The
   export recomputes every HTML hash, verifies capture and install ownership,
   requires every all-bound capture owner and signed source URL to match its
   listing creator and source URL exactly, checks install/submission/revocation
   chronology, and verifies the P-256 signature. Unbound submissions outside
   the selected listing set are reported explicitly as exclusions and warnings.
   An ambiguous or hostile listing binding, count mismatch, corrupt selected
   capture, or database-integrity failure leaves the source not ready.

   Dry-run prints that readiness report and creates neither requested file.
   With `--apply`, a ready snapshot publishes the manifest and, when requested,
   the readiness report. A non-ready snapshot publishes only the requested
   readiness report, leaves `captures.json` absent, and exits unsuccessfully.
   The two output paths must be different and neither is overwritten. Each JSON
   artifact is pretty-printed with a final newline into a sibling `0600`
   temporary file, flushed and synchronized, then published without clobbering
   an existing path; its parent directory is synchronized before success is
   reported. When both artifacts are requested, the readiness report is
   published first and the manifest last, making the manifest the final usable
   replay artifact. A failure after publishing the readiness report can leave
   that diagnostic file while the manifest remains absent. A file or directory
   synchronization failure after a no-clobber rename can leave a complete
   artifact whose crash durability is uncertain; inspect it before deciding
   whether to retain it, because a retry will not overwrite it.

2. Import into an empty shadow target:

   ```text
   aircost-admin import-replay-manifest --source-database SOURCE \
     --database SHADOW --manifest captures.json --apply
   ```

   Import revalidates the live source against the manifest, imports only the
   selected users, installs, and capture bytes, preserves original IDs and
   submission timestamps, and resets extraction, error, and canonical-listing
   fields.

3. Seed the exact reviewed catalog closure from the current verified source:

   ```text
   aircost-admin seed-verified-catalog --source-database SOURCE \
     --catalog-fingerprint-sha256 REVIEWED_SHA256 --database SHADOW
   aircost-admin seed-verified-catalog --source-database SOURCE \
     --catalog-fingerprint-sha256 REVIEWED_SHA256 --database SHADOW --apply
   ```

   The source is opened diagnostically. Apply takes the target writer lock
   before its final exhaustive base-table scan, admits only exact schema
   bootstrap rows plus immutable imported users, installs, and captures, and
   installs only the approved aircraft/FAA/reference and avionics closure. The
   locked validation requires the exact startup migration-receipt inventory and
   reauthenticates every retained capture's owner, HTML digest, P-256 signature,
   and install/submission/revocation chronology.
   Avionics models pass through the schema's normal unreviewed-to-approved
   transition; generated manufacturer keys, product identities, and curated
   origins must match the projection instead of being replaced. The operation
   has no provider client, is transactional, resets identity sequences, then
   reloads the target through the same catalog projection both before commit
   and after reopening the database, requiring exact fingerprint, row, and
   count parity. A second invocation is rejected because the target is no
   longer clean.

   SQLite takes `BEGIN IMMEDIATE` before the final scan. PostgreSQL first takes
   a transaction-scoped advisory lock for competing seed commands, discovers
   every public base table, and then locks that exact inventory in `SHARE ROW
   EXCLUSIVE` mode before repeating discovery and checking any rows. This mode
   conflicts with the `ROW EXCLUSIVE` lock acquired by ordinary
   `INSERT`/`UPDATE`/`DELETE` statements, so an application writer cannot race
   the clean check or any materialization write; unlike `ACCESS EXCLUSIVE`, it
   does not unnecessarily block plain diagnostic reads. Sequence resets use
   transactional `ALTER SEQUENCE ... RESTART`, including both identity and
   serial-owned `id` sequences—never non-transactional `setval`.

4. Create and inspect the extraction checkpoint:

   ```text
   aircost-admin replay-extraction --database SHADOW --submission-id ID
   aircost-admin replay-extraction --database SHADOW --submission-id ID --apply
   ```

   Dry-run is provider-free and fails closed on corrupt signatures or invalid
   existing checkpoints. Apply performs only current-schema listing extraction.
   It stops before FAA admission, catalog resolution, listing insertion, and
   finalization. The checkpoint retains the pinned visual-identity report when
   visual recovery was used.

5. Materialize the exact checkpoint:

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
An applied single-submission report includes `selected_item` with the exact
phase state and its stable ledger reason code. A failed operation also includes
one bounded `transient_error` classified as `schema`, `evidence`, `provider`, or
`database`. That diagnostic is assembled only for the command response from
fixed sanitized text; raw provider responses, prompts, listing content, and
source dossiers are neither returned nor added to the durable replay ledger.
Automation must stop when the selected state is `failed`, `rejected`, or
`blocked`, even though the coordinator retains batch-style success exit status
after recording an item-level outcome.
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

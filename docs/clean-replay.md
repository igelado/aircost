# Clean listing replay

Clean replay rebuilds derived listing state from an explicit set of retained,
signed plugin captures. It never copies listings, catalogs, reviews, Gemini
responses, or other derived rows from the source database.

The source must be a file-backed SQLite database. Replay commands open it with
SQLite read-only mode and do not run schema initialization, migrations, or seed
writes. The target must be a different database.

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
   typed result and leaves the checkpoint unbound. Any failure after creation
   compensates the newly created listing and binding.

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


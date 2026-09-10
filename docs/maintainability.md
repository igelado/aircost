# Maintainability and Web UX Improvement Register

Last audited: 2026-09-09

This is the living register for simplifying AirCost and improving its web and
extension interfaces. It records outcomes rather than scattered source-code
TODOs so that each improvement has an owner, dependencies, preserved
invariants, and an objective definition of done.

## Non-negotiable constraints

- SQLite and PostgreSQL are intentional, first-class supported backends.
  Simplification must share orchestration, query primitives, fixtures, and
  contracts without deleting either backend or weakening backend-specific
  transaction and DDL guarantees.
- Existing provenance, fail-closed verification, source authentication,
  idempotency, immutable-history, and human-review guarantees must survive all
  simplification work.
- Prefer removing obsolete compatibility paths over adding another adapter.
- Do not split large files until the domain boundary being extracted has one
  authoritative model and transition path. File movement alone is not a
  simplification.
- Performance changes must retain deterministic results and explicit resource
  bounds.

## Audit baseline

- About 252,000 lines of Rust, with several production files between 10,000 and
  21,000 lines.
- The current SQLite database has 105 tables, 277 triggers, 17 views, and 66
  explicit indexes.
- There are 33 logical migrations represented by 65 dialect files and about
  33,000 lines of migration SQL.
- Warm full Rust test runs observed during this audit took between 4.5 and 8
  minutes and used about 700 MB of memory.
- All 87 current browser and extension tests pass, but most UI tests are source
  or markup contract checks rather than behavioral interaction tests.
- `cargo clippy --locked --all-targets --all-features` emits more than 100
  library warnings, dominated by oversized argument lists, enum variants, and
  complex types. Warnings are not currently a CI gate.
- A fresh empty web app was rendered at 1440 x 1000 and 390 x 844. The mobile
  render has overlapping primary navigation, and the desktop listing toolbar
  compresses ten filtering/action controls into one row.
- The checked-in development database currently fails startup contract
  validation. Applying the migration named by the error to a disposable copy
  also fails because the recorded contract and physical table shape disagree.
  The original database was not modified.

Measurements are evidence, not permanent targets. Record new before/after
measurements when an item is completed.

## Status and ownership rules

- Status is one of `open`, `accepted`, `in_progress`, `blocked`, `done`, or
  `declined`.
- `accepted` and `in_progress` require an owner.
- `blocked` requires a blocker and a concrete unblock condition.
- `done` requires every acceptance criterion plus a PR or commit and
  before/after evidence.
- `declined` requires a rationale.
- Backend-touching work is not done until its SQLite and PostgreSQL criteria
  pass.
- Line numbers below are observations from the audit date. Stable symbols and
  paths are the durable anchors.

## Index

| ID | Priority | Status | Owner | Area | Backend | Outcome |
| --- | --- | --- | --- | --- | --- | --- |
| MNT-001 | P0 | open | unassigned | persistence | both | Atomic application transactions |
| MNT-002 | P0 | open | unassigned | review | both | One authoritative review-aspect model |
| MNT-003 | P0 | open | unassigned | database | both | Versioned, repairable startup and migrations |
| MNT-004 | P0 | open | unassigned | listings | both | Bounded list reads and targeted cleanup |
| MNT-005 | P0 | open | unassigned | tests/CI | both | CI executes the supported test inventory |
| MNT-006 | P1 | open | unassigned | aircraft | both | One canonical aircraft identity write model |
| MNT-007 | P1 | open | unassigned | database | both | Shared dual-backend execution boundary |
| MNT-008 | P1 | open | unassigned | listing | both | Finish the legacy listing-module migration |
| MNT-009 | P1 | open | unassigned | API/domain | none | Typed commands, pages, and error contracts |
| MNT-010 | P1 | open | unassigned | catalog | both | One source for shared vocabularies and rules |
| MNT-011 | P1 | open | unassigned | verification | both | Reusable revision-bound execution plans |
| MNT-012 | P1 | open | unassigned | ingestion/replay | both | Shared lifecycle engines and compact CAS guards |
| MNT-013 | P1 | open | unassigned | valuation | both | Atomic, set-based, sparse model workflows |
| MNT-014 | P2 | open | unassigned | network/bulk | both | Bounded-concurrent and set-based bulk work |
| MNT-015 | P1 | open | unassigned | frontend | none | Incremental controller and mutation architecture |
| MNT-016 | P1 | open | unassigned | extension | none | Background-owned capture and one canonical editor |
| MNT-017 | P1 | open | unassigned | tooling/docs | both | Declarative CLI and accurate documentation |
| MNT-018 | P1 | open | unassigned | quality | both | Fast fixtures, lint policy, and pinned toolchains |
| MNT-019 | P2 | open | unassigned | storage | both | Cold capture content and query-aligned indexes |
| MNT-020 | P2 | open | unassigned | compatibility | none | Time-bounded compatibility policy |
| MNT-021 | P1 | open | unassigned | avionics catalog | both | Retire products without rewriting history |
| MNT-022 | P1 | open | unassigned | catalog integrity | both | One catalog-mutation authorization protocol |
| MNT-023 | P1 | open | unassigned | capture contracts | both | One typed checkpoint and capture identity |
| MNT-024 | P2 | open | unassigned | aircraft curation | both | Separate diagnostics from executable commands |
| MNT-025 | P2 | open | unassigned | verification runs | both | Derive run summaries from one terminal result |
| WEB-001 | P0 | open | unassigned | responsive UI | none | Navigation works at every supported width |
| WEB-002 | P1 | open | unassigned | information architecture | none | Task-oriented navigation and deep links |
| WEB-003 | P1 | open | unassigned | listings UX | none | Progressive listing discovery and useful zero state |
| WEB-004 | P1 | open | unassigned | review UX | none | A comprehensible review work queue |
| WEB-005 | P1 | open | unassigned | forms | none | Guided, accessible listing editing |
| WEB-006 | P1 | open | unassigned | aircraft UX | none | Deliberate, explainable valuation exploration |
| WEB-007 | P1 | open | unassigned | avionics UX | none | Searchable catalog with clear scope and actions |
| WEB-008 | P1 | open | unassigned | system feedback | none | Explicit loading, stale, empty, and error states |
| WEB-009 | P1 | open | unassigned | tables | none | Responsive priority views instead of wide overflow |
| WEB-010 | P1 | open | unassigned | visual design | none | Small, coherent visual design system |
| WEB-011 | P1 | open | unassigned | accessibility | none | Keyboard, focus, form, and status semantics |
| WEB-012 | P2 | open | unassigned | content design | none | Plain, consistent workflow language |
| WEB-013 | P1 | open | unassigned | extension UX | none | Fast capture-first extension experience |
| WEB-014 | P1 | open | unassigned | UI tests | none | Behavioral coverage for critical interactions |

## Maintainability and performance findings

### MNT-001 — Make each local application operation atomic

- Priority: P0
- Status: open
- Owner: unassigned
- Area: persistence
- Backend scope: both
- Depends on: MNT-007

Evidence:

- `persist_snapshot` in `src/valuation/dataset.rs:1128` checks for an existing
  parent, inserts it, and then inserts children individually through the pool.
  A retry can accept the partially populated parent.
- Structural version, artifact, and fold rows are separate commits in
  `src/valuation/store.rs:67` and `:146`; the DNN store already demonstrates a
  complete transaction at `src/valuation/dnn/store.rs:359`.
- Listing scalar fields commit before identity, equipment, and valuation facts
  in `src/listings.rs:3214-3275`. Fact replacement deletes and reinserts outside
  one transaction around `src/listings.rs:7605`.

Desired simplification:

Use one transaction-scoped unit of work for all deterministic database effects
of a listing mutation, valuation snapshot, or model-candidate write. Reserve
durable sagas and compensation for real external-service boundaries.

Preserved invariants:

- Last-moment admission and revision checks remain fail-closed.
- Idempotency and immutable snapshot hashes remain enforced.
- SQLite and PostgreSQL retain their native isolation/locking behavior.

Acceptance criteria:

- [ ] Repository writes accept an existing backend transaction.
- [ ] Parent, artifact, and child rows commit or roll back together.
- [ ] An existing content hash is returned only after child count/hash
      validation.
- [ ] Failure-injection tests after every write stage leave no partial state on
      SQLite or PostgreSQL.
- [ ] Child persistence is batched where it lowers round trips without hiding
      row-level validation.

### MNT-002 — Make review aspects the single source of truth

- Priority: P0
- Status: open
- Owner: unassigned
- Area: listing review and avionics
- Backend scope: both
- Depends on: MNT-001, MNT-006

Evidence:

- Mutable pending-review JSON begins around `schema/sqlite.sql:3012`, while
  dispositions and association authorizations store overlapping conclusions
  elsewhere, including `schema/sqlite.sql:8436`.
- `restage_pending_review_if_current_with_commit` spans about 1,700 lines in
  `src/listing/review.rs:1998-3721`; another full-resolution path repeats much
  of the persistence around `src/listing/review.rs:9651`.
- Human, automated, and verification association paths use different command
  and prepared-link types in `src/listing/review/automation.rs`,
  `src/listing/review.rs`, and `src/avionics/verification.rs`.
- Queue reads invoke mutating preparation/restaging work.

Desired simplification:

Normalize stable review-aspect rows, separate immutable observations from
mutable decisions, and route every decision origin through one pure transition
engine and one persistence implementation. Keep the existing JSON only as a
temporary API projection during migration.

Preserved invariants:

- Human and automatic evidence policies remain different and are recorded as
  the decision origin.
- Replacement, occurrence, collision, and provenance guarantees remain
  explicit.
- Queue GETs become side-effect-free; explicit repair remains available.

Acceptance criteria:

- [ ] Every retained occurrence has one stable aspect ID and one current state.
- [ ] Source observation, decision history, replacement, and covered-aspect
      relationships use normalized keys/foreign keys.
- [ ] `allowed_actions` is derived rather than persisted and repaired.
- [ ] Human, automatic, replay, and verification decisions use one
      `ReviewDecision -> ReviewDiff` transition.
- [ ] The 1,700-line restage function and duplicate resolution persistence are
      removed.
- [ ] Refreshing or opening a queue performs no writes.

### MNT-003 — Replace schema replay with a versioned, repairable migration path

- Priority: P0
- Status: open
- Owner: unassigned
- Area: database/startup
- Backend scope: both
- Depends on: MNT-005

Evidence:

- `AppDb::initialize_transactionally` in `src/db.rs:10189` runs the full
  migration/contract gate, executes every canonical schema statement, and runs
  the gate again on every connection for both databases.
- The required-migration gate begins at `src/db.rs:1533` and spans thousands of
  lines. Runtime source also embeds historical migration/object definitions,
  contrary to the repository rule that runtime code remain migration-free.
- A focused fresh-schema reinitialization test took about 15 seconds.
- During this audit, startup told the operator to apply
  `20260725_identity_deduplication_postconditions`; applying that exact file to
  a disposable database copy failed because its recorded contract and physical
  `avionics_manufacturer_identities` columns disagree.

Desired simplification:

Use an ordered migration manifest with immutable checksums. Fresh databases
apply the canonical schema once; existing databases validate the ledger once
and run only pending migrations. Move exhaustive object attestation to a
doctor/CI command with an explicit repair report.

Preserved invariants:

- Unknown, incomplete, reordered, and tampered migrations still fail closed.
- PostgreSQL search-path hardening and advisory serialization remain.
- SQLite serialized schema changes remain transactional.

Acceptance criteria:

- [ ] An up-to-date database does not replay `CREATE IF NOT EXISTS` schema SQL.
- [ ] Startup reports the ordered pending migration set and one executable
      command, rather than naming a migration that cannot repair the state.
- [ ] `aircost-admin db doctor` reports ledger, shape, and repair diagnostics
      without writing; migration apply supports dry-run and backup guidance.
- [ ] Current-state object contracts come from canonical schema/manifest data,
      not embedded historical migration bodies.
- [ ] Warm startup of an up-to-date local SQLite database is below one second;
      a PostgreSQL threshold is measured and recorded before implementation.
- [ ] Hostile-schema and interrupted-migration tests pass for both backends.

### MNT-004 — Bound list reads and remove global cleanup from requests

- Priority: P0
- Status: open
- Owner: unassigned
- Area: listings and catalog reads
- Backend scope: both
- Depends on: MNT-009

Evidence:

- `list_listings` at `src/listings.rs:1905` reads every visible listing and then
  hydrates each row with three sequential queries. The HTTP handler at
  `src/server.rs:506` accepts no page parameters.
- Avionics catalog filtering/pagination occurs after broad loading and in-memory
  work in `src/avionics/inspection.rs:915-951`.
- `src/cleanup.rs:56` performs seven global `DELETE ... NOT EXISTS` sweeps;
  normal finalization/update paths can invoke it twice and ignore one error.

Desired simplification:

Make pages and summaries explicit contracts, batch-load only their related
rows, push filtering/count/order into SQL, and delete only IDs made orphaned by
the current transaction. Keep the broad sweep as an administrator maintenance
operation.

Acceptance criteria:

- [ ] Listing and catalog endpoints require bounded page sizes and stable
      cursors.
- [ ] Listing page hydration uses a constant number of queries.
- [ ] Catalog capability/usage loading is scoped to the returned product IDs.
- [ ] No normal request executes a database-wide orphan sweep.
- [ ] Query-count and response-size regression tests cover 1, 50, and 1,000
      available records on both backends.

### MNT-005 — Make CI execute the supported test inventory

- Priority: P0
- Status: open
- Owner: unassigned
- Area: tests/CI
- Backend scope: both
- Depends on: none

Evidence:

- `.github/workflows/test.yml:78-104` invokes only 6 of 17 scripts in
  `tests/schema`; the unreferenced scripts contain substantive adversarial
  coverage.
- There are 36 ignored Rust tests. After excluding four genuinely manual
  fixture tests, roughly 18 PostgreSQL integration tests are not selected by
  the workflow.
- The optional DNN/Burn feature is not compiled or tested in CI.

Desired simplification:

Use test categories and discovering runners instead of manually selecting
individual tests/scripts in workflow YAML.

Acceptance criteria:

- [ ] One schema runner discovers or registers every schema test and fails when
      a new script is omitted.
- [ ] All environment-backed PostgreSQL tests run through one serial category;
      only manual external-fixture tests remain ignored.
- [ ] CI compiles and tests the DNN feature.
- [ ] Fast pure/unit feedback and slower database-contract suites are separate
      jobs with shared reporting.
- [ ] Both database jobs are required checks.

### MNT-006 — Complete the canonical aircraft identity cutover

- Priority: P1
- Status: open
- Owner: unassigned
- Area: aircraft identity
- Backend scope: both
- Depends on: MNT-001, MNT-003

Evidence:

- Legacy aircraft identity tables begin around `schema/sqlite.sql:492`, while
  the canonical make/family/designation/generation/package hierarchy begins
  around `schema/sqlite.sql:4781`.
- Compatibility projections, command rows, placeholders, and roughly 80
  triggers synchronize the two representations around
  `schema/sqlite.sql:7232`.

Desired simplification:

Make the canonical hierarchy the sole application write model, backfill direct
canonical references, validate a temporary dual-read period, and then delete
the projection command/placeholder machinery and legacy identity columns.

Acceptance criteria:

- [ ] Every listing, valuation row, and reference path reads canonical IDs.
- [ ] Every writer writes canonical IDs directly.
- [ ] A measured dual-read period reports zero mismatches on both backends.
- [ ] Compatibility command tables, placeholder rows, projection triggers, and
      legacy identity columns are removed in an explicit cutover migration.
- [ ] Rollback/export steps are documented before destructive schema removal.

### MNT-007 — Centralize dual-backend execution without erasing dialect safety

- Priority: P1
- Status: open
- Owner: unassigned
- Area: database abstraction
- Backend scope: both
- Depends on: none

Evidence:

- There are hundreds of `DatabaseBackend::{Sqlite, Postgres}` dispatch sites.
- Local variants of `query_as_all`, `query_as_optional`, scalar, and execute
  macros are repeated in `src/listings.rs:83-151`, `src/aircraft.rs:32-57`,
  `src/aircraft/reference/persistence.rs:274-310`, `src/avionics/mod.rs:32-70`,
  and other modules.
- `postgres_placeholders` at `src/db.rs:10675` rewrites every `?` character and
  is not SQL-token aware.

Desired simplification:

Hide pools and common dispatch behind transaction/executor primitives in
`db`. Keep backend-specific SQL, locks, casts, and DDL explicit and tested. Do
not adopt an abstraction solely because it makes code look uniform.

Acceptance criteria:

- [ ] Common fetch/execute/transaction dispatch is defined once.
- [ ] Domain services do not pattern-match pools for ordinary CRUD.
- [ ] Dialect-specific repositories are limited to genuinely different SQL or
      concurrency behavior.
- [ ] Placeholder binding is driver-native or token-aware and tested with
      literals, comments, and PostgreSQL operators.
- [ ] Duplicate dispatch/macro LOC decreases materially while both backend
      suites continue to pass.

### MNT-008 — Finish the listing module migration

- Priority: P1
- Status: open
- Owner: unassigned
- Area: listing architecture
- Backend scope: both
- Depends on: MNT-001, MNT-002, MNT-007, MNT-009

Evidence:

- `src/listing/mod.rs:1` explicitly describes its contents as workflows outside
  the legacy listing-store module.
- `src/listings.rs` remains about 15,300 lines and combines DTO hydration,
  creation, updates, finalization, facts, equipment, cleanup, and tests.

Desired simplification:

Move coherent behavior into `listing::{model, command, query, repository,
finalization}` in bounded changes, then delete `listings.rs` without a
compatibility re-export.

Acceptance criteria:

- [ ] Each extraction removes behavior from `listings.rs`; no forwarding shell
      or duplicate implementation remains.
- [ ] Query projections are separate from mutation commands.
- [ ] Finalization and transactional persistence share one unit of work.
- [ ] `src/listings.rs` and its plural public module are deleted.
- [ ] SQLite and PostgreSQL listing, plugin, replay, and review tests pass after
      every extraction.

### MNT-009 — Replace stringly transport contracts with typed domain commands

- Priority: P1
- Status: open
- Owner: unassigned
- Area: API/domain boundary
- Backend scope: none
- Depends on: none

Evidence:

- Listing preview/update accepts raw `serde_json::Value` in
  `src/models.rs:171`; `src/listings.rs:4793-5091` manually reimplements field,
  null, enum, and nested-object deserialization.
- `src/server.rs:286-429` defines a large flat router with many specialized
  review mutation endpoints.
- Although `ApiError` supports codes/details at `src/server.rs:2240`, browser
  review behavior still regex-parses human error messages to recover aspect
  identity.

Desired simplification:

Deserialize once into typed, unknown-field-denying commands. Use reusable page
and mutation-result DTOs, structured field/domain errors, and one tagged
aspect-decision endpoint/application service.

Acceptance criteria:

- [ ] Listing patch fields have explicit omitted/null/value semantics.
- [ ] Status, confidence, time basis, configuration action, and review decision
      are enums at the transport boundary.
- [ ] Clients never inspect error prose for control flow.
- [ ] Resource responses omit the unused repeated `current_user` envelope.
- [ ] The unused `original_listing` creation argument at
      `src/listings.rs:997`/`:3326` is removed rather than carried into the new
      command.
- [ ] One tagged review-decision endpoint replaces action-per-endpoint dispatch
      after compatibility callers are migrated.

### MNT-010 — Generate shared vocabularies and validation fixtures once

- Priority: P1
- Status: open
- Owner: unassigned
- Area: catalog/domain rules
- Backend scope: both
- Depends on: MNT-003, MNT-009

Evidence:

- Generic avionics labels are enumerated independently in
  `src/normalize.rs:82-184`, embedded PostgreSQL function source in
  `src/db.rs:113-154`, canonical schemas, and multiple migrations.
- Avionics capability lists and listing validation are independently encoded in
  the Rust server, `web/app.js:16-42`, and `chrome-extension/popup.js:6-32`.
  Validation has already drifted: web asking price minimum is 10,000 while the
  extension accepts zero.

Desired simplification:

Maintain one versioned domain-data source and generate Rust constants, SQL
fragments/seed data, client options, and cross-language fixtures from it.

Acceptance criteria:

- [ ] One reviewed source owns each shared vocabulary and constraint.
- [ ] Generated artifacts carry a source version/hash and are checked for
      freshness in CI.
- [ ] Rust, SQLite, PostgreSQL, web, and extension parity tests consume the same
      fixtures.
- [ ] Hand-copied lists and client-specific price rules are removed.

### MNT-011 — Execute reusable, revision-bound plans

- Priority: P1
- Status: open
- Owner: unassigned
- Area: verification/catalog
- Backend scope: both
- Depends on: MNT-002, MNT-004, MNT-007

Evidence:

- Composite verification preflights every listing and execution repeats the
  same aircraft and avionics work in `src/listing/verification.rs:395-482` and
  `:643-679`.
- Avionics verification similarly computes a complete preflight report and then
  reconstructs the work around `src/avionics/verification.rs:539-1249`.
- Product loading can create capability-by-component Cartesian rows and filter
  whole catalogs in memory.
- Catalog consistency frequently uses broad table locks or full-content hashes.

Desired simplification:

Return a `Prepared...Plan` carrying resolved inputs, affected IDs, expected
provider work, and relevant monotonic revisions. Execute that plan directly and
revalidate only mutable commit guards.

Acceptance criteria:

- [ ] Dry-run and apply use the same prepared plan representation.
- [ ] One request/page-scoped catalog snapshot is shared across listings.
- [ ] Product, capability, component, authorization, and status data are loaded
      in bounded set queries scoped to affected IDs.
- [ ] Monotonic revisions replace whole-catalog hashing on online paths.
- [ ] Narrow row/revision guards replace broad catalog locks where uniqueness
      constraints already arbitrate conflicts.

### MNT-012 — Consolidate ingestion/replay lifecycle engines and guards

- Priority: P1
- Status: open
- Owner: unassigned
- Area: plugin ingestion and replay
- Backend scope: both
- Depends on: MNT-001, MNT-009, MNT-011

Evidence:

- New plugin submission and reprocessing duplicate extraction, admission,
  materialization, error classification, and receipt handling in
  `src/plugin.rs:414-682` and `:728-971`.
- Listing creation repeats three reuse branches in `src/listings.rs:1175-1290`
  and accepts parameter combinations that represent invalid states.
- Replay authenticates/fingerprints a frozen target and then repeatedly checks
  membership and large row payloads while active-run database triggers already
  prevent target mutation.

Desired simplification:

Create one typed `process_capture` lifecycle, one candidate-reuse path, and one
materialization receipt. Authenticate a replay target once per lease and use
compact revision/token compare-and-swap guards thereafter.

Acceptance criteria:

- [ ] New and stored captures use one state machine with source-specific input
      variants.
- [ ] Admission is computed once and passed to listing creation.
- [ ] Candidate selection returns one `ReuseCandidate` before one update path.
- [ ] Replay persists one target digest/revision per acquired lease and item
      transitions use item state plus owner token.
- [ ] Domain materialization receipt is authoritative for terminal outcome.

### MNT-013 — Make valuation persistence set-based and fitting sparse

- Priority: P1
- Status: open
- Owner: unassigned
- Area: valuation
- Backend scope: both
- Depends on: MNT-001, MNT-004, MNT-011

Evidence:

- Snapshot preparation performs per-listing reference/authorization queries and
  inserts every child independently in `src/valuation/dataset.rs`.
- Structural fitting repeatedly rebuilds a dense design matrix and performs
  dense normal-equation/Cholesky solves for sparse categorical inputs in
  `src/valuation/structural.rs:425-580`.
- DNN metadata advertises a batch size of at most 32 while training rebuilds a
  full encoded batch every epoch in `src/valuation/dnn/train.rs:368-466`.

Desired simplification:

Bulk-load snapshot inputs, persist them atomically, reuse one feature layout,
and use sparse or block-structured fitting. Implement actual mini-batches or
truthfully reuse one pre-encoded full batch.

Acceptance criteria:

- [ ] Snapshot preparation and persistence use bounded query counts and one
      transaction on both databases.
- [ ] Structural design data is built once and represented sparsely.
- [ ] Parameter search reuses work or warm starts.
- [ ] DNN schedule metadata matches actual batching.
- [ ] Production-scale benchmarks record wall time, allocations/peak memory,
      and unchanged validation metrics.

### MNT-014 — Bound concurrency and make bulk workflows genuinely bulk

- Priority: P2
- Status: open
- Owner: unassigned
- Area: FAA, backfill, citations, images
- Backend scope: both
- Depends on: MNT-004, MNT-011

Evidence:

- FAA admission and legacy review backfill perform broad reads followed by
  sequential per-listing work.
- Citation/final-URL and publisher fetches process up to 20 URLs sequentially in
  `src/gemini/curation/workflow.rs` despite long per-request timeouts.
- Listing image evidence downloads up to 12 images sequentially in
  `src/html/listing/download.rs:61-107`.

Desired simplification:

Use set-based database admission/preparation and small bounded worker pools for
independent network requests while preserving deterministic output order and
security limits.

Acceptance criteria:

- [ ] FAA and backfill reads bind only selected IDs and reuse indexed snapshots.
- [ ] URL/image work has a documented concurrency limit, per-request deadline,
      total-job deadline, and cancellation behavior.
- [ ] DNS pinning, redirect, MIME, citation, byte, and fail-closed policies are
      unchanged.
- [ ] Output ordering is deterministic.

### MNT-015 — Make web controllers lazy and incremental

- Priority: P1
- Status: open
- Owner: unassigned
- Area: frontend architecture
- Backend scope: none
- Depends on: MNT-004, MNT-009, WEB-002, WEB-008

Evidence:

- `web/review.js` is about 5,400 lines with more than 40 shared state fields and
  several interleaved workflows.
- Review APIs expose pagination, but the browser drains every page before first
  render. Verification polling re-downloads all run items every two seconds.
- Initial boot loads Aircraft options and automatically fetches first-variant
  detail even though Listings is active (`web/app.js:62-91`, `:289-381`).
- Listing mutations return updated data, but the client discards it and reloads
  Listings, Aircraft options, and detail (`web/app.js:1073-1139`).

Desired simplification:

Use a small controller per panel with owned resource state, abort lifecycle,
and lazy activation. Render the first page immediately, patch mutation results,
and mark inactive resources stale rather than cascading reloads.

Acceptance criteria:

- [ ] Aircraft, Avionics, and Review load only on first activation.
- [ ] Pipeline, product, association, and manual queues retain server pagination.
- [ ] Verification polling fetches aggregate status plus deltas and retries
      transient failure with bounded backoff.
- [ ] Each controller owns one `AbortController`; stale responses cannot commit.
- [ ] Create/update/delete patches visible state from typed mutation results.
- [ ] `review.js` is split by actual workflow boundaries with no shared global
      busy/request flags.

### MNT-016 — Give capture durable background ownership and remove editor duplication

- Priority: P1
- Status: open
- Owner: unassigned
- Area: Chrome extension
- Backend scope: none
- Depends on: MNT-009, MNT-010, WEB-013

Evidence:

- The popup captures, hashes, signs, and transfers complete rendered HTML before
  the background worker owns the job (`chrome-extension/popup.js:258-316` and
  `:967-986`).
- Every progress event performs storage read/filter/sort/write, broadcasts, and
  can trigger multiple full popup renders.
- The popup duplicates the complete web listing editor and capability schema;
  validation has already drifted.

Desired simplification:

Send `{tabId, jobId}` to the worker, which captures/signs/uploads and owns job
state immediately. Keep the popup focused on capture, progress, retry, and a
deep link to the canonical web editor.

Acceptance criteria:

- [ ] Closing the popup immediately after acceptance cannot interrupt capture
      ownership.
- [ ] Progress persistence is debounced or limited to stage/terminal changes.
- [ ] One keyed subscriber patches one upload row rather than rebuilding lists.
- [ ] The duplicate inline editor is removed, or generated from the same schema
      if a documented user need requires it.
- [ ] Stream parsing requires exactly one recognized terminal event and cancels
      on terminal state.
- [ ] Connection reset removes only connection keys; upload history has a
      separate explicit action.

### MNT-017 — Make CLI and documentation derive from current behavior

- Priority: P1
- Status: open
- Owner: unassigned
- Area: tooling/docs
- Backend scope: both
- Depends on: MNT-003

Evidence:

- `src/admin.rs:1209-2452` hand-parses 22 commands and maintains a separate help
  string plus dozens of parser tests.
- `AGENTS.md` references removed `heal-aircraft-models` and
  `fit-depreciation` commands instead of current commands.
- Implemented valuation code remains described by proposal documents as merely
  proposed or references obsolete module paths.

Desired simplification:

Use a typed declarative CLI only if it materially reduces net parser/help/test
LOC. Generate or verify command documentation from that one registry, and mark
proposal lifecycle status explicitly.

Acceptance criteria:

- [ ] Command names, defaults, conflicts, and help have one source.
- [ ] Top-level and per-command help have smoke tests.
- [ ] Repository documentation contains only commands accepted by the binary.
- [ ] Proposals state `proposed`, `experimental`, `implemented`, `superseded`,
      or `declined`, with implementation date/commit where applicable.
- [ ] Adding a CLI dependency is accepted only with a material net LOC and
      complexity reduction.

### MNT-018 — Restore fast feedback and establish a warning budget

- Priority: P1
- Status: open
- Owner: unassigned
- Area: tests/build quality
- Backend scope: both
- Depends on: MNT-005, MNT-007

Evidence:

- About 1,700 Rust tests live inside production compilation units; the library
  test executable is roughly 500 MB.
- One 11-case database contract test repeatedly creates, initializes, corrupts,
  reconnects, and deletes a full database and took about 83 seconds.
- Clippy reports more than 100 library warnings and 35 explicit
  `allow(clippy::too_many_arguments)` attributes. CI has no lint policy.
- CI uses mutable runner/toolchain versions and ambient Node.

Desired simplification:

Reuse canonical initialized database templates within focused domain suites,
separate fast and exhaustive jobs, fix warning categories as architectural
groups, and pin reproducible toolchains. Avoid adding a heavyweight JavaScript
toolchain for an 87-test suite that runs in under a second.

Acceptance criteria:

- [ ] Full-schema setup is amortized per suite while each test mutates an
      isolated copy/transaction.
- [ ] Most contract matrix cases test the matcher directly, retaining at least
      one end-to-end case per backend.
- [ ] PR fast feedback has a measured target and exhaustive database jobs run in
      parallel as required checks.
- [ ] `cargo clippy --locked --all-targets --all-features -- -D warnings` passes;
      justified exceptions use narrowly documented expectations.
- [ ] Rust, Node, and CI runner versions plus one canonical verification command
      are documented.

### MNT-019 — Move cold capture bodies off hot relational rows and add workload indexes

- Priority: P2
- Status: open
- Owner: unassigned
- Area: storage/query design
- Backend scope: both
- Depends on: MNT-004

Evidence:

- Stored rendered HTML accounts for most of the current local SQLite database
  size despite fewer than 100 plugin submissions.
- Plugin source/user lookup and listing-first association paths do not have
  indexes aligned with their observed order/predicates.

Desired simplification:

Keep submission identity, hash, status, and bounded summary in hot tables; move
large immutable content to a cold-content table or content-addressed store.
Add indexes only for measured production query shapes.

Acceptance criteria:

- [ ] Capture hash/authentication and replay export semantics are unchanged.
- [ ] Content retention/deletion and backup behavior are explicit for both
      databases.
- [ ] Query plans prove indexes serve source-URL/user/time and listing-first
      association lookups.
- [ ] Before/after hot database size and lookup latency are recorded.

### MNT-020 — Put retained compatibility paths on a lifecycle

- Priority: P2
- Status: open
- Owner: unassigned
- Area: compatibility/configuration
- Backend scope: none
- Depends on: MNT-017

Evidence:

- Gemini configuration intentionally maps six legacy environment variables in
  `src/gemini/config.rs:612-657`, and current documentation still recommends
  some of them.
- Legacy status names and temporary repair branches remain visible in review
  domain/client contracts.

Desired simplification:

Inventory every compatibility path with a consumer, replacement, telemetry or
deployment confirmation, deprecation window, and removal milestone. Do not
preserve compatibility indefinitely by default.

Acceptance criteria:

- [ ] Each retained path has a named consumer/owner and removal condition.
- [ ] Operational configuration migrates before aliases are removed.
- [ ] One release of actionable warnings is used where silent removal could
      disrupt deployment.
- [ ] Removed paths include their tests, docs, and adapters in the same change.

### MNT-021 — Retire avionics products instead of rewriting historical evidence

- Priority: P1
- Status: open
- Owner: unassigned
- Area: avionics catalog lifecycle
- Backend scope: both
- Depends on: MNT-002, MNT-022

Evidence:

- `src/avionics/deletion.rs` is about 1,300 lines. A physical delete takes a
  broad 17-table PostgreSQL lock, loads bound submissions and pending reviews,
  parses historical JSON, and rewrites references.
- A transient deletion-guard table exists around `schema/sqlite.sql:1200`.
- Products already have lifecycle states (`unreviewed`, `approved`, and
  `rejected`), so permanent removal is not required for normal catalog use.

Desired simplification:

Add an explicit `retired` state and audit metadata. Exclude retired products
from active choices while preserving immutable observations, historical links,
dispositions, and checkpoints. Keep physical purge as an offline operation for
objects proven unreferenced.

Acceptance criteria:

- [ ] Retiring a product is a narrow transaction with actor, reason, and time.
- [ ] Historical review/replay records remain readable and are not rewritten.
- [ ] Pending suggestions referencing a retired product render it as unavailable
      and require a new decision.
- [ ] Active uniqueness and search semantics for retired identities are
      explicitly defined and identical across backends.
- [ ] Broad online delete locks, JSON repair, and the transient delete guard are
      removed.

### MNT-022 — Represent catalog-mutation authorization once

- Priority: P1
- Status: open
- Owner: unassigned
- Area: catalog integrity
- Backend scope: both
- Depends on: MNT-003, MNT-007, MNT-010

Evidence:

- Human, grounded, and stable-identifier consolidation each maintain separate
  authorization, member, guard, and claim structures around
  `schema/sqlite.sql:1634-2424` before being unioned into an effective view.
- Deletion must query several of these protocols merely to determine whether a
  product participates in a consolidation.

Desired simplification:

Use one authorization header carrying operation kind, actor/evidence, expected
catalog revision, and state; one member table carrying entity, role, and row
fingerprint; and one active claim mechanism. Keep evidence-kind-specific facts
separate where their policies genuinely differ.

Acceptance criteria:

- [ ] A side-by-side projection proves the unified valid entity-pair set exactly
      matches existing authorization protocols on both databases.
- [ ] Mutation triggers consult one active authorization/claim representation.
- [ ] Human and automatic evidence policy remains distinguishable and auditable.
- [ ] Redundant guard/member/claim tables and their reconciliation views are
      removed after parity observation.

### MNT-023 — Use one typed extraction checkpoint and capture identity

- Priority: P1
- Status: open
- Owner: unassigned
- Area: plugin/listing/replay contracts
- Backend scope: both
- Depends on: MNT-009, MNT-010, MNT-012

Evidence:

- `CURRENT_CHECKPOINT_FIELDS`, serialization mutation, manual allow-list
  validation, and `ParsedListing` deserialization independently define the
  extraction checkpoint in `src/plugin.rs:48` and `:1344-1410`.
- Capture/submission/extraction identity is copied through
  `PluginReplayCaptureAttestation`, `SignedSourceListingBinding`,
  `GroundedCapabilityReplayScope`, `ExactListingSourceCaptureScope`, and
  `StoredGroundedCapabilityScope` in `src/plugin.rs` and `src/listings.rs`.
- Large payload equality guards compare rendered HTML and repeated JSON/text
  fields instead of a compact immutable identity/revision.

Desired simplification:

Define a versioned `ListingExtractionCheckpoint` with typed listing and visual
recovery data plus `deny_unknown_fields`. Pass a compact
`CaptureCheckpointId { submission_id, rendered_html_sha256, extraction_sha256
}` through domain services and load the full attestation only at trust
boundaries.

Acceptance criteria:

- [ ] One versioned type owns checkpoint validation, canonical serialization,
      and hashing.
- [ ] V1 decode is isolated at the persistence boundary and upgrades to the
      current in-memory type.
- [ ] Old/new decoding and hashes are compared before stored data is rewritten.
- [ ] Signature verification remains bound to the complete authenticated
      capture.
- [ ] Online CAS uses IDs/revisions rather than repeated full payload equality.

### MNT-024 — Separate curation diagnostics from executable commands

- Priority: P2
- Status: open
- Owner: unassigned
- Area: aircraft curation
- Backend scope: both
- Depends on: MNT-009, MNT-011

Evidence:

- `AircraftHierarchyCurationCaseReport` around
  `src/aircraft/curation/workflow.rs:548` combines counters, raw interactions,
  FAA/catalog evidence, validation errors, reviewable commands, and approved
  reuse commands.
- The persistence application consumes this report and then re-proves agreement
  among its repeated fields in `src/aircraft/curation/application.rs:117-363`.

Desired simplification:

Return diagnostics separately from typed prepared outcomes such as `Blocked`,
`ReuseApproved`, and `CreateApproved`. Let persistence accept only a prepared
outcome and revalidate current database revisions/observation identity.

Acceptance criteria:

- [ ] Apply code does not consume presentation/report DTOs.
- [ ] Report totals and UI output are projections over prepared outcomes.
- [ ] Existing report format remains temporarily available only if an identified
      external consumer needs it.
- [ ] Repeated trace/grounding consistency checks disappear from the command
      path without weakening commit-time revision checks.

### MNT-025 — Derive verification run state instead of synchronizing copies

- Priority: P2
- Status: open
- Owner: unassigned
- Area: verification run state
- Backend scope: both
- Depends on: MNT-009, MNT-011

Evidence:

- Verification stage/outcome/reason values are strings remapped in several Rust
  functions and again in `web/review/automation.mjs`.
- Parent run status is stored separately even though `get_verification_run`
  aggregates item state; `refresh_run_status_sql` repeatedly rescans children to
  synchronize the parent.
- Completed items store a detailed `outcome_json` plus derived status and reason
  copies.

Desired simplification:

Use typed Rust states/reason codes and one terminal item result. Keep child item
status as queue state, persist only explicit parent intent such as
`cancel_requested_at`, and derive aggregate run state and summaries.

Acceptance criteria:

- [ ] Rust enums own stage, terminal outcome, and reason code.
- [ ] Browser protocol constants/projections are generated or fixture-verified
      from the same specification.
- [ ] Parent run state cannot disagree with its items because it is derived.
- [ ] Duplicate outcome reason fields and synchronization rescans are removed.

## Web usability and visual audit

### WEB-001 — Replace the broken mobile navigation

- Priority: P0
- Status: open
- Owner: unassigned
- Area: responsive navigation
- Backend scope: none
- Depends on: WEB-002

Evidence:

- At `max-width: 760px`, `web/app.css:2920-2940` places all six labels in six
  equal grid columns. At 390 px, “ListingsReview” and
  “AvionicsComparisonsRentals” visibly overlap.
- The sticky block includes both brand and navigation, consuming substantial
  vertical space before page content.
- The automatic-review page was measured at about 1,347 px wide in a 390 px
  viewport because its pipeline table retains a 1,320 px minimum width.

Desired experience:

Use a compact mobile header plus an accessible menu, or a four-item primary
navigation with secondary operator/unfinished areas elsewhere. Never shrink
full text labels until they collide.

Acceptance criteria:

- [ ] No label overlap or document-level horizontal scroll at 320, 390, 760,
      1024, and 1440 px.
- [ ] The active destination is announced with `aria-current="page"`.
- [ ] Menu open/close, focus return, Escape, and outside-click behavior work by
      keyboard and pointer.
- [ ] A user can reach every available page at 200% zoom.

### WEB-002 — Organize navigation around user tasks and make it addressable

- Priority: P1
- Status: open
- Owner: unassigned
- Area: information architecture/navigation
- Backend scope: none
- Depends on: none

Evidence:

- Primary navigation mixes market tasks (`Listings`, `Aircraft`), operator
  workflows (`Review`, `Avionics`), and two placeholder destinations
  (`Comparisons`, `Rentals`) in `web/index.html:15-25` and `:608-620`.
- `activatePanel` at `web/app.js:247` only toggles classes; most panels cannot be
  bookmarked and browser Back does not navigate between them. Review detail has
  a separate query-string history implementation.
- `Aircraft` is actually a valuation/model exploration view, which is not clear
  from its label.

Desired experience:

Separate everyday market analysis from catalog/review operations. Use actual
links and stable URLs such as `/listings`, `/values`, `/review`, and `/catalog`,
or equivalent hash routes in the current single page. Hide unfinished features
or label them honestly as unavailable rather than presenting dead navigation.

Acceptance criteria:

- [ ] Each functional destination and selected record has a reload-safe URL.
- [ ] Browser Back/Forward restores panel, filters, page, and selected detail.
- [ ] Primary versus operator navigation is clear without relying on color.
- [ ] Placeholder pages are removed from primary navigation until they offer a
      useful task or explicit roadmap action.
- [ ] Page titles and headings match the task, including a clearer name for the
      aircraft valuation view.

### WEB-003 — Turn Listings into progressive discovery, not a filter wall

- Priority: P1
- Status: open
- Owner: unassigned
- Area: listings UX
- Backend scope: none
- Depends on: MNT-004, MNT-015, WEB-008, WEB-009

Evidence:

- The desktop toolbar uses a nine-column grid for search, five selects, two
  paired ranges, and icon actions (`web/app.css:246-257`). Labels are mostly
  placeholders/accessible-only text.
- On an empty database, three zero metric cards and a minimum 420 px empty table
  dominate the page (`web/app.css:417-426`); the only creation affordance is an
  unlabeled icon.
- Filtering rescans and rebuilds every client-loaded row on both `input` and
  `change` (`web/app.js:205-225`, `:494-542`).
- Median ask uses the upper-middle value for even result counts at
  `web/app.js:732-740`.

Desired experience:

Lead with search, status, and a labeled `Add listing` action. Put less-used
filters in a disclosed filter panel with applied-filter chips and result count.
Use a compact, useful zero state with capture/import instructions.

Acceptance criteria:

- [ ] Page one and aggregate metrics render without loading the full result set.
- [ ] Search and primary filters fit without truncation at common desktop widths.
- [ ] Advanced filters have visible labels, reset affordances, and URL state.
- [ ] Empty data offers `Add listing` and browser-extension capture guidance;
      empty filtered results offer `Clear filters`.
- [ ] Even/odd median calculations have unit tests, or median is supplied as a
      server aggregate.
- [ ] Verified rows retain a usable `View details` action instead of only
      disabled edit/delete buttons.

### WEB-004 — Reframe Review as one prioritized work queue

- Priority: P1
- Status: open
- Owner: unassigned
- Area: operator/review UX
- Backend scope: none
- Depends on: MNT-002, MNT-011, MNT-015, WEB-008

Evidence:

- One page switches between `Automatic acceptance`, `OEM source automation`,
  and `Manual review`, while also exposing provider plans, catalog structure,
  source recovery, run status, and occurrence-level decisions.
- The HTML and `web/review.js` expose internal implementation terminology such
  as retained occurrences, restaging/rebuilding, source recovery, collision
  blockers, and Gemini route planning before the operator's next action is
  clear.
- Product, pipeline, and listing queues download every server page before first
  render.

Desired experience:

Start with outcome-oriented buckets: `Ready for automatic checks`, `Needs a
source`, `Needs an aircraft correction`, and `Needs a human product decision`.
For one selected item, show evidence, recommended action, alternatives, and
consequences in that order. Put internal diagnostics behind disclosure.

Acceptance criteria:

- [ ] First useful queue content renders after page one.
- [ ] Every queue row states why it is blocked and its single recommended next
      action in plain language.
- [ ] Evidence and decision controls remain visible together without long-page
      hunting at desktop widths.
- [ ] Run progress uses summary plus deltas, has a reconnecting state, and does
      not stop permanently after one transient failure.
- [ ] Restage/rebuild/repair actions are absent from routine navigation and live
      in explicit maintenance controls with consequences explained.
- [ ] A short usability test can be completed without explaining internal data
      structures to the participant.

### WEB-005 — Make listing editing guided, grouped, and field-specific

- Priority: P1
- Status: open
- Owner: unassigned
- Area: forms
- Backend scope: none
- Depends on: MNT-009, MNT-010, WEB-011

Evidence:

- The modal presents twelve fields plus a repeatable avionics editor in one
  dense form (`web/index.html:624-713`). Required values and admission
  constraints are not explained before submission.
- The primary icon is titled `New aircraft` even though it creates a sale
  listing. The form asks users to type identity before using the existing URL
  preview/import capability, and registration appears optional even though FAA
  admission requires it.
- Errors are rendered as one message at the bottom; fields do not receive
  structured server errors, `aria-invalid`, or associated remediation text.
- Native confirmation dialogs are used for destructive actions.
- The web and extension editors duplicate fields and disagree on validation.

Desired experience:

Start with `Import from URL`, `Captured by extension`, or `Enter manually`, then
perform N-number/FAA lookup before the long form. Populate admitted identity and
make corrections an explicit exception. Group `Aircraft identity`, `Listing`,
`Time/hours`, and optional `Equipment`. Explain verified-record immutability
before save. Show inline errors and a top error summary that moves focus to the
first invalid field.

Acceptance criteria:

- [ ] Visible `fieldset`/`legend` groups describe the form structure.
- [ ] The primary action is consistently named `Add listing`; a source or
      N-number-first flow exposes admission failure before detailed entry.
- [ ] The UI uses the existing URL preview/import capability and allows review
      of extracted values before save.
- [ ] Required/optional state, units, examples, and constraints are visible.
- [ ] Structured server field errors set `aria-invalid` and `aria-describedby`;
      the first invalid field receives focus.
- [ ] Save progress prevents duplicate submission without replacing button text
      needed by assistive technology.
- [ ] Closing a dialog restores focus to its trigger; unsaved edits prompt
      before dismissal.
- [ ] Destructive confirmation identifies the exact listing and does not depend
      on a browser-native dialog.

### WEB-006 — Make aircraft values deliberate and explainable

- Priority: P1
- Status: open
- Owner: unassigned
- Area: aircraft valuation UX
- Backend scope: none
- Depends on: MNT-013, MNT-015, WEB-008, WEB-011

Evidence:

- The hidden Aircraft panel loads on initial boot and automatically selects and
  evaluates the first variant.
- Three controls have accessible names but only placeholder-like visible option
  labels (`web/index.html:483-499`).
- The global valuation warning appears in the top bar on every destination and
  communicates detail only through a hover title.
- The SVG chart has a broad `aria-label`, while essential calibration,
  confidence, and fallback meaning is distributed between status and table.
- With no aircraft options, the initial `Loading aircraft...` message can remain
  after the request settles.

Desired experience:

Load this view only when requested. Ask the user to select make, model, and
variant deliberately, then present estimate range, calibration status, sample
size, major factors, chart, and comparable listings as one explanation.

Acceptance criteria:

- [ ] No aircraft options/detail request runs before first activation.
- [ ] Controls have persistent visible labels and dependent loading/empty states.
- [ ] Valuation-unavailable/fallback warnings are contextual banners with an
      explanation and recovery action, not global red badges with tooltip-only
      detail.
- [ ] Chart meaning is available as adjacent text/table and not encoded only by
      color or geometry.
- [ ] Loading a new variant cannot display stale data from the previous request.

### WEB-007 — Clarify catalog scope, metrics, and actions

- Priority: P1
- Status: open
- Owner: unassigned
- Area: avionics catalog UX
- Backend scope: none
- Depends on: MNT-004, MNT-015, WEB-008, WEB-009

Evidence:

- Metrics mix global-sounding labels with page-only values (`Complete on page`,
  `Observed in listings on page`) in `web/index.html:541-555`.
- Catalog detail/delete behavior is hidden behind an inspect action and a large
  modal; deletion triggers broad reloads.
- Filter metadata and catalog data load serially, and one failure can overwrite
  another resource's message.

Desired experience:

Present one result summary with explicit scope, searchable identity columns,
clear completeness/status definitions, and a stable detail route or side panel.
Separate destructive catalog maintenance from normal inspection.

Acceptance criteria:

- [ ] Counts state whether they describe all matches or only the current page.
- [ ] Search, filter, order, count, and pagination are server-backed and URL
      addressable.
- [ ] The first result page can render if filter metadata fails, with a separate
      warning.
- [ ] Detail has a reload-safe URL and preserves search/page on close.
- [ ] Destructive actions state downstream usage and require appropriate
      authorization/confirmation.

### WEB-008 — Model asynchronous resource state explicitly

- Priority: P1
- Status: open
- Owner: unassigned
- Area: loading/error/empty/stale feedback
- Backend scope: none
- Depends on: MNT-009, MNT-015

Evidence:

- Listings can retain old rows after a failed refresh without identifying them
  as stale.
- Avionics filter-option and catalog messages overwrite each other; catalog
  failure clears rows but also hides the empty state.
- Review queues retain previous content after failures, while one verification
  polling error permanently stops polling.
- The three review modes have separate request counters but share metrics and
  message regions. A late product response was reproduced overwriting Manual
  review with a product-preparation message.

Desired experience:

Each resource owns `idle | loading | ready-empty | ready-data | stale-data |
error`, last-updated time, and retry behavior. Preserve useful stale data but
label it. Use skeletons only where they communicate layout; use compact progress
for background work.

Acceptance criteria:

- [ ] Empty, error, loading, and stale states are visually and semantically
      distinct for every page and dialog.
- [ ] Recoverable errors include one clear retry action.
- [ ] Rapid mode changes cancel old work and cannot commit messages, metrics, or
      disabled state into the newly active mode.
- [ ] Background refresh does not erase usable content.
- [ ] Live regions announce concise state changes once without repeated full
      table narration.
- [ ] Busy buttons and panels retain accessible names and expose `aria-busy`.

### WEB-009 — Use priority views on small screens instead of leaking wide tables

- Priority: P1
- Status: open
- Owner: unassigned
- Area: responsive data presentation
- Backend scope: none
- Depends on: WEB-001, WEB-003, WEB-004, WEB-007

Evidence:

- Base tables have `min-width: 980px` and listing shells have a fixed 420 px
  minimum height (`web/app.css:417-426`).
- Product and pipeline review tables retain minimum widths of about 760 and
  1,320 px. At mobile width, their shared shell changes to `overflow: visible`,
  but only the manual-review table receives card conversion.

Desired experience:

Define essential columns per workflow. At smaller widths, use compact cards or
row summaries with a detail drill-in. Preserve real tables where comparison
across columns is the task; contain horizontal scrolling when unavoidable.

Acceptance criteria:

- [ ] No page-level horizontal overflow at supported viewport/zoom sizes.
- [ ] Listings, pipeline, product review, manual review, aircraft values, and
      avionics each have an intentional small-screen presentation.
- [ ] Hidden table headers are represented by visible `data-label` content in
      card layouts.
- [ ] Empty states size to their content rather than reserving a large blank
      table canvas.

### WEB-010 — Establish a small visual system and reduce density

- Priority: P1
- Status: open
- Owner: unassigned
- Area: visual design
- Backend scope: none
- Depends on: WEB-002, WEB-003, WEB-004

Evidence:

- `web/app.css` is about 3,250 lines with about 62 distinct literal colors,
  several one-off surface shades, and many feature-specific combinations.
- There are about 50 declarations at 10 or 11 px, making important evidence,
  status, and helper text hard to scan.
- Most surfaces use the same border, radius, white background, and shadow, so
  nested panels communicate containment more than hierarchy.
- The desktop zero-data screen devotes most of the viewport to an empty white
  table while primary actions remain icon-only.

Desired experience:

Keep the restrained aviation/teal character, but define semantic tokens for
type, spacing, surfaces, borders, focus, and status. Use whitespace and type
weight for hierarchy; reserve shadows and tinted surfaces for meaningful
elevation/status. Favor labels plus icons for primary actions.

Acceptance criteria:

- [ ] CSS tokens cover the approved neutral, brand, success, warning, danger,
      and information roles in default/hover/focus/disabled states.
- [ ] A documented type scale has a readable body/helper minimum and tabular
      number treatment.
- [ ] A small spacing/radius/elevation scale replaces one-off values.
- [ ] Listings, review, aircraft, avionics, dialogs, and extension use the same
      semantic status language while retaining layout appropriate to each.
- [ ] Primary actions are text-labeled; icon-only actions remain secondary and
      have labels/tooltips.
- [ ] Visual regression captures cover empty, populated, loading, error,
      dialog, and mobile states.

### WEB-011 — Complete keyboard, focus, form, and status accessibility

- Priority: P1
- Status: open
- Owner: unassigned
- Area: accessibility
- Backend scope: none
- Depends on: WEB-001, WEB-005, WEB-008, WEB-009, WEB-010

Evidence:

- Primary navigation is implemented as buttons with only a visual active class;
  it has no `aria-current`, tab semantics, or link destination.
- Main form controls remove the native outline and use `:focus` styling, while
  buttons have inconsistent explicit focus treatment. Web CSS has no reduced
  motion or forced-colors accommodation.
- There is no skip link; primary listing and aircraft-value tables have no
  captions/scoped headers; dynamic avionics inputs rely on placeholders instead
  of persistent labels.
- The listing avionics `fieldset` uses a styled `div` heading rather than a
  semantic `legend`.
- Status/error messages are mostly broad live regions rather than field-linked
  errors; disabled row actions cannot explain themselves to keyboard users.
- Some important dynamically created review buttons use `button-primary`, while
  CSS defines `.button.primary`, so their intended primary treatment is absent.

Desired experience:

Use native links, buttons, form groups, dialogs, progress, and tables wherever
their semantics match. Add custom ARIA only to express state native HTML cannot.
Use consistent visible focus, target sizing, contrast, and focus restoration.

Acceptance criteria:

- [ ] Every task is operable in a logical keyboard order with an always-visible
      focus indicator.
- [ ] Active navigation, tabs, dialogs, menus, progress, errors, and busy state
      are announced correctly.
- [ ] Form groups use `fieldset`/`legend`; errors are linked to their fields.
- [ ] Disabled actions are omitted or paired with a focusable explanation.
- [ ] Status is never conveyed by color alone, and normal/large text meets WCAG
      AA contrast.
- [ ] Reduced-motion and forced-colors behavior is explicitly tested.
- [ ] Automated checks are supplemented by keyboard and screen-reader smoke
      testing for critical flows.

### WEB-012 — Use plain, stable content instead of internal terminology

- Priority: P2
- Status: open
- Owner: unassigned
- Area: content design
- Backend scope: none
- Depends on: MNT-009, WEB-004, WEB-008

Evidence:

- The review UI alternates among checks, acceptance, verification, validation,
  review, maintenance, recovery, retained source, occurrence, and restaging for
  closely related tasks.
- Long button labels such as “Automatically apply to eligible unique
  occurrences” describe implementation rather than the immediate outcome.
- Error prose is sometimes part of client control flow, which prevents safe
  content improvements.

Desired experience:

Create a short product glossary and action-writing rules: verb plus object for
buttons, outcome plus recovery for messages, details on demand for technical
provenance. Keep terms stable across server, web, and extension.

Acceptance criteria:

- [ ] One preferred user-facing term exists for each workflow concept.
- [ ] Buttons say what will happen; nearby text explains irreversible or
      external effects.
- [ ] Queue reasons use plain summaries with technical evidence disclosed below.
- [ ] Copy changes cannot affect program control flow.

### WEB-013 — Keep the extension capture-first

- Priority: P1
- Status: open
- Owner: unassigned
- Area: extension usability/visual design
- Backend scope: none
- Depends on: MNT-016, WEB-005, WEB-008, WEB-010, WEB-011

Evidence:

- A 520 px popup contains setup, current-page status, pipeline diagnostics,
  recent uploads, settings, recovery, and a complete listing editor.
- The editor is the largest and most maintenance-sensitive workflow, while the
  popup's unique value is quick capture from the current tab.
- Progress updates cause repeated storage and full-list rendering work.

Desired experience:

Show current-page eligibility, one dominant `Add to AirCost` action, durable
progress, retry, and recent results. Open the main app for detailed editing and
review. Keep technical events collapsed unless troubleshooting.

Acceptance criteria:

- [ ] Initial capture can be understood and started without scrolling.
- [ ] Accepted background ownership is visible before the popup can close.
- [ ] Reopened popup restores authoritative job state without duplicate event
      rendering.
- [ ] Completed jobs link to the canonical listing detail/editor.
- [ ] Setup/reset language distinguishes connection settings from upload history.

### WEB-014 — Replace source-regex UI tests with behavioral coverage

- Priority: P1
- Status: open
- Owner: unassigned
- Area: frontend/extension testing
- Backend scope: none
- Depends on: MNT-015, MNT-016, WEB-001, WEB-008, WEB-011

Evidence:

- `web/review/ui.test.mjs` largely asserts strings and regexes in source/CSS.
- There are no behavioral tests for `web/app.js`; avionics and popup behavior
  have only narrow tests despite substantial state/race logic.

Desired simplification:

Keep a small number of markup contract tests, extract controller/domain
functions that can run against a lightweight DOM harness, and test observable
behavior rather than implementation spelling. Avoid introducing a large
frontend framework solely for testing.

Acceptance criteria:

- [ ] Tests cover navigation/history, first-page rendering, pagination, stale
      request cancellation, save/delete patching, and transient retry.
- [ ] Review tests cover mode switching, unsaved decisions, completion, and
      overlapping run updates.
- [ ] Extension tests cover popup closure, large capture handoff, event
      coalescing, reconnect/resume, and strict terminal-event parsing.
- [ ] Accessibility tests cover focus movement, dialog return, labels, live
      regions, and mobile menu state.
- [ ] Visual captures cover representative widths/states without becoming the
      only assertion mechanism.

## Historical staged execution sketch (superseded)

Do not schedule work from the phase list below. It records the first audit
grouping and does not express implementation prerequisites or shared-file
serialization. The authoritative branch order, split units, and conflict
barriers are in `docs/improvement-execution-plan.md`.

### Phase 1 — Correctness and reliable feedback

1. MNT-001 atomic persistence.
2. MNT-005 complete CI inventory.
3. MNT-003 ordered startup/migration path.
4. MNT-018 reusable fixtures and warning reduction.
5. WEB-001 mobile navigation hotfix and WEB-003 median correction.

### Phase 2 — Bounded reads and explicit contracts

1. MNT-009 typed commands/pages/errors.
2. MNT-004 listing/catalog pagination and targeted cleanup.
3. MNT-007 shared dual-backend execution primitives.
4. MNT-015 lazy controllers, aborts, and mutation patching.
5. WEB-008 explicit resource state.

### Phase 3 — Remove duplicate domain authority

1. MNT-002 normalized review aspects and transition engine.
2. MNT-011 reusable verification plans/revisions.
3. MNT-006 canonical aircraft identity cutover.
4. MNT-012 shared ingestion/replay lifecycle.
5. MNT-010 generated vocabularies/contracts.
6. MNT-021 product retirement and MNT-022 unified mutation authorization.
7. MNT-023 typed checkpoint/capture identity and MNT-025 derived run state.

### Phase 4 — Product and interface redesign

1. WEB-002 information architecture and routes.
2. WEB-003 through WEB-009 task-flow redesigns.
3. WEB-010 through WEB-013 visual, accessibility, language, and extension work.
4. WEB-014 behavioral and visual regression coverage.

### Phase 5 — Computational and structural cleanup

1. MNT-013 sparse/set-based valuation.
2. MNT-014 bounded bulk/network work.
3. MNT-019 cold storage/index tuning.
4. MNT-008 listing module extraction after its domain seams are stable.
5. MNT-024 curation diagnostic/command separation.
6. MNT-017 and MNT-020 tooling/document/compatibility cleanup.

## Completion evidence template

Copy this under a completed item:

```text
Completion evidence:
- Owner:
- PR/commit:
- Completed:
- Before:
- After:
- SQLite verification:
- PostgreSQL verification:
- UI/accessibility verification:
- Follow-up items:
```

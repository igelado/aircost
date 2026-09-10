# Improvement Execution Plan

This plan turns the findings in `docs/maintainability.md` into independently
mergeable work while keeping SQLite and PostgreSQL first-class. It is the
coordination contract for the improvement program: one implementation unit per
branch and pull request, fresh branches from the latest merged `main`, and
exclusive ownership of known conflict hotspots.

## Preconditions

Before implementation starts:

1. Land `AGENTS.md`, `docs/maintainability.md`, and this plan together on the
   administrative branch `codex/improvement-register-and-plan`.
2. Start feature work only from a clean `main` containing that merge.
3. Do not reuse the stale, prunable `/tmp/aircost-*` worktree registrations.
   Inspect them before pruning; do not delete branches as part of cleanup.
4. Give every active agent an isolated checkout. A configured `coder` follows
   its private-clone workflow. A runtime that shares one clone instead uses a
   separate worktree under a writable, configurable root outside the repository
   (for example `../aircost-worktrees/`). Never hard-code a contributor's home
   directory.
5. Create a dependent branch only after all prerequisites have merged. Do not
   stack sibling branches on other unmerged branches.

The root agent is the integration owner. It keeps the main worktree clean,
reviews file fences, rebases, merges, verifies CI, and deletes merged branches.
It does not implement a feature while acting as integrator.

## Branch and merge rules

- A branch and PR carry exactly one register ID or registered child ID.
- Large register entries are split below because their acceptance criteria are
  not safe as one review or one deployment. A child branch may not include work
  from another parent ID.
- Writable implementation uses the repository-configured `coder` or
  `architect` agents. Branches use `codex/<id>-<outcome>`, as required by the
  configured writer workflow, and are cut from the latest merged `origin/main`.
- Agents do not edit `docs/maintainability.md`. Completion evidence is kept in
  the PR while work is parallel; the integrator updates the register in one
  serialized administrative change after each merge wave.
- One branch at a time owns each conflict hotspot listed below. A branch that
  needs a hotspot waits even if its logical dependencies are otherwise ready.
- Every schema change has paired SQLite and PostgreSQL migrations, canonical
  schema updates, and parity/upgrade tests. Supporting two databases is not an
  optional simplification target.
- No compatibility shell, forwarding module, duplicated implementation, or
  speculative abstraction may be introduced solely to make a branch smaller.

Before a PR is eligible to merge, the integrator verifies:

```text
git status --short
git fetch origin
git rebase origin/main
git log --oneline origin/main..HEAD
git diff --name-status origin/main...HEAD
git diff --check origin/main...HEAD
```

The changed-file list must fit the declared fence. Relevant local tests, both
database suites where applicable, formatting, linting, CI, and automated review
must pass before squash merge. Performance branches record baselines again on
their rebased base rather than comparing against a stale branch.

## Dependency corrections

The initial register describes outcome dependencies and contains cycles. Use
these implementation dependencies instead:

- `MNT-015A` establishes controller/resource ownership without depending on
  `WEB-008`; `WEB-008` presents the resource states; `MNT-015B` then completes
  incremental loading and mutation patching.
- `MNT-016A` gives the background worker durable job ownership. `WEB-013`
  redesigns the popup against that protocol. `MNT-016B` then removes the
  temporary editor and protocol compatibility code.
- `WEB-009A` introduces only reusable responsive-view primitives.
  `WEB-003`, `WEB-004`, and `WEB-007` define page content priorities, and
  `WEB-009B` completes the cross-page responsive migration and audit.
- `WEB-005` owns the listing editor's field semantics, inline errors, dirty
  protection, and focus restoration. It no longer depends on `WEB-011`;
  `WEB-011` is the later application-wide accessibility audit.
- `WEB-005` no longer depends on `WEB-011`, and `WEB-003`/`WEB-007` no longer
  depend on completed `WEB-009`. These are direction corrections, not omitted
  acceptance criteria.
- `MNT-019` additionally waits for `MNT-003`, `MNT-008D`, `MNT-022`, and
  `MNT-023` so cold storage targets the final migration, listing, authorization,
  and capture-identity models.

## Agent ownership

The named explorers are session planning/review specialists and are not
writable repository agents. Each implementation unit is written by a fresh
instance of the repository-configured `coder` agent after its specification is
settled. For a multi-module unit whose design still needs decomposition, the
configured `architect` agent first refines the specification and delegates the
same single-ID implementation to `coder`. The root agent remains integrator.

| Review specialist | Primary lane | Exclusive responsibility while active |
|---|---|---|
| Lovelace (`explorer_backend`) | database, API contracts, listing/capture architecture | review database executor, listing writes/contracts/extractions, capture lifecycle, verification run state |
| Leibniz (`explorer_avionics`) | avionics/review integrity | review shared vocabularies, review aspects, verification plans, catalog authorization/retirement, checkpoints |
| Rawls (`explorer_performance`) | performance and valuation | review atomic valuation, bounded reads/bulk work, sparse fitting, cold storage |
| Parfit (`explorer_tests`) | CI, tooling, compatibility, integration-quality work | review test discovery, migrations, fixtures/toolchains, CLI/docs, compatibility lifecycle, UI test harness |
| Boole (`explorer_web_ux`) | frontend and extension interaction | review routing, controller lifecycle, task flows, capture popup behavior |
| Gauss (`explorer_visual`) | responsive/visual/accessibility/content layer | review mobile navigation, responsive primitives/audit, visual system, accessibility, language |

Review specialists may review across lanes, but only the assigned writable
`coder`/`architect` agent changes an active branch. Ownership moves only by an
explicit plan amendment.

## Implementation units

### Foundation, database, tests, and tooling

| Unit | Owner / branch | Must be merged first | File fence / integration note |
|---|---|---|---|
| MNT-005 | `coder` (Parfit review) — `codex/mnt-005-ci-test-inventory` | register branch | `.github/workflows/test.yml`, schema-test runner/registry, PG/DNN test selection; no migration redesign |
| MNT-003 | `architect` -> `coder` (Parfit review) — `codex/mnt-003-versioned-migrations` | MNT-005 | `src/db.rs`, DB portions of `src/admin.rs`, `schema/**`, `migrations/**`, DB contract tests, `docs/database.md` |
| MNT-018A | `coder` (Parfit review) — `codex/mnt-018a-pin-toolchains` | MNT-005 | CI toolchain/runner pins, `rust-toolchain.toml`, one Node version file, verification docs |
| MNT-007A | `architect` -> `coder` (Lovelace review) — `codex/mnt-007a-db-executor-core` | MNT-003 | `src/db.rs`, new `src/db/**`; freeze the executor/transaction API after merge |
| MNT-007B | `coder` (Lovelace review) — `codex/mnt-007b-db-executor-adoption` | MNT-007A | ordinary CRUD dispatch call sites and deletion of local macros; backend-specific SQL remains explicit |
| MNT-018B | `coder` (Parfit review) — `codex/mnt-018b-amortize-db-fixtures` | MNT-003, MNT-007B, MNT-018A | test support/templates and DB contract tests; both backends remain covered |
| MNT-018C | `coder` (Lovelace review) — `codex/mnt-018c-core-warning-budget` | MNT-018B | `src/db/**`, `src/listing/**`, `src/listings.rs`, `src/models.rs`, `src/server.rs`, core module wiring |
| MNT-018D | `coder` (Leibniz review) — `codex/mnt-018d-catalog-warning-budget` | MNT-018B | `src/aircraft/**`, `src/avionics/**`, `src/plugin.rs`, `src/html/**` |
| MNT-018E | `coder` (Rawls review) — `codex/mnt-018e-modeling-warning-budget` | MNT-018B | `src/valuation/**`, `src/gemini/**`, `src/admin.rs`, binaries; no shared Cargo policy |
| MNT-018F | `coder` (Parfit review) — `codex/mnt-018f-enforce-quality-gate` | MNT-018C, MNT-018D, MNT-018E | CI/lint policy and canonical `-D warnings` command only |
| MNT-017 | `architect` -> `coder` (Parfit review) — `codex/mnt-017-cli-doc-registry` | MNT-003, MNT-018F | `src/admin.rs`, command docs, proposal lifecycle headers; add a CLI dependency only for measured net simplification |
| MNT-020A | `coder` (Parfit review) — `codex/mnt-020a-compatibility-inventory` | MNT-017 | inventory, consumers, warnings, telemetry/deployment evidence, removal gates; no premature removal |
| MNT-020B | `coder` (Parfit review) — `codex/mnt-020b-remove-expired-compatibility` | MNT-020A, MNT-002, MNT-015B | remove only paths whose consumer/removal evidence is satisfied, including tests/docs/adapters |

`MNT-018C`, `MNT-018D`, and `MNT-018E` are the only intentionally parallel
Rust-wide cleanup branches. Their file fences are disjoint. Merge them one at a
time with a rebase between merges, then enable the gate in `MNT-018F`.

### Persistence, listing, review, avionics, and valuation

| Unit | Owner / branch | Must be merged first | File fence / integration note |
|---|---|---|---|
| MNT-009A | `architect` -> `coder` (Lovelace review) — `codex/mnt-009a-typed-listing-commands` | MNT-007B, MNT-018F | listing transport commands, structured field errors, `src/models.rs`, listing parser removal, thin server wiring |
| MNT-009B | `coder` (Lovelace review) — `codex/mnt-009b-structured-review-contracts` | MNT-009A | tagged review decision endpoint/result/error protocol; client compatibility only, no review persistence redesign |
| MNT-001A | `architect` -> `coder` (Rawls review) — `codex/mnt-001a-atomic-valuation-writes` | MNT-007B, MNT-018F | `src/valuation/{dataset,store}.rs` and failure injection; no aircraft identity cutover |
| MNT-001B | `architect` -> `coder` (Lovelace review) — `codex/mnt-001b-atomic-listing-writes` | MNT-007B, MNT-009A, MNT-018F | listing mutation/fact/finalization unit of work and failure injection; no module extraction |
| MNT-004 | `architect` -> `coder` (Rawls review) — `codex/mnt-004-bounded-list-reads` | MNT-001A, MNT-001B, MNT-009B | listing/catalog page queries, `src/cleanup.rs`, page DTO/handler wiring, query-count tests |
| MNT-010 | `coder` (Leibniz review) — `codex/mnt-010-shared-domain-vocabularies` | MNT-003, MNT-009B | one versioned vocabulary source, generators/artifacts, parity fixtures; generated files cease manual ownership |
| MNT-006A | `architect` -> `coder` (Lovelace review) — `codex/mnt-006a-canonical-identity-dual-read` | MNT-001A, MNT-001B, MNT-003, MNT-004 | canonical direct writes/reads, paired migration, mismatch instrumentation, documented rollback/export |
| MNT-006B | `architect` -> `coder` (Lovelace review) — `codex/mnt-006b-remove-legacy-identity` | MNT-006A plus recorded zero-mismatch observation on SQLite and PostgreSQL | destructive cutover migration and deletion of projections/placeholders/triggers/legacy columns; never start on synthetic evidence alone |
| MNT-022 | `architect` -> `coder` (Leibniz review) — `codex/mnt-022-catalog-mutation-authorizations` | MNT-003, MNT-006B, MNT-007B, MNT-010 | consolidation/authorization tables, guards, claims, triggers, paired migrations and parity projection |
| MNT-002 | `architect` -> `coder` (Leibniz review) — `codex/mnt-002-review-aspect-source-of-truth` | MNT-001B, MNT-006B; branch after MNT-022 to serialize schema | review persistence/transition engine, pending-review schema/migration, compatibility projection; queue GETs stay read-only |
| MNT-011 | `architect` -> `coder` (Leibniz review) — `codex/mnt-011-revision-bound-verification-plans` | MNT-002, MNT-004, MNT-007B, MNT-010 | listing/avionics/aircraft verification plans and scoped catalog reads; consume review API without reopening persistence |
| MNT-024 | `coder` (Parfit review) — `codex/mnt-024-curation-prepared-outcomes` | MNT-009B, MNT-011 | aircraft curation workflow/application/report separation; keep schema-free if possible |
| MNT-014 | `coder` (Rawls review) — `codex/mnt-014-bounded-bulk-workflows` | MNT-004, MNT-011 | FAA/backfill set operations and bounded citation/image workers; preserve security and deterministic ordering |
| MNT-013 | `architect` -> `coder` (Rawls review) — `codex/mnt-013-set-based-sparse-valuation` | MNT-001A, MNT-004, MNT-011, MNT-014 | `src/valuation/**`, benchmarks, optional sparse dependency; final measurements on rebased main |
| MNT-021 | `architect` -> `coder` (Leibniz review) — `codex/mnt-021-avionics-product-retirement` | MNT-002, MNT-022 | retirement lifecycle and projections; remove online physical-delete repair/locks without rewriting history |
| MNT-025 | `coder` (Lovelace review) — `codex/mnt-025-derived-verification-run-state` | MNT-009B, MNT-010, MNT-011 | listing verification item/result projection and browser protocol fixtures; no replay-run edits |
| MNT-012 | `architect` -> `coder` (Lovelace review) — `codex/mnt-012-capture-lifecycle-engine` | MNT-001B, MNT-009B, MNT-011, MNT-021 | `src/plugin.rs`, listing creation/reuse, replay admission/run lifecycle; exclusive listing/capture ownership |
| MNT-023 | `architect` -> `coder` (Leibniz review) — `codex/mnt-023-typed-capture-checkpoints` | MNT-009B, MNT-010, MNT-012 | checkpoint type/version/hash and compact capture identity across plugin/listing/replay boundaries |
| MNT-008A | `coder` (Lovelace review) — `codex/mnt-008a-listing-model-commands` | MNT-001B, MNT-002, MNT-007B, MNT-009B, MNT-012, MNT-023 | extract model/command behavior from `src/listings.rs`; delete moved code |
| MNT-008B | `coder` (Lovelace review) — `codex/mnt-008b-listing-query-repository` | MNT-008A | extract queries/repository; no forwarding implementation remains |
| MNT-008C | `coder` (Lovelace review) — `codex/mnt-008c-listing-finalization` | MNT-008B | extract finalization into the settled transaction unit of work |
| MNT-008D | `coder` (Lovelace review) — `codex/mnt-008d-remove-listings-module` | MNT-008C | delete `src/listings.rs`, plural module/imports, and temporary migration seams |
| MNT-019 | `architect` -> `coder` (Rawls review) — `codex/mnt-019-cold-capture-storage` | MNT-003, MNT-004, MNT-008D, MNT-022, MNT-023 | paired migrations, cold content loader, replay/export adapters, measured indexes/retention/backup semantics |

`MNT-006B` is an operational gate, not a long-lived branch. Other independent
work continues during the observation period. The branch is created only when
real SQLite and PostgreSQL observations satisfy the cutover condition.

### Web and extension usability

The web lane is intentionally serialized through shared shell files. After
`MNT-015A` creates controller boundaries, page branches must stay inside their
feature modules and assigned page blocks. Task-flow branches own content and
behavior; `WEB-009` owns responsive projection; `WEB-010` owns appearance;
`WEB-011` owns cross-app semantics; `WEB-012` owns wording.

| Unit | Owner / branch | Must be merged first | File fence / integration note |
|---|---|---|---|
| WEB-002 | `coder` (Boole review) — `codex/web-002-task-routing` | register branch | navigation shell, URL state, Back/Forward, titles, placeholder removal; freeze route/panel IDs |
| WEB-001 | `coder` (Gauss review) — `codex/web-001-mobile-navigation` | WEB-002 | compact navigation layout/behavior, Escape/outside click/focus return; no page redesign |
| MNT-015A | `architect` -> `coder` (Boole review) — `codex/mnt-015a-controller-lifecycle-core` | MNT-004, MNT-009B, WEB-002 | lazy activation, owned abort/resource state, controller/module boundaries; no visible redesign |
| WEB-008 | `coder` (Boole review) — `codex/web-008-resource-feedback-states` | MNT-009B, MNT-015A | idle/loading/data/empty/stale/error presentation, retry/last-updated, race-proof message ownership |
| MNT-015B | `architect` -> `coder` (Boole review) — `codex/mnt-015b-incremental-controllers` | MNT-004, MNT-009B, MNT-015A, WEB-008 | first-page rendering, retained pagination, delta polling, result patching, bounded retries, final workflow split |
| WEB-009A | `coder` (Gauss review) — `codex/web-009a-responsive-view-primitives` | WEB-001, MNT-015B | reusable priority/card/table primitives only; no feature content decisions |
| WEB-003 | `coder` (Boole review) — `codex/web-003-progressive-listings` | MNT-004, MNT-015B, WEB-008, WEB-009A | listing browse controller/view and page block; no editor or shell changes |
| WEB-005 | `coder` (Boole review) — `codex/web-005-guided-listing-editor` | MNT-009B, MNT-010, MNT-015A | listing editor/dialog only, structured inline errors, dirty protection/focus; no listing browse redesign |
| WEB-007 | `coder` (Boole review) — `codex/web-007-clear-catalog` | MNT-004, MNT-015B, WEB-008, WEB-009A | catalog controller/view, server-backed scope/filter/page, detail and danger-zone separation |
| MNT-016A | `architect` -> `coder` (Boole review) — `codex/mnt-016a-background-capture` | MNT-009B, MNT-010 | extension background job ownership/protocol, strict terminal parsing, coalesced persistence; minimal popup shim |
| WEB-004 | `coder` (Boole review) — `codex/web-004-prioritized-review-queue` | MNT-002, MNT-011, MNT-015B, MNT-020B, WEB-008 | review modules/page only: task buckets, evidence/decision hierarchy, reconnecting progress |
| WEB-009B | `coder` (Gauss review) — `codex/web-009b-responsive-priority-views` | WEB-001, WEB-003, WEB-004, WEB-007, WEB-009A | migrate/audit page priority views and contained overflow; no fetching, wording, or desktop workflow changes |
| WEB-010 | `coder` (Gauss review) — `codex/web-010-visual-system` | WEB-002, WEB-003, WEB-004, WEB-009B | tokens, type/spacing/density/status appearance and extension styling; no semantics or workflow changes |
| WEB-011 | `coder` (Gauss review) — `codex/web-011-accessibility-completion` | WEB-001, WEB-005, WEB-008, WEB-009B, WEB-010 | keyboard/focus/form/status/table semantics, reduced motion/forced colors, behavioral accessibility tests |
| WEB-006 | `coder` (Boole review) — `codex/web-006-explainable-values` | MNT-013, MNT-015B, WEB-008, WEB-011 | aircraft/value controller/view, outcome summary and responsive explainable chart |
| WEB-013 | `coder` (Boole review) — `codex/web-013-capture-first-extension` | MNT-016A, WEB-005, WEB-008, WEB-010, WEB-011 | popup HTML/CSS/JS only; consume frozen background protocol and deep-link to canonical editor |
| MNT-016B | `coder` (Boole review) — `codex/mnt-016b-remove-popup-editor` | MNT-016A, WEB-013 | delete duplicate editor/schema/legacy messages; separate connection reset from history removal |
| WEB-012 | `coder` (Gauss review) — `codex/web-012-plain-language` | MNT-009B, MNT-016B, WEB-004, WEB-006, WEB-008, WEB-011, WEB-013 | strings and matching accessible names only; update the language guide/contracts |
| WEB-014 | `coder` (Parfit review) — `codex/web-014-behavioral-ui-tests` | MNT-015B, MNT-016B, WEB-001, WEB-008, WEB-011, WEB-012 | final critical-flow behavioral coverage for web and extension; no source-regex assertions |

## Conflict barriers

The integration owner enforces these exclusive queues even when agents could
otherwise start:

1. **Database/schema:** `src/db.rs`, `src/admin.rs` database commands,
   `schema/sqlite.sql`, `schema/postgres.sql`, `migrations/**`, and migration
   registration. Order: MNT-003 -> MNT-007A -> schema-bearing domain branches.
   Every later schema branch rebases before creating its migration pair and
   canonical-schema tail.
2. **Listing/API:** `src/listings.rs`, `src/models.rs`, and listing portions of
   `src/server.rs`. Order: MNT-009 -> MNT-001B -> MNT-004 -> MNT-006 -> MNT-002
   -> MNT-012 -> MNT-023 -> MNT-008A/B/C/D.
3. **Review/verification:** `src/listing/review**`,
   `src/listing/verification.rs`, `src/avionics/verification.rs`, and review
   protocol files. Order: MNT-002 -> MNT-011 -> MNT-025.
4. **Catalog mutation:** avionics consolidation/deletion and guard triggers.
   Order: MNT-022 -> MNT-021.
5. **Capture/replay:** `src/plugin.rs` and replay lifecycle files. Order:
   MNT-012 -> MNT-023 -> MNT-019.
6. **Shared web shell:** `web/index.html`, `web/app.css`, `web/app.js`, and
   static-asset wiring. Run only one Boole/Gauss web branch at a time unless
   both changed-file fences have been proven disjoint after controller
   extraction.
7. **Extension popup:** `chrome-extension/popup.*`. Order: WEB-010 -> WEB-011
   -> WEB-013 -> MNT-016B -> WEB-012. `MNT-016A` owns `background.js` and may
   run beside main-web work.
8. **CI/Cargo:** `.github/workflows/test.yml`, `Cargo.toml`, `Cargo.lock`,
   toolchain/lint files. Parfit schedules these serially; a feature dependency
   addition waits or receives an explicit temporary fence.

Thin integration files such as `src/server.rs`, `src/admin.rs`, `src/lib.rs`,
and shared generated artifacts are changed only in a final wiring commit after
rebasing the branch that needs them.

## Rolling launch schedule

Use dependency readiness rather than keeping speculative branches open. The
initial and major gates are:

1. **Register:** merge `codex/improvement-register-and-plan`.
2. **First launch:** run MNT-005 and WEB-002 in separate isolated checkouts.
3. **Early parallel work:** after MNT-005, run MNT-003; after WEB-002, run
   WEB-001. Then land MNT-018A and MNT-007A/B without overlapping their shared
   CI/database files.
4. **Feedback barrier:** land MNT-018B; run MNT-018C/D/E concurrently in their
   disjoint Rust trees; merge/rebase them serially; land MNT-018F.
5. **Contract/atomicity wave:** run MNT-009A/B and MNT-001A where file fences
   permit; then MNT-001B, MNT-004, and MNT-010. MNT-017/MNT-020A may proceed in
   the tooling lane.
6. **Identity/controller wave:** run MNT-006A and the main-web controller train
   through MNT-015B in different worktrees. Run WEB-009A and MNT-016A after
   their prerequisites. Continue unrelated web/tooling work during the MNT-006
   observation period.
7. **Schema/review train:** after the observation gate, serialize MNT-006B,
   MNT-022, MNT-002, and MNT-011.
8. **Domain expansion wave:** after MNT-011, run disjoint MNT-014, MNT-024,
   MNT-025, MNT-020B, and eligible web work in parallel. Then land MNT-013,
   MNT-021, MNT-012, and MNT-023 subject to their hotspot queues.
9. **Removal/storage train:** serialize MNT-008A/B/C/D, then MNT-019.
10. **Web finish:** serialize remaining task flows, WEB-009B, WEB-010,
    WEB-011, WEB-006, WEB-013, MNT-016B, and WEB-012. Land WEB-014 last.

Six implementation agents are the capacity ceiling, not a utilization target.
A slot stays idle when filling it would create an unmergeable branch or violate
a hotspot barrier. As soon as an eligible branch merges, the next writable
agent creates a fresh private clone or worktree from current `main` for the next
ready unit.

## Completion accounting

A parent register item is complete only when all of its child units are merged
and every original acceptance criterion has evidence. The integration owner
records:

- PR and squash commit;
- tests and database backends exercised;
- before/after performance or usability evidence where required;
- residual compatibility or operational gates;
- follow-up IDs, if acceptance was deliberately deferred.

No parent is marked complete merely because its first child branch landed.

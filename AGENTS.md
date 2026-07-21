# Coding Agent Guide

This project models aircraft ownership economics and asking-market values. The
current web application is Rust-first. Older Python scripts remain in the repo
as reference utilities, but new app behavior should be implemented in Rust.

## Documentation Map

- `README.md`: project overview and common commands.
- `docs/depreciation_model.md`: value model, parameters, fitting, and graph
  behavior.
- `docs/database.md`: schema ownership and listing/plugin write lifecycle.
- `docs/llm_usage.md`: Gemini extraction, grounding, validation, and correction
  rules.
- `docs/webapp.md`: web app and extension API notes.

Read the relevant docs before changing valuation, schema, ingestion, or LLM
prompts.

## Branch And PR Workflow

Create feature branches named:

```text
<agent-name>/<functionality>
```

Examples:

```text
codex/variant-normalization
codex/aircraft-docs
```

When work is ready, open a pull request. Before considering it done:

- run the relevant local tests and formatters
- push the branch
- confirm all CI pipelines pass
- inspect automated code review comments
- address actionable automated review comments with follow-up commits
- leave a concise PR summary that describes behavior changes and validation
- when merging a PR make sure to delete the merged branch

## Design Rules

- Favor clean design and implementation over patching over existing code.
- Always look for generic implementations that can handle many of the cases,
  instead of creating a per-case custom implementation.
- Always look for ways to simplify the existing code and reducing the code
  size when addint new functionality.
- Favor simple generic strategies over custom handling for individual makers,
  models, or listings.
- Add the least code that preserves the required functionality and invariants.
- Prefer schema/data improvements, prompt clarity, and generic validation over
  hard-coded aliases or exception tables.
- Keep runtime code free of embedded migrations. Use explicit schema changes and
  migration scripts when needed.
- Remove obsolete fields and compatibility paths during active development
  instead of preserving dead behavior.
- Treat generated lookup rows as disposable when no root record references them.

## Data And LLM Guardrails

- Do not store generic avionics labels as concrete avionics models.
- Do not store aircraft variant labels that include maker names or model years.
- Do not use aircraft maker names as engine, propeller, or avionics makers
  unless they are factually the component maker.
- Required estimation fields should be non-null. Nulls are only acceptable for
  optional metadata that the app can operate without.
- If an LLM response is invalid, repair it by giving the model the original
  prompt, its invalid response, and the exact validation failure.
- If the model cannot ground or correct a value confidently, reject it instead
  of storing a dubious fact.

## Local Development

Run the web app:

```bash
cargo run --bin aircost-web -- --port 8001
```

Run Rust checks:

```bash
cargo fmt --all
cargo test
cargo check
```

Use admin commands for database healing and fitting. Prefer dry runs first:

```bash
cargo run --bin aircost-admin -- heal-aircraft-models --dry-run

# Agent Delegation Rules

When the task matches one of these patterns, delegate to the appropriate custom agent:

## Exploration / Questions about existing code
- "How does X work?", "Where is Y defined?", "Trace the flow of Z"
- → Use `explorer` agent(s). Spawn multiple if the question spans independent modules.

## Implementation with a defined spec
- "Add support for X", "Fix the error caused by Y", "Implement functionality Z"
- → Use `developer` agent. For multi-module changes, spawn one per module boundary.

## Code generator with detailed spec and implementation instructions
- "Implement X according to the spec", "Add feature Y as described in DESIGN.md"
- → Use `coder` agent. For fully specified changes with implementation instructions.

## When NOT to delegate
- Simple single-file edits, quick questions, or tasks that need tight back-and-forth iteration.
- Keep these in the main agent thread.
cargo run --bin aircost-admin -- curate-avionics --dry-run
cargo run --bin aircost-admin -- fit-depreciation --dry-run
```

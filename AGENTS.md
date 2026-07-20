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

## Design Rules

- Favor clean design and implementation over maximizing code reuse.
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
cargo run --bin aircost-admin -- curate-avionics --dry-run
cargo run --bin aircost-admin -- fit-depreciation --dry-run
```

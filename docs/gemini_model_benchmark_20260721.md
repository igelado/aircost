# Gemini Model Benchmark — 2026-07-21

## Scope

This benchmark used retained inputs from the production-shaped SQLite database.
The database contained 70 canonical listings, of which 69 had a linked retained
plugin submission suitable for replay. The deterministic four-listing sample
used submission IDs `36`, `20`, `75`, and `13`. Visual identity and grounded
metadata used the two explicitly audited submissions `20` and `36`; aircraft
hierarchy curation used listing `20` (`N1925X`, serial `18256025`).

Live benchmark execution did not update listings, submissions, aircraft,
avionics, or valuation data. Its only database writes were `gemini_api_usage`
rows. SHA-3 hashes for all 72 domain tables present in the pre-accounting backup
were identical after the runs.

## Selected Runtime Defaults

| Task | Default model | Decision |
|---|---|---|
| Listing extraction | `gemini-3.5-flash-lite` | Same 4/4 structural success, one fewer provider call, much lower latency and lower estimated cost than 3.1 Flash-Lite. |
| Grounded metadata | `gemini-3.5-flash` | Conservative placeholder. No candidate actually used Search, so none passed the grounding gate and no downgrade is justified. |
| Avionics identity | `gemini-3.5-flash` | Only candidate with 4/4 structurally complete classifications and any observed Search use. It still failed the required per-case grounding gate. |
| Avionics collision review | `gemini-3.5-flash` | Same conservative choice as identity; cheaper candidates had additional structural/review failures. |
| Aircraft visual identity | `gemini-3.1-flash-lite` | 2/2 exact N-number transcriptions and the lowest estimated cost. |
| Aircraft Search grounding | `gemini-3.5-flash` | Full Flash produced substantially stronger research and more official evidence than Flash-Lite. |
| Aircraft URL verification | `gemini-3.5-flash` | Keep the evidence workflow on one stronger model until each stage has a larger labeled sample. |
| Aircraft structure | `gemini-3.5-flash` | Both candidates failed authority validation; no safe downgrade was demonstrated. |
| Aircraft catalog adjudication | `gemini-3.5-flash` | Full Flash produced the stronger evidence set, although the end-to-end case remained blocked. |
| Aircraft hierarchy verification | `gemini-3.5-flash` | Verification was not reached after either candidate failed earlier authority checks. |

The active values and comparison matrices are in `config/gemini.toml`. They are
runtime configuration, not compile-time constants.

## Comparison Results

### Listing extraction

| Model | Structurally valid | Provider calls | Mean latency | Approximate report cost |
|---|---:|---:|---:|---:|
| `gemini-3.1-flash-lite` | 4/4 | 5 | 9.58 s | ~$0.0302 |
| `gemini-3.5-flash-lite` | 4/4 | 4 | 2.94 s | ~$0.0217 |

The 3.1 model needed one JSON repair. Both produced plausible extractions, but
the retained historical extraction is model-produced and was not treated as
labeled truth. The costs above are comparison estimates, not invoice totals;
some provider counters were omitted.

### Grounded avionics metadata

The two cases were Garmin AERA 796 and the misleading listing label
`Mid-Continent PAI-700`.

| Model | Structurally valid | Search/citation valid | Mean latency |
|---|---:|---:|---:|
| `gemini-3.1-flash-lite` | 2/2 | 0/2 | 3.30 s |
| `gemini-3.5-flash-lite` | 2/2 | 0/2 | 3.10 s |
| `gemini-3.5-flash` | 2/2 | 0/2 | 9.37 s |

All three models emitted plausible-looking source URLs without an observed
Google Search call or grounding citation. Their unsupported values also
diverged materially: AERA 796 installed contribution ranged from $800 to
$1,500 and replacement cost from $1,200 to $2,499; PAI-700 introduction year
was either 1990 or 2000. These outputs must not be accepted as grounded facts.

### Avionics identity and collision review

The four cases covered an exact AERA 796 catalog match, GEA-71 versus GEA71B,
IFD440 multifunction capabilities, and correction of PAI-700 from the listing's
`Mid-Continent` label to Precision Aviation.

| Model | Structurally complete | Cases with required grounding | Mean latency | Approximate report cost |
|---|---:|---:|---:|---:|
| `gemini-3.1-flash-lite` | 3/4 | 0/4 | 7.39 s | ~$0.0196 |
| `gemini-3.5-flash-lite` | 2/4 | 0/4 | 3.74 s | ~$0.0182 |
| `gemini-3.5-flash` | 4/4 | 0/4 | 16.66 s | ~$0.1818 |

Full Flash made one observed Search call across eight classification/review
requests; the other calls merely returned plausible URLs. The benchmark
therefore rejected every model despite the better structural result from full
Flash.

### Single-photo aircraft identity

The two audited photos had exact expected registrations `N1925X` and `N182KW`.

| Model | Exact result | Mean latency | Approximate report cost |
|---|---:|---:|---:|
| `gemini-3.1-flash-lite` | 2/2 | 3.61 s | ~$0.00155 |
| `gemini-3.5-flash-lite` | 2/2 | 2.38 s | ~$0.00168 |
| `gemini-3.6-flash` | 2/2 | 3.66 s | ~$0.0101 |

This is enough to choose the cheapest exact candidate as the current default,
but only for the existing conservative one-photo transcription contract. Two
clear photos are not a broad visual benchmark.

### Aircraft hierarchy curation

Both full Flash and Flash-Lite were tested end to end against the same FAA-gated
Cessna 182H case. Full Flash took about 92 seconds and issued 10 Search queries;
Flash-Lite took about 21 seconds and issued 2. Full Flash found stronger FAA
registry/DRS evidence, while Flash-Lite relied on a non-FAA-hosted TCDS copy.

Neither case became reviewable. Full Flash mislabeled AFAC/EASA sources as
acceptable FAA regulator/type-certificate authority for the N-registered
identity; Flash-Lite mislabeled a private Peter-FTP copy. Deterministic source
authority validation correctly blocked both. Only the Search-stage costs were
fully known in durable accounting: $0.194563 for full Flash and $0.029959 for
Flash-Lite.

## Accounting Interpretation

There are 61 usage rows from all comparison and diagnostic runs. Six have every
counter needed for a dated list-price calculation, totaling $0.324998. Cost is
unknown for the other 55 calls because at least one provider counter was
omitted. The implementation deliberately stores null for those costs instead
of treating missing counters as zero. Consequently, $0.324998 is a known-cost
lower bound, not the total bill.

## Blocking Findings

1. Enabling the GenerateContent Google Search tool does not make its use
   mandatory. Metadata and avionics responses can look sourced while performing
   no Search. The next design step should move evidence discovery to a separate
   Interactions request with forced tool choice, followed by URL verification
   and structured classification.
2. Any-Search/any-citation is still too weak. Evidence must support each identity,
   introduction-year, configuration, and monetary claim individually.
3. Avionics metadata currently has identity provenance fields but no distinct
   source fields for introduction year and each value. Persisting `gemini` as a
   generic value source is insufficient.
4. Aircraft source authority remains a semantic failure point. The local FAA
   host and claim-specific authority checks correctly prevent bad proposals,
   but the model prompts/schema need to make those distinctions more reliably.
5. The four-listing and two-image samples are useful routing smoke tests, not a
   statistically representative quality benchmark. Default promotion should
   eventually require a larger frozen, human-labeled suite.

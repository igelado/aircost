# Proposal: A Tiny Monotone DNN for Aircraft Value and Depreciation

Status: proposed experiment; deploy only if it improves small-sample validation

Scope: a neural substitute for the learned valuation path, using only current
aircraft sale listings

Reviewed: 2026-07-20

## Goal and Design Position

The network should return a useful approximate value even when the training
snapshot is small. It is not intended to reproduce a professional appraisal,
and it should not require appraisal-grade data.

The architecture is therefore a **tiny monotone neural additive model**, not a
large language model, generic multilayer perceptron, or tabular Transformer. It
combines hard-to-break depreciation structure with a small learned residual.
Strong sharing and bounded outputs are more valuable here than raw capacity.

The model consumes only facts found in current aircraft sale listings and is
trained on their current USD asking prices. With at least one valid listing, it
always emits:

- a current point-value estimate
- an empirically calibrated error range
- a constant-market depreciation schedule
- a high, medium, or low support grade

The pooled structural regression in
[valuation_model_improvements.md](valuation_model_improvements.md) remains the
recommended first implementation. This DNN is a substitute only if it beats
that model on the same grouped out-of-sample predictions. A tie goes to the
simpler regression.

## Why the Earlier Large-DNN Direction Is Wrong for This Goal

A multi-million-parameter Feature Tokenizer Transformer, raw-text encoder, and
50,000-listing activation threshold solve a different problem. They would leave
the intended small-sample use case without a trained model and create enough
capacity to memorize individual advertisements.

The revised design makes four changes:

1. Cap the learned core at roughly 10,000 parameters, and lower when the
   snapshot is very small.
2. Encode a decreasing, bounded age relationship in the architecture rather
   than asking the data to discover it.
3. Share nearly all slopes across aircraft and let identity mainly adjust the
   price level.
4. Use a fallback path inside the network, so unknown or rare aircraft still
   receive the global estimate rather than an abstention.

## Data Constraint

Every label and aircraft-specific feature must come from a current sale listing
in the frozen snapshot. The DNN must not read or derive features from:

- closed sales, appraisals, or price guides
- historical listings or historical new prices
- CPI, currency rates, or market indices
- registry, accident, maintenance, or title databases
- externally researched specifications
- external TBOs, overhaul costs, or equipment prices
- facts supplied from model pretraining without a span in the listing

Only listings with an explicit USD asking price train the initial model. A
non-USD listing is usable only when the same advertisement states a USD price.

Fixed time constants, parameter bounds, initial weights, and regularization are
model assumptions. They do not introduce additional factual aircraft data.

## Prediction Target

For aircraft facts `x` and the market snapshot `S`, train the point model on:

```text
y = log(asking_price_usd)
```

The displayed value is:

```text
estimated_value = exp(predicted_y)
```

The model is optimized to estimate the central current asking price implied by
the snapshot. An out-of-fold residual distribution supplies the error range;
the network does not need a fragile three-quantile head when there are only a
few examples.

## Architecture Overview

```text
listing fields
    |
    +-- identity hierarchy ---------> shrunk price-level tower -----+
    |                                                             |
    +-- category context -----------> monotone age tower -----------+
    |                                                             |
    +-- age + airframe hours -------> monotone utilization tower ---+--> log value
    |                                                             |
    +-- component state -----------> bounded component tower -------+
    |                                                             |
    +-- equipment/condition -------> bounded residual tower --------+
                                                                  |
                                      global snapshot anchor -------+
```

The total prediction is additive on log price:

```text
log_value = global_anchor
          + identity_offset
          + age_effect
          + airframe_hours_effect
          + component_effect
          + bounded_listing_residual
```

This structure has a useful failure mode: when data cannot support the neural
branches, regularization drives their outputs toward zero and the result
collapses to a global current-listing anchor plus the generic age curve.

## Inputs

### Identity hierarchy

- category
- maker
- model
- variant

Use normalized names derived from the listing. Do not add externally sourced
performance or specification fields.

### Numeric fields

- aircraft age at snapshot date
- total airframe hours
- engine and propeller numeric times, when stated
- engine and propeller counts
- number of listing fields known
- number of explicitly named equipment items and modifications

Transform long-tailed hour and count values with `log1p`. Fit medians and scales
on each training fold only. Every optional numeric field has a missingness bit;
missing values are set to the fold median after the bit is created.

### Time-basis fields

For each engine or propeller time, retain the listing's stated basis:

- since new
- since overhaul
- since inspection
- time remaining
- unknown

The architecture must not interpret raw component hours as percent of life
without an externally sourced TBO.

### Equipment and condition

The initial version uses compact summaries and stable extracted flags. An
optional extension represents explicitly named items as a small set:

```text
item token -> 4-dimensional hashed embedding -> mean pooling + log item count
```

Use at most 128 hash buckets, include an unknown bucket, and attach the source
text span to every extracted token. Remove prices, financing terms, seller
claims such as `priced to sell`, URLs, phone numbers, and source-site identity
before feature extraction.

Do not feed raw description text to the initial DNN. With a small dataset, the
network is more likely to learn seller/source artifacts or recover the asking
price than a general aircraft-quality signal.

## Tower Details

### 1. Global anchor and identity price level

Initialize `global_anchor` to the training fold's median age-adjusted log price:

```text
global_anchor = median(log(price) - initialized_age_effect(age))
```

Learn a small adjustment during training. Age adjustment is necessary because
using the raw median and then adding the age tower would discount the training
aircraft twice.

Represent category, maker, model, and variant with scalar offsets plus tiny
context embeddings:

```text
identity_offset = category_scalar
                + maker_scalar
                + model_scalar
                + variant_scalar
```

Penalize child offsets more strongly when their groups have few examples.
Singleton variants can influence their estimate, but only through a heavily
shrunk offset. Rare and unseen identities use zero at that level and therefore
fall back to their known parent or the global anchor.

Suggested context dimensions:

| Field | Dimension |
| --- | ---: |
| Category | 3 |
| Maker | 3 |
| Model | 4 |
| Variant | 4 |

Concatenate available context embeddings and project them to an 8-dimensional
context vector. Vocabulary entries with inadequate support can share an
`unknown` vector while retaining their scalar hierarchical offset.

### 2. Monotone age tower

Use fixed saturating exponential bases at several time scales:

```text
b_k(age) = 1 - exp(-age / tau_k)
tau = [1, 5, 15, 40] years
```

Predict four non-negative weights from a global parameter plus a very small
category context:

```text
raw_weights = global_age_weights + Linear(category_context)
weights = bounded_nonnegative(raw_weights)

age_effect = -sum_k(weights_k * b_k(age))
```

Constrain the sum of the weights to at most `2.3` log points, so the age tower
alone retains at least approximately 10% of its age-zero value at its long-run
floor. Initialize the weights to resemble the existing generic depreciation
curve and heavily penalize category departures from the global weights.

This construction guarantees:

- value cannot increase solely because the aircraft becomes older
- the age adjustment is smooth
- the rate can be faster early and slower later
- the age effect approaches a nonzero floor
- the global curve still works for an unseen category

It also uses only four global shape weights rather than fitting a separate
multi-parameter curve to each model.

### 3. Monotone airframe-hours tower

Estimate expected log hours by age from current listings, using the same robust
pooled relationship proposed for the regression. Feed the signed residual to a
constrained coefficient:

```text
hours_residual = log(1 + hours) - expected_log_hours(age, category)
beta_hours = -softplus(raw_beta_hours)

airframe_hours_effect = clip(beta_hours * hours_residual, -0.35, 0.35)
```

The shared coefficient guarantees that above-typical hours do not increase
value. The clipping bound limits this branch to approximately a 30% decrease or
42% increase. Missing hours use a zero residual plus a missingness flag in the
residual tower.

Only allow category-specific adjustments to `beta_hours` after an ablation
shows a repeatable cross-validation benefit.

### 4. Bounded component tower

For engine and propeller values, use small monotone linear splines conditioned
on the stated time basis. `Time remaining` receives the opposite monotonic
direction from `time since new/overhaul`; an unknown basis receives no numeric
effect.

Pool multiple components by mean, minimum, and count. A component set is
permutation-invariant, so swapping engine order cannot change value.

Clip the combined engine/propeller effect to `[-0.25, 0.25]` log points and
regularize it strongly toward zero. Freeze this tower at zero when the training
fold lacks repeated examples with comparable time bases.

### 5. Bounded listing residual tower

Concatenate:

- the 8-dimensional identity context
- numeric missingness flags
- listing completeness
- equipment and modification counts
- optional pooled equipment embedding
- explicit listing-derived condition flags

Use a small network:

```text
input
 -> Linear(24), SiLU, Dropout(0.10)
 -> Linear(12), SiLU, Dropout(0.10)
 -> Linear(1)
 -> 0.30 * tanh(output)
```

The residual can alter price by at most `0.30` log points, or roughly -26% to
+35%. This prevents one rare equipment token or condition phrase from
overwhelming the aircraft's shared price level.

If the snapshot is too small to populate these fields repeatedly, omit the
tower or freeze its output at zero.

## Parameter Budget

The exact count depends on the current snapshot's vocabulary. Enforce these
limits:

| Component | Target maximum |
| --- | ---: |
| Identity scalar offsets and embeddings | 6,000 |
| Equipment hash embeddings | 512 |
| Age, hours, and component towers | 1,000 |
| Residual tower | 2,000 |
| Total trainable parameters | 10,000 |

When the identity vocabulary would exceed the budget, map the least-supported
variants and models to their parent/unknown context. Keep their listing prices
available to the parent fit; do not create thousands of one-example vectors.

The 10,000-parameter ceiling is a maximum, not a target. Early snapshots should
train substantially fewer parameters.

## Progressive Capacity for Very Small Snapshots

The model always returns a point estimate, but not every tower should be free to
learn at every sample size.

| Deduplicated listings | Trainable capacity |
| --- | --- |
| 1-9 | Global current-price anchor; fixed initialized age curve; all neural residuals frozen. |
| 10-49 | Global age weights, shared hours effect, and strongly shrunk identity scalars. |
| 50-199 | Tiny context embeddings and bounded listing residual, if cross-validation improves. |
| 200+ | Full architecture up to the parameter cap, still subject to ablation. |

These are conservative defaults, not hard scientific thresholds. A diverse set
of 40 listings can identify a global age curve better than 100 near-duplicate
listings. The effective rule is that a branch remains frozen unless its
out-of-fold benefit is stable across resamples.

At the smallest sizes this system behaves more like a neural parameterization
of the structural formula than a conventional DNN. That is intentional: it
retains a value-producing prior and grows only as today's sample permits.

## Training

### Objective

Use Huber loss on log asking price:

```text
loss = huber(predicted_log_price, actual_log_price)
     + identity_shrinkage
     + age_prior_penalty
     + weight_decay
```

Suggested starting settings:

- AdamW optimizer
- learning rate `1e-3`
- weight decay between `1e-3` and `1e-2`
- batch size `min(32, training_count)`
- gradient norm clipping at `1.0`
- at most 500 epochs
- one fixed small hyperparameter set before considering a search

Do not run a large hyperparameter sweep on a small snapshot. Choose at most a
few predeclared regularization settings using the grouped folds, then freeze
them for subsequent comparisons.

### Initialization

- Initialize the dollar anchor from the current fold's median age-adjusted log
  asking price.
- Initialize identity and residual effects to zero.
- Initialize the age tower from the current generic bounded curve.
- Initialize component effects to zero.
- Initialize the hours coefficient to a small negative value.

If training has little signal, these initial values yield a plausible generic
curve anchored to today's listing prices rather than an arbitrary neural
output.

### Resampling ensemble

Train five to ten small models on bootstrap resamples of complete physical
aircraft groups. Use the median prediction as the displayed point. The spread
across members is a model-instability signal, not the entire prediction error.

Combine it with the parent proposal's grouped out-of-fold residual range:

```text
reported_log_error = max(
    out_of_fold_q80_for_support_group,
    ensemble_instability_allowance
)
```

When the sample is extremely small, use the same conservative fallback error
range as the structural model. The DNN must never turn low confidence into no
value.

## Validation

Use exactly the same prepared rows and splits for every candidate:

1. hierarchical/global median
2. pooled structural regression
3. boosted-tree baseline only when sample size makes it meaningful
4. tiny monotone DNN

For fewer than 20 unique aircraft, use leave-one-aircraft-out validation. For
larger snapshots, use repeated grouped five-fold validation. Advertisements for
the same physical aircraft must stay in one fold. Also run leave-one-model-out
tests to exercise the DNN's parent/global fallback.

All preprocessing, category vocabularies, price anchors, hour norms, and
equipment vocabularies must be learned inside each training fold. Otherwise the
small reported error will be optimistic.

Use the same initial engineering goals as the structural proposal:

| Metric | Initial target |
| --- | --- |
| Median absolute percentage error | 25% or less |
| Mean signed percentage error | between -10% and +10% |
| 80th-percentile absolute percentage error | 40% or less |
| 80% empirical range coverage | 70% to 90% |

In addition, require:

- no negative, zero, NaN, or infinite estimates
- monotone non-increasing age-only projections
- no more than a configured bounded change from any one residual tower
- a finite global prediction for every unknown identity and missing-field
  combination
- stability under leave-one-aircraft-out and bootstrap resampling

The DNN is accepted only when its improvement over the pooled regression is
larger than the variability across folds. A small mean improvement caused by
one lucky split is not enough.

### Required ablations

Measure the incremental value of:

- category-conditioned age weights
- identity context embeddings beyond scalar offsets
- component tower
- listing residual tower
- equipment set embeddings beyond simple counts
- ensemble versus a single seeded network

Remove any branch that does not improve held-out error or bias. The deployed
network may be much smaller than this maximum architecture.

## Depreciation Output

The monotone age tower makes depreciation a direct model output rather than a
separate unconstrained network.

For current age `a` and horizon `t`:

```text
future_hours = current_hours + listing_derived_utilization * t

log_value_change(t) = age_effect(a + t) - age_effect(a)
                    + hours_effect(future_hours) - hours_effect(current_hours)

value(t) = value_now * exp(log_value_change(t))
```

Keep identity, equipment, condition, and component effects fixed. Derive typical
utilization from current listings and shrink it from comparable groups to the
global median. Do not simulate overhaul cycles without TBO and cost data.

This is the same explicit approximation as the structural proposal: today's
cross-sectional age and hours relationships are assumed to remain stable, and
the overall market price level is held fixed. Return constant-today USD values,
dollar depreciation, percentage depreciation, and an error range widened with
horizon.

Because age weights are non-negative and the future hours effect is monotone,
the projected value cannot rise merely from aging and accumulating typical
hours.

## Fallback Behavior

Unknown identities map to zero offsets and unknown context vectors:

```text
known variant -> known model -> known maker/category -> category -> global
```

Missing optional features contribute their neutral effect plus missingness
bits. If all optional fields are absent, the global anchor, any known identity
levels, and the age tower still produce a finite value.

Support grading matches the structural proposal:

- **High:** several close deduplicated comparables inform identity and residual
  features.
- **Medium:** price level mainly comes from the maker/category hierarchy.
- **Low:** prediction mainly uses the global anchor and initialized structural
  curve.

The product always displays the point estimate. Low support widens the error
range and explains the fallback; it is not an abstention state.

## Implementation Instructions

Implement the DNN only after the shared `src/valuation` types, frozen snapshot,
fold builder, persistence tables, comparable baseline, and structural model in
[valuation_model_improvements.md](valuation_model_improvements.md) exist. The
DNN must reuse those components rather than defining a second data contract or
evaluation path.

### 1. Add an optional Rust training backend

Keep the default structural build lightweight. Add Burn as an optional Cargo
feature and pin it in `Cargo.lock`. At this document's review date, Burn 0.21
supports training/autodiff, a pure-Rust Flex CPU backend, and model storage.

Use this shape in `Cargo.toml`, adjusting only if the pinned release's feature
names require it:

```toml
[features]
default = []
dnn = ["dep:burn"]

[dependencies.burn]
version = "0.21"
optional = true
default-features = false
features = ["std", "train", "flex", "store"]
```

Use the Flex backend for the first implementation; this network is too small to
justify a required CUDA or LibTorch runtime. Wrap Flex with Burn's autodiff
backend for training and use the inner non-autodiff backend for inference.

Guard all neural code with the `dnn` feature. A default build must still serve
the structural model. When `fit-valuation --kind dnn` is requested from a build
without the feature, return a clear configuration error rather than silently
fitting another model.

### 2. Add the DNN module tree

Create:

```text
src/valuation/dnn/
  mod.rs          feature-gated public DNN model
  features.rs     fold-local vocabularies and tensor encoding
  network.rs      constrained Burn module and forward pass
  train.rs        loss, optimizer, capacity tiers, folds, and bootstrap fit
  artifact.rs     metadata plus model-record serialization
```

Expose `DnnValuationModel` through the existing `ValuationModel` trait. No DNN
type should leak into `aircraft.rs` or HTTP response types.

Define an explicit capacity enum:

```rust
pub enum DnnCapacity {
    PriorOnly,
    Shared,
    Contextual,
    Full,
}
```

Map the sample-size table in this proposal to a default capacity, then reduce
capacity further when diversity is poor. Persist the chosen tier; inference
must never infer architecture from the current database contents.

`PriorOnly` still creates a valid artifact containing the listing-derived
anchor and fixed constrained age curve. It has no trained residual branch and
therefore behaves like the structural fallback for a one-to-nine-listing
snapshot.

### 3. Build a fold-local feature encoder

Define and serialize `FeatureEncoderV1` with:

- category, maker, model, and variant vocabularies
- index zero reserved for unknown at every identity level
- numeric field order
- training-fold medians and robust scales
- missingness-field order
- equipment hash seed and bucket count
- component time-basis vocabulary
- feature-schema version

Build the encoder from training rows only for every outer fold. For the final
artifact, build it from the full frozen snapshot. Never use the target price to
construct a vocabulary, normalize a feature, or select an equipment token.

The current database schema supports maker/model/variant, age, three hour
values, missingness, and listing-attached avionics names. Until the extractor
stores listing-stated category and component time bases:

- map category to unknown
- treat engine/propeller time basis as unknown
- disable the component tower

Do not infer either field from an external aircraft specification.

Map rare context embeddings to unknown according to the selected capacity
tier. Retain their scalar hierarchical offsets only when repeated data support
them. Use a stable SHA-256-derived equipment bucket rather than Rust's default
process-randomized hash. Pad equipment sets to a fixed configured maximum and
carry a mask so padding does not affect mean pooling.

Add a test that serializes an encoded example and enumerates every source
feature. Asking price, URLs, source site, financing language, and seller contact
information must be absent.

### 4. Implement the constrained network

Define `TinyValuationNet<B>` as a Burn module with these parameter groups:

```text
global anchor
identity scalar embeddings
small identity context embeddings and 8-D projection
four age-mixture parameters plus optional category adjustment
one shared hours parameter
optional component spline parameters
optional equipment embedding and 24 -> 12 -> 1 residual MLP
```

The forward pass should be visibly additive:

```rust
let log_value = global_anchor
    + identity_offset
    + monotone_age_effect
    + monotone_hours_effect
    + bounded_component_effect
    + bounded_residual;
```

Do not pass age into the unconstrained residual MLP. Age belongs only in the
monotone age tower. Do not pass raw airframe hours into the residual MLP; use
them only through the constrained hours tower. These separations are what make
future projections monotone by construction.

Implement age weights as:

```text
total_drop = 2.3 * sigmoid(raw_total_drop)
mixture = softmax(raw_global_mix + bounded_category_delta)
weights = total_drop * mixture

age_effect(age) = -sum_k(
    weights_k * (1 - exp(-age / tau_k))
)
```

with fixed `tau = [1, 5, 15, 40]`. Bound the category delta before the softmax
and regularize it toward zero. This parameterization enforces nonnegative
weights and a maximum 2.3-log-point age drop without post-prediction repair.

Implement the hours coefficient as `-softplus(raw_beta_hours)`, multiply it by
the listing-only hours residual, then clip that branch to `[-0.35, 0.35]`.
Implement component and final residual bounds exactly as specified earlier in
this proposal.

Initialize:

- global anchor from the structural model's age-adjusted current-listing anchor
- global age mixture from the fitted structural age curve
- all identity, context, component, and residual parameters to zero
- hours parameter from the structural model's nonpositive coefficient

The structural candidate used for initialization must use the same snapshot
and must be recorded as `baseline_model_version_id` in DNN metadata. No value
from the old external-metadata estimator may initialize the network.

### 5. Enforce capacity in code

Compute the parameter count from vocabulary sizes and layer shapes before
allocating a network. Reject a configuration above 10,000 trainable parameters.
Also record the count from the instantiated module and assert that both counts
agree in a test.

Instantiate different modules per capacity tier rather than registering every
branch and hoping frozen parameters stay unchanged:

| Capacity | Enabled parameters |
| --- | --- |
| `PriorOnly` | Listing-derived anchor and fixed age constants only. |
| `Shared` | Global age mixture, shared hours coefficient, identity scalar offsets. |
| `Contextual` | `Shared` plus tiny identity context and bounded residual using simple counts/missingness. |
| `Full` | `Contextual` plus equipment-set and component branches when source fields exist. |

If a branch fails its ablation, construct the final artifact with the lower
capacity rather than storing unused trainable weights.

### 6. Implement loss and regularization

Use Huber loss on log price. Add penalties as explicit terms rather than relying
only on optimizer weight decay:

```text
loss = huber(predicted_log_price, actual_log_price)
     + identity_count_aware_penalty
     + category_age_deviation_penalty
     + residual_branch_penalty
     + optimizer_weight_decay
```

For an identity group `g`, scale the scalar/embedding penalty inversely with
`1 + training_count_g`; singleton and rare groups therefore stay closest to
their parents. Do not regularize the listing-derived global anchor toward zero.

Use full-batch training for very small folds and `min(32, row_count)` batches
otherwise. Start with the optimizer, learning rate, epoch, dropout, and gradient
clipping values specified in this proposal. Keep exactly one predeclared
training configuration for the first experiment.

Early stopping must not inspect the outer held-out fold. Establish an epoch
budget with an inner grouped validation split, record the selected epoch for
each outer fit, and use the median selected epoch count when fitting the final
full-snapshot ensemble. For folds too small to create an inner split, use the
predeclared epoch budget and the lower capacity tier.

Set and persist every random seed. Determinism is not required across different
CPU instruction sets, but repeated runs on the same build and device should
produce predictions equal within a documented floating-point tolerance.

### 7. Reuse the shared evaluator

`train.rs` must request folds from `valuation::validation`; it must not perform a
random row split. For every outer fold:

1. create a training-only `FeatureEncoderV1`
2. select capacity from training-group counts only
3. initialize from a structural model fitted only to that training fold
4. train without reading held-out rows
5. predict held-out rows with unknown fallback for unseen identities
6. return ordinary `FoldPrediction` values to the shared metrics code

Run the structural and DNN candidates on identical fold assignments. Store the
paired prediction delta per held-out aircraft so improvement can be separated
from fold composition.

Perform each optional-branch ablation with the same seed list and folds. Do not
retain a component, context, or equipment branch merely because its full-data
training loss is lower.

### 8. Train and store the final ensemble

After selecting a capacity and fixed training schedule, train five final members
on bootstrap resamples of duplicate groups from the full snapshot. Use one
full-snapshot feature encoder for all members. If a bootstrap omits an identity,
its unused embedding remains at the zero/parent initialization for that member.

Store these artifacts through `valuation_model_artifacts`:

```text
metadata.json
member-00.safetensors
member-01.safetensors
member-02.safetensors
member-03.safetensors
member-04.safetensors
```

`metadata.json` should contain:

```rust
pub struct DnnArtifactMetadataV1 {
    pub snapshot_id: i64,
    pub baseline_model_version_id: i64,
    pub capacity: DnnCapacity,
    pub architecture: DnnArchitectureConfig,
    pub encoder: FeatureEncoderV1,
    pub member_seeds: Vec<u64>,
    pub member_hashes: Vec<String>,
    pub parameter_count_per_member: usize,
    pub training_schedule: TrainingSchedule,
    pub group_counts: GroupCounts,
    pub error_bands: ErrorBands,
    pub utilization_rates: UtilizationRates,
}
```

Use Burn's model storage support to serialize weights. Verify every member hash
and metadata/artifact-format version before activation and again when loading.
A record with missing, extra, or incorrectly shaped tensors is invalid.

### 9. Implement inference and depreciation

At application startup:

1. load and validate metadata
2. construct the exact configured architecture
3. load all member records on the non-autodiff Flex backend
4. run one known-answer smoke input
5. wrap the ensemble in `Arc<dyn ValuationModel>`

For a request, encode once, batch the five member forward passes where
practical, take the median predicted log value, and exponentiate. Reject the
artifact at load time if its smoke prediction is nonfinite. If an individual
member nevertheless fails at runtime, omit it and emit a diagnostic; if no
member remains, call the active structural model referenced by the DNN artifact.

Choose the support grade and empirical range from the shared group counts and
out-of-fold error bands. Ensemble spread may widen the range but cannot narrow
the calibrated structural/DNN residual range.

Generate depreciation through paired predictions:

```text
log_change(t) = age_tower(age + t) - age_tower(age)
              + hours_tower(age + t, future_hours)
              - hours_tower(age, current_hours)

future_value(t) = current_value * exp(log_change(t))
```

Do not recompute identity or residual towers for future years. Advance hours
with the snapshot's shrunk utilization rate and keep equipment/components
fixed. Assert after inference that every curve point is no greater than the
previous point within floating-point tolerance; treat a violation as an
artifact error, not an opportunity to sort or clamp the output silently.

The existing `aircraft.rs` integration should remain unchanged when switching
from structural to DNN because both implement `ValuationModel`.

### 10. Extend the admin workflow

Use the commands defined in the structural implementation instructions:

```text
aircost-admin fit-valuation --kind dnn --snapshot-id ID [--apply]
aircost-admin validate-valuation --model-version-id ID
aircost-admin activate-valuation --model-version-id ID
```

Add DNN-specific dry-run output:

- selected capacity and reason
- parameter count per member
- enabled/disabled branch list
- inner selected epoch counts
- per-seed and ensemble metrics
- paired deltas versus the structural baseline
- constraint-test and artifact-load results

Candidate persistence and activation remain separate. Activation must also
confirm that the referenced structural fallback exists and uses the same
snapshot.

### 11. Add neural-specific tests

Run these tests only with the `dnn` feature where appropriate:

- every capacity tier builds and stays within its parameter budget
- an unknown identity produces a finite global/parent prediction
- increasing age on a dense grid never increases the age-tower output
- increasing above-typical airframe hours never increases value
- component order does not change prediction
- residual/component branches cannot exceed their log bounds
- padded equipment tokens do not change mean pooling
- price and source-site fields never enter an encoded tensor
- a synthetic listing-only dataset lowers held-out loss from initialization
- save/load round-trip preserves predictions within tolerance
- corrupt hashes and tensor shapes prevent activation
- five-member aggregation returns the median and widens for instability
- fixed seeds are reproducible within the documented tolerance
- DNN and structural models consume identical outer folds

Also add a property test over random valid queries checking finite positive
values and non-increasing 30-year curves. Do not assert exact floating-point
weights across backends.

Before merging neural code, run:

```text
cargo fmt --all -- --check
cargo test --locked
cargo check --locked
cargo test --locked --features dnn
cargo check --locked --features dnn
```

### 12. Apply explicit activation gates

In addition to the common error targets, require all of the following:

- aggregate MdAPE improves over the structural model by at least two percentage
  points, or signed bias materially improves without MdAPE worsening by more
  than one point
- DNN absolute percentage error is lower on at least 60% of paired held-out
  aircraft groups
- DNN 80th-percentile error is no more than five percentage points worse than
  the structural model
- all monotonicity, bound, artifact, fallback, and parameter-count checks pass
- the improvement remains in the same direction across a majority of bootstrap
  resamples

If these gates fail, retain the structural model and the DNN candidate for
analysis. Do not loosen the gates after looking at one favorable full-data fit.

### 13. Roll out in shadow mode first

Load the DNN beside the active structural model and compute both for sampled
requests. Do not use production requests as new training labels; only a newly
frozen current-listing snapshot can trigger retraining.

Measure inference latency, load time, artifact size, fallback frequency, and
prediction deltas. After a successful rollback drill, activate the DNN through
the same database transaction used for structural models. Retain the structural
artifact referenced by the DNN for immediate fallback.

Implementation is complete when a clean build with `--features dnn` can fit all
outer folds without leakage, persist and reload a five-member artifact, beat
the activation gates, and serve the same point/range/depreciation response as
the structural implementation for known and unknown aircraft.

## Main Risks and Controls

| Risk | Control |
| --- | --- |
| Memorizing a few listings | Tiny parameter budget, strong shrinkage, no raw text, grouped resampling. |
| Identity embedding absorbs asking price of a singleton | Scalar child effect with count-aware shrinkage and leave-one-model-out testing. |
| Implausible depreciation | Monotone saturating age basis with a bounded long-run drop. |
| One feature dominates price | Bounded component and residual towers. |
| Duplicate advertisements leak across folds | Group by physical aircraft before splitting. |
| Network looks better through hyperparameter search | Predeclare a few settings and compare on identical folds. |
| Unknown aircraft has no embedding | Parent/global zero-offset fallback. |
| Error estimate is overconfident | Out-of-fold empirical residuals, bootstrap stability, conservative fallback range. |

## Decision

Prototype this architecture only after the shared listing snapshot, simple
baselines, and grouped evaluator exist. It is suitable as a small-sample DNN
because it behaves like a bounded structural estimator at low data volume and
adds capacity gradually.

Deploy it only if it produces a repeatable out-of-fold improvement over the
pooled structural regression. Regardless of which model wins, use the DNN's
core product behavior as specified here: always return a point value, quantify
the likely error, and derive depreciation from a monotone cross-sectional curve
in constant-today dollars.

## References

- [Burn 0.21 crate documentation](https://docs.rs/burn/0.21.0/burn/).
- [Burn Book: automatic differentiation](https://burn.dev/books/burn/building-blocks/autodiff.html).
- You et al. (2017), [Deep Lattice Networks and Partial Monotonic Functions](https://arxiv.org/abs/1709.06680).
- Zaheer et al. (2017), [Deep Sets](https://arxiv.org/abs/1703.06114).
- Grinsztajn et al. (2022), [Why do tree-based models still outperform deep learning on typical tabular data?](https://proceedings.neurips.cc/paper_files/paper/2022/hash/0378c7692da36807bdec87ab043cdadc-Abstract-Datasets_and_Benchmarks.html).
- Gorishniy et al. (2021), [Revisiting Deep Learning Models for Tabular Data](https://arxiv.org/abs/2106.11959).

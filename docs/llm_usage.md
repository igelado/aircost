# LLM Usage

The app uses Gemini for extraction, normalization, and grounded metadata
enrichment. LLM output is never treated as durable truth without schema checks
and local validation.

The implementation is in `src/extract.rs`. The default model environment is:

```text
AIRCOST_GEMINI_MODEL=gemini-3.1-flash-lite
AIRCOST_GEMINI_GROUNDING_MODEL=gemini-3.1-flash-lite
AIRCOST_GEMINI_AVIONICS_REVIEW_MODEL=gemini-3.1-flash-lite
AIRCOST_GEMINI_THINKING_LEVEL=low
```

Set `GEMINI_API_KEY` to enable extraction and enrichment. If the key is absent,
manual listing preview still works, but URL/plugin extraction reports an error.

## JSON Contract

All model calls request `application/json` with an explicit response schema.
Parsing uses this contract:

- Parse the model response as a single JSON object.
- If parsing fails, send the original prompt, invalid response, and parse error
  back to Gemini for one repair attempt.
- Validate required fields locally after parsing.
- Reject or correct responses that omit required source rows, repeat source
  rows, produce unknown source IDs, return null for required fields, or produce
  generic values where concrete values are required.

Prompts for creation-critical extraction explicitly require non-null values.
Null is allowed only for optional metadata such as registration or serial number
when the listing does not provide it.

## Listing Extraction

The extraction prompt receives cleaned listing text and returns:

- manufacturer
- model family
- variant
- model year
- asking price and currency
- airframe, engine, and propeller hours
- registration and serial number when present
- status
- avionics candidates

The model/variant split is important:

- `model` is the broad economic family used for depreciation fitting.
- `variant` is the concise material configuration inside that family.
- Variant labels must omit maker and model year.
- Variant labels must keep material distinctions such as turbo, pressurized,
  retractable, amphibious, turbine, generation, or package when those affect the
  aircraft configuration.

## Aircraft Model And Variant Normalization

After extraction, the code compares the returned manufacturer/model/variant to
known database rows.

For model families, the LLM is asked whether the extracted model and a known
candidate are the same economic family. This allows values such as `182T` to map
to a broader family such as `182 SKYLANE` while preserving `182T` as variant
information.

For variants, the LLM is asked whether an extracted variant and a known variant
identify the same exact material configuration. The code passes listing context
and plausible candidates; it does not add maker/model-specific aliases.

Variant healing sends all variants for one manufacturer/model family to Gemini
and asks for groups. The local validator requires every input variant to appear
exactly once. If a subset is missing or duplicated, the correction prompt sends
the original context, previous response, validation error, missing rows, and
duplicated rows back to the model.

## Avionics Resolution

Avionics parsing is intentionally strict. A durable avionics row should be a
concrete unit, integrated suite, or named package. Generic labels are not useful
for valuation and should not be stored as concrete avionics models.

For each extracted avionics candidate that is not already verified in the DB,
the system runs two checks:

- a grounded resolution call that verifies or corrects the candidate
- a second concreteness classifier that flags generic or ambiguous labels

The grounded resolver returns one of:

- `concrete`: the candidate or corrected label identifies a real avionics unit,
  suite, or package
- `factory_default`: the candidate was generic or malformed, but a concrete
  default unit for the aircraft year/model/variant was found
- `reject`: no reliable concrete unit should be stored

Local review rejects common failure modes: aircraft maker used as avionics
maker, empty or generic manufacturer/model, model equal to the equipment class,
combined alternative model numbers, broad product families, low confidence, and
missing source data.

## Grounded Metadata

Gemini with Google Search grounding is used for factual metadata:

- avionics introduction year and estimated unit value
- default/factory avionics by aircraft variant and model year
- model-year new-price points
- variant-level aircraft specs, engine model, propeller model, TBOs, overhaul
  costs, fuel burn, and maintenance assumptions

Grounded prompts require source URL, source title, confidence, and non-null
values for fields that the database needs.

## Normalization Philosophy

Do not fix LLM mistakes by adding one-off maker/model branches. The preferred
repair path is:

1. Make the prompt more precise.
2. Add generic validation for the class of error.
3. Send the original prompt, invalid response, and exact validation issues back
   to the model for correction.
4. Reject low-confidence or generic output instead of storing bad facts.
5. Add durable, reusable database facts only after they are concrete.

This keeps the system able to handle new manufacturers and aircraft families
without accumulating fragile special cases.

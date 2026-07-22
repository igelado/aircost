-- Add provider-agnostic request accounting for every Gemini task. This
-- migration is additive and idempotent; it does not alter listing or curation
-- data. Back up the database first and invoke sqlite3 with -bail.

PRAGMA foreign_keys = ON;
BEGIN IMMEDIATE;

CREATE TABLE IF NOT EXISTS gemini_api_usage (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  task TEXT NOT NULL,
  purpose TEXT NOT NULL,
  api_family TEXT NOT NULL
    CHECK (api_family IN ('generate_content', 'interactions')),
  api_version TEXT,
  model TEXT NOT NULL,
  service_tier TEXT NOT NULL DEFAULT 'standard',
  status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN (
    'pending', 'completed', 'failed', 'cancelled', 'incomplete',
    'requires_action', 'budget_exceeded'
  )),
  validation_status TEXT NOT NULL DEFAULT 'not_evaluated'
    CHECK (validation_status IN ('not_evaluated', 'accepted', 'rejected')),
  provider_request_id TEXT,
  correlation_id TEXT,
  request_fingerprint TEXT,
  aircraft_sale_listing_id INTEGER
    REFERENCES aircraft_sale_listings(id) ON DELETE SET NULL,
  source_kind TEXT,
  source_id TEXT,
  input_tokens INTEGER CHECK (input_tokens IS NULL OR input_tokens >= 0),
  output_tokens INTEGER CHECK (output_tokens IS NULL OR output_tokens >= 0),
  thought_tokens INTEGER CHECK (thought_tokens IS NULL OR thought_tokens >= 0),
  cached_tokens INTEGER CHECK (cached_tokens IS NULL OR cached_tokens >= 0),
  tool_tokens INTEGER CHECK (tool_tokens IS NULL OR tool_tokens >= 0),
  search_query_count INTEGER
    CHECK (search_query_count IS NULL OR search_query_count >= 0),
  attempt_count INTEGER NOT NULL DEFAULT 1 CHECK (attempt_count >= 1),
  retry_count INTEGER NOT NULL DEFAULT 0
    CHECK (retry_count >= 0 AND retry_count = attempt_count - 1),
  latency_ms INTEGER CHECK (latency_ms IS NULL OR latency_ms >= 0),
  error_text TEXT,
  estimated_cost_microusd INTEGER
    CHECK (estimated_cost_microusd IS NULL OR estimated_cost_microusd >= 0),
  pricing_snapshot_json TEXT,
  started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  CHECK (length(trim(task)) > 0),
  CHECK (length(trim(purpose)) > 0),
  CHECK (api_version IS NULL OR length(trim(api_version)) > 0),
  CHECK (length(trim(model)) > 0),
  CHECK (length(trim(service_tier)) > 0),
  CHECK (provider_request_id IS NULL OR length(trim(provider_request_id)) > 0),
  CHECK (correlation_id IS NULL OR length(trim(correlation_id)) > 0),
  CHECK (request_fingerprint IS NULL OR length(trim(request_fingerprint)) > 0),
  CHECK (
    (source_kind IS NULL AND source_id IS NULL)
    OR (
      source_kind IS NOT NULL AND length(trim(source_kind)) > 0
      AND source_id IS NOT NULL AND length(trim(source_id)) > 0
    )
  ),
  CHECK (
    (estimated_cost_microusd IS NULL AND pricing_snapshot_json IS NULL)
    OR (estimated_cost_microusd IS NOT NULL AND pricing_snapshot_json IS NOT NULL)
  ),
  CHECK (
    (status = 'pending' AND completed_at IS NULL)
    OR (status <> 'pending' AND completed_at IS NOT NULL)
  ),
  CHECK (status = 'completed' OR validation_status = 'not_evaluated'),
  CHECK (status <> 'failed' OR length(trim(error_text)) > 0),
  CHECK (status <> 'completed' OR error_text IS NULL)
);

CREATE INDEX IF NOT EXISTS idx_gemini_api_usage_correlation
  ON gemini_api_usage (correlation_id, id);
CREATE INDEX IF NOT EXISTS idx_gemini_api_usage_task_model
  ON gemini_api_usage (task, purpose, model, service_tier, started_at);
CREATE INDEX IF NOT EXISTS idx_gemini_api_usage_listing
  ON gemini_api_usage (aircraft_sale_listing_id, started_at);
CREATE INDEX IF NOT EXISTS idx_gemini_api_usage_source
  ON gemini_api_usage (source_kind, source_id, started_at);

COMMIT;
PRAGMA foreign_key_check;

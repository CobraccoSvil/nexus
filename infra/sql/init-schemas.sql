-- Enable extensions
CREATE EXTENSION IF NOT EXISTS pgvector;
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- Audit table per LLM calls
CREATE TABLE IF NOT EXISTS audit_llm_calls (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  request_id VARCHAR(255) NOT NULL UNIQUE,
  tenant_id VARCHAR(255) NOT NULL,
  user_id VARCHAR(255),
  feature VARCHAR(255),
  sensitivity_tier INT,
  model_requested VARCHAR(255),
  model_used VARCHAR(255),
  provider_used VARCHAR(255),
  prompt_hash VARCHAR(64),
  response_hash VARCHAR(64),
  input_tokens INT,
  output_tokens INT,
  latency_ms INT,
  finish_reason VARCHAR(50),
  redaction_applied BOOLEAN DEFAULT FALSE,
  dlp_blocked BOOLEAN DEFAULT FALSE,
  dlp_patterns TEXT[],
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  retention_until TIMESTAMP DEFAULT CURRENT_TIMESTAMP + INTERVAL '90 days'
) PARTITION BY RANGE (created_at);

CREATE INDEX IF NOT EXISTS idx_audit_tenant_created ON audit_llm_calls(tenant_id, created_at);
CREATE INDEX IF NOT EXISTS idx_audit_request_id ON audit_llm_calls(request_id);

-- Vector embeddings table
CREATE TABLE IF NOT EXISTS embeddings (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id VARCHAR(255) NOT NULL,
  document_id VARCHAR(255),
  chunk_index INT,
  content TEXT NOT NULL,
  embedding vector(1024),
  metadata JSONB,
  sensitivity_tier INT,
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_embeddings_tenant ON embeddings(tenant_id);
CREATE INDEX IF NOT EXISTS idx_embeddings_vector ON embeddings USING ivfflat (embedding vector_cosine_ops);
CREATE INDEX IF NOT EXISTS idx_embeddings_metadata ON embeddings USING gin(metadata);
CREATE INDEX IF NOT EXISTS idx_embeddings_trgm ON embeddings USING gin(content gin_trgm_ops);

-- Configuration cache
CREATE TABLE IF NOT EXISTS config_cache (
  key VARCHAR(255) PRIMARY KEY,
  value JSONB NOT NULL,
  updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Rate limit tracking
CREATE TABLE IF NOT EXISTS rate_limits (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id VARCHAR(255) NOT NULL,
  endpoint VARCHAR(255),
  request_count INT DEFAULT 1,
  window_start TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  window_end TIMESTAMP,
  UNIQUE(tenant_id, endpoint, window_start)
);

CREATE INDEX IF NOT EXISTS idx_rate_limits_tenant ON rate_limits(tenant_id);

-- Tenant configuration
CREATE TABLE IF NOT EXISTS tenants (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id VARCHAR(255) NOT NULL UNIQUE,
  name VARCHAR(255),
  profile VARCHAR(50) DEFAULT 'cloud',
  created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
  updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_tenants_id ON tenants(tenant_id);

-- Add MCP tools management to prompt templates
ALTER TABLE nexus_prompt_templates ADD COLUMN IF NOT EXISTS mcp_tools_json JSONB DEFAULT '[]'::jsonb;
ALTER TABLE nexus_prompt_templates ADD COLUMN IF NOT EXISTS suggested_tools_json JSONB DEFAULT '[]'::jsonb;

-- Create table to track tool associations
CREATE TABLE IF NOT EXISTS prompt_mcp_tools (
    id SERIAL PRIMARY KEY,
    prompt_template_id INTEGER NOT NULL REFERENCES nexus_prompt_templates(id) ON DELETE CASCADE,
    tool_name VARCHAR(255) NOT NULL,
    tool_server VARCHAR(255) NOT NULL,
    usage_context TEXT,
    added_at TIMESTAMP DEFAULT NOW(),
    UNIQUE(prompt_template_id, tool_name, tool_server)
);

CREATE INDEX IF NOT EXISTS idx_prompt_mcp_tools_template ON prompt_mcp_tools(prompt_template_id);

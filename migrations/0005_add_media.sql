CREATE TABLE IF NOT EXISTS media (
    id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    content_type TEXT NOT NULL,
    content_length BIGINT NOT NULL,
    sha256_hash TEXT NOT NULL,
    uploaded_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (id, project_id)
);

CREATE INDEX IF NOT EXISTS idx_media_project_sha256 ON media (project_id, sha256_hash);

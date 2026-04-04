CREATE TABLE api_keys (
    key_hash   TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    created_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE gifs (
    id          TEXT PRIMARY KEY,
    filename    TEXT NOT NULL,
    hash        TEXT NOT NULL UNIQUE,
    frame_count INTEGER NOT NULL DEFAULT 1,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    uses        BIGINT NOT NULL DEFAULT 0,
    uploaded_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE gif_renditions (
    gif_id     TEXT REFERENCES gifs(id) ON DELETE CASCADE,
    rendition  TEXT NOT NULL,
    filename   TEXT NOT NULL,
    width      INTEGER NOT NULL,
    height     INTEGER NOT NULL,
    size_bytes INTEGER NOT NULL,
    PRIMARY KEY (gif_id, rendition)
);

CREATE TABLE tags (
    gif_id TEXT REFERENCES gifs(id) ON DELETE CASCADE,
    tag    TEXT NOT NULL,
    PRIMARY KEY (gif_id, tag)
);

CREATE INDEX idx_tags_tag         ON tags(tag);
CREATE INDEX idx_tags_tag_prefix  ON tags(tag text_pattern_ops);
CREATE INDEX idx_gifs_uses        ON gifs(uses DESC);
CREATE INDEX idx_gifs_uploaded_at ON gifs(uploaded_at DESC);

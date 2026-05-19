CREATE TABLE gifs (
    id          TEXT PRIMARY KEY,
    filename    TEXT NOT NULL,
    hash        TEXT NOT NULL UNIQUE,
    uploader_id TEXT NOT NULL,
    visibility  TEXT NOT NULL DEFAULT 'shared'
                CHECK (visibility IN ('shared', 'private')),
    is_nsfw     BOOLEAN NOT NULL DEFAULT FALSE,
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

CREATE TABLE favorites (
    mxid       TEXT NOT NULL,
    gif_id     TEXT NOT NULL REFERENCES gifs(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (mxid, gif_id)
);

CREATE TABLE selections (
    mxid    TEXT NOT NULL,
    gif_id  TEXT NOT NULL REFERENCES gifs(id) ON DELETE CASCADE,
    used_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (mxid, gif_id)
);

CREATE INDEX idx_favorites_mxid   ON favorites(mxid, created_at DESC);
CREATE INDEX idx_selections_mxid  ON selections(mxid, used_at DESC);
CREATE INDEX idx_tags_tag         ON tags(tag);
CREATE INDEX idx_tags_tag_prefix  ON tags(tag text_pattern_ops);
CREATE INDEX idx_gifs_uses        ON gifs(uses DESC);
CREATE INDEX idx_gifs_uploaded_at ON gifs(uploaded_at DESC);
CREATE INDEX idx_gifs_uploader    ON gifs(uploader_id);
CREATE INDEX idx_gifs_visibility  ON gifs(visibility);

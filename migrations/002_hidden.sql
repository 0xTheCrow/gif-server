CREATE TABLE hidden (
    mxid       TEXT NOT NULL,
    gif_id     TEXT NOT NULL REFERENCES gifs(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (mxid, gif_id)
);

CREATE INDEX idx_hidden_mxid ON hidden(mxid, created_at DESC);

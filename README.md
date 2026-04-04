# gif-server

A self-hosted GIF server with tag-based search, modeled after the Tenor API. Written in Rust using axum and Postgres.

## Features

- Upload GIFs with tags — automatically generates original, preview (220px), and thumbnail renditions
- Tag-based search with relevance ranking
- Filename full-text search fallback
- Autocomplete tag suggestions
- Featured (sorted by selections) and recent endpoints
- Cursor-based pagination
- File deduplication via SHA-256
- API key authentication
- Per-IP rate limiting (60 req/s, burst 30)

## Requirements

- [Rust](https://rustup.rs) 1.94+
- [Docker](https://docs.docker.com/get-docker/) with Docker Compose

## Local development

**1. Copy the example env file:**

```bash
cp .env.example .env
```

Edit `.env` if needed — the defaults work as-is for local development.

**2. Start Postgres:**

```bash
docker compose up -d db
```

**3. Run the server:**

```bash
cargo run
```

The server starts on `http://localhost:8847`.

**4. Add an API key:**

```bash
cargo run -- add-key myapp
```

This prints a key — save it. It is hashed before storage and cannot be retrieved again.

**5. Test it:**

```bash
curl -H "X-API-Key: <your-key>" http://localhost:8847/gifs/search
```

## Docker deployment

Builds the server into a container alongside Postgres:

```bash
docker compose up --build
```

After the first deploy, add an API key inside the container:

```bash
docker compose exec app gif-server add-key myapp
```

Rebuild after code changes:

```bash
docker compose up --build app
```

GIF files are stored in a named Docker volume (`gif_storage`) and persist across restarts.

## Configuration

All configuration is via environment variables (or `.env`):

| Variable | Default | Description |
|---|---|---|
| `DATABASE_URL` | — | Postgres connection string (required) |
| `STORAGE_PATH` | `storage` | Directory for GIF files on disk |
| `HOST` | `0.0.0.0` | Bind address |
| `PORT` | `8847` | Listen port |
| `BASE_URL` | `http://localhost:8847` | Public base URL used in rendition URLs |

Set `BASE_URL` to your public domain in production (e.g. `https://gifs.example.com`).

## API

All endpoints require an `X-API-Key` header.

### Upload

```
POST /gifs
Content-Type: multipart/form-data

Fields:
  file   — GIF file (required)
  tags   — comma-separated tags (optional, max 20, each max 100 chars)
```

Returns `201 Created` with GIF metadata. Returns `200 OK` if the file was already uploaded (deduplicated by content hash).

### Fetch

```
GET  /gifs/:id                        — metadata + rendition URLs
GET  /gifs/:id/file                   — serve file (?rendition=original|preview|thumbnail)
POST /gifs/:id/select                 — record a user selection (increments ranking)
DELETE /gifs/:id                      — delete GIF and all rendition files
```

### Search

```
GET /gifs/search?q=cat+funny&limit=20&pos=<cursor>   — search by tags / filename
GET /gifs/featured?limit=20&pos=<cursor>             — sorted by selection count
GET /gifs/recent?limit=20&pos=<cursor>               — sorted by upload date
GET /gifs/tags/suggest?q=ca&limit=10                 — tag autocomplete
```

`limit` defaults to 20, max 50. `pos` is a pagination cursor returned in the `next` field of list responses.

### Tags

```
PUT    /gifs/:id/tags   {"tags": ["a", "b"]}   — replace all tags
PATCH  /gifs/:id/tags   {"tags": ["c"]}        — add tags
DELETE /gifs/:id/tags   {"tags": ["a"]}        — remove specific tags
```

### Response shape

**GIF object:**
```json
{
  "id": "79789093-766a-4a3e-a8c8-aff34fcb5e1f",
  "filename": "animation.gif",
  "tags": ["nature", "birds"],
  "frame_count": 42,
  "duration_ms": 4200,
  "uses": 7,
  "uploaded_at": "2026-04-03T23:17:45.630134Z",
  "renditions": {
    "original":  { "url": "...", "width": 508, "height": 346, "size_bytes": 3067114 },
    "preview":   { "url": "...", "width": 220, "height": 149, "size_bytes": 1657739 },
    "thumbnail": { "url": "...", "width": 220, "height": 149, "size_bytes": 74973 }
  }
}
```

**List response:**
```json
{ "results": [ /* GIF objects */ ], "next": "<cursor | null>" }
```

**Error response:**
```json
{ "error": "not_found", "message": "not found" }
```

## Running tests

Unit tests (no database needed):

```bash
cargo test --lib
```

Integration tests (requires Postgres running):

```bash
docker compose up -d db
cargo test --test api
```

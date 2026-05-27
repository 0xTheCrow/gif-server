# gif-server

A self-hosted GIF server with tag-based search, modeled after the Tenor API. Written in Rust using axum and Postgres.

## Features

- Upload GIFs with tags — automatically generates original, preview (220px), and thumbnail renditions
- Tag-based search with relevance ranking
- Filename full-text search fallback
- Autocomplete tag suggestions
- Featured (sorted by selections) and recent endpoints
- Cursor-based pagination
- Per-uploader file deduplication via SHA-256
- Matrix identity auth — users authenticate with a Matrix OpenID token from a trusted homeserver
- Per-user uploads with shared/private visibility; owner + admin moderation
- Total storage size cap
- Per-user rate limiting; strict global limit on the unauthenticated auth endpoint
- Global and per-user storage quotas

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

The server starts on `http://localhost:8847`. Set the `MATRIX_*` and
`SESSION_SECRET` variables in `.env` before running (see Configuration).

**4. Authenticate and test it:**

Obtain a session token by posting a Matrix OpenID token (from
`mx.getOpenIdToken()` in a Matrix client) to `/auth/matrix`:

```bash
curl -X POST http://localhost:8847/auth/matrix \
  -H 'Content-Type: application/json' \
  -d '{"access_token":"<openid-token>","matrix_server_name":"example.com"}'
# => { "token": "<session-jwt>", "mxid": "@you:example.com", "expires_in": 3600, "is_admin": false }

curl -H "Authorization: Bearer <session-jwt>" http://localhost:8847/gifs/search
```

## Docker deployment

Builds the server into a container alongside Postgres:

```bash
docker compose up --build
```

Set the `MATRIX_*` and `SESSION_SECRET` environment variables in
`docker-compose.yml` (or via an env file) before deploying.

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
| `MATRIX_SERVER_NAME` | — | Trusted homeserver name, e.g. `example.com` (required) |
| `MATRIX_FEDERATION_URL` | — | Where the homeserver federation API is reachable for OpenID verification, e.g. `https://example.com:8448` (required) |
| `MATRIX_ADMIN_MXIDS` | _(empty)_ | Comma-separated admin Matrix IDs |
| `SESSION_SECRET` | — | HMAC secret for signing session tokens (required, min 32 chars) |
| `STORAGE_MAX_BYTES` | `10GB` | Max total rendition bytes across all users; suffix `KB`/`MB`/`GB`/`TB`, `0` disables |
| `PER_USER_STORAGE_BYTES` | `1GB` | Max rendition bytes per uploader; same suffixes, `0` disables |
| `CORS_ALLOWED_ORIGINS` | _(empty)_ | Comma-separated allowed browser origins (e.g. `https://app.example.com`). Empty allows any origin with a startup warning — set this in production |

Set `BASE_URL` to your public domain in production (e.g. `https://gifs.example.com`).

Only OpenID tokens issued by `MATRIX_SERVER_NAME`, for users whose mxid lives
on that server, are accepted — so registration gating on your homeserver
gates this server automatically. `MATRIX_FEDERATION_URL` must be reachable
from the gif server; if it's down, nobody can authenticate.

## Authentication

```
POST /auth/matrix
Content-Type: application/json

{ "access_token": "<matrix openid token>", "matrix_server_name": "example.com" }
```

Returns `{ "token": "<jwt>", "mxid": "...", "expires_in": 3600, "is_admin": false }`.
The token is verified against the homeserver's federation OpenID userinfo
endpoint. `is_admin` reflects whether the mxid is listed in `MATRIX_ADMIN_MXIDS`,
letting clients decide whether to surface admin UI. Send the returned JWT as
`Authorization: Bearer <jwt>` on every other endpoint; re-exchange a fresh
OpenID token when it expires.

## API

All endpoints except `POST /auth/matrix` and `GET /health` require an
`Authorization: Bearer <session-jwt>` header. `GET /health` is an
unauthenticated probe returning `200` if the database is reachable, else `503`.

Authenticated endpoints are rate-limited per Matrix user: ≈30 req/s (burst 60)
for API/mutation calls, ≈50 req/s (burst 200) for `GET /gifs/tags/suggest`
(autocomplete fires per keystroke), and a much looser ≈200 req/s (burst 1000)
for `GET /gifs/:id/file`, since one grid page fans out to many image fetches.
`POST /auth/matrix` has two layered limits: a global cap (≈5 req/s, burst 20)
bounds total backend load, and a per-IP cap (≈1 req/s, burst 5) prevents any
single attacker from draining the global bucket and denying login to others.
The per-IP key uses `X-Forwarded-For` / `X-Real-IP` / `Forwarded`, so the
server must be reached only through a trusted reverse proxy that sets these
headers. Exceeding any limit returns `429 Too Many Requests`.

A GIF is **shared** (visible to everyone) or **private** (visible only to its
uploader), and may be flagged **NSFW** (`is_nsfw`). NSFW GIFs are excluded
from all listings unless `grab_nsfw=true`, but are still returned by a direct
`GET /gifs/:id`. The uploader, or an admin acting on a shared GIF, can change
visibility, the NSFW flag, tags, or delete it. Admins never see or touch other
users' private GIFs.

### Upload

```
POST /gifs
Content-Type: multipart/form-data

Fields:
  file       — GIF file (required)
  tags       — comma-separated tags (optional, max 20, each max 100 chars)
  visibility — "shared" (default) or "private"
  nsfw       — "true"/"1"/"yes" to flag NSFW (optional, default false)
```

Returns `201 Created` with GIF metadata. Returns `200 OK` if **you** already
uploaded this file (dedup is per-uploader by content hash — another user
uploading the same bytes gets their own independent GIF, and cannot tell
whether anyone else has it). Returns `507 Insufficient Storage` if the upload
would exceed `STORAGE_MAX_BYTES` or your `PER_USER_STORAGE_BYTES`. Returns
`400` if the
GIF exceeds 4096×4096, 1000 frames, or 250M total pixels (width·height·frames)
— bounds that keep decoding cost bounded. Decoding/resizing runs off the async
runtime so a large upload can't stall other requests.

### Fetch

```
GET  /gifs/:id                        — metadata + rendition URLs
GET  /gifs/:id/file                   — serve file (?rendition=original|preview|thumbnail)
POST /gifs/:id/select                 — record a selection: adds the GIF to your history and bumps the shared featured ranking (counted at most once per user per GIF per 24h)
PUT    /gifs/:id/favorite             — add to your favorites (idempotent)
DELETE /gifs/:id/favorite             — remove from your favorites
PUT    /gifs/:id/hide                  — hide this GIF from your browsing (idempotent)
DELETE /gifs/:id/hide                  — un-hide this GIF
PATCH  /gifs/:id  {"visibility"?,"is_nsfw"?}  — update metadata; >=1 field required (uploader, or admin on shared)
DELETE /gifs/:id                      — delete GIF and all rendition files (uploader, or admin on shared)
```

GIFs you can't see (another user's private GIF) return `404`. Visible but
not yours returns `403` on mutating actions.

### Search

```
GET /gifs/search?q=cat+funny&limit=20&pos=<cursor>   — search by tags / filename
GET /gifs/featured?limit=20&pos=<cursor>             — sorted by selection count
GET /gifs/recent?limit=20&pos=<cursor>               — sorted by upload date
GET /gifs/favorites?limit=20&pos=<cursor>            — your favorited GIFs, newest-first
GET /gifs/hidden?limit=20&pos=<cursor>               — GIFs you've hidden, newest-first (to un-hide)
GET /gifs/history?limit=20&pos=<cursor>              — GIFs you've selected, most-recently-used first
GET /gifs/tags/suggest?q=ca&limit=10                 — tag autocomplete
```

`limit` defaults to 20, max 50. `pos` is a pagination cursor returned in the
`next` field of list responses. Add `&mine=true` to search/featured/recent to
restrict results to your own uploads. Add `&grab_nsfw=true` to include GIFs
flagged NSFW; by default they are excluded from all listings (unconditionally —
even from your own `&mine=true`, favorites, and history results). List/search
results always exclude other users' private GIFs.

GIFs you have hidden are excluded from `search`, `featured`, and `recent`
unless you pass `&grab_hidden=true`. Hiding is per-user and never affects what
anyone else sees. Hidden GIFs are still reachable by direct `GET /gifs/:id` and
are always listed by `GET /gifs/hidden` (regardless of `grab_hidden`) so you
can un-hide one undone by mistake.

`tags/suggest` only suggests tags that appear on a shared, non-NSFW GIF — tags
existing solely on private GIFs (anyone's, including yours) or NSFW GIFs are
never returned (pass `&grab_nsfw=true` to include NSFW-only tags). It is a
prefix autocomplete, not fuzzy matching.

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
  "uploader_id": "@you:example.com",
  "visibility": "shared",
  "is_nsfw": false,
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

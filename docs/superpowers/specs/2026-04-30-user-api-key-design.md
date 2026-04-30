# User Accounts and Per-User API Keys Design

## Goal

Add real user isolation and per-user API key storage so the app can be deployed as a multi-user service without sharing documents, audio, chunks, or provider credentials across users.

## Scope

Initial provider support is Gemini only. The schema and API should allow OpenAI and Claude later, but implementation should not add those providers yet.

## Environments

Production uses managed GCP services:

- Authentication: Google Login via OAuth/OIDC.
- Backend: Cloud Run.
- Metadata DB: Cloud SQL Postgres.
- PDF/audio storage: Cloud Storage.
- API key encryption: Cloud KMS.

Local development must not require GCP:

- Authentication: fixed development user from `DEV_USER_ID`, defaulting to `local-user`.
- Metadata DB: SQLite.
- PDF/audio storage: local `data/`.
- API key encryption: local symmetric encryption using `LOCAL_KEY_ENCRYPTION_SECRET`.
- `.env` provider keys may remain as a development fallback, but stored per-user keys take precedence.

## Authentication

Frontend obtains a Google ID token in production and sends it to the backend:

```text
Authorization: Bearer <id_token>
```

Backend validates the token and uses Google `sub` as `user_id`.

In local mode, backend skips token verification and resolves every request to `DEV_USER_ID`.

### First Login — Auto User Provisioning

On every authenticated request, the backend resolves the user as follows:

1. Extract `user_id` (Google `sub` in production, `DEV_USER_ID` in local).
2. Look up the user in the `users` table.
3. If not found, INSERT a new row using the ID token claims (`email`, `name` → `display_name`).
4. Proceed with the now-guaranteed user record.

This means the first request after signup automatically creates the account. There is no separate registration step.

## Data Isolation

Every user-owned record stores `user_id` where it is needed for direct, join-free queries. `chunks` omits `user_id` because access always goes through `document_id`, and the parent `documents` row already enforces ownership.

```text
users
- id            (Google sub in prod, DEV_USER_ID in local)
- email
- display_name
- created_at
- updated_at

documents
- user_id       (FK → users.id)
- id
- title
- file_name
- total_pages
- created_at

chunks
- id
- document_id   (FK → documents.id — ownership is derived via this join)
- title
- page_start
- page_end
- source_text
- key_points    (JSONB in Postgres, JSON text in SQLite)
- summary_text
- dialogue_script
- dialogue_turns (JSONB in Postgres, JSON text in SQLite)
- qa_context
- audio_path

api_keys
- user_id       (FK → users.id)
- provider      ("gemini" | "openai" | "claude")
- encrypted_api_key
- key_hint      (last 4 characters of the plaintext key)
- created_at
- updated_at
```

All document and audio endpoints must verify ownership by checking `documents.user_id = authenticated_user_id`. Chunk endpoints join to the parent document to verify ownership. If a record exists for another user, return `404`, not `403`, to avoid leaking IDs.

## Storage Paths

Storage paths include `user_id` to prevent cross-user collisions.

```text
production:
  gs://<bucket>/users/<user_id>/documents/<document_id>.pdf
  gs://<bucket>/users/<user_id>/audio/<chunk_id>.wav

local:
  data/users/<user_id>/documents/<document_id>.pdf
  data/users/<user_id>/audio/<chunk_id>.wav
```

### HTTP Audio Endpoint

The backend serves audio files under a user-scoped path:

```text
GET /audio/<user_id>/<chunk_id>.wav
```

`audio_path` stored in the database and returned to the frontend uses this same path, e.g. `/audio/local-user/abc123.wav`.

## API Key Storage

Frontend can submit an API key but can never read it back.

```text
GET    /api/me
GET    /api/me/api-keys
PUT    /api/me/api-keys/:provider
DELETE /api/me/api-keys/:provider
```

`PUT /api/me/api-keys/gemini` accepts:

```json
{ "api_key": "..." }
```

Backend stores only:

- encrypted API key
- provider name
- key hint (last 4 characters of plaintext key)
- timestamps

`GET /api/me` returns:

```json
{
  "id": "...",
  "email": "...",
  "display_name": "..."
}
```

`GET /api/me/api-keys` returns:

```json
[
  {
    "provider": "gemini",
    "configured": true,
    "key_hint": "...abcd"
  }
]
```

Generation resolves credentials in this order:

1. User-owned encrypted provider key.
2. Local development fallback from `.env`, only when `APP_ENV=local`.
3. Error: `Gemini API key is not configured for this user`.

## Encryption

Production encryption uses Cloud KMS. The Cloud Run service account is the only principal with decrypt permission. DB readers cannot decrypt API keys.

Local encryption uses AES-256-GCM with a key derived from `LOCAL_KEY_ENCRYPTION_SECRET`. If `LOCAL_KEY_ENCRYPTION_SECRET` is not set:

- The API key save/load endpoints return a clear error (`key storage is not configured`).
- The app **starts normally** and falls back to the `.env` key.
- A warning is logged at startup: `LOCAL_KEY_ENCRYPTION_SECRET not set — per-user API key storage disabled`.

This allows development without configuring encryption while still being able to test the full flow when needed.

API keys must never be logged, returned to frontend, included in errors, or written to generated files.

## Existing API Changes

Keep existing route shapes where possible:

```text
GET    /api/documents
POST   /api/documents
DELETE /api/documents/:id
POST   /api/documents/:id/generate
GET    /api/documents/:id/chunks
GET    /api/chunks/:id
POST   /api/chunks/:id/audio
```

Each handler receives an authenticated user context and applies `user_id` to storage and queries.

## UI

Add a settings area for API keys:

- Show whether Gemini key is configured.
- Show only key hint, never the full key.
- Allow replacing the key.
- Allow deleting the key.
- Disable generation when no key is available for the user and no `.env` fallback is active.

In local mode, the UI may show that an `.env` fallback key is active.

## Testing

Required tests:

- Local auth resolves `DEV_USER_ID`.
- Production auth rejects missing/invalid tokens.
- Document list only returns current user's documents.
- Chunk access returns `404` for another user's chunk.
- API key save stores encrypted data and key hint.
- API key read never returns plaintext.
- Generation uses user key before `.env` fallback.
- Local encryption round-trips with `LOCAL_KEY_ENCRYPTION_SECRET`.
- Auto-provisioning creates a user record on first authenticated request.

## Migration

Current local `data/documents` and `data/chunks` are single-user data. For local migration, assign existing records to `DEV_USER_ID` (default `local-user`) and move files to `data/users/local-user/`. Production can start empty.

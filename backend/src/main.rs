mod auth;
mod db;
mod models;

use std::{
    collections::HashMap,
    env, fs,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    process::Command,
    sync::{Arc, RwLock},
};

use axum::{
    Json, Router,
    extract::DefaultBodyLimit,
    extract::Multipart,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
};
use base64::{Engine as _, engine::general_purpose};
use chrono::Utc;
use models::{
    ApiKeyInfo, BookChunk, ChunkListItem, CreatedDocumentResponse, Document, GenerateAudioResponse,
    GenerateDocumentResponse, User,
};
use reqwest::Client;
use serde::Deserialize;
use sqlx::SqlitePool;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::info;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    store: Arc<RwLock<Store>>,
    data_dir: PathBuf,
    http_client: Client,
    voicevox_base_url: String,
    gemini_config: Option<GeminiConfig>,
    db: SqlitePool,
    app_env: String,
    dev_user_id: String,
}

#[derive(Clone)]
struct GeminiConfig {
    api_key: String,
    fallback_models: Vec<String>,
}

#[derive(Default)]
struct Store {
    documents: HashMap<String, Document>,
    chunks: HashMap<String, BookChunk>,
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::from_filename("../.env").or_else(|_| dotenvy::dotenv());

    tracing_subscriber::fmt()
        .with_env_filter("personal_news_backend=debug,tower_http=debug")
        .init();

    let app_env = env::var("APP_ENV").unwrap_or_else(|_| "local".to_string());
    let dev_user_id =
        env::var("DEV_USER_ID").unwrap_or_else(|_| "local-user".to_string());

    // Ensure data directory exists before connecting to SQLite
    fs::create_dir_all("../data").expect("failed to create data directory");

    let db_path = env::var("DATABASE_PATH")
        .unwrap_or_else(|_| "../data/db.sqlite".to_string());
    let db = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&db_path)
                .create_if_missing(true),
        )
        .await
        .expect("failed to connect to database");
    sqlx::migrate!("./migrations")
        .run(&db)
        .await
        .expect("failed to run database migrations");

    let data_dir = init_data_dirs(&dev_user_id);
    migrate_legacy_data(&data_dir, &dev_user_id);

    let state = AppState {
        store: Arc::new(RwLock::new(load_store(&data_dir, &dev_user_id))),
        data_dir: data_dir.clone(),
        http_client: Client::new(),
        voicevox_base_url: env::var("VOICEVOX_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:50021".to_string()),
        gemini_config: build_gemini_config(),
        db,
        app_env,
        dev_user_id,
    };

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/me", get(get_me))
        .route("/api/me/api-keys", get(list_api_keys))
        .route("/api/me/api-keys/{provider}", put(put_api_key).delete(delete_api_key_handler))
        .route("/api/documents", get(list_documents).post(create_document))
        .route("/api/documents/{id}", delete(delete_document))
        .route("/api/documents/{id}/generate", post(generate_document))
        .route("/api/documents/{id}/chunks", get(list_document_chunks))
        .route("/api/chunks/{id}", get(get_chunk))
        .route("/api/chunks/{id}/audio", post(generate_audio))
        .route("/audio/{user_id}/{chunk_id}", get(serve_audio))
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let port: u16 = env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("backend listening on http://{addr}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind TCP listener");

    axum::serve(listener, app)
        .await
        .expect("failed to serve axum app");
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "service": "pdf-reading-radio-backend"
    }))
}

async fn get_me(
    auth: auth::AuthUser,
    State(_state): State<AppState>,
) -> impl IntoResponse {
    Json(User {
        id: auth.user_id,
        email: auth.email,
        display_name: auth.display_name,
    })
}

async fn list_api_keys(
    auth: auth::AuthUser,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let mut keys = db::list_api_keys(&state.db, &auth.user_id)
        .await
        .map_err(|_| AppError::internal("failed to list api keys"))?;

    // If .env fallback is active and no stored key, show it as a hint
    if state.app_env == "local" {
        if !keys.iter().any(|k| k.provider == "gemini") {
            if let Some(ref cfg) = state.gemini_config {
                let hint = cfg.api_key.chars().rev().take(4).collect::<String>()
                    .chars().rev().collect::<String>();
                keys.push(ApiKeyInfo {
                    provider: "gemini".to_string(),
                    configured: true,
                    key_hint: format!("…{hint} (.env)"),
                });
            }
        }
    }

    Ok(Json(keys))
}

#[derive(Deserialize)]
struct PutApiKeyBody {
    api_key: String,
}

async fn put_api_key(
    auth: auth::AuthUser,
    Path(provider): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<PutApiKeyBody>,
) -> Result<StatusCode, AppError> {
    let key_hint = {
        let last4: String = body.api_key.chars().rev().take(4).collect::<String>()
            .chars().rev().collect();
        format!("…{last4}")
    };

    db::save_api_key(&state.db, &auth.user_id, &provider, &body.api_key, &key_hint)
        .await
        .map_err(|_| AppError::internal("failed to save api key"))?;

    Ok(StatusCode::NO_CONTENT)
}

async fn delete_api_key_handler(
    auth: auth::AuthUser,
    Path(provider): Path<String>,
    State(state): State<AppState>,
) -> Result<StatusCode, AppError> {
    let deleted = db::delete_api_key(&state.db, &auth.user_id, &provider)
        .await
        .map_err(|_| AppError::internal("failed to delete api key"))?;

    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::not_found("api key not found"))
    }
}

async fn serve_audio(
    Path((uid, chunk_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let path = state.data_dir.join("users").join(&uid).join("audio").join(&chunk_id);
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            StatusCode::OK,
            [("content-type", "audio/wav"), ("cache-control", "public, max-age=86400")],
            bytes,
        ).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn list_documents(
    auth: auth::AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<Document>>, AppError> {
    let store = state
        .store
        .read()
        .map_err(|_| AppError::internal("failed to read document store"))?;

    let mut documents: Vec<_> = store
        .documents
        .values()
        .filter(|d| d.user_id == auth.user_id)
        .cloned()
        .collect();
    documents.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(Json(documents))
}

async fn delete_document(
    auth: auth::AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<StatusCode, AppError> {
    let (owner_user_id, chunk_ids_and_audio): (String, Vec<(String, Option<String>)>) = {
        let store = state
            .store
            .read()
            .map_err(|_| AppError::internal("failed to read store"))?;
        let doc = store
            .documents
            .get(&id)
            .ok_or_else(|| AppError::not_found("document not found"))?;
        if doc.user_id != auth.user_id {
            return Err(AppError::not_found("document not found"));
        }
        let chunks = store
            .chunks
            .values()
            .filter(|c| c.document_id == id)
            .map(|c| (c.id.clone(), c.audio_path.clone()))
            .collect();
        (doc.user_id.clone(), chunks)
    };

    {
        let mut store = state
            .store
            .write()
            .map_err(|_| AppError::internal("failed to write store"))?;
        store.documents.remove(&id);
        for (chunk_id, _) in &chunk_ids_and_audio {
            store.chunks.remove(chunk_id);
        }
    }

    let udir = user_dir(&state.data_dir, &owner_user_id);
    let _ = fs::remove_file(udir.join("documents").join(format!("{id}.json")));
    let _ = fs::remove_file(udir.join("documents").join(format!("{id}.pdf")));

    for (chunk_id, audio_path) in chunk_ids_and_audio {
        let _ = fs::remove_file(udir.join("chunks").join(format!("{chunk_id}.json")));
        if let Some(path) = audio_path {
            // path is like /audio/{user_id}/{chunk_id}.wav — extract filename
            if let Some(file_name) = FsPath::new(&path).file_name() {
                let _ = fs::remove_file(udir.join("audio").join(file_name));
            }
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn create_document(
    auth: auth::AuthUser,
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<CreatedDocumentResponse>, AppError> {
    let mut file_name = None;
    let mut file_bytes = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| AppError::bad_request("failed to read multipart payload"))?
    {
        if field.name() != Some("file") {
            continue;
        }

        file_name = field.file_name().map(str::to_string);
        file_bytes = Some(
            field
                .bytes()
                .await
                .map_err(|_| AppError::bad_request("failed to read uploaded file"))?,
        );
        break;
    }

    let file_name = file_name.ok_or_else(|| AppError::bad_request("missing file field"))?;
    let file_bytes = file_bytes.ok_or_else(|| AppError::bad_request("missing file field"))?;

    if !file_name.to_lowercase().ends_with(".pdf") {
        return Err(AppError::bad_request("uploaded file must be a PDF"));
    }

    let document_id = Uuid::new_v4().to_string();
    let doc_dir = user_dir(&state.data_dir, &auth.user_id).join("documents");
    fs::create_dir_all(&doc_dir).map_err(|_| AppError::internal("failed to create document dir"))?;
    let pdf_path = doc_dir.join(format!("{document_id}.pdf"));

    tokio::fs::write(&pdf_path, file_bytes)
        .await
        .map_err(|_| AppError::internal("failed to store uploaded PDF"))?;

    let page_count = extract_page_count(&pdf_path).await?;
    let document = Document {
        id: document_id.clone(),
        user_id: auth.user_id.clone(),
        title: file_stem_or_name(&file_name),
        file_name,
        total_pages: page_count,
        created_at: Utc::now(),
    };

    let chunks = initial_chunks_for_upload();

    {
        let mut store = state
            .store
            .write()
            .map_err(|_| AppError::internal("failed to update document store"))?;
        store
            .documents
            .insert(document.id.clone(), document.clone());
        for chunk in &chunks {
            store.chunks.insert(chunk.id.clone(), chunk.clone());
        }
    }

    persist_document(&state.data_dir, &document)?;
    for chunk in &chunks {
        persist_chunk(&state.data_dir, chunk, &document.user_id)?;
    }

    let response = CreatedDocumentResponse {
        document,
        chunks: chunks.iter().map(ChunkListItem::from).collect(),
    };

    Ok(Json(response))
}

async fn list_document_chunks(
    auth: auth::AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<Vec<ChunkListItem>>, AppError> {
    let store = state
        .store
        .read()
        .map_err(|_| AppError::internal("failed to read chunk store"))?;

    let doc = store
        .documents
        .get(&id)
        .ok_or_else(|| AppError::not_found("document not found"))?;
    if doc.user_id != auth.user_id {
        return Err(AppError::not_found("document not found"));
    }

    let chunks = store
        .chunks
        .values()
        .filter(|chunk| chunk.document_id == id)
        .map(ChunkListItem::from)
        .collect::<Vec<_>>();
    let mut chunks = chunks;
    chunks.sort_by(|a, b| {
        a.page_start
            .cmp(&b.page_start)
            .then_with(|| a.page_end.cmp(&b.page_end))
    });

    Ok(Json(chunks))
}

async fn get_chunk(
    auth: auth::AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<BookChunk>, AppError> {
    let store = state
        .store
        .read()
        .map_err(|_| AppError::internal("failed to read chunk store"))?;

    let chunk = store
        .chunks
        .get(&id)
        .cloned()
        .ok_or_else(|| AppError::not_found("chunk not found"))?;

    // ownership check via parent document
    let doc = store.documents.get(&chunk.document_id);
    if doc.map(|d| d.user_id.as_str()) != Some(auth.user_id.as_str()) {
        return Err(AppError::not_found("chunk not found"));
    }

    Ok(Json(chunk))
}

#[derive(Debug, Deserialize)]
struct GenerateQuery {
    model: Option<String>,
}

async fn generate_document(
    auth: auth::AuthUser,
    Path(id): Path<String>,
    Query(query): Query<GenerateQuery>,
    State(state): State<AppState>,
) -> Result<Json<GenerateDocumentResponse>, AppError> {
    let document = {
        let store = state
            .store
            .read()
            .map_err(|_| AppError::internal("failed to read document store"))?;
        let doc = store
            .documents
            .get(&id)
            .cloned()
            .ok_or_else(|| AppError::not_found("document not found"))?;
        if doc.user_id != auth.user_id {
            return Err(AppError::not_found("document not found"));
        }
        doc
    };

    // Resolve Gemini API key: user key > .env fallback (local only)
    let mut config = resolve_gemini_config(&state, &auth.user_id).await?;
    if let Some(model) = query.model {
        config.fallback_models = vec![normalize_gemini_model(&model).to_string()];
    }
    let pdf_path = user_dir(&state.data_dir, &document.user_id)
        .join("documents")
        .join(format!("{}.pdf", document.id));
    let (chunks, model) =
        generate_chunks_with_gemini(&state.http_client, &config, &document, &pdf_path).await?;

    replace_document_chunks(&state, &document.id, &document.user_id, &chunks)?;

    Ok(Json(GenerateDocumentResponse {
        document,
        chunks,
        model,
    }))
}

async fn generate_audio(
    auth: auth::AuthUser,
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<GenerateAudioResponse>, AppError> {
    let chunk = {
        let store = state
            .store
            .read()
            .map_err(|_| AppError::internal("failed to read chunk store"))?;
        let chunk = store
            .chunks
            .get(id.as_str())
            .cloned()
            .ok_or_else(|| AppError::not_found("chunk not found"))?;
        let doc = store.documents.get(&chunk.document_id);
        if doc.map(|d| d.user_id.as_str()) != Some(auth.user_id.as_str()) {
            return Err(AppError::not_found("chunk not found"));
        }
        chunk
    };
    let updated_chunk = synthesize_chunk_audio(&state, chunk, &auth.user_id).await?;
    let audio_url = updated_chunk
        .audio_path
        .clone()
        .ok_or_else(|| AppError::internal("audio path was not set after synthesis"))?;

    Ok(Json(GenerateAudioResponse {
        chunk: updated_chunk,
        audio_url,
    }))
}

fn init_data_dirs(dev_user_id: &str) -> PathBuf {
    let data_dir = PathBuf::from("../data");
    let user_dir = data_dir.join("users").join(dev_user_id);
    for name in ["documents", "chunks", "audio"] {
        fs::create_dir_all(user_dir.join(name)).expect("failed to create data directories");
    }
    data_dir
}

fn migrate_legacy_data(data_dir: &FsPath, dev_user_id: &str) {
    let user_dir = data_dir.join("users").join(dev_user_id);
    for name in ["documents", "chunks", "audio"] {
        let old_dir = data_dir.join(name);
        let new_dir = user_dir.join(name);
        let Ok(entries) = fs::read_dir(&old_dir) else { continue };
        for entry in entries.flatten() {
            let old_path = entry.path();
            let Some(file_name) = old_path.file_name() else { continue };
            let new_path = new_dir.join(file_name);
            if !new_path.exists() {
                if let Err(e) = fs::rename(&old_path, &new_path) {
                    tracing::warn!("failed to migrate {}: {e}", old_path.display());
                }
            }
        }
    }
}

fn build_gemini_config() -> Option<GeminiConfig> {
    env::var("GEMINI_API_KEY").ok().map(|api_key| {
        let model = env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".to_string());
        GeminiConfig {
            fallback_models: gemini_model_candidates(&model),
            api_key,
        }
    })
}

fn initial_chunks_for_upload() -> Vec<BookChunk> {
    Vec::new()
}

fn gemini_model_candidates(primary_model: &str) -> Vec<String> {
    let mut models = vec![normalize_gemini_model(primary_model).to_string()];
    let env_fallbacks = env::var("GEMINI_FALLBACK_MODELS")
        .unwrap_or_else(|_| "gemini-2.5-flash-lite,gemini-2.5-pro".to_string());

    for model in env_fallbacks
        .split(',')
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        let model = normalize_gemini_model(model);
        if !models.iter().any(|candidate| candidate == model) {
            models.push(model.to_string());
        }
    }

    models
}

fn normalize_gemini_model(model: &str) -> &str {
    match model.trim() {
        "gemini-1.5-flash" | "models/gemini-1.5-flash" => "gemini-2.5-flash-lite",
        "gemini-1.5-pro" | "models/gemini-1.5-pro" => "gemini-2.5-pro",
        value => value,
    }
}

fn load_store(data_dir: &FsPath, dev_user_id: &str) -> Store {
    let mut documents = HashMap::new();
    let mut chunks = HashMap::new();

    let user_dir = data_dir.join("users").join(dev_user_id);

    let documents_dir = user_dir.join("documents");
    if let Ok(entries) = fs::read_dir(&documents_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            match fs::read_to_string(&path)
                .ok()
                .and_then(|contents| serde_json::from_str::<Document>(&contents).ok())
            {
                Some(mut document) => {
                    if document.user_id.is_empty() {
                        document.user_id = dev_user_id.to_string();
                    }
                    documents.insert(document.id.clone(), document);
                }
                None => {
                    tracing::warn!("failed to load document from {}", path.display());
                }
            }
        }
    }

    let chunks_dir = user_dir.join("chunks");
    if let Ok(entries) = fs::read_dir(&chunks_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            match fs::read_to_string(&path)
                .ok()
                .and_then(|contents| serde_json::from_str::<BookChunk>(&contents).ok())
            {
                Some(chunk) => {
                    chunks.insert(chunk.id.clone(), chunk);
                }
                None => {
                    tracing::warn!("failed to load chunk from {}", path.display());
                }
            }
        }
    }

    Store { documents, chunks }
}

fn user_dir(data_dir: &FsPath, user_id: &str) -> PathBuf {
    data_dir.join("users").join(user_id)
}

fn persist_document(data_dir: &FsPath, document: &Document) -> Result<(), AppError> {
    let dir = user_dir(data_dir, &document.user_id).join("documents");
    fs::create_dir_all(&dir).map_err(|_| AppError::internal("failed to create document dir"))?;
    let json = serde_json::to_string_pretty(document)
        .map_err(|_| AppError::internal("failed to serialize document metadata"))?;
    fs::write(dir.join(format!("{}.json", document.id)), json)
        .map_err(|_| AppError::internal("failed to persist document metadata"))
}

fn persist_chunk(data_dir: &FsPath, chunk: &BookChunk, user_id: &str) -> Result<(), AppError> {
    let dir = user_dir(data_dir, user_id).join("chunks");
    fs::create_dir_all(&dir).map_err(|_| AppError::internal("failed to create chunk dir"))?;
    let json = serde_json::to_string_pretty(chunk)
        .map_err(|_| AppError::internal("failed to serialize chunk metadata"))?;
    fs::write(dir.join(format!("{}.json", chunk.id)), json)
        .map_err(|_| AppError::internal("failed to persist chunk metadata"))
}

fn replace_document_chunks(
    state: &AppState,
    document_id: &str,
    user_id: &str,
    chunks: &[BookChunk],
) -> Result<(), AppError> {
    let old_chunk_ids = {
        let mut store = state
            .store
            .write()
            .map_err(|_| AppError::internal("failed to update chunk store"))?;
        let old_chunk_ids = store
            .chunks
            .values()
            .filter(|chunk| chunk.document_id == document_id)
            .map(|chunk| chunk.id.clone())
            .collect::<Vec<_>>();

        for chunk_id in &old_chunk_ids {
            store.chunks.remove(chunk_id);
        }
        for chunk in chunks {
            store.chunks.insert(chunk.id.clone(), chunk.clone());
        }
        old_chunk_ids
    };

    let chunks_dir = user_dir(&state.data_dir, user_id).join("chunks");
    for chunk_id in old_chunk_ids {
        let path = chunks_dir.join(format!("{chunk_id}.json"));
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(AppError::internal("failed to remove old chunk metadata"));
        }
    }

    for chunk in chunks {
        persist_chunk(&state.data_dir, chunk, user_id)?;
    }

    Ok(())
}

async fn resolve_gemini_config(
    state: &AppState,
    user_id: &str,
) -> Result<GeminiConfig, AppError> {
    // 1. User's stored key (plaintext; production will use KMS here)
    if let Ok(Some(api_key)) = db::get_api_key(&state.db, user_id, "gemini").await {
        let model = env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-2.0-flash".to_string());
        return Ok(GeminiConfig { api_key, fallback_models: gemini_model_candidates(&model) });
    }

    // 2. .env fallback (local only)
    state
        .gemini_config
        .clone()
        .ok_or_else(|| AppError::bad_request("Gemini API key is not configured for this user"))
}

#[derive(Debug, Deserialize)]
struct GeminiChunks {
    chunks: Vec<GeminiChunk>,
}

#[derive(Debug, Deserialize)]
struct GeminiDialogueTurn {
    speaker: String,
    text: String,
}

#[derive(Debug, Deserialize)]
struct GeminiChunk {
    title: String,
    page_start: u32,
    page_end: u32,
    source_text: String,
    key_points: Vec<String>,
    summary_text: String,
    #[serde(default)]
    dialogue_script: String,
    #[serde(default)]
    dialogue_turns: Vec<GeminiDialogueTurn>,
    qa_context: String,
}

#[derive(Debug, Deserialize)]
struct GeminiGenerateContentResponse {
    candidates: Vec<GeminiCandidate>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    content: GeminiContent,
}

#[derive(Debug, Deserialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Deserialize)]
struct GeminiPart {
    text: Option<String>,
}

fn parse_gemini_chunks(raw: &str) -> Result<GeminiChunks, AppError> {
    let json = extract_json_object(raw)?;
    serde_json::from_str::<GeminiChunks>(&json)
        .map_err(|_| AppError::internal("failed to parse Gemini chunk JSON"))
}

fn extract_json_object(raw: &str) -> Result<String, AppError> {
    let trimmed = raw.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Ok(trimmed.to_string());
    }

    let fenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim);

    if let Some(fenced) = fenced
        && fenced.starts_with('{')
        && fenced.ends_with('}')
    {
        return Ok(fenced.to_string());
    }

    let start = trimmed
        .find('{')
        .ok_or_else(|| AppError::internal("Gemini output did not include JSON"))?;
    let end = trimmed
        .rfind('}')
        .ok_or_else(|| AppError::internal("Gemini output did not include JSON"))?;

    Ok(trimmed[start..=end].to_string())
}

async fn generate_chunks_with_gemini(
    http_client: &Client,
    config: &GeminiConfig,
    document: &Document,
    pdf_path: &FsPath,
) -> Result<(Vec<BookChunk>, String), AppError> {
    let pdf_bytes = tokio::fs::read(pdf_path)
        .await
        .map_err(|_| AppError::internal("failed to read stored PDF"))?;
    let pdf_base64 = general_purpose::STANDARD.encode(pdf_bytes);
    let prompt = build_gemini_processing_prompt(document);
    let mut last_error = None;
    let mut response = None;
    let mut used_model = None;

    for model in &config.fallback_models {
        match call_gemini_generate_content(
            http_client,
            &config.api_key,
            model,
            &pdf_base64,
            &prompt,
        )
        .await
        {
            Ok(value) => {
                response = Some(value);
                used_model = Some(model.clone());
                break;
            }
            Err(error) => {
                last_error = Some(error);
            }
        }
    }

    let response = response.ok_or_else(|| {
        last_error.unwrap_or_else(|| AppError::internal("Gemini API did not return a response"))
    })?;
    let used_model =
        used_model.ok_or_else(|| AppError::internal("Gemini model selection failed"))?;
    let text = response
        .candidates
        .first()
        .and_then(|candidate| {
            candidate
                .content
                .parts
                .iter()
                .find_map(|part| part.text.as_deref())
        })
        .ok_or_else(|| AppError::internal("Gemini response did not include text"))?;
    let parsed = parse_gemini_chunks(text)?;

    let mut chunks = parsed
        .chunks
        .into_iter()
        .map(|chunk| {
            use models::DialogueTurn;
            let dialogue_turns: Vec<DialogueTurn> = chunk
                .dialogue_turns
                .into_iter()
                .map(|t| DialogueTurn {
                    speaker: t.speaker,
                    text: t.text,
                })
                .collect();
            BookChunk {
                id: Uuid::new_v4().to_string(),
                document_id: document.id.clone(),
                title: chunk.title,
                page_start: chunk.page_start,
                page_end: chunk.page_end,
                source_text: chunk.source_text,
                key_points: chunk.key_points,
                summary_text: chunk.summary_text,
                dialogue_script: chunk.dialogue_script,
                dialogue_turns,
                qa_context: chunk.qa_context,
                audio_path: None,
            }
        })
        .collect::<Vec<_>>();

    chunks.sort_by(|a, b| {
        a.page_start
            .cmp(&b.page_start)
            .then_with(|| a.page_end.cmp(&b.page_end))
    });

    Ok((chunks, used_model))
}

async fn call_gemini_generate_content(
    http_client: &Client,
    api_key: &str,
    model: &str,
    pdf_base64: &str,
    prompt: &str,
) -> Result<GeminiGenerateContentResponse, AppError> {
    let url =
        format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent");
    let body = serde_json::json!({
        "contents": [{
            "parts": [
                {
                    "inline_data": {
                        "mime_type": "application/pdf",
                        "data": pdf_base64
                    }
                },
                {
                    "text": prompt
                }
            ]
        }],
        "generation_config": {
            "response_mime_type": "application/json"
        }
    });

    let response = http_client
        .post(url)
        .header("x-goog-api-key", api_key)
        .json(&body)
        .send()
        .await
        .map_err(|_| AppError::internal(format!("failed to call Gemini API model {model}")))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|_| AppError::internal("failed to read Gemini API response"))?;

    if !status.is_success() {
        return Err(AppError::internal(format!(
            "Gemini API model {model} failed with status {status}: {}",
            preview_text(&body, 600)
        )));
    }

    serde_json::from_str::<GeminiGenerateContentResponse>(&body)
        .map_err(|_| AppError::internal("failed to parse Gemini API response"))
}

fn build_gemini_processing_prompt(document: &Document) -> String {
    format!(
        "あなたは「ずんだもんラジオ」の台本ライターです。添付されたPDFを読み込み、10〜15分のラジオ番組台本を1本生成してください。\n\
\n\
【出演者】\n\
ずんだもん：元気いっぱいで好奇心旺盛。大学生程度の教養と理系基礎知識を持っている。語尾に「〜なのだ」「〜のだ」を自然に混ぜる。難しい内容を「例えると」「ざっくり言うと」で噛み砕く。\n\
四国めたん：知的でちょっとツンデレ。大学生程度の教養と理系基礎知識を持っている。語尾に「〜わ」「〜かしら」「〜ね」を自然に混ぜる。ずんだもんの解説に補足やツッコミを入れる。\n\
\n\
【番組の雰囲気】\n\
「この論文、何の役に立つの？」「だから何がすごいの？」を中心に掛け合いで進める。学術的な正確さよりも「聞いていて面白い」を最優先にする。例え話や身近なたとえを積極的に使う。テンポよくボケとツッコミが入る。技術的な詳細は最小限に抑え、「なぜ面白いか」「世界が変わるとしたら何が？」を優先する。\n\
\n\
【PDF外の補足知識】\n\
dialogue_turnsでは、PDFの理解に必要な範囲で一般的な大学生レベルの背景知識を補ってよい。専門用語の前提、歴史的背景、似た概念、身近な例、なぜその分野で重要かを自然に説明する。ただし、PDFに書かれている主張と外部知識を混同しない。「PDFではこう言っている」「背景としてはこう考えると分かりやすい」のように区別する。PDFにない新事実を論文の主張として断定しない。\n\
\n\
【出力形式】\n\
以下のJSON形式のみで返答すること。それ以外は出力しない：\n\
{{\"chunks\":[{{\"title\":\"...\",\"page_start\":1,\"page_end\":{total_pages},\"source_text\":\"...\",\"key_points\":[\"...\"],\"summary_text\":\"...\",\"dialogue_turns\":[{{\"speaker\":\"zundamon\",\"text\":\"...\"}},{{\"speaker\":\"metan\",\"text\":\"...\"}}],\"qa_context\":\"...\"}}]}}\n\
\n\
【チャンク構成】\n\
chunksは1つだけ生成する。page_start=1、page_end={total_pages}。titleはラジオ番組らしい日本語タイトルにする。\n\
\n\
【dialogue_turnsの書き方】\n\
speakerは「zundamon」または「metan」のみ使う。ずんだもんから始め、交互に20〜35ターン生成する。各ターンは1〜3文、自然な会話の流れにする。\n\
冒頭はずんだもんの「ハイどうもー！ずんだもんのペーパーラジオへようこそなのだ！今日は〜をやっていくのだ！」から始める。\n\
めたんの最初の反応は「また変な論文持ってきたわね」などの軽いツッコミにする。\n\
本編は「この論文の一番すごいところって何？」から始め、重要なポイントを掛け合いで紹介する。\n\
全体の文字数は3000〜4500文字を目安にする。\n\
締めはずんだもんの「今日のペーパーラジオはここまでなのだ！また次回もよろしくなのだ！」で終わる。\n\
\n\
【VOICEVOX制約】\n\
記号・絵文字・箇条書き・Markdown・数式・英数字の羅列は使わない。数式は言葉で説明する。役名・ト書き・演出指示は書かない。JSON以外の説明やMarkdown fenceは出力しない。\n\
\n\
【その他フィールド】\n\
source_text：PDFの主要な内容をまとめたメモ（Q&A用）。key_points：3〜5個、このPDFの重要なポイント。summary_text：3〜5文でPDF全体の概要。qa_context：Q&A用の事実メモ。qa_contextにはPDF由来の事実と、理解補助の一般背景知識を分けて書く。PDFにない新事実をPDFの主張として扱わない。\n\
\n\
Document title: {title}\n\
Original file name: {file_name}\n\
Total pages: {total_pages}",
        title = document.title,
        file_name = document.file_name,
        total_pages = document.total_pages
    )
}

fn speaker_id(speaker: &str) -> u32 {
    match speaker {
        "metan" => 2,
        _ => 3, // zundamon default
    }
}

fn extract_wav_pcm(wav: &[u8]) -> &[u8] {
    // Find "data" chunk marker and return the raw PCM bytes
    for i in 0..wav.len().saturating_sub(8) {
        if wav[i..i + 4] == *b"data" {
            let data_size =
                u32::from_le_bytes(wav[i + 4..i + 8].try_into().unwrap_or([0; 4])) as usize;
            let start = i + 8;
            let end = (start + data_size).min(wav.len());
            return &wav[start..end];
        }
    }
    &[]
}

fn build_wav(pcm: &[u8]) -> Vec<u8> {
    // VOICEVOX outputs 24000 Hz, 16-bit, mono
    let sample_rate: u32 = 24000;
    let channels: u16 = 1;
    let bits: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * bits as u32 / 8;
    let block_align = channels * bits / 8;
    let data_size = pcm.len() as u32;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(data_size + 36).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_size.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

fn concatenate_wavs(wavs: &[Vec<u8>], pause_ms: u32) -> Vec<u8> {
    // 400ms silence at 24000 Hz 16-bit mono = 24000 * 0.4 * 2 bytes
    let silence_bytes = (24000 * pause_ms / 1000 * 2) as usize;
    let silence = vec![0u8; silence_bytes];
    let mut pcm = Vec::new();
    for (i, wav) in wavs.iter().enumerate() {
        if i > 0 {
            pcm.extend_from_slice(&silence);
        }
        pcm.extend_from_slice(extract_wav_pcm(wav));
    }
    build_wav(&pcm)
}

async fn synthesize_chunk_audio(
    state: &AppState,
    chunk: BookChunk,
    user_id: &str,
) -> Result<BookChunk, AppError> {
    let audio_bytes = if !chunk.dialogue_turns.is_empty() {
        let mut turn_wavs = Vec::new();
        for turn in &chunk.dialogue_turns {
            if turn.text.trim().is_empty() {
                continue;
            }
            let wav = synthesize_with_voicevox(
                &state.http_client,
                &state.voicevox_base_url,
                &turn.text,
                speaker_id(&turn.speaker),
            )
            .await?;
            turn_wavs.push(wav);
        }
        if turn_wavs.is_empty() {
            return Err(AppError::bad_request("dialogue_turns are all empty"));
        }
        concatenate_wavs(&turn_wavs, 400)
    } else if !chunk.dialogue_script.trim().is_empty() {
        synthesize_with_voicevox(
            &state.http_client,
            &state.voicevox_base_url,
            &chunk.dialogue_script,
            3,
        )
        .await?
    } else {
        return Err(AppError::bad_request("no dialogue content to synthesize"));
    };

    let file_name = format!("{}.wav", chunk.id);
    let audio_dir = user_dir(&state.data_dir, user_id).join("audio");
    fs::create_dir_all(&audio_dir).map_err(|_| AppError::internal("failed to create audio dir"))?;
    tokio::fs::write(audio_dir.join(&file_name), audio_bytes)
        .await
        .map_err(|_| AppError::internal("failed to write synthesized audio"))?;

    let updated_chunk = BookChunk {
        audio_path: Some(format!("/audio/{user_id}/{file_name}")),
        ..chunk
    };

    {
        let mut store = state
            .store
            .write()
            .map_err(|_| AppError::internal("failed to update chunk store"))?;
        store
            .chunks
            .insert(updated_chunk.id.clone(), updated_chunk.clone());
    }

    persist_chunk(&state.data_dir, &updated_chunk, user_id)?;

    Ok(updated_chunk)
}

async fn synthesize_with_voicevox(
    http_client: &Client,
    base_url: &str,
    text: &str,
    speaker: u32,
) -> Result<Vec<u8>, AppError> {
    let query_url = format!("{}/audio_query", base_url.trim_end_matches('/'));
    let synthesis_url = format!("{}/synthesis", base_url.trim_end_matches('/'));

    let query = http_client
        .post(&query_url)
        .query(&[("text", text), ("speaker", &speaker.to_string())])
        .send()
        .await
        .map_err(|_| AppError::internal("failed to call VOICEVOX audio_query"))?;

    if !query.status().is_success() {
        return Err(AppError::internal(format!(
            "VOICEVOX audio_query failed with status {}",
            query.status()
        )));
    }

    let voice_query = query
        .json::<serde_json::Value>()
        .await
        .map_err(|_| AppError::internal("failed to parse VOICEVOX audio_query response"))?;

    let synthesis = http_client
        .post(&synthesis_url)
        .query(&[("speaker", &speaker.to_string())])
        .json(&voice_query)
        .send()
        .await
        .map_err(|_| AppError::internal("failed to call VOICEVOX synthesis"))?;

    if !synthesis.status().is_success() {
        return Err(AppError::internal(format!(
            "VOICEVOX synthesis failed with status {}",
            synthesis.status()
        )));
    }

    synthesis
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|_| AppError::internal("failed to read VOICEVOX synthesized audio"))
}

async fn extract_page_count(pdf_path: &FsPath) -> Result<u32, AppError> {
    let pdf_path = pdf_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let output = Command::new("pdfinfo")
            .arg(&pdf_path)
            .output()
            .map_err(|_| AppError::internal("failed to run pdfinfo"))?;

        if !output.status.success() {
            return Err(AppError::internal("pdfinfo failed"));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let pages_line = stdout
            .lines()
            .find(|line| line.starts_with("Pages:"))
            .ok_or_else(|| AppError::internal("could not parse page count"))?;

        let pages = pages_line
            .split(':')
            .nth(1)
            .map(str::trim)
            .ok_or_else(|| AppError::internal("could not parse page count"))?
            .parse::<u32>()
            .map_err(|_| AppError::internal("invalid page count"))?;

        Ok(pages)
    })
    .await
    .map_err(|_| AppError::internal("pdfinfo task failed"))?
}

fn preview_text(text: &str, max_chars: usize) -> String {
    if text.is_empty() {
        return "No text could be extracted from these pages.".to_string();
    }

    let mut preview = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        preview.push_str("...");
    }
    preview
}

fn file_stem_or_name(file_name: &str) -> String {
    FsPath::new(file_name)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(file_name)
        .to_string()
}

#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub message: String,
}

impl AppError {
    pub fn not_found(message: impl Into<String>) -> Self {
        Self { status: StatusCode::NOT_FOUND, message: message.into() }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self { status: StatusCode::INTERNAL_SERVER_ERROR, message: message.into() }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self { status: StatusCode::BAD_REQUEST, message: message.into() }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gemini_chunk_json_from_fenced_response() {
        let raw = r#"
```json
{
  "chunks": [
    {
      "title": "第1章 導入",
      "page_start": 5,
      "page_end": 8,
      "source_text": "この章では設計の前提を説明する。",
      "key_points": ["前提を整理する", "後続章の読み方を示す"],
      "summary_text": "この範囲は導入です。",
      "dialogue_script": "今回は導入を確認します。",
      "qa_context": "設計の前提と後続章の読み方を扱う。"
    }
  ]
}
```
"#;

        let parsed = parse_gemini_chunks(raw).expect("Gemini JSON should parse");

        assert_eq!(parsed.chunks.len(), 1);
        assert_eq!(parsed.chunks[0].title, "第1章 導入");
        assert_eq!(parsed.chunks[0].page_start, 5);
        assert_eq!(parsed.chunks[0].key_points.len(), 2);
    }

    #[test]
    fn builds_unique_gemini_model_fallbacks_from_primary_model() {
        let models = gemini_model_candidates("gemini-2.5-flash");

        assert_eq!(models[0], "gemini-2.5-flash");
        assert!(models.contains(&"gemini-2.5-flash-lite".to_string()));
        assert_eq!(
            models
                .iter()
                .filter(|model| *model == "gemini-2.5-flash")
                .count(),
            1
        );
    }

    #[test]
    fn normalizes_removed_gemini_15_flash_model() {
        let models = gemini_model_candidates("gemini-1.5-flash");

        assert_eq!(models[0], "gemini-2.5-flash-lite");
        assert!(!models.contains(&"gemini-1.5-flash".to_string()));
    }

    #[test]
    fn upload_starts_without_local_chunks() {
        assert!(initial_chunks_for_upload().is_empty());
    }
}

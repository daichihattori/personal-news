mod llm;
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
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use chrono::Utc;
use llm::{GeneratedChunkContent, SharedLlmClient, build_llm_client};
use models::{
    BookChunk, ChunkListItem, CreatedDocumentResponse, Document, GenerateAudioResponse,
    GenerateChunkResponse, GenerateDocumentResponse, QaRequest, QaResponse,
};
use reqwest::Client;
use tower_http::services::ServeDir;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::info;
use uuid::Uuid;

const CHUNK_START_PAGE: u32 = 5;

#[derive(Clone)]
struct AppState {
    store: Arc<RwLock<Store>>,
    data_dir: PathBuf,
    llm_client: SharedLlmClient,
    http_client: Client,
    voicevox_base_url: String,
}

#[derive(Default)]
struct Store {
    documents: HashMap<String, Document>,
    chunks: HashMap<String, BookChunk>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("personal_news_backend=debug,tower_http=debug")
        .init();

    let data_dir = init_data_dirs();
    let state = AppState {
        store: Arc::new(RwLock::new(load_store(&data_dir))),
        data_dir,
        llm_client: build_llm_client(),
        http_client: Client::new(),
        voicevox_base_url: env::var("VOICEVOX_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:50021".to_string()),
    };

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/documents", get(list_documents).post(create_document))
        .route("/api/documents/{id}/generate", post(generate_document))
        .route("/api/documents/{id}/chunks", get(list_document_chunks))
        .route("/api/chunks/{id}", get(get_chunk))
        .route("/api/chunks/{id}/generate", post(generate_chunk))
        .route("/api/chunks/{id}/audio", post(generate_audio))
        .route("/api/chunks/{id}/qa", post(answer_chunk_question))
        .nest_service("/audio", ServeDir::new(state.data_dir.join("audio")))
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .with_state(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
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

async fn list_documents(State(state): State<AppState>) -> Result<Json<Vec<Document>>, AppError> {
    let store = state
        .store
        .read()
        .map_err(|_| AppError::internal("failed to read document store"))?;

    let mut documents: Vec<_> = store.documents.values().cloned().collect();
    documents.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(Json(documents))
}

async fn create_document(
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
    let stored_file_name = format!("{document_id}.pdf");
    let pdf_path = state.data_dir.join("documents").join(stored_file_name);

    tokio::fs::write(&pdf_path, file_bytes)
        .await
        .map_err(|_| AppError::internal("failed to store uploaded PDF"))?;

    let page_count = extract_page_count(&pdf_path).await?;
    let document = Document {
        id: document_id.clone(),
        title: file_stem_or_name(&file_name),
        file_name,
        total_pages: page_count,
        created_at: Utc::now(),
    };

    let chunks = build_chunks(&document, &pdf_path, 3).await?;

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
        persist_chunk(&state.data_dir, chunk)?;
    }

    let response = CreatedDocumentResponse {
        document,
        chunks: chunks.iter().map(ChunkListItem::from).collect(),
    };

    Ok(Json(response))
}

async fn list_document_chunks(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<Vec<ChunkListItem>>, AppError> {
    let store = state
        .store
        .read()
        .map_err(|_| AppError::internal("failed to read chunk store"))?;

    if !store.documents.contains_key(&id) {
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

    Ok(Json(chunk))
}

async fn answer_chunk_question(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(payload): Json<QaRequest>,
) -> Result<Json<QaResponse>, AppError> {
    let chunk = read_chunk(&state, &id)?;

    let response = state
        .llm_client
        .answer_question(&chunk, &payload.question)
        .await
        .map(QaResponse::from)
        .unwrap_or_else(|_| QaResponse {
            answer: format!(
                "Stub answer for chunk {} (pages {}-{}): '{}' is not wired to Claude yet. Use qa_context as the source of truth.",
                chunk.title, chunk.page_start, chunk.page_end, payload.question
            ),
            references: vec![format!("pages {}-{}", chunk.page_start, chunk.page_end)],
        });

    Ok(Json(response))
}

async fn generate_chunk(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<GenerateChunkResponse>, AppError> {
    let chunk = read_chunk(&state, &id)?;
    let updated_chunk = generate_chunk_content_with_fallback(&state, chunk).await?;

    Ok(Json(GenerateChunkResponse {
        chunk: updated_chunk,
    }))
}

async fn generate_document(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<GenerateDocumentResponse>, AppError> {
    let document = {
        let store = state
            .store
            .read()
            .map_err(|_| AppError::internal("failed to read document store"))?;
        store
            .documents
            .get(&id)
            .cloned()
            .ok_or_else(|| AppError::not_found("document not found"))?
    };

    let mut chunk_ids = {
        let store = state
            .store
            .read()
            .map_err(|_| AppError::internal("failed to read chunk store"))?;
        store
            .chunks
            .values()
            .filter(|chunk| chunk.document_id == id)
            .map(|chunk| chunk.id.clone())
            .collect::<Vec<_>>()
    };
    chunk_ids.sort();

    let mut generated_chunks = Vec::with_capacity(chunk_ids.len());
    for chunk_id in chunk_ids {
        let chunk = read_chunk(&state, &chunk_id)?;
        let updated_chunk = generate_chunk_content_with_fallback(&state, chunk).await?;
        generated_chunks.push(updated_chunk);
    }

    generated_chunks.sort_by(|a, b| {
        a.page_start
            .cmp(&b.page_start)
            .then_with(|| a.page_end.cmp(&b.page_end))
    });

    Ok(Json(GenerateDocumentResponse {
        document,
        generated_chunks,
    }))
}

async fn generate_audio(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> Result<Json<GenerateAudioResponse>, AppError> {
    let chunk = read_chunk(&state, &id)?;
    let updated_chunk = synthesize_chunk_audio(&state, chunk).await?;
    let audio_url = updated_chunk
        .audio_path
        .clone()
        .ok_or_else(|| AppError::internal("audio path was not set after synthesis"))?;

    Ok(Json(GenerateAudioResponse {
        chunk: updated_chunk,
        audio_url,
    }))
}

fn read_chunk(state: &AppState, id: &str) -> Result<BookChunk, AppError> {
    let store = state
        .store
        .read()
        .map_err(|_| AppError::internal("failed to read chunk store"))?;

    store
        .chunks
        .get(id)
        .cloned()
        .ok_or_else(|| AppError::not_found("chunk not found"))
}

fn init_data_dirs() -> PathBuf {
    let data_dir = PathBuf::from("../data");
    for name in ["documents", "chunks", "audio"] {
        fs::create_dir_all(data_dir.join(name)).expect("failed to create data directories");
    }
    data_dir
}

fn load_store(data_dir: &FsPath) -> Store {
    let mut documents = HashMap::new();
    let mut chunks = HashMap::new();

    let documents_dir = data_dir.join("documents");
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
                Some(document) => {
                    documents.insert(document.id.clone(), document);
                }
                None => {
                    tracing::warn!("failed to load document metadata from {}", path.display());
                }
            }
        }
    }

    let chunks_dir = data_dir.join("chunks");
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
                    tracing::warn!("failed to load chunk metadata from {}", path.display());
                }
            }
        }
    }

    Store { documents, chunks }
}

fn persist_document(data_dir: &FsPath, document: &Document) -> Result<(), AppError> {
    let path = data_dir
        .join("documents")
        .join(format!("{}.json", document.id));
    let json = serde_json::to_string_pretty(document)
        .map_err(|_| AppError::internal("failed to serialize document metadata"))?;
    fs::write(path, json).map_err(|_| AppError::internal("failed to persist document metadata"))
}

fn persist_chunk(data_dir: &FsPath, chunk: &BookChunk) -> Result<(), AppError> {
    let path = data_dir.join("chunks").join(format!("{}.json", chunk.id));
    let json = serde_json::to_string_pretty(chunk)
        .map_err(|_| AppError::internal("failed to serialize chunk metadata"))?;
    fs::write(path, json).map_err(|_| AppError::internal("failed to persist chunk metadata"))
}

async fn generate_chunk_content_with_fallback(
    state: &AppState,
    chunk: BookChunk,
) -> Result<BookChunk, AppError> {
    let generated = state
        .llm_client
        .generate_chunk_content(&chunk)
        .await
        .unwrap_or_else(|_| fallback_generated_content(&chunk));

    let updated_chunk = BookChunk {
        key_points: generated.key_points,
        summary_text: generated.summary_text,
        dialogue_script: generated.dialogue_script,
        qa_context: generated.qa_context,
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

    persist_chunk(&state.data_dir, &updated_chunk)?;

    Ok(updated_chunk)
}

fn fallback_generated_content(chunk: &BookChunk) -> GeneratedChunkContent {
    GeneratedChunkContent {
        key_points: vec![
            format!(
                "Pages {}-{} were extracted from the uploaded PDF.",
                chunk.page_start, chunk.page_end
            ),
            "Claude generation is unavailable, so this chunk still uses extracted text."
                .to_string(),
            "Switching to API-backed generation later only requires replacing the LlmClient implementation.".to_string(),
        ],
        summary_text: preview_text(&chunk.source_text, 220),
        dialogue_script: format!(
            "ずんだもん: 今回は {} の {}ページから{}ページを読むのだ。内容の冒頭を確認すると、{}",
            chunk.title,
            chunk.page_start,
            chunk.page_end,
            preview_text(&chunk.source_text, 220)
        ),
        qa_context: chunk.source_text.clone(),
    }
}

async fn synthesize_chunk_audio(state: &AppState, chunk: BookChunk) -> Result<BookChunk, AppError> {
    let script = if chunk.dialogue_script.trim().is_empty() {
        return Err(AppError::bad_request("dialogue_script is empty"));
    } else {
        chunk.dialogue_script.clone()
    };

    let audio_bytes =
        synthesize_with_voicevox(&state.http_client, &state.voicevox_base_url, &script, 3).await?;

    let file_name = format!("{}.wav", chunk.id);
    let audio_path = state.data_dir.join("audio").join(&file_name);
    tokio::fs::write(&audio_path, audio_bytes)
        .await
        .map_err(|_| AppError::internal("failed to write synthesized audio"))?;

    let updated_chunk = BookChunk {
        audio_path: Some(format!("/audio/{file_name}")),
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

    persist_chunk(&state.data_dir, &updated_chunk)?;

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

async fn build_chunks(
    document: &Document,
    pdf_path: &FsPath,
    pages_per_chunk: u32,
) -> Result<Vec<BookChunk>, AppError> {
    let mut chunks = Vec::new();
    let mut start = CHUNK_START_PAGE;

    if document.total_pages < CHUNK_START_PAGE {
        return Ok(chunks);
    }

    while start <= document.total_pages {
        let end = (start + pages_per_chunk - 1).min(document.total_pages);
        let source_text = extract_pdf_text(pdf_path, start, end).await?;
        let normalized = normalize_text(&source_text);
        let preview = preview_text(&normalized, 220);

        chunks.push(BookChunk {
            id: Uuid::new_v4().to_string(),
            document_id: document.id.clone(),
            title: format!("{} (pages {}-{})", document.title, start, end),
            page_start: start,
            page_end: end,
            source_text: normalized.clone(),
            key_points: vec![
                format!("Pages {}-{} were extracted from the uploaded PDF.", start, end),
                "Claude summary is not wired yet, so this is raw extracted text.".to_string(),
            ],
            summary_text: preview.clone(),
            dialogue_script: format!(
                "ずんだもん: 今回は {} の {}ページから{}ページを読むのだ。内容の冒頭を確認すると、{}",
                document.title, start, end, preview
            ),
            qa_context: normalized,
            audio_path: None,
        });

        start = end + 1;
    }

    Ok(chunks)
}

async fn extract_pdf_text(pdf_path: &FsPath, start: u32, end: u32) -> Result<String, AppError> {
    let pdf_path = pdf_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let output = Command::new("pdftotext")
            .arg("-layout")
            .arg("-f")
            .arg(start.to_string())
            .arg("-l")
            .arg(end.to_string())
            .arg(&pdf_path)
            .arg("-")
            .output()
            .map_err(|_| AppError::internal("failed to run pdftotext"))?;

        if !output.status.success() {
            return Err(AppError::internal("pdftotext failed"));
        }

        let text = String::from_utf8(output.stdout)
            .map_err(|_| AppError::internal("pdftotext output was not valid UTF-8"))?;

        Ok(text)
    })
    .await
    .map_err(|_| AppError::internal("pdftotext task failed"))?
}

fn normalize_text(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
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
struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(serde_json::json!({
                "error": self.message
            })),
        )
            .into_response()
    }
}

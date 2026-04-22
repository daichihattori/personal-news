mod llm;
mod models;

use std::{
    collections::HashMap,
    fs,
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
    BookChunk, ChunkListItem, CreatedDocumentResponse, Document, GenerateChunkResponse, QaRequest,
    QaResponse,
};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::info;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    store: Arc<RwLock<Store>>,
    data_dir: PathBuf,
    llm_client: SharedLlmClient,
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

    let state = AppState {
        store: Arc::new(RwLock::new(seed_store())),
        data_dir: init_data_dirs(),
        llm_client: build_llm_client(),
    };

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/documents", get(list_documents).post(create_document))
        .route("/api/documents/{id}/chunks", get(list_document_chunks))
        .route("/api/chunks/{id}", get(get_chunk))
        .route("/api/chunks/{id}/generate", post(generate_chunk))
        .route("/api/chunks/{id}/qa", post(answer_chunk_question))
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

    let documents = store.documents.values().cloned().collect();
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
        .collect();

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

    let generated = state
        .llm_client
        .generate_chunk_content(&chunk)
        .await
        .unwrap_or_else(|_| GeneratedChunkContent {
            key_points: vec![
                format!("Pages {}-{} were extracted from the uploaded PDF.", chunk.page_start, chunk.page_end),
                "Claude generation is unavailable, so this chunk still uses extracted text.".to_string(),
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
        });

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

    Ok(Json(GenerateChunkResponse {
        chunk: updated_chunk,
    }))
}

fn seed_store() -> Store {
    let document_id = Uuid::new_v4().to_string();
    let chunk_id = Uuid::new_v4().to_string();

    let document = Document {
        id: document_id.clone(),
        title: "Sample Book".to_string(),
        file_name: "sample-book.pdf".to_string(),
        total_pages: 42,
        created_at: Utc::now(),
    };

    let chunk = BookChunk {
        id: chunk_id.clone(),
        document_id: document_id.clone(),
        title: "Chapter 1: Why This Matters".to_string(),
        page_start: 1,
        page_end: 3,
        source_text: "This is placeholder source text for the first three pages of the document."
            .to_string(),
        key_points: vec![
            "The author introduces the core problem.".to_string(),
            "The chapter frames the rest of the book.".to_string(),
        ],
        summary_text: "The opening pages explain the central theme and why the topic matters."
            .to_string(),
        dialogue_script: "ずんだもん: まず、この本が何を問題にしているかを見るのだ。ここでは、これから扱うテーマの背景が整理されているのだ。"
            .to_string(),
        qa_context: "Pages 1-3 introduce the book's main question, explain its context, and set up the argument for the following chapters."
            .to_string(),
        audio_path: None,
    };

    let mut documents = HashMap::new();
    documents.insert(document.id.clone(), document);

    let mut chunks = HashMap::new();
    chunks.insert(chunk.id.clone(), chunk);

    Store { documents, chunks }
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
    let mut start = 1;

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

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

const MIN_SCAN_START_PAGE: u32 = 5;
const MIN_CHUNK_PAGES: usize = 2;
const MAX_CHUNK_PAGES: usize = 4;

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

    let chunks = build_chunks(&document, &pdf_path).await?;

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
        title: generated.title,
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
        title: chunk.title.clone(),
        key_points: vec![
            format!("{}ページから{}ページの本文を読み取りました。", chunk.page_start, chunk.page_end),
            "Claude 生成が使えないため、抽出テキストをもとに暫定の説明を作っています。".to_string(),
            "見出しと本文を確認してから、必要に応じて再生成してください。".to_string(),
        ],
        summary_text: format!(
            "{}ページから{}ページの抽出結果です。現状は自動要約が使えないため、本文の冒頭だけを表示しています。{}",
            chunk.page_start,
            chunk.page_end,
            preview_text(&chunk.source_text, 180)
        ),
        dialogue_script: format!(
            "今回は {} の {}ページから{}ページを見ます。まだきちんとした要約は作れていないので、まずは抽出できた本文の冒頭を確認します。{}",
            chunk.title,
            chunk.page_start,
            chunk.page_end,
            preview_text(&chunk.source_text, 180)
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

async fn build_chunks(document: &Document, pdf_path: &FsPath) -> Result<Vec<BookChunk>, AppError> {
    let mut pages = Vec::new();
    for page_number in MIN_SCAN_START_PAGE..=document.total_pages {
        let raw_text = extract_pdf_text(pdf_path, page_number, page_number).await?;
        let normalized = normalize_text(&raw_text);
        if normalized.is_empty() {
            continue;
        }

        pages.push(ExtractedPage {
            page_number,
            text: normalized.clone(),
            heading: detect_heading(&normalized),
        });
    }

    if pages.is_empty() {
        return Ok(Vec::new());
    }

    let content_start = detect_content_start_page(&pages).unwrap_or(MIN_SCAN_START_PAGE);
    let relevant_pages = pages
        .into_iter()
        .filter(|page| page.page_number >= content_start)
        .collect::<Vec<_>>();

    chunk_pages(document, &relevant_pages)
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
        .filter(|line| !is_page_artifact(line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_page_artifact(line: &str) -> bool {
    if line.chars().all(|ch| ch.is_ascii_digit()) {
        return true;
    }

    let compact = line.replace(' ', "");
    if !compact.is_empty() && compact.chars().all(|ch| matches!(ch, 'i' | 'v' | 'x' | 'I' | 'V' | 'X')) {
        return true;
    }

    false
}

#[derive(Debug, Clone)]
struct ExtractedPage {
    page_number: u32,
    text: String,
    heading: Option<DetectedHeading>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeadingStrength {
    Major,
    Section,
}

#[derive(Debug, Clone)]
struct DetectedHeading {
    title: String,
    strength: HeadingStrength,
}

fn detect_content_start_page(pages: &[ExtractedPage]) -> Option<u32> {
    pages.iter().find_map(|page| {
        page.heading.as_ref().and_then(|heading| {
            if heading.strength == HeadingStrength::Major {
                Some(page.page_number)
            } else {
                None
            }
        })
    })
}

fn chunk_pages(document: &Document, pages: &[ExtractedPage]) -> Result<Vec<BookChunk>, AppError> {
    if pages.is_empty() {
        return Ok(Vec::new());
    }

    let mut ranges = Vec::new();
    let mut start_index = 0usize;

    for index in 1..pages.len() {
        let current_len = index - start_index;
        let next_has_heading = pages[index].heading.is_some();

        if current_len >= MIN_CHUNK_PAGES && next_has_heading {
            ranges.push((start_index, index));
            start_index = index;
            continue;
        }

        if current_len >= MAX_CHUNK_PAGES {
            ranges.push((start_index, index));
            start_index = index;
        }
    }

    if let Some((last_start, last_end)) = ranges.last_mut() {
        let trailing_len = pages.len() - start_index;
        if trailing_len < MIN_CHUNK_PAGES && (*last_end - *last_start) < MAX_CHUNK_PAGES {
            *last_end = pages.len();
        } else {
            ranges.push((start_index, pages.len()));
        }
    } else {
        ranges.push((start_index, pages.len()));
    }

    let chunks = ranges
        .into_iter()
        .map(|(start_index, end_index)| {
            let page_slice = &pages[start_index..end_index];
            build_chunk_from_pages(document, page_slice)
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(chunks)
}

fn build_chunk_from_pages(
    document: &Document,
    page_slice: &[ExtractedPage],
) -> Result<BookChunk, AppError> {
    let first_page = page_slice
        .first()
        .ok_or_else(|| AppError::internal("cannot build chunk from empty page slice"))?;
    let last_page = page_slice
        .last()
        .ok_or_else(|| AppError::internal("cannot build chunk from empty page slice"))?;

    let source_text = page_slice
        .iter()
        .map(|page| format!("[Page {}]\n{}", page.page_number, page.text))
        .collect::<Vec<_>>()
        .join("\n\n");
    let preview = preview_text(&source_text, 220);
    let title = page_slice
        .iter()
        .find_map(|page| page.heading.as_ref().map(|heading| heading.title.clone()))
        .unwrap_or_else(|| {
            format!(
                "{} {}-{}ページ",
                document.title, first_page.page_number, last_page.page_number
            )
        });

    Ok(BookChunk {
        id: Uuid::new_v4().to_string(),
        document_id: document.id.clone(),
        title: title.clone(),
        page_start: first_page.page_number,
        page_end: last_page.page_number,
        source_text: source_text.clone(),
        key_points: vec![
            format!(
                "{}ページから{}ページの抽出内容です。",
                first_page.page_number, last_page.page_number
            ),
            "見出し単位に近づけるため、章や節の切れ目で chunk を分けています。".to_string(),
        ],
        summary_text: format!(
            "{}ページから{}ページの暫定プレビューです。{}",
            first_page.page_number,
            last_page.page_number,
            preview
        ),
        dialogue_script: format!(
            "今回は {} を見ます。まだ整理前なので、まずは本文の冒頭を確認します。{}",
            title, preview
        ),
        qa_context: source_text,
        audio_path: None,
    })
}

fn detect_heading(text: &str) -> Option<DetectedHeading> {
    let lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(12)
        .collect::<Vec<_>>();

    if lines.is_empty() {
        return None;
    }

    for window in lines.windows(2) {
        let first = window[0];
        let second = window[1];
        if is_chapter_line(first) && !looks_like_noise(second) {
            return Some(DetectedHeading {
                title: clean_heading_title(&format!("{} {}", first, second)),
                strength: HeadingStrength::Major,
            });
        }
    }

    for line in &lines {
        if is_numbered_section_line(line) {
            return Some(DetectedHeading {
                title: clean_heading_title(line),
                strength: HeadingStrength::Section,
            });
        }

        if is_major_heading_line(line) {
            return Some(DetectedHeading {
                title: clean_heading_title(line),
                strength: HeadingStrength::Major,
            });
        }
    }

    None
}

fn is_chapter_line(line: &str) -> bool {
    let compact = line.replace(' ', "");
    compact.ends_with('章')
        && compact[..compact.len().saturating_sub("章".len())]
            .chars()
            .all(|ch| ch.is_ascii_digit() || ch == '第')
}

fn is_major_heading_line(line: &str) -> bool {
    let compact = line.replace(' ', "");
    (compact.starts_with('第') && compact.ends_with('章'))
        || compact.ends_with("編")
        || compact.ends_with("部")
}

fn is_numbered_section_line(line: &str) -> bool {
    let trimmed = line.trim();
    let mut seen_digit = false;
    let mut seen_dot = false;

    for ch in trimmed.chars() {
        if ch.is_ascii_digit() {
            seen_digit = true;
            continue;
        }

        if ch == '.' {
            if !seen_digit {
                return false;
            }
            seen_dot = true;
            continue;
        }

        return seen_digit && seen_dot && ch.is_whitespace();
    }

    false
}

fn looks_like_noise(line: &str) -> bool {
    let compact = line.replace(' ', "");
    compact.is_empty() || compact.chars().all(|ch| ch.is_ascii_digit())
}

fn clean_heading_title(raw: &str) -> String {
    let mut parts = raw.split_whitespace().collect::<Vec<_>>();
    while parts
        .last()
        .map(|part| part.chars().all(|ch| ch.is_ascii_digit()))
        .unwrap_or(false)
    {
        parts.pop();
    }

    parts.join(" ")
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

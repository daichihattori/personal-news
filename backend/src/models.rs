use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::llm::QuestionAnswer;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: String,
    pub title: String,
    pub file_name: String,
    pub total_pages: u32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatedDocumentResponse {
    pub document: Document,
    pub chunks: Vec<ChunkListItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GenerateChunkResponse {
    pub chunk: BookChunk,
}

#[derive(Debug, Clone, Serialize)]
pub struct GenerateAudioResponse {
    pub chunk: BookChunk,
    pub audio_url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GenerateDocumentResponse {
    pub document: Document,
    pub generated_chunks: Vec<BookChunk>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookChunk {
    pub id: String,
    pub document_id: String,
    pub title: String,
    pub page_start: u32,
    pub page_end: u32,
    pub source_text: String,
    pub key_points: Vec<String>,
    pub summary_text: String,
    pub dialogue_script: String,
    pub qa_context: String,
    pub audio_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChunkListItem {
    pub id: String,
    pub title: String,
    pub page_start: u32,
    pub page_end: u32,
    pub summary_text: String,
    pub audio_path: Option<String>,
}

impl From<&BookChunk> for ChunkListItem {
    fn from(chunk: &BookChunk) -> Self {
        Self {
            id: chunk.id.clone(),
            title: chunk.title.clone(),
            page_start: chunk.page_start,
            page_end: chunk.page_end,
            summary_text: chunk.summary_text.clone(),
            audio_path: chunk.audio_path.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct QaRequest {
    pub question: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct QaResponse {
    pub answer: String,
    pub references: Vec<String>,
}

impl From<QuestionAnswer> for QaResponse {
    fn from(value: QuestionAnswer) -> Self {
        Self {
            answer: value.answer,
            references: value.references,
        }
    }
}

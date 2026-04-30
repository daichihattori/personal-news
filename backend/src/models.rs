use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

fn default_user_id() -> String {
    "local-user".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyInfo {
    pub provider: String,
    pub configured: bool,
    pub key_hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: String,
    #[serde(default = "default_user_id")]
    pub user_id: String,
    pub title: String,
    pub file_name: String,
    pub total_pages: u32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DialogueTurn {
    pub speaker: String, // "zundamon" | "metan"
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatedDocumentResponse {
    pub document: Document,
    pub chunks: Vec<ChunkListItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GenerateDocumentResponse {
    pub document: Document,
    pub chunks: Vec<BookChunk>,
    pub model: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GenerateAudioResponse {
    pub chunk: BookChunk,
    pub audio_url: String,
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
    #[serde(default)]
    pub dialogue_script: String,
    #[serde(default)]
    pub dialogue_turns: Vec<DialogueTurn>,
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

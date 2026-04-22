use std::{
    env,
    path::PathBuf,
    process::{Command, Stdio},
    sync::Arc,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{AppError, models::BookChunk};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedChunkContent {
    pub title: String,
    pub key_points: Vec<String>,
    pub summary_text: String,
    pub dialogue_script: String,
    pub qa_context: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub answer: String,
    pub references: Vec<String>,
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn generate_chunk_content(
        &self,
        chunk: &BookChunk,
    ) -> Result<GeneratedChunkContent, AppError>;

    async fn answer_question(
        &self,
        chunk: &BookChunk,
        question: &str,
    ) -> Result<QuestionAnswer, AppError>;
}

pub type SharedLlmClient = Arc<dyn LlmClient>;

pub fn build_llm_client() -> SharedLlmClient {
    Arc::new(LocalClaudeCliClient::from_env())
}

pub struct LocalClaudeCliClient {
    command: String,
    home_dir: Option<PathBuf>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChunkAnalysis {
    title: String,
    key_points: Vec<String>,
    summary_text: String,
    qa_context: String,
}

#[derive(Debug, Deserialize)]
struct DialogueDraft {
    dialogue_script: String,
}

impl LocalClaudeCliClient {
    fn from_env() -> Self {
        Self {
            command: env::var("CLAUDE_CODE_COMMAND").unwrap_or_else(|_| "claude".to_string()),
            home_dir: env::var("CLAUDE_CODE_HOME").ok().map(PathBuf::from),
            model: env::var("CLAUDE_CODE_MODEL").ok(),
        }
    }

    async fn run_prompt<T>(&self, prompt: String) -> Result<T, AppError>
    where
        T: for<'de> Deserialize<'de> + Send + 'static,
    {
        let command = self.command.clone();
        let home_dir = self.home_dir.clone();
        let model = self.model.clone();

        tokio::task::spawn_blocking(move || {
            let mut cmd = Command::new(&command);
            cmd.arg("-p")
                .arg("--output-format")
                .arg("text")
                .arg("--permission-mode")
                .arg("bypassPermissions")
                .arg("--dangerously-skip-permissions")
                .arg(prompt)
                .stdin(Stdio::null())
                .stderr(Stdio::piped())
                .stdout(Stdio::piped());

            if let Some(home_dir) = home_dir {
                cmd.env("HOME", home_dir);
            }

            if let Some(model) = model {
                cmd.arg("--model").arg(model);
            }

            let output = cmd
                .output()
                .map_err(|_| AppError::internal("failed to launch claude command"))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(AppError::internal(format!(
                    "claude command failed: {}",
                    stderr.trim()
                )));
            }

            let stdout = String::from_utf8(output.stdout)
                .map_err(|_| AppError::internal("claude output was not valid UTF-8"))?;

            let json = extract_json(&stdout)?;
            serde_json::from_str::<T>(&json)
                .map_err(|_| AppError::internal("failed to parse claude JSON output"))
        })
        .await
        .map_err(|_| AppError::internal("claude task failed"))?
    }
}

#[async_trait]
impl LlmClient for LocalClaudeCliClient {
    async fn generate_chunk_content(
        &self,
        chunk: &BookChunk,
    ) -> Result<GeneratedChunkContent, AppError> {
        let analysis_prompt = format!(
            "You are analyzing OCR-like extracted pages from a technical book.\n\
Ignore page numbers, running headers, repeated chapter headers, figure labels, and navigation text unless they are essential to the explanation.\n\
Return only valid JSON with this exact schema:\n\
{{\"title\":\"...\",\"key_points\":[\"...\"],\"summary_text\":\"...\",\"qa_context\":\"...\"}}\n\
\n\
Rules:\n\
- Write everything in Japanese.\n\
- title: a short section title that reflects the actual topic. Do not include page ranges.\n\
- key_points: 3 to 5 concrete points grounded in the text.\n\
- summary_text: 4 to 6 sentences. Explain what this chunk is teaching, why it matters, and what the reader should retain.\n\
- qa_context: 5 to 10 factual sentences for later Q&A. Include formulas, definitions, code identifiers, or constraints when they matter.\n\
- If the chunk is still introduction, setup, or exercise guidance rather than core theory, say that explicitly instead of pretending it teaches something else.\n\
- Do not add facts that are not supported by the text.\n\
- Do not use markdown fences.\n\
\n\
Chunk metadata:\n\
document_title: {title}\n\
page_start: {page_start}\n\
page_end: {page_end}\n\
\n\
Source text:\n\
{source_text}",
            title = chunk.title,
            page_start = chunk.page_start,
            page_end = chunk.page_end,
            source_text = chunk.source_text
        );

        let analysis: ChunkAnalysis = self.run_prompt(analysis_prompt).await?;
        let dialogue_prompt = format!(
            "You are writing a short Japanese reading script for a single-speaker study audio.\n\
Return only valid JSON with this exact schema:\n\
{{\"dialogue_script\":\"...\"}}\n\
\n\
Rules:\n\
- The output is for voice synthesis, so write short spoken sentences.\n\
- Use natural Japanese. Calm, clear, and explanatory. No hype.\n\
- Start by saying what this chunk is about in plain language.\n\
- Then explain 2 or 3 important points in order.\n\
- End with one sentence about what to pay attention to next.\n\
- No bullet points, no markdown, no role labels, no stage directions.\n\
- Stay faithful to the structured notes below.\n\
\n\
Chunk title:\n\
{title}\n\
\n\
Summary:\n\
{summary_text}\n\
\n\
Key points:\n\
{key_points}\n\
\n\
Q&A context:\n\
{qa_context}",
            title = analysis.title,
            summary_text = analysis.summary_text,
            key_points = analysis.key_points.join("\n- "),
            qa_context = analysis.qa_context
        );
        let dialogue: DialogueDraft = self.run_prompt(dialogue_prompt).await?;

        Ok(GeneratedChunkContent {
            title: analysis.title,
            key_points: analysis.key_points,
            summary_text: analysis.summary_text,
            dialogue_script: dialogue.dialogue_script,
            qa_context: analysis.qa_context,
        })
    }

    async fn answer_question(
        &self,
        chunk: &BookChunk,
        question: &str,
    ) -> Result<QuestionAnswer, AppError> {
        let prompt = format!(
            "You answer questions about a book chunk.\n\
Return only valid JSON with this exact schema:\n\
{{\"answer\":\"...\",\"references\":[\"...\"]}}\n\
\n\
Rules:\n\
- Answer in Japanese\n\
- Stay within the provided chunk context\n\
- If the answer is not supported by the chunk, say so clearly\n\
- references should be short strings such as page ranges or section hints\n\
- Do not use markdown fences\n\
\n\
Chunk title: {title}\n\
Pages: {page_start}-{page_end}\n\
\n\
Summary:\n\
{summary_text}\n\
\n\
Key points:\n\
{key_points}\n\
\n\
Context:\n\
{qa_context}\n\
\n\
Question:\n\
{question}",
            title = chunk.title,
            page_start = chunk.page_start,
            page_end = chunk.page_end,
            summary_text = chunk.summary_text,
            key_points = chunk.key_points.join("\n- "),
            qa_context = chunk.qa_context,
            question = question
        );

        self.run_prompt(prompt).await
    }
}

fn extract_json(raw: &str) -> Result<String, AppError> {
    let trimmed = raw.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Ok(trimmed.to_string());
    }

    let fenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim);

    if let Some(fenced) = fenced {
        if fenced.starts_with('{') && fenced.ends_with('}') {
            return Ok(fenced.to_string());
        }
    }

    let start = trimmed
        .find('{')
        .ok_or_else(|| AppError::internal("claude output did not include JSON"))?;
    let end = trimmed
        .rfind('}')
        .ok_or_else(|| AppError::internal("claude output did not include JSON"))?;

    Ok(trimmed[start..=end].to_string())
}

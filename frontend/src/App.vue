<script setup>
import { computed, onMounted, ref } from 'vue'

const CONTEXT_WINDOW = 180_000
const CHARS_PER_TOKEN = 2.5
const GEMINI_MODEL_OPTIONS = [
  { value: 'gemini-2.5-flash-lite', label: 'Gemini 2.5 Flash Lite' },
  { value: 'gemini-2.5-flash', label: 'Gemini 2.5 Flash' },
  { value: 'gemini-2.5-pro', label: 'Gemini 2.5 Pro' },
  { value: 'gemini-2.0-flash', label: 'Gemini 2.0 Flash' }
]

const apiBase = import.meta.env.VITE_API_BASE_URL || 'http://127.0.0.1:3000'

const documents = ref([])
const selectedDocumentId = ref('')
const chunks = ref([])
const selectedChunkId = ref('')
const selectedChunk = ref(null)
const statusMessage = ref('')
const uploadInput = ref(null)
const showTranscript = ref(false)
const sidebarOpen = ref(true)
const deletingId = ref('')
const zundamonPortrait = ref('')
const selectedGeminiModel = ref('gemini-2.5-flash-lite')

async function fetchZundamonPortrait() {
  try {
    const voicevoxBase = 'http://127.0.0.1:50021'
    const speakers = await fetch(`${voicevoxBase}/speakers`).then((r) => r.json())
    const zundamon = speakers.find((s) => s.name === 'ずんだもん')
    if (!zundamon) return
    const info = await fetch(
      `${voicevoxBase}/speaker_info?speaker_uuid=${zundamon.speaker_uuid}`
    ).then((r) => r.json())
    zundamonPortrait.value = `data:image/png;base64,${info.portrait}`
  } catch {
    // VOICEVOXが起動していない場合はフォールバック
  }
}

const loadingDocuments = ref(false)
const generatingDocument = ref(false)
const generatingAudio = ref(false)
const uploading = ref(false)

const selectedDocument = computed(() =>
  documents.value.find((d) => d.id === selectedDocumentId.value) ?? null
)

const audioUrl = computed(() => {
  const path = selectedChunk.value?.audio_path
  if (!path) return ''
  return path.startsWith('http') ? path : `${apiBase}${path}`
})

const hasAudio = computed(() => Boolean(audioUrl.value))
const hasChunks = computed(() => chunks.value.length > 0)

const sidebarWidth = computed(() => sidebarOpen.value ? '260px' : '0px')

const estimatedTokens = computed(() => {
  const totalChars = chunks.value.reduce((sum, c) => sum + (c.source_text?.length ?? 0), 0)
  return Math.round(totalChars / CHARS_PER_TOKEN)
})

const tokenUsagePct = computed(() =>
  Math.min(100, Math.round((estimatedTokens.value / CONTEXT_WINDOW) * 100))
)

onMounted(() => {
  refreshDocuments()
  fetchZundamonPortrait()
})

async function request(path, options = {}) {
  const res = await fetch(`${apiBase}${path}`, options)
  if (!res.ok) {
    let msg = `Request failed: ${res.status}`
    try {
      const j = await res.json()
      if (j.error) msg = j.error
    } catch {}
    throw new Error(msg)
  }
  return res
}

async function refreshDocuments() {
  loadingDocuments.value = true
  try {
    const res = await request('/api/documents')
    documents.value = await res.json()
    if (documents.value.length && !documents.value.some((d) => d.id === selectedDocumentId.value)) {
      selectedDocumentId.value = documents.value[0].id
    }
    if (selectedDocumentId.value) await refreshChunks(selectedDocumentId.value)
  } catch (e) {
    statusMessage.value = `読み込み失敗: ${e.message}`
  } finally {
    loadingDocuments.value = false
  }
}

async function refreshChunks(docId = selectedDocumentId.value) {
  if (!docId) return
  try {
    const res = await request(`/api/documents/${docId}/chunks`)
    chunks.value = await res.json()
  } catch (e) {
    statusMessage.value = `チャンク読み込み失敗: ${e.message}`
  }
}

async function selectDocument(id) {
  selectedDocumentId.value = id
  selectedChunkId.value = ''
  selectedChunk.value = null
  showTranscript.value = false
  await refreshChunks(id)
}

async function selectChunk(chunk) {
  if (selectedChunkId.value === chunk.id) return
  selectedChunkId.value = chunk.id
  showTranscript.value = false
  try {
    const res = await request(`/api/chunks/${chunk.id}`)
    selectedChunk.value = await res.json()
  } catch (e) {
    statusMessage.value = `詳細取得失敗: ${e.message}`
  }
}

function triggerUpload() {
  uploadInput.value?.click()
}

async function uploadPdf(event) {
  const [file] = event.target.files ?? []
  if (!file) return
  const formData = new FormData()
  formData.append('file', file)
  uploading.value = true
  statusMessage.value = ''
  try {
    const res = await request('/api/documents', { method: 'POST', body: formData })
    const result = await res.json()
    statusMessage.value = `「${result.document.title}」を追加しました`
    selectedDocumentId.value = result.document.id
    await refreshDocuments()
  } catch (e) {
    statusMessage.value = `アップロード失敗: ${e.message}`
  } finally {
    uploading.value = false
    if (uploadInput.value) uploadInput.value.value = ''
  }
}

async function generateAll() {
  if (!selectedDocumentId.value) return
  generatingDocument.value = true
  statusMessage.value = '要約と音声を生成中… しばらくお待ちください'
  try {
    const model = encodeURIComponent(selectedGeminiModel.value)
    const res = await request(`/api/documents/${selectedDocumentId.value}/generate?model=${model}`, {
      method: 'POST'
    })
    const result = await res.json()
    statusMessage.value = `生成完了！ 使用モデル: ${result.model}`
    await refreshChunks()
  } catch (e) {
    statusMessage.value = `生成失敗: ${e.message}`
  } finally {
    generatingDocument.value = false
  }
}

async function deleteDocument(id) {
  deletingId.value = id
  try {
    await request(`/api/documents/${id}`, { method: 'DELETE' })
    if (selectedDocumentId.value === id) {
      selectedDocumentId.value = ''
      chunks.value = []
      selectedChunkId.value = ''
      selectedChunk.value = null
    }
    await refreshDocuments()
  } catch (e) {
    statusMessage.value = `削除失敗: ${e.message}`
  } finally {
    deletingId.value = ''
  }
}

async function generateChunkAudio(chunkId) {
  generatingAudio.value = true
  statusMessage.value = '音声を生成中…'
  try {
    const res = await request(`/api/chunks/${chunkId}/audio`, { method: 'POST' })
    const result = await res.json()
    if (selectedChunkId.value === chunkId) selectedChunk.value = result.chunk
    await refreshChunks()
    statusMessage.value = '音声生成完了！'
  } catch (e) {
    statusMessage.value = `音声生成失敗: ${e.message}`
  } finally {
    generatingAudio.value = false
  }
}
</script>

<template>
  <div class="shell">
    <!-- Hidden file input -->
    <input
      ref="uploadInput"
      type="file"
      accept="application/pdf"
      style="display: none"
      @change="uploadPdf"
    />

    <!-- サイドバートグルボタン（常時表示） -->
    <button
      class="sidebar-toggle-btn"
      :style="{ left: sidebarOpen ? '228px' : '8px' }"
      @click="sidebarOpen = !sidebarOpen"
      :title="sidebarOpen ? 'サイドバーを閉じる' : 'サイドバーを開く'"
    >
      <span class="icon">{{ sidebarOpen ? 'chevron_left' : 'menu' }}</span>
    </button>

    <!-- ── Sidebar ── -->
    <aside class="sidebar" :style="{ width: sidebarWidth }">
      <div class="sidebar-inner">
        <div class="sidebar-brand">
          <img v-if="zundamonPortrait" :src="zundamonPortrait" class="brand-portrait" alt="ずんだもん" />
          <span v-else class="icon brand-icon-fallback">radio</span>
          <div class="brand-text">
            <span class="brand-title">ずんだもんラジオ</span>
            <span class="brand-sub">論文をラジオ感覚で</span>
          </div>
        </div>

        <button class="new-pdf-btn" @click="triggerUpload" :disabled="uploading">
          <span class="icon">upload_file</span>
          {{ uploading ? 'アップロード中…' : 'PDFを追加' }}
        </button>

        <div class="doc-section-label">ドキュメント</div>

        <nav class="doc-nav">
          <p v-if="loadingDocuments" class="doc-loading">読み込み中…</p>
          <p v-else-if="!documents.length" class="doc-empty">まだPDFがありません</p>
          <div
            v-for="doc in documents"
            :key="doc.id"
            class="doc-item"
            :class="{ active: doc.id === selectedDocumentId }"
            @click="selectDocument(doc.id)"
          >
            <span class="icon doc-item-icon">description</span>
            <span class="doc-item-name">{{ doc.title }}</span>
            <button
              class="doc-delete-btn"
              :class="{ deleting: deletingId === doc.id }"
              @click.stop="deleteDocument(doc.id)"
              title="削除"
            ><span class="icon">delete</span></button>
          </div>
        </nav>

        <!-- トークン使用量バー -->
        <div v-if="hasChunks" class="token-usage">
          <div class="token-label">
            <span>コンテキスト使用量</span>
            <span class="token-count">{{ estimatedTokens.toLocaleString() }} / {{ CONTEXT_WINDOW.toLocaleString() }}</span>
          </div>
          <div class="token-bar-bg">
            <div
              class="token-bar-fill"
              :style="{ width: tokenUsagePct + '%' }"
              :class="{ warn: tokenUsagePct > 60, danger: tokenUsagePct > 85 }"
            />
          </div>
          <p class="token-note">推定値（source_text文字数÷2.5）</p>
        </div>
      </div>
    </aside>

    <!-- ── Main ── -->
    <main class="main" :class="{ 'has-player': selectedChunk }" :style="{ marginLeft: sidebarWidth }">

      <!-- Status -->
      <Transition name="fade">
        <div v-if="statusMessage" class="status-bar">{{ statusMessage }}</div>
      </Transition>

      <!-- No document selected -->
      <div v-if="!selectedDocument" class="hero">
        <span class="icon hero-icon">radio</span>
        <h2>PDFを選んで始めよう</h2>
        <p class="hero-desc">
          左のサイドバーからPDFを選ぶか、<br>
          新しくアップロードしてください
        </p>
      </div>

      <template v-else>
        <!-- No chunks yet -->
        <div v-if="!hasChunks" class="generate-prompt">
          <p class="generate-prompt-doc">{{ selectedDocument.title }}</p>
          <p class="generate-prompt-text">📋 まだ処理されていません</p>
          <label class="model-picker">
            <span>モデル</span>
            <select v-model="selectedGeminiModel" :disabled="generatingDocument">
              <option v-for="option in GEMINI_MODEL_OPTIONS" :key="option.value" :value="option.value">
                {{ option.label }}
              </option>
            </select>
          </label>
          <button class="btn-primary large" @click="generateAll" :disabled="generatingDocument">
            {{ generatingDocument ? '⏳ 生成中…' : '✨ 要約と音声を生成する' }}
          </button>
        </div>

        <!-- Episode list -->
        <div v-else class="episodes">
          <div class="episodes-header">
            <div>
              <h2 class="episodes-doc-title">{{ selectedDocument.title }}</h2>
              <p class="episodes-count">{{ chunks.length }} エピソード</p>
            </div>
            <div class="generate-controls">
              <label class="model-picker compact">
                <span>モデル</span>
                <select v-model="selectedGeminiModel" :disabled="generatingDocument">
                  <option v-for="option in GEMINI_MODEL_OPTIONS" :key="option.value" :value="option.value">
                    {{ option.label }}
                  </option>
                </select>
              </label>
              <button class="btn-ghost" @click="generateAll" :disabled="generatingDocument">
                {{ generatingDocument ? '生成中…' : '再生成' }}
              </button>
            </div>
          </div>

          <div class="episode-list">
            <button
              v-for="(chunk, i) in chunks"
              :key="chunk.id"
              class="episode-card"
              :class="{ active: chunk.id === selectedChunkId }"
              @click="selectChunk(chunk)"
            >
              <div class="ep-num">{{ String(i + 1).padStart(2, '0') }}</div>
              <div class="ep-body">
                <div class="ep-title">{{ chunk.title || `セクション ${chunk.page_start}` }}</div>
                <div class="ep-meta">p.{{ chunk.page_start }} – {{ chunk.page_end }}</div>
                <div
                  class="ep-summary-wrap"
                  :class="{ open: chunk.id === selectedChunkId && selectedChunk?.summary_text }"
                >
                  <div class="ep-summary">{{ selectedChunk?.summary_text }}</div>
                </div>
              </div>
              <span v-if="chunk.audio_path" class="icon badge-audio">music_note</span>
              <span v-else class="icon badge-none">radio_button_unchecked</span>
            </button>
          </div>
        </div>
      </template>
    </main>

    <!-- ── Now Playing ── -->
    <Transition name="player-slide">
      <footer v-if="selectedChunk" class="player" :style="{ left: sidebarWidth }">
        <div class="player-inner">
          <div class="player-info">
            <span class="now-playing-label">Now Playing</span>
            <span class="now-playing-title">{{ selectedChunk.title }}</span>
          </div>

          <div class="player-controls">
            <audio v-if="hasAudio" :src="audioUrl" controls class="audio-el" />
            <div v-else class="no-audio">
              <span>音声未生成</span>
              <button class="btn-ghost small" @click="generateChunkAudio(selectedChunkId)" :disabled="generatingAudio">
                <span class="icon" style="font-size:16px">mic</span>
                {{ generatingAudio ? '生成中…' : '音声を生成' }}
              </button>
            </div>
          </div>

          <button
            class="transcript-toggle"
            @click="showTranscript = !showTranscript"
            :title="showTranscript ? '閉じる' : 'テキストを表示'"
          >
            <span class="icon">{{ showTranscript ? 'close' : 'notes' }}</span>
          </button>
        </div>

        <Transition name="expand">
          <div v-if="showTranscript" class="transcript">
            <template v-if="selectedChunk.dialogue_turns?.length">
              <div
                v-for="(turn, i) in selectedChunk.dialogue_turns"
                :key="i"
                class="chat-turn"
                :class="turn.speaker"
              >
                <span class="chat-speaker">{{ turn.speaker === 'zundamon' ? 'ずんだもん' : 'めたん' }}</span>
                <div class="chat-bubble">{{ turn.text }}</div>
              </div>
            </template>
            <p v-else>{{ selectedChunk.dialogue_script || selectedChunk.summary_text || '(テキストなし)' }}</p>
          </div>
        </Transition>
      </footer>
    </Transition>
  </div>
</template>

<style>
/* ── Material Symbols ── */
.icon {
  font-family: 'Material Symbols Rounded';
  font-size: 20px;
  line-height: 1;
  user-select: none;
  vertical-align: middle;
}

/* ── Shell ── */
.shell {
  min-height: 100vh;
}

/* ── Sidebar toggle ── */
.sidebar-toggle-btn {
  position: fixed;
  top: 18px;
  z-index: 300;
  width: 32px;
  height: 32px;
  border-radius: 50%;
  background: #2d4a2d;
  border: 1px solid rgba(255, 255, 255, 0.12);
  color: #a5d6a7;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: left 0.25s ease, background 0.15s;
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.3);
}

.sidebar-toggle-btn .icon {
  font-size: 20px;
}

.sidebar-toggle-btn:hover {
  background: #3d5a3d;
}

/* ── Sidebar ── */
.sidebar {
  position: fixed;
  top: 0;
  left: 0;
  bottom: 0;
  background: #1a2b1a;
  overflow: hidden;
  transition: width 0.25s ease;
  z-index: 200;
}

.sidebar-inner {
  width: 260px;
  height: 100%;
  display: flex;
  flex-direction: column;
  padding: 16px 12px;
  overflow: hidden;
}

.sidebar-brand {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 8px 20px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
  margin-bottom: 12px;
}

.brand-portrait {
  width: 44px;
  height: 44px;
  border-radius: 50%;
  object-fit: cover;
  object-position: top;
  border: 2px solid rgba(200, 230, 201, 0.3);
  flex-shrink: 0;
}

.brand-icon-fallback {
  font-size: 32px;
  color: #81c784;
}

.brand-text {
  display: flex;
  flex-direction: column;
  line-height: 1.25;
}

.brand-title {
  font-size: 14px;
  font-weight: 800;
  color: #c8e6c9;
}

.brand-sub {
  font-size: 11px;
  color: rgba(200, 230, 201, 0.5);
}

.new-pdf-btn {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  padding: 10px 14px;
  border-radius: 10px;
  border: 1px solid rgba(255, 255, 255, 0.15);
  background: transparent;
  color: #c8e6c9;
  font-size: 14px;
  font-weight: 600;
  cursor: pointer;
  transition: background 0.15s;
  margin-bottom: 20px;
}

.new-pdf-btn:hover:not(:disabled) {
  background: rgba(255, 255, 255, 0.07);
}

.new-pdf-btn:disabled {
  opacity: 0.45;
  cursor: not-allowed;
}

.new-pdf-btn .icon {
  font-size: 20px;
  color: #81c784;
}

.doc-section-label {
  font-size: 11px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  color: rgba(200, 230, 201, 0.35);
  padding: 0 8px;
  margin-bottom: 6px;
}

.doc-nav {
  flex: 1;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.doc-nav::-webkit-scrollbar {
  width: 4px;
}

.doc-nav::-webkit-scrollbar-thumb {
  background: rgba(255, 255, 255, 0.1);
  border-radius: 2px;
}

.doc-loading,
.doc-empty {
  font-size: 13px;
  color: rgba(200, 230, 201, 0.35);
  padding: 8px;
  margin: 0;
}

.doc-item {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 9px 10px;
  border-radius: 8px;
  background: transparent;
  color: rgba(200, 230, 201, 0.75);
  font-size: 13px;
  cursor: pointer;
  text-align: left;
  transition: background 0.12s, color 0.12s;
  position: relative;
}

.doc-item:hover {
  background: rgba(255, 255, 255, 0.06);
  color: #c8e6c9;
}

.doc-item.active {
  background: rgba(67, 160, 71, 0.25);
  color: #a5d6a7;
}

.doc-item-icon {
  font-size: 18px;
  flex-shrink: 0;
  color: rgba(200, 230, 201, 0.5);
}

.doc-item-name {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.doc-delete-btn {
  flex-shrink: 0;
  opacity: 0;
  width: 24px;
  height: 24px;
  border-radius: 4px;
  background: rgba(255, 80, 80, 0.15);
  color: #ef9a9a;
  display: flex;
  align-items: center;
  justify-content: center;
  border: none;
  cursor: pointer;
  transition: opacity 0.15s, background 0.15s;
}

.doc-delete-btn .icon {
  font-size: 16px;
}

.doc-item:hover .doc-delete-btn {
  opacity: 1;
}

.doc-delete-btn:hover {
  background: rgba(255, 80, 80, 0.35);
  color: #ffcdd2;
}

.doc-delete-btn.deleting {
  opacity: 1;
  background: rgba(255, 80, 80, 0.5);
}

/* ── Token usage bar ── */
.token-usage {
  margin-top: auto;
  padding-top: 16px;
  border-top: 1px solid rgba(255, 255, 255, 0.07);
}

.token-label {
  display: flex;
  justify-content: space-between;
  align-items: baseline;
  margin-bottom: 6px;
  font-size: 11px;
  color: rgba(200, 230, 201, 0.45);
}

.token-count {
  font-variant-numeric: tabular-nums;
}

.token-bar-bg {
  height: 4px;
  border-radius: 2px;
  background: rgba(255, 255, 255, 0.08);
  overflow: hidden;
}

.token-bar-fill {
  height: 100%;
  border-radius: 2px;
  background: #43a047;
  transition: width 0.4s ease, background 0.3s;
}

.token-bar-fill.warn {
  background: #f9a825;
}

.token-bar-fill.danger {
  background: #e53935;
}

.token-note {
  margin: 4px 0 0;
  font-size: 10px;
  color: rgba(200, 230, 201, 0.25);
}

/* ── Main ── */
.main {
  min-height: 100vh;
  padding: 40px 48px 48px;
  transition: margin-left 0.25s ease;
}

.main.has-player {
  padding-bottom: 110px;
}

/* ── Status bar ── */
.status-bar {
  margin-bottom: 20px;
  padding: 10px 16px;
  border-radius: 10px;
  background: rgba(200, 230, 201, 0.25);
  font-size: 13px;
  color: #2e7d32;
}

/* ── Hero ── */
.hero {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  min-height: 60vh;
  text-align: center;
}

.hero-icon {
  font-size: 72px;
  color: #a5d6a7;
  margin: 0 0 20px;
  display: block;
}

.hero h2 {
  font-size: 22px;
  font-weight: 800;
  color: #2e7d32;
  margin: 0 0 12px;
}

.hero-desc {
  color: #558b2f;
  font-size: 14px;
  line-height: 1.8;
  margin: 0;
}

/* ── Generate prompt ── */
.generate-prompt {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  min-height: 60vh;
  text-align: center;
  gap: 12px;
}

.generate-prompt-doc {
  font-size: 20px;
  font-weight: 800;
  color: #1a1a1a;
  margin: 0;
}

.generate-prompt-text {
  font-size: 15px;
  color: #78909c;
  margin: 0 0 12px;
}

.model-picker {
  display: inline-flex;
  align-items: center;
  gap: 10px;
  color: #558b2f;
  font-size: 13px;
  font-weight: 700;
}

.model-picker select {
  min-width: 210px;
  padding: 8px 12px;
  border-radius: 999px;
  border: 1px solid rgba(67, 160, 71, 0.24);
  background: white;
  color: #1a1a1a;
  font: inherit;
}

.model-picker.compact select {
  min-width: 190px;
}

/* ── Buttons ── */
.btn-primary {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 12px 22px;
  background: #43a047;
  color: white;
  border-radius: 999px;
  font-weight: 700;
  font-size: 14px;
  cursor: pointer;
  border: none;
  transition: background 0.18s, transform 0.14s, box-shadow 0.18s;
  box-shadow: 0 4px 16px rgba(67, 160, 71, 0.3);
}

.btn-primary.large {
  padding: 14px 32px;
  font-size: 15px;
}

.btn-primary:hover:not(:disabled) {
  background: #388e3c;
  transform: translateY(-2px);
  box-shadow: 0 6px 22px rgba(67, 160, 71, 0.4);
}

.btn-primary:disabled {
  background: #a5d6a7;
  cursor: not-allowed;
  box-shadow: none;
  transform: none;
}

.btn-ghost {
  padding: 6px 14px;
  border-radius: 999px;
  background: rgba(67, 160, 71, 0.1);
  color: #2e7d32;
  font-weight: 600;
  font-size: 13px;
  transition: background 0.15s;
  border: none;
  cursor: pointer;
}

.btn-ghost:hover:not(:disabled) {
  background: rgba(67, 160, 71, 0.2);
}

.btn-ghost:disabled {
  opacity: 0.45;
  cursor: not-allowed;
}

.btn-ghost.small {
  padding: 4px 12px;
  font-size: 12px;
}

/* ── Episodes ── */
.episodes-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 24px;
}

.generate-controls {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  justify-content: flex-end;
}

.episodes-doc-title {
  font-size: 20px;
  font-weight: 800;
  color: #1a1a1a;
  margin: 0 0 4px;
}

.episodes-count {
  font-size: 13px;
  color: #90a4ae;
  margin: 0;
}

.episode-list {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.episode-card {
  display: grid;
  grid-template-columns: 44px 1fr 24px;
  align-items: start;
  gap: 14px;
  padding: 16px 18px;
  background: white;
  border-radius: 16px;
  border: 2px solid transparent;
  box-shadow: 0 1px 8px rgba(0, 40, 0, 0.07);
  text-align: left;
  width: 100%;
  cursor: pointer;
  transition: border-color 0.18s, box-shadow 0.18s, transform 0.13s;
}

.episode-card:hover {
  box-shadow: 0 4px 16px rgba(67, 160, 71, 0.14);
  transform: translateY(-1px);
}

.episode-card.active {
  border-color: #43a047;
  box-shadow: 0 4px 20px rgba(67, 160, 71, 0.2);
}

.ep-num {
  font-size: 20px;
  font-weight: 900;
  color: #c8e6c9;
  line-height: 1;
  padding-top: 3px;
  font-variant-numeric: tabular-nums;
}

.episode-card.active .ep-num {
  color: #43a047;
}

.ep-body {
  min-width: 0;
}

.ep-title {
  font-size: 15px;
  font-weight: 700;
  color: #1a1a1a;
  margin-bottom: 3px;
  line-height: 1.35;
}

.ep-meta {
  font-size: 12px;
  color: #90a4ae;
}

.ep-summary-wrap {
  display: grid;
  grid-template-rows: 0fr;
  transition: grid-template-rows 0.28s ease;
}

.ep-summary-wrap.open {
  grid-template-rows: 1fr;
}

.ep-summary {
  overflow: hidden;
  padding-top: 0;
  transition: padding-top 0.28s ease;
  font-size: 13px;
  color: #546e7a;
  line-height: 1.7;
  white-space: pre-wrap;
}

.ep-summary-wrap.open .ep-summary {
  padding-top: 10px;
}

.badge-audio {
  font-size: 18px;
  color: #43a047;
  padding-top: 2px;
}

.badge-none {
  font-size: 18px;
  color: #cfd8dc;
  padding-top: 2px;
}

/* ── Now Playing ── */
.player {
  position: fixed;
  bottom: 0;
  right: 0;
  transition: left 0.25s ease;
  z-index: 200;
  background: rgba(255, 255, 255, 0.96);
  backdrop-filter: blur(20px);
  border-top: 1.5px solid #e8f5e9;
  box-shadow: 0 -4px 24px rgba(0, 60, 0, 0.08);
}

.player-inner {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 12px 32px;
}

.player-info {
  flex: 0 0 200px;
  min-width: 0;
  overflow: hidden;
}

.now-playing-label {
  display: block;
  font-size: 10px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.1em;
  color: #81c784;
  margin-bottom: 2px;
}

.now-playing-title {
  display: block;
  font-size: 13px;
  font-weight: 700;
  color: #2e7d32;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.player-controls {
  flex: 1;
  min-width: 0;
}

.audio-el {
  width: 100%;
  height: 36px;
  accent-color: #43a047;
}

.no-audio {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 13px;
  color: #90a4ae;
}

.transcript-toggle {
  flex-shrink: 0;
  width: 36px;
  height: 36px;
  border-radius: 50%;
  background: rgba(67, 160, 71, 0.1);
  color: #2e7d32;
  font-size: 15px;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: background 0.15s;
  border: none;
  cursor: pointer;
}

.transcript-toggle:hover {
  background: rgba(67, 160, 71, 0.2);
}

.transcript {
  border-top: 1px solid #f1f8e9;
  padding: 14px 32px;
  max-height: 180px;
  overflow-y: auto;
}

.transcript p {
  margin: 0;
  font-size: 13px;
  line-height: 1.85;
  color: #546e7a;
  white-space: pre-wrap;
}

/* ── Chat transcript ── */
.chat-turn {
  display: flex;
  flex-direction: column;
  margin-bottom: 10px;
}

.chat-turn.zundamon {
  align-items: flex-start;
}

.chat-turn.metan {
  align-items: flex-end;
}

.chat-speaker {
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.05em;
  margin-bottom: 3px;
  text-transform: none;
}

.chat-turn.zundamon .chat-speaker {
  color: #2e7d32;
}

.chat-turn.metan .chat-speaker {
  color: #6a1b9a;
}

.chat-bubble {
  max-width: 82%;
  padding: 8px 12px;
  border-radius: 14px;
  font-size: 13px;
  line-height: 1.7;
}

.chat-turn.zundamon .chat-bubble {
  background: #e8f5e9;
  color: #1b5e20;
  border-bottom-left-radius: 4px;
}

.chat-turn.metan .chat-bubble {
  background: #f3e5f5;
  color: #4a148c;
  border-bottom-right-radius: 4px;
}

/* ── Transitions ── */
.player-slide-enter-active,
.player-slide-leave-active {
  transition: transform 0.28s cubic-bezier(0.4, 0, 0.2, 1);
}

.player-slide-enter-from,
.player-slide-leave-to {
  transform: translateY(100%);
}

.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.2s;
}

.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}

</style>

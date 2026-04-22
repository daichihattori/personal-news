<script setup>
import { computed, onMounted, ref } from 'vue'

const apiBase = import.meta.env.VITE_API_BASE_URL || 'http://127.0.0.1:3000'

const documents = ref([])
const selectedDocumentId = ref('')
const chunks = ref([])
const selectedChunkId = ref('')
const selectedChunk = ref(null)

const statusMessage = ref('Backend からデータを読み込みます。')
const uploadMessage = ref('')
const qaQuestion = ref('')
const qaAnswer = ref('')
const qaReferences = ref([])
const uploadInput = ref(null)

const loadingDocuments = ref(false)
const loadingChunks = ref(false)
const loadingChunkDetail = ref(false)
const generatingDocument = ref(false)
const generatingChunk = ref(false)
const generatingAudio = ref(false)
const askingQuestion = ref(false)
const uploading = ref(false)

const selectedDocument = computed(() =>
  documents.value.find((document) => document.id === selectedDocumentId.value) || null
)

const audioUrl = computed(() => {
  const path = selectedChunk.value?.audio_path
  if (!path) return ''
  if (path.startsWith('http://') || path.startsWith('https://')) return path
  return `${apiBase}${path}`
})

const canActOnChunk = computed(() => Boolean(selectedChunk.value))

onMounted(async () => {
  await refreshDocuments()
})

async function request(path, options = {}) {
  const response = await fetch(`${apiBase}${path}`, options)
  if (!response.ok) {
    let message = `Request failed: ${response.status}`
    try {
      const json = await response.json()
      if (json.error) message = json.error
    } catch {
      // Ignore JSON parse errors for non-JSON responses.
    }
    throw new Error(message)
  }
  return response
}

async function refreshDocuments() {
  loadingDocuments.value = true
  try {
    const response = await request('/api/documents')
    documents.value = await response.json()

    if (!documents.value.length) {
      selectedDocumentId.value = ''
      chunks.value = []
      selectedChunkId.value = ''
      selectedChunk.value = null
      statusMessage.value = 'PDF をアップロードすると document がここに並びます。'
      return
    }

    if (!documents.value.some((document) => document.id === selectedDocumentId.value)) {
      selectedDocumentId.value = documents.value[0].id
    }

    statusMessage.value = 'Document 一覧を読み込みました。'
    await refreshChunks(selectedDocumentId.value)
  } catch (error) {
    statusMessage.value = `Document 読み込み失敗: ${error.message}`
  } finally {
    loadingDocuments.value = false
  }
}

async function refreshChunks(documentId = selectedDocumentId.value) {
  if (!documentId) return

  loadingChunks.value = true
  try {
    const response = await request(`/api/documents/${documentId}/chunks`)
    chunks.value = await response.json()

    if (!chunks.value.length) {
      selectedChunkId.value = ''
      selectedChunk.value = null
      statusMessage.value = 'この document に chunk はまだありません。'
      return
    }

    if (!chunks.value.some((chunk) => chunk.id === selectedChunkId.value)) {
      selectedChunkId.value = chunks.value[0].id
    }

    statusMessage.value = `${chunks.value.length} 件の chunk を読み込みました。`
    await refreshChunkDetail(selectedChunkId.value)
  } catch (error) {
    statusMessage.value = `Chunk 一覧読み込み失敗: ${error.message}`
  } finally {
    loadingChunks.value = false
  }
}

async function refreshChunkDetail(chunkId = selectedChunkId.value) {
  if (!chunkId) return

  loadingChunkDetail.value = true
  qaAnswer.value = ''
  qaReferences.value = []
  try {
    const response = await request(`/api/chunks/${chunkId}`)
    selectedChunk.value = await response.json()
    statusMessage.value = `Chunk ${selectedChunk.value.page_start}-${selectedChunk.value.page_end} を表示しています。`
  } catch (error) {
    statusMessage.value = `Chunk 詳細読み込み失敗: ${error.message}`
  } finally {
    loadingChunkDetail.value = false
  }
}

async function onDocumentChange(event) {
  selectedDocumentId.value = event.target.value
  selectedChunkId.value = ''
  selectedChunk.value = null
  await refreshChunks(selectedDocumentId.value)
}

async function onChunkSelect(chunkId) {
  selectedChunkId.value = chunkId
  await refreshChunkDetail(chunkId)
}

async function uploadPdf(event) {
  const [file] = event.target.files || []
  if (!file) return

  const formData = new FormData()
  formData.append('file', file)

  uploading.value = true
  uploadMessage.value = ''
  try {
    const response = await request('/api/documents', {
      method: 'POST',
      body: formData
    })
    const result = await response.json()
    uploadMessage.value = `${result.document.title} をアップロードしました。`
    selectedDocumentId.value = result.document.id
    await refreshDocuments()
  } catch (error) {
    uploadMessage.value = `アップロード失敗: ${error.message}`
  } finally {
    uploading.value = false
    if (uploadInput.value) uploadInput.value.value = ''
  }
}

async function generateDocument() {
  if (!selectedDocumentId.value) return
  generatingDocument.value = true
  try {
    await request(`/api/documents/${selectedDocumentId.value}/generate`, { method: 'POST' })
    statusMessage.value = 'Document 全体の生成が完了しました。'
    await refreshChunks(selectedDocumentId.value)
  } catch (error) {
    statusMessage.value = `Document 生成失敗: ${error.message}`
  } finally {
    generatingDocument.value = false
  }
}

async function generateSelectedChunk() {
  if (!selectedChunkId.value) return
  generatingChunk.value = true
  try {
    const response = await request(`/api/chunks/${selectedChunkId.value}/generate`, {
      method: 'POST'
    })
    const result = await response.json()
    selectedChunk.value = result.chunk
    statusMessage.value = '選択中 chunk の要約を更新しました。'
    await refreshChunks(selectedDocumentId.value)
  } catch (error) {
    statusMessage.value = `Chunk 生成失敗: ${error.message}`
  } finally {
    generatingChunk.value = false
  }
}

async function generateSelectedAudio() {
  if (!selectedChunkId.value) return
  generatingAudio.value = true
  try {
    const response = await request(`/api/chunks/${selectedChunkId.value}/audio`, {
      method: 'POST'
    })
    const result = await response.json()
    selectedChunk.value = result.chunk
    statusMessage.value = '音声生成が完了しました。'
    await refreshChunks(selectedDocumentId.value)
  } catch (error) {
    statusMessage.value = `音声生成失敗: ${error.message}`
  } finally {
    generatingAudio.value = false
  }
}

async function askQuestion() {
  if (!selectedChunkId.value || !qaQuestion.value.trim()) return

  askingQuestion.value = true
  qaAnswer.value = ''
  qaReferences.value = []
  try {
    const response = await request(`/api/chunks/${selectedChunkId.value}/qa`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json'
      },
      body: JSON.stringify({
        question: qaQuestion.value
      })
    })
    const result = await response.json()
    qaAnswer.value = result.answer
    qaReferences.value = result.references || []
    statusMessage.value = 'Q&A の回答を受け取りました。'
  } catch (error) {
    statusMessage.value = `Q&A 失敗: ${error.message}`
  } finally {
    askingQuestion.value = false
  }
}
</script>

<template>
  <div class="app-shell">
    <aside class="sidebar">
      <div class="panel brand-panel">
        <p class="eyebrow">Local Control Panel</p>
        <h1>PDF Reading Radio</h1>
        <p class="muted">
          Claude 要約、VOICEVOX 音声化、Q&A を 1 画面で試すための Vue フロントです。
        </p>
      </div>

      <div class="panel upload-panel">
        <label class="panel-title">PDF Upload</label>
        <input ref="uploadInput" type="file" accept="application/pdf" @change="uploadPdf" />
        <p class="helper" v-if="uploading">アップロード中...</p>
        <p class="helper" v-else-if="uploadMessage">{{ uploadMessage }}</p>
      </div>

      <div class="panel document-panel">
        <div class="panel-header">
          <span class="panel-title">Documents</span>
          <button class="ghost-button" @click="refreshDocuments" :disabled="loadingDocuments">
            再読込
          </button>
        </div>
        <select class="document-select" :value="selectedDocumentId" @change="onDocumentChange">
          <option disabled value="">Document を選択</option>
          <option v-for="document in documents" :key="document.id" :value="document.id">
            {{ document.title }}
          </option>
        </select>
        <div class="document-actions">
          <button class="primary-button" @click="generateDocument" :disabled="!selectedDocumentId || generatingDocument">
            {{ generatingDocument ? '生成中...' : 'Document 全体を生成' }}
          </button>
        </div>
      </div>

      <div class="panel chunk-panel">
        <div class="panel-header">
          <span class="panel-title">Chunks</span>
          <span class="helper">{{ chunks.length }} 件</span>
        </div>
        <div class="chunk-list">
          <button
            v-for="chunk in chunks"
            :key="chunk.id"
            class="chunk-item"
            :class="{ active: chunk.id === selectedChunkId }"
            @click="onChunkSelect(chunk.id)"
          >
            <span class="chunk-pages">{{ chunk.page_start }}-{{ chunk.page_end }}</span>
            <span class="chunk-title">{{ chunk.title }}</span>
          </button>
        </div>
      </div>
    </aside>

    <main class="workspace">
      <div class="workspace-top">
        <div class="status-bar">
          <span class="status-label">Status</span>
          <span>{{ statusMessage }}</span>
        </div>
      </div>

      <div v-if="selectedChunk" class="content-grid">
        <section class="panel content-panel">
          <div class="panel-header">
            <div>
              <p class="eyebrow">Selected Chunk</p>
              <h2>{{ selectedChunk.title }}</h2>
              <p class="helper">pages {{ selectedChunk.page_start }}-{{ selectedChunk.page_end }}</p>
            </div>
            <div class="action-row">
              <button class="primary-button" @click="generateSelectedChunk" :disabled="!canActOnChunk || generatingChunk">
                {{ generatingChunk ? '生成中...' : '要約を生成' }}
              </button>
              <button class="secondary-button" @click="generateSelectedAudio" :disabled="!canActOnChunk || generatingAudio">
                {{ generatingAudio ? '音声生成中...' : '音声を生成' }}
              </button>
            </div>
          </div>

          <div class="text-block">
            <h3>Summary</h3>
            <p>{{ selectedChunk.summary_text }}</p>
          </div>

          <div class="text-block">
            <h3>Dialogue Script</h3>
            <p>{{ selectedChunk.dialogue_script }}</p>
          </div>

          <div class="text-block">
            <h3>Key Points</h3>
            <ul class="flat-list">
              <li v-for="point in selectedChunk.key_points" :key="point">{{ point }}</li>
            </ul>
          </div>

          <details class="source-block">
            <summary>Source Text</summary>
            <pre>{{ selectedChunk.source_text }}</pre>
          </details>
        </section>

        <section class="stack">
          <div class="panel audio-panel">
            <div class="panel-header">
              <span class="panel-title">Audio</span>
              <a v-if="audioUrl" class="ghost-button link-button" :href="audioUrl" target="_blank" rel="noreferrer">
                別タブで開く
              </a>
            </div>
            <audio v-if="audioUrl" :src="audioUrl" controls class="audio-player"></audio>
            <p v-else class="helper">まだ音声は生成されていません。</p>
          </div>

          <div class="panel qa-panel">
            <div class="panel-header">
              <span class="panel-title">Q&A</span>
            </div>
            <textarea
              v-model="qaQuestion"
              class="qa-input"
              rows="5"
              placeholder="この範囲について聞きたいことを書いてください。"
            />
            <button class="primary-button full-width" @click="askQuestion" :disabled="askingQuestion || !qaQuestion.trim()">
              {{ askingQuestion ? '質問中...' : '質問する' }}
            </button>
            <div v-if="qaAnswer" class="qa-answer">
              <h3>Answer</h3>
              <p>{{ qaAnswer }}</p>
              <ul v-if="qaReferences.length" class="flat-list references">
                <li v-for="reference in qaReferences" :key="reference">{{ reference }}</li>
              </ul>
            </div>
          </div>
        </section>
      </div>

      <div v-else class="empty-state panel">
        <h2>Chunk を選ぶとここに内容が出ます</h2>
        <p class="muted">
          まず document を選んで、左の chunk 一覧から 1 件選択してください。
        </p>
      </div>
    </main>
  </div>
</template>

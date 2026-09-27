<script setup lang="ts">
/**
 * 三栏外壳与操作区。
 *
 * 布局：左侧文章列表，中间 Markdown 编辑，右侧即时排版。
 * 窗口过窄时切为「编辑／预览」选项卡，不强塞双列。
 * 顶部操作保持明显分离：本地保存状态、同步到写作分支、发布到网站、
 * 从网站撤下、删除文件——每次危险操作都先展示确认清单。
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import ArticleList from '@/components/ArticleList.vue'
import ArticleMeta from '@/components/ArticleMeta.vue'
import MarkdownEditor from '@/components/MarkdownEditor.vue'
import ModalDialog from '@/components/ModalDialog.vue'
import OperationStatus from '@/components/OperationStatus.vue'
import ReadingPreview from '@/components/ReadingPreview.vue'
import RecentlyDeleted from '@/components/RecentlyDeleted.vue'
import SyncDiff from '@/components/SyncDiff.vue'
import { useArticle } from '@/composables/useArticle'
import { PUBLIC_DISCLOSURE } from '@/types/article'
import { filterArticles, searchArticles } from '@/services/backend'
import type {
  ArticleFilter,
  ArticleMeta as Meta,
  DeleteAssessment,
  DeploymentStatus,
  EditorMode,
  PreviewSessionInfo,
  PublishPrecheck,
  RenameAssessment,
  WriterError,
} from '@/types/article'

const view = useArticle()

// ---- 首次连接 ----
const workspaceInput = ref('')

// ---- 界面状态 ----
const activeMobilePane = ref<'editor' | 'preview'>('editor')
const showTrash = ref(false)
const showPreferences = ref(false)
const showRenameDialog = ref(false)
const showCreateDialog = ref(false)
const showImportDialog = ref(false)
const showPublishDialog = ref(false)
const showDeleteDialog = ref(false)
const showWithdrawDialog = ref(false)
/** 「同步到写作分支」的确认对话框（Ctrl+Shift+S 与按钮都先经它）。 */
const showSyncDialog = ref(false)
const showRecoveryNotice = ref(false)

const newArticleId = ref('')
const newTitle = ref('')
const newDescription = ref('')
const newPubDate = ref('')
const importPath = ref('')
const importId = ref('')

const renameNewId = ref('')
const renameAssessment = ref<RenameAssessment | null>(null)

const precheck = ref<PublishPrecheck | null>(null)
const deleteAssessment = ref<DeleteAssessment | null>(null)
const deployment = ref<DeploymentStatus | null>(null)

const busy = ref(false)
const actionError = ref<WriterError | null>(null)
const actionNotice = ref<string | null>(null)
const confirmRenamePublished = ref(false)

const prefsDraft = ref({
  fontSize: 16,
  lineHeight: 175,
  codeTheme: 'github',
  previewWidth: 460,
  autoSaveDebounceMs: 800,
})

const editorMode = ref<EditorMode>('sv')
const roundTripNotice = ref<string | null>(null)

/** 窄窗口下切换为选项卡布局。 */
const isNarrow = ref(false)
const editorPane = ref<{
  load: (body: string) => void
  insertText: (snippet: string) => void
} | null>(null)

/**
 * 把一段 Markdown 片段插入正文。
 *
 * 优先走编辑器的光标插入；编辑器尚未就绪时**回退**为追加到正文状态，
 * 保证引用绝不会因为某个环节没就绪而静默丢失。
 */
function insertIntoBody(snippet: string): void {
  const pane = editorPane.value
  if (pane?.insertText) {
    pane.insertText(snippet)
    return
  }
  view.draftBody.value = `${view.draftBody.value}${snippet}`
}

/**
 * 用当前正文刷新编辑器。
 *
 * 编辑器组件自身已 watch 了 `v-model`，因此这里主要服务于「编辑器重建后需要
 * 立即载入最新内容」的场景；组件未就绪时是无操作，正文状态本身不受影响。
 */
function syncEditorFromBody(): void {
  const pane = editorPane.value
  if (pane?.load) {
    pane.load(view.draftBody.value)
  }
}

function updateNarrow(): void {
  isNarrow.value = window.innerWidth < 980
}

onMounted(async () => {
  updateNarrow()
  window.addEventListener('resize', updateNarrow)
  await view.refreshConnection()
  if (view.connection.value?.connected) {
    await view.refreshAll()
    if (view.recovery.value.length > 0) {
      showRecoveryNotice.value = true
    }
  }
  // 默认新建日期为本地今天，由后端给出更可靠的日期。
  newPubDate.value = new Date().toISOString().slice(0, 10)
})

onBeforeUnmount(() => {
  window.removeEventListener('resize', updateNarrow)
  void view.stopSitePreview()
})

// 偏好同步到草稿。
watch(
  () => view.preferences.value,
  (value) => {
    if (!value) return
    prefsDraft.value = {
      fontSize: value.fontSize,
      lineHeight: value.lineHeight,
      codeTheme: value.codeTheme,
      previewWidth: value.previewWidth,
      autoSaveDebounceMs: value.autoSaveDebounceMs,
    }
    editorMode.value = value.editorMode
  },
  { immediate: true },
)

// ---- 派生数据 ----
const draftMeta = computed<Meta | null>(() => view.draftMeta.value)
const currentStatus = computed(() => view.current.value?.status ?? null)

const filterCounts = computed(() => {
  const all = view.articles.value
  const globalQuery = ''
  const counts: Record<string, number> = { all: searchArticles(all, globalQuery).length }
  for (const value of ['local-only', 'remote-saved', 'site-published', 'conflict'] as ArticleFilter[]) {
    counts[value] = filterArticles(all, value).length
  }
  return counts
})

const fieldsWithError = computed(() => {
  const detail = view.saveError.value?.detail
  return detail ? [detail] : []
})

const updatedDateIsSet = computed(() => Boolean(draftMeta.value?.updatedDate))

const publishedOnSite = computed(() => {
  const site = currentStatus.value?.site
  return site === 'live-current-version' || site === 'live-old-version'
})

const recoveryArticleIds = computed(() => view.recovery.value.map((item) => item.articleId))

/** 键盘快捷键：Ctrl+S 本地保存，Ctrl+Shift+S 打开同步确认，Ctrl+P 切到预览。 */
function handleKeydown(event: KeyboardEvent): void {
  if (!(event.ctrlKey || event.metaKey)) return
  const key = event.key.toLowerCase()
  if (key === 's' && event.shiftKey) {
    // 方案 §4.2：Ctrl+Shift+S 先打开同步确认，不直接执行。
    event.preventDefault()
    requestSync()
    return
  }
  if (key === 's') {
    event.preventDefault()
    void view.flush()
    return
  }
  if (key === 'p') {
    event.preventDefault()
    // 窄屏切到预览选项卡；宽屏双栏下预览本就常显，仅阻止浏览器打印。
    activeMobilePane.value = 'preview'
    return
  }
}

onMounted(() => {
  window.addEventListener('keydown', handleKeydown)
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', handleKeydown)
})

/** 统一包装一次操作，负责 busy 与错误提示。 */
async function run(action: () => Promise<void>): Promise<void> {
  busy.value = true
  actionError.value = null
  actionNotice.value = null
  try {
    await action()
  } catch (error) {
    actionError.value = error as WriterError
  } finally {
    busy.value = false
  }
}

// ---- 连接 ----
async function doConnect(): Promise<void> {
  await run(async () => {
    await view.connect(workspaceInput.value.trim() || undefined)
    showRecoveryNotice.value = view.recovery.value.length > 0
  })
}

async function doAcknowledge(): Promise<void> {
  await run(async () => {
    await view.acknowledgeDisclosure()
  })
}

// ---- 文章 ----
async function selectArticle(id: string): Promise<void> {
  await run(async () => {
    await view.openArticle(id)
    deployment.value = null
    roundTripNotice.value = null
    syncEditorFromBody()
  })
}

async function doCreate(): Promise<void> {
  await run(async () => {
    const meta: Meta = {
      title: newTitle.value.trim(),
      description: newDescription.value.trim(),
      pubDate: newPubDate.value,
      tags: [],
      draft: true,
    }
    await view.createArticle(newArticleId.value.trim(), meta)
    syncEditorFromBody()
    showCreateDialog.value = false
    newArticleId.value = ''
    newTitle.value = ''
    newDescription.value = ''
  })
}

async function doImport(): Promise<void> {
  await run(async () => {
    await view.importArticle(importPath.value.trim(), importId.value.trim())
    syncEditorFromBody()
    showImportDialog.value = false
    importPath.value = ''
    importId.value = ''
  })
}

/**
 * 插入图片：用系统文件对话框选择，后端校验后归档到 `public/blog/<article-id>/`，
 * 再把站点根路径的引用插入正文。
 */
async function insertImage(): Promise<void> {
  const id = view.current.value?.id
  if (!id) return
  await run(async () => {
    const { open } = await import('@tauri-apps/plugin-dialog')
    const selected = await open({
      multiple: false,
      directory: false,
      filters: [{ name: '图片', extensions: ['png', 'jpg', 'jpeg', 'webp', 'gif'] }],
    })
    if (typeof selected !== 'string') {
      // 用户取消：不是错误，不提示。
      return
    }
    const image = await view.insertImage(id, selected)
    insertIntoBody(insertSnippet(image))
    actionNotice.value = `已插入图片 ${image.fileName}（${(image.size / 1024).toFixed(1)} KiB），归档到 ${image.relPath}`
  })
}

/** 生成插入正文的片段（独立成段，便于排版与后续引用检查）。 */
function insertSnippet(image: { fileName: string; url: string }): string {
  return `\n![${image.fileName}](${image.url})\n`
}

/**
 * 插入来自剪贴板粘贴或拖入的图片字节。
 *
 * 与文件选择插入共用后端同一套校验与归档逻辑；归档成功后才把引用写进正文，
 * 因此「图片尚未复制完成前不允许远程同步」这一约束自然满足。
 */
async function insertImageFromEvent(payload: {
  fileName: string
  bytes: Uint8Array
  origin: 'pasted' | 'dropped'
}): Promise<void> {
  const id = view.current.value?.id
  if (!id) return
  await run(async () => {
    const image = await view.insertImageBytes(id, payload.fileName, payload.bytes)
    insertIntoBody(insertSnippet(image))
    const source = payload.origin === 'pasted' ? '粘贴' : '拖入'
    actionNotice.value = `已插入${source}的图片 ${image.fileName}（${(image.size / 1024).toFixed(1)} KiB），归档到 ${image.relPath}`
  })
}

/** 粘贴或拖入无法作为图片插入时的说明。 */
function onImageRejected(reason: string): void {
  actionError.value = { code: 'image-unsupported', message: reason }
}

// ---- 同步 ----
/** 同步前先让用户确认目标与影响（Ctrl+Shift+S 与按钮都走这里）。 */
function requestSync(): void {
  if (!view.current.value) return
  showSyncDialog.value = true
}

/** 用户在确认对话框中选择「确认同步」。 */
async function confirmSync(): Promise<void> {
  showSyncDialog.value = false
  await doSync()
}

async function doSync(adoptLocal = false): Promise<void> {
  const id = view.current.value?.id
  if (!id) return
  await run(async () => {
    await view.syncArticle(id, adoptLocal)
    if (view.conflict.value) {
      actionNotice.value = '远端这篇文章已变化，请先在差异界面选择处理方式。'
    } else {
      actionNotice.value = '已保存到写作分支。网站版本未改变。'
    }
  })
}

async function doAdoptRemote(): Promise<void> {
  const id = view.current.value?.id
  if (!id) return
  await run(async () => {
    await view.adoptRemote(id)
    syncEditorFromBody()
    actionNotice.value = '已采用远端版本；本地修改已存入恢复副本。'
  })
}

async function doManualMerge(): Promise<void> {
  await run(async () => {
    await view.resolveConflictManually()
    actionNotice.value = '已保存手工合并的结果。'
  })
}

// ---- 发布 ----
async function openPublishDialog(): Promise<void> {
  const id = view.current.value?.id
  if (!id) return
  await run(async () => {
    precheck.value = await view.publishPrecheck(id)
    showPublishDialog.value = true
  })
}

async function doPublish(): Promise<void> {
  const id = view.current.value?.id
  if (!id) return
  await run(async () => {
    const outcome = await view.publishArticle(id)
    showPublishDialog.value = false
    actionNotice.value = `已提交发布（${outcome.commit.slice(0, 8)}）。部署结果需另行核实。`
    deployment.value = await view.deploymentStatus(id)
  })
}

async function checkDeployment(): Promise<void> {
  const id = view.current.value?.id
  if (!id) return
  await run(async () => {
    deployment.value = await view.deploymentStatus(id)
  })
}

// ---- 撤下 ----
async function openWithdrawDialog(): Promise<void> {
  showWithdrawDialog.value = true
}

async function doWithdraw(): Promise<void> {
  const id = view.current.value?.id
  if (!id) return
  await run(async () => {
    await view.withdrawArticle(id)
    showWithdrawDialog.value = false
    actionNotice.value = '已从网站撤下。写作分支中的稿件保持不变。'
    await selectArticle(id)
  })
}

// ---- 删除 ----
async function openDeleteDialog(): Promise<void> {
  const id = view.current.value?.id
  if (!id) return
  await run(async () => {
    deleteAssessment.value = await view.assessDelete(id)
    showDeleteDialog.value = true
  })
}

async function doDelete(): Promise<void> {
  const id = view.current.value?.id
  if (!id) return
  await run(async () => {
    const outcome = await view.deleteArticle(id)
    showDeleteDialog.value = false
    actionNotice.value = outcome.writingDone && outcome.mainDone
      ? '删除完成。本地保留了可恢复副本。'
      : `${outcome.branchState}。可在最近删除中补做未完成的分支。`
    showTrash.value = true
  })
}

// ---- 回收区 ----
async function doRestore(opId: string): Promise<void> {
  await run(async () => {
    await view.restoreArticle(opId)
    showTrash.value = false
    syncEditorFromBody()
    actionNotice.value = '已恢复为本地未发布草稿。需要手动同步与发布才会重新上线。'
  })
}

async function doRetryDelete(opId: string): Promise<void> {
  await run(async () => {
    const outcome = await view.retryDelete(opId)
    actionNotice.value = outcome.writingDone && outcome.mainDone
      ? '未完成的分支已补做完成。'
      : outcome.branchState
  })
}

async function doPurge(opId: string): Promise<void> {
  await run(async () => {
    await view.purgeTrash(opId)
  })
}

async function doDiscardRecovery(articleId: string): Promise<void> {
  await run(async () => {
    await view.discardRecovery(articleId)
  })
}

/** 恢复一篇崩溃副本（把未保存内容写回文章文件）。 */
async function doRestoreRecovery(articleId: string): Promise<void> {
  await run(async () => {
    await view.restoreRecovery(articleId)
    showTrash.value = false
    actionNotice.value =
      '已恢复未保存的内容。它只写入本地文件，仍需你确认后手动同步与发布。'
  })
}

/** 逐篇恢复全部崩溃副本。 */
async function restoreAllRecovery(): Promise<void> {
  const pending = [...view.recovery.value]
  if (pending.length === 0) return
  await run(async () => {
    let restored = 0
    for (const draft of pending) {
      await view.restoreRecovery(draft.articleId)
      restored += 1
    }
    showRecoveryNotice.value = false
    actionNotice.value = `已恢复 ${restored} 篇未保存的内容（仅本地，尚未同步或发布）。`
  })
}

// ---- URL 改名 ----
async function openRenameDialog(): Promise<void> {
  const id = view.current.value?.id
  if (!id) return
  renameNewId.value = id
  renameAssessment.value = null
  confirmRenamePublished.value = false
  showRenameDialog.value = true
}

async function assessRename(): Promise<void> {
  const id = view.current.value?.id
  if (!id) return
  await run(async () => {
    renameAssessment.value = await view.assessRename(id, renameNewId.value.trim())
  })
}

async function doRename(): Promise<void> {
  const id = view.current.value?.id
  if (!id) return
  await run(async () => {
    await view.renameUrl(id, renameNewId.value.trim(), confirmRenamePublished.value)
    showRenameDialog.value = false
    syncEditorFromBody()
    actionNotice.value = '已更改 URL 标识。旧链接可能失效；已发布文章需重新发布。'
  })
}

// ---- 元数据 ----
function updateMeta(value: Meta): void {
  view.draftMeta.value = value
}

async function setUpdatedDate(value: string): Promise<void> {
  if (!value) {
    await view.flush({ action: 'remove' })
    return
  }
  await view.flush({ action: 'set', value })
}

async function clearUpdatedDate(): Promise<void> {
  await view.flush({ action: 'remove' })
}

// ---- 偏好与预览 ----
async function savePreferences(): Promise<void> {
  await run(async () => {
    const current = view.preferences.value
    await view.savePreferences({
      ...prefsDraft.value,
      editorMode: editorMode.value,
      codeTheme: current?.codeTheme ?? prefsDraft.value.codeTheme,
    })
    showPreferences.value = false
    actionNotice.value = '写作外观偏好已保存到本机，未修改文章或网站样式。'
  })
}

async function startWebsitePreview(simulatePublic: boolean): Promise<void> {
  const id = view.current.value?.id
  if (!id) return
  await run(async () => {
    const info: PreviewSessionInfo = await view.startSitePreview(id, simulatePublic)
    if (info.offlineFontNotice) {
      actionNotice.value = info.offlineFontNotice
    }
  })
}

async function stopWebsitePreview(): Promise<void> {
  await run(async () => {
    await view.stopSitePreview()
  })
}

async function openExternal(url: string): Promise<void> {
  // 用系统默认浏览器打开，避免把第三方页面放进拥有本地命令权限的 webview。
  try {
    const { openUrl } = await import('@tauri-apps/plugin-opener')
    await openUrl(url)
  } catch {
    actionError.value = { code: 'invalid-argument', message: `无法打开外部链接：${url}` }
  }
}

function onRoundTripNotice(message: string): void {
  roundTripNotice.value = message
}

async function changeEditorMode(mode: EditorMode): Promise<void> {
  editorMode.value = mode
  const current = view.preferences.value
  if (current) {
    await view.savePreferences({ ...current, editorMode: mode })
  }
}

const connectionReady = computed(() => view.connection.value?.connected === true)
/**
 * 公开性说明必须先被确认。
 *
 * 后端不可达（`connection === null`）时同样要求确认，确保「公开仓库会公开远程
 * 草稿」这一事实不会被跳过。
 */
const disclosureMissing = computed(
  () => !(view.connection.value?.disclosedPublicDrafts === true),
)

/** 披露文案：优先后端返回，后端不可达时用前端常量兜底。 */
const disclosureText = computed(
  () => view.connection.value?.publicDisclosure ?? PUBLIC_DISCLOSURE,
)
</script>

<template>
  <div class="app-shell">
    <!-- 首次启动：仓库身份、克隆目录与前置条件 -->
    <section v-if="!connectionReady" class="onboarding panel" aria-label="首次启动">
      <h1>观澜志写作</h1>
      <p class="subtle">
        在一个界面中管理文章列表、中文 Markdown 写作、两层预览、远程保存与按篇发布。
      </p>

      <dl class="facts">
        <div>
          <dt>目标仓库（只读）</dt>
          <dd>{{ view.connection.value?.repoLabel ?? '—' }}</dd>
        </div>
        <div>
          <dt>工作目录</dt>
          <dd class="mono">{{ view.connection.value?.workspaceDir || '尚未创建' }}</dd>
        </div>
      </dl>

      <div v-if="view.connection.value" class="toolchain">
        <h2 class="section-title">前置条件检测</h2>
        <ul class="tool-list">
          <li>
            Git：<span :class="view.connection.value.toolchain.git.available ? '' : 'error-text'">
              {{ view.connection.value.toolchain.git.version ?? '未找到' }}
            </span>
          </li>
          <li>
            Node.js：<span :class="view.connection.value.toolchain.nodeMeetsMinimum ? '' : 'error-text'">
              {{ view.connection.value.toolchain.node.version ?? '未找到' }}
            </span>
            <span class="subtle">（网站预览需要 >= 22.12.0）</span>
          </li>
          <li>
            pnpm：<span :class="view.connection.value.toolchain.pnpm.available ? '' : 'error-text'">
              {{ view.connection.value.toolchain.pnpm.version ?? '未找到' }}
            </span>
          </li>
        </ul>
        <p v-if="view.connection.value.toolchain.missing.length > 0" class="warn-box">
          缺少：{{ view.connection.value.toolchain.missing.join('；') }}。<br />
          文章编辑与即时排版仍可用，网站预览需要先补齐这些前置条件。
          <span v-for="tip in view.connection.value.toolchain.guidance" :key="tip" class="subtle block">
            · {{ tip }}
          </span>
        </p>
        <p v-else class="subtle">前置条件齐备，网站预览可用。</p>
      </div>

      <p class="disclosure">{{ disclosureText }}</p>

      <div class="onboarding-actions">
        <button
          v-if="disclosureMissing"
          type="button"
          class="primary"
          :disabled="busy"
          @click="doAcknowledge"
        >
          我已了解，继续
        </button>
        <template v-else>
          <label class="sr-only" for="workspace-dir">工作目录（留空使用默认位置）</label>
          <input
            id="workspace-dir"
            type="text"
            :value="workspaceInput"
            placeholder="留空使用软件默认工作目录（推荐）"
            @input="workspaceInput = ($event.target as HTMLInputElement).value"
          />
          <button type="button" class="primary" :disabled="busy" @click="doConnect">
            连接并建立独立工作目录
          </button>
        </template>
      </div>

      <p v-if="view.connectionError.value" class="error-text" role="alert">
        {{ view.connectionError.value.message }}
      </p>
      <p v-if="actionError" class="error-text" role="alert">{{ actionError.message }}</p>
    </section>

    <template v-else>
      <!-- 顶部操作区 -->
      <header class="topbar">
        <div class="topbar-left">
          <strong class="brand">观澜志写作</strong>
          <button type="button" class="ghost" :disabled="busy" @click="showPreferences = true">
            写作外观
          </button>
          <button type="button" class="ghost" :disabled="busy" @click="showTrash = true">
            最近删除<span v-if="view.trash.value.length" class="count">{{ view.trash.value.length }}</span>
          </button>
        </div>

        <div class="topbar-actions">
          <button type="button" :disabled="busy || !view.current.value" @click="view.flush()">
            本地保存 (Ctrl+S)
          </button>
          <button
            type="button"
            :disabled="busy || !view.current.value"
            @click="showSyncDialog = true"
          >
            同步到写作分支 (Ctrl+Shift+S)
          </button>
          <button
            type="button"
            class="primary"
            :disabled="busy || !view.current.value"
            @click="openPublishDialog"
          >
            发布到网站…
          </button>
          <button
            type="button"
            :disabled="busy || !view.current.value"
            @click="openWithdrawDialog"
          >
            从网站撤下…
          </button>
          <button
            type="button"
            class="danger"
            :disabled="busy || !view.current.value"
            @click="openDeleteDialog"
          >
            删除文件…
          </button>
        </div>
      </header>

      <OperationStatus
        :status="currentStatus"
        :deployment="deployment"
        :last-error="actionError"
        :busy="busy"
        @check-deployment="checkDeployment"
        @open-run="openExternal"
      />

      <p v-if="actionNotice" class="notice" role="status">{{ actionNotice }}</p>
      <p v-if="roundTripNotice" class="notice" role="status">{{ roundTripNotice }}</p>
      <p v-if="showRecoveryNotice && view.recovery.value.length > 0" class="notice" role="status">
        检测到 {{ view.recovery.value.length }} 份未保存的恢复副本
        <template v-for="(draft, index) in view.recovery.value" :key="draft.articleId">
          <span v-if="index === 0">：
            <code>{{ draft.articleId }}</code>
          </span>
          <span v-else-if="index < 3">、<code>{{ draft.articleId }}</code></span>
        </template>
        。可逐篇恢复或丢弃。
        <button
          type="button"
          class="ghost small"
          :disabled="busy"
          @click="restoreAllRecovery"
        >
          全部恢复
        </button>
        <button type="button" class="ghost small" :disabled="busy" @click="showTrash = true">
          逐篇处理
        </button>
      </p>

      <!-- 三栏主体 -->
      <main class="workspace" :class="{ narrow: isNarrow }">
        <ArticleList
          :articles="view.visibleArticles.value"
          :selected-id="view.current.value?.id ?? null"
          :filter="view.filter.value"
          :sort="view.sort.value"
          :query="view.query.value"
          :loading="view.listLoading.value"
          :error="view.listError.value"
          :selected-counts="filterCounts"
          @update:filter="view.filter.value = $event"
          @update:sort="view.sort.value = $event"
          @update:query="view.query.value = $event"
          @select="selectArticle"
          @create="showCreateDialog = true"
          @import="showImportDialog = true"
          @show-trash="showTrash = true"
          @retry="view.refreshList()"
        />

        <section class="editor-column">
          <div v-if="!view.current.value" class="empty-state">
            <p>从左侧选择一篇文章，或新建一篇开始写作。</p>
          </div>

          <template v-else>
            <div v-if="isNarrow" class="pane-tabs" role="tablist" aria-label="编辑与预览切换">
              <button
                type="button"
                role="tab"
                :aria-selected="activeMobilePane === 'editor'"
                :class="{ active: activeMobilePane === 'editor' }"
                @click="activeMobilePane = 'editor'"
              >
                编辑
              </button>
              <button
                type="button"
                role="tab"
                :aria-selected="activeMobilePane === 'preview'"
                :class="{ active: activeMobilePane === 'preview' }"
                @click="activeMobilePane = 'preview'"
              >
                预览
              </button>
            </div>

            <ArticleMeta
              v-if="draftMeta"
              :article-id="view.current.value.id"
              :meta="draftMeta"
              :word-count="view.wordCount.value"
              :save-state-text="view.saveStateText.value"
              :save-error="view.saveError.value"
              :fields-with-error="fieldsWithError"
              :published-on-site="publishedOnSite"
              :updated-date-is-set="updatedDateIsSet"
              :disabled="busy"
              @update:meta="updateMeta"
              @edit-url="openRenameDialog"
              @set-updated-date="setUpdatedDate"
              @clear-updated-date="clearUpdatedDate"
            />

            <SyncDiff
              v-if="view.conflict.value"
              :assessment="view.conflict.value"
              :baseline-hash="view.conflict.value.baseHash"
              :busy="busy"
              @adopt-local="doSync(true)"
              @adopt-remote="doAdoptRemote"
              @manual-merge="doManualMerge"
              @close="view.conflict.value = null"
            />

            <div class="editor-row" :class="{ split: !isNarrow }">
              <div v-show="!isNarrow || activeMobilePane === 'editor'" class="editor-pane">
                <div class="pane-toolbar">
                  <span class="subtle">正文（front matter 由软件独立维护）</span>
                  <div class="pane-toolbar-actions">
                    <button
                      type="button"
                      :disabled="busy"
                      title="选择图片，校验后归档到 public/blog/<文章标识>/ 并插入引用"
                      @click="insertImage"
                    >
                      插入图片…
                    </button>
                    <div class="mode-switch" role="group" aria-label="编辑模式">
                      <button
                        type="button"
                        :class="{ active: editorMode === 'sv' }"
                        :aria-pressed="editorMode === 'sv'"
                        @click="changeEditorMode('sv')"
                      >
                        源码分屏
                      </button>
                      <button
                        type="button"
                        :class="{ active: editorMode === 'ir' }"
                        :aria-pressed="editorMode === 'ir'"
                        @click="changeEditorMode('ir')"
                      >
                        正文即时渲染
                      </button>
                    </div>
                  </div>
                </div>
                <MarkdownEditor
                  ref="editorPane"
                  v-model="view.draftBody.value"
                  :mode="editorMode"
                  :font-size="view.preferences.value?.fontSize ?? 16"
                  :line-height="view.preferences.value?.lineHeight ?? 175"
                  :code-theme="view.preferences.value?.codeTheme ?? 'github'"
                  :preview-width="view.preferences.value?.previewWidth ?? 460"
                  :disabled="busy"
                  :can-insert-image="Boolean(view.current.value)"
                  @blocked="(report) => onRoundTripNotice(report.reason)"
                  @updated="
                    (report) =>
                      onRoundTripNotice(`模式切换完成：${report.reason}`)
                  "
                  @pick-image="insertImageFromEvent"
                  @image-rejected="onImageRejected"
                />
              </div>

              <div v-show="!isNarrow || activeMobilePane === 'preview'" class="preview-pane">
                <ReadingPreview
                  :preferences="view.preferences.value"
                  :preview-url="view.previewUrl.value"
                  :preview-banner="view.previewBanner.value"
                  :preview-notice="view.previewNotice.value"
                  :starting="view.previewStarting.value"
                  :editor-mode="editorMode"
                  @start-preview="startWebsitePreview"
                  @stop-preview="stopWebsitePreview"
                  @open-preview="openExternal(view.previewUrl.value!)"
                />
              </div>
            </div>
          </template>
        </section>
      </main>
    </template>

    <!-- 最近删除 -->
    <ModalDialog v-if="showTrash" label="最近删除" @close="showTrash = false">
      <RecentlyDeleted
        :entries="view.trash.value"
        :recovery-article-ids="recoveryArticleIds"
        :busy="busy"
        @restore="doRestore"
        @retry="doRetryDelete"
        @purge="doPurge"
        @discard-recovery="doDiscardRecovery"
        @restore-recovery="doRestoreRecovery"
        @close="showTrash = false"
      />
    </ModalDialog>

    <!-- 写作外观 -->
    <ModalDialog v-if="showPreferences" label="写作外观" @close="showPreferences = false">
      <h2 class="dialog-title">写作外观（仅本机）</h2>
      <p class="subtle">
        这些设置只影响你在这里的阅读与编辑体验，不会写入文章 Markdown 或网站 CSS。
      </p>

      <label class="field">
        正文字号：{{ prefsDraft.fontSize }} px
        <input v-model.number="prefsDraft.fontSize" type="range" min="12" max="28" step="1" />
      </label>

      <label class="field">
        行距：{{ prefsDraft.lineHeight }}%
        <input v-model.number="prefsDraft.lineHeight" type="range" min="120" max="260" step="5" />
      </label>

      <label class="field">
        预览宽度：{{ prefsDraft.previewWidth }} px
        <input v-model.number="prefsDraft.previewWidth" type="range" min="280" max="900" step="20" />
      </label>

      <label class="field">
        自动保存停顿：{{ prefsDraft.autoSaveDebounceMs }} ms
        <input
          v-model.number="prefsDraft.autoSaveDebounceMs"
          type="range"
          min="300"
          max="3000"
          step="100"
        />
      </label>

      <label class="field">
        代码主题
        <select v-model="prefsDraft.codeTheme">
          <option value="github">github（浅色）</option>
          <option value="monokai">monokai</option>
          <option value="native">native</option>
        </select>
      </label>

      <div class="dialog-actions">
        <button type="button" @click="showPreferences = false">取消</button>
        <button type="button" class="primary" :disabled="busy" @click="savePreferences">保存</button>
      </div>
    </ModalDialog>

    <!-- 新建文章 -->
    <ModalDialog v-if="showCreateDialog" label="新建文章" @close="showCreateDialog = false">
      <h2 class="dialog-title">新建文章</h2>
      <p class="subtle">
        URL 标识在创建时确定，之后不随标题变化。新建文章默认不公开（draft: true）。
      </p>

      <label class="field">
        URL 标识（小写英文、数字与短横线）
        <input v-model="newArticleId" type="text" placeholder="read-code-notes" />
      </label>
      <label class="field">
        标题 <span class="required">*</span>
        <input v-model="newTitle" type="text" />
      </label>
      <label class="field">
        摘要 <span class="required">*</span>
        <input v-model="newDescription" type="text" />
      </label>
      <label class="field">
        发布日期
        <input v-model="newPubDate" type="date" />
      </label>

      <div class="dialog-actions">
        <button type="button" @click="showCreateDialog = false">取消</button>
        <button
          type="button"
          class="primary"
          :disabled="busy || !newArticleId.trim() || !newTitle.trim() || !newDescription.trim()"
          @click="doCreate"
        >
          创建
        </button>
      </div>
    </ModalDialog>

    <!-- 从本地导入 -->
    <ModalDialog v-if="showImportDialog" label="从本地导入" @close="showImportDialog = false">
      <h2 class="dialog-title">从本地导入 Markdown</h2>
      <p class="subtle">
        只导入你明确选择的文件；项目原文件与 Git 状态不会被改动。
      </p>
      <label class="field">
        文件绝对路径
        <input v-model="importPath" type="text" placeholder="D:\\路径\\示例文章.md" />
      </label>
      <label class="field">
        导入后的 URL 标识
        <input v-model="importId" type="text" placeholder="example-post" />
      </label>
      <div class="dialog-actions">
        <button type="button" @click="showImportDialog = false">取消</button>
        <button
          type="button"
          class="primary"
          :disabled="busy || !importPath.trim() || !importId.trim()"
          @click="doImport"
        >
          导入
        </button>
      </div>
    </ModalDialog>

    <!-- 同步确认 -->
    <ModalDialog v-if="showSyncDialog" label="同步确认" @close="showSyncDialog = false">
      <h2 class="dialog-title">同步到写作分支</h2>
      <p class="subtle">
        同步是把当前文章的最新版本保存到远端的 <span class="mono">writing</span> 分支，
        <strong>不会</strong>改变网站版本，也不会带出其它文章或图片。
      </p>
      <dl class="facts">
        <div>
          <dt>当前文章</dt>
          <dd>
            {{ view.current.value?.meta.title ?? '（未选择）' }}
            <span class="subtle">（{{ view.current.value?.id }}）</span>
          </dd>
        </div>
        <div>
          <dt>目标分支</dt>
          <dd class="mono">writing</dd>
        </div>
        <div>
          <dt>网站影响</dt>
          <dd>无，需另行「发布到网站」</dd>
        </div>
      </dl>
      <div class="dialog-actions">
        <button type="button" @click="showSyncDialog = false">取消</button>
        <button type="button" class="primary" :disabled="busy" @click="confirmSync">
          确认同步
        </button>
      </div>
    </ModalDialog>

    <!-- 发布确认 -->
    <ModalDialog
      v-if="showPublishDialog && precheck"
      label="发布确认"
      @close="showPublishDialog = false"
    >
      <h2 class="dialog-title">发布到网站</h2>
      <dl class="facts">
        <div>
          <dt>当前文章</dt>
          <dd>{{ precheck.title }}<span class="subtle">（{{ precheck.articleId }}）</span></dd>
        </div>
        <div>
          <dt>图片</dt>
          <dd>{{ precheck.imageFileCount }} 个文件，共 {{ (precheck.imageTotalBytes / 1024).toFixed(1) }} KiB</dd>
        </div>
        <div>
          <dt>目标仓库 / 分支</dt>
          <dd class="mono">{{ precheck.targetRepo }} / {{ precheck.targetBranch }}</dd>
        </div>
        <div>
          <dt>与在线版本的差异</dt>
          <dd>{{ precheck.differsFromOnline ? '有差异，将更新该文章' : '内容一致，无需更新' }}</dd>
        </div>
      </dl>

      <p class="subtle">
        发布只处理这篇文章及其专属图片，<strong>不会</strong>带出其它草稿。
        推送成功显示「已提交发布」，网站是否上线需等待部署结果。
      </p>

      <div class="dialog-actions">
        <button type="button" @click="showPublishDialog = false">取消</button>
        <button type="button" class="primary" :disabled="busy" @click="doPublish">
          确认发布这篇文章
        </button>
      </div>
    </ModalDialog>

    <!-- 撤下确认 -->
    <ModalDialog v-if="showWithdrawDialog" label="撤下确认" @close="showWithdrawDialog = false">
      <h2 class="dialog-title">从网站撤下</h2>
      <p>
        将把 <strong>{{ draftMeta?.title }}</strong> 在主站标记为不公开（draft: true）。
        文章会从站点列表、首页与 RSS 消失，但仓库中仍可读。
      </p>
      <p class="subtle">写作分支中的稿件不受影响，日后可以再次发布。</p>
      <div class="dialog-actions">
        <button type="button" @click="showWithdrawDialog = false">取消</button>
        <button type="button" class="primary" :disabled="busy" @click="doWithdraw">
          确认撤下
        </button>
      </div>
    </ModalDialog>

    <!-- 删除确认 -->
    <ModalDialog
      v-if="showDeleteDialog && deleteAssessment"
      label="删除确认"
      @close="showDeleteDialog = false"
    >
      <h2 class="dialog-title">删除文件</h2>
      <p class="subtle">
        与「从网站撤下」不同：删除会从当前分支移除文件，但 Git 历史中仍然可见，
        软件不提供彻底抹除历史的操作。
      </p>

      <dl class="facts">
        <div>
          <dt>将删除的 Markdown</dt>
          <dd class="mono">{{ deleteAssessment.markdownRelPath }}</dd>
        </div>
        <div>
          <dt>写作分支</dt>
          <dd>{{ deleteAssessment.onWriting ? '存在，将被删除' : '不存在' }}</dd>
        </div>
        <div>
          <dt>网站分支</dt>
          <dd>{{ deleteAssessment.onMain ? '存在，将被删除' : '不存在' }}</dd>
        </div>
      </dl>

      <div v-if="deleteAssessment.exclusiveImages.length > 0">
        <h3 class="section-title">将一并清理的独占图片（{{ deleteAssessment.exclusiveImages.length }} 个）</h3>
        <ul class="path-list">
          <li v-for="path in deleteAssessment.exclusiveImages" :key="path" class="mono">{{ path }}</li>
        </ul>
      </div>

      <div v-if="deleteAssessment.protectedImages.length > 0">
        <h3 class="section-title">被其它文章引用而保留的图片（{{ deleteAssessment.protectedImages.length }} 个）</h3>
        <ul class="path-list">
          <li v-for="path in deleteAssessment.protectedImages" :key="path" class="mono">{{ path }}</li>
        </ul>
      </div>

      <p class="subtle">
        删除前会先在本地回收区保留正文与图片副本。两个分支没有跨分支事务，
        可能出现「写作分支已删／网站仍在」，届时可在最近删除中补做。
      </p>

      <div class="dialog-actions">
        <button type="button" @click="showDeleteDialog = false">取消</button>
        <button type="button" class="danger" :disabled="busy" @click="doDelete">
          确认删除并保留本地副本
        </button>
      </div>
    </ModalDialog>

    <!-- URL 改名 -->
    <ModalDialog v-if="showRenameDialog" label="更改 URL 标识" @close="showRenameDialog = false">
      <h2 class="dialog-title">更改 URL 标识</h2>
      <p class="subtle">
        改标题不会改变 URL。更改 URL 会让旧链接失效；已发布的文章需要重新发布才会生效。
      </p>

      <label class="field">
        当前 URL
        <input :value="`/blog/${view.current.value?.id ?? ''}/`" type="text" readonly />
      </label>
      <label class="field">
        新的 URL 标识
        <input v-model="renameNewId" type="text" />
      </label>

      <button type="button" :disabled="busy || !renameNewId.trim()" @click="assessRename">
        检查影响
      </button>

      <div v-if="renameAssessment" class="rename-assessment">
        <dl class="facts">
          <div>
            <dt>新 URL</dt>
            <dd class="mono">{{ renameAssessment.newUrl }}</dd>
          </div>
          <div>
            <dt>文章状态</dt>
            <dd>{{ renameAssessment.published ? '已发布，需要重新发布' : '未发布' }}</dd>
          </div>
        </dl>
        <div v-if="renameAssessment.referencingArticles.length > 0">
          <h3 class="section-title">站内引用旧 URL 的文章</h3>
          <ul class="path-list">
            <li v-for="id in renameAssessment.referencingArticles" :key="id" class="mono">{{ id }}</li>
          </ul>
        </div>
        <p v-else class="subtle">站内没有引用旧 URL 的文章。</p>

        <label v-if="renameAssessment.requiresRepublish" class="checkbox-field">
          <input v-model="confirmRenamePublished" type="checkbox" />
          我确认旧链接可能失效，并了解需要重新发布
        </label>
      </div>

        <div class="dialog-actions">
          <button type="button" @click="showRenameDialog = false">取消</button>
          <button
            type="button"
            class="primary"
            :disabled="
              busy ||
              !renameAssessment ||
              (renameAssessment.requiresRepublish && !confirmRenamePublished)
            "
            @click="doRename"
          >
            确认更改 URL
          </button>
        </div>
    </ModalDialog>
  </div>
</template>

<style scoped>
.app-shell {
  display: flex;
  flex-direction: column;
  height: 100vh;
  min-height: 0;
}

.onboarding {
  max-width: 720px;
  margin: 48px auto;
  padding: 24px;
  display: flex;
  flex-direction: column;
  gap: var(--gl-gap);
}

.onboarding h1 {
  margin: 0;
  font-size: 22px;
}

.section-title {
  margin: 0 0 6px;
  font-size: 13px;
  color: var(--gl-text-muted);
}

.facts {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
  gap: 12px;
  margin: 0;
}

.facts dt {
  font-size: 12px;
  color: var(--gl-text-muted);
}

.facts dd {
  margin: 2px 0 0;
}

.tool-list {
  margin: 0;
  padding-left: 18px;
}

.warn-box {
  background: var(--gl-warn-soft);
  border: 1px solid var(--gl-warn);
  border-radius: var(--gl-radius-sm);
  padding: 10px;
  margin: 8px 0 0;
}

.block {
  display: block;
}

.disclosure {
  background: var(--gl-info-soft);
  border: 1px solid var(--gl-accent);
  border-radius: var(--gl-radius-sm);
  padding: 10px;
  margin: 0;
}

.onboarding-actions {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}

.topbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 14px;
  background: var(--gl-surface);
  border-bottom: 1px solid var(--gl-border);
  flex-wrap: wrap;
}

.topbar-left,
.topbar-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.brand {
  font-size: 15px;
}

.count {
  margin-left: 4px;
  color: var(--gl-text-subtle);
}

.notice {
  margin: 0;
  padding: 8px 14px;
  background: var(--gl-info-soft);
  color: var(--gl-accent);
  font-size: 13px;
  border-bottom: 1px solid var(--gl-border);
}

.workspace {
  flex: 1;
  min-height: 0;
  display: grid;
  grid-template-columns: 300px 1fr;
}

.editor-column {
  display: flex;
  flex-direction: column;
  min-height: 0;
  gap: 10px;
  padding: 10px;
  overflow-y: auto;
}

.pane-tabs {
  display: flex;
  gap: 6px;
}

.pane-tabs button {
  flex: 1;
}

.pane-tabs button.active {
  background: var(--gl-accent-soft);
  border-color: var(--gl-accent);
  color: var(--gl-accent);
  font-weight: 600;
}

.editor-row {
  display: flex;
  flex-direction: column;
  gap: 10px;
  min-height: 360px;
  flex: 1;
}

.editor-row.split {
  flex-direction: row;
}

.editor-pane,
.preview-pane {
  display: flex;
  flex-direction: column;
  min-width: 0;
  flex: 1;
  border: 1px solid var(--gl-border);
  border-radius: var(--gl-radius);
  background: var(--gl-surface);
  overflow: hidden;
}

.pane-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  padding: 8px 10px;
  border-bottom: 1px solid var(--gl-border);
  flex-wrap: wrap;
}

.mode-switch {
  display: flex;
  gap: 6px;
}

.pane-toolbar-actions {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.mode-switch button {
  min-height: 32px;
  padding: 0 10px;
  font-size: 12px;
}

.mode-switch button.active {
  background: var(--gl-accent-soft);
  border-color: var(--gl-accent);
  color: var(--gl-accent);
  font-weight: 600;
}

.preview-actions {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}

.preview-banner {
  display: flex;
  gap: 10px;
  align-items: baseline;
  flex-wrap: wrap;
  margin: 0;
  padding: 8px 12px;
  background: var(--gl-warn-soft);
  border: 1px solid var(--gl-warn);
  border-radius: var(--gl-radius-sm);
  font-size: 13px;
}

.dialog-title {
  margin: 0;
  font-size: 17px;
}

.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 13px;
  color: var(--gl-text-muted);
}

.checkbox-field {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
}

.checkbox-field input {
  width: auto;
  min-height: 0;
}

.dialog-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 4px;
}

.path-list {
  margin: 0;
  padding-left: 18px;
  max-height: 160px;
  overflow-y: auto;
}

.mono {
  font-family: var(--gl-font-mono);
  font-size: 12px;
  overflow-wrap: anywhere;
}

.rename-assessment {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 10px;
  background: var(--gl-surface-muted);
  border-radius: var(--gl-radius-sm);
}
</style>

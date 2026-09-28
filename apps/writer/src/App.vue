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
import AppMenuBar from '@/components/AppMenuBar.vue'
import ArticleList from '@/components/ArticleList.vue'
import ArticleMeta from '@/components/ArticleMeta.vue'
import CodeMirrorEditor from '@/components/CodeMirrorEditor.vue'
import InstantPreview from '@/components/InstantPreview.vue'
import ModalDialog from '@/components/ModalDialog.vue'
import OperationStatus from '@/components/OperationStatus.vue'
import RecentlyDeleted from '@/components/RecentlyDeleted.vue'
import StatusBar from '@/components/StatusBar.vue'
import StyleDrawer from '@/components/StyleDrawer.vue'
import SyncDiff from '@/components/SyncDiff.vue'
import { SplitterGroup, SplitterPanel, SplitterResizeHandle } from 'reka-ui'
import { useArticle } from '@/composables/useArticle'
import { usePreviewStyles, type PreviewStyles } from '@/composables/usePreviewStyles'
import { useShellLayout } from '@/composables/useShellLayout'
import { useShellTheme } from '@/composables/useShellTheme'
import { PUBLIC_DISCLOSURE } from '@/types/article'
import { backend, filterArticles, searchArticles } from '@/services/backend'
import type { MarkdownEditorHandle } from '@/editor/codemirror'
import { FORMAT_COMMANDS, LINK_COMMAND, type EditorCommand } from '@/editor/commands'
import { openSearchPanel as openSearchPanelFor } from '@codemirror/search'
import { redo, undo } from '@codemirror/commands'
import type {
  ArticleFilter,
  ArticleMeta as Meta,
  DeleteAssessment,
  DeploymentStatus,
  PreviewSessionInfo,
  PublishPrecheck,
  RenameAssessment,
  ShellTheme,
  ToolchainState,
  WriterError,
} from '@/types/article'

const view = useArticle()

// ---- 首次连接 ----
const workspaceInput = ref('')

// ---- 界面状态 ----
/** 底部视图切换：编辑 / 双屏 / 预览（D7 的可折叠三段式，§3.1-3）。 */
const { viewMode, listCollapsed } = useShellLayout()
/** 外壳配色：跟随系统 / 浅色 / 深色（§3.5-4，只作用于软件外壳）。 */
const shellTheme = computed(() => view.preferences.value?.shellTheme ?? 'system')
useShellTheme(shellTheme)
const showTrash = ref(false)
const showPreferences = ref(false)
const showStyleDrawer = ref(false)
const showRenameDialog = ref(false)
const showCreateDialog = ref(false)
const showImportDialog = ref(false)
const showPublishDialog = ref(false)
const showDeleteDialog = ref(false)
const showWithdrawDialog = ref(false)
/** 「同步到写作分支」的确认对话框（Ctrl+Shift+S 与按钮都先经它）。 */
const showSyncDialog = ref(false)
const showRecoveryNotice = ref(false)
/** 帮助菜单里的说明弹窗。 */
const helpDialog = ref<'shortcuts' | 'syntax' | 'about' | null>(null)
/** 清空正文需要明确确认（且保留撤销能力）。 */
const showClearDialog = ref(false)

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
  shellTheme: 'system' as ShellTheme,
})

// 编辑区外观偏好（旧字段保留以兼容已有本机设置）。
// `codeTheme` 只在编辑区有消费者；站点预览由样式抽屉单独控制（§3.4）。
const editorFontSize = computed(() => view.preferences.value?.fontSize ?? 16)
const editorLineHeight = computed(() => (view.preferences.value?.lineHeight ?? 175) / 100)

/** 即时预览的本机样式覆盖（§3.4：只影响预览，不改变网站文章）。 */
const previewStyles = usePreviewStyles()
const styleDrawerOpen = computed({
  get: () => showStyleDrawer.value,
  set: (value: boolean) => (showStyleDrawer.value = value),
})
function updatePreviewStyles(next: PreviewStyles): void {
  previewStyles.styles.value = next
}

/** 光标位置（状态栏用）。 */
const cursorInfo = ref<{ line: number; column: number; selectedChars: number } | null>(null)

/**
 * 编辑器是否正处于输入法组合态。
 *
 * 组合未结束时切换文章会整体替换文档，打断候选输入并可能丢掉尚未上屏的拼音；
 * 因此外壳在组合期间**拒绝切篇**，并给出可操作提示（§3.2-3）。
 */
const composing = ref(false)
/** 组合期间被拒绝的切篇请求，等组合结束后继续。 */
const pendingArticleId = ref<string | null>(null)

/** 窄窗口下切换为选项卡布局。 */
const isNarrow = ref(false)
const editorPane = ref<{
  load: (body: string) => void
  insertText: (snippet: string) => void
  handle: () => MarkdownEditorHandle | null
} | null>(null)

/** 当前编辑器句柄（菜单栏、快捷键与右键菜单共用）。 */
const editorHandle = computed<MarkdownEditorHandle | null>(() => editorPane.value?.handle() ?? null)

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
      shellTheme: value.shellTheme,
    }
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
  for (const value of [
    'local-only',
    'remote-saved',
    'site-published',
    'conflict',
    'unverified',
  ] as ArticleFilter[]) {
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

/**
 * 窄屏只剩「编辑｜预览」；进入窄屏时若正处于双屏则自动切到编辑（§3.1-3）。
 */
function updateNarrow(): void {
  const narrow = window.innerWidth < 980
  if (narrow && !isNarrow.value && viewMode.value === 'split') {
    viewMode.value = 'edit'
  }
  isNarrow.value = narrow
}

/** 视图切换：窄屏下的双屏选项被隐藏。 */
/** 视图切换：窄屏下 `AppMenuBar` 会隐藏双屏选项。 */
function setViewMode(next: 'edit' | 'split' | 'preview'): void {
  viewMode.value = next
}

const showEditor = computed(() => viewMode.value === 'edit' || viewMode.value === 'split')
const showPreview = computed(() => viewMode.value === 'preview' || viewMode.value === 'split')

// ---- 编辑器命令（菜单栏 / 快捷键 / 右键菜单共用同一批实现） ----

/**
 * 打开 CodeMirror 的查找面板；`replace` 为真时把焦点移到替换输入框。
 *
 * CodeMirror 的 `search` 扩展用一个面板同时承载查找与替换，因此这里只打开一次。
 */
function openSearchPanel(replace: boolean): void {
  const handle = editorHandle.value
  if (!handle) return
  openSearchPanelFor(handle.view)
  if (replace) {
    window.setTimeout(() => {
      handle.view.dom.querySelector<HTMLInputElement>('.cm-search input[name=replace]')?.focus()
    }, 0)
  }
}

/** 按 id 取格式命令；id 写错时立即失败，而不是静默无操作。 */
function formatCommand(id: string): EditorCommand {
  const found = FORMAT_COMMANDS.find((command) => command.id === id)
  if (!found) throw new Error(`未知的编辑命令：${id}`)
  return found
}

/** 执行一条编辑器命令。 */
function runEditorCommand(command: EditorCommand): void {
  const handle = editorHandle.value
  if (!handle) return
  command.run(handle.view)
}

/**
 * 跳转到指定行。
 *
 * CodeMirror 没有内置的行号对话框，这里用浏览器 `prompt` 收集行号；
 * 输入非法时不改变选区。
 */
function gotoLine(): void {
  const handle = editorHandle.value
  if (!handle) return
  const total = handle.view.state.doc.lines
  const input = window.prompt(`跳转到行（1-${total}）`, '1')
  if (!input) return
  const lineNo = Number.parseInt(input, 10)
  if (!Number.isFinite(lineNo) || lineNo < 1 || lineNo > total) return
  const line = handle.view.state.doc.line(lineNo)
  handle.view.dispatch({ selection: { anchor: line.from }, scrollIntoView: true })
  handle.view.focus()
}

/** 复制正文（Markdown 原文或纯文本）。 */
async function copyBody(asMarkdown: boolean): Promise<void> {
  const payload = asMarkdown ? view.draftBody.value : stripMarkdown(view.draftBody.value)
  try {
    await navigator.clipboard.writeText(payload)
    actionNotice.value = asMarkdown ? '已复制 Markdown 原文' : '已复制纯文本'
  } catch (error) {
    actionError.value = {
      code: 'io-failed',
      message: '复制失败，请手动选择正文复制',
      detail: String(error),
    }
  }
}

/**
 * 极简的 Markdown 去标记。
 *
 * 只处理常见的行内与块级标记，用于「复制纯文本」；不追求解析级正确，
 * 也不修改正文本身。
 */
function stripMarkdown(text: string): string {
  return text
    .replace(/```[\s\S]*?```/g, (block) => block.replace(/```[^\n]*\n?/g, ''))
    .replace(/`([^`]*)`/g, '$1')
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/^#{1,6}\s+/gm, '')
    .replace(/^\s*>\s?/gm, '')
    .replace(/^\s*[-*+]\s+/gm, '')
    .replace(/^\s*\d+\.\s+/gm, '')
    .replace(/(\*\*|__)(.*?)\1/g, '$2')
    .replace(/(\*|_)(.*?)\1/g, '$2')
    .replace(/~~(.*?)~~/g, '$1')
    .replace(/^\s*---\s*$/gm, '')
}

/**
 * 清空正文（用户已在对话框里确认）。
 *
 * 用编辑器的一次可撤销改写，而不是重建实例——清空之后 Ctrl+Z 必须能还原。
 */
function clearBody(): void {
  const handle = editorHandle.value
  if (!handle) return
  handle.view.dispatch({
    changes: { from: 0, to: handle.view.state.doc.length, insert: '' },
    selection: { anchor: 0 },
  })
  handle.view.focus()
  showClearDialog.value = false
}

/** 行内格式的编辑快捷键（Ctrl+B/I/D/E）。 */
const INLINE_SHORTCUTS: { key: string; id: string }[] = [
  { key: 'b', id: 'bold' },
  { key: 'i', id: 'italic' },
  { key: 'd', id: 'strikethrough' },
  { key: 'e', id: 'inline-code' },
]

/**
 * 键盘快捷键。
 *
 * Ctrl+S 本地保存，Ctrl+Shift+S 打开同步确认，Ctrl+P 切到预览（阻止打印），
 * 其余为编辑命令与查找/跳转。Ctrl+Z / Ctrl+Y 由 CodeMirror 的历史扩展处理，
 * 这里不重复绑定。
 */
function handleKeydown(event: KeyboardEvent): void {
  if (!(event.ctrlKey || event.metaKey)) return
  const key = event.key.toLowerCase()

  if (key === 's' && event.shiftKey) {
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
    setViewMode('preview')
    return
  }
  // 以下都是编辑类快捷键：编辑器未就绪时不动，避免把按键吞掉。
  if (!editorHandle.value) return

  if (key === 'k') {
    event.preventDefault()
    runEditorCommand(LINK_COMMAND)
    return
  }
  if (key === 'f') {
    event.preventDefault()
    openSearchPanel(false)
    return
  }
  if (key === 'h') {
    event.preventDefault()
    openSearchPanel(true)
    return
  }
  if (key === 'g') {
    event.preventDefault()
    gotoLine()
    return
  }
  if (/^[1-6]$/.test(key)) {
    event.preventDefault()
    runEditorCommand(formatCommand(`heading-${key}`))
    return
  }
  if (key === 'u') {
    event.preventDefault()
    runEditorCommand(formatCommand('bullet-list'))
    return
  }
  if (key === 'o') {
    event.preventDefault()
    runEditorCommand(formatCommand('ordered-list'))
    return
  }
  const matched = INLINE_SHORTCUTS.find((entry) => entry.key === key)
  if (matched) {
    event.preventDefault()
    runEditorCommand(formatCommand(matched.id))
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

// ---- 环境检查（用户显式触发，启动路径不探测） ----
const toolchainChecking = ref(false)

/** 上次环境检查结果；未检查时是 `{ kind: 'unchecked' }`。 */
const toolchainState = computed<ToolchainState>(
  () => view.connection.value?.toolchain ?? { kind: 'unchecked' },
)

/** 检查时间的可读表示（只在该状态下有意义）。 */
const checkedAtText = computed(() => {
  const state = toolchainState.value
  if (state.kind !== 'checked') return '—'
  const at = new Date(state.atUnix * 1000)
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())} ${pad(at.getHours())}:${pad(at.getMinutes())}`
})

async function doToolchainCheck(): Promise<void> {
  toolchainChecking.value = true
  actionError.value = null
  try {
    const state = await backend.toolchainReport()
    if (view.connection.value) {
      view.connection.value = { ...view.connection.value, toolchain: state }
    }
  } catch (error) {
    actionError.value = error as WriterError
  } finally {
    toolchainChecking.value = false
  }
}

/** 手动刷新当前文章的远端状态（显式联网核对）。 */
async function doRefreshRemote(): Promise<void> {
  await run(async () => {
    await view.refreshRemoteNow()
  })
}

async function doAcknowledge(): Promise<void> {
  await run(async () => {
    await view.acknowledgeDisclosure()
  })
}

// ---- 文章 ----
async function selectArticle(id: string): Promise<void> {
  // 组合未结束时不切换：此时文档整体替换会打断候选输入。
  if (composing.value) {
    pendingArticleId.value = id
    actionNotice.value = '正在输入法组字，已延后切换文章；组字完成后会自动打开'
    return
  }
  await run(async () => {
    await view.openArticle(id)
    deployment.value = null
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
 * 把当前文章导出到用户选定的文件（右键菜单入口）。
 *
 * 先把内存里的改动落盘再导出：否则磁盘原文会落后于界面上看到的内容，
 * 导出结果与用户预期不符。`flush` 失败时抛错，导出随之中止。
 */
async function exportCurrentArticle(): Promise<void> {
  const id = view.current.value?.id
  if (!id) return
  await run(async () => {
    await view.flushBeforeRemote()
    const { save } = await import('@tauri-apps/plugin-dialog')
    const target = await save({
      title: '导出 Markdown 文件',
      defaultPath: `${id}.md`,
      filters: [{ name: 'Markdown', extensions: ['md'] }],
    })
    if (typeof target !== 'string') {
      // 用户取消：不是错误，不提示。
      return
    }
    await view.exportArticle(id, target)
    actionNotice.value = `已导出到 ${target}`
  })
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

/**
 * 输入法组合态变化。
 *
 * 组合结束时若用户在组字期间点过别的文章，此刻才真正切换——既不会打断输入，
 * 也不会把「点了没反应」留给用户。
 */
function onComposition(info: { composing: boolean; data?: string }): void {
  composing.value = info.composing
  if (info.composing) return
  const queued = pendingArticleId.value
  pendingArticleId.value = null
  if (queued) {
    void selectArticle(queued)
  }
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
    await view.savePreferences({
      ...prefsDraft.value,
      // 旧字段保留以兼容已有本机设置；编辑区模式由底部视图切换承担，
      // `editorMode` 不再有界面入口，写回原值避免静默改变用户设置。
      editorMode: view.preferences.value?.editorMode ?? 'sv',
      codeTheme: prefsDraft.value.codeTheme,
      shellTheme: prefsDraft.value.shellTheme,
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

/** 用户显式请求准备预览依赖：启动预览不会自动下载依赖。 */
async function prepareWebsitePreviewDependencies(): Promise<void> {
  await run(async () => {
    const status = await view.preparePreviewDependencies()
    actionNotice.value =
      status.kind === 'ready'
        ? '预览依赖已就绪，可以启动网站预览。'
        : `依赖准备未完成：${status.kind === 'failed' ? status.reason : '状态未知'}`
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

/** 撤销 / 重做：走 CodeMirror 的历史扩展，保证与 Ctrl+Z / Ctrl+Y 同源。 */
function doUndo(): void {
  const handle = editorHandle.value
  if (!handle) return
  undo(handle.view)
}

function doRedo(): void {
  const handle = editorHandle.value
  if (!handle) return
  redo(handle.view)
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
        <!--
          启动路径不执行探测命令：这里先显示上次结果或「尚未检查」，
          由用户点按钮触发真实检查。找得到候选文件不等于程序可执行。
        -->
        <template v-if="toolchainState.kind === 'checked'">
          <ul class="tool-list">
            <li>
              Git：<span :class="toolchainState.report.git.available ? '' : 'error-text'">
                {{ toolchainState.report.git.version ?? '未找到' }}
              </span>
            </li>
            <li>
              Node.js：<span :class="toolchainState.report.nodeMeetsMinimum ? '' : 'error-text'">
                {{ toolchainState.report.node.version ?? '未找到' }}
              </span>
              <span class="subtle">（网站预览需要 >= 22.12.0）</span>
            </li>
            <li>
              pnpm：<span :class="toolchainState.report.pnpm.available ? '' : 'error-text'">
                {{ toolchainState.report.pnpm.version ?? '未找到' }}
              </span>
            </li>
          </ul>
          <p v-if="toolchainState.report.missing.length > 0" class="warn-box">
            缺少：{{ toolchainState.report.missing.join('；') }}。<br />
            文章编辑与即时排版仍可用，网站预览需要先补齐这些前置条件。
            <span v-for="tip in toolchainState.report.guidance" :key="tip" class="subtle block">
              · {{ tip }}
            </span>
          </p>
          <p v-else class="subtle">前置条件齐备，网站预览可用。</p>
          <p class="subtle">检查时间：{{ checkedAtText }}</p>
        </template>
        <p v-else-if="toolchainState.kind === 'checkFailed'" class="warn-box">
          环境检查未完成：{{ toolchainState.reason }}
        </p>
        <p v-else class="subtle">
          尚未检查本机环境。启动不再自动执行检查，需要时点下方按钮。
        </p>
        <button type="button" class="ghost small" :disabled="toolchainChecking" @click="doToolchainCheck">
          {{ toolchainChecking ? '正在检查…' : '检查 Git / Node / pnpm' }}
        </button>
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
      <!-- 菜单栏（D10：文件/编辑/格式/插入/样式/帮助，无图标工具栏） -->
      <AppMenuBar
        :has-article="Boolean(view.current.value)"
        :view="viewMode"
        :narrow="isNarrow"
        @command="runEditorCommand"
        @new-article="showCreateDialog = true"
        @import-article="showImportDialog = true"
        @open-trash="showTrash = true"
        @save="view.flush()"
        @export-markdown="exportCurrentArticle"
        @refresh-remote="doRefreshRemote"
        @open-preferences="showPreferences = true"
        @open-style-drawer="showStyleDrawer = true"
        @undo="doUndo"
        @redo="doRedo"
        @copy-markdown="copyBody(true)"
        @copy-plaintext="copyBody(false)"
        @find="openSearchPanel(false)"
        @replace="openSearchPanel(true)"
        @goto-line="gotoLine"
        @clear-content="showClearDialog = true"
        @insert-image="insertImage"
        @update:view="setViewMode"
        @show-shortcuts="helpDialog = 'shortcuts'"
        @show-syntax-help="helpDialog = 'syntax'"
        @show-about="helpDialog = 'about'"
      />

      <!-- 顶部主操作区：远端操作留在最显眼处，不收进菜单深处 -->
      <header class="topbar">
        <div class="topbar-left">
          <button type="button" class="ghost" :disabled="busy" @click="showPreferences = true">
            写作外观
          </button>
          <button type="button" class="ghost" :disabled="busy" @click="showTrash = true">
            最近删除<span v-if="view.trash.value.length" class="count">{{ view.trash.value.length }}</span>
          </button>
        </div>

        <div class="topbar-actions">
          <!--
            刷新远端状态：显式联网核对当前文章。保存路径不再联网，
            因此这是用户获得当前远端结论的唯一入口。
          -->
          <button
            type="button"
            :disabled="busy || !view.current.value || view.checkingRemote.value !== null"
            @click="doRefreshRemote"
          >
            {{ view.checkingRemote.value ? '正在核对远端…' : '刷新远端状态' }}
          </button>
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
        :checking="view.checkingRemote.value !== null"
        @check-deployment="checkDeployment"
        @open-run="openExternal"
      />

      <p v-if="actionNotice" class="notice" role="status">{{ actionNotice }}</p>
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

      <!-- 三段式主体：文章列表｜编辑｜预览（§3.1）。
           分隔条用 reka-ui 的 Splitter 原语：可拖动、比例随 autoSaveId 持久化，
           并且它对键盘与触屏都提供了等价交互（不是自写的鼠标事件）。 -->
      <main class="workspace" :class="{ narrow: isNarrow }">
        <!-- 窄屏：不做三列（会挤坏），列表作为整宽面板出现；选中文章后自动收起，
             回到编辑／预览。列表始终可以从折叠按钮重新打开。 -->
        <template v-if="isNarrow">
          <div class="narrow-pane">
            <div v-if="!listCollapsed" class="list-pane narrow-list">
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
            </div>

            <div v-else-if="viewMode !== 'preview'" class="editor-column">
              <ArticleMeta
                v-if="draftMeta && view.current.value"
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
              <div v-if="!view.current.value" class="empty-state">
                <p>点左上角的折叠按钮打开文章列表，或新建一篇开始写作。</p>
              </div>
              <CodeMirrorEditor
                ref="editorPane"
                v-model="view.draftBody.value"
                :font-size="editorFontSize"
                :line-height="editorLineHeight"
                :disabled="busy"
                :can-insert-image="Boolean(view.current.value)"
                @cursor="cursorInfo = $event"
                @composition="onComposition"
                @pick-image="insertImageFromEvent"
                @image-rejected="onImageRejected"
                @export-markdown="exportCurrentArticle"
              />
            </div>
            <div v-else class="preview-pane narrow-preview">
              <InstantPreview
                v-if="view.current.value"
                :markdown="view.draftBody.value"
                :title="draftMeta?.title ?? view.current.value.id"
                :description="draftMeta?.description ?? ''"
                :pub-date="draftMeta?.pubDate ?? ''"
                :tags="draftMeta?.tags ?? []"
                :styles="previewStyles.styles.value"
              />
            </div>
          </div>

          <button
            type="button"
            class="collapse-toggle"
            :aria-expanded="!listCollapsed"
            :title="listCollapsed ? '打开文章列表' : '收起文章列表'"
            @click="listCollapsed = !listCollapsed"
          >
            {{ listCollapsed ? '›' : '‹' }}
          </button>
        </template>

        <!-- 桌面三列：列表（可折叠）｜编辑｜预览。分隔条来自 reka-ui 的 Splitter
             原语：可拖动、比例随 autoSaveId 持久化，键盘与触屏交互由它提供。 -->
        <template v-else>
          <SplitterGroup
            id="guanlanzhi-workspace"
            class="workspace-group"
            direction="horizontal"
            auto-save-id="guanlanzhi.workspaceRatio"
          >
            <!-- 列表收起时不渲染该面板：reka-ui 用 `order` 处理条件面板。 -->
            <SplitterPanel
              v-if="!listCollapsed"
              id="article-list"
              :order="1"
              :default-size="20"
              :min-size="14"
              :max-size="38"
              class="list-pane"
            >
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
            </SplitterPanel>
            <SplitterResizeHandle v-if="!listCollapsed" class="resize-handle" aria-label="调整文章列表宽度" />

            <SplitterPanel id="main-column" class="main-panel" :order="2" :min-size="30">
              <section class="editor-column">
                <div v-if="!view.current.value" class="empty-state">
                  <p>从左侧选择一篇文章，或新建一篇开始写作。</p>
                </div>

                <template v-else>
                  <!-- 元数据表单属于编辑视图：`预览` 模式下隐藏它，让文章预览占满整列
                       （否则表单吃掉 366px，预览只剩约 140px，等于没得看）。 -->
                  <ArticleMeta
                    v-if="draftMeta && showEditor"
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

                  <!-- 编辑与预览：双屏时左右并排、中间可拖；单栏视图只渲染一列。 -->
                  <SplitterGroup
                    id="guanlanzhi-editor-split"
                    class="editor-row"
                    direction="horizontal"
                    auto-save-id="guanlanzhi.editorRatio"
                  >
                    <SplitterPanel
                      v-if="showEditor"
                      id="editor-pane-panel"
                      :order="1"
                      :default-size="50"
                      :min-size="25"
                    >
                      <div class="editor-pane">
                        <div class="pane-toolbar">
                          <span class="subtle">
                            正文 · front matter 由软件独立维护 · 格式化在菜单栏、<span class="mono">/</span> 或右键菜单
                          </span>
                          <div class="pane-toolbar-actions">
                            <button
                              type="button"
                              :disabled="busy"
                              title="选择图片，校验后归档到 public/blog/<文章标识>/ 并插入引用"
                              @click="insertImage"
                            >
                              插入图片…
                            </button>
                          </div>
                        </div>
                        <CodeMirrorEditor
                          ref="editorPane"
                          v-model="view.draftBody.value"
                          :font-size="editorFontSize"
                          :line-height="editorLineHeight"
                          :disabled="busy"
                          :can-insert-image="Boolean(view.current.value)"
                          @cursor="cursorInfo = $event"
                          @composition="onComposition"
                          @pick-image="insertImageFromEvent"
                          @image-rejected="onImageRejected"
                          @export-markdown="exportCurrentArticle"
                        />
                      </div>
                    </SplitterPanel>
                    <SplitterResizeHandle
                      v-if="showEditor && showPreview"
                      class="resize-handle"
                      aria-label="调整编辑与预览宽度"
                    />
                    <SplitterPanel
                      v-if="showPreview"
                      id="preview-pane-panel"
                      :order="2"
                      :min-size="25"
                    >
                      <div class="preview-pane">
                        <InstantPreview
                          :markdown="view.draftBody.value"
                          :title="draftMeta?.title ?? view.current.value.id"
                          :description="draftMeta?.description ?? ''"
                          :pub-date="draftMeta?.pubDate ?? ''"
                          :tags="draftMeta?.tags ?? []"
                          :styles="previewStyles.styles.value"
                        />
                        <ReadingPreview
                          :preview-url="view.previewUrl.value"
                          :preview-banner="view.previewBanner.value"
                          :preview-notice="view.previewNotice.value"
                          :starting="view.previewStarting.value"
                          :dependency="view.previewDependency.value"
                          :preparing-dependencies="view.previewDependenciesPreparing.value"
                          @start-preview="startWebsitePreview"
                          @prepare-dependencies="prepareWebsitePreviewDependencies"
                          @stop-preview="stopWebsitePreview"
                          @open-preview="openExternal(view.previewUrl.value!)"
                        />
                      </div>
                    </SplitterPanel>
                  </SplitterGroup>
                </template>
              </section>
            </SplitterPanel>
          </SplitterGroup>

          <button
            type="button"
            class="collapse-toggle"
            :aria-expanded="!listCollapsed"
            :title="listCollapsed ? '展开文章列表' : '收起文章列表'"
            @click="listCollapsed = !listCollapsed"
          >
            {{ listCollapsed ? '›' : '‹' }}
          </button>
        </template>

        <StyleDrawer
          v-if="styleDrawerOpen"
          :open="styleDrawerOpen"
          :styles="previewStyles.styles.value"
          @update:styles="updatePreviewStyles"
          @close="showStyleDrawer = false"
        />
      </main>

      <StatusBar
        :status="currentStatus"
        :word-count="view.wordCount.value"
        :checking="view.checkingRemote.value !== null"
        :cursor="cursorInfo"
      />
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
        自动保存停顿：{{ prefsDraft.autoSaveDebounceMs }} ms
        <input
          v-model.number="prefsDraft.autoSaveDebounceMs"
          type="range"
          min="300"
          max="3000"
          step="100"
        />
      </label>

      <p class="subtle">
        预览的字号、行距、段距与列宽在工具栏「样式」抽屉里调整——那里有「还原站点」，
        与这里的编辑区外观是两回事。旧的「预览宽度」偏好已不再控制预览（预览列宽由
        样式抽屉决定），因此不再提供该控件，取值继续保留在本机配置里不被改写。
      </p>

      <label class="field">
        代码主题
        <select v-model="prefsDraft.codeTheme">
          <option value="github">github（浅色）</option>
          <option value="monokai">monokai</option>
          <option value="native">native</option>
        </select>
      </label>

      <label class="field">
        软件配色
        <select v-model="prefsDraft.shellTheme">
          <option value="system">跟随系统</option>
          <option value="light">浅色</option>
          <option value="dark">深色</option>
        </select>
      </label>
      <p class="subtle">
        只改变软件自身的外壳配色；即时预览与网站始终是浅色，因为网站只有浅色主题。
      </p>

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

    <!-- 帮助：快捷键 / Markdown 语法 / 关于 -->
    <ModalDialog v-if="helpDialog" label="帮助" @close="helpDialog = null">
      <template v-if="helpDialog === 'shortcuts'">
        <h2 class="dialog-title">键盘快捷键</h2>
        <table class="shortcut-table">
          <tbody>
            <tr><th>Ctrl+S</th><td>保存到本地磁盘</td></tr>
            <tr><th>Ctrl+Shift+S</th><td>同步到写作分支（先确认）</td></tr>
            <tr><th>Ctrl+P</th><td>切到预览</td></tr>
            <tr><th>Ctrl+B / I / D / E</th><td>加粗 / 斜体 / 删除线 / 行内代码</td></tr>
            <tr><th>Ctrl+K</th><td>插入超链接</td></tr>
            <tr><th>Ctrl+1…Ctrl+6</th><td>标题 1–6</td></tr>
            <tr><th>Ctrl+U / Ctrl+O</th><td>无序列表 / 有序列表</td></tr>
            <tr><th>Ctrl+F / Ctrl+H</th><td>查找 / 替换</td></tr>
            <tr><th>Ctrl+G</th><td>跳转到行</td></tr>
            <tr><th>Ctrl+Z / Ctrl+Y</th><td>撤销 / 重做</td></tr>
            <tr><th>/</th><td>在行首打开斜杠命令</td></tr>
            <tr><th>Tab</th><td>缩进两个空格</td></tr>
          </tbody>
        </table>
      </template>

      <template v-else-if="helpDialog === 'syntax'">
        <h2 class="dialog-title">Markdown 语法</h2>
        <p class="subtle">
          正文使用普通 Markdown（GFM）。以下写法在站点与本软件预览中都可渲染。
        </p>
        <table class="shortcut-table">
          <tbody>
            <tr><th># 标题</th><td>一级到六级标题</td></tr>
            <tr><th>**加粗**</th><td>加粗文字</td></tr>
            <tr><th>*斜体*</th><td>斜体文字</td></tr>
            <tr><th>~~删除线~~</th><td>删除线</td></tr>
            <tr><th>`代码`</th><td>行内代码</td></tr>
            <tr><th>```语言</th><td>围栏代码块</td></tr>
            <tr><th>[文字](https://)</th><td>超链接</td></tr>
            <tr><th>![说明](/blog/…png)</th><td>图片（用「插入图片」会自动归档并生成路径）</td></tr>
            <tr><th>| 列 | 列 |</th><td>GFM 表格（分隔行用 ---）</td></tr>
            <tr><th>- [ ] 任务</th><td>任务列表</td></tr>
            <tr><th>&gt; 引用</th><td>引用块</td></tr>
            <tr><th>---</th><td>分隔线</td></tr>
          </tbody>
        </table>
        <p class="subtle">
          站点未配置公式渲染，因此不提供公式语法；内嵌 HTML 会被净化后展示。
        </p>
      </template>

      <template v-else>
        <h2 class="dialog-title">关于观澜志写作</h2>
        <p>
          这是「观澜志」个人学习记录博客的本地写作工具：管理文章列表、写中文 Markdown、
          预览排版，并把文章同步到写作分支、按篇发布到网站。
        </p>
        <p class="subtle">
          对站点是只读消费；同步与发布仍按既有预检执行，不会代替你做安全判断。
        </p>
      </template>

      <div class="dialog-actions">
        <button type="button" class="primary" @click="helpDialog = null">关闭</button>
      </div>
    </ModalDialog>

    <!-- 清空正文需要明确确认 -->
    <ModalDialog v-if="showClearDialog" label="清空正文" @close="showClearDialog = false">
      <h2 class="dialog-title">清空正文？</h2>
      <p>
        这会移除编辑器中的全部正文内容。改动在你保存之前不会落盘，
        且可以用 Ctrl+Z 撤销。
      </p>
      <div class="dialog-actions">
        <button type="button" @click="showClearDialog = false">取消</button>
        <button type="button" class="danger" @click="clearBody">清空正文</button>
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
  display: flex;
  align-items: stretch;
  overflow: hidden;
}

/* 折叠按钮：列表收起后唯一能把它展开的入口（宽度与旧实现一致，44px 点击区）。 */
.workspace > .collapse-toggle {
  flex: 0 0 var(--gl-hit);
  width: var(--gl-hit);
}

/* reka-ui 的 SplitterGroup 自己设 flexDirection；这里只约束尺寸。 */
.workspace-group {
  flex: 1;
  min-height: 0;
  min-width: 0;
}

/* 面板内容撑满分配到的空间（Panel 上的 flex 由 reka-ui 控制，不要覆盖）。 */
.list-pane > *,
.editor-column {
  min-width: 0;
}

.list-pane {
  display: flex;
  overflow: hidden;
  min-width: 0;
  border-right: 1px solid var(--gl-border);
}

.list-pane > * {
  flex: 1;
}

/* 拖动分隔条：可见细线 + 更宽的命中区（reka-ui 另带 5/15px 命中扩展）。 */
.resize-handle {
  flex: 0 0 6px;
  width: 6px;
  background: var(--gl-border);
  cursor: col-resize;
  position: relative;
}

.resize-handle:hover,
.resize-handle[data-state='drag'],
.resize-handle[data-resize-handle-active] {
  background: var(--gl-accent);
}

.resize-handle:focus-visible {
  outline: 2px solid var(--gl-focus);
  outline-offset: -2px;
}

.narrow-pane {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  min-height: 0;
  overflow: hidden;
}

/* 窄屏下列表占满整宽（此时没有三列可分）。 */
.narrow-list {
  flex: 1;
  border-right: none;
}

/* 分隔面板内的内容撑满面板；reka-ui 只负责面板的 flex 尺寸，不设 display。 */
.main-panel {
  display: flex;
  min-width: 0;
  overflow: hidden;
}

.main-panel > .editor-column {
  flex: 1;
  height: 100%;
}

.editor-column {
  flex: 1 1 auto;
  min-width: 0;
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

/* 编辑／预览并排：面板在 reka-ui 的横向 group 里，行本身不再需要自己的方向。 */
.editor-row {
  display: flex;
  min-height: 0;
  flex: 1;
}

.editor-row > [data-panel] {
  display: flex;
  min-width: 0;
  overflow: hidden;
}

.editor-row > [data-panel] > .editor-pane,
.editor-row > [data-panel] > .preview-pane {
  flex: 1;
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

.shortcut-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
}

.shortcut-table th {
  width: 40%;
  padding: 5px 8px 5px 0;
  text-align: left;
  vertical-align: top;
  font-family: var(--gl-font-mono);
  font-weight: 600;
  color: var(--gl-text-muted);
  white-space: nowrap;
}

.shortcut-table td {
  padding: 5px 0;
  color: var(--gl-text-muted);
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

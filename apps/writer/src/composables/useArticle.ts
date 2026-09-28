/**
 * 页面状态与编辑自动保存调度。
 *
 * 关键约定：
 * - 自动保存用停顿防抖（默认 800ms），切换文章、失焦或关闭窗口时强制 flush；
 * - 磁盘写入失败时**不**显示绿色成功，而是「尚未保存到磁盘」；
 * - 元数据与正文处于同一事务，避免「标题已保存但正文丢失」；
 * - 输入过程只触发本地保存，绝不触发远端 Git。
 */
import { computed, onBeforeUnmount, ref, shallowRef, watch } from 'vue'
import { backend, filterArticles, searchArticles, sortArticles, toWriterError } from '@/services/backend'
import type {
  ArticleContent,
  ArticleFilter,
  ArticleMeta,
  ArticleStatus,
  ArticleSort,
  ArticleSummary,
  BranchCheck,
  ConnectionStatus,
  DeleteAssessment,
  PreviewDependencyStatus,
  RecoveryDraft,
  RemoteCheckOutcome,
  SyncAssessment,
  TrashEntry,
  UpdatedDateAction,
  WriterError,
  WritingPreferences,
} from '@/types/article'

/** 保存状态：三态分明，不用布尔值掩盖失败。 */
export type SaveState = 'idle' | 'dirty' | 'saving' | 'saved' | 'failed'

/** 编辑器模式。 */
export type EditorMode = 'sv' | 'ir'

export function useArticle() {
  // ---- 连接状态 ----
  const connection = ref<ConnectionStatus | null>(null)
  const connectionError = shallowRef<WriterError | null>(null)

  // ---- 列表 ----
  const articles = ref<ArticleSummary[]>([])
  const trash = ref<TrashEntry[]>([])
  const recovery = ref<RecoveryDraft[]>([])
  const listError = shallowRef<WriterError | null>(null)
  const listLoading = ref(false)

  // ---- 当前文章 ----
  const current = ref<ArticleContent | null>(null)
  const draftMeta = ref<ArticleMeta | null>(null)
  const draftBody = ref('')
  const saveState = ref<SaveState>('idle')
  const saveError = shallowRef<WriterError | null>(null)
  const lastSavedAt = ref<number | null>(null)

  // ---- 冲突 ----
  const conflict = ref<SyncAssessment | null>(null)

  // ---- 偏好 ----
  const preferences = ref<WritingPreferences | null>(null)

  // ---- 筛选与搜索 ----
  const filter = ref<ArticleFilter>('all')
  const sort = ref<ArticleSort>('recent-edited')
  const query = ref('')

  // ---- 预览 ----
  const previewUrl = ref<string | null>(null)
  const previewBanner = ref<string>('')
  const previewNotice = ref<string | null>(null)
  const previewStarting = ref(false)
  /** 预览依赖的准备状态（不安装时为 `missing`）。 */
  const previewDependency = ref<PreviewDependencyStatus>({ kind: 'missing' })
  const previewDependencyError = shallowRef<WriterError | null>(null)
  /** 是否正有一个用户触发的依赖准备在轮询等待。 */
  const previewDependenciesPreparing = ref(false)

  let debounceTimer: number | null = null
  let flushPromise: Promise<void> | null = null
  /** 崩溃恢复快照的轻量防抖计时器（与保存防抖独立、更短）。 */
  let snapshotTimer: number | null = null

  // ---- 远端核对调度 ----
  /** 单调递增的请求序号：用于丢弃切换文章后才返回的旧核对结果。 */
  let remoteCheckSeq = 0
  /** 当前正在核对的文章 ID → 序号，用于同一篇文章的去重。 */
  const inFlightChecks = new Map<string, number>()
  /**
   * 待核对队列：单槽，后来者覆盖前者。
   *
   * 连续切换文章会产生多个核对请求，但只有用户最后停在的那篇值得查——中间
   * 掠过的文章查完也没人看。单槽 + 只保留最新一篇，既避免任务积压，也保证
   * 不会同时跑多个 `git fetch` 互相争抢。
   */
  let queuedCheck: string | null = null
  /** 界面用的核对中标记（按文章 ID）。 */
  const checkingRemote = ref<string | null>(null)
  const remoteCheckError = shallowRef<WriterError | null>(null)

  const visibleArticles = computed(() =>
    sortArticles(searchArticles(filterArticles(articles.value, filter.value), query.value), sort.value),
  )

  /**
   * 当前是否有尚未落盘的改动。
   *
   * 含 `failed`：「保存失败」同样代表磁盘上没有最新内容，关窗前必须提示，
   * 否则用户会以为已经保存。恢复副本仍在，但不该因此省略提示。
   */
  const hasUnsavedChanges = computed(
    () => saveState.value === 'dirty' || saveState.value === 'saving' || saveState.value === 'failed',
  )

  /** 面向用户的保存状态文案。 */
  const saveStateText = computed(() => {
    switch (saveState.value) {
      case 'dirty':
        return '有未保存的改动'
      case 'saving':
        return '正在保存…'
      case 'saved':
        return '已保存到磁盘'
      case 'failed':
        return '尚未保存到磁盘'
      case 'idle':
        return lastSavedAt.value ? '已保存到磁盘' : '尚未改动'
    }
  })

  /** 字数统计（中文按字符计，接近作者的直觉）。 */
  const wordCount = computed(() => draftBody.value.replace(/\s+/g, '').length)

  // ---------------------------------------------------------------- 加载

  async function refreshConnection(): Promise<void> {
    try {
      connection.value = await backend.connectionStatus()
      connectionError.value = null
    } catch (error) {
      connectionError.value = toWriterError(error)
    }
  }

  async function acknowledgeDisclosure(): Promise<void> {
    connection.value = await backend.acknowledgeDisclosure()
  }

  async function connect(workspaceDir?: string): Promise<void> {
    connection.value = await backend.connect(workspaceDir)
    await refreshAll()
  }

  async function refreshAll(): Promise<void> {
    if (!connection.value?.connected) return
    await Promise.all([
      refreshList(),
      refreshTrash(),
      refreshRecovery(),
      refreshPreferences(),
      refreshPreviewDependencyStatus(),
    ])
  }

  async function refreshList(): Promise<void> {
    listLoading.value = true
    try {
      articles.value = await backend.listArticles()
      listError.value = null
    } catch (error) {
      listError.value = toWriterError(error)
    } finally {
      listLoading.value = false
    }
  }

  async function refreshTrash(): Promise<void> {
    try {
      trash.value = await backend.listTrash()
    } catch {
      trash.value = []
    }
  }

  async function refreshRecovery(): Promise<void> {
    try {
      recovery.value = await backend.pendingRecovery()
    } catch {
      recovery.value = []
    }
  }

  async function refreshPreferences(): Promise<void> {
    try {
      preferences.value = await backend.getPreferences()
    } catch {
      preferences.value = null
    }
  }

  // ---------------------------------------------------------------- 远端核对

  /**
   * 把一次核对结果写回界面。
   *
   * 两道校验，任一不过就**整体丢弃**：
   * 1. 请求序号仍是该文章最新的一次（切换文章后返回的旧请求作废）；
   * 2. 结果携带的本地哈希与当前磁盘内容一致（核对期间又编辑过则作废）。
   *
   * 状态是整体替换而不是逐字段合并：远端结论必须与它依据的本地内容版本成套，
   * 否则会出现「新正文 ＋ 旧远端结论」这种自相矛盾的显示。
   */
  function applyRemoteCheck(outcome: RemoteCheckOutcome): void {
    if (inFlightChecks.get(outcome.articleId) !== remoteCheckSeq) return
    const entry = articles.value.find((article) => article.id === outcome.articleId)
    if (entry && entry.status.localBodyHash !== outcome.localBodyHash) {
      // 本地内容已变化：这条结论依据的内容版本过期了，不能写进列表。
      return
    }
    if (current.value?.id === outcome.articleId && current.value.contentHash === outcome.localBodyHash) {
      current.value = { ...current.value, status: outcome.status }
    }
    if (entry) {
      entry.status = outcome.status
    }
  }

  /**
   * 核对一篇文章的远端状态（联网）。同一篇文章同时只跑一个请求，重复调用直接复用。
   *
   * 这是**唯一**会联网读取远端的路径（另有同步/发布的预检各自核对）。
   * 保存路径绝不调用它。核对串行执行：忙时把请求放进单槽队列，只保留最新一篇。
   */
  async function checkRemote(articleId: string): Promise<void> {
    if (inFlightChecks.has(articleId)) return
    // 已有核对在跑：排队等候，且只保留最后请求的那一篇。
    if (checkingRemote.value !== null) {
      queuedCheck = articleId
      return
    }
    const seq = ++remoteCheckSeq
    inFlightChecks.set(articleId, seq)
    checkingRemote.value = articleId
    try {
      const outcome = await backend.checkArticleRemote(articleId)
      applyRemoteCheck(outcome)
      remoteCheckError.value = null
    } catch (error) {
      // 核对失败把原因交给界面显示「待核对」，不影响编辑。
      remoteCheckError.value = toWriterError(error)
    } finally {
      if (inFlightChecks.get(articleId) === seq) {
        inFlightChecks.delete(articleId)
      }
      checkingRemote.value = null
      // 取出排队的那一篇继续；期间又排了新的就再排一次。
      const next = queuedCheck
      queuedCheck = null
      if (next) {
        void checkRemote(next)
      }
    }
  }

  /** 手动刷新当前文章的远端状态。 */
  async function refreshRemoteNow(articleId?: string): Promise<void> {
    const id = articleId ?? current.value?.id
    if (!id) return
    // 手动刷新是显式意图：即使该文章正在核对中，也要在它跑完后重新查一次。
    if (checkingRemote.value !== null) {
      queuedCheck = id
      return
    }
    await checkRemote(id)
  }

  // ---------------------------------------------------------------- 编辑

  /**
   * 打开一篇文章。
   *
   * 切换前必须先 flush，避免上一篇的改动被丢掉。**flush 失败时必须中断切换**：
   * `flush()` 把失败降级为界面状态（`saveState = 'failed'`）而不抛错，若继续读入
   * 新文章，上一篇尚未落盘的正文会被整体替换，用户能看到的就只剩恢复副本——
   * 与「保存失败却继续远端操作」是同一类问题，故与 `flushBeforeRemote` 同源处理。
   */
  async function openArticle(articleId: string): Promise<void> {
    await flush()
    if (saveState.value === 'failed') {
      throw (
        saveError.value ?? {
          code: 'io-failed' as const,
          message: '本地保存失败，已停止切换文章；当前草稿仍保留在编辑器中',
        }
      )
    }
    const content = await backend.readArticle(articleId)
    current.value = content
    draftMeta.value = { ...content.meta }
    draftBody.value = content.body
    saveState.value = 'idle'
    saveError.value = null
    conflict.value = null
    // 本地内容先展示，再异步补远端结论：不要求网络「秒开」。
    void checkRemote(articleId)
  }

  function markDirty(): void {
    if (saveState.value === 'failed') {
      // 上次失败后用户又改了内容，仍然保持 dirty 语义。
    }
    saveState.value = 'dirty'
    scheduleAutoSave()
    scheduleRecoverySnapshot()
  }

  /**
   * 把当前内存中的内容写入本机恢复区。
   *
   * 比自动保存更早、更频繁地触发（默认 400ms），用于覆盖「进程被强制结束、
   * 来不及执行关闭时的 flush」这种情况：下次启动时用户仍能看到并取回这部分内容。
   *
   * 这是本机文件写入，不触发任何远端 Git 操作。
   */
  function scheduleRecoverySnapshot(): void {
    if (snapshotTimer !== null) {
      window.clearTimeout(snapshotTimer)
    }
    snapshotTimer = window.setTimeout(() => {
      snapshotTimer = null
      void takeRecoverySnapshot()
    }, 400)
  }

  /** 立即写入一次恢复快照（失败不打断编辑，仅记录在诊断信息里）。 */
  async function takeRecoverySnapshot(): Promise<void> {
    const id = current.value?.id
    const meta = draftMeta.value
    if (!id || !meta) return
    // 内容与磁盘一致时无需快照，避免产生无意义的「未保存内容」提示。
    if (saveState.value === 'saved' || saveState.value === 'idle') return
    try {
      await backend.snapshotRecovery(id, meta, draftBody.value)
    } catch {
      // 恢复快照是尽力而为的兜底，不应打断写作。
    }
  }

  function scheduleAutoSave(): void {
    if (debounceTimer !== null) {
      window.clearTimeout(debounceTimer)
    }
    const delay = preferences.value?.autoSaveDebounceMs ?? 800
    debounceTimer = window.setTimeout(() => {
      debounceTimer = null
      void flush()
    }, delay)
  }

  /**
   * 把「刚保存的本地内容」应用到列表项。
   *
   * 不重新拉列表：`list_articles` 会扫描整个工作区并读取远端缓存，放在保存
   * 路径上会让每次自动保存都变成一次重型操作。这里只用保存返回值更新**本地
   * 字段**，并且**不照搬它的远端状态**——`workspace.save` 返回的默认快照不是
   * 远端事实，照搬会把「未核对」写成「已同步」。
   *
   * 本地原文变了就意味着旧的远端结论（如果曾核实过）已经对不上当前内容，
   * 因此把两个分支的结论降级为「待核对 ＋ 原因」，等用户打开文章或手动刷新
   * 时再重新核对。
   */
  function applySavedContent(content: ArticleContent, localChanged: boolean): void {
    const entry = articles.value.find((article) => article.id === content.id)
    if (!entry) return
    entry.title = content.meta.title
    entry.description = content.meta.description
    entry.tags = content.meta.tags
    entry.pubDate = content.meta.pubDate
    // 可选字段用 delete 清空：`exactOptionalPropertyTypes` 下不能赋值 undefined。
    if (content.meta.updatedDate === undefined) {
      delete entry.updatedDate
    } else {
      entry.updatedDate = content.meta.updatedDate
    }
    entry.draft = content.meta.draft
    // 保存成功后这条记录已能正常解析，之前的读取错误不再成立。
    delete entry.loadError
    entry.status = localChanged ? staleStatus(content.status) : content.status
  }

  /**
   * 本地内容变化后，把旧的远端结论降级为「待核对」。
   *
   * 保留本地哈希（它是当前磁盘事实），远端两分支一律标为未核对并说明原因。
   */
  function staleStatus(base: ArticleStatus): ArticleStatus {
    const reason = '本地已改动，远端状态待重新核对'
    const stale: BranchCheck = { state: 'unverified', reason }
    const next: ArticleStatus = {
      ...base,
      remoteSync: 'unverified',
      site: 'unverified',
      writing: stale,
      main: stale,
    }
    // 上次核对时间保留为历史信息：它记录的是「什么时候查过」，不是当前结论。
    if (base.remoteCheckedAtUnix === undefined) {
      delete next.remoteCheckedAtUnix
    } else {
      next.remoteCheckedAtUnix = base.remoteCheckedAtUnix
    }
    return next
  }

  /**
   * 立即把当前编辑写入磁盘。
   *
   * 返回的 Promise 可被 await，保证切换文章／关闭窗口时不会丢失改动。
   *
   * **保存路径完全离线**：只调 `save_article`（本地写入）与恢复副本清理，
   * 不刷新列表、不核对远端。打开文章后的一次异步核对是唯一的读取远端时机。
   */
  async function flush(updatedDateAction?: UpdatedDateAction): Promise<void> {
    if (debounceTimer !== null) {
      window.clearTimeout(debounceTimer)
      debounceTimer = null
    }
    // 串行化并发 flush，避免同一次编辑被写两次。
    if (flushPromise) {
      await flushPromise
    }
    if (!current.value || !draftMeta.value) return
    if (saveState.value !== 'dirty' && saveState.value !== 'failed' && !updatedDateAction) {
      return
    }

    const articleId = current.value.id
    const previousHash = current.value.contentHash
    const meta = { ...draftMeta.value }
    const body = draftBody.value
    saveState.value = 'saving'

    flushPromise = (async () => {
      try {
        const saved = await backend.saveArticle(articleId, meta, body, updatedDateAction ?? null)
        current.value = saved
        draftMeta.value = { ...saved.meta }
        // 保存期间用户可能又输入了内容；此时不能把新内容标记为已保存。
        if (draftBody.value === body) {
          draftBody.value = saved.body
          saveState.value = 'saved'
          saveError.value = null
          lastSavedAt.value = Date.now()
          // 磁盘原文确实变了：旧的远端结论已失效，降级为待核对。
          applySavedContent(saved, saved.contentHash !== previousHash)
          // 内容已落盘，清除恢复快照，避免下次启动误报「未保存内容」。
          if (snapshotTimer !== null) {
            window.clearTimeout(snapshotTimer)
            snapshotTimer = null
          }
          try {
            await backend.discardRecovery(articleId)
          } catch {
            // 清理快照失败不影响保存结果；下次成功保存时会再次尝试。
          }
        } else {
          saveState.value = 'dirty'
          scheduleAutoSave()
          scheduleRecoverySnapshot()
        }
      } catch (error) {
        // 失败必须如实显示，不能给绿色成功。
        saveError.value = toWriterError(error)
        saveState.value = 'failed'
      } finally {
        flushPromise = null
      }
    })()
    await flushPromise
  }

  /**
   * 远端操作（同步／发布／删除／改名／预览）前的强制保存。
   *
   * `flush` 自身把失败降级为界面状态（`saveState = 'failed'`），若远端操作
   * 继续执行，后端会读到**磁盘上的旧内容**并成功推送，同时界面还可能提示
   * 「已保存」——这与「不显示误导性的成功」相矛盾。因此这里在保存失败时
   * 抛出可操作错误，阻断后续远端动作。
   */
  async function flushBeforeRemote(): Promise<void> {
    await flush()
    if (saveState.value === 'failed') {
      throw (
        saveError.value ?? {
          code: 'io-failed' as const,
          message: '本地保存失败，已停止后续操作；请先解决磁盘写入问题',
        }
      )
    }
  }

  /** 新建文章。 */
  async function createArticle(articleId: string, meta: ArticleMeta): Promise<ArticleContent> {
    const created = await backend.createArticle(articleId, meta, '')
    await refreshList()
    await openArticle(created.id)
    return created
  }

  /** 显式导入一篇已有的本地 Markdown。 */
  async function importArticle(sourcePath: string, articleId: string): Promise<ArticleContent> {
    const imported = await backend.importArticle(sourcePath, articleId)
    await refreshList()
    await openArticle(imported.id)
    return imported
  }

  /**
   * 插入图片：后端校验文件头与大小后归档到 `public/blog/<article-id>/`。
   *
   * 返回归档结果，调用方负责把引用写进正文。图片未落盘前不允许远程同步，
   * 因此这里先完成归档再返回。
   */
  async function insertImage(articleId: string, sourcePath: string) {
    const image = await backend.importArticleImage(articleId, sourcePath)
    await refreshList()
    return image
  }

  /**
   * 插入**内存中的**图片字节（剪贴板粘贴、页面内拖入）。
   *
   * 与文件选择插入共用后端的同一套校验与归档逻辑。
   */
  async function insertImageBytes(articleId: string, fileName: string, bytes: Uint8Array) {
    const image = await backend.importArticleImageBytes(articleId, fileName, bytes)
    await refreshList()
    return image
  }

  /** 列出当前文章已归档的图片。 */
  async function listImages(articleId: string) {
    return backend.listArticleImages(articleId)
  }

  // ---------------------------------------------------------------- 同步与发布

  /** 同步前评估；有冲突时交给冲突界面。 */
  async function assessSync(articleId: string): Promise<SyncAssessment> {
    const assessment = await backend.assessSync(articleId)
    conflict.value = assessment.decision === 'remote-changed' ? assessment : null
    return assessment
  }

  /**
   * 同步到写作分支。
   *
   * 若后端报告远端已变化，则加载差异界面而不是抛错中断界面。
   */
  async function syncArticle(articleId: string, adoptLocal = false): Promise<void> {
    await flushBeforeRemote()
    try {
      await backend.syncArticle(articleId, adoptLocal)
      conflict.value = null
      await refreshList()
      if (current.value?.id === articleId) {
        current.value = await backend.readArticle(articleId)
      }
    } catch (error) {
      const err = toWriterError(error)
      if (err.code === 'remote-changed') {
        conflict.value = await backend.assessSync(articleId)
        return
      }
      throw err
    }
  }

  /** 采用远端版本（本地改动已由后端存入恢复副本）。 */
  async function adoptRemote(articleId: string): Promise<void> {
    const content = await backend.adoptRemote(articleId)
    conflict.value = null
    current.value = content
    draftMeta.value = { ...content.meta }
    draftBody.value = content.body
    saveState.value = 'saved'
    await Promise.all([refreshList(), refreshRecovery()])
  }

  /** 手工合并后保存。 */
  async function resolveConflictManually(): Promise<void> {
    if (!current.value || !draftMeta.value) return
    const saved = await backend.resolveConflictManually(
      current.value.id,
      draftMeta.value,
      draftBody.value,
    )
    current.value = saved
    draftMeta.value = { ...saved.meta }
    saveState.value = 'saved'
    conflict.value = null
    await refreshList()
  }

  /** 发布预检。 */
  async function publishPrecheck(articleId: string) {
    await flushBeforeRemote()
    return backend.publishPrecheck(articleId)
  }

  /** 发布。 */
  async function publishArticle(articleId: string, removedImages: string[] = []) {
    await flushBeforeRemote()
    const outcome = await backend.publishArticle(articleId, removedImages)
    await refreshList()
    return outcome
  }

  /** 撤下。 */
  async function withdrawArticle(articleId: string) {
    const outcome = await backend.withdrawArticle(articleId)
    await refreshList()
    return outcome
  }

  // ---------------------------------------------------------------- 删除与恢复

  async function assessDelete(articleId: string): Promise<DeleteAssessment> {
    return backend.assessDelete(articleId)
  }

  async function deleteArticle(articleId: string) {
    await flushBeforeRemote()
    const outcome = await backend.deleteArticle(articleId)
    if (current.value?.id === articleId && outcome.writingDone && outcome.mainDone) {
      current.value = null
      draftMeta.value = null
      draftBody.value = ''
    }
    await Promise.all([refreshList(), refreshTrash()])
    return outcome
  }

  async function retryDelete(opId: string) {
    const outcome = await backend.retryDelete(opId)
    await Promise.all([refreshList(), refreshTrash()])
    return outcome
  }

  async function restoreArticle(opId: string) {
    const content = await backend.restoreArticle(opId)
    await Promise.all([refreshList(), refreshTrash()])
    await openArticle(content.id)
    return content
  }

  async function purgeTrash(opId: string) {
    await backend.purgeTrash(opId)
    await refreshTrash()
  }

  async function discardRecovery(articleId: string) {
    await backend.discardRecovery(articleId)
    await refreshRecovery()
  }

  /**
   * 用恢复副本覆盖本地文章文件（用户显式确认）。
   *
   * 只改本地文件，不触碰写作分支或 main；随后仍需用户手动同步与发布。
   */
  async function restoreRecovery(articleId: string): Promise<ArticleContent> {
    const content = await backend.restoreRecovery(articleId)
    await Promise.all([refreshRecovery(), refreshList()])
    await openArticle(content.id)
    return content
  }

  /**
   * 把一篇文章的磁盘原文导出到用户选定的绝对路径。
   *
   * 纯本地只读：不触碰工作区、不做远端操作，因此不刷新列表。
   * 调用方应先 `flushBeforeRemote()`，保证导出的是磁盘上与界面一致的内容。
   */
  async function exportArticle(articleId: string, targetPath: string): Promise<void> {
    await backend.exportArticle(articleId, targetPath)
  }

  // ---------------------------------------------------------------- URL 改名

  async function assessRename(oldId: string, newId: string) {
    return backend.assessRenameUrl(oldId, newId)
  }

  async function renameUrl(oldId: string, newId: string, confirmPublished: boolean) {
    await flushBeforeRemote()
    const content = await backend.renameArticleUrl(oldId, newId, confirmPublished)
    await refreshList()
    await openArticle(content.id)
    return content
  }

  // ---------------------------------------------------------------- 偏好与预览

  async function savePreferences(next: WritingPreferences) {
    preferences.value = await backend.setPreferences(next)
    return preferences.value
  }

  async function startSitePreview(articleId: string, simulatePublic: boolean) {
    previewStarting.value = true
    try {
      await flushBeforeRemote()
      const info = await backend.startSitePreview(articleId, simulatePublic)
      previewUrl.value = info.url
      previewBanner.value = info.banner
      previewNotice.value = info.offlineFontNotice ?? null
      return info
    } catch (error) {
      const mapped = toWriterError(error)
      // 缺依赖是可操作状态：让界面显示「需准备依赖」，而不是一句笼统的失败。
      if (mapped.code === 'preview-dependencies-missing') {
        void refreshPreviewDependencyStatus()
      }
      throw mapped
    } finally {
      previewStarting.value = false
    }
  }

  /** 查询预览依赖准备状态（不启动任务）。 */
  async function refreshPreviewDependencyStatus(): Promise<PreviewDependencyStatus> {
    try {
      previewDependency.value = await backend.previewDependencyStatus()
    } catch (error) {
      previewDependencyError.value = toWriterError(error)
    }
    return previewDependency.value
  }

  /**
   * 请求准备预览依赖。
   *
   * 后端只登记真实任务并立即返回，安装在线程内进行；这里在「进行中」期间轮询状态，
   * 直到得到就绪或失败。轮询在组件销毁或用户停止后自然结束（同一 view 实例内串行）。
   */
  async function preparePreviewDependencies(): Promise<PreviewDependencyStatus> {
    previewDependenciesPreparing.value = true
    try {
      let status = await backend.preparePreviewDependencies()
      previewDependency.value = status
      previewDependencyError.value = null
      while (status.kind === 'preparing') {
        await new Promise((resolve) => window.setTimeout(resolve, 700))
        status = await backend.previewDependencyStatus()
        previewDependency.value = status
      }
      if (status.kind === 'failed') {
        previewDependencyError.value = {
          code: 'preview-failed',
          message: status.reason,
          ...(status.detail ? { detail: status.detail } : {}),
        }
      }
      return status
    } catch (error) {
      previewDependencyError.value = toWriterError(error)
      throw previewDependencyError.value
    } finally {
      previewDependenciesPreparing.value = false
    }
  }

  async function stopSitePreview() {
    await backend.stopSitePreview()
    previewUrl.value = null
    previewBanner.value = ''
    previewNotice.value = null
  }

  async function deploymentStatus(articleId: string) {
    return backend.deploymentStatus(articleId)
  }

  // ---------------------------------------------------------------- 生命周期

  // 关闭窗口前强制 flush，尽力避免丢失改动。
  function handleBeforeUnload(event: BeforeUnloadEvent): void {
    if (hasUnsavedChanges.value) {
      void flush()
      event.preventDefault()
      event.returnValue = ''
    }
  }

  // 失焦时也 flush。
  function handleBlur(): void {
    void flush()
  }

  if (typeof window !== 'undefined') {
    window.addEventListener('beforeunload', handleBeforeUnload)
    window.addEventListener('blur', handleBlur)
  }

  onBeforeUnmount(() => {
    if (typeof window !== 'undefined') {
      window.removeEventListener('beforeunload', handleBeforeUnload)
      window.removeEventListener('blur', handleBlur)
    }
    if (debounceTimer !== null) {
      window.clearTimeout(debounceTimer)
    }
  })

  // 编辑守卫：正文或元数据变化即标记 dirty，只触发本地保存。
  watch([draftBody, draftMeta], () => {
    if (!current.value) return
    const meta = draftMeta.value
    if (!meta) return
    const changedBody = draftBody.value !== current.value.body
    const changedMeta = JSON.stringify(meta) !== JSON.stringify(current.value.meta)
    if (changedBody || changedMeta) {
      markDirty()
    }
  })

  return {
    // 状态
    connection,
    connectionError,
    articles,
    trash,
    recovery,
    listError,
    listLoading,
    current,
    draftMeta,
    draftBody,
    saveState,
    saveStateText,
    saveError,
    conflict,
    preferences,
    filter,
    sort,
    query,
    previewUrl,
    previewBanner,
    previewNotice,
    previewStarting,
    previewDependency,
    previewDependencyError,
    previewDependenciesPreparing,
    visibleArticles,
    hasUnsavedChanges,
    wordCount,
    checkingRemote,
    remoteCheckError,
    // 动作
    refreshConnection,
    acknowledgeDisclosure,
    connect,
    refreshAll,
    refreshList,
    refreshTrash,
    refreshRecovery,
    refreshPreferences,
    openArticle,
    checkRemote,
    refreshRemoteNow,
    flush,
    flushBeforeRemote,
    createArticle,
    importArticle,
    exportArticle,
    insertImage,
    insertImageBytes,
    listImages,
    assessSync,
    syncArticle,
    adoptRemote,
    resolveConflictManually,
    publishPrecheck,
    publishArticle,
    withdrawArticle,
    assessDelete,
    deleteArticle,
    retryDelete,
    restoreArticle,
    restoreRecovery,
    purgeTrash,
    discardRecovery,
    assessRename,
    renameUrl,
    savePreferences,
    startSitePreview,
    stopSitePreview,
    refreshPreviewDependencyStatus,
    preparePreviewDependencies,
    deploymentStatus,
  }
}

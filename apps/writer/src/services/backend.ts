/**
 * 强类型 invoke 包装。
 *
 * 这是前端唯一能触达后端的入口：不直连 Git、不直连文件系统，
 * 所有调用都走已注册的业务命令。错误统一为 `WriterError`。
 */
import { invoke } from '@tauri-apps/api/core'
import type {
  ArticleContent,
  ArticleFilter,
  ArticleMeta,
  ArticleSort,
  ArticleStatus,
  ArticleSummary,
  ConnectionStatus,
  DeleteAssessment,
  DeleteOutcome,
  DeploymentStatus,
  ImportedImage,
  PreviewSessionInfo,
  PublishOutcome,
  PublishPrecheck,
  RecoveryDraft,
  RenameAssessment,
  SyncAssessment,
  SyncOutcome,
  ToolchainReport,
  TrashEntry,
  UpdatedDateAction,
  WithdrawOutcome,
  WriterError,
  WritingPreferences,
} from '@/types/article'

/** 把任意抛出物规整为 `WriterError`。 */
export function toWriterError(error: unknown): WriterError {
  if (typeof error === 'object' && error !== null && 'code' in error && 'message' in error) {
    return error as WriterError
  }
  // Tauri 的错误可能是后端 `Err(WriterError)` 的序列化结果。
  if (typeof error === 'object' && error !== null) {
    const record = error as Record<string, unknown>
    if (typeof record.code === 'string' && typeof record.message === 'string') {
      return {
        code: record.code as WriterError['code'],
        message: record.message,
        ...(typeof record.detail === 'string' ? { detail: record.detail } : {}),
      }
    }
  }
  if (typeof error === 'string') {
    if (isMissingBridgeMessage(error)) {
      return { code: 'toolchain-missing', message: BRIDGE_UNAVAILABLE_MESSAGE }
    }
    return { code: 'io-failed', message: error }
  }
  // 桥缺失时 `invoke` 会抛出 TypeError；单独识别，避免显示「未知错误」。
  if (error instanceof Error) {
    if (isMissingBridgeMessage(error.message)) {
      return { code: 'toolchain-missing', message: BRIDGE_UNAVAILABLE_MESSAGE }
    }
    return { code: 'io-failed', message: error.message || '发生未知错误' }
  }
  return { code: 'io-failed', message: '发生未知错误' }
}

/**
 * 判断错误信息是否表示 Tauri IPC 桥不可用。
 *
 * `@tauri-apps/api` 在 `window.__TAURI_INTERNALS__` 缺失时会直接读取其属性，
 * 因此典型表现是 "Cannot read properties of undefined (reading 'invoke')"。
 */
function isMissingBridgeMessage(message: string): boolean {
  const lower = message.toLowerCase()
  return (
    lower.includes('__tauri') ||
    lower.includes('tauri_invoke') ||
    lower.includes('not in tauri') ||
    (lower.includes('cannot read properties of undefined') &&
      (lower.includes('invoke') || lower.includes('transformcallback'))) ||
    (lower.includes('undefined is not an object') && lower.includes('invoke'))
  )
}

/** 界面不在软件外壳中运行时的说明。 */
export const BRIDGE_UNAVAILABLE_MESSAGE =
  '当前界面没有连接到软件后端。请在「观澜志写作」软件窗口中操作；直接用浏览器打开界面无法读写文章。'

/** Tauri IPC 桥是否可用。 */
export function isBridgeAvailable(): boolean {
  if (typeof window === 'undefined') return false
  const candidate = window as unknown as { __TAURI_INTERNALS__?: { invoke?: unknown } }
  return typeof candidate.__TAURI_INTERNALS__?.invoke === 'function'
}

/** 调用一个命令并把错误规整化。 */
async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args)
  } catch (error) {
    throw toWriterError(error)
  }
}

/** 首次连接与能力检测。 */
export const backend = {
  connectionStatus: () => call<ConnectionStatus>('connection_status'),
  acknowledgeDisclosure: () => call<ConnectionStatus>('acknowledge_disclosure'),
  connect: (workspaceDir?: string) =>
    call<ConnectionStatus>('connect', { workspaceDir: workspaceDir ?? null }),
  toolchainReport: () => call<ToolchainReport>('toolchain_report'),

  /** 文章。 */
  listArticles: () => call<ArticleSummary[]>('list_articles'),
  readArticle: (articleId: string) => call<ArticleContent>('read_article', { articleId }),
  createArticle: (articleId: string, meta: ArticleMeta, body: string) =>
    call<ArticleContent>('create_article', { articleId, meta, body }),
  saveArticle: (
    articleId: string,
    meta: ArticleMeta,
    body: string,
    updatedDateAction?: UpdatedDateAction | null,
  ) =>
    call<ArticleContent>('save_article', {
      articleId,
      meta,
      body,
      // Rust 侧是 Option<UpdatedDateAction>：null / 省略表示不改动该字段。
      updatedDateAction: updatedDateAction ?? null,
    }),
  importArticle: (sourcePath: string, articleId: string) =>
    call<ArticleContent>('import_article', { sourcePath, articleId }),
  /** 插入图片：后端校验文件头与大小后归档到 public/blog/<article-id>/。 */
  importArticleImage: (articleId: string, sourcePath: string) =>
    call<ImportedImage>('import_article_image', { articleId, sourcePath }),

  /**
   * 插入**内存中的**图片字节（剪贴板粘贴、页面内拖入）。
   *
   * 用 Tauri 的原始请求体承载字节（避免把图片序列化成 JSON 数字数组），
   * 文章标识与文件名经 URL 编码放在请求头里。
   */
  importArticleImageBytes: async (
    articleId: string,
    fileName: string,
    bytes: Uint8Array,
  ): Promise<ImportedImage> => {
    try {
      return await invoke<ImportedImage>('import_article_image_bytes', bytes, {
        headers: {
          'x-guanlanzhi-article-id': encodeURIComponent(articleId),
          'x-guanlanzhi-file-name': encodeURIComponent(fileName),
        },
      })
    } catch (error) {
      throw toWriterError(error)
    }
  },

  listArticleImages: (articleId: string) =>
    call<ImportedImage[]>('list_article_images', { articleId }),

  /** 同步。 */
  assessSync: (articleId: string) => call<SyncAssessment>('assess_sync', { articleId }),
  syncArticle: (articleId: string, adoptLocal = false) =>
    call<SyncOutcome>('sync_article', { articleId, adoptLocal }),
  adoptRemote: (articleId: string) => call<ArticleContent>('adopt_remote', { articleId }),
  resolveConflictManually: (articleId: string, meta: ArticleMeta, body: string) =>
    call<ArticleContent>('resolve_conflict_manually', { articleId, meta, body }),

  /** 发布。 */
  publishPrecheck: (articleId: string) => call<PublishPrecheck>('publish_precheck', { articleId }),
  publishArticle: (
    articleId: string,
    removedImagePaths: string[] = [],
    removeOldMarkdownPath?: string,
  ) =>
    call<PublishOutcome>('publish_article', {
      articleId,
      removedImagePaths,
      removeOldMarkdownPath: removeOldMarkdownPath ?? null,
    }),
  deploymentStatus: (articleId: string) =>
    call<DeploymentStatus>('deployment_status', { articleId }),

  /** 撤下、删除、恢复。 */
  withdrawArticle: (articleId: string) =>
    call<WithdrawOutcome>('withdraw_article', { articleId }),
  assessDelete: (articleId: string) => call<DeleteAssessment>('assess_delete', { articleId }),
  deleteArticle: (articleId: string) => call<DeleteOutcome>('delete_article', { articleId }),
  retryDelete: (opId: string) => call<DeleteOutcome>('retry_delete', { opId }),
  restoreArticle: (opId: string) => call<ArticleContent>('restore_article', { opId }),
  listTrash: () => call<TrashEntry[]>('list_trash'),
  purgeTrash: (opId: string) => call<void>('purge_trash', { opId }),

  /** 崩溃恢复。 */
  pendingRecovery: () => call<RecoveryDraft[]>('pending_recovery'),
  discardRecovery: (articleId: string) => call<void>('discard_recovery', { articleId }),
  /** 把当前内存中的元数据与正文写入本机恢复区（编辑过程中轻量防抖调用）。 */
  snapshotRecovery: (articleId: string, meta: ArticleMeta, body: string) =>
    call<void>('snapshot_recovery', { articleId, meta, body }),
  readRecovery: (articleId: string) =>
    call<RecoveryDraft | null>('read_recovery', { articleId }),
  /** 用恢复副本覆盖本地文章文件（用户显式确认）。 */
  restoreRecovery: (articleId: string) =>
    call<ArticleContent>('restore_recovery', { articleId }),

  /** URL 改名。 */
  assessRenameUrl: (oldId: string, newId: string) =>
    call<RenameAssessment>('assess_rename_url', { oldId, newId }),
  renameArticleUrl: (oldId: string, newId: string, confirmPublished: boolean) =>
    call<ArticleContent>('rename_article_url', { oldId, newId, confirmPublished }),

  /** 写作偏好与预览。 */
  getPreferences: () => call<WritingPreferences>('get_preferences'),
  setPreferences: (preferences: WritingPreferences) =>
    call<WritingPreferences>('set_preferences', { preferences }),
  startSitePreview: (articleId: string, simulatePublic: boolean) =>
    call<PreviewSessionInfo>('start_site_preview', { articleId, simulatePublic }),
  stopSitePreview: () => call<void>('stop_site_preview'),
  statusOverview: () => call<ArticleStatus[]>('status_overview'),
}

/** 按筛选与排序条件得到列表。 */
export function filterArticles(
  articles: ArticleSummary[],
  filter: ArticleFilter,
): ArticleSummary[] {
  switch (filter) {
    case 'all':
      return articles
    case 'local-only':
      return articles.filter((a) => a.status.remoteSync !== 'saved')
    case 'remote-saved':
      return articles.filter((a) => a.status.remoteSync === 'saved')
    case 'site-published':
      // 「已发布到网站」= 这篇文章已经进入 `main`（含刚提交、部署中、已上线、
      // 部署失败）。只有从未发布与已撤下的不算。刻意不用单一布尔值判断，
      // 避免把「已提交但未上线」误当成已上线、或把部署失败漏掉。
      return articles.filter(
        (a) => a.status.site !== 'never-published' && a.status.site !== 'withdrawn',
      )
    case 'conflict':
      // 解析异常也归入需要处理的一类，避免被静默忽略。
      return articles.filter((a) => a.status.remoteSync === 'conflict' || Boolean(a.loadError))
    case 'trash':
      return []
  }
}

/** 搜索标题、摘要、标签与 ID。 */
export function searchArticles(articles: ArticleSummary[], query: string): ArticleSummary[] {
  const needle = query.trim().toLowerCase()
  if (!needle) return articles
  return articles.filter((article) => {
    const haystack = [article.id, article.title, article.description, ...article.tags]
      .join('\n')
      .toLowerCase()
    return haystack.includes(needle)
  })
}

/** 排序：网站按发布日期倒序，软件支持按最近编辑排序。 */
export function sortArticles(articles: ArticleSummary[], sort: ArticleSort): ArticleSummary[] {
  const copy = [...articles]
  if (sort === 'recent-edited') {
    copy.sort((a, b) => (b.lastEditedUnix ?? 0) - (a.lastEditedUnix ?? 0))
  } else {
    copy.sort((a, b) => (a.pubDate < b.pubDate ? 1 : a.pubDate > b.pubDate ? -1 : 0))
  }
  return copy
}

/** 字节数的人类可读表示。 */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`
  return `${(bytes / 1024 / 1024).toFixed(1)} MiB`
}

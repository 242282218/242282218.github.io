/**
 * 前后端共享 JSON 协议的类型镜像。
 *
 * 与 `src-tauri/src/model.rs`、`sync.rs`、`publish.rs`、`trash.rs` 中的
 * `Serialize` 结构一一对应。字段类型显式，不用单个 `published: boolean`
 * 概括本地、远程与网站三类状态。
 */

/** 远程写作分支的同步状态。 */
export type RemoteSync = 'local-only' | 'saving' | 'saved' | 'conflict' | 'failed'

/** 主站版本与 Pages 部署状态。 */
export type SiteState =
  | 'never-published'
  | 'live-old-version'
  | 'publication-submitted'
  | 'deploying'
  | 'live-current-version'
  | 'deploy-failed'
  | 'withdrawn'

/** 业务错误码。前端据此给出操作性提示。 */
export type ErrorCode =
  | 'front-matter-missing'
  | 'front-matter-unterminated'
  | 'front-matter-invalid'
  | 'meta-field-invalid'
  | 'article-id-invalid'
  | 'article-exists'
  | 'article-not-found'
  | 'path-out-of-scope'
  | 'image-unsupported'
  | 'image-too-large'
  | 'git-failed'
  | 'remote-changed'
  | 'push-rejected'
  | 'auth-failed'
  | 'offline'
  | 'toolchain-missing'
  | 'preview-failed'
  | 'build-failed'
  | 'io-failed'
  | 'invalid-argument'

/** 结构化业务错误。 */
export type WriterError = {
  code: ErrorCode
  message: string
  detail?: string
}

/** 文章状态：本地、writing、main 三处独立表达。 */
export type ArticleStatus = {
  locallySaved: boolean
  remoteSync: RemoteSync
  site: SiteState
  /** 本地文件完整内容哈希。 */
  localBodyHash: string
  writingBodyHash?: string
  mainBodyHash?: string
  mainCommit?: string
  deploymentUrl?: string
}

/** 文章站点元数据（对应 `src/content.config.ts` 的 schema）。 */
export type ArticleMeta = {
  title: string
  description: string
  /** `YYYY-MM-DD` */
  pubDate: string
  updatedDate?: string
  tags: string[]
  draft: boolean
}

/** 文章来源。 */
export type ArticleSource = 'workspace' | 'imported'

/** 文章列表项。 */
export type ArticleSummary = {
  id: string
  title: string
  description: string
  tags: string[]
  pubDate: string
  updatedDate?: string
  draft: boolean
  imageCount: number
  source: ArticleSource
  lastEditedUnix?: number
  status: ArticleStatus
  /** 读取或解析异常；存在时条目仍要展示并给出原文入口。 */
  loadError?: WriterError
}

/** 编辑页所需完整内容。 */
export type ArticleContent = {
  id: string
  meta: ArticleMeta
  rawFrontMatter: string
  body: string
  contentHash: string
  status: ArticleStatus
}

/** 同步判定结果。 */
export type SyncDecision = 'up-to-date' | 'ready' | 'remote-changed'

/** 冲突来源。 */
export type ConflictSource = { kind: 'markdown' } | { kind: 'image'; relPath: string }

/** 同步评估结果（差异界面数据源）。 */
export type SyncAssessment = {
  articleId: string
  decision: SyncDecision
  localHash: string
  writingHash?: string
  baseHash?: string
  writingMarkdown?: string
  localMarkdown: string
  conflicts: ConflictSource[]
  imagePaths: string[]
}

/** 同步结果。 */
export type SyncOutcome = {
  pushedCommit?: string
  writingHash?: string
  createdBranch: boolean
}

/** 发布预检的四个可核对事实。 */
export type PublishPrecheck = {
  articleId: string
  title: string
  imageFileCount: number
  imageTotalBytes: number
  targetRepo: string
  targetBranch: string
  mainHead: string
  mainHash?: string
  writingHash: string
  differsFromOnline: boolean
}

/** 发布结果。 */
export type PublishOutcome = {
  commit: string
  mainHash: string
  changedPaths: string[]
}

/** 撤下结果。 */
export type WithdrawOutcome = {
  opId: string
  writingDone: boolean
  mainDone: boolean
  mainCommit?: string
}

/** 删除影响评估。 */
export type DeleteAssessment = {
  articleId: string
  title: string
  markdownRelPath: string
  exclusiveImages: string[]
  protectedImages: string[]
  onWriting: boolean
  onMain: boolean
  wasPublished: boolean
  writingHead?: string
  mainHead?: string
}

/** 删除结果：两个分支的完成状态分开表达。 */
export type DeleteOutcome = {
  opId: string
  writingDone: boolean
  mainDone: boolean
  removedImages: string[]
  keptImages: string[]
  branchState: string
  lastError?: string
}

/** 回收区条目。 */
export type TrashEntry = {
  opId: string
  kind: 'deleted' | 'withdrawn'
  articleId: string
  title: string
  deletedAt: string
  markdownRelPath: string
  hasMarkdown: boolean
  images: TrashImage[]
  writingDone: boolean
  mainDone: boolean
  wasPublished: boolean
  sourceWritingSha?: string
  sourceMainSha?: string
  lastError?: string
}

export type TrashImage = {
  relPath: string
  backupName: string
  size: number
  contentHash: string
}

/** 崩溃恢复副本。 */
export type RecoveryDraft = {
  articleId: string
  markdownRelPath: string
  markdown: string
  savedAtUnix: number
}

/** 单个外部工具的可用性。 */
export type ToolAvailability = {
  available: boolean
  version?: string
}

/** 网站预览的前置条件报告。 */
export type ToolchainReport = {
  git: ToolAvailability
  node: ToolAvailability
  pnpm: ToolAvailability
  nodeMeetsMinimum: boolean
  missing: string[]
  guidance: string[]
}

/** 首次连接状态。 */
export type ConnectionStatus = {
  repoLabel: string
  repoUrl: string
  workspaceDir: string
  connected: boolean
  workspaceReady: boolean
  toolchain: ToolchainReport
  publicDisclosure: string
  disclosedPublicDrafts: boolean
  blockingIssue?: string
}

/** 写作外观偏好：只影响本机，绝不写入文章或网站 CSS。 */
export type WritingPreferences = {
  fontSize: number
  lineHeight: number
  codeTheme: string
  previewWidth: number
  editorMode: EditorMode
  autoSaveDebounceMs: number
}

/** 编辑器模式：`sv` 源码分屏 / `ir` 正文即时渲染。 */
export type EditorMode = 'sv' | 'ir'

/**
 * 对 `updatedDate` 字段的一次显式操作。
 *
 * 不能只用一个可空字符串：那样无法区分「不改动该字段」与「删除该字段」。
 * 省略该参数即表示本次保存不改动 `updatedDate`。
 */
export type UpdatedDateAction =
  | { action: 'set'; value: string }
  | { action: 'remove' }

/** 网站预览会话信息。 */
export type PreviewSessionInfo = {
  url: string
  banner: string
  tempRoot: string
  offlineFontNotice?: string
  simulatePublic: boolean
}

/** 部署状态查询结果。 */
export type DeploymentStatus = {
  commit?: string
  state: string
  runUrl?: string
  checked: boolean
  notice?: string
}

/** URL 改名影响评估。 */
export type RenameAssessment = {
  oldId: string
  newId: string
  oldUrl: string
  newUrl: string
  published: boolean
  referencingArticles: string[]
  requiresRepublish: boolean
}

/** 已归档到文章目录的图片。 */
export type ImportedImage = {
  articleId: string
  /** 仓库内相对路径，如 `public/blog/read-code/figure-01-abcd1234.png`。 */
  relPath: string
  /** 站点根路径，写入 Markdown 的形式，如 `/blog/read-code/figure-01-abcd1234.png`。 */
  url: string
  fileName: string
  size: number
  contentHash: string
  mime?: string
}

/** 文章列表筛选。 */
export type ArticleFilter =
  | 'all'
  | 'local-only'
  | 'remote-saved'
  | 'site-published'
  | 'conflict'
  | 'trash'

/** 文章列表排序。 */
export type ArticleSort = 'recent-edited' | 'pub-date'

/** 三类状态的短文案与颜色提示（颜色只是辅助，必须有文字）。 */
export function describeStatus(status: ArticleStatus): {
  local: string
  remote: string
  site: string
  tone: 'neutral' | 'info' | 'warn' | 'danger' | 'ok'
} {
  const local = status.locallySaved ? '本地已保存' : '尚未保存到磁盘'

  const remoteMap: Record<RemoteSync, string> = {
    'local-only': '尚未同步',
    saving: '正在同步',
    saved: '远程已存',
    conflict: '远端有冲突',
    failed: '同步失败',
  }

  const siteMap: Record<SiteState, string> = {
    'never-published': '从未发布',
    'live-old-version': '网站仍是旧版',
    'publication-submitted': '已提交发布',
    deploying: '部署中',
    'live-current-version': '网站已上线',
    'deploy-failed': '部署失败',
    withdrawn: '已从网站撤下',
  }

  const toneBySite: Partial<Record<SiteState, 'neutral' | 'info' | 'warn' | 'danger' | 'ok'>> = {
    'live-old-version': 'warn',
    'publication-submitted': 'info',
    deploying: 'info',
    'live-current-version': 'ok',
    'deploy-failed': 'danger',
  }

  return {
    local,
    remote: remoteMap[status.remoteSync],
    site: siteMap[status.site],
    tone: toneBySite[status.site] ?? 'neutral',
  }
}

/** 危险操作的中文标签（按钮必须有文字，不只靠颜色）。 */
export const DANGER_LABELS = {
  withdraw: '从网站撤下',
  delete: '从 GitHub 当前版本删除文件',
} as const

/**
 * 首次启动必须展示的公开性说明。
 *
 * 与 Rust 侧 `connection::PUBLIC_DISCLOSURE` 保持一致；这里保留一份前端常量，
 * 使「公开仓库会公开远程草稿」这一事实在**后端不可达时也照常展示**，
 * 不会因为读取失败就跳过披露。
 */
export const PUBLIC_DISCLOSURE =
  '本软件使用公开仓库保存远程草稿：在写作分支中同步的草稿对任何访问者可见，网站上不会展示未发布的文章。'

/** 删除确认必须区分的两件事。 */
export const DELETE_CONFIRM_NOTICE =
  '「从网站撤下」只让文章不再公开；「删除文件」会从当前分支移除文件，但 Git 历史中仍可见。'

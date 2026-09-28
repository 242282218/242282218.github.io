/**
 * 前后端共享 JSON 协议的类型镜像。
 *
 * 与 `src-tauri/src/model.rs`、`sync.rs`、`publish.rs`、`trash.rs` 中的
 * `Serialize` 结构一一对应。字段类型显式，不用单个 `published: boolean`
 * 概括本地、远程与网站三类状态。
 */

/** 远程写作分支的同步状态。 */
export type RemoteSync = 'unverified' | 'local-only' | 'saving' | 'saved' | 'conflict' | 'failed'

/** 主站版本与 Pages 部署状态。 */
export type SiteState =
  | 'unverified'
  | 'never-published'
  | 'live-old-version'
  | 'publication-submitted'
  | 'deploying'
  | 'live-current-version'
  | 'deploy-failed'
  | 'withdrawn'

/**
 * 单个远端分支上一次核对的结论类别。
 *
 * `undefined` 无法区分「没查过」「查了但失败」「确认不存在」——三者对用户
 * 意味着完全不同的下一步，因此必须分开表达；未知一律不得等同于肯定结论。
 */
export type CheckState = 'unverified' | 'absent' | 'present'

/** 单个远端分支上该文章的核对结论。 */
export type BranchCheck = {
  state: CheckState
  /** 未核对时的原因（面向用户）。 */
  reason?: string
  /** 核对时所依据的远端头（分支不存在则为空）。 */
  head?: string
  /** 核对时间（Unix 秒）。 */
  checkedAtUnix?: number
  /** 该文件的原始内容哈希（`present` 时才有值，含 `draft` 字段）。 */
  bodyHash?: string
  /** 该文件按站点发布版规范化后的哈希（`present` 时才有值）。 */
  siteHash?: string
  /** 该分支上是否公开（`draft: false`）。 */
  published?: boolean
  /** 该分支上该文章记录的上线地址。 */
  deploymentUrl?: string
}

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
  | 'preview-dependencies-missing'
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
  /** `writing` 分支的核对结论（含未核对态）。 */
  writing: BranchCheck
  /** `main` 分支的核对结论（含未核对态）。 */
  main: BranchCheck
  /** 远端两分支中最近一次核对时间（Unix 秒）。未核对时为空。 */
  remoteCheckedAtUnix?: number
}

/** 一次远端核对的结果；`localBodyHash` 用于丢弃过期结果。 */
export type RemoteCheckOutcome = {
  articleId: string
  localBodyHash: string
  status: ArticleStatus
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

/**
 * 环境检查状态。
 *
 * 启动路径**不**执行探测命令，只读上次结果；因此界面必须先显示「尚未检查」，
 * 由用户显式触发后才变成有结论的状态。「找到候选文件」不等于「程序可执行」。
 */
export type ToolchainState =
  | { kind: 'unchecked' }
  | { kind: 'checkFailed'; reason: string }
  | { kind: 'checked'; atUnix: number; report: ToolchainReport }

/** 首次连接状态。 */
export type ConnectionStatus = {
  repoLabel: string
  repoUrl: string
  workspaceDir: string
  connected: boolean
  workspaceReady: boolean
  toolchain: ToolchainState
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
  /** 外壳配色。`system` 跟随操作系统，是默认值。 */
  shellTheme: ShellTheme
}

/** 外壳配色：跟随系统 / 浅色 / 深色。只作用于软件外壳，站点预览始终浅色。 */
export type ShellTheme = 'system' | 'light' | 'dark'

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

/**
 * 网站预览依赖的准备状态。
 *
 * `preparing` 只会在**真的启动了后台任务**时出现：启动预览本身不会安装依赖，
 * 缺依赖时返回 `preview-dependencies-missing` 错误；准备任务由用户显式触发，
 * 带真实任务标识，可去重与查询。
 */
export type PreviewDependencyStatus =
  | { kind: 'missing' }
  | { kind: 'preparing'; taskId: string; startedAtUnix: number }
  | { kind: 'ready'; taskId?: string; checkedAtUnix: number }
  | { kind: 'failed'; taskId: string; reason: string; detail?: string; failedAtUnix: number }

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
  | 'unverified'
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
    // 「待核对」必须与「尚未同步」分开：前者是不知道，后者是核对了确实没有。
    unverified: '远端待核对',
    'local-only': '尚未同步',
    saving: '正在同步',
    saved: '远程已存',
    conflict: '远端有冲突',
    failed: '同步失败',
  }

  const siteMap: Record<SiteState, string> = {
    unverified: '网站状态待核对',
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

/**
 * 状态栏的「上次远端核对时间」文案。
 *
 * 未核对时必须是明确的未知，不能因为「没有时间」就省略这一项——省略会让
 * 用户以为状态是新鲜的。
 */
export function describeRemoteCheckedAt(checkedAtUnix?: number): string {
  if (!checkedAtUnix) return '尚未核对远端'
  const at = new Date(checkedAtUnix * 1000)
  const pad = (n: number) => String(n).padStart(2, '0')
  return `上次远端核对：${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())} ${pad(at.getHours())}:${pad(at.getMinutes())}`
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

/**
 * 界面夹具：在浏览器中渲染真实的 App 外壳，并用内存中的假后端替换 Tauri IPC。
 *
 * 目的：在没有 Tauri 运行时的情况下，验证三栏布局、窄屏选项卡、状态文案与
 * 危险操作确认清单的渲染。所有数据都是**测试样稿**，不会写入任何仓库。
 *
 * 不替换任何真实文件；仅用于界面验收。
 */
import { createApp } from 'vue'
import App from '@/App.vue'
import '@/styles/tokens.css'
import 'vditor/dist/index.css'
import type {
  ArticleContent,
  ArticleSummary,
  ConnectionStatus,
  DeploymentStatus,
  DeleteAssessment,
  PublishPrecheck,
  SyncAssessment,
  TrashEntry,
  WriterError,
} from '@/types/article'

/** 测试样稿的元数据。 */
function sampleMeta(title: string, draft: boolean) {
  return {
    title,
    description: `${title} 的测试摘要。`,
    pubDate: '2026-09-23',
    tags: ['测试样稿'],
    draft,
  }
}

const ARTICLE_BODY =
  '这是测试样稿正文，不对应真实观澜记录。\n\n## 先用自己的话复述\n\n例如，看到一个按日期排列条目的函数，可以先把目标写成一句话。\n\n```ts\nexport function pick<T>(items: readonly T[]): T[] {\n  return [...items].filter(Boolean)\n}\n```\n\n| 步骤 | 说明 |\n| --- | --- |\n| 一 | 找输入与输出 |\n\n![测试图](/blog/mock-article/figure-01.png)\n'

const articles: ArticleSummary[] = [
  {
    id: 'mock-article',
    title: '测试样稿：网站仍是旧版',
    description: '用于界面验收的生成式样稿，展示「网站仍是旧版」状态。',
    tags: ['测试样稿', '阅读'],
    pubDate: '2026-09-23',
    draft: true,
    imageCount: 1,
    source: 'workspace',
    lastEditedUnix: 1789000000,
    status: {
      locallySaved: true,
      remoteSync: 'saved',
      site: 'live-old-version',
      localBodyHash: 'a'.repeat(64),
      writingBodyHash: 'a'.repeat(64),
      mainBodyHash: 'b'.repeat(64),
      mainCommit: 'c'.repeat(40),
    },
  },
  {
    id: 'mock-draft-only',
    title: '测试样稿：从未发布',
    description: '展示「从未发布」与「尚未同步」的状态组合。',
    tags: ['测试样稿'],
    pubDate: '2026-09-20',
    draft: true,
    imageCount: 0,
    source: 'workspace',
    lastEditedUnix: 1788000000,
    status: {
      locallySaved: true,
      remoteSync: 'local-only',
      site: 'never-published',
      localBodyHash: 'd'.repeat(64),
    },
  },
  {
    id: 'mock-conflict',
    title: '测试样稿：远端冲突',
    description: '展示「远端有冲突」与部署失败的状态组合。',
    tags: ['测试样稿'],
    pubDate: '2026-09-18',
    draft: false,
    imageCount: 2,
    source: 'workspace',
    lastEditedUnix: 1787000000,
    status: {
      locallySaved: true,
      remoteSync: 'conflict',
      site: 'deploy-failed',
      localBodyHash: 'e'.repeat(64),
      writingBodyHash: 'f'.repeat(64),
      mainCommit: '1'.repeat(40),
    },
  },
  {
    id: 'mock-broken',
    title: 'src/content/blog/mock-broken.md',
    description: '',
    tags: [],
    pubDate: '',
    draft: true,
    imageCount: 0,
    source: 'workspace',
    status: {
      locallySaved: true,
      remoteSync: 'local-only',
      site: 'never-published',
      localBodyHash: '2'.repeat(64),
    },
    loadError: {
      code: 'front-matter-missing',
      message: '文件未以 --- 包围的 front matter 开头，无法安全编辑',
      detail: '第 1 行',
    },
  },
]

const connection: ConnectionStatus = {
  repoLabel: 'guanlangzg/guanlangzg.github.io',
  repoUrl: 'https://github.com/guanlangzg/guanlangzg.github.io.git',
  workspaceDir: 'C:\\Users\\example\\AppData\\Local\\guanlanzhi-writer\\workspace',
  connected: true,
  workspaceReady: true,
  toolchain: {
    git: { available: true, version: 'git version 2.52.0.windows.1' },
    node: { available: true, version: 'v24.12.0' },
    pnpm: { available: true, version: '10.28.2' },
    nodeMeetsMinimum: true,
    missing: [],
    guidance: [],
  },
  publicDisclosure:
    '本软件使用公开仓库保存远程草稿：在写作分支中同步的草稿对任何访问者可见，网站上不会展示未发布的文章。',
  disclosedPublicDrafts: true,
}

function contentFor(id: string): ArticleContent {
  const found = articles.find((item) => item.id === id) ?? articles[0]!
  return {
    id,
    meta: found.loadError ? sampleMeta(found.title, true) : sampleMeta(found.title, found.draft),
    rawFrontMatter: `title: "${found.title}"\ndescription: "${found.description}"\npubDate: "${found.pubDate}"\ntags: [测试样稿]\ndraft: ${found.draft}`,
    body: ARTICLE_BODY,
    contentHash: found.status.localBodyHash,
    status: found.status,
  }
}

const trash: TrashEntry[] = [
  {
    opId: 'op-mock-partial',
    kind: 'deleted',
    articleId: 'mock-deleted',
    title: '测试样稿：部分删除',
    deletedAt: '2026-09-25',
    markdownRelPath: 'src/content/blog/mock-deleted.md',
    hasMarkdown: true,
    images: [],
    writingDone: true,
    mainDone: false,
    wasPublished: true,
    lastError: '网站分支：推送前发现远端已更新，已停止；可重试以基于最新版本补做',
  },
  {
    opId: 'op-mock-done',
    kind: 'deleted',
    articleId: 'mock-removed',
    title: '测试样稿：已完成删除',
    deletedAt: '2026-09-24',
    markdownRelPath: 'src/content/blog/mock-removed.md',
    hasMarkdown: true,
    images: [
      { relPath: 'public/blog/mock-removed/figure-01.png', backupName: 'figure-01.png', size: 1024, contentHash: '3'.repeat(64) },
    ],
    writingDone: true,
    mainDone: true,
    wasPublished: false,
  },
]

const precheck: PublishPrecheck = {
  articleId: 'mock-article',
  title: '测试样稿：网站仍是旧版',
  imageFileCount: 1,
  imageTotalBytes: 2048,
  targetRepo: 'guanlangzg/guanlangzg.github.io',
  targetBranch: 'main',
  mainHead: '4'.repeat(40),
  mainHash: 'b'.repeat(64),
  writingHash: 'a'.repeat(64),
  differsFromOnline: true,
}

const assessment: SyncAssessment = {
  articleId: 'mock-article',
  decision: 'remote-changed',
  localHash: '5'.repeat(64),
  writingHash: '6'.repeat(64),
  baseHash: 'a'.repeat(64),
  writingMarkdown:
    '---\ntitle: "测试样稿：网站仍是旧版"\ndraft: true\n---\n\n远端设备改过的正文。\n\n| 步骤 | 说明 |\n| --- | --- |\n| 一 | 找输入与输出 |\n',
  localMarkdown:
    '---\ntitle: "测试样稿：网站仍是旧版"\ndraft: true\n---\n\n本地改过的正文。\n\n| 步骤 | 说明 |\n| --- | --- |\n| 一 | 先找边界 |\n',
  conflicts: [{ kind: 'markdown' }, { kind: 'image', relPath: 'public/blog/mock-article/figure-01.png' }],
  imagePaths: ['public/blog/mock-article/figure-01.png'],
}

const deleteAssessment: DeleteAssessment = {
  articleId: 'mock-article',
  title: '测试样稿：网站仍是旧版',
  markdownRelPath: 'src/content/blog/mock-article.md',
  exclusiveImages: ['public/blog/mock-article/figure-01.png'],
  protectedImages: ['public/blog/shared/shared-figure.png'],
  onWriting: true,
  onMain: true,
  wasPublished: true,
  writingHead: '7'.repeat(40),
  mainHead: '8'.repeat(40),
}

const deployment: DeploymentStatus = {
  commit: 'c'.repeat(40),
  state: 'deploy-failed',
  runUrl: 'https://github.com/guanlangzg/guanlangzg.github.io/actions',
  checked: true,
}

/** 命令处理器表：键为命令名，值为根据参数返回数据的函数。 */
const handlers: Record<string, (args: Record<string, unknown>) => unknown> = {
  connection_status: () => connection,
  acknowledge_disclosure: () => connection,
  connect: () => connection,
  toolchain_report: () => connection.toolchain,
  list_articles: () => articles,
  read_article: (args: Record<string, unknown>) => contentFor(String(args.articleId)),
  create_article: (args: Record<string, unknown>) => ({
    ...contentFor(String(args.articleId)),
    meta: args.meta,
  }),
  save_article: (args: Record<string, unknown>) => ({
    ...contentFor(String(args.articleId)),
    meta: args.meta,
    body: String(args.body),
  }),
  import_article: (args: Record<string, unknown>) => contentFor(String(args.articleId)),
  import_article_image: (args: Record<string, unknown>) => ({
    articleId: args.articleId,
    relPath: `public/blog/${args.articleId}/figure-mock-0000.png`,
    url: `/blog/${args.articleId}/figure-mock-0000.png`,
    fileName: 'figure-mock.png',
    size: 1234,
    contentHash: 'a'.repeat(64),
    mime: 'image/png',
  }),
  list_article_images: () => [],
  /** 记录最近一次粘贴/拖入收到的字节，供界面夹具断言。 */
  import_article_image_bytes: (args: Record<string, unknown>) => {
    const bytes = args.__rawBytes as Uint8Array | undefined
    const headers = (args.__headers ?? {}) as Record<string, string>
    const id = decodeURIComponent(headers['x-guanlanzhi-article-id'] ?? 'unknown')
    const name = decodeURIComponent(headers['x-guanlanzhi-file-name'] ?? 'clip.png')
    lastRawInvoke = { byteLength: bytes?.byteLength ?? 0, articleId: id, fileName: name }
    return {
      articleId: id,
      relPath: `public/blog/${id}/figure-pasted-0000.png`,
      url: `/blog/${id}/figure-pasted-0000.png`,
      fileName: name,
      size: bytes?.byteLength ?? 0,
      contentHash: 'b'.repeat(64),
      mime: 'image/png',
    }
  },
  assess_sync: () => assessment,
  sync_article: () => {
    throw { code: 'remote-changed', message: '远端这篇文章已变化，需要先在差异界面选择处理方式' }
  },
  adopt_remote: (args: Record<string, unknown>) => contentFor(String(args.articleId)),
  resolve_conflict_manually: (args: Record<string, unknown>) => ({
    ...contentFor(String(args.articleId)),
    meta: args.meta,
    body: args.body,
  }),
  publish_precheck: () => precheck,
  publish_article: () => ({ commit: '9'.repeat(40), mainHash: 'b'.repeat(64), changedPaths: ['src/content/blog/mock-article.md'] }),
  deployment_status: () => deployment,
  withdraw_article: () => ({ opId: 'op-w', writingDone: true, mainDone: true, mainCommit: 'a'.repeat(40) }),
  assess_delete: () => deleteAssessment,
  delete_article: () => ({
    opId: 'op-d',
    writingDone: true,
    mainDone: false,
    removedImages: [],
    keptImages: ['public/blog/shared/shared-figure.png'],
    branchState: '写作分支已处理／网站仍在',
    lastError: '网站分支：推送前发现远端已更新，已停止',
  }),
  retry_delete: () => ({
    opId: 'op-mock-partial',
    writingDone: true,
    mainDone: true,
    removedImages: [],
    keptImages: [],
    branchState: '写作分支与网站均已处理',
  }),
  restore_article: () => contentFor('mock-deleted'),
  list_trash: () => trash,
  purge_trash: () => null,
  pending_recovery: () => [
    {
      articleId: 'mock-article',
      markdownRelPath: 'src/content/blog/mock-article.md',
      markdown: ARTICLE_BODY,
      savedAtUnix: 1789000000,
    },
  ],
  discard_recovery: (args: Record<string, unknown>) => {
    lastRecoveryCall = { action: 'discard', articleId: String(args.articleId) }
    return null
  },
  snapshot_recovery: () => null,
  read_recovery: () => null,
  restore_recovery: (args: Record<string, unknown>) => {
    const id = String(args.articleId)
    lastRecoveryCall = { action: 'restore', articleId: id }
    return contentFor(id)
  },
  assess_rename_url: () => ({
    oldId: 'mock-article',
    newId: 'mock-article-2',
    oldUrl: '/blog/mock-article/',
    newUrl: '/blog/mock-article-2/',
    published: true,
    referencingArticles: ['mock-conflict'],
    requiresRepublish: true,
  }),
  rename_article_url: (args: Record<string, unknown>) => contentFor(String(args.newId)),
  get_preferences: () => ({
    fontSize: 16,
    lineHeight: 175,
    codeTheme: 'github',
    previewWidth: 460,
    editorMode: 'sv',
    autoSaveDebounceMs: 800,
  }),
  set_preferences: (args: Record<string, unknown>) => args.preferences,
  start_site_preview: () => ({
    url: 'http://127.0.0.1:4321/',
    banner: '本地预览 · 尚未发布',
    tempRoot: 'C:\\Users\\example\\AppData\\Local\\guanlanzhi-writer\\preview\\mock',
    offlineFontNotice: '离线状态下，网页字体可能回退到系统中的替代字体，视觉与联网时可能不同',
    simulatePublic: true,
  }),
  stop_site_preview: () => null,
  status_overview: () => articles.map((item) => item.status),
}

// 伪装 Tauri IPC 桥，让真实前端代码走原路径。
/** 最近一次「原始字节插图」调用收到的内容，供测试断言。 */
let lastRawInvoke: { byteLength: number; articleId: string; fileName: string } | null = null

/** 最近一次崩溃恢复操作（恢复/丢弃），供测试断言。 */
let lastRecoveryCall: { action: 'restore' | 'discard'; articleId: string } | null = null

;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
  invoke: async (
    command: string,
    args: Record<string, unknown> | Uint8Array = {},
    options?: { headers?: Record<string, string> },
  ) => {
    // 原始体调用：第二个参数是 Uint8Array，第三个参数带 headers。
    if (args instanceof Uint8Array) {
      const handler = handlers[command]
      if (!handler) {
        throw { code: 'invalid-argument', message: `夹具未实现命令：${command}` }
      }
      return handler({
        __rawBytes: args,
        __headers: options?.headers ?? {},
      })
    }
    const handler = handlers[command]
    if (!handler) {
      const error: WriterError = { code: 'invalid-argument', message: `夹具未实现命令：${command}` }
      throw error
    }
    return handler(args as Record<string, unknown>)
  },
  transformCallback: (callback: unknown) => callback,
  convertFileSrc: (value: string) => value,
}

// 暴露给界面夹具断言用的读取口。
;(window as unknown as Record<string, unknown>).__mockState = {
  lastRawInvoke: () => lastRawInvoke,
  lastRecoveryCall: () => lastRecoveryCall,
}

createApp(App).mount('#app')

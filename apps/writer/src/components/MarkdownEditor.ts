/**
 * Vditor 适配层：正文读写与模式切换。
 *
 * 安全约定（对应方案第三节与阶段 0）：
 * - 前端**不接触 front matter**：这里只处理正文，元数据由后端独立维护；
 * - 模式切换前先等待中文输入法 composition 完成，再比较往返转换结果；
 * - 往返导致内容或元数据损坏时**不覆盖**最后可恢复的原始 Markdown，
 *   而是发出明确提示并回退到源码模式（`sv`）。
 */
import { onBeforeUnmount, ref, shallowRef, watch, type Ref } from 'vue'
import type { EditorMode } from '@/types/article'
import { sanitizeHtml } from '@/services/sanitize'

/** Vditor 实例的最小结构（避免引入其未导出的内部类型）。 */
type VditorInstance = {
  getValue: () => string
  setValue: (value: string, clearStack?: boolean) => void
  destroy: () => void
  focus: () => void
  disabled: () => void
  enable: () => void
  setPreviewMode?: (mode: 'both' | 'editor' | 'preview') => void
  /** 在光标处插入内容（保留撤销栈）。 */
  insertValue?: (value: string) => void
}

/** Vditor 构造函数的形状（仅描述本适配层用到的部分）。 */
export type VditorConstructor = new (
  element: HTMLElement,
  options: Record<string, unknown>,
) => VditorInstance

/** 一次模式切换的验收结果。 */
export type RoundTripReport = {
  from: EditorMode
  to: EditorMode
  /** 往返前的正文。 */
  before: string
  /** 往返后的正文。 */
  after: string
  /** 是否发生了（可接受的）规范化差异。 */
  normalized: boolean
  /** 规范化的种类，用于界面给出具体提示。 */
  normalizationKind?: 'whitespace' | 'table-format'
  /** 是否应当阻止切换并回退源码模式。 */
  blocked: boolean
  /** 说明差异的中文原因。 */
  reason: string
}

/** 判断两段正文是否「语义等价」：忽略行尾空白与结尾空行差异。 */
function normalizeWhitespace(text: string): string {
  return text
    .replace(/\r\n/g, '\n')
    .split('\n')
    .map((line) => line.replace(/[ \t]+$/, ''))
    .join('\n')
    .replace(/\n+$/, '')
}

/**
 * 折叠连续空行，并把块级元素之间统一为单个空行。
 *
 * Markdown 中连续多个空行与单个空行渲染结果一致；实测 `ir` 模式会在代码块
 * 与后续表格之间补一个空行，这属于可接受的规范化。
 */
function collapseBlankLines(text: string): string {
  return text.replace(/\n{2,}/g, '\n\n').replace(/^\n+/, '')
}

/** 判断一行是否是 GFM 表格的分隔行（`| --- | --- |`）。 */
function isTableSeparator(line: string): boolean {
  const trimmed = line.trim()
  if (!trimmed.includes('-') || !trimmed.startsWith('|')) return false
  return trimmed
    .split('|')
    .filter((cell) => cell.trim() !== '')
    .every((cell) => /^:?-+:?$/.test(cell.trim()))
}

/** 判断一行是否是 GFM 表格的数据行。 */
function isTableRow(line: string): boolean {
  const trimmed = line.trim()
  return trimmed.startsWith('|') && trimmed.length > 1
}

/**
 * 表格行的语义化：拆出单元格内容，去掉对齐用的填充空白。
 *
 * 实测（真实 Chromium 中的 Vditor 4.0.0）`ir` 模式会重排 GFM 表格：
 * 单元格被补齐空白、分隔行的短横线被加长。单元格**内容**不变，
 * 渲染结果一致，因此这属于可接受的规范化而非内容损坏。
 */
function normalizeTableRow(line: string): string {
  const inner = line.trim().replace(/^\|/, '').replace(/\|$/, '')
  const cells = inner.split('|').map((cell) => cell.trim())
  if (cells.every((cell) => /^:?-+:?$/.test(cell))) {
    // 分隔行统一成 `---`，保留对齐标记。
    const markers = cells.map((cell) => {
      const left = cell.startsWith(':')
      const right = cell.endsWith(':')
      return `${left ? ':' : ''}---${right ? ':' : ''}`
    })
    return `| ${markers.join(' | ')} |`
  }
  return `| ${cells.join(' | ')} |`
}

/** 生成忽略表格填充与空行数量的语义指纹。 */
function semanticFingerprint(text: string): string {
  return collapseBlankLines(normalizeWhitespace(text))
    .split('\n')
    .map((line) => {
      if (isTableSeparator(line) || isTableRow(line)) {
        return normalizeTableRow(line)
      }
      return line
    })
    .join('\n')
}

/** 统计会构成结构性风险的特征（忽略空行数量差异）。 */
function structuralFeatures(text: string): { fences: number; tableRows: number; lines: number } {
  const lines = collapseBlankLines(normalizeWhitespace(text)).split('\n')
  return {
    fences: lines.filter((line) => /^\s*```/.test(line)).length,
    tableRows: lines.filter((line) => isTableRow(line)).length,
    lines: lines.length,
  }
}

/**
 * 检查往返结果是否安全。
 *
 * 判定规则（按严格度递进）：
 * 1. 完全一致 → 安全；
 * 2. 仅行尾空白／结尾换行不同 → 可接受规范化；
 * 3. 仅表格填充与分隔行长度不同（单元格内容一致）→ 可接受规范化；
 * 4. 其余差异（内容增减、代码围栏或表格结构变化）→ 阻止切换。
 */
export function evaluateRoundTrip(
  from: EditorMode,
  to: EditorMode,
  before: string,
  after: string,
): RoundTripReport {
  if (before === after) {
    return { from, to, before, after, normalized: false, blocked: false, reason: '内容完全一致' }
  }

  if (collapseBlankLines(normalizeWhitespace(before)) === collapseBlankLines(normalizeWhitespace(after))) {
    return {
      from,
      to,
      before,
      after,
      normalized: true,
      normalizationKind: 'whitespace',
      blocked: false,
      reason: '仅空行数量、行尾空白或结尾换行存在差异，属可接受规范化',
    }
  }

  if (semanticFingerprint(before) === semanticFingerprint(after)) {
    return {
      from,
      to,
      before,
      after,
      normalized: true,
      normalizationKind: 'table-format',
      blocked: false,
      reason:
        '表格的对齐填充被重排（单元格内容不变，渲染结果一致），属可接受规范化',
    }
  }

  // 结构性风险：代码围栏或表格行数变化说明解析器改写了原文。
  const beforeFeatures = structuralFeatures(before)
  const afterFeatures = structuralFeatures(after)
  const risks: string[] = []
  if (beforeFeatures.fences !== afterFeatures.fences) {
    risks.push('代码围栏数量变化')
  }
  if (beforeFeatures.tableRows !== afterFeatures.tableRows) {
    risks.push('表格行数量变化')
  }
  if (beforeFeatures.lines !== afterFeatures.lines) {
    risks.push('行数变化')
  }

  return {
    from,
    to,
    before,
    after,
    normalized: false,
    blocked: true,
    reason:
      risks.length > 0
        ? `模式切换会改动正文结构（${risks.join('、')}），已回退源码模式以保护原文`
        : '模式切换会改动正文内容，已回退源码模式以保护原文',
  }
}

/** 编辑器控制器。 */
export function useVditor(
  body: Ref<string>,
  mode: Ref<EditorMode>,
  options: {
    /** 字体大小等本机写作偏好（不写入文章）。 */
    fontSize?: Ref<number>
    lineHeight?: Ref<number>
    codeTheme?: Ref<string>
    /** 即时排版预览区宽度（px）。 */
    previewWidth?: Ref<number>
    /** 往返被阻止时回调，供界面显示提示。 */
    onBlocked?: (report: RoundTripReport) => void
    /** 往返发生可接受规范化时回调。 */
    onNormalized?: (report: RoundTripReport) => void
    /**
     * Vditor 构造函数注入点。
     *
     * 默认通过动态 import 加载真实 Vditor；测试可以注入替身，从而验证
     * 模式切换、往返判定与插入等逻辑，而无需依赖完整排版引擎。
     */
    loadVditor?: () => Promise<VditorConstructor>
  } = {},
) {
  const host = shallowRef<HTMLElement | null>(null)
  const instance = shallowRef<VditorInstance | null>(null)
  const lastReport = shallowRef<RoundTripReport | null>(null)
  const ready = ref(false)
  /** 是否处于中文输入法组合态。 */
  const composing = ref(false)
  /** 记录最近一次被阻止的往返报告，供界面展示差异。 */
  const blockedReport = shallowRef<RoundTripReport | null>(null)

  /** 程序化写入时抑制 watch 回写，避免光标跳动。 */
  let suppressSync = false

  function syncFromBody(next: string): void {
    const editor = instance.value
    if (!editor) return
    if (editor.getValue() === next) return
    suppressSync = true
    editor.setValue(next)
    suppressSync = false
  }

  /** 打开一篇文章时强制用新内容刷新编辑器。 */
  function load(bodyText: string): void {
    syncFromBody(bodyText)
  }

  /**
   * 在光标处插入文本片段（用于插入图片引用）。
   *
   * 走编辑器自身的插入 API，使撤销栈把这次插入当作一步，用户可 Ctrl+Z 撤回。
   */
  function insertText(snippet: string): void {
    const editor = instance.value as unknown as {
      insertValue?: (value: string) => void
    } | null
    if (editor?.insertValue) {
      editor.insertValue(snippet)
      // 插入后同步回状态，避免下一步自动保存写入旧正文。
      body.value = instance.value?.getValue() ?? body.value
      return
    }
    // 回退：追加到正文末尾，仍保留原内容。
    body.value = `${body.value}${snippet}`
    syncFromBody(body.value)
  }

  /** 应用本机写作外观（字号、行距、预览宽度）。只改本机显示，不写入文章 Markdown。 */
  function applyAppearance(): void {
    const root = host.value
    if (!root) return
    // Vditor 在自身元素上硬编码了字号（`.vditor-sv`/`.vditor-reset` 为 16px），
    // 祖先继承会被覆盖，因此用 CSS 变量 + 内联样式显式设到编辑器元素上。
    const size = options.fontSize?.value
    if (typeof size === 'number' && Number.isFinite(size)) {
      root.style.setProperty('--vditor-font-size', `${size}px`)
    }
    const ratio = options.lineHeight?.value
    if (typeof ratio === 'number' && Number.isFinite(ratio) && ratio > 0) {
      root.style.setProperty('--vditor-line-height', String(ratio / 100))
    }
    const width = options.previewWidth?.value
    if (typeof width === 'number' && Number.isFinite(width) && width > 0) {
      root.style.setProperty('--vditor-preview-width', `${width}px`)
    }
  }

  /** Vditor 初始化完成后的收尾（幂等）。 */
  let afterInitApplied = false
  function applyAfterInit(): void {
    if (afterInitApplied) return
    const created = instance.value
    if (!created) return
    afterInitApplied = true
    ready.value = true
    // 初始化期间用户可能已改过正文，这里以当前正文为准同步一次。
    suppressSync = true
    created.setValue(body.value)
    suppressSync = false
    applyAppearance()
  }

  async function createVditor(): Promise<void> {
    if (!host.value) return
    const Vditor = options.loadVditor
      ? await options.loadVditor()
      : ((await import('vditor')).default as unknown as VditorConstructor)
    // Vditor 的样式已由 main.ts 静态引入，这里不再重复 import。
    afterInitApplied = false

    const editor = new Vditor(host.value, {
      mode: mode.value === 'ir' ? 'ir' : 'sv',
      // 只用正文，front matter 由后端维护，因此不启用 value 之外的元数据处理。
      value: body.value,
      height: '100%',
      minHeight: 240,
      // 只从**本机打包资源**加载 Lute、i18n 与图标，不用 unpkg CDN。
      //
      // Vditor 默认从 `https://unpkg.com/vditor@<ver>` 注入 `<script>`；这既有
      // 离线不可用的问题，也把第三方脚本带进了拥有业务命令权限的 webview，
      // 与 CSP（`script-src 'self'`）冲突。资源随构建复制到 `public/vditor/`。
      cdn: `${import.meta.env.BASE_URL}vditor`,
      _lutePath: `${import.meta.env.BASE_URL}vditor/dist/js/lute/lute.min.js`,
      lang: 'zh_CN',
      icon: 'material',
      // 图片通过软件自己的导入流程处理（归档到 public/blog/<id>/），
      // 不使用编辑器内置的上传通道。
      upload: { url: '', handler: () => null },
      cache: { enable: false },
      counter: { enable: false },
      preview: {
        // 即时排版区域的主题，与网站接近但明确不是网站预览。
        theme: { current: 'light' },
        hljs: { style: options.codeTheme?.value ?? 'github' },
        math: { engine: 'KaTeX', inlineDigit: true },
        // 文章 Markdown 可能含内嵌 HTML，按不可信数据处理：Lute 自身会消毒
        // （`markdown.sanitize` 默认 true），这里再经 `transform` 做一层显式
        // 剥离（脚本、事件处理器、危险协议）。两层都在，不依赖单一默认值。
        transform: (html: string) => sanitizeHtml(html),
      },
      // 链接不交给编辑器默认的 window.open，先校验协议。
      link: {
        click: (element: HTMLElement) => {
          const href = (element.getAttribute?.('href') ?? '').trim()
          if (/^(https?:|mailto:|tel:)/i.test(href) || href.startsWith('/') || href.startsWith('#')) {
            window.open(href, '_blank', 'noopener,noreferrer')
          }
        },
      },
      toolbar: [
        'headings',
        'bold',
        'italic',
        'link',
        '|',
        'list',
        'ordered-list',
        'check',
        '|',
        'quote',
        'line',
        'code',
        'inline-code',
        '|',
        'upload',
        'table',
        '|',
        'undo',
        'redo',
      ],
      input: (value: string) => {
        if (suppressSync) return
        body.value = value
      },
      after: () => {
        // Vditor 可能在构造期间同步回调 `after`；此时闭包的 `editor` 仍处于
        // TDZ，而 `instance.value` 也尚未赋值。两种情况都交给 `applyAfterInit`
        // 处理：它在实例可用后（或紧随赋值后）统一完成收尾。
        applyAfterInit()
      },
    } as never)

    instance.value = editor as unknown as VditorInstance
    // 若 `after` 在构造期间已同步触发，这里补做收尾；幂等。
    applyAfterInit()
  }

  /**
   * 切换编辑模式。
   *
   * 先把当前正文写回（等待 IME composition 结束），重建编辑器后再比较往返结果；
   * 结果不安全时回退到源码模式并保留原正文。
   */
  async function switchMode(next: EditorMode): Promise<RoundTripReport | null> {
    if (next === mode.value) return null
    // 组合态期间不切换，避免把未上屏的输入算作正文。
    if (composing.value) {
      await waitForCompositionEnd()
    }

    const before = body.value
    const from = mode.value

    // 重建编辑器以应用新模式（Vditor 的模式只能在初始化时确定）。
    destroy()
    mode.value = next
    await createVditor()

    const editor = instance.value
    const after = editor ? editor.getValue() : body.value
    const report = evaluateRoundTrip(from, next, before, after)
    lastReport.value = report

    if (report.blocked) {
      // 关键：不覆盖原始 Markdown，回退源码模式。
      blockedReport.value = report
      options.onBlocked?.(report)
      destroy()
      mode.value = 'sv'
      body.value = before
      await createVditor()
      return report
    }

    if (report.normalized) {
      options.onNormalized?.(report)
    }
    // 接受规范化结果：以编辑器内容为准。
    body.value = after
    blockedReport.value = null
    return report
  }

  /** 等待输入法组合结束（最多 300ms，避免界面卡死）。 */
  function waitForCompositionEnd(): Promise<void> {
    return new Promise((resolve) => {
      const deadline = Date.now() + 300
      const tick = () => {
        if (!composing.value || Date.now() > deadline) {
          resolve()
          return
        }
        window.setTimeout(tick, 16)
      }
      tick()
    })
  }

  function destroy(): void {
    instance.value?.destroy()
    instance.value = null
    ready.value = false
  }

  // 外部内容变化（切换文章）时同步进编辑器。
  watch(body, (next) => {
    if (suppressSync) return
    syncFromBody(next)
  })

  // 写作外观偏好变化时立即应用（只影响本机显示）。
  if (options.fontSize) {
    watch(options.fontSize, applyAppearance)
  }
  if (options.lineHeight) {
    watch(options.lineHeight, applyAppearance)
  }
  if (options.previewWidth) {
    watch(options.previewWidth, applyAppearance)
  }

  // 组合态监听：IME 期间不把半成品写进正文状态。
  if (typeof window !== 'undefined') {
    window.addEventListener('compositionstart', () => {
      composing.value = true
    })
    window.addEventListener('compositionend', () => {
      composing.value = false
    })
  }

  onBeforeUnmount(() => {
    destroy()
  })

  return {
    host,
    instance,
    ready,
    composing,
    lastReport,
    blockedReport,
    createVditor,
    switchMode,
    insertText,
    /** 编辑器当前正文（未经 Vue 状态中转，用于 flush 前取值）。 */
    currentValue: () => instance.value?.getValue() ?? body.value,
    destroy,
    load,
  }
}

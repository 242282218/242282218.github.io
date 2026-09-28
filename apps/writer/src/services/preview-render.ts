/**
 * 即时预览的 Markdown → HTML 渲染（§3.3）。
 *
 * 分层与职责：
 * 1. `marked` + GFM 解析（与站点 Astro 产物对照的最小实现）；
 * 2. `sanitizeHtml` 净化（文章可能含内嵌 HTML，一律按不可信数据处理）；
 * 3. 隔离容器里应用站点正文样式的**受控快照**（构建期从仓库自己的
 *    `src/styles/global.css` 生成，见 `scripts/build-preview-css.mjs`）。
 *
 * 这里**不**在运行时读取工作区 CSS：工作区内容可变，`@import`、`url()` 与
 * 逃逸选择器会引入资源加载与样式注入风险，而 webview 具备本地命令权限。
 *
 * 即时预览是**近似**；精确的网页效果仍由现有 Astro 真服务提供。
 */
import { Marked, type Tokens } from 'marked'
import { sanitizeHtml } from '@/services/sanitize'

/**
 * 站点使用 Astro 7.3.5 默认的 Sätteri 处理器（GFM + 智能标点）。
 * `marked` 的 GFM 开关与之对齐；智能标点无法逐一复刻，属于已知差异。
 */
const marked = new Marked({
  gfm: true,
  breaks: false,
  pedantic: false,
})

/**
 * 标题锚点。
 *
 * 站点由 Astro 生成锚点，规则不能预设一致（§1.4）。这里只做**结构近似**，
 * 并把差异记为已知项，而不是假装一致。
 */
marked.use({
  renderer: {
    heading(this: unknown, token: Tokens.Heading) {
      // `this` 由 marked 注入（renderer 上下文）；用其 parser 渲染内联内容。
      const parser = (this as { parser: { parseInline: (tokens: unknown[]) => string } }).parser
      const text = parser.parseInline(token.tokens)
      return `<h${token.depth} id="${slugify(token.text)}">${text}</h${token.depth}>\n`
    },
    /**
     * 代码块：与站点一致地输出 `<pre><code class="language-x">`。
     *
     * 站点用 Shiki 上色（子节点带内联语法色），本软件不做语法高亮——
     * 这是明确的已知差异，只在验收记录里说明，不声称等同站点。
     */
    code(this: unknown, token: Tokens.Code) {
      const language = (token.lang ?? '').trim()
      const className = language ? ` class="language-${escapeAttr(language)}"` : ''
      return `<pre><code${className}>${escapeHtmlText(token.text)}</code></pre>\n`
    },
  },
})

/** 生成锚点：与常见实现的 slug 规则一致（小写、空格转连字符）。 */
export function slugify(text: string): string {
  return text
    .trim()
    .toLowerCase()
    .replace(/[\s]+/g, '-')
    .replace(/[^\p{L}\p{N}-]/gu, '')
}

function escapeHtmlText(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
}

function escapeAttr(text: string): string {
  return text.replace(/[^\w+#.-]/g, '')
}

/** 渲染结果与净化报告。 */
export type PreviewRender = {
  html: string
  /** 净化是否剥离了内容（界面据此提示差异）。 */
  sanitized: boolean
}

/**
 * 把正文 Markdown 渲染为可安全插入预览容器的 HTML。
 *
 * 净化失败时**不降级为直接注入原始 HTML**：`sanitizeHtml` 在没有 DOM 时
 * 退化为纯文本转义，宁可显示成文字也不把未净化内容插进界面。
 */
export function renderPreviewHtml(markdown: string): PreviewRender {
  const raw = marked.parse(markdown ?? '', { async: false })
  const html = sanitizeHtml(raw)
  return { html, sanitized: html !== raw }
}

/**
 * 从站点的 `src/styles/global.css` 生成正文样式的受控快照。
 *
 * 为什么需要这个脚本（开发文档 §3.3-1「来源与隔离」）：
 * - 预览样式只能来自仓库内已审阅的站点 CSS，**不能**在运行时读取 clone 的工作区：
 *   工作区内容可变，`@import`、`url()` 与逃逸选择器会带来资源加载与样式注入风险；
 * - 快照必须是「受控」的：按白名单只取文章页真正用到的规则，并**拒绝**一切
 *   `@import` 与 `url()`，保证预览完全离线、不发起任何资源请求。
 *
 * 隔离方式：快照保留站点原选择器（`:root` / `body` / `.shell` / `.article` / `.prose` …），
 * 由预览组件注入**独立文档**（`InstantPreview.vue` 的 iframe）作为隔离外壳。
 * 之所以不把选择器改写成「根元素后代」：站点结构是 `.shell > .article > .prose`，
 * 而 `.shell` / `.article` 是 `.prose` 的**祖先**，前缀式限定无法表达祖先规则，
 * 会静默丢掉外壳与头部样式。iframe 本身就是隔离边界——它有自己的 `:root`/`body`
 * 视口与媒体查询判定，也正因如此站点在 `max-width:420px` 下的 `.shell` 内边距才成立。
 * **不要把本快照注入软件主文档**，那会把站点全局规则带进软件外壳。
 *
 * 解析方式：本文件自带一个感知注释、字符串、括号的 CSS 规则切分器，按**选择器**
 * 逐条白名单过滤，不做任何字符串替换。`lightningcss` 虽然随 Tailwind 存在于
 * `node_modules/.pnpm`，但其 Node 侧只导出 `transform` / `bundle`（没有 `parse`），
 * 无法用来「按选择器挑规则」，因此不引入该依赖，保持脚本零依赖、可离线运行。
 *
 * 用法：
 *   node scripts/build-preview-css.mjs           写入 src/styles/preview-article.css
 *   node scripts/build-preview-css.mjs --stdout  只打印到标准输出（防漂移测试用）
 */
import { readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

/** 仓库根 `src/styles/global.css`：脚本位于 `<repo>/apps/writer/scripts/`。 */
const SOURCE_URL = new URL('../../../src/styles/global.css', import.meta.url)
const OUTPUT_URL = new URL('../src/styles/preview-article.css', import.meta.url)

/**
 * 纳入快照的选择器白名单（按归一化后的精确文本匹配）。
 * 与文章页 DOM（`src/pages/blog/[...id].astro`）逐条对应；新增站点规则时先在这里确认。
 */
const RETAINED_SELECTORS = new Set([
  // 全局：只保留真正影响正文的元素级规则
  ':root',
  'body',
  '*',
  '::selection',
  'a',
  'a:hover',
  'a:focus-visible',
  'img',
  // 文章页外壳与头部
  '.shell',
  '.article',
  '.article-header',
  '.article-header h1',
  '.post-meta',
  '.article-description',
  // 正文
  '.prose',
  '.prose h2',
  '.prose h3',
  '.prose p',
  '.prose a',
  '.prose ul',
  '.prose ol',
  '.prose li',
  '.prose blockquote',
  '.prose :not(pre) > code',
  '.prose pre',
  '.prose img',
  '.prose figure',
  '.prose figcaption',
  // 正文之后的返回链接
  '.back-link',
])

/** 纳入快照的媒体查询（归一化条件文本）。560px 只改站点导航，对文章页无影响，故不在其中。 */
const RETAINED_MEDIA_QUERIES = new Set([
  '(max-width: 780px)',
  '(max-width: 560px)',
  '(max-width: 420px)',
])

/**
 * 命中即视为「文章页样式」。白名单之外的这类选择器会被记进快照尾注，
 * 让站点新增正文规则时快照发生变化、防漂移测试变红，从而需要人工确认。
 */
const ARTICLE_SCOPE_PATTERN =
  /(^|[\s>+~,(])(\.prose|\.article|\.article-header|\.article-description|\.shell|\.post-meta|\.back-link|figcaption)\b/

/** 读入一个字符串字面量（含引号），返回文本与下一个扫描位置。 */
function readString(text, start) {
  const quote = text[start]
  let index = start + 1
  while (index < text.length) {
    const char = text[index]
    if (char === '\\') {
      index += 2
      continue
    }
    if (char === quote) {
      index += 1
      break
    }
    index += 1
  }
  return { value: text.slice(start, index), next: index }
}

/** 去掉注释，保留字符串字面量；注释替换为空格以免拼接出新的记号。 */
function stripComments(css) {
  let out = ''
  let index = 0
  while (index < css.length) {
    const char = css[index]
    if (char === '/' && css[index + 1] === '*') {
      const end = css.indexOf('*/', index + 2)
      index = end === -1 ? css.length : end + 2
      out += ' '
      continue
    }
    if (char === '"' || char === "'") {
      const literal = readString(css, index)
      out += literal.value
      index = literal.next
      continue
    }
    out += char
    index += 1
  }
  return out
}

/** 按分隔符切分，忽略字符串与圆括号／方括号内部的分隔符。 */
function splitTopLevel(text, separator) {
  const parts = []
  let depth = 0
  let current = ''
  let index = 0
  while (index < text.length) {
    const char = text[index]
    if (char === '"' || char === "'") {
      const literal = readString(text, index)
      current += literal.value
      index = literal.next
      continue
    }
    if (char === '(' || char === '[') depth += 1
    else if (char === ')' || char === ']') depth = Math.max(0, depth - 1)
    if (char === separator && depth === 0) {
      parts.push(current)
      current = ''
      index += 1
      continue
    }
    current += char
    index += 1
  }
  parts.push(current)
  return parts
}

/** 定位顶层分隔符位置，找不到返回 -1。 */
function topLevelIndexOf(text, needle) {
  let depth = 0
  let index = 0
  while (index < text.length) {
    const char = text[index]
    if (char === '"' || char === "'") {
      index = readString(text, index).next
      continue
    }
    if (char === '(' || char === '[') depth += 1
    else if (char === ')' || char === ']') depth = Math.max(0, depth - 1)
    else if (char === needle && depth === 0) return index
    index += 1
  }
  return -1
}

/**
 * 把样式表切成顶层节点：`{ prelude, body }`。
 * `body` 为 `null` 表示没有块的语句（`@import` / `@charset`），调用方自行决定是否保留。
 */
function parseStylesheet(css) {
  const nodes = []
  let prelude = ''
  let bodyStart = -1
  let depth = 0
  let index = 0
  while (index < css.length) {
    const char = css[index]
    if (char === '"' || char === "'") {
      const literal = readString(css, index)
      if (depth === 0) prelude += literal.value
      index = literal.next
      continue
    }
    if (char === '{') {
      depth += 1
      if (depth === 1) bodyStart = index + 1
      index += 1
      continue
    }
    if (char === '}') {
      depth -= 1
      if (depth === 0) {
        nodes.push({ prelude: prelude.trim(), body: css.slice(bodyStart, index) })
        prelude = ''
      }
      index += 1
      continue
    }
    if (char === ';' && depth === 0) {
      // Top-level statement such as `@import` / `@charset`: terminates the prelude.
      const statement = prelude.trim()
      if (statement) nodes.push({ prelude: statement, body: null })
      prelude = ''
      index += 1
      continue
    }
    if (depth === 0) prelude += char
    index += 1
  }
  const tail = prelude.trim()
  if (tail) nodes.push({ prelude: tail, body: null })
  return nodes
}

/** 归一化选择器列表。 */
function parseSelectorList(prelude) {
  return splitTopLevel(prelude, ',')
    .map((selector) => selector.replace(/\s+/g, ' ').trim())
    .filter(Boolean)
}

/** 归一化声明块，统一成一行一条、单空格分隔，保证输出与源文件排版无关。 */
function parseDeclarations(body) {
  const declarations = []
  for (const part of splitTopLevel(body, ';')) {
    const text = part.trim()
    if (!text) continue
    const colon = topLevelIndexOf(text, ':')
    if (colon === -1) continue
    const property = text.slice(0, colon).trim()
    const value = text.slice(colon + 1).replace(/\s+/g, ' ').trim()
    if (!property || !value) continue
    declarations.push(`${property}: ${value};`)
  }
  return declarations
}

/** 归一化 `@media` 的条件文本。 */
function normalizeMediaQuery(prelude) {
  return prelude.slice('@media'.length).replace(/\s+/g, ' ').trim()
}

function formatRule(selectors, declarations, indent) {
  const pad = ' '.repeat(indent)
  const lines = [`${pad}${selectors.join(', ')} {`]
  for (const declaration of declarations) lines.push(`${pad}  ${declaration}`)
  lines.push(`${pad}}`)
  return lines.join('\n')
}

/**
 * 生成快照。
 * @param {string} sourceCss 站点 `src/styles/global.css` 的当前内容。
 * @returns {{ css: string, unmatched: string[], missing: string[] }}
 */
function generateSnapshot(sourceCss) {
  const nodes = parseStylesheet(stripComments(sourceCss))
  const blocks = []
  const unmatched = []
  const found = new Set()

  const renderRule = (node, mediaLabel, indent) => {
    const retained = []
    for (const selector of parseSelectorList(node.prelude)) {
      if (RETAINED_SELECTORS.has(selector)) {
        retained.push(selector)
        found.add(selector)
      } else if (ARTICLE_SCOPE_PATTERN.test(selector)) {
        unmatched.push(mediaLabel ? `@media ${mediaLabel} → ${selector}` : selector)
      }
    }
    if (retained.length === 0) return null
    const declarations = parseDeclarations(node.body)
    if (declarations.length === 0) return null
    return formatRule(retained, declarations, indent)
  }

  for (const node of nodes) {
    // `@import` / `@charset` 等语句一律不复制：软件不打包 webfont，预览保持离线。
    if (node.body === null) continue
    if (!node.prelude.startsWith('@')) {
      const rule = renderRule(node, null, 0)
      if (rule) blocks.push(rule)
      continue
    }
    const atRule = node.prelude.split(/\s|\(/)[0]
    // 只保留媒体查询；`@font-face` / `@supports` 等一律丢弃，避免任何外部资源引用。
    if (atRule !== '@media') continue
    const query = normalizeMediaQuery(node.prelude)
    const keepQuery = RETAINED_MEDIA_QUERIES.has(query)
    const inner = []
    for (const child of parseStylesheet(node.body)) {
      if (child.body === null || child.prelude.startsWith('@')) continue
      const rule = renderRule(child, query, keepQuery ? 2 : 0)
      if (keepQuery && rule) inner.push(rule)
    }
    if (inner.length === 0) continue
    blocks.push(`@media ${query} {\n${inner.join('\n')}\n}`)
  }

  const missing = [...RETAINED_SELECTORS].filter((selector) => !found.has(selector))
  const notes = []
  if (unmatched.length > 0) {
    notes.push(
      '以下站点规则带有文章页选择器但未纳入快照，站点改动需先确认再更新白名单：',
      ...unmatched.map((entry) => ` * - ${entry}`),
    )
  }
  if (missing.length > 0) {
    notes.push(
      '以下白名单选择器在当前源文件中没有出现（可能被改名或删除），需人工复核：',
      ...missing.map((selector) => ` * - ${selector}`),
    )
  }

  const header = [
    '/*',
    ' * 站点正文样式的受控快照：由脚本生成，请勿手工修改。',
    ' *',
    ' * 来源：src/styles/global.css',
    ' * 生成：pnpm --dir apps/writer build:preview-css',
    ' * 作用域：保留站点原选择器（:root / body / .shell / .article / .prose …），由预览组件',
    ' *         注入**独立文档**（InstantPreview.vue 的 iframe）作为隔离外壳。',
    ' * 请勿把本文件注入软件主文档：那会把站点的 :root / body / a / * 规则带进软件外壳。',
    ' * 未复制站点的 @import（Google Fonts）：本软件不打包 webfont，预览保持离线，',
    ' *         字体走系统回退栈，字形与换行可能与线上不同。',
    ' */',
  ]

  const sections = [header.join('\n'), ...blocks]
  if (notes.length > 0) sections.push(['/*', ...notes, ' */'].join('\n'))
  return { css: `${sections.join('\n\n')}\n`, unmatched, missing }
}

function main() {
  const sourcePath = fileURLToPath(SOURCE_URL)
  const outputPath = fileURLToPath(OUTPUT_URL)
  let sourceCss
  try {
    sourceCss = readFileSync(sourcePath, 'utf8')
  } catch (error) {
    console.error(`[预览样式快照] 读取站点样式失败：${sourcePath}`)
    console.error(error instanceof Error ? error.message : String(error))
    process.exitCode = 1
    return
  }

  const { css, unmatched, missing } = generateSnapshot(sourceCss)

  if (process.argv.includes('--stdout')) {
    process.stdout.write(css)
    return
  }

  writeFileSync(outputPath, css, 'utf8')
  console.log(`[预览样式快照] 已写入 ${outputPath}`)
  if (unmatched.length > 0) {
    console.log(`[预览样式快照] 未纳入的文章相关规则 ${unmatched.length} 条：`)
    for (const entry of unmatched) console.log(`  - ${entry}`)
  }
  if (missing.length > 0) {
    console.log(`[预览样式快照] 白名单未命中选择器 ${missing.length} 条：`)
    for (const selector of missing) console.log(`  - ${selector}`)
  }
}

// Only run as a CLI: importing this module (e.g. to reuse generateSnapshot) must not write files.
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main()
}

export { generateSnapshot }

/**
 * 预览内容消毒。
 *
 * 文章的 Markdown 可能包含内嵌 HTML；这类内容按**不可信数据**处理：
 * 只保留安全标签与安全属性，移除脚本、事件处理器与危险 URL 协议。
 *
 * 网页渲染只发生在软件自己的界面上，绝不把第三方脚本带进拥有本地命令权限的
 * webview；网站预览则使用独立窗口访问仅绑定 127.0.0.1 的本地服务。
 */

/** 允许保留的标签（与博客正文可能出现的结构一致）。 */
const ALLOWED_TAGS = new Set([
  'a', 'p', 'br', 'hr', 'strong', 'em', 'b', 'i', 'u', 's', 'del', 'ins', 'mark', 'small', 'sub', 'sup',
  'h1', 'h2', 'h3', 'h4', 'h5', 'h6',
  'ul', 'ol', 'li', 'dl', 'dt', 'dd',
  'blockquote', 'pre', 'code', 'kbd', 'samp',
  'table', 'thead', 'tbody', 'tfoot', 'tr', 'th', 'td', 'caption',
  'img', 'figure', 'figcaption',
  'div', 'span', 'section', 'article', 'aside', 'header', 'footer', 'main', 'nav',
])

/** 允许保留的属性。 */
const ALLOWED_ATTRS = new Set([
  'href', 'title', 'alt', 'src', 'width', 'height', 'class', 'id', 'lang', 'dir',
  'colspan', 'rowspan', 'scope', 'start', 'reversed', 'value', 'type',
  'align', 'loading', 'decoding', 'srcset', 'sizes',
])

/** 链接协议白名单。 */
const SAFE_URL_SCHEMES = new Set(['http:', 'https:', 'mailto:', 'tel:'])

/** 图片协议白名单（含站点内相对路径与本地预览地址）。 */
const SAFE_IMAGE_SCHEMES = new Set(['http:', 'https:', 'data:'])

/** 直接丢弃整个子树的标签。 */
const DROP_SUBTREE = new Set([
  'script', 'style', 'iframe', 'object', 'embed', 'form', 'input', 'button',
  'textarea', 'select', 'option', 'link', 'meta', 'base', 'template', 'svg', 'math',
])

function isSafeUrl(value: string, forImage: boolean): boolean {
  const trimmed = value.trim()
  // 去掉可能用于绕过检测的控制字符。
  const compact = trimmed.replace(/[\u0000-\u001f\u007f]/g, '')
  // 站点内相对路径与锚点。
  if (compact.startsWith('/') || compact.startsWith('./') || compact.startsWith('../')) {
    return true
  }
  if (compact.startsWith('#')) return true
  const schemeMatch = /^([a-zA-Z][a-zA-Z0-9+.-]*):/.exec(compact)
  if (!schemeMatch) return true // 无协议的相对路径。
  const scheme = `${schemeMatch[1]?.toLowerCase() ?? ''}:`
  return forImage ? SAFE_IMAGE_SCHEMES.has(scheme) : SAFE_URL_SCHEMES.has(scheme)
}

/**
 * 消毒一段 HTML。
 *
 * 在浏览器环境中使用 `DOMParser`（不执行脚本）；不可用时退化为纯文本转义，
 * 绝不把原始 HTML 直接插入界面。
 */
export function sanitizeHtml(html: string): string {
  if (typeof window === 'undefined' || typeof window.DOMParser === 'undefined') {
    return escapeHtml(html)
  }

  const doc = new window.DOMParser().parseFromString(html, 'text/html')
  const body = doc.body
  if (!body) return ''

  // 先移除危险子树，再逐个检查其余元素。
  for (const tag of DROP_SUBTREE) {
    for (const node of Array.from(body.querySelectorAll(tag))) {
      node.remove()
    }
  }

  const elements = Array.from(body.querySelectorAll('*'))
  for (const element of elements) {
    const tagName = element.tagName.toLowerCase()
    if (!ALLOWED_TAGS.has(tagName)) {
      // 未在白名单中的标签：保留文字内容，去掉标签本身。
      const text = doc.createTextNode(element.textContent ?? '')
      element.replaceWith(text)
      continue
    }

    for (const attr of Array.from(element.attributes)) {
      const name = attr.name.toLowerCase()
      // 事件处理器一律移除。
      if (name.startsWith('on')) {
        element.removeAttribute(attr.name)
        continue
      }
      if (!ALLOWED_ATTRS.has(name)) {
        element.removeAttribute(attr.name)
        continue
      }
      if ((name === 'href' || name === 'src' || name === 'srcset') && !isSafeUrl(attr.value, name !== 'href')) {
        element.removeAttribute(attr.name)
      }
    }

    // 外链在独立窗口打开，且不传递引用来源。
    if (tagName === 'a') {
      const href = element.getAttribute('href') ?? ''
      if (/^https?:/i.test(href.trim())) {
        element.setAttribute('rel', 'noopener noreferrer')
        element.setAttribute('target', '_blank')
      }
    }
    // 图片不做跨站追踪，且延迟加载。
    if (tagName === 'img') {
      element.setAttribute('loading', 'lazy')
      element.setAttribute('decoding', 'async')
    }
  }

  return body.innerHTML
}

/** 纯文本转义（无 DOM 环境时的兜底）。 */
export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;')
}

/** 判断消毒后是否发生了实质剥离（供界面提示）。 */
export function sanitizeWithReport(html: string): { html: string; removedSomething: boolean } {
  const cleaned = sanitizeHtml(html)
  return { html: cleaned, removedSomething: cleaned !== html }
}

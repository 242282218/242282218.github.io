/**
 * 预览消毒测试。
 *
 * 文章的 Markdown 可能含内嵌 HTML，这些内容按不可信数据处理：
 * 脚本、事件处理器与危险协议必须被剥离，正文文字必须保留。
 */
import { describe, expect, it } from 'vitest'
import { escapeHtml, sanitizeHtml, sanitizeWithReport } from '@/services/sanitize'

describe('sanitizeHtml：剥离可执行内容', () => {
  it('移除 script 标签及其内容', () => {
    const result = sanitizeHtml('<p>正文</p><script>alert(1)</script>')
    expect(result).toContain('正文')
    expect(result).not.toContain('script')
    expect(result).not.toContain('alert')
  })

  it('移除事件处理器属性', () => {
    const result = sanitizeHtml('<p onclick="alert(1)" onmouseover="x()">正文</p>')
    expect(result).toContain('正文')
    expect(result).not.toContain('onclick')
    expect(result).not.toContain('onmouseover')
  })

  it('移除 iframe 与 embed', () => {
    const result = sanitizeHtml('<iframe src="https://evil.invalid/"></iframe><embed src="x">')
    expect(result).not.toContain('iframe')
    expect(result).not.toContain('embed')
  })

  it('移除 javascript: 链接', () => {
    const result = sanitizeHtml('<a href="javascript:alert(1)">点我</a>')
    expect(result).toContain('点我')
    expect(result).not.toContain('javascript:')
  })

  it('移除被控制字符绕过的 javascript: 链接', () => {
    const result = sanitizeHtml('<a href="java\tscript:alert(1)">点我</a>')
    expect(result).not.toContain('script:')
  })

  it('移除 data: 链接（图片以外）', () => {
    const result = sanitizeHtml('<a href="data:text/html,<script>alert(1)</script>">x</a>')
    expect(result).not.toContain('data:text/html')
  })

  it('移除 style 标签与 svg 子树', () => {
    const result = sanitizeHtml('<style>body{display:none}</style><svg><script>x</script></svg><p>正文</p>')
    expect(result).toContain('正文')
    expect(result).not.toContain('display:none')
    expect(result).not.toContain('<svg')
  })

  it('移除表单控件', () => {
    const result = sanitizeHtml('<form action="/x"><input name="a"><button>提交</button></form>')
    expect(result).not.toContain('<form')
    expect(result).not.toContain('<input')
    expect(result).not.toContain('<button')
  })
})

describe('sanitizeHtml：保留正常内容', () => {
  it('保留标题、段落、强调与代码', () => {
    const html = '<h2>小标题</h2><p>正文<strong>加粗</strong><code>code</code></p>'
    expect(sanitizeHtml(html)).toBe(html)
  })

  it('保留表格结构', () => {
    const html = '<table><thead><tr><th>步骤</th></tr></thead><tbody><tr><td>一</td></tr></tbody></table>'
    expect(sanitizeHtml(html)).toBe(html)
  })

  it('保留站点内根路径图片与链接', () => {
    const html = '<img src="/blog/read-code/figure-01.png" alt="图"><a href="/blog/other/">另一篇</a>'
    const result = sanitizeHtml(html)
    expect(result).toContain('/blog/read-code/figure-01.png')
    expect(result).toContain('/blog/other/')
  })

  it('保留相对路径图片（编辑器常见写法）', () => {
    const result = sanitizeHtml('<img src="./figure-02.png" alt="图">')
    expect(result).toContain('./figure-02.png')
  })

  it('保留中文与 emoji', () => {
    const result = sanitizeHtml('<p>「中文引号」、全角（括号）……🎯</p>')
    expect(result).toContain('「中文引号」')
    expect(result).toContain('🎯')
  })

  it('为图片补上延迟加载属性', () => {
    const result = sanitizeHtml('<img src="/blog/a/f.png" alt="x">')
    expect(result).toContain('loading="lazy"')
  })

  it('为外链加上 noopener noreferrer', () => {
    const result = sanitizeHtml('<a href="https://example.invalid/">外链</a>')
    expect(result).toContain('rel="noopener noreferrer"')
  })
})

describe('sanitizeWithReport', () => {
  it('报告是否发生了剥离', () => {
    const clean = sanitizeWithReport('<p>正文</p>')
    expect(clean.removedSomething).toBe(false)

    const stripped = sanitizeWithReport('<p>正文</p><script>bad()</script>')
    expect(stripped.removedSomething).toBe(true)
    expect(stripped.html).toContain('正文')
  })
})

describe('escapeHtml 兜底', () => {
  it('转义全部危险字符', () => {
    const result = escapeHtml('<p>"a" & \'b\'</p>')
    expect(result).toBe('&lt;p&gt;&quot;a&quot; &amp; &#39;b&#39;&lt;/p&gt;')
  })
})

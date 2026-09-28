/**
 * 预览样式快照的防漂移测试（开发文档 §3.3-2）。
 *
 * 快照 `src/styles/preview-article.css` 由 `scripts/build-preview-css.mjs` 从
 * 仓库的 `src/styles/global.css` 生成。本测试**重新运行同一个生成器**，逐行比对
 * 提交进仓库的快照；站点正文样式变化后若没有重新生成，测试即失败。
 *
 * 注意：只有「实际带进预览的那部分规则」参与比对。站点改动 `.site-nav` 之类与
 * 文章页无关的规则时，快照不变，测试保持绿色；这与「快照未漂移」是同一件事。
 */
import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

/**
 * 用 `node:path` 拼路径而不是 `new URL(相对路径, import.meta.url)`：
 * Vite 会把后者当成资源引用并在转换阶段改写，跨出工程根时会产出非 file 协议的 URL。
 */
const TESTS_DIR = dirname(fileURLToPath(import.meta.url))
/** 站点样式源：`<repo>/src/styles/global.css`（本文件在 `<repo>/apps/writer/src/tests/`）。 */
const SITE_STYLES_PATH = resolve(TESTS_DIR, '../../../../src/styles/global.css')
const GENERATOR_PATH = resolve(TESTS_DIR, '../../scripts/build-preview-css.mjs')
const SNAPSHOT_PATH = resolve(TESTS_DIR, '../styles/preview-article.css')

const REGENERATE_HINT = [
  '预览样式快照已与站点 src/styles/global.css 不一致。',
  '请运行 `pnpm --dir apps/writer build:preview-css` 重新生成 apps/writer/src/styles/preview-article.css，',
  '并复核差异是否只包含有意带入预览的正文样式变化（源文件位置：src/styles/global.css）。',
].join('\n')

/** 统一行尾：仓库 `core.autocrlf` 为 true 时工作区可能是 CRLF，与生成器输出无关。 */
function readText(path: string): string {
  return readFileSync(path, 'utf8').replace(/\r\n/g, '\n')
}

/** 重新生成快照（走 CLI，确保测试与开发者手动执行的路径完全一致）。 */
function regenerate(): string {
  return execFileSync(process.execPath, [GENERATOR_PATH, '--stdout'], {
    encoding: 'utf8',
    maxBuffer: 4 * 1024 * 1024,
  }).replace(/\r\n/g, '\n')
}

/** 给出首个差异行，避免只看巨大的字符 diff。 */
function describeFirstDifference(actual: string, expected: string): string {
  const actualLines = actual.split('\n')
  const expectedLines = expected.split('\n')
  const count = Math.max(actualLines.length, expectedLines.length)
  for (let index = 0; index < count; index += 1) {
    if (actualLines[index] !== expectedLines[index]) {
      return [
        `首个差异在第 ${index + 1} 行：`,
        `  已提交快照：${actualLines[index] ?? '<缺少该行>'}`,
        `  重新生成：  ${expectedLines[index] ?? '<缺少该行>'}`,
      ].join('\n')
    }
  }
  return '逐行内容相同，差异只在文件末尾。'
}

/** 取出顶层某条规则的声明部分，用于断言具体排版参数。 */
function declarationsOf(css: string, selector: string): string {
  const start = css.indexOf(`\n${selector} {\n`)
  if (start === -1) throw new Error(`快照中找不到顶层规则：${selector}`)
  const end = css.indexOf('\n}', start)
  return css.slice(start, end === -1 ? css.length : end)
}

const snapshot = readText(SNAPSHOT_PATH)

describe('预览样式快照：与站点源文件同步', () => {
  it('当前快照等于按 src/styles/global.css 重新生成的结果', () => {
    const regenerated = regenerate()
    expect(
      snapshot,
      `${describeFirstDifference(snapshot, regenerated)}\n\n${REGENERATE_HINT}`,
    ).toBe(regenerated)
  })

  it('站点源文件存在且可读（比对对象不是空文件）', () => {
    const source = readText(SITE_STYLES_PATH)
    expect(source).toContain('.prose')
    expect(source).toContain('--paper')
  })
})

describe('预览样式快照：内容必须非空且含关键排版参数', () => {
  it('规模足够，不是被掏空的快照', () => {
    expect(snapshot.length).toBeGreaterThan(1200)
    const ruleCount = (snapshot.match(/\{\n/g) ?? []).length
    expect(ruleCount, '快照规则数量过少，疑似抽取失败').toBeGreaterThanOrEqual(25)
  })

  it('含站点根变量（:root 原样保留，供隔离预览文档使用）', () => {
    const root = declarationsOf(snapshot, ':root')
    for (const variable of [
      '--paper: #f8fafb;',
      '--ink: #1f2c3b;',
      '--blue: #286697;',
      '--blue-deep: #1d557f;',
      '--muted: #52697a;',
      '--line: #dce5eb;',
      '--surface: #eff4f7;',
    ]) {
      expect(root).toContain(variable)
    }
    // `:root` 必须存在：预览是独立 iframe 文档，变量注入那里的 :root 才会生效。
    expect(snapshot).toMatch(/^:root \{$/m)
  })

  it('含 .prose 基准排版：16px / 2.05 行高 / 段落 21px 下边距', () => {
    const prose = declarationsOf(snapshot, '.prose')
    expect(prose).toContain('font-size: 16px;')
    expect(prose).toContain('line-height: 2.05;')
    expect(declarationsOf(snapshot, '.prose p')).toContain('margin: 0 0 21px;')
  })

  it('含 .shell / .article / .article-header 等祖先规则（前缀式限定无法表达）', () => {
    expect(declarationsOf(snapshot, '.shell')).toContain('width: min(100% - 64px, 1000px);')
    expect(declarationsOf(snapshot, '.article')).toContain('max-width: 760px;')
    expect(declarationsOf(snapshot, '.article-header')).toContain(
      'border-bottom: 1px solid var(--line);',
    )
    expect(declarationsOf(snapshot, '.article-description')).toContain('font-size: 16px;')
  })

  it('含文章正文关键规则', () => {
    expect(declarationsOf(snapshot, '.prose pre')).toContain('background: #e9f1f6 !important;')
    expect(declarationsOf(snapshot, '.prose blockquote')).toContain(
      'border-left: 3px solid var(--blue);',
    )
    expect(declarationsOf(snapshot, '.prose :not(pre) > code')).toContain(
      'background: #e9f1f6;',
    )
    expect(declarationsOf(snapshot, '.prose figcaption')).toContain('text-align: center;')
  })

  it('含桌面与手机宽度的媒体查询', () => {
    expect(snapshot).toContain('@media (max-width: 780px) {')
    expect(snapshot).toContain('@media (max-width: 420px) {')
    expect(snapshot).toContain('width: calc(100% - 32px);')
  })

  it('保持离线与隔离：不含 @import、@font-face 或 url()', () => {
    // §3.3-1：快照不得引入资源加载，预览必须完全离线。
    // 先去掉注释，只检查真正的规则体（文件头说明里提到了 @import 这个词）。
    const rules = snapshot.replace(/\/\*[\s\S]*?\*\//g, '')
    expect(rules).not.toMatch(/@import/)
    expect(rules).not.toMatch(/@font-face/)
    expect(rules).not.toMatch(/url\s*\(/)
    expect(rules).not.toMatch(/fonts\.googleapis/)
  })
})

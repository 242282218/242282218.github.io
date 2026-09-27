/**
 * 往返判定规则的单元测试。
 *
 * 说明：Vditor 依赖真实排版引擎，在 jsdom 中无法初始化（`getValue` 会因
 * `currentMode` 未就绪而抛错）。因此：
 * - **判定规则**在这里做单元测试（本文件）；
 * - **真实 Chromium 中的 sv↔ir 往返**由 `harness/vditor-roundtrip.html` 验收，
 *   该夹具直接 import 本仓库的判定实现，保证规则只有一份。
 */
import { describe, expect, it } from 'vitest'
import { evaluateRoundTrip } from '@/components/MarkdownEditor'

describe('evaluateRoundTrip：完全一致', () => {
  it('相同文本既不规范化也不阻止', () => {
    const report = evaluateRoundTrip('sv', 'ir', '# 标题\n正文\n', '# 标题\n正文\n')
    expect(report.blocked).toBe(false)
    expect(report.normalized).toBe(false)
    expect(report.reason).toContain('完全一致')
  })
})

describe('evaluateRoundTrip：可接受的空白规范化', () => {
  it('行尾空白差异不被视为损坏', () => {
    const report = evaluateRoundTrip('sv', 'ir', '# 标题  \n正文  \n', '# 标题\n正文\n')
    expect(report.blocked).toBe(false)
    expect(report.normalized).toBe(true)
    expect(report.normalizationKind).toBe('whitespace')
  })

  it('CRLF 与 LF 的差异不被视为损坏', () => {
    const report = evaluateRoundTrip('sv', 'ir', '# 标题\r\n正文\r\n', '# 标题\n正文\n')
    expect(report.blocked).toBe(false)
  })

  it('结尾空行差异不被视为损坏', () => {
    const report = evaluateRoundTrip('sv', 'ir', '正文\n\n\n', '正文\n')
    expect(report.blocked).toBe(false)
  })

  it('块之间多余空行被折叠，属可接受规范化', () => {
    // 实测：ir 模式会在代码块与后续表格之间补一个空行。
    const before = '```ts\nconst a = 1\n```\n\n| a | b |\n| - | - |\n'
    const after = '```ts\nconst a = 1\n```\n\n\n| a | b |\n| - | - |\n'
    const report = evaluateRoundTrip('sv', 'ir', before, after)
    expect(report.blocked, report.reason).toBe(false)
    expect(report.normalized).toBe(true)
    expect(report.normalizationKind).toBe('whitespace')
  })
})

describe('evaluateRoundTrip：表格填充重排被视为安全规范化', () => {
  it('实测的 ir 表格补空行为不被视为损坏', () => {
    // 这段 after 来自真实 Chromium 中 Vditor 4.0.0 的 ir 模式输出。
    const before = '| 步骤 | 说明 |\n| --- | --- |\n| 一 | 找输入与输出 |\n| 二 | 试一个最小输入 |\n'
    const after =
      '| 步骤 | 说明           |\n| ---- | -------------- |\n| 一   | 找输入与输出   |\n| 二   | 试一个最小输入 |\n'
    const report = evaluateRoundTrip('sv', 'ir', before, after)
    expect(report.blocked, report.reason).toBe(false)
    expect(report.normalized).toBe(true)
    expect(report.normalizationKind).toBe('table-format')
    expect(report.reason).toContain('渲染结果一致')
  })

  it('表格分隔行的对齐标记被保留', () => {
    const before = '| a | b | c |\n| :-- | :-: | --: |\n| 1 | 2 | 3 |\n'
    const after = '| a   |  b  |   c |\n| :-- | :-: | --: |\n| 1   |  2  |   3 |\n'
    const report = evaluateRoundTrip('sv', 'ir', before, after)
    expect(report.blocked, report.reason).toBe(false)
    expect(report.normalizationKind).toBe('table-format')
  })

  it('单元格内容变化不被当作表格填充', () => {
    const before = '| a | b |\n| - | - |\n| 1 | 2 |\n'
    const after = '| a | b |\n| - | - |\n| 1 | 3 |\n'
    const report = evaluateRoundTrip('sv', 'ir', before, after)
    expect(report.blocked).toBe(true)
    expect(report.normalized).toBe(false)
  })
})

describe('evaluateRoundTrip：阻止会造成损坏的切换', () => {
  it('正文缺失会阻止切换并提示回退源码模式', () => {
    const report = evaluateRoundTrip('sv', 'ir', '# 标题\n正文\n', '# 标题\n')
    expect(report.blocked).toBe(true)
    expect(report.reason).toContain('回退源码模式')
  })

  it('代码围栏数量变化被单独识别', () => {
    const before = '```ts\nconst a = 1\n```\n'
    const after = 'const a = 1\n'
    const report = evaluateRoundTrip('sv', 'ir', before, after)
    expect(report.blocked).toBe(true)
    expect(report.reason).toContain('代码围栏')
  })

  it('表格行数量变化被单独识别', () => {
    const before = '| a | b |\n| - | - |\n| 1 | 2 |\n'
    const after = '| a | b |\n| - | - |\n'
    const report = evaluateRoundTrip('sv', 'ir', before, after)
    expect(report.blocked).toBe(true)
    expect(report.reason).toContain('表格行')
  })

  it('正文被改写为渲染后的 HTML 会阻止切换', () => {
    const before = '中文段落\n'
    const after = '<p>中文段落</p>\n'
    const report = evaluateRoundTrip('sv', 'ir', before, after)
    expect(report.blocked).toBe(true)
  })

  it('阻止切换时保留往返前后原文，供界面展示差异', () => {
    const report = evaluateRoundTrip('sv', 'ir', '原文\n', '改过\n')
    expect(report.before).toBe('原文\n')
    expect(report.after).toBe('改过\n')
  })
})

describe('真实样稿的往返安全性（文本级判定）', () => {
  /** 站点现有示例文章的正文节选，用于确认真实内容不触发阻止。 */
  const realBodies = [
    '读到一段陌生代码时，第一遍没有看懂很正常。比起从第一行逐字读到最后一行，可以先找它的边界：谁调用它？它接收什么？\n\n## 先用自己的话复述\n\n例如，看到一个按日期排列条目的函数，可以先把目标写成一句话：*从全部条目中取出可公开的内容，并按日期从新到旧排列。*\n',
    '有些尝试以“暂时还不知道”结束。只要过程被写清楚，它依然是一份有用的记录。\n\n```text\n预期：命令结束后生成一个文件\n实际：这次运行没有生成；再次运行时生成了\n```\n\n这里的内容只是演示写法，并非真实日志。\n',
    '| 步骤 | 说明 |\n| --- | --- |\n| 一 | 找输入与输出 |\n',
  ]

  it.each(realBodies.map((body, index) => [index, body]))(
    '样稿 %i 的自比对不会被误判为损坏',
    (_index, body) => {
      const report = evaluateRoundTrip('sv', 'ir', body as string, body as string)
      expect(report.blocked).toBe(false)
    },
  )

  it('中文标点与 emoji 在完全相同的内容下不被误判', () => {
    const body = '这是「中文引号」、全角（括号）与省略号……还有 emoji 🎯。\n'
    const report = evaluateRoundTrip('ir', 'sv', body, body)
    expect(report.blocked).toBe(false)
  })
})

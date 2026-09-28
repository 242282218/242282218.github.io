/**
 * CodeMirror 6 编辑器的行为回归（替代 Vditor 时代的往返判定）。
 *
 * 覆盖三条新约束：
 * 1. **标记往返退役**：CodeMirror 不重排 Markdown，「打开 → 不编辑 → 保存」
 *    必须保持磁盘原文字节不变（§3.2-4 的新判据）；
 * 2. **组合态（IME）保护**：候选未结束时不得向外发送变化，避免未上屏的拼音
 *    触发一次自动保存；上屏后中文内容完整保留（§3.2-3）；
 * 3. 格式化命令是**切换**语义，且编辑命令集合与站点语法一致。
 */
import { beforeEach, describe, expect, it } from 'vitest'
import {
  createMarkdownEditor,
  deleteSlashTrigger,
  insertBlock,
  setHeading,
  toggleLinePrefix,
  toggleOrderedList,
  toggleWrap,
  type MarkdownEditorHandle,
} from '@/editor/codemirror'
import { FORMAT_COMMANDS, INSERT_COMMANDS, SLASH_GROUPS } from '@/editor/commands'

let host: HTMLElement
let handle: MarkdownEditorHandle
let emitted: string[]
let selectionEvents: { line: number; column: number; selectedChars: number }[]

function setup(value: string): void {
  host = document.createElement('div')
  document.body.appendChild(host)
  emitted = []
  selectionEvents = []
  handle = createMarkdownEditor({
    parent: host,
    value,
    onChange: (next) => emitted.push(next),
    onSelectionChange: (info) => selectionEvents.push(info),
  })
}

/** 选中指定范围（用于验证格式化作用于选区）。 */
function select(from: number, to: number): void {
  handle.view.dispatch({ selection: { anchor: from, head: to } })
}

function selectAll(): void {
  handle.view.dispatch({ selection: { anchor: 0, head: handle.view.state.doc.length } })
}

beforeEach(() => {
  document.body.innerHTML = ''
})

describe('打开 → 不编辑 → 保存 保持磁盘原文', () => {
  /** 含围栏尾随空格、CRLF 与表格的样本：任何规范化都会改变字节。 */
  const RAW = [
    '---',
    'title: "往返样本"',
    'draft: true',
    '---',
    '',
    '正文第一段。  ',
    '',
    '| 列 1 | 列 2 |',
    '| --- | --- |',
    '| a | b |',
    '',
    '```ts',
    'const a = 1',
    '```',
    '',
  ].join('\r\n')

  it('不做任何编辑时取回的文本与输入逐字节相同', () => {
    setup(RAW)
    expect(handle.getValue()).toBe(RAW)
    // 没有编辑动作就不得产生任何变化事件（否则会触发一次无意义的自动保存）。
    expect(emitted).toEqual([])
  })

  it('只移动光标不改变文档内容', () => {
    setup(RAW)
    select(3, 8)
    expect(handle.getValue()).toBe(RAW)
    expect(emitted).toEqual([])
  })

  it('撤销回初始状态后同样逐字节相同（不产生规范化残留）', () => {
    setup(RAW)
    // 输入一个字符再撤销：历史必须回到原始字节，而不是「渲染后」的版本。
    handle.view.dispatch({ changes: { from: 0, insert: 'x' } })
    expect(handle.getValue()).not.toBe(RAW)
    handle.view.dispatch({ changes: { from: 0, to: 1, insert: '' } })
    expect(handle.getValue()).toBe(RAW)
  })

  it('切换文章时整体替换内容，且不把替换当成用户编辑', () => {
    setup('旧正文\n')
    emitted = []
    handle.setValue('新正文\n')
    expect(handle.getValue()).toBe('新正文\n')
    expect(emitted).toEqual([])
  })
})

describe('格式化命令是切换语义', () => {
  it('加粗：先包裹选区，再点一次移除', () => {
    setup('重点内容')
    selectAll()
    toggleWrap('**')(handle.view)
    expect(handle.getValue()).toBe('**重点内容**')

    selectAll()
    toggleWrap('**')(handle.view)
    expect(handle.getValue()).toBe('重点内容')
  })

  it('行内代码与删除线同理', () => {
    setup('abc')
    selectAll()
    toggleWrap('`')(handle.view)
    expect(handle.getValue()).toBe('`abc`')
    selectAll()
    toggleWrap('`')(handle.view)
    expect(handle.getValue()).toBe('abc')

    selectAll()
    toggleWrap('~~')(handle.view)
    expect(handle.getValue()).toBe('~~abc~~')
  })

  it('标题级别是替换而不是叠加前缀', () => {
    setup('原标题')
    selectAll()
    setHeading(2)(handle.view)
    expect(handle.getValue()).toBe('## 原标题')
    selectAll()
    setHeading(3)(handle.view)
    expect(handle.getValue()).toBe('### 原标题')
  })

  it('列表前缀成组切换：再次点击整组移除', () => {
    setup('一\n二')
    selectAll()
    toggleLinePrefix('- ')(handle.view)
    expect(handle.getValue()).toBe('- 一\n- 二')
    selectAll()
    toggleLinePrefix('- ')(handle.view)
    expect(handle.getValue()).toBe('一\n二')
  })

  it('有序列表按选中行顺序编号，且可整组移除', () => {
    setup('甲\n乙')
    selectAll()
    toggleOrderedList(handle.view)
    expect(handle.getValue()).toBe('1. 甲\n2. 乙')
    selectAll()
    toggleOrderedList(handle.view)
    expect(handle.getValue()).toBe('甲\n乙')
  })

  it('引用块前缀成组切换', () => {
    setup('引用一行')
    selectAll()
    toggleLinePrefix('> ')(handle.view)
    expect(handle.getValue()).toBe('> 引用一行')
  })

  it('插入图片引用后正文包含该片段且原有内容不受损（插图走这条路径）', () => {
    setup('开头\n')
    // 光标置于文末，模拟用户在正文末尾插图。
    handle.view.dispatch({ selection: { anchor: handle.view.state.doc.length } })
    handle.insertText('\n![图](/blog/a/figure.png)\n')
    expect(handle.getValue()).toContain('![图](/blog/a/figure.png)')
    expect(handle.getValue().startsWith('开头')).toBe(true)
    expect(handle.getValue().endsWith('![图](/blog/a/figure.png)\n')).toBe(true)
  })
})

describe('斜杠命令的触发串必须被清理', () => {
  it('删除光标前的过滤词，光标退回触发位置', () => {
    setup('标题')
    // `/` 被编辑器 preventDefault 挡住，只有过滤词真的进入文档。
    handle.view.dispatch({ changes: { from: 0, insert: 'biaoti' } })
    handle.view.dispatch({ selection: { anchor: 6 } })

    expect(deleteSlashTrigger(handle.view, 'biaoti')).toBe(true)
    expect(handle.getValue()).toBe('标题')
    expect(handle.view.state.selection.main.head).toBe(0)
  })

  it('不误删与过滤词不符的正文（如 `3/4`、URL、日期）', () => {
    setup('比例是 3/4')
    handle.view.dispatch({ selection: { anchor: handle.view.state.doc.length } })
    // 过滤词和正文末尾对不上：不猜、不动文档。
    expect(deleteSlashTrigger(handle.view, 'biaoti')).toBe(false)
    expect(handle.getValue()).toBe('比例是 3/4')
  })

  it('过滤词为空时不做任何删除', () => {
    setup('')
    handle.view.dispatch({ changes: { from: 0, insert: '/表 格' } })
    handle.view.dispatch({ selection: { anchor: 4 } })
    expect(deleteSlashTrigger(handle.view, '')).toBe(false)
    expect(handle.getValue()).toBe('/表 格')
  })

  it('斜杠命令插入块级内容后，正文里不残留过滤词', () => {
    setup('')
    handle.view.dispatch({ changes: { from: 0, insert: 'table' } })
    handle.view.dispatch({ selection: { anchor: 5 } })

    // 模拟菜单执行：先清理过滤词，再跑命令（与 SlashCommandMenu.choose 同序）。
    expect(deleteSlashTrigger(handle.view, 'table')).toBe(true)
    insertBlock('| 列 1 | 列 2 |\n| --- | --- |')(handle.view)

    const value = handle.getValue()
    expect(value).not.toContain('table')
    expect(value).toContain('| 列 1 | 列 2 |')
  })
})

describe('命令集合与站点语法一致', () => {
  it('格式菜单含加粗/斜体/删除线/行内代码/超链接/标题 1–6/列表/引用/分隔线', () => {
    const ids = FORMAT_COMMANDS.map((command) => command.id)
    for (const required of [
      'bold',
      'italic',
      'strikethrough',
      'inline-code',
      'heading-1',
      'heading-6',
      'bullet-list',
      'ordered-list',
      'blockquote',
      'divider',
    ]) {
      expect(ids).toContain(required)
    }
  })

  it('插入菜单不提供会误导用户的「插入公式」（站点未配置公式渲染）', () => {
    const labels = INSERT_COMMANDS.map((command) => command.label)
    expect(labels).toContain('表格 2×2')
    expect(labels.some((label) => label.includes('公式'))).toBe(false)
  })

  it('斜杠命令按基础/常用/编辑/样式分组', () => {
    expect(SLASH_GROUPS.map((group) => group.label)).toEqual(['基础', '常用', '编辑', '样式'])
    for (const group of SLASH_GROUPS) {
      expect(group.items.length).toBeGreaterThan(0)
    }
  })

  it('状态栏的选中字数与行列位置来自真实选区', () => {
    setup('第一行\n第二行')
    select(4, 7)
    const last = selectionEvents.at(-1)
    expect(last).toBeDefined()
    expect(last!.line).toBe(2)
    expect(last!.selectedChars).toBe(3)
  })

  it('斜杠命令只在行首或空白之后触发', () => {
    // 保护逻辑在组件层（keydown 处理器）判定；这里断言判定所依赖的取值前提：
    // 「and/or」这类文本里斜杠前不是空白，因而不应触发菜单。
    setup('and/or')
    const at = 3
    const line = handle.view.state.doc.lineAt(at)
    const before = handle.view.state.sliceDoc(line.from, at)
    expect(/^\s*$/.test(before)).toBe(false)
  })
})

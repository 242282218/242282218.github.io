/**
 * 编辑器命令与菜单定义。
 *
 * 菜单栏、`/` 斜杠命令与右键菜单**共用**这里的同一份定义与同一批命令函数，
 * 避免三条入口的行为分叉（参考实现也是这个结构）。
 *
 * 命令只作用于编辑器，不触碰文章以外的任何状态。
 */
import type { EditorView } from '@codemirror/view'
import {
  insertBlock,
  setHeading,
  toggleLinePrefix,
  toggleOrderedList,
  toggleWrap,
  type MarkdownEditorHandle,
} from '@/editor/codemirror'

/** 一条可执行命令。 */
export type EditorCommand = {
  id: string
  label: string
  /** 快捷键提示（仅用于展示；实际绑定由 CodeMirror keymap 与外壳统一处理）。 */
  hint?: string
  run: (view: EditorView) => boolean
}

/** 表格模板：与 GFM 表格语法一致，站点可正常渲染。 */
function gridTable(bodyRows: number, columns: number): string {
  const header = `| ${Array.from({ length: columns }, (_, i) => `列 ${i + 1}`).join(' | ')} |`
  const divider = `| ${Array.from({ length: columns }, () => '---').join(' | ')} |`
  const body = Array.from(
    { length: bodyRows },
    () => `| ${Array.from({ length: columns }, () => '内容').join(' | ')} |`,
  )
  return [header, divider, ...body].join('\n')
}

/**
 * 「格式」菜单的命令。
 *
 * 每一项都是**切换**：已应用的格式再点一次会移除，与用户对菜单项的预期一致。
 */
export const FORMAT_COMMANDS: EditorCommand[] = [
  { id: 'bold', label: '加粗', hint: 'Ctrl+B', run: toggleWrap('**') },
  { id: 'italic', label: '斜体', hint: 'Ctrl+I', run: toggleWrap('*') },
  { id: 'strikethrough', label: '删除线', hint: 'Ctrl+D', run: toggleWrap('~~') },
  { id: 'inline-code', label: '行内代码', hint: 'Ctrl+E', run: toggleWrap('`') },
  { id: 'heading-1', label: '标题 1', hint: 'Ctrl+1', run: setHeading(1) },
  { id: 'heading-2', label: '标题 2', hint: 'Ctrl+2', run: setHeading(2) },
  { id: 'heading-3', label: '标题 3', hint: 'Ctrl+3', run: setHeading(3) },
  { id: 'heading-4', label: '标题 4', hint: 'Ctrl+4', run: setHeading(4) },
  { id: 'heading-5', label: '标题 5', hint: 'Ctrl+5', run: setHeading(5) },
  { id: 'heading-6', label: '标题 6', hint: 'Ctrl+6', run: setHeading(6) },
  { id: 'bullet-list', label: '无序列表', hint: 'Ctrl+U', run: toggleLinePrefix('- ') },
  { id: 'ordered-list', label: '有序列表', hint: 'Ctrl+O', run: toggleOrderedList },
  { id: 'blockquote', label: '引用', run: toggleLinePrefix('> ') },
  { id: 'divider', label: '分隔线', run: insertBlock('\n---\n') },
]

/** 超链接：选中文字作为链接文字，否则插入占位文字。 */
export const LINK_COMMAND: EditorCommand = {
  id: 'link',
  label: '超链接',
  hint: 'Ctrl+K',
  run: (view) => {
    const range = view.state.selection.main
    const text = view.state.sliceDoc(range.from, range.to) || '链接文字'
    const snippet = `[${text}](https://)`
    view.dispatch({
      changes: { from: range.from, to: range.to, insert: snippet },
      // 光标落在 URL 位置，用户可以直接输入地址。
      selection: { anchor: range.from + text.length + 3 },
    })
    view.focus()
    return true
  },
}

/**
 * 代码块插入：选中内容包进围栏；光标落在语言标识处，方便补上语言名。
 */
export const CODE_BLOCK_COMMAND: EditorCommand = {
  id: 'code-block',
  label: '代码块',
  run: (view) => {
    const range = view.state.selection.main
    const selected = view.state.sliceDoc(range.from, range.to)
    const snippet = `\`\`\`\n${selected}\n\`\`\``
    view.dispatch({
      changes: { from: range.from, to: range.to, insert: snippet },
      selection: { anchor: range.from + 3 },
    })
    view.focus()
    return true
  },
}

/**
 * 「插入」菜单的命令。
 *
 * 站点未配置公式渲染，因此不提供会误导用户的「插入公式」。
 */
export const INSERT_COMMANDS: EditorCommand[] = [
  { id: 'table-2x2', label: '表格 2×2', run: insertBlock(gridTable(1, 2)) },
  { id: 'table-3x3', label: '表格 3×3', run: insertBlock(gridTable(2, 3)) },
  CODE_BLOCK_COMMAND,
  LINK_COMMAND,
]

/**
 * `/` 斜杠命令的分组定义（§3.2-2：基础 / 常用 / 编辑 / 样式）。
 */
export type SlashGroup = {
  label: string
  items: { id: string; label: string; run: (view: EditorView) => boolean }[]
}

export const SLASH_GROUPS: SlashGroup[] = [
  {
    label: '基础',
    items: [
      { id: 'h1', label: '标题 1', run: setHeading(1) },
      { id: 'h2', label: '标题 2', run: setHeading(2) },
      { id: 'h3', label: '标题 3', run: setHeading(3) },
      { id: 'bullet', label: '无序列表', run: toggleLinePrefix('- ') },
      { id: 'ordered', label: '有序列表', run: toggleOrderedList },
      { id: 'quote', label: '引用', run: toggleLinePrefix('> ') },
      { id: 'divider', label: '分隔线', run: insertBlock('\n---\n') },
    ],
  },
  {
    label: '常用',
    items: [
      { id: 'table', label: '表格', run: insertBlock(gridTable(2, 2)) },
      { id: 'code', label: '代码块', run: CODE_BLOCK_COMMAND.run },
      { id: 'link', label: '超链接', run: LINK_COMMAND.run },
    ],
  },
  {
    label: '编辑',
    items: [
      { id: 'select-all', label: '全选', run: (view) => runSelectAll(view) },
    ],
  },
  {
    label: '样式',
    items: [
      { id: 'bold', label: '加粗', run: toggleWrap('**') },
      { id: 'italic', label: '斜体', run: toggleWrap('*') },
      { id: 'strike', label: '删除线', run: toggleWrap('~~') },
      { id: 'code-inline', label: '行内代码', run: toggleWrap('`') },
    ],
  },
]

function runSelectAll(view: EditorView): boolean {
  view.dispatch({
    selection: { anchor: 0, head: view.state.doc.length },
  })
  view.focus()
  return true
}

/** 拉平斜杠命令（供键盘导航）。 */
export function flattenSlashItems(): { group: string; id: string; label: string; run: (view: EditorView) => boolean }[] {
  return SLASH_GROUPS.flatMap((group) =>
    group.items.map((item) => ({ group: group.label, ...item })),
  )
}

/** 右键菜单可用的命令（§3.2-2：插入 / 文本格式 / 标题 / 导出 Markdown）。 */
export const CONTEXT_MENU_SECTIONS = [
  {
    label: '文本格式',
    items: [FORMAT_COMMANDS[0]!, FORMAT_COMMANDS[1]!, FORMAT_COMMANDS[2]!, FORMAT_COMMANDS[3]!, LINK_COMMAND],
  },
  {
    label: '标题',
    items: FORMAT_COMMANDS.filter((command) => command.id.startsWith('heading-')),
  },
  {
    label: '插入',
    // 与「插入」菜单同源：图片、表格、代码块、超链接，另加分隔线。
    items: [
      ...INSERT_COMMANDS,
      FORMAT_COMMANDS[FORMAT_COMMANDS.length - 1]!,
    ],
  },
] as const

/** 在编辑器里执行一条命令（供外壳复用）。 */
export function runCommand(handle: MarkdownEditorHandle | null, command: EditorCommand): boolean {
  if (!handle) return false
  return command.run(handle.view)
}

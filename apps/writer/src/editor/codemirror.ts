/**
 * CodeMirror 6 编辑器内核。
 *
 * 与 Vditor 的关键区别：CodeMirror 编辑的是**原文**，不重排 Markdown。
 * 因此「打开 → 不编辑 → 保存」能保持磁盘字节不变（见 §3.2-4 的新约束）。
 *
 * 本模块只提供编辑器实例与命令；界面外壳（菜单栏、斜杠命令、右键菜单）
 * 复用同一组导出命令，保证三条入口的行为一致。
 */
import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands'
import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { cssLanguage } from '@codemirror/lang-css'
import { htmlLanguage } from '@codemirror/lang-html'
import { javascriptLanguage } from '@codemirror/lang-javascript'
import { jsonLanguage } from '@codemirror/lang-json'
import { pythonLanguage } from '@codemirror/lang-python'
import { rustLanguage } from '@codemirror/lang-rust'
import { yamlLanguage } from '@codemirror/lang-yaml'
import {
  EditorSelection,
  EditorState,
  type ChangeSpec,
  type SelectionRange,
} from '@codemirror/state'
import {
  EditorView,
  drawSelection,
  dropCursor,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  rectangularSelection,
} from '@codemirror/view'
import { search, searchKeymap } from '@codemirror/search'

/**
 * 围栏代码块的语言支持。
 *
 * 用 `codeLanguages` 的**回调形式**直接返回语法扩展：`LanguageDescription[]`
 * 要求 `load`/`loadFunc` 这类懒加载描述符，而这里已经静态导入了语法，
 * 回调省掉一层描述符映射，也不会把 `@codemirror/language-data` 整包打进产物。
 */
function codeLanguageFor(info: string) {
  const name = info.trim().toLowerCase()
  switch (name) {
    case 'js':
    case 'javascript':
    case 'jsx':
    case 'mjs':
    case 'cjs':
    case 'ts':
    case 'typescript':
    case 'tsx':
      return javascriptLanguage
    case 'css':
      return cssLanguage
    case 'html':
    case 'htm':
    case 'vue':
      return htmlLanguage
    case 'json':
      return jsonLanguage
    case 'yaml':
    case 'yml':
      return yamlLanguage
    case 'python':
    case 'py':
      return pythonLanguage
    case 'rust':
    case 'rs':
      return rustLanguage
    default:
      return null
  }
}

/** 一次可撤销的文本改写。 */
export type EditSpec = {
  /** 新的文档文本。 */
  text: string
  /** 改写后的选区（默认沿用原选区的折叠位置）。 */
  selection?: { anchor: number; head: number }
}

/**
 * 删除斜杠命令的过滤词。
 *
 * 触发用的 `/` 本身被编辑器 `preventDefault` 挡掉了（不会进正文），但**过滤词
 * 会真的写进文档**——菜单不阻止这些字符键入，否则用户打不出「表」来筛选。因此
 * 执行命令前必须把这段过滤词去掉，否则插入的块级内容旁边会残留「表」这样的垃圾。
 *
 * 只删除「光标前确实是这段过滤词」的情况：与文档不一致时不猜、不动文档，避免
 * 误删正文。返回是否真的删除了内容。
 */
export function deleteSlashTrigger(view: EditorView, query: string): boolean {
  const { state } = view
  const range = state.selection.main
  if (!range.empty || query.length === 0) return false
  const line = state.doc.lineAt(range.from)
  const from = range.from - query.length
  if (from < line.from) return false
  if (state.sliceDoc(from, range.from) !== query) return false
  view.dispatch({
    changes: { from, to: range.from, insert: '' },
    selection: EditorSelection.cursor(from),
  })
  return true
}

/**
 * 行内标记的包裹/解除包裹。
 *
 * 已包裹时再点一次**移除**标记——这才是「切换」，也是用户对加粗按钮的预期。
 */
export function toggleWrap(open: string, close = open): (view: EditorView) => boolean {
  return (view) => {
    const { state } = view
    const changes: ChangeSpec[] = []
    const selections: SelectionRange[] = []

    for (const range of state.selection.ranges) {
      const text = state.sliceDoc(range.from, range.to)
      // 选区外已带标记：移除它。
      const outerStart = range.from - open.length
      const outerEnd = range.to + close.length
      const wrapped =
        outerStart >= 0 &&
        outerEnd <= state.doc.length &&
        state.sliceDoc(outerStart, range.from) === open &&
        state.sliceDoc(range.to, outerEnd) === close

      if (wrapped) {
        changes.push({ from: outerStart, to: range.from, insert: '' })
        changes.push({ from: range.to, to: outerEnd, insert: '' })
        selections.push(EditorSelection.range(range.from - open.length, range.to - open.length))
        continue
      }

      // 选区内侧已带标记（`**text**` 全选）：同样移除。
      if (
        text.length >= open.length + close.length &&
        text.startsWith(open) &&
        text.endsWith(close)
      ) {
        const inner = text.slice(open.length, text.length - close.length)
        changes.push({ from: range.from, to: range.to, insert: inner })
        selections.push(EditorSelection.range(range.from, range.from + inner.length))
        continue
      }

      changes.push({ from: range.from, to: range.from, insert: open })
      changes.push({ from: range.to, to: range.to, insert: close })
      selections.push(EditorSelection.range(range.from + open.length, range.to + open.length))
    }

    view.dispatch({ changes, selection: EditorSelection.create(selections) })
    view.focus()
    return true
  }
}

/**
 * 设置行首标题级别（`#` ~ `######`）。
 *
 * 与「插入 `#`」不同：这是**切换**级别，已有标题会被替换而不是叠加。
 */
export function setHeading(level: number): (view: EditorView) => boolean {
  return (view) => {
    const { state } = view
    const changes: ChangeSpec[] = []
    const touchedLines = new Set<number>()

    for (const range of state.selection.ranges) {
      const startLine = state.doc.lineAt(range.from)
      const endLine = state.doc.lineAt(range.to)
      for (let lineNo = startLine.number; lineNo <= endLine.number; lineNo += 1) {
        if (touchedLines.has(lineNo)) continue
        touchedLines.add(lineNo)
        const line = state.doc.line(lineNo)
        const match = /^(#{1,6})\s+/.exec(line.text)
        const body = match ? line.text.slice(match[0].length) : line.text
        const prefix = `${'#'.repeat(level)} `
        changes.push({ from: line.from, to: line.to, insert: prefix + body })
      }
    }

    view.dispatch({ changes })
    view.focus()
    return true
  }
}

/**
 * 行首前缀的切换（列表、引用）。
 *
 * 同样的前缀再次点击即移除，保持「切换」语义。
 */
export function toggleLinePrefix(prefix: string): (view: EditorView) => boolean {
  return (view) => {
    const { state } = view
    const changes: ChangeSpec[] = []
    const lines = collectLines(state)

    // 全部行都已有前缀时整体移除，否则整体加上。
    const allPrefixed = lines.every((line) => line.text.startsWith(prefix))
    for (const line of lines) {
      if (allPrefixed) {
        changes.push({ from: line.from, to: line.from + prefix.length, insert: '' })
      } else if (!line.text.startsWith(prefix)) {
        changes.push({ from: line.from, to: line.from, insert: prefix })
      }
    }

    view.dispatch({ changes })
    view.focus()
    return true
  }
}

/** 有序列表：按选中行顺序编号。 */
export function toggleOrderedList(view: EditorView): boolean {
  const { state } = view
  const lines = collectLines(state)
  const allNumbered = lines.every((line) => /^\d+\.\s/.test(line.text))
  const changes: ChangeSpec[] = []

  lines.forEach((line, index) => {
    const existing = /^(\d+)\.\s/.exec(line.text)
    if (allNumbered) {
      if (existing) changes.push({ from: line.from, to: line.from + existing[0].length, insert: '' })
    } else {
      const body = existing ? line.text.slice(existing[0].length) : line.text
      changes.push({ from: line.from, to: line.to, insert: `${index + 1}. ${body}` })
    }
  })

  view.dispatch({ changes })
  view.focus()
  return true
}

/** 在选区前后各插入一行（用于表格、代码块这类块级内容）。 */
export function insertBlock(block: string): (view: EditorView) => boolean {
  return (view) => {
    const { state } = view
    const range = state.selection.main
    const line = state.doc.lineAt(range.from)
    // 空行处直接插入；否则先把块另起一行，避免接在正文后面。
    const needsLeadingBreak = line.text.trim().length > 0
    const insert = `${needsLeadingBreak ? '\n\n' : ''}${block}`
    view.dispatch({
      changes: { from: line.from, to: line.from, insert },
      selection: EditorSelection.cursor(line.from + insert.length),
    })
    view.focus()
    return true
  }
}

/** 收集选中范围内的所有行（去重、按文档顺序）。 */
function collectLines(state: EditorState) {
  const seen = new Set<number>()
  const out: { from: number; to: number; text: string; number: number }[] = []
  for (const range of state.selection.ranges) {
    const start = state.doc.lineAt(range.from).number
    const end = state.doc.lineAt(range.to).number
    for (let n = start; n <= end; n += 1) {
      if (seen.has(n)) continue
      seen.add(n)
      const line = state.doc.line(n)
      out.push({ from: line.from, to: line.to, text: line.text, number: n })
    }
  }
  out.sort((a, b) => a.number - b.number)
  return out
}

/**
 * 探测文本使用的换行风格。
 *
 * CodeMirror 6 在 `EditorState` 内部**固定**用 `\n` 存储行（`Text.of` 不接收
 * 分隔符，`EditorState.lineSeparator` 只影响变更解析，不影响文档本身的存储），
 * 因此 `\r\n` 会被静默抹掉——那会让「打开 → 不编辑 → 保存」改变磁盘字节。
 * 结论：换行风格必须在编辑器**外面**透明处理，不能指望它自己往返。
 */
export function detectLineSeparator(text: string): '\n' | '\r\n' {
  return text.includes('\r\n') ? '\r\n' : '\n'
}

/**
 * 编辑器句柄。
 *
 * 对外收发的一律是**原文换行风格**的文本；内部按 `\n` 存储。
 */
export type MarkdownEditorHandle = {
  view: EditorView
  /** 取回与载入时同一换行风格的文本。 */
  getValue: () => string
  /** 载入新文章内容（按该内容自己的换行风格往返）。 */
  setValue: (value: string) => void
  focus: () => void
  destroy: () => void
  /** 应用一次可撤销的改写。 */
  apply: (spec: EditSpec) => void
  /** 在光标处插入文本（插入图片引用等）。 */
  insertText: (text: string) => void
}

/**
 * 创建编辑器视图。
 *
 * `onChange` 只在**文档实际变化**时触发，因此打开文章时的初始赋值不会被
 * 当成用户编辑（否则会立刻触发一次自动保存，把未改动的文件写一遍）。
 */
export function createMarkdownEditor(options: {
  parent: HTMLElement
  value: string
  readonly?: boolean
  onChange: (value: string) => void
  /** 光标或选区变化（用于状态栏的行号与选中字数）。 */
  onSelectionChange?: (info: { line: number; column: number; selectedChars: number }) => void
  /** `/` 斜杠命令：返回 `true` 表示已接管。 */
  onSlash?: (view: EditorView) => boolean
  /** 右键菜单：返回 `true` 表示已接管。 */
  onContextMenu?: (view: EditorView, event: MouseEvent) => boolean
}): MarkdownEditorHandle {
  /**
   * 程序性写入（打开/切换文章）期间抑制变化事件。
   *
   * 放在这里而不是调用方：`setValue` / `load` 的任何调用都不是用户编辑，
   * 让每个调用方各自记得加标志迟早会漏一个。
   */
  let suppressChange = false
  /**
   * 向外发送时使用的换行符。
   *
   * CodeMirror 内部一律用 `\n` 存行，因此 `\r\n` 原文必须在进出两侧各转换一次
   * 才能逐字节往返（见 `detectLineSeparator` 的说明）。
   */
  let outSeparator = detectLineSeparator(options.value)
  const toInner = (text: string): string => text.replace(/\r\n/g, '\n')
  const toOuter = (text: string): string =>
    outSeparator === '\n' ? text : text.replace(/\n/g, '\r\n')

  const updateListener = EditorView.updateListener.of((update) => {
    if (update.docChanged) {
      if (!suppressChange) {
        options.onChange(toOuter(update.state.doc.toString()))
      }
    }
    if (update.selectionSet || update.docChanged) {
      const range = update.state.selection.main
      const line = update.state.doc.lineAt(range.head)
      options.onSelectionChange?.({
        line: line.number,
        column: range.head - line.from + 1,
        selectedChars: Math.abs(range.to - range.from),
      })
    }
  })

  const extensions = [
    lineNumbers(),
    highlightActiveLineGutter(),
    highlightSpecialChars(),
    history(),
    drawSelection(),
    dropCursor(),
    EditorState.allowMultipleSelections.of(true),
    rectangularSelection(),
    highlightActiveLine(),
    search({ top: true }),
    markdown({ base: markdownLanguage, codeLanguages: codeLanguageFor }),
    keymap.of([...defaultKeymap, ...historyKeymap, ...searchKeymap, indentWithTab]),
    updateListener,
    EditorView.lineWrapping,
    EditorView.contentAttributes.of({ 'aria-label': 'Markdown 正文' }),
  ]

  if (options.onSlash || options.onContextMenu) {
    extensions.push(
      EditorView.domEventHandlers({
        keydown: (event, view) => {
          if (event.key === '/' && options.onSlash && !event.ctrlKey && !event.metaKey) {
            // 斜杠命令只在行首或空白之后触发，避免打断 `and/or` 这类正常输入。
            const range = view.state.selection.main
            const line = view.state.doc.lineAt(range.from)
            const before = view.state.sliceDoc(line.from, range.from)
            if (/^\s*$/.test(before) && options.onSlash(view)) {
              event.preventDefault()
              return true
            }
          }
          return false
        },
        contextmenu: (event, view) => {
          if (!options.onContextMenu) return false
          return options.onContextMenu(view, event)
        },
      }),
    )
  }

  if (options.readonly) {
    extensions.push(EditorState.readOnly.of(true))
  }

  const view = new EditorView({
    state: EditorState.create({
      doc: options.value,
      // 按原文换行风格往返，保证「不编辑就保存」字节不变。
      extensions: [EditorState.lineSeparator.of(detectLineSeparator(options.value)), ...extensions],
    }),
    parent: options.parent,
  })

  /**
   * 整体替换文档内容（打开/切换文章）。
   *
   * 用一次性 dispatch 保持撤销历史可用；期间抑制变化事件，并把换行风格
   * 切换到新内容的风格。
   */
  const replaceAll = (value: string): void => {
    suppressChange = true
    try {
      // 切换到新内容的换行风格（不同文章可能来自不同的编辑器/平台）。
      outSeparator = detectLineSeparator(value)
      view.dispatch({
        changes: { from: 0, to: view.state.doc.length, insert: toInner(value) },
        selection: EditorSelection.cursor(0),
      })
    } finally {
      suppressChange = false
    }
  }

  return {
    view,
    getValue: () => toOuter(view.state.doc.toString()),
    setValue: replaceAll,
    focus: () => view.focus(),
    destroy: () => view.destroy(),
    apply: (spec: EditSpec) => {
      replaceAll(spec.text)
      if (spec.selection) {
        view.dispatch({
          selection: EditorSelection.range(spec.selection.anchor, spec.selection.head),
        })
      }
      view.focus()
    },
    insertText: (text: string) => {
      const range = view.state.selection.main
      // 插入的片段本身可能带 CRLF；统一转成内部风格，避免混入 `\r`。
      const inner = toInner(text)
      view.dispatch({
        changes: { from: range.from, to: range.to, insert: inner },
        selection: EditorSelection.cursor(range.from + inner.length),
      })
      view.focus()
    },
  }
}

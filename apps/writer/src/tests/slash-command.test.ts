/**
 * 斜杠命令菜单的行为回归（§3.2-2）。
 *
 * 为什么单独一个文件：这一族缺陷的历史形态是「声明了但没人消费」——
 * `deleteSlashTrigger` 有实现、有单测，但菜单不调用它，过滤词照样留在正文里，
 * 类型检查与既有测试全绿。因此这里**驱动真实组件**：挂载菜单、敲入过滤词、
 * 点击命令，再断言编辑器文档里没有残留。
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import { createMarkdownEditor, type MarkdownEditorHandle } from '@/editor/codemirror'
import SlashCommandMenu from '@/components/SlashCommandMenu.vue'

let host: HTMLElement
let handle: MarkdownEditorHandle
let wrapper: ReturnType<typeof mount> | null = null

/** 敲入一个字符（与菜单的 `handleKeydown` 读取的字段一致）。 */
function typeKey(key: string): void {
  window.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true }))
}

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.appendChild(host)
  handle = createMarkdownEditor({ parent: host, value: '', onChange: () => {} })
})

afterEach(() => {
  wrapper?.unmount()
  wrapper = null
  document.body.innerHTML = ''
})

describe('斜杠菜单执行后不残留过滤词', () => {
  it('敲入过滤词再点命令：正文里没有该过滤词，只有插入的块', async () => {
    // 模拟用户在空文档里输入过滤词（`/` 被编辑器挡住，不进文档）。
    handle.view.dispatch({ changes: { from: 0, insert: 'table' } })
    handle.view.dispatch({ selection: { anchor: 5 } })

    wrapper = mount(SlashCommandMenu, { props: { handle }, attachTo: document.body })
    for (const key of ['t', 'a', 'b', 'l', 'e']) typeKey(key)
    await flushPromises()

    // 点第一条匹配的命令（`表格`）。
    const first = wrapper.findAll('button.slash-item')[0]
    expect(first, '过滤后应有候选项').toBeTruthy()
    await first!.trigger('click')

    const text = handle.view.state.doc.toString()
    expect(text, `正文不应残留过滤词，实际：${JSON.stringify(text)}`).not.toContain('table')
    expect(text).toContain('|')
  })

  it('未敲过滤词时直接执行命令，正文同样干净', async () => {
    wrapper = mount(SlashCommandMenu, { props: { handle }, attachTo: document.body })
    const first = wrapper.findAll('button.slash-item')[0]
    await first!.trigger('click')

    const text = handle.view.state.doc.toString()
    expect(text).not.toContain('table')
    expect(text.length).toBeGreaterThan(0)
  })
})

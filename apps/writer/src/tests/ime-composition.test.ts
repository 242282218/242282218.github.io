/**
 * 中文输入法（IME）组合态保护——CodeMirror 6 版本。
 *
 * 本机无法自动完成真人输入法操作（见交付说明的「未验证项」），因此这里用
 * 标准 DOM 组合事件（`compositionstart` / `compositionupdate` / `compositionend`）
 * 驱动**真实组件代码路径**，确定性地验证保护逻辑：
 *
 * 1. 组合期间不向外发送正文变化 → 不会排入自动保存（「候选未结束不触发保存」）；
 * 2. 组合结束后一次性交还上屏结果，中文内容完整保留；
 * 3. 组合期间的内容不会被丢弃（`compositionend` 交还的是最终文本）。
 *
 * 这不等于真人在微软拼音下的手工验收；它验证的是保护逻辑本身。
 */
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import { nextTick } from 'vue'
import CodeMirrorEditor from '@/components/CodeMirrorEditor.vue'

let wrapper: ReturnType<typeof mount> | null = null

function mountEditor(value = '原有正文\n') {
  wrapper = mount(CodeMirrorEditor, {
    props: {
      modelValue: value,
      fontSize: 16,
      lineHeight: 1.75,
      canInsertImage: true,
    },
    attachTo: document.body,
  })
  return wrapper
}

/** 在组件宿主上派发一个组合事件（与真实输入法的事件目标一致）。 */
function dispatchComposition(
  el: Element,
  type: 'compositionstart' | 'compositionupdate' | 'compositionend',
  data = '',
): void {
  el.dispatchEvent(new CompositionEvent(type, { data, bubbles: true }))
}

async function settle(): Promise<void> {
  await nextTick()
  await Promise.resolve()
  await nextTick()
}

beforeEach(() => {
  document.body.innerHTML = ''
})

afterEach(() => {
  wrapper?.unmount()
  wrapper = null
})

describe('CodeMirror 组合态（IME）保护', () => {
  it('组合期间不发送正文变化（候选未结束不触发保存）', async () => {
    const editor = mountEditor()
    await settle()

    const content = editor.find('.cm-content').element
    // 组合开始后模拟编辑器注入中间文本（拼音/候选）。
    dispatchComposition(content, 'compositionstart')
    // 直接改文档，模拟 CodeMirror 在组合中更新内容。
    // 这条路径必须被保护：否则父组件会立刻标 dirty 并排自动保存。
    const handle = (editor.vm as unknown as { handle: () => { view: import('@codemirror/view').EditorView } | null }).handle()
    handle!.view.dispatch({ changes: { from: 0, insert: 'zhongwen' } })
    await settle()

    expect(editor.emitted('update:modelValue')).toBeUndefined()

    dispatchComposition(content, 'compositionend', '中文')
    await settle()

    // 组合结束后一次性交还最终文本。
    const events = editor.emitted('update:modelValue')
    expect(events).toBeTruthy()
    expect(events!.at(-1)![0]).toContain('zhongwen')
  })

  it('组合结束后中文内容完整保留，不被截断或转义', async () => {
    const editor = mountEditor('')
    await settle()
    const content = editor.find('.cm-content').element
    const handle = (editor.vm as unknown as { handle: () => { view: import('@codemirror/view').EditorView } | null }).handle()

    const chinese = '这是用中文输入法上屏的一段正文，含「引号」与（括号）。'
    dispatchComposition(content, 'compositionstart')
    dispatchComposition(content, 'compositionupdate', 'zheshi')
    handle!.view.dispatch({ changes: { from: 0, insert: chinese } })
    dispatchComposition(content, 'compositionend', chinese)
    await settle()

    const events = editor.emitted('update:modelValue')
    expect(events!.at(-1)![0]).toBe(chinese)
    expect(String(events!.at(-1)![0])).toContain('「引号」')
    // 只有组合结束时那一次外部发送（组合期间被抑制）。
    expect(events).toHaveLength(1)
  })

  it('组合态变化会通知父组件（界面据此避免打断输入）', async () => {
    const editor = mountEditor()
    await settle()
    const content = editor.find('.cm-content').element

    dispatchComposition(content, 'compositionstart')
    dispatchComposition(content, 'compositionupdate', 'pin')
    dispatchComposition(content, 'compositionend', '拼')
    await settle()

    const states = (editor.emitted('composition') ?? []).map(
      (event) => (event[0] as { composing: boolean }).composing,
    )
    // 关键性质：组合一开始就为 true（外壳立即拒绝切篇），结束时回到 false。
    expect(states[0]).toBe(true)
    expect(states.at(-1)).toBe(false)
  })

  it('连续两次组合各自独立，第二次的内容不会丢失', async () => {
    const editor = mountEditor('')
    await settle()
    const content = editor.find('.cm-content').element
    const handle = (editor.vm as unknown as { handle: () => { view: import('@codemirror/view').EditorView } | null }).handle()

    dispatchComposition(content, 'compositionstart')
    handle!.view.dispatch({ changes: { from: 0, insert: '第一个' } })
    dispatchComposition(content, 'compositionend', '第一个')
    await settle()

    dispatchComposition(content, 'compositionstart')
    handle!.view.dispatch({ changes: { from: 3, insert: '第二个' } })
    dispatchComposition(content, 'compositionend', '第二个')
    await settle()

    const text = handle!.view.state.doc.toString()
    expect(text).toBe('第一个第二个')
  })

  it('非组合态的普通输入照常向外发送', async () => {
    const editor = mountEditor('')
    await settle()
    const handle = (editor.vm as unknown as { handle: () => { view: import('@codemirror/view').EditorView } | null }).handle()

    handle!.view.dispatch({ changes: { from: 0, insert: 'plain' } })
    await settle()

    const events = editor.emitted('update:modelValue')
    expect(events).toBeTruthy()
    expect(events!.at(-1)![0]).toBe('plain')
  })

  it('组合期间父组件换文章：挂起替换，组合结束后落到新内容且不发出旧文章的编辑', async () => {
    const editor = mountEditor('第一篇文章')
    await settle()
    const content = editor.find('.cm-content').element
    const handle = (editor.vm as unknown as { handle: () => { view: import('@codemirror/view').EditorView } | null }).handle()

    dispatchComposition(content, 'compositionstart')
    // 组合中输入了拼音（尚未上屏）。
    handle!.view.dispatch({ changes: { from: 0, insert: 'pinyin' } })
    await settle()

    // 此刻父组件换文章：不打断输入法会话，先挂起。
    await editor.setProps({ modelValue: '第二篇文章' })
    await settle()
    // 组合未结束，文档仍是组合中的内容——替换被挂起。
    expect(handle!.view.state.doc.toString()).not.toBe('第二篇文章')

    dispatchComposition(content, 'compositionend', '拼音')
    await settle()

    // 组合结束后落到新文章；组合产生的旧文章文本被丢弃，不回写正文。
    expect(handle!.view.state.doc.toString()).toBe('第二篇文章')
    expect(editor.emitted('update:modelValue')).toBeUndefined()
  })

  it('组合结束后通知外壳可以恢复交互', async () => {
    const editor = mountEditor('')
    await settle()
    const content = editor.find('.cm-content').element

    dispatchComposition(content, 'compositionstart')
    await settle()
    // 组合开始时就通知外壳「正在组字」，外壳据此拒绝切篇。
    expect((editor.emitted('composition')![0]![0] as { composing: boolean }).composing).toBe(true)

    dispatchComposition(content, 'compositionend', '字')
    await settle()
    const states = editor.emitted('composition')!
    expect((states.at(-1)![0] as { composing: boolean }).composing).toBe(false)
  })
})

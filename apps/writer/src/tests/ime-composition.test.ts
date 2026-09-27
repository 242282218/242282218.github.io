/**
 * 组合态（IME）保护逻辑测试。
 *
 * 方案阶段 1 要求「Windows 手工 IME 测试」。本机无法自动完成真人输入法操作，
 * 因此这里用**标准 DOM 组合事件**（`compositionstart` / `compositionupdate` /
 * `compositionend`）驱动真实代码路径，确定性地验证保护逻辑：
 *
 * 1. 组合态在 start/end 之间被正确标记；
 * 2. 组合态期间切换编辑模式会等待组合结束，不会在半成品上切换；
 * 3. 上屏后的中文内容完整保留；
 * 4. 模式切换会真正重建编辑器；
 * 5. 往返会损坏内容时回退源码模式并保留原文；
 * 6. 插入文本片段落到编辑器内容里（插图引用走这条路径）。
 *
 * 编辑器通过 `useVditor` 的注入点替换为替身，因此这里不依赖真实排版引擎。
 * 这不等于真人在中文输入法下的手工验收（见交付说明的未验证项），
 * 它验证的是保护逻辑本身。
 */
import { beforeEach, describe, expect, it } from 'vitest'
import { defineComponent, h, nextTick, onMounted, ref } from 'vue'
import { mount } from '@vue/test-utils'
import { useVditor, type VditorConstructor } from '@/components/MarkdownEditor'
import type { EditorMode } from '@/types/article'

/** 假 Vditor 实例的能力（与组件用到的方法一致）。 */
type FakeInstance = {
  value: string
  destroyed: boolean
  getValue: () => string
  setValue: (value: string) => void
  destroy: () => void
  focus: () => void
  disabled: () => void
  enable: () => void
  insertValue: (value: string) => void
}

/** 共享的替身状态。 */
const fake = {
  /** 最近一次创建的实例。 */
  instance: null as FakeInstance | null,
  /** 累计销毁次数，用于验证模式切换确实重建了编辑器。 */
  destroyCount: 0,
  /**
   * 模拟「某个编辑模式会改写正文」。
   *
   * 设为 `'ir'` 时，只有以 `ir` 模式创建的实例 `getValue()` 会返回
   * `corruptTo`；源码模式（回退后的模式）不受影响——这忠实对应
   * 「ir 解析器丢掉代码围栏、回退到 sv 后应恢复原文」的场景。
   */
  corruptInMode: null as null | EditorMode,
  corruptTo: null as string | null,
}

/** 构造一个替身 Vditor 构造函数。 */
function makeFakeConstructor(): VditorConstructor {
  class FakeVditor {
    private instance: FakeInstance

    constructor(_element: HTMLElement, options: Record<string, unknown>) {
      const self = this
      const corrupts = fake.corruptInMode !== null && options.mode === fake.corruptInMode
      this.instance = {
        value: (options.value as string | undefined) ?? '',
        destroyed: false,
        getValue: () => (corrupts ? fake.corruptTo ?? '' : self.instance.value),
        setValue: (value: string) => {
          self.instance.value = value
        },
        destroy: () => {
          self.instance.destroyed = true
          fake.destroyCount += 1
        },
        focus: () => undefined,
        disabled: () => undefined,
        enable: () => undefined,
        insertValue: (snippet: string) => {
          self.instance.value += snippet
        },
      }
      fake.instance = this.instance
      // 真实 Vditor 在初始化完成后回调 `after`；这里同步回调即可。
      const after = options.after as (() => void) | undefined
      if (after) after()
    }

    getValue() {
      return this.instance.getValue()
    }

    setValue(value: string) {
      this.instance.setValue(value)
    }

    destroy() {
      this.instance.destroy()
    }

    focus() {
      this.instance.focus()
    }

    disabled() {
      this.instance.disabled()
    }

    enable() {
      this.instance.enable()
    }

    insertValue(value: string) {
      this.instance.insertValue(value)
    }
  }
  return FakeVditor as unknown as VditorConstructor
}

/** 把 `useVditor` 挂到一个最小组件里，使 onMounted / onBeforeUnmount 生效。 */
function mountEditor(bodyText: string, mode: EditorMode = 'sv') {
  const body = ref(bodyText)
  const editorMode = ref<EditorMode>(mode)
  let api: ReturnType<typeof useVditor> | null = null

  const Harness = defineComponent({
    setup() {
      api = useVditor(body, editorMode, {
        loadVditor: async () => makeFakeConstructor(),
      })
      // 与 `MarkdownEditor.vue` 一致：挂载后创建编辑器。
      onMounted(() => {
        void api!.createVditor()
      })
      return () => h('div', { ref: api!.host })
    },
  })

  const wrapper = mount(Harness, { attachTo: document.body })
  return { wrapper, body, editorMode, api: () => api! }
}

/** 派发一个组合事件。 */
function dispatch(type: 'compositionstart' | 'compositionupdate' | 'compositionend', data = '') {
  window.dispatchEvent(new CompositionEvent(type, { data, bubbles: true }))
}

/** 等待 `after` 微任务与 Vue 更新都完成。 */
async function settle() {
  await nextTick()
  await Promise.resolve()
  await nextTick()
}

describe('组合态（IME）保护', () => {
  beforeEach(() => {
    fake.instance = null
    fake.destroyCount = 0
    fake.corruptInMode = null
    fake.corruptTo = null
  })

  it('组合态在 start/end 之间被正确标记', async () => {
    const { wrapper, api } = mountEditor('原有正文')
    await settle()

    expect(api().composing.value).toBe(false)
    dispatch('compositionstart')
    expect(api().composing.value).toBe(true)
    dispatch('compositionupdate', 'zhongwen')
    expect(api().composing.value).toBe(true)
    dispatch('compositionend', '中文')
    expect(api().composing.value).toBe(false)

    wrapper.unmount()
  })

  it('组合态期间切换模式会等待组合结束，不会在半成品上切换', async () => {
    const { wrapper, editorMode, api } = mountEditor('正文', 'sv')
    await settle()

    dispatch('compositionstart')
    let settled = false
    const switching = api().switchMode('ir').then(() => {
      settled = true
    })

    // 组合未结束时切换不应完成。
    await nextTick()
    await Promise.resolve()
    expect(settled, '组合态未结束前不应切换完成').toBe(false)
    expect(editorMode.value).toBe('sv')

    // 结束组合后切换继续。
    dispatch('compositionend', '中文')
    await switching
    await settle()
    expect(settled).toBe(true)
    expect(editorMode.value).toBe('ir')

    wrapper.unmount()
  })

  it('上屏后的中文内容完整保留，不被误判为冲突', async () => {
    const chinese = '这是用中文输入法上屏的一段正文，含「引号」与（括号）。'
    const { wrapper, body, api } = mountEditor('', 'sv')
    await settle()

    dispatch('compositionstart')
    dispatch('compositionupdate', 'zheshi')
    dispatch('compositionend', chinese)
    // 模拟编辑器把上屏结果写入正文（真实 Vditor 走 input 回调）。
    body.value = chinese
    await settle()

    expect(api().blockedReport.value).toBeNull()
    expect(api().lastReport.value).toBeNull()
    expect(body.value).toBe(chinese)
    expect(body.value).toContain('「引号」')

    wrapper.unmount()
  })

  it('模式切换在内容一致时不阻止，并真正重建编辑器', async () => {
    const { wrapper, editorMode, api } = mountEditor('正文\n', 'sv')
    await settle()

    const report = await api().switchMode('ir')
    await settle()

    expect(report?.blocked ?? false).toBe(false)
    expect(editorMode.value).toBe('ir')
    expect(fake.destroyCount).toBeGreaterThan(0)
    expect(api().blockedReport.value).toBeNull()

    wrapper.unmount()
  })

  it('插入文本片段会落到编辑器内容里（插图引用走这条路径）', async () => {
    const { wrapper, api } = mountEditor('开头\n', 'sv')
    await settle()

    api().insertText('\n![图](/blog/a/f.png)\n')
    await settle()

    const value = api().currentValue()
    expect(value).toContain('![图](/blog/a/f.png)')
    expect(value.startsWith('开头')).toBe(true)
    // 插入后正文状态同步，后续自动保存不会写回旧内容。
    expect(value).toBe(api().currentValue())

    wrapper.unmount()
  })

  it('往返会损坏内容时回退源码模式并保留原文', async () => {
    const original = '```ts\nconst a = 1\n```\n'
    const { wrapper, body, editorMode, api } = mountEditor(original, 'sv')
    await settle()

    // 模拟 ir 解析器丢掉代码围栏（只有 ir 模式会出现这种改写）。
    fake.corruptInMode = 'ir'
    fake.corruptTo = 'const a = 1\n'

    await api().switchMode('ir')
    await settle()

    // 必须回退源码模式，且原文没有被覆盖。
    expect(editorMode.value).toBe('sv')
    const blocked = api().blockedReport.value
    expect(blocked).not.toBeNull()
    expect(blocked?.reason).toContain('回退源码模式')
    // 正文状态与回退后的编辑器内容都是原文。
    expect(body.value).toBe(original)
    expect(api().currentValue()).toBe(original)

    wrapper.unmount()
  })
})

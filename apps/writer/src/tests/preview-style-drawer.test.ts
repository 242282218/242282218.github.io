/**
 * 样式抽屉的消费点回归（§3.4）。
 *
 * 为什么单独守护这一条：`InstantPreview` 里的覆盖规则曾经用
 * `var(--preview-font-size, 16px)` 写，而变量定义在**父文档**——iframe 是
 * 独立文档，自定义属性不跨边界，`var()` 静默退化成 fallback，于是「把字号
 * 调到 18px」在预览里毫无变化，且类型检查与既有测试全绿。
 *
 * 本测试断言覆盖值是**字面量**并且真的写进了 iframe 文档；改回变量形式即变红。
 */
import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import InstantPreview from '@/components/InstantPreview.vue'
import { DEFAULT_PREVIEW_STYLES, type PreviewStyles } from '@/composables/usePreviewStyles'

/** 挂载预览并取回它写进 iframe 的覆盖规则。 */
function overrideRules(styles: Partial<PreviewStyles>): string {
  const wrapper = mount(InstantPreview, {
    props: {
      markdown: '# 标题\n\n正文',
      title: '标题',
      description: '摘要',
      pubDate: '2026-09-27',
      tags: [],
      styles: { ...DEFAULT_PREVIEW_STYLES, ...styles },
    },
    attachTo: document.body,
  })
  const frame = wrapper.find('iframe').element as HTMLIFrameElement
  const doc = frame.contentDocument
  if (!doc) throw new Error('预览 iframe 文档未就绪')
  // 覆盖块是第二个 <style>（第一个是站点样式快照）。
  const blocks = Array.from(doc.querySelectorAll('style')).map((node) => node.textContent ?? '')
  const override = blocks.find((text) => text.includes('data-site-preview'))
  wrapper.unmount()
  if (!override) throw new Error('预览文档里没有找到覆盖规则')
  return override
}

describe('样式抽屉：覆盖值必须真的写进预览文档', () => {
  it('字号是字面量，随抽屉取值变化', () => {
    expect(overrideRules({ fontSize: 18 })).toContain('font-size: 18px;')
    expect(overrideRules({ fontSize: 14 })).toContain('font-size: 14px;')
  })

  it('行距是字面量，随抽屉取值变化', () => {
    expect(overrideRules({ lineHeight: 1.5 })).toContain('line-height: 1.5;')
    expect(overrideRules({ lineHeight: 1.9 })).toContain('line-height: 1.9;')
  })

  it('未显式选择时落到站点基准 16px / 2.05', () => {
    const rules = overrideRules({ fontSize: null, lineHeight: null })
    expect(rules).toContain('font-size: 16px;')
    expect(rules).toContain('line-height: 2.05;')
  })

  it('覆盖规则不得依赖 CSS 变量（变量不跨 iframe 文档边界）', () => {
    // 这是本用例的核心：`var(--x, fallback)` 会让缺失的定义静默失效。
    const rules = overrideRules({ fontSize: 18, lineHeight: 1.5 })
    expect(rules).not.toContain('var(--preview-font-size')
    expect(rules).not.toContain('var(--preview-line-height')
  })

  it('段间距缩放仍显式写到 .prose p 上', () => {
    expect(overrideRules({ paragraphSpacingScale: 1.5 })).toContain(
      'margin-bottom: calc(21px * 1.5);',
    )
    // 站点原貌（×1）时不产生多余规则，避免无谓地覆盖站点样式。
    expect(overrideRules({ paragraphSpacingScale: 1 })).not.toContain('margin-bottom')
  })
})

describe('预览必须跟随头部字段变化', () => {
  /** 挂载预览并返回 iframe 文档的纯文本。 */
  function mountPreview(props: Record<string, unknown>) {
    const wrapper = mount(InstantPreview, {
      props: {
        markdown: '正文',
        title: '标题',
        description: '摘要',
        pubDate: '2026-09-27',
        tags: [],
        styles: { ...DEFAULT_PREVIEW_STYLES },
        ...props,
      },
      attachTo: document.body,
    })
    return wrapper
  }

  function textOf(wrapper: ReturnType<typeof mountPreview>): string {
    const frame = wrapper.find('iframe').element as HTMLIFrameElement
    return frame.contentDocument?.body.textContent ?? ''
  }

  it('改发布日期与标签后预览头部更新', async () => {
    const wrapper = mountPreview({ tags: ['旧标签'] })
    expect(textOf(wrapper)).toContain('2026-09-27')
    expect(textOf(wrapper)).toContain('旧标签')

    await wrapper.setProps({ pubDate: '2026-10-01', tags: ['新标签'] })
    const text = textOf(wrapper)
    expect(text).toContain('2026-10-01')
    expect(text).toContain('新标签')
    expect(text).not.toContain('旧标签')
    wrapper.unmount()
  })

  it('只改标签也会重绘', async () => {
    const wrapper = mountPreview({})
    await wrapper.setProps({ tags: ['随笔'] })
    expect(textOf(wrapper)).toContain('随笔')
    wrapper.unmount()
  })
})

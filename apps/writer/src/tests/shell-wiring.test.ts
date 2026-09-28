/**
 * 外壳接线回归：这几条都曾以「声明了但没人消费」的形态存在。
 *
 * 覆盖（施工单 §3.1-2 / §3.2-2 / §3.5-4 / §3.1-5）：
 * 1. 文章列表的选中态必须有对应 CSS——`:class="{ selected }"` 绑定了却没有规则，
 *    选中项在视觉上与未选中完全相同；
 * 2. 右键菜单的「导出 Markdown 文件」必须真的被监听（曾经 emit 出去无人接）；
 * 3. 外侧配色必须真的写到根元素（`:root.dark` 曾经零触发点）；
 * 4. 视图与侧栏折叠必须持久化。
 */
import { beforeEach, describe, expect, it } from 'vitest'
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { nextTick } from 'vue'
import { mount } from '@vue/test-utils'
import ArticleList from '@/components/ArticleList.vue'
import EditorContextMenu from '@/components/EditorContextMenu.vue'
import { CONTEXT_MENU_SECTIONS } from '@/editor/commands'
import { useShellLayout } from '@/composables/useShellLayout'
import { applyShellTheme, resolveDark } from '@/composables/useShellTheme'
import type { ArticleSummary } from '@/types/article'

const SRC_DIR = join(dirname(fileURLToPath(import.meta.url)), '..')
const read = (rel: string): string => readFileSync(join(SRC_DIR, rel), 'utf8')

/**
 * jsdom 没有实现 `matchMedia`，而「跟随系统」正是靠它判断的。
 * 补一个最小替身来驱动真实代码路径，而不是把测试降级成只看常量。
 */
let systemDark = false
const listeners = new Set<() => void>()
if (typeof window.matchMedia !== 'function') {
  window.matchMedia = ((query: string) =>
    ({
      media: query,
      get matches() {
        return query.includes('prefers-color-scheme: dark') ? systemDark : false
      },
      addEventListener: (_type: string, handler: () => void) => listeners.add(handler),
      removeEventListener: (_type: string, handler: () => void) => listeners.delete(handler),
    }) as unknown as MediaQueryList) as typeof window.matchMedia
}

/** 切换系统配色并通知监听者。 */
function setSystemDark(value: boolean): void {
  systemDark = value
  for (const handler of [...listeners]) handler()
}

describe('文章列表选中态', () => {
  it('选中规则真的作用于 .item.selected，不只是绑定 class', () => {
    const css = read('components/ArticleList.vue')
    // 绑定侧（模板）与规则侧（样式）必须同时存在。
    expect(css).toMatch(/:class="\{ selected:/)
    expect(css).toMatch(/\.item\.selected\s*\{/)
  })

  it('选中态不只靠颜色：同时有左侧竖条或边框变化', () => {
    const css = read('components/ArticleList.vue')
    const block = css.slice(css.indexOf('.item.selected {'))
    const body = block.slice(0, block.indexOf('}'))
    expect(body).toMatch(/box-shadow|border-left|border-color/)
  })

  it('选中项渲染时带上 selected 类', () => {
    const articles: ArticleSummary[] = [
      {
        id: 'a',
        title: '甲',
        description: '',
        tags: [],
        pubDate: '2026-09-01',
        draft: true,
        imageCount: 0,
        source: 'workspace',
        status: {
          locallySaved: true,
          remoteSync: 'unverified',
          site: 'unverified',
          localBodyHash: 'h',
          writing: { state: 'unverified' },
          main: { state: 'unverified' },
        },
      },
    ]
    const wrapper = mount(ArticleList, {
      props: {
        articles,
        selectedId: 'a',
        filter: 'all',
        sort: 'recent-edited',
        query: '',
        loading: false,
        selectedCounts: {},
      },
    })
    expect(wrapper.find('button.item').classes()).toContain('selected')
    wrapper.unmount()
  })
})

describe('右键菜单的导出入口', () => {
  it('组件真的监听并使用导出事件（不是只声明 emit）', () => {
    const source = read('components/CodeMirrorEditor.vue')
    expect(source).toMatch(/@export-markdown="emit\('export-markdown'\)"/)
    // 外壳必须接住它，否则引用到此为止。
    const app = read('App.vue')
    expect(app).toMatch(/@export-markdown="exportCurrentArticle"/)
    expect(app).toMatch(/async function exportCurrentArticle/)
  })

  it('点击「导出 Markdown 文件」会 emit 出去', async () => {
    const wrapper = mount(EditorContextMenu, {
      props: { x: 10, y: 10, handle: null },
    })
    const exportButton = wrapper
      .findAll('button.menu-item')
      .find((node) => node.text().includes('导出 Markdown 文件'))
    expect(exportButton, '右键菜单里应有导出项').toBeTruthy()
    await exportButton!.trigger('click')
    expect(wrapper.emitted('export-markdown')).toHaveLength(1)
    wrapper.unmount()
  })

  it('「插入」分组含超链接（与插入菜单同源）', () => {
    const insert = CONTEXT_MENU_SECTIONS.find((section) => section.label === '插入')
    const labels = insert!.items.map((item) => item.label)
    expect(labels).toContain('超链接')
    expect(labels).toContain('表格 2×2')
    expect(labels).toContain('代码块')
  })
})

describe('三段式布局与分隔条（§3.1-3 / §3.1-5）', () => {
  const app = read('App.vue')

  it('用 reka-ui 的 Splitter 原语提供可拖动分隔条（不是自写鼠标事件）', () => {
    expect(app).toMatch(/from 'reka-ui'/)
    expect(app).toMatch(/<SplitterGroup/)
    expect(app).toMatch(/<SplitterPanel/)
    expect(app).toMatch(/<SplitterResizeHandle/)
  })

  it('布局比例交给 SplitterGroup 的 autoSaveId 持久化，不另存一份宽度', () => {
    expect(app).toMatch(/auto-save-id="guanlanzhi\.workspaceRatio"/)
    expect(app).toMatch(/auto-save-id="guanlanzhi\.editorRatio"/)
    // 旧实现用自写 localStorage 宽度，会与 autoSaveId 立刻不一致。
    expect(read('composables/useShellLayout.ts')).not.toMatch(/listWidth/)
  })

  it('编辑与预览是左右并排（横向 group），不是上下堆叠', () => {
    // 编辑／预览的 group 必须是 direction="horizontal"；纵向堆叠正是本轮修掉的缺陷。
    const editorSplit = app.slice(app.indexOf('id="guanlanzhi-editor-split"'))
    const group = editorSplit.slice(0, editorSplit.indexOf('>'))
    expect(group).toContain('direction="horizontal"')
    // `.editor-row` 不再自己声明 flex-direction: column。
    const css = app.slice(app.indexOf('.editor-row {'))
    expect(css.slice(0, css.indexOf('}'))).not.toContain('flex-direction: column')
  })

  it('窄屏不渲染分隔条（三列强塞会挤坏布局）', () => {
    // 窄屏分支里不出现 SplitterGroup。
    const narrow = app.slice(app.indexOf('<template v-if="isNarrow">'))
    const narrowBlock = narrow.slice(0, narrow.indexOf('<!-- 桌面三列'))
    expect(narrowBlock).not.toContain('SplitterGroup')
  })

  it('窄屏也能打开文章列表（列表不因改布局而不可达）', () => {
    const narrow = app.slice(app.indexOf('<template v-if="isNarrow">'))
    const narrowBlock = narrow.slice(0, narrow.indexOf('<!-- 桌面三列'))
    // 窄屏分支必须同时含列表与折叠按钮：只有编辑器的话用户就选不了文章。
    expect(narrowBlock).toContain('<ArticleList')
    expect(narrowBlock).toContain('collapse-toggle')
    expect(narrowBlock).toMatch(/@click="listCollapsed = !listCollapsed"/)
  })
})

describe('预览视口宽度（§3.3-5）', () => {
  it('桌面预览按站点 .shell 视口取宽，而不是把阅读列当成视口', async () => {
    const { SITE_PROSE_BASELINE } = await import('@/composables/usePreviewStyles')
    // 760px 会落进站点 `@media (max-width: 780px)`，误触发平板规则。
    expect(SITE_PROSE_BASELINE.shellMaxWidthPx).toBeGreaterThan(780)
    const source = read('components/InstantPreview.vue')
    expect(source).toMatch(/shellMaxWidthPx/)
    expect(source).not.toMatch(/`\$\{SITE_PROSE_BASELINE\.articleMaxWidthPx\}px`/)
  })
})

describe('左栏折叠按钮的展开入口', () => {
  it('折叠后仍能展开（按钮在有列表与无列表两种情况下都存在）', () => {
    const app = read('App.vue')
    // 折叠按钮在 SplitterGroup 之外，因此收起来时依然可点。
    const btnIndex = app.indexOf('class="collapse-toggle"')
    expect(btnIndex).toBeGreaterThan(-1)
    expect(app.slice(btnIndex)).toMatch(/@click="listCollapsed = !listCollapsed"/)
  })
})

describe('外壳配色', () => {
  beforeEach(() => {
    document.documentElement.classList.remove('dark')
  })

  it('system 跟随系统，light / dark 是显式钉死', () => {
    expect(resolveDark('dark')).toBe(true)
    expect(resolveDark('light')).toBe(false)
    setSystemDark(true)
    expect(resolveDark('system')).toBe(true)
    setSystemDark(false)
    expect(resolveDark('system')).toBe(false)
  })

  it('应用到根元素的 dark 类上（:root.dark 的触发点）', () => {
    applyShellTheme('dark')
    expect(document.documentElement.classList.contains('dark')).toBe(true)
    applyShellTheme('light')
    expect(document.documentElement.classList.contains('dark')).toBe(false)
    // 跟随系统时由系统决定。
    setSystemDark(true)
    applyShellTheme('system')
    expect(document.documentElement.classList.contains('dark')).toBe(true)
    setSystemDark(false)
  })

  it('system 模式下系统配色切换会重新应用（有监听者，不是一次性读取）', async () => {
    setSystemDark(false)
    const { useShellTheme } = await import('@/composables/useShellTheme')
    const { ref } = await import('vue')
    const theme = ref<'system' | 'light' | 'dark'>('system')
    useShellTheme(theme)
    await nextTick()
    expect(document.documentElement.classList.contains('dark')).toBe(false)

    setSystemDark(true)
    await nextTick()
    expect(document.documentElement.classList.contains('dark')).toBe(true)

    setSystemDark(false)
  })

  it('theme.css 里确实有 :root.dark 规则可供切换', () => {
    expect(read('styles/theme.css')).toMatch(/:root\.dark\s*\{/)
  })
})

describe('外壳布局持久化', () => {
  beforeEach(() => {
    localStorage.clear()
  })

  it('视图与折叠状态写入本机存储并可恢复', async () => {
    const first = useShellLayout()
    first.viewMode.value = 'edit'
    first.listCollapsed.value = true
    await nextTick()

    expect(localStorage.getItem('guanlanzhi.viewMode')).toBe('edit')
    expect(localStorage.getItem('guanlanzhi.listCollapsed')).toBe('true')

    const second = useShellLayout()
    expect(second.viewMode.value).toBe('edit')
    expect(second.listCollapsed.value).toBe(true)
  })

  it('存储被写坏时退回默认值，而不是让界面拿不到状态', () => {
    localStorage.setItem('guanlanzhi.viewMode', 'nonsense')
    localStorage.setItem('guanlanzhi.listCollapsed', 'yes')
    const layout = useShellLayout()
    expect(layout.viewMode.value).toBe('split')
    expect(layout.listCollapsed.value).toBe(false)
  })
})
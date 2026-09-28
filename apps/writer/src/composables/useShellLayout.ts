/**
 * 外壳布局状态的持久化（§3.1-5：侧栏折叠状态与会话视图持久化）。
 *
 * 只保存「界面怎么看」——当前视图（编辑/双屏/预览）与文章列表是否收起——
 * 不涉及文章内容，因此走本机 `localStorage` 而不是写入文章或后端偏好。
 * 与 `usePreviewStyles` 同一模式：读取失败或取值非法时静默退回默认值，
 * 不让损坏的本地状态卡住界面。
 *
 * **面板比例不在这里**：三段式与编辑/预览的分隔条比例由 reka-ui 的
 * `SplitterGroup` 以 `autoSaveId` 自行持久化（见 `App.vue`），两处各存一份
 * 会立刻产生不一致的两个「当前比例」。
 */
import { ref, watch } from 'vue'

/** 编辑区视图：编辑 / 双屏 / 预览。 */
export type ViewMode = 'edit' | 'split' | 'preview'

const VIEW_KEY = 'guanlanzhi.viewMode'
const COLLAPSED_KEY = 'guanlanzhi.listCollapsed'

/** 默认视图：双屏（与施工单的三段式布局一致）。 */
export const DEFAULT_VIEW_MODE: ViewMode = 'split'

function loadViewMode(): ViewMode {
  if (typeof localStorage === 'undefined') return DEFAULT_VIEW_MODE
  try {
    const raw = localStorage.getItem(VIEW_KEY)
    // 窄屏会在启动时把 split 降级为 edit，因此这里允许三个取值原样恢复。
    if (raw === 'edit' || raw === 'split' || raw === 'preview') return raw
    return DEFAULT_VIEW_MODE
  } catch {
    return DEFAULT_VIEW_MODE
  }
}

function loadCollapsed(): boolean {
  if (typeof localStorage === 'undefined') return false
  try {
    return localStorage.getItem(COLLAPSED_KEY) === 'true'
  } catch {
    return false
  }
}

function persist(key: string, value: string): void {
  if (typeof localStorage === 'undefined') return
  try {
    localStorage.setItem(key, value)
  } catch {
    // 存储不可用（隐私模式、配额）不影响界面本身。
  }
}

export function useShellLayout() {
  const viewMode = ref<ViewMode>(loadViewMode())
  const listCollapsed = ref(loadCollapsed())

  watch(viewMode, (value) => persist(VIEW_KEY, value))
  watch(listCollapsed, (value) => persist(COLLAPSED_KEY, String(value)))

  return { viewMode, listCollapsed }
}

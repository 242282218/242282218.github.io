/**
 * 即时预览的显示样式（§3.4）。
 *
 * 站点 `.prose` 的基准是 16px 字号、2.05 行高、21px 段落下边距；软件里原先的
 * `lineHeight=175` 是**旧编辑偏好，不是站点默认**。因此这里用 `null` 明确表示
 * 「跟随站点基准」，与「用户显式选了 16px」区分开——后者在本机偏好里是一份
 * 显式覆盖，前者会随站点快照更新而变化。
 *
 * 这些值只影响软件内的即时预览，绝不写入文章或网站 CSS。
 */
import { ref, watch } from 'vue'

/** 预览列宽：站点桌面列，或 375px 手机视口模拟。 */
export type PreviewWidth = 'site' | 'mobile'

export type PreviewStyles = {
  /** `null` 表示使用站点快照的字号。 */
  fontSize: number | null
  /** `null` 表示使用站点快照的行高。 */
  lineHeight: number | null
  /** `1` 即站点原貌；其它值为本机缩放。 */
  paragraphSpacingScale: number
  width: PreviewWidth
}

/** 站点 `.prose` 的基准值，与 `src/styles/global.css` 对齐（由快照测试守护）。 */
export const SITE_PROSE_BASELINE = {
  fontSizePx: 16,
  lineHeight: 2.05,
  paragraphMarginPx: 21,
  /** `.article` 的最大宽度：真实桌面页的阅读列。 */
  articleMaxWidthPx: 760,
  /**
   * `.shell` 的最大宽度：桌面页的**视口**基准。
   *
   * 预览 iframe 必须按视口宽度取值，而不是把阅读列宽当成视口——站点的
   * `@media (max-width: 780px)` 是按视口判断的，iframe 只有 760px 时会命中
   * 平板规则（`.shell` 与 `.article` 内边距都变），桌面预览因此与真实页面不同。
   */
  shellMaxWidthPx: 1000,
} as const

const STORAGE_KEY = 'guanlanzhi.previewStyles'

export const DEFAULT_PREVIEW_STYLES: PreviewStyles = {
  fontSize: null,
  lineHeight: null,
  paragraphSpacingScale: 1,
  width: 'site',
}

/** 从本机存储恢复（损坏时退回默认值）。 */
function load(): PreviewStyles {
  if (typeof localStorage === 'undefined') return { ...DEFAULT_PREVIEW_STYLES }
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return { ...DEFAULT_PREVIEW_STYLES }
    const parsed = JSON.parse(raw) as Partial<PreviewStyles>
    return normalize(parsed)
  } catch {
    return { ...DEFAULT_PREVIEW_STYLES }
  }
}

/** 收敛到合法取值：非法输入不得让预览显示成站点没有的样子。 */
function normalize(input: Partial<PreviewStyles>): PreviewStyles {
  const fontSize =
    typeof input.fontSize === 'number' && input.fontSize >= 12 && input.fontSize <= 24
      ? input.fontSize
      : null
  const lineHeight =
    typeof input.lineHeight === 'number' && input.lineHeight >= 1.2 && input.lineHeight <= 2.4
      ? input.lineHeight
      : null
  const scale =
    typeof input.paragraphSpacingScale === 'number' &&
    input.paragraphSpacingScale >= 0.5 &&
    input.paragraphSpacingScale <= 2
      ? input.paragraphSpacingScale
      : 1
  const width: PreviewWidth = input.width === 'mobile' ? 'mobile' : 'site'
  return { fontSize, lineHeight, paragraphSpacingScale: scale, width }
}

/**
 * 是否仍处于站点原貌（没有任何本机覆盖）。
 *
 * 抽成纯函数并导出，让「还原站点基准」按钮的禁用条件与预览实际使用的判断同源：
 * 两处各写一份同样的比较，改了一处就会与真实行为不一致。
 */
export function isSiteDefaultStyles(styles: PreviewStyles): boolean {
  return (
    styles.fontSize === null &&
    styles.lineHeight === null &&
    styles.paragraphSpacingScale === 1 &&
    styles.width === 'site'
  )
}

export function usePreviewStyles() {
  const styles = ref<PreviewStyles>(load())

  watch(
    styles,
    (value) => {
      if (typeof localStorage === 'undefined') return
      try {
        localStorage.setItem(STORAGE_KEY, JSON.stringify(value))
      } catch {
        // 存储不可用（隐私模式等）不影响预览本身。
      }
    },
    { deep: true },
  )

  /**
   * 覆盖值由 `InstantPreview.vue` 以**字面量**写进预览文档的覆盖规则里。
   *
   * 这里不产出 CSS 变量：iframe 是独立文档，自定义属性不跨文档边界，
   * 定义在父文档的变量在预览里读不到（`var()` 会静默退化成 fallback）。
   * 消费点见 `InstantPreview.overrideCss`，由 `preview-style-drawer.test.ts` 守护。
   */
  return { styles }
}

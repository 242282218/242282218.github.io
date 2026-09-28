/**
 * 外壳配色（§3.5-4）。
 *
 * 默认**跟随系统**，也可显式钉成 light / dark。深色变量在 `theme.css` 的
 * `:root.dark` 里；这里只负责切换根元素上的 `dark` 类。
 *
 * 只作用于软件外壳：站点预览始终浅色（网站只有 light 主题，跟着变黑会得到
 * 一个网站上不存在的观感）。因此预览所在的 iframe 文档不在此处受影响。
 *
 * 取值来自本机偏好（`WritingPreferences.shellTheme`），与其余外观偏好同一条
 * 持久化通道，不额外引入第二份状态。
 */
import { onBeforeUnmount, watch, type Ref } from 'vue'
import type { ShellTheme } from '@/types/article'

/** 系统是否偏好深色。 */
const DARK_QUERY = '(prefers-color-scheme: dark)'

function systemPrefersDark(): boolean {
  if (typeof window === 'undefined' || !window.matchMedia) return false
  return window.matchMedia(DARK_QUERY).matches
}

/** 把 `theme` 解析成实际生效的深色与否。 */
export function resolveDark(theme: ShellTheme): boolean {
  if (theme === 'dark') return true
  if (theme === 'light') return false
  return systemPrefersDark()
}

/** 立即把当前主题写到根元素上。 */
export function applyShellTheme(theme: ShellTheme): void {
  if (typeof document === 'undefined') return
  document.documentElement.classList.toggle('dark', resolveDark(theme))
}

/**
 * 跟随偏好变化应用主题，并在「跟随系统」时监听系统配色切换。
 *
 * 偏好尚未加载（未连接后端）时按默认值 `system` 处理，因此首帧就是
 * 用户系统的配色，不会先闪一下浅色。
 */
export function useShellTheme(theme: Ref<ShellTheme>): void {
  const media = typeof window !== 'undefined' && window.matchMedia ? window.matchMedia(DARK_QUERY) : null

  // 系统配色变化只在「跟随系统」时需要重新应用。
  const onSystemChange = (): void => {
    if (theme.value === 'system') applyShellTheme('system')
  }

  media?.addEventListener('change', onSystemChange)
  watch(theme, (value) => applyShellTheme(value), { immediate: true })

  onBeforeUnmount(() => {
    media?.removeEventListener('change', onSystemChange)
  })
}

/**
 * 在挂载前按默认值先应用一次。
 *
 * 偏好要等后端返回才拿得到，若只依赖 `useShellTheme` 的 `immediate`，
 * 首帧会先按浅色渲染再跳成深色。这里在 `main.ts` 里先把默认值（跟随系统）
 * 写上去，避免闪一下白底。
 */
export function applyInitialShellTheme(): void {
  applyShellTheme('system')
}

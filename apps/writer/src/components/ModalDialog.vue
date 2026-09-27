<script setup lang="ts">
/**
 * 模态对话框外壳：统一提供遮罩、`role="dialog"`、`aria-modal`、Esc 关闭、
 * 焦点陷阱与打开时的初始聚焦。
 *
 * 通过 `<Teleport to="body">` 把遮罩移出 `#app`，这样对 `#app` 加 `inert`
 * 只会屏蔽背景内容，不会把对话框自身也屏蔽掉；键盘与读屏都无法 Tab 到遮罩后。
 */
import { onBeforeUnmount, onMounted, ref } from 'vue'

const props = defineProps<{
  /** 可访问名称（对话标题）。 */
  label: string
  /** 打开时优先聚焦的元素选择器；缺省聚焦容器内首个可聚焦元素。 */
  initialFocus?: string
}>()

const emit = defineEmits<{ (event: 'close'): void }>()

const container = ref<HTMLElement | null>(null)
/** 打开前的焦点元素，关闭后归还，避免焦点丢失。 */
let previouslyFocused: HTMLElement | null = null

const FOCUSABLE = [
  'a[href]',
  'button:not([disabled])',
  'input:not([disabled])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',')

function focusable(): HTMLElement[] {
  const root = container.value
  if (!root) return []
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (el) => !el.hasAttribute('disabled'),
  )
}

function handleKeydown(event: KeyboardEvent): void {
  if (event.key === 'Escape') {
    event.preventDefault()
    emit('close')
    return
  }
  if (event.key !== 'Tab') return
  // 焦点陷阱：把 Tab 循环限制在对话框内部。
  const items = focusable()
  if (items.length === 0) {
    event.preventDefault()
    container.value?.focus()
    return
  }
  const first = items[0]!
  const last = items[items.length - 1]!
  const active = document.activeElement as HTMLElement | null
  const inside = container.value?.contains(active) ?? false
  if (event.shiftKey && (!inside || active === first)) {
    event.preventDefault()
    last.focus()
  } else if (!event.shiftKey && (!inside || active === last)) {
    event.preventDefault()
    first.focus()
  }
}

onMounted(() => {
  previouslyFocused = document.activeElement as HTMLElement | null
  // 遮罩已被 Teleport 到 body，因此这里对应用根加 inert 只影响背景。
  document.getElementById('app')?.setAttribute('inert', '')
  window.addEventListener('keydown', handleKeydown)

  const root = container.value
  if (!root) return
  const preferred = props.initialFocus ? root.querySelector<HTMLElement>(props.initialFocus) : null
  const target = preferred ?? focusable()[0] ?? root
  target.focus()
})

onBeforeUnmount(() => {
  window.removeEventListener('keydown', handleKeydown)
  document.getElementById('app')?.removeAttribute('inert')
  previouslyFocused?.focus?.()
})
</script>

<template>
  <Teleport to="body">
    <div class="overlay" @click.self="emit('close')">
      <section
        ref="container"
        class="dialog panel"
        role="dialog"
        aria-modal="true"
        :aria-label="label"
        tabindex="-1"
      >
        <slot />
      </section>
    </div>
  </Teleport>
</template>

<style scoped>
.overlay {
  position: fixed;
  inset: 0;
  background: rgba(19, 26, 38, 0.35);
  display: flex;
  align-items: flex-start;
  justify-content: center;
  padding: 48px 16px;
  overflow-y: auto;
  z-index: 50;
}

.dialog {
  width: min(720px, 100%);
  padding: 20px;
  display: flex;
  flex-direction: column;
  gap: 12px;
  box-shadow: var(--gl-shadow-lg);
}

.dialog:focus {
  /* 容器本身可编程聚焦，但不显示焦点环（焦点环留给内部控件）。 */
  outline: none;
}
</style>

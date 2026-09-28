<script setup lang="ts">
/**
 * 编辑器右键菜单（§3.2-2：插入 / 文本格式 / 标题 / 导出 Markdown）。
 *
 * 与菜单栏、斜杠命令共用 `@/editor/commands` 的命令函数，
 * 三个入口的行为因此保持一致。
 */
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { CONTEXT_MENU_SECTIONS } from '@/editor/commands'
import type { MarkdownEditorHandle } from '@/editor/codemirror'

const props = defineProps<{
  x: number
  y: number
  handle: MarkdownEditorHandle | null
}>()

const emit = defineEmits<{
  (event: 'close'): void
  (event: 'export-markdown'): void
}>()

const rootRef = ref<HTMLElement | null>(null)

/** 把菜单收进视口：靠近右/下边缘时向左上偏移。 */
const position = ref({ left: props.x, top: props.y })

onMounted(() => {
  const element = rootRef.value
  if (!element) return
  const rect = element.getBoundingClientRect()
  const left = Math.min(props.x, window.innerWidth - rect.width - 8)
  const top = Math.min(props.y, window.innerHeight - rect.height - 8)
  position.value = { left: Math.max(8, left), top: Math.max(8, top) }
})

function run(item: { run: (view: import('@codemirror/view').EditorView) => boolean }): void {
  if (props.handle) item.run(props.handle.view)
  emit('close')
}

function handleKeydown(event: KeyboardEvent): void {
  if (event.key === 'Escape') emit('close')
}

function handleDocumentClick(event: MouseEvent): void {
  if (rootRef.value && !rootRef.value.contains(event.target as Node)) {
    emit('close')
  }
}

onMounted(() => {
  window.addEventListener('keydown', handleKeydown)
  // 延后一拍，避免打开菜单的那次右键/左键立刻把它关掉。
  window.setTimeout(() => document.addEventListener('click', handleDocumentClick), 0)
})

onBeforeUnmount(() => {
  window.removeEventListener('keydown', handleKeydown)
  document.removeEventListener('click', handleDocumentClick)
})
</script>

<template>
  <div
    ref="rootRef"
    class="context-menu"
    :style="{ left: `${position.left}px`, top: `${position.top}px` }"
    role="menu"
    aria-label="编辑器菜单"
  >
    <template v-for="section in CONTEXT_MENU_SECTIONS" :key="section.label">
      <p class="section-label">{{ section.label }}</p>
      <button
        v-for="item in section.items"
        :key="item.id"
        type="button"
        role="menuitem"
        class="menu-item"
        @click="run(item)"
      >
        <span>{{ item.label }}</span>
        <span v-if="item.hint" class="hint mono">{{ item.hint }}</span>
      </button>
    </template>
    <p class="section-label">导出</p>
    <button type="button" role="menuitem" class="menu-item" @click="emit('export-markdown')">
      <span>导出 Markdown 文件</span>
    </button>
  </div>
</template>

<style scoped>
.context-menu {
  position: fixed;
  z-index: 50;
  min-width: 200px;
  padding: 6px;
  background: var(--gl-surface);
  border: 1px solid var(--gl-border-strong);
  border-radius: var(--gl-radius-sm);
  box-shadow: var(--gl-shadow-lg);
}

.section-label {
  margin: 6px 8px 2px;
  font-size: 11px;
  color: var(--gl-text-subtle);
}

.menu-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  width: 100%;
  min-height: 32px;
  padding: 4px 8px;
  background: transparent;
  border: none;
  border-radius: 4px;
  text-align: left;
  font-size: 13px;
  color: var(--gl-text);
  cursor: pointer;
}

.menu-item:hover {
  background: var(--gl-accent-soft);
}

.hint {
  color: var(--gl-text-subtle);
  font-size: 11px;
}
</style>

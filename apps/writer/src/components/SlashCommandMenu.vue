<script setup lang="ts">
/**
 * `/` 斜杠命令菜单。
 *
 * 键盘 ↑↓ 选择、Enter 执行、Esc 关闭；鼠标点击等效。分组来自
 * `@/editor/commands`，与菜单栏、右键菜单共用同一批命令。
 */
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { SLASH_GROUPS } from '@/editor/commands'
import { deleteSlashTrigger, type MarkdownEditorHandle } from '@/editor/codemirror'

const props = defineProps<{
  handle: MarkdownEditorHandle | null
}>()

const emit = defineEmits<{
  (event: 'close'): void
  (event: 'done'): void
}>()

const query = ref('')
const activeIndex = ref(0)
const rootRef = ref<HTMLElement | null>(null)

/** 拉平的候选项（保留所属分组用于展示）。 */
const flatItems = computed(() =>
  SLASH_GROUPS.flatMap((group) => group.items.map((item) => ({ group: group.label, ...item }))),
)

const filtered = computed(() => {
  const needle = query.value.trim().toLowerCase()
  if (!needle) return flatItems.value
  return flatItems.value.filter(
    (item) => item.label.toLowerCase().includes(needle) || item.id.includes(needle),
  )
})

function choose(index: number): void {
  const item = filtered.value[index]
  if (!item || !props.handle) return
  // 先清掉过滤词再执行命令：它确实写进了正文，不清理就会在插入的块级内容旁
  // 残留「表」这类垃圾文本（`/` 本身已被编辑器挡住，不会进正文）。
  deleteSlashTrigger(props.handle.view, query.value)
  item.run(props.handle.view)
  emit('done')
}

/**
 * 键盘导航。
 *
 * 捕获阶段处理，确保上下键不会先被编辑器拿去移动光标。
 */
function handleKeydown(event: KeyboardEvent): void {
  if (event.key === 'Escape') {
    event.preventDefault()
    emit('close')
    return
  }
  if (event.key === 'ArrowDown') {
    event.preventDefault()
    activeIndex.value = Math.min(activeIndex.value + 1, filtered.value.length - 1)
    return
  }
  if (event.key === 'ArrowUp') {
    event.preventDefault()
    activeIndex.value = Math.max(activeIndex.value - 1, 0)
    return
  }
  if (event.key === 'Enter') {
    event.preventDefault()
    choose(activeIndex.value)
    return
  }
  // 退格：先缩短过滤词，减到空再关闭，避免留下一个永远开着的空菜单。
  //
  // 这里**不** `preventDefault`：退格同时删掉正文里的过滤词字符，与 query 保持同步。
  // 若只更新 query 而不让编辑器删除，正文与筛选词就会对不上（残留字符无法清理）。
  if (event.key === 'Backspace') {
    if (query.value === '') {
      emit('close')
      return
    }
    const next = [...query.value]
    next.pop()
    query.value = next.join('')
    activeIndex.value = 0
    return
  }
  // 单字符输入作为过滤词（不阻止默认行为，编辑器里也会出现该字符，
  // 但斜杠命令插入的是块级内容，执行前会由 `deleteSlashTrigger` 清理）。
  if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
    query.value += event.key
    activeIndex.value = 0
  }
}

onMounted(() => {
  window.addEventListener('keydown', handleKeydown, true)
})

onBeforeUnmount(() => {
  window.removeEventListener('keydown', handleKeydown, true)
})

/** 点击菜单外部关闭。 */
function handleDocumentClick(event: MouseEvent): void {
  if (rootRef.value && !rootRef.value.contains(event.target as Node)) {
    emit('close')
  }
}

onMounted(() => {
  // 延后一拍注册，避免触发本菜单的那次点击立刻把它关掉。
  window.setTimeout(() => document.addEventListener('click', handleDocumentClick), 0)
})

onBeforeUnmount(() => {
  document.removeEventListener('click', handleDocumentClick)
})
</script>

<template>
  <div ref="rootRef" class="slash-menu" role="dialog" aria-label="斜杠命令">
    <p class="slash-hint">
      <span class="mono">/</span>
      <span v-if="query" class="mono">{{ query }}</span>
      <span v-else class="subtle">选择要插入的内容 · ↑↓ 选择 · Enter 确认 · Esc 关闭</span>
    </p>
    <ul v-if="filtered.length > 0" class="slash-list">
      <li v-for="(item, index) in filtered" :key="`${item.group}-${item.id}`">
        <button
          type="button"
          :class="['slash-item', { active: index === activeIndex }]"
          @mouseenter="activeIndex = index"
          @click="choose(index)"
        >
          <span class="group">{{ item.group }}</span>
          <span>{{ item.label }}</span>
        </button>
      </li>
    </ul>
    <p v-else class="subtle slash-empty">没有匹配的命令</p>
  </div>
</template>

<style scoped>
.slash-menu {
  position: absolute;
  left: 16px;
  bottom: 44px;
  z-index: 40;
  width: 260px;
  max-height: 320px;
  overflow-y: auto;
  background: var(--gl-surface);
  border: 1px solid var(--gl-border-strong);
  border-radius: var(--gl-radius-sm);
  box-shadow: var(--gl-shadow-lg);
  padding: 8px;
}

.slash-hint {
  margin: 0 0 6px;
  padding: 0 4px;
  font-size: 12px;
}

.slash-list {
  list-style: none;
  margin: 0;
  padding: 0;
}

.slash-item {
  display: flex;
  align-items: center;
  gap: 8px;
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

.slash-item:hover,
.slash-item.active {
  background: var(--gl-accent-soft);
}

.slash-item .group {
  color: var(--gl-text-subtle);
  font-size: 12px;
}

.slash-empty {
  margin: 4px;
  font-size: 12px;
}
</style>

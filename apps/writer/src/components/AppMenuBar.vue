<script setup lang="ts">
/**
 * 顶部菜单栏（D10：文件 / 编辑 / 格式 / 插入 / 样式 / 帮助）。
 *
 * 照搬参考实现的形态：**没有图标工具栏**，格式化与插入全部走菜单 +
 * `/` 斜杠命令 + 右键菜单。
 *
 * 远端操作（同步 / 发布 / 撤下 / 删除）刻意**不**进菜单深处，而是留在
 * 右侧主操作区——这是本软件的生命线，不该被埋起来。
 */
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import {
  FORMAT_COMMANDS,
  INSERT_COMMANDS,
  LINK_COMMAND,
  type EditorCommand,
} from '@/editor/commands'

const props = defineProps<{
  /** 有文章打开时才允许编辑类操作。 */
  hasArticle: boolean
  /** 当前视图：编辑 / 双屏 / 预览。 */
  view: 'edit' | 'split' | 'preview'
  /** 窄屏只保留编辑与预览。 */
  narrow: boolean
}>()

const emit = defineEmits<{
  /** 执行一条编辑器命令。 */
  (event: 'command', command: EditorCommand): void
  (event: 'new-article'): void
  (event: 'import-article'): void
  (event: 'open-trash'): void
  (event: 'save'): void
  (event: 'export-markdown'): void
  (event: 'refresh-remote'): void
  (event: 'open-preferences'): void
  (event: 'open-style-drawer'): void
  (event: 'undo'): void
  (event: 'redo'): void
  (event: 'copy-markdown'): void
  (event: 'copy-plaintext'): void
  (event: 'find'): void
  (event: 'replace'): void
  (event: 'goto-line'): void
  (event: 'clear-content'): void
  (event: 'insert-image'): void
  (event: 'update:view', value: 'edit' | 'split' | 'preview'): void
  (event: 'show-shortcuts'): void
  (event: 'show-syntax-help'): void
  (event: 'show-about'): void
}>()

type MenuItem =
  | { kind: 'command'; command: EditorCommand }
  | { kind: 'action'; label: string; hint?: string; event: string; disabled?: boolean }
  | { kind: 'separator' }
  | { kind: 'view'; label: string; value: 'edit' | 'split' | 'preview' }

type Menu = { id: string; label: string; items: MenuItem[] }

const openMenu = ref<string | null>(null)
const rootRef = ref<HTMLElement | null>(null)
/** 窄屏收成一个汉堡菜单。 */
const narrowOpen = ref(false)

const canEdit = computed(() => props.hasArticle)

/**
 * 菜单结构。
 *
 * 「编辑」里的格式化相关项只在**不会意外改写文章原文**的前提下提供：
 * 撤销/重做由 CodeMirror 的历史扩展负责，清空需要明确确认且可撤销。
 */
const menus = computed<Menu[]>(() => [
  {
    id: 'file',
    label: '文件',
    items: [
      { kind: 'action', label: '新建文章…', event: 'new-article' },
      { kind: 'action', label: '从本地导入…', event: 'import-article' },
      { kind: 'action', label: '最近删除…', event: 'open-trash' },
      { kind: 'separator' },
      { kind: 'action', label: '保存', hint: 'Ctrl+S', event: 'save', disabled: !canEdit.value },
      { kind: 'action', label: '导出 Markdown 文件…', event: 'export-markdown', disabled: !canEdit.value },
      { kind: 'action', label: '刷新远端状态', event: 'refresh-remote', disabled: !canEdit.value },
      { kind: 'separator' },
      { kind: 'action', label: '偏好设置…', event: 'open-preferences' },
    ],
  },
  {
    id: 'edit',
    label: '编辑',
    items: [
      { kind: 'action', label: '撤销', hint: 'Ctrl+Z', event: 'undo', disabled: !canEdit.value },
      { kind: 'action', label: '重做', hint: 'Ctrl+Y', event: 'redo', disabled: !canEdit.value },
      { kind: 'separator' },
      { kind: 'action', label: '复制 Markdown', event: 'copy-markdown', disabled: !canEdit.value },
      { kind: 'action', label: '复制纯文本', event: 'copy-plaintext', disabled: !canEdit.value },
      { kind: 'separator' },
      { kind: 'action', label: '查找', hint: 'Ctrl+F', event: 'find', disabled: !canEdit.value },
      { kind: 'action', label: '替换', hint: 'Ctrl+H', event: 'replace', disabled: !canEdit.value },
      { kind: 'action', label: '跳转到行', hint: 'Ctrl+G', event: 'goto-line', disabled: !canEdit.value },
      { kind: 'separator' },
      { kind: 'action', label: '清空正文…', event: 'clear-content', disabled: !canEdit.value },
    ],
  },
  {
    id: 'format',
    label: '格式',
    items: [
      ...FORMAT_COMMANDS.slice(0, 4).map((command) => ({ kind: 'command' as const, command })),
      { kind: 'command' as const, command: LINK_COMMAND },
      { kind: 'separator' as const },
      // 标题 1–6。
      ...FORMAT_COMMANDS.slice(4, 10).map((command) => ({ kind: 'command' as const, command })),
      { kind: 'separator' as const },
      // 列表、引用、分隔线。
      ...FORMAT_COMMANDS.slice(10).map((command) => ({ kind: 'command' as const, command })),
    ],
  },
  {
    id: 'insert',
    label: '插入',
    items: [
      { kind: 'action', label: '图片…', event: 'insert-image', disabled: !canEdit.value },
      ...INSERT_COMMANDS.map((command) => ({ kind: 'command' as const, command })),
    ],
  },
  {
    id: 'style',
    label: '样式',
    items: [{ kind: 'action', label: '预览样式抽屉…', event: 'open-style-drawer' }],
  },
  {
    id: 'help',
    label: '帮助',
    items: [
      { kind: 'action', label: '键盘快捷键', event: 'show-shortcuts' },
      { kind: 'action', label: 'Markdown 语法帮助', event: 'show-syntax-help' },
      { kind: 'separator' },
      { kind: 'action', label: '关于', event: 'show-about' },
    ],
  },
])

function toggleMenu(id: string): void {
  openMenu.value = openMenu.value === id ? null : id
}

const viewItems = computed<MenuItem[]>(() => [
  { kind: 'view', label: '编辑', value: 'edit' },
  ...(props.narrow
    ? []
    : [{ kind: 'view' as const, label: '双屏', value: 'split' as const }]),
  { kind: 'view', label: '预览', value: 'preview' },
])

function activate(item: MenuItem): void {
  openMenu.value = null
  narrowOpen.value = false
  if (item.kind === 'command') {
    emit('command', item.command)
    return
  }
  if (item.kind === 'view') {
    emit('update:view', item.value)
    return
  }
  if (item.kind !== 'action') return
  switch (item.event) {
    case 'new-article': emit('new-article'); break
    case 'import-article': emit('import-article'); break
    case 'open-trash': emit('open-trash'); break
    case 'save': emit('save'); break
    case 'export-markdown': emit('export-markdown'); break
    case 'refresh-remote': emit('refresh-remote'); break
    case 'open-preferences': emit('open-preferences'); break
    case 'open-style-drawer': emit('open-style-drawer'); break
    case 'undo': emit('undo'); break
    case 'redo': emit('redo'); break
    case 'copy-markdown': emit('copy-markdown'); break
    case 'copy-plaintext': emit('copy-plaintext'); break
    case 'find': emit('find'); break
    case 'replace': emit('replace'); break
    case 'goto-line': emit('goto-line'); break
    case 'clear-content': emit('clear-content'); break
    case 'insert-image': emit('insert-image'); break
    case 'show-shortcuts': emit('show-shortcuts'); break
    case 'show-syntax-help': emit('show-syntax-help'); break
    case 'show-about': emit('show-about'); break
  }
}

function handleDocumentClick(event: MouseEvent): void {
  if (rootRef.value && !rootRef.value.contains(event.target as Node)) {
    openMenu.value = null
    narrowOpen.value = false
  }
}

function handleKeydown(event: KeyboardEvent): void {
  if (event.key === 'Escape') {
    openMenu.value = null
    narrowOpen.value = false
  }
}

onMounted(() => {
  document.addEventListener('click', handleDocumentClick)
  window.addEventListener('keydown', handleKeydown)
})

onBeforeUnmount(() => {
  document.removeEventListener('click', handleDocumentClick)
  window.removeEventListener('keydown', handleKeydown)
})

defineExpose({ close: () => { openMenu.value = null; narrowOpen.value = false } })
</script>

<template>
  <nav ref="rootRef" class="menu-bar" aria-label="主菜单">
    <!-- 窄屏：收成一个汉堡菜单 -->
    <button
      v-if="narrow"
      type="button"
      class="hamburger"
      :aria-expanded="narrowOpen"
      aria-haspopup="menu"
      @click="narrowOpen = !narrowOpen"
    >
      ☰ 菜单
    </button>

    <ul v-if="!narrow || narrowOpen" class="menu-list" role="menubar">
      <li v-for="menu in menus" :key="menu.id" class="menu-root">
        <button
          type="button"
          role="menuitem"
          :aria-expanded="openMenu === menu.id"
          aria-haspopup="menu"
          :class="['menu-title', { open: openMenu === menu.id }]"
          @click="toggleMenu(menu.id)"
          @mouseenter="openMenu ? (openMenu = menu.id) : undefined"
        >
          {{ menu.label }}
        </button>
        <ul v-if="openMenu === menu.id" class="menu-popup" role="menu">
          <template v-for="(item, index) in menu.items" :key="index">
            <li v-if="item.kind === 'separator'" class="menu-separator" role="separator" />
            <li v-else role="none">
              <button
                type="button"
                role="menuitem"
                class="menu-entry"
                :disabled="item.kind === 'action' ? (item.disabled ?? false) : false"
                @click="activate(item)"
              >
                <span>{{ item.kind === 'command' ? item.command.label : item.label }}</span>
                <span
                  v-if="(item.kind === 'command' && item.command.hint) || (item.kind === 'action' && item.hint)"
                  class="hint mono"
                >
                  {{ item.kind === 'command' ? item.command.hint : item.hint }}
                </span>
              </button>
            </li>
          </template>
        </ul>
      </li>
      <li class="menu-root view-switch" role="none">
        <span class="view-label">视图</span>
        <button
          v-for="item in viewItems"
          :key="item.kind === 'view' ? item.value : 'unknown'"
          type="button"
          class="view-button"
          :class="{ active: item.kind === 'view' && item.value === view }"
          @click="activate(item)"
        >
          {{ item.kind === 'view' ? item.label : '' }}
        </button>
      </li>
    </ul>
  </nav>
</template>

<style scoped>
.menu-bar {
  position: relative;
  display: flex;
  align-items: center;
  gap: 4px;
  height: var(--gl-header, 60px);
  padding: 0 8px;
  background: var(--gl-surface);
  border-bottom: 1px solid var(--gl-border);
}

.menu-list {
  display: flex;
  align-items: center;
  gap: 2px;
  list-style: none;
  margin: 0;
  padding: 0;
  flex: 1;
  min-width: 0;
}

.menu-root {
  position: relative;
}

.menu-title {
  min-height: 32px;
  padding: 0 12px;
  background: transparent;
  border: none;
  border-radius: 6px;
  font-size: 13px;
  color: var(--gl-text);
  cursor: pointer;
}

.menu-title:hover,
.menu-title.open {
  background: var(--gl-surface-muted);
}

.hamburger {
  min-height: 36px;
  padding: 0 12px;
  border: 1px solid var(--gl-border);
  border-radius: 6px;
  background: var(--gl-surface);
  font-size: 13px;
  cursor: pointer;
}

.menu-popup {
  position: absolute;
  top: calc(100% + 4px);
  left: 0;
  z-index: 60;
  min-width: 220px;
  margin: 0;
  padding: 6px;
  list-style: none;
  background: var(--gl-surface);
  border: 1px solid var(--gl-border-strong);
  border-radius: 8px;
  box-shadow: var(--gl-shadow-lg);
}

.menu-entry {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  width: 100%;
  min-height: 32px;
  padding: 4px 10px;
  background: transparent;
  border: none;
  border-radius: 4px;
  text-align: left;
  font-size: 13px;
  color: var(--gl-text);
  cursor: pointer;
}

.menu-entry:hover:not(:disabled) {
  background: var(--gl-accent-soft);
}

.menu-entry:disabled {
  color: var(--gl-text-subtle);
  cursor: not-allowed;
}

.hint {
  color: var(--gl-text-subtle);
  font-size: 11px;
}

.menu-separator {
  height: 1px;
  margin: 4px 6px;
  background: var(--gl-border);
}

.view-switch {
  display: flex;
  align-items: center;
  gap: 4px;
  margin-left: auto;
}

.view-label {
  font-size: 12px;
  color: var(--gl-text-subtle);
  margin-right: 4px;
}

.view-button {
  min-height: 28px;
  padding: 0 10px;
  background: transparent;
  border: 1px solid var(--gl-border);
  border-radius: 6px;
  font-size: 12px;
  color: var(--gl-text-muted);
  cursor: pointer;
}

.view-button.active {
  background: var(--gl-accent-soft);
  border-color: var(--gl-accent);
  color: var(--gl-accent);
}
</style>

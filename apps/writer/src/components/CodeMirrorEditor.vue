<script setup lang="ts">
/**
 * CodeMirror 6 编辑器组件。
 *
 * 与 retired 的 Vditor 版本相比，这里编辑的是**原文**：不重排 Markdown，
 * 因此「打开 → 不编辑 → 保存」能保持磁盘字节不变。
 *
 * 组件只负责正文读写、斜杠命令与右键菜单的触发；菜单栏在 `App.vue`，
 * 但三条入口调用的是同一批命令函数（`@/editor/commands`）。
 *
 * 图片来源（粘贴、拖入）统一交给父组件通过后端归档，本组件不直连后端。
 */
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { createMarkdownEditor, type MarkdownEditorHandle } from '@/editor/codemirror'
import { dragHasFiles, extractImage } from '@/services/imageDrop'
import SlashCommandMenu from '@/components/SlashCommandMenu.vue'
import EditorContextMenu from '@/components/EditorContextMenu.vue'

const props = defineProps<{
  modelValue: string
  fontSize: number
  /** 正文行距（倍数，如 1.75）。 */
  lineHeight?: number
  /** 只读（操作进行中）时禁用输入。 */
  disabled?: boolean
  /** 未选择文章时不允许插入图片。 */
  canInsertImage?: boolean
}>()

const emit = defineEmits<{
  (event: 'update:modelValue', value: string): void
  /** 请求插入一张来源为粘贴或拖入的图片。 */
  (event: 'pick-image', payload: { fileName: string; bytes: Uint8Array; origin: 'pasted' | 'dropped' }): void
  /** 粘贴/拖入内容无法作为图片插入时的说明。 */
  (event: 'image-rejected', reason: string): void
  /** 光标位置变化（状态栏用）。 */
  (event: 'cursor', info: { line: number; column: number; selectedChars: number }): void
  /** 输入法组合态变化（界面据此提示「正在输入」并避免打断）。 */
  (event: 'composition', info: { composing: boolean; data?: string }): void
  /** 右键菜单请求导出当前文章的 Markdown 文件（由外壳负责落盘与对话框）。 */
  (event: 'export-markdown'): void
}>()

const hostRef = ref<HTMLElement | null>(null)
const dragActive = ref(false)
const slashOpen = ref(false)
const contextMenu = ref<{ x: number; y: number } | null>(null)

let editor: MarkdownEditorHandle | null = null
/**
 * 中文输入法组合态。
 *
 * 组合期间的中间文本（拼音串、候选字）不是用户的最终内容；此时把它交给父组件
 * 会立刻标记 dirty 并排入自动保存，造成「拼音还没上屏就先存一次盘」。
 * 因此组合期间不向外发送变化，等 `compositionend` 一次性交出上屏结果。
 */
let composing = false
/** 组合期间最后一次文本，供 `compositionend` 交还。 */
let pendingCompositionValue: string | null = null
/**
 * 组合期间到达的**外部**内容（父组件换了文章）。
 *
 * 组合未结束时整体替换文档会打断输入法会话，并可能丢掉尚未上屏的拼音；因此
 * 先把外部内容挂起，等 `compositionend` 再落地。此时组合产生的文本属于**旧**
 * 文章，应当丢弃——外部内容才是用户此刻要看的那一篇。
 */
let deferredValue: string | null = null

onMounted(() => {
  if (!hostRef.value) return
  editor = createMarkdownEditor({
    parent: hostRef.value,
    value: props.modelValue,
    readonly: props.disabled,
    onChange: (value) => {
      // 组合态：只暂存，不向外发送（见 `composing` 的说明）。
      if (composing) {
        pendingCompositionValue = value
        return
      }
      emit('update:modelValue', value)
    },
    onSelectionChange: (info) => emit('cursor', info),
    onSlash: () => {
      slashOpen.value = true
      contextMenu.value = null
      return true
    },
    onContextMenu: (_view, event) => {
      event.preventDefault()
      contextMenu.value = { x: event.clientX, y: event.clientY }
      slashOpen.value = false
      return true
    },
  })
})

onBeforeUnmount(() => {
  editor?.destroy()
  editor = null
})

// 父组件切换文章时同步内容（`setValue` 自身抑制变化事件，不会触发 dirty）。
watch(
  () => props.modelValue,
  (value) => {
    if (!editor || editor.getValue() === value) return
    // 组合进行中不替换文档：等 `compositionend` 再落地，避免打断输入法会话。
    if (composing) {
      deferredValue = value
      return
    }
    editor.setValue(value)
  },
)

watch(
  () => props.disabled,
  () => {
    // CodeMirror 的只读由扩展控制，重建代价高；这里用编辑器的
    // `contenteditable` 属性切换，避免丢失滚动位置与撤销历史。
    const content = editor?.view.contentDOM
    if (!content) return
    if (props.disabled) {
      content.setAttribute('contenteditable', 'false')
    } else {
      content.setAttribute('contenteditable', 'true')
    }
  },
)

/**
 * 粘贴处理。
 *
 * 只在剪贴板里确实有图片（或不可用的图片格式）时接管；纯文本粘贴保持编辑器原生行为。
 */
async function handlePaste(event: ClipboardEvent): Promise<void> {
  if (props.disabled || !props.canInsertImage) return
  const result = await extractImage(event.clipboardData, 'pasted')
  if (result.kind === 'none') return
  event.preventDefault()
  if (result.kind === 'rejected') {
    emit('image-rejected', result.reason)
    return
  }
  emit('pick-image', result.image)
}

/** 拖入悬停：只对含文件内容的拖拽给出反馈并拦截默认行为。 */
function handleDragOver(event: DragEvent): void {
  if (props.disabled || !props.canInsertImage) return
  if (!dragHasFiles(event.dataTransfer)) return
  event.preventDefault()
  if (event.dataTransfer) {
    event.dataTransfer.dropEffect = 'copy'
  }
  dragActive.value = true
}

function handleDragLeave(event: DragEvent): void {
  // 仅当离开整个编辑器区域时才复位，避免子元素间移动导致闪烁。
  const related = event.relatedTarget as Node | null
  if (related && hostRef.value?.contains(related)) return
  dragActive.value = false
}

/**
 * 组合开始：进入保护态。
 *
 * 事件在宿主容器上以捕获方式监听，因此无论焦点落在 CodeMirror 的哪个内部
 * 元素，都会先经过这里。
 */
function handleCompositionStart(): void {
  composing = true
  pendingCompositionValue = null
  // 立刻通知外壳：它据此拒绝切篇，不能等到第一次 `compositionupdate` 才知道
  // 用户正在组字（那之间的窗口足够用户点开另一篇文章）。
  emit('composition', { composing: true })
}

/** 组合结束：交还上屏后的最终文本。 */
function handleCompositionEnd(): void {
  composing = false
  const value = pendingCompositionValue ?? editor?.getValue() ?? null
  pendingCompositionValue = null
  // 组合期间父组件换过文章：外部内容优先，组合产出的旧文章文本直接丢弃。
  if (deferredValue !== null) {
    const next = deferredValue
    deferredValue = null
    editor?.setValue(next)
    emit('composition', { composing: false })
    return
  }
  if (value !== null && value !== props.modelValue) {
    emit('update:modelValue', value)
  }
  emit('composition', { composing: false })
}

/** 组合进行中：通知父组件当前处于组合态（界面据此避免打断输入）。 */
function handleCompositionUpdate(event: CompositionEvent): void {
  emit('composition', { composing: true, data: event.data ?? '' })
}

async function handleDrop(event: DragEvent): Promise<void> {
  dragActive.value = false
  if (props.disabled || !props.canInsertImage) return
  const result = await extractImage(event.dataTransfer, 'dropped')
  if (result.kind === 'none') return
  event.preventDefault()
  if (result.kind === 'rejected') {
    emit('image-rejected', result.reason)
    return
  }
  emit('pick-image', result.image)
}

defineExpose({
  /** 打开文章时强制刷新编辑器内容（组合进行中则挂起，见 `deferredValue`）。 */
  load: (value: string) => {
    if (composing) {
      deferredValue = value
      return
    }
    editor?.setValue(value)
  },
  /** 在光标处插入文本片段（用于插入图片引用）。 */
  insertText: (snippet: string) => {
    editor?.insertText(snippet)
  },
  /** 供外壳菜单/快捷键执行的底层句柄。 */
  handle: () => editor,
})
</script>

<template>
  <div
    class="editor-host"
    :class="{ 'drag-active': dragActive }"
    :style="{ '--cm-font-size': `${fontSize}px`, '--cm-line-height': `${lineHeight ?? 1.75}` }"
    @paste="handlePaste"
    @dragover="handleDragOver"
    @dragleave="handleDragLeave"
    @drop="handleDrop"
    @compositionstart.capture="handleCompositionStart"
    @compositionupdate.capture="handleCompositionUpdate"
    @compositionend.capture="handleCompositionEnd"
  >
    <div ref="hostRef" class="cm-mount" />
    <p v-if="dragActive" class="drop-hint" role="status">松手即可插入这张图片</p>

    <SlashCommandMenu
      v-if="slashOpen"
      :handle="editor"
      @close="slashOpen = false"
      @done="slashOpen = false"
    />
    <EditorContextMenu
      v-if="contextMenu"
      :x="contextMenu.x"
      :y="contextMenu.y"
      :handle="editor"
      @export-markdown="emit('export-markdown')"
      @close="contextMenu = null"
    />
  </div>
</template>

<style scoped>
.editor-host {
  flex: 1;
  min-height: 320px;
  overflow: hidden;
  position: relative;
  display: flex;
  flex-direction: column;
}

.editor-host.drag-active {
  outline: 2px dashed var(--gl-accent);
  outline-offset: -4px;
  background: var(--gl-accent-soft);
}

.cm-mount {
  flex: 1;
  min-height: 0;
  overflow: auto;
}

/* 字号与行距来自本机偏好（只影响软件内编辑区，不写入文章或网站）。 */
.cm-mount :deep(.cm-editor) {
  height: 100%;
  font-size: var(--cm-font-size, 16px);
}

.cm-mount :deep(.cm-content) {
  font-family: var(--gl-font-mono);
  line-height: var(--cm-line-height, 1.75);
  padding: 12px 0;
}

.cm-mount :deep(.cm-editor.cm-focused) {
  outline: none;
}

.cm-mount :deep(.cm-gutters) {
  background: var(--gl-surface-muted);
  border-right: 1px solid var(--gl-border);
  color: var(--gl-text-subtle);
}

.drop-hint {
  position: absolute;
  left: 50%;
  bottom: 12px;
  transform: translateX(-50%);
  margin: 0;
  padding: 6px 12px;
  background: var(--gl-accent);
  color: #fff;
  border-radius: 999px;
  font-size: 12px;
  pointer-events: none;
}
</style>

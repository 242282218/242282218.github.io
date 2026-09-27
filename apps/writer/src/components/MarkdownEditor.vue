<script setup lang="ts">
/**
 * Vditor 适配组件：正文读写与模式切换，并承载剪贴板粘贴与拖入图片。
 *
 * front matter 由后端独立维护，编辑器只处理正文。
 * 模式切换前会等待中文输入法组合结束，并在往返结果不安全时回退源码模式。
 *
 * 图片来源（粘贴、拖入）统一交给父组件通过后端归档，本组件不直连后端。
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useVditor, type RoundTripReport } from '@/components/MarkdownEditor'
import { dragHasFiles, extractImage } from '@/services/imageDrop'
import type { EditorMode } from '@/types/article'

const props = defineProps<{
  modelValue: string
  mode: EditorMode
  fontSize: number
  /** 正文行距（百分比，如 175 表示 1.75 倍）。 */
  lineHeight?: number
  /** 代码块高亮主题（Vditor/hljs 主题名）。 */
  codeTheme?: string
  /** 即时排版预览区宽度（px）。 */
  previewWidth?: number
  /** 只读（操作进行中）时禁用输入。 */
  disabled?: boolean
  /** 未选择文章时不允许插入图片。 */
  canInsertImage?: boolean
}>()

const emit = defineEmits<{
  (event: 'update:modelValue', value: string): void
  (event: 'blocked', report: RoundTripReport): void
  (event: 'updated', report: RoundTripReport): void
  /** 请求插入一张来源为粘贴或拖入的图片。 */
  (event: 'pick-image', payload: { fileName: string; bytes: Uint8Array; origin: 'pasted' | 'dropped' }): void
  /** 粘贴/拖入内容无法作为图片插入时的说明。 */
  (event: 'image-rejected', reason: string): void
}>()

const body = ref(props.modelValue)
const mode = ref<EditorMode>(props.mode)
const hostRef = ref<HTMLElement | null>(null)
/** 拖拽悬停状态，用于给出可见反馈。 */
const dragActive = ref(false)

// 写作外观偏好的响应式镜像，交给 useVditor 消费（只影响本机显示，不写入文章）。
const fontSize = computed(() => props.fontSize)
const lineHeight = computed(() => props.lineHeight ?? 175)
const codeTheme = computed(() => props.codeTheme ?? 'github')
const previewWidth = computed(() => props.previewWidth ?? 460)

const editor = useVditor(body, mode, {
  fontSize,
  lineHeight,
  codeTheme,
  previewWidth,
  onBlocked: (report) => emit('blocked', report),
  onNormalized: (report) => emit('updated', report),
})

// 正文变化回传给父组件（父组件负责自动保存调度）。
watch(body, (value) => {
  if (value !== props.modelValue) {
    emit('update:modelValue', value)
  }
})

// 父组件切换文章时同步内容。
watch(
  () => props.modelValue,
  (value) => {
    if (value !== body.value) {
      body.value = value
      editor.load(value)
    }
  },
)

// 父组件切换模式时执行带验收的切换。
watch(
  () => props.mode,
  async (value) => {
    if (value === mode.value) return
    const report = await editor.switchMode(value)
    if (report && report.blocked) {
      emit('blocked', report)
    } else if (report) {
      emit('updated', report)
    }
  },
)

watch(
  () => props.disabled,
  (disabled) => {
    const instance = editor.instance.value
    if (!instance) return
    if (disabled) {
      instance.disabled()
    } else {
      instance.enable()
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

  // 接管本次粘贴，避免把图片的二进制内容当成文本插入正文。
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
    // 明确告知可以复制，界面据此显示「松手插入」。
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

onMounted(async () => {
  editor.host.value = hostRef.value
  await editor.createVditor()
})

onBeforeUnmount(() => {
  editor.destroy()
})

defineExpose({
  /** 打开文章时强制刷新编辑器内容。 */
  load: (value: string) => {
    body.value = value
    editor.load(value)
  },
  /** 在光标处插入文本片段（用于插入图片引用）。 */
  insertText: (snippet: string) => {
    editor.insertText(snippet)
  },
})
</script>

<template>
  <div
    class="editor-host"
    :class="{ 'drag-active': dragActive }"
    :style="{ fontSize: `${fontSize}px` }"
    @paste="handlePaste"
    @dragover="handleDragOver"
    @dragleave="handleDragLeave"
    @drop="handleDrop"
  >
    <div ref="hostRef" class="vditor-mount" />
    <p v-if="dragActive" class="drop-hint" role="status">松手即可插入这张图片</p>
  </div>
</template>

<style scoped>
.editor-host {
  flex: 1;
  min-height: 320px;
  overflow: hidden;
  position: relative;
}

.editor-host.drag-active {
  outline: 2px dashed var(--gl-accent);
  outline-offset: -4px;
  background: var(--gl-accent-soft);
}

.vditor-mount {
  height: 100%;
}

/* Vditor 自身的边框由外层容器提供，这里避免双重边框。 */
.vditor-mount :deep(.vditor) {
  border: none;
  height: 100%;
}

/* 写作外观：字号与行距来自本机偏好（CSS 变量由适配层写入）。
   Vditor 在自身元素上硬编码 16px 字号，因此必须显式覆盖到内容元素上。 */
.vditor-mount :deep(.vditor-sv),
.vditor-mount :deep(.vditor-reset),
.vditor-mount :deep(.vditor-ir .vditor-reset) {
  font-size: var(--vditor-font-size, 16px);
  line-height: var(--vditor-line-height, 1.75);
}

/* 即时排版（Vditor 自带分屏的预览区）宽度跟随本机偏好。 */
.vditor-mount :deep(.vditor-preview) {
  width: var(--vditor-preview-width, 460px);
  min-width: 0;
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

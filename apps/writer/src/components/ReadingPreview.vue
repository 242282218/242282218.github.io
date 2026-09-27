<script setup lang="ts">
/**
 * 即时排版与网站预览的侧栏容器。
 *
 * 按方案 §6.1 的要求，**不额外创建第二个预览引擎**：正文的即时排版由 Vditor
 * 自带的分屏预览承担（避免视觉与滚动状态重复）。本组件只承载：
 * - 「即时排版 / 网站预览」的清晰标签与区别说明；
 * - 写作外观的当前取值展示；
 * - 网站预览的启动与关闭入口。
 */
import { computed } from 'vue'
import type { WritingPreferences } from '@/types/article'

const props = defineProps<{
  preferences: WritingPreferences | null
  previewUrl: string | null
  previewBanner: string
  previewNotice: string | null
  starting: boolean
  /** 当前编辑模式，用于说明即时排版在哪里呈现。 */
  editorMode: 'sv' | 'ir'
}>()

const emit = defineEmits<{
  (event: 'start-preview', simulatePublic: boolean): void
  (event: 'stop-preview'): void
  (event: 'open-preview'): void
}>()

/** 说明即时排版的呈现位置（两种模式落点不同）。 */
const instantLayoutHint = computed(() =>
  props.editorMode === 'sv'
    ? '正文区右侧即为即时排版（由编辑器自带的分屏渲染），随输入实时更新。'
    : '正文即时渲染模式下，排版直接呈现在编辑区中，随输入实时更新。',
)

const appearanceSummary = computed(() => {
  const prefs = props.preferences
  if (!prefs) return '使用默认外观'
  return `字号 ${prefs.fontSize}px · 行距 ${prefs.lineHeight}% · 代码主题 ${prefs.codeTheme}`
})
</script>

<template>
  <section class="reading-preview" aria-label="即时排版与网站预览">
    <header class="preview-header">
      <strong>预览</strong>
    </header>

    <div class="preview-body">
      <section aria-label="即时排版">
        <h3 class="block-title">即时排版</h3>
        <p class="subtle">{{ instantLayoutHint }}</p>
        <p class="subtle">
          这是本机写作预览，使用与博客接近的字体、行距与代码块间距，
          但**不代表**网站在线效果。
        </p>
        <p class="subtle">当前写作外观：{{ appearanceSummary }}</p>
      </section>

      <section class="site-preview-block" aria-label="网站预览">
        <h3 class="block-title">网站预览</h3>
        <p class="subtle">
          使用最新可获取的站点代码，在隔离的临时目录中覆盖当前文章内容后启动，
          只绑定本机 127.0.0.1，不会推送、不会修改正式仓库。
        </p>

        <div class="preview-actions">
          <button type="button" :disabled="starting" @click="emit('start-preview', true)">
            以「模拟公开」预览
          </button>
          <button type="button" :disabled="starting" @click="emit('start-preview', false)">
            按草稿状态预览
          </button>
        </div>

        <div v-if="previewUrl" class="preview-live">
          <p class="banner">
            <strong>{{ previewBanner }}</strong>
            <span class="mono">{{ previewUrl }}</span>
          </p>
          <p v-if="previewNotice" class="subtle">{{ previewNotice }}</p>
          <div class="preview-actions">
            <button type="button" class="primary" @click="emit('open-preview')">
              打开预览窗口
            </button>
            <button type="button" class="ghost" @click="emit('stop-preview')">关闭预览</button>
          </div>
        </div>

        <p v-else class="subtle">
          尚未启动网站预览。启动需要本机具备 Git、Node 22.12+ 与 pnpm。
        </p>
      </section>
    </div>
  </section>
</template>

<style scoped>
.reading-preview {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  overflow: hidden;
  background: var(--gl-surface);
  border-left: 1px solid var(--gl-border);
}

.preview-header {
  display: flex;
  align-items: baseline;
  gap: 8px;
  padding: 10px 14px;
  border-bottom: 1px solid var(--gl-border);
}

.preview-body {
  padding: 14px;
  overflow-y: auto;
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 18px;
}

.block-title {
  margin: 0 0 6px;
  font-size: 13px;
  color: var(--gl-text-muted);
}

.site-preview-block {
  padding-top: 14px;
  border-top: 1px solid var(--gl-border);
}

.preview-actions {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
  margin-top: 10px;
}

.preview-live {
  margin-top: 12px;
}

.banner {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin: 0 0 8px;
  padding: 8px 10px;
  background: var(--gl-warn-soft);
  border: 1px solid var(--gl-warn);
  border-radius: var(--gl-radius-sm);
  font-size: 13px;
}

.mono {
  font-family: var(--gl-font-mono);
  font-size: 12px;
  overflow-wrap: anywhere;
}
</style>

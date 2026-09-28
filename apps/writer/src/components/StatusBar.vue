<script setup lang="ts">
/**
 * 底部状态栏。
 *
 * 显示本地 / 写作分支 / 网站三处状态、上次远端核对时间、光标位置与字数。
 * 状态语义与 `describeStatus` 同源，**不得回退**：未知绝不显示成已同步。
 */
import { computed } from 'vue'
import { describeRemoteCheckedAt, describeStatus, type ArticleStatus } from '@/types/article'

const props = defineProps<{
  status: ArticleStatus | null
  wordCount: number
  /** 当前是否正在核对远端。 */
  checking: boolean
  /** 光标位置（编辑器提供）。 */
  cursor: { line: number; column: number; selectedChars: number } | null
}>()

const summary = computed(() => (props.status ? describeStatus(props.status) : null))

/** 上次远端核对时间；未核对时明确说明，不省略。 */
const checkedAt = computed(() => describeRemoteCheckedAt(props.status?.remoteCheckedAtUnix))

const tone = computed(() => summary.value?.tone ?? 'neutral')
</script>

<template>
  <footer class="status-bar" aria-label="状态栏">
    <span class="item">
      <span class="label">本地</span>
      <span>{{ summary?.local ?? '尚未选择文章' }}</span>
    </span>
    <span class="item">
      <span class="label">写作分支</span>
      <span :class="['tone', `tone-${tone}`]">{{ summary?.remote ?? '—' }}</span>
    </span>
    <span class="item">
      <span class="label">网站</span>
      <span :class="['tone', `tone-${tone}`]">{{ summary?.site ?? '—' }}</span>
    </span>
    <span class="item">
      <span class="label">远端核对</span>
      <span>{{ checking ? '正在核对…' : checkedAt }}</span>
    </span>

    <span class="spacer" />

    <span v-if="cursor" class="item mono">
      第 {{ cursor.line }} 行 · 第 {{ cursor.column }} 列
      <template v-if="cursor.selectedChars > 0"> · 选中 {{ cursor.selectedChars }} 字</template>
    </span>
    <span class="item">全文 {{ wordCount }} 字</span>
  </footer>
</template>

<style scoped>
.status-bar {
  display: flex;
  align-items: center;
  gap: 16px;
  height: var(--gl-statusbar, 24px);
  padding: 0 12px;
  background: var(--gl-surface-muted);
  border-top: 1px solid var(--gl-border);
  font-size: 12px;
  color: var(--gl-text-muted);
  overflow: hidden;
  white-space: nowrap;
}

.item {
  display: inline-flex;
  align-items: center;
  gap: 5px;
}

.label {
  color: var(--gl-text-subtle);
}

.spacer {
  flex: 1;
}

.tone-ok {
  color: var(--gl-ok);
}

.tone-warn {
  color: var(--gl-warn);
}

.tone-danger {
  color: var(--gl-danger);
}

.tone-info {
  color: var(--gl-accent);
}
</style>

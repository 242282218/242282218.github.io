<script setup lang="ts">
/**
 * 冲突界面：本地基线／本地候选／远端当前版本的差异与动作选择。
 *
 * 约束：
 * - 图片冲突单独展示文件名、大小与内容哈希，不假称可以文本合并；
 * - 「采用本地」必须显式确认将被覆盖的确切路径与远端版本；
 * - 「采用远端」会把尚未保存的本地修改先放入恢复副本（由后端完成）。
 */
import { computed } from 'vue'
import type { ConflictSource, SyncAssessment } from '@/types/article'

const props = defineProps<{
  assessment: SyncAssessment
  /** 后端为本篇记录的最近一次同步哈希（本地基线）。 */
  baselineHash?: string | null | undefined
  busy?: boolean | undefined
}>()

const emit = defineEmits<{
  (event: 'adopt-local'): void
  (event: 'adopt-remote'): void
  (event: 'manual-merge'): void
  (event: 'close'): void
}>()

/** 本地与远端文本的逐行差异。 */
type DiffLine = { kind: 'same' | 'added' | 'removed'; left: string; right: string }

const markdownConflict = computed(() =>
  props.assessment.conflicts.some((c: ConflictSource) => c.kind === 'markdown'),
)

const imageConflicts = computed(() =>
  props.assessment.conflicts.filter(
    (c: ConflictSource): c is { kind: 'image'; relPath: string } => c.kind === 'image',
  ),
)

/** 生成简单的双栏对齐差异。 */
const diffLines = computed<DiffLine[]>(() => {
  const left = (props.assessment.writingMarkdown ?? '').split('\n')
  const right = props.assessment.localMarkdown.split('\n')
  const result: DiffLine[] = []
  const max = Math.max(left.length, right.length)
  for (let i = 0; i < max; i += 1) {
    const l = left[i]
    const r = right[i]
    if (l === r) {
      result.push({ kind: 'same', left: l ?? '', right: r ?? '' })
    } else if (l === undefined) {
      result.push({ kind: 'added', left: '', right: r ?? '' })
    } else if (r === undefined) {
      result.push({ kind: 'removed', left: l, right: '' })
    } else {
      result.push({ kind: 'removed', left: l, right: '' })
      result.push({ kind: 'added', left: '', right: r })
    }
  }
  return result
})

const differingLineCount = computed(() => diffLines.value.filter((line) => line.kind !== 'same').length)

function shortHash(hash?: string | null): string {
  if (!hash) return '（未记录）'
  return hash.slice(0, 12)
}
</script>

<template>
  <section class="sync-diff panel" aria-label="远端冲突">
    <header class="diff-header">
      <div>
        <strong>远端这篇文章已被改动</strong>
        <p class="subtle">
          软件不会自动覆盖任何一方。请选择处理方式，选择只影响这一篇文章及其图片。
        </p>
      </div>
      <button type="button" class="ghost" :disabled="busy" @click="emit('close')">
        稍后处理
      </button>
    </header>

    <dl class="hash-row">
      <div>
        <dt>本地基线</dt>
        <dd class="mono">{{ shortHash(baselineHash) }}</dd>
      </div>
      <div>
        <dt>远端当前</dt>
        <dd class="mono">{{ shortHash(assessment.writingHash) }}</dd>
      </div>
      <div>
        <dt>本地候选</dt>
        <dd class="mono">{{ shortHash(assessment.localHash) }}</dd>
      </div>
    </dl>

    <div v-if="markdownConflict" class="diff-block">
      <h3 class="diff-title">
        正文差异（<span class="error-text">{{ differingLineCount }}</span> 行不同）
      </h3>
      <div class="diff-table table-scroll">
        <table>
          <thead>
            <tr>
              <th scope="col">远端当前版本</th>
              <th scope="col">本地候选版本</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(line, index) in diffLines" :key="index" :class="line.kind">
              <td class="mono">{{ line.left || '\u00a0' }}</td>
              <td class="mono">{{ line.right || '\u00a0' }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>

    <div v-if="imageConflicts.length > 0" class="diff-block">
      <h3 class="diff-title">图片冲突（二进制文件，无法文本合并）</h3>
      <ul class="image-list">
        <li v-for="item in imageConflicts" :key="item.relPath">
          <code>{{ item.relPath }}</code>
          <span class="subtle">两侧内容不同，需要选择保留哪一份。</span>
        </li>
      </ul>
    </div>

    <footer class="diff-actions">
      <button
        type="button"
        class="danger"
        :disabled="busy"
        @click="emit('adopt-remote')"
      >
        采用远端版本（本地修改先存入恢复副本）
      </button>
      <button type="button" :disabled="busy" @click="emit('manual-merge')">
        手工合并后保存
      </button>
      <button type="button" class="primary" :disabled="busy" @click="emit('adopt-local')">
        采用本地并覆盖远端
      </button>
    </footer>

    <p v-if="busy" class="subtle" role="status">正在处理，请稍候…</p>
  </section>
</template>

<style scoped>
.sync-diff {
  padding: 14px;
  display: flex;
  flex-direction: column;
  gap: 12px;
  border-color: var(--gl-warn);
  background: var(--gl-warn-soft);
}

.diff-header {
  display: flex;
  justify-content: space-between;
  gap: 12px;
  align-items: flex-start;
}

.diff-header p {
  margin: 4px 0 0;
}

.hash-row {
  display: flex;
  gap: 20px;
  flex-wrap: wrap;
  margin: 0;
}

.hash-row dt {
  font-size: 12px;
  color: var(--gl-text-muted);
}

.hash-row dd {
  margin: 0;
}

.mono {
  font-family: var(--gl-font-mono);
  font-size: 12px;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.diff-block {
  background: var(--gl-surface);
  border: 1px solid var(--gl-border);
  border-radius: var(--gl-radius-sm);
  padding: 10px;
}

.diff-title {
  margin: 0 0 8px;
  font-size: 13px;
}

.diff-table {
  max-height: 280px;
  overflow-y: auto;
}

.diff-table table {
  table-layout: fixed;
}

.diff-table td {
  vertical-align: top;
  word-break: break-word;
}

tr.removed td:first-child {
  background: var(--gl-danger-soft);
}

tr.added td:last-child {
  background: var(--gl-ok-soft);
}

.image-list {
  margin: 0;
  padding-left: 18px;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.diff-actions {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}
</style>

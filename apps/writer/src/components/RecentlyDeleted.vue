<script setup lang="ts">
/**
 * 回收区：最近删除与恢复。
 *
 * 诚实表达：
 * - 无条目时是空状态，不承诺 Git 历史清除；
 * - 每个条目展示两个分支的完成状态，部分失败必须看得见并可重试；
 * - 恢复后是**未发布草稿**，需要用户再次手动同步与发布。
 */
import type { TrashEntry } from '@/types/article'

defineProps<{
  entries: TrashEntry[]
  /** 待处理的崩溃恢复副本对应的文章 ID。 */
  recoveryArticleIds: string[]
  busy?: boolean
}>()

const emit = defineEmits<{
  (event: 'restore', opId: string): void
  (event: 'retry', opId: string): void
  (event: 'purge', opId: string): void
  (event: 'discard-recovery', articleId: string): void
  (event: 'restore-recovery', articleId: string): void
  (event: 'close'): void
}>()

function branchState(entry: TrashEntry): { text: string; tone: string } {
  if (entry.writingDone && entry.mainDone) {
    return { text: '写作分支与网站均已处理', tone: 'ok' }
  }
  if (entry.writingDone && !entry.mainDone) {
    return { text: '写作分支已处理／网站仍在', tone: 'warn' }
  }
  if (!entry.writingDone && entry.mainDone) {
    return { text: '网站已处理／写作分支仍在', tone: 'warn' }
  }
  return { text: '两个分支均未处理', tone: 'danger' }
}
</script>

<template>
  <section class="recently-deleted panel" aria-label="最近删除">
    <header class="trash-header">
      <div>
        <strong>最近删除</strong>
        <p class="subtle">
          本地保留可恢复副本。删除只影响 GitHub 当前版本，Git 历史中仍可见，
          软件不提供「彻底抹除所有历史」。
        </p>
      </div>
      <button type="button" class="ghost" :disabled="busy" @click="emit('close')">关闭</button>
    </header>

    <div v-if="recoveryArticleIds.length > 0" class="recovery-block">
      <h3 class="section-title">未保存的恢复副本</h3>
      <p class="subtle">
        这些文章在异常退出时留有尚未写入磁盘的编辑内容。「恢复」会把这份内容写回
        本地文章文件（只改本地，仍需你手动同步与发布）；「丢弃」则不再保留。
      </p>
      <ul class="entry-list">
        <li v-for="articleId in recoveryArticleIds" :key="articleId" class="entry">
          <div class="entry-main">
            <code>{{ articleId }}</code>
            <span class="badge warn">有未保存副本</span>
          </div>
          <div class="entry-actions">
            <button
              type="button"
              class="primary small"
              :disabled="busy"
              @click="emit('restore-recovery', articleId)"
            >
              恢复这份内容
            </button>
            <button
              type="button"
              class="ghost small"
              :disabled="busy"
              @click="emit('discard-recovery', articleId)"
            >
              丢弃副本
            </button>
          </div>
        </li>
      </ul>
    </div>

    <div v-if="entries.length === 0 && recoveryArticleIds.length === 0" class="empty-state">
      <p>回收区为空。删除文章后，可在这里恢复为未发布的本地草稿。</p>
    </div>

    <ul v-else-if="entries.length > 0" class="entry-list">
      <li v-for="entry in entries" :key="entry.opId" class="entry">
        <div class="entry-main">
          <span class="entry-title">{{ entry.title || entry.articleId }}</span>
          <code class="subtle">{{ entry.markdownRelPath }}</code>
          <span class="subtle">删除于 {{ entry.deletedAt }}</span>
          <span class="badge" :class="branchState(entry).tone">{{ branchState(entry).text }}</span>
          <span v-if="entry.images.length > 0" class="badge neutral">
            保留图片 {{ entry.images.length }} 个
          </span>
          <span v-if="entry.wasPublished" class="badge neutral">删除前已发布</span>
        </div>

        <p v-if="entry.lastError" class="error-text entry-error" role="alert">
          {{ entry.lastError }}
        </p>

        <div class="entry-actions">
          <button
            v-if="!entry.writingDone || !entry.mainDone"
            type="button"
            class="primary small"
            :disabled="busy"
            @click="emit('retry', entry.opId)"
          >
            补做未完成的分支
          </button>
          <button
            type="button"
            class="small"
            :disabled="busy || !entry.hasMarkdown"
            @click="emit('restore', entry.opId)"
          >
            恢复为未发布草稿
          </button>
          <button
            type="button"
            class="ghost small"
            :disabled="busy"
            @click="emit('purge', entry.opId)"
          >
            删除副本
          </button>
        </div>
      </li>
    </ul>
  </section>
</template>

<style scoped>
.recently-deleted {
  padding: 14px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.trash-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px;
}

.trash-header p {
  margin: 4px 0 0;
  max-width: 62ch;
}

.section-title {
  margin: 0 0 6px;
  font-size: 13px;
}

.recovery-block {
  background: var(--gl-warn-soft);
  border: 1px solid var(--gl-warn);
  border-radius: var(--gl-radius-sm);
  padding: 10px;
}

.entry-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.entry {
  border: 1px solid var(--gl-border);
  border-radius: var(--gl-radius-sm);
  padding: 10px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.entry-main {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.entry-title {
  font-weight: 600;
}

.entry-actions {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
}

button.small {
  min-height: var(--gl-hit);
  padding: 0 10px;
  font-size: 12px;
}

.entry-error {
  margin: 0;
  font-size: 13px;
}

code {
  font-family: var(--gl-font-mono);
  font-size: 12px;
}
</style>

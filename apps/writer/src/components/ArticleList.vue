<script setup lang="ts">
/**
 * 文章列表：搜索、筛选、排序与三处状态短文案。
 *
 * 颜色只作辅助，每个条目都带文字状态。
 */
import { computed } from 'vue'
import { describeStatus, type ArticleFilter, type ArticleSort, type ArticleSummary } from '@/types/article'

const props = defineProps<{
  articles: ArticleSummary[]
  selectedId: string | null
  filter: ArticleFilter
  sort: ArticleSort
  query: string
  loading: boolean
  error?: { message: string; detail?: string } | null
  selectedCounts: Record<string, number>
}>()

const emit = defineEmits<{
  (event: 'update:filter', value: ArticleFilter): void
  (event: 'update:sort', value: ArticleSort): void
  (event: 'update:query', value: string): void
  (event: 'select', id: string): void
  (event: 'create'): void
  (event: 'import'): void
  (event: 'show-trash'): void
  (event: 'retry'): void
}>()

const FILTERS: Array<{ value: ArticleFilter; label: string }> = [
  { value: 'all', label: '全部' },
  { value: 'local-only', label: '仅本地' },
  { value: 'remote-saved', label: '远程已存' },
  { value: 'site-published', label: '网站已发布' },
  { value: 'conflict', label: '需要处理' },
]

const SORTS: Array<{ value: ArticleSort; label: string }> = [
  { value: 'recent-edited', label: '最近编辑' },
  { value: 'pub-date', label: '发布日期' },
]

function filterCount(value: ArticleFilter): number {
  if (value === 'all') return props.selectedCounts.all ?? props.articles.length
  return props.selectedCounts[value] ?? 0
}

function statusOf(article: ArticleSummary) {
  return describeStatus(article.status)
}

/** 日期显示：空值显示为「未设置日期」。 */
function dateText(article: ArticleSummary): string {
  return article.pubDate || '未设置日期'
}

const hasArticles = computed(() => props.articles.length > 0)
</script>

<template>
  <section class="article-list" aria-label="文章列表">
    <header class="list-header">
      <div class="search-row">
        <label class="sr-only" for="article-search">搜索文章</label>
        <input
          id="article-search"
          type="text"
          :value="query"
          placeholder="搜索标题、摘要或标签"
          @input="emit('update:query', ($event.target as HTMLInputElement).value)"
        />
        <button class="primary" type="button" @click="emit('create')">新建文章</button>
      </div>

      <div class="filters" role="group" aria-label="筛选文章">
        <button
          v-for="item in FILTERS"
          :key="item.value"
          type="button"
          class="chip"
          :class="{ active: filter === item.value }"
          :aria-pressed="filter === item.value"
          @click="emit('update:filter', item.value)"
        >
          {{ item.label }}
          <span class="count">{{ filterCount(item.value) }}</span>
        </button>
      </div>

      <div class="secondary-row">
        <label class="inline-label">
          排序
          <select
            :value="sort"
            @change="emit('update:sort', ($event.target as HTMLSelectElement).value as ArticleSort)"
          >
            <option v-for="item in SORTS" :key="item.value" :value="item.value">
              {{ item.label }}
            </option>
          </select>
        </label>
        <button type="button" class="ghost" @click="emit('import')">从本地导入</button>
        <button type="button" class="ghost" @click="emit('show-trash')">最近删除</button>
      </div>
    </header>

    <div v-if="loading" class="empty-state">正在读取文章…</div>

    <div v-else-if="error" class="empty-state">
      <p class="error-text">{{ error.message }}</p>
      <p v-if="error.detail" class="subtle">{{ error.detail }}</p>
      <button type="button" @click="emit('retry')">重试读取</button>
    </div>

    <div v-else-if="!hasArticles" class="empty-state">
      <p>还没有文章。可以新建一篇，或从本地导入已有 Markdown。</p>
      <button class="primary" type="button" @click="emit('create')">新建文章</button>
    </div>

    <ul v-else class="items">
      <li v-for="article in articles" :key="article.id">
        <button
          type="button"
          class="item"
          :class="{ selected: article.id === selectedId }"
          :aria-current="article.id === selectedId ? 'true' : undefined"
          @click="emit('select', article.id)"
        >
          <span class="item-title">{{ article.title || article.id }}</span>
          <span class="item-meta">
            <span class="subtle">{{ dateText(article) }}</span>
            <span v-if="article.tags.length" class="subtle">· {{ article.tags.join('、') }}</span>
          </span>
          <span class="badges">
            <span class="badge" :class="statusOf(article).tone">
              {{ statusOf(article).site }}
            </span>
            <span class="badge neutral">{{ statusOf(article).remote }}</span>
            <span v-if="article.imageCount > 0" class="badge neutral">
              图片 {{ article.imageCount }}
            </span>
          </span>
          <span v-if="article.loadError" class="badge danger error-badge">
            {{ article.loadError.message }}
            <span v-if="article.loadError.detail" class="subtle">（{{ article.loadError.detail }}）</span>
          </span>
        </button>
      </li>
    </ul>
  </section>
</template>

<style scoped>
.article-list {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  background: var(--gl-surface);
  border-right: 1px solid var(--gl-border);
}

.list-header {
  display: flex;
  flex-direction: column;
  gap: var(--gl-gap);
  padding: 14px;
  border-bottom: 1px solid var(--gl-border);
}

.search-row {
  display: flex;
  gap: 8px;
  align-items: center;
}

.filters {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.chip {
  min-height: var(--gl-hit);
  padding: 0 10px;
  font-size: 13px;
  border-radius: 999px;
}

.chip.active {
  background: var(--gl-accent-soft);
  border-color: var(--gl-accent);
  color: var(--gl-accent);
  font-weight: 600;
}

.chip .count {
  margin-left: 4px;
  color: var(--gl-text-subtle);
  font-size: 12px;
}

.secondary-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}

.inline-label {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
  color: var(--gl-text-muted);
}

.inline-label select {
  width: auto;
  min-height: var(--gl-hit);
  padding: 4px 8px;
}

.items {
  list-style: none;
  margin: 0;
  padding: 6px;
  overflow-y: auto;
  overflow-x: hidden;
  flex: 1;
  min-height: 0;
}

.item {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 4px;
  width: 100%;
  padding: 10px;
  margin-bottom: 4px;
  text-align: left;
  border-color: transparent;
  background: transparent;
  min-height: 0;
  min-width: 0;
}

.item-title {
  font-weight: 600;
  color: var(--gl-text);
  overflow-wrap: anywhere;
  max-width: 100%;
}

/*
 * 选中态（施工单 §3.1-2 点名要补的缺口）。
 *
 * 只靠颜色不够：左侧竖条 + 表面底色 + 强调色标题三者同时变化，
 * 保证在灰度或低对比环境下仍能分辨当前文章。
 */
.item.selected {
  background: var(--gl-accent-soft);
  border-color: var(--gl-accent);
  box-shadow: inset 3px 0 0 var(--gl-accent);
}

.item.selected .item-title {
  color: var(--gl-accent);
}

.item.selected:hover {
  background: var(--gl-accent-soft);
  border-color: var(--gl-accent);
}

.item-meta {
  display: flex;
  gap: 6px;
  flex-wrap: wrap;
}

.badges {
  display: flex;
  gap: 4px;
  flex-wrap: wrap;
}

/*
 * 解析错误可能很长；徽标默认 nowrap 会导致侧栏横向溢出。
 * 错误徽标允许换行并按容器宽度收敛。
 */
.error-badge {
  white-space: normal;
  overflow-wrap: anywhere;
  max-width: 100%;
  border-radius: var(--gl-radius-sm);
  text-align: left;
}
</style>

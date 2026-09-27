<script setup lang="ts">
/**
 * 文章元数据编辑：标题、摘要、日期、标签与固定 URL。
 *
 * 元数据与正文处于同一自动保存事务（都由父组件的 `draftMeta`/`draftBody` 驱动）。
 * 校验错误靠近字段显示。URL 标识默认不可编辑，改 URL 走独立的显式操作。
 */
import { computed } from 'vue'
import type { ArticleMeta } from '@/types/article'

const props = defineProps<{
  articleId: string
  meta: ArticleMeta
  wordCount: number
  saveStateText: string
  saveError?: { message: string; detail?: string } | null
  /** 后端返回的字段级校验错误（detail 即字段名）。 */
  fieldsWithError: string[]
  /** 该文章在远端是否已公开（决定 updatedDate 的提示语）。 */
  publishedOnSite: boolean
  updatedDateIsSet: boolean
  disabled?: boolean
}>()

const emit = defineEmits<{
  (event: 'update:meta', value: ArticleMeta): void
  (event: 'edit-url'): void
  (event: 'clear-updated-date'): void
  (event: 'set-updated-date', value: string): void
}>()

const tagsText = computed({
  get: () => props.meta.tags.join('，'),
  set: (value: string) => {
    // 输入时去除首尾空白，保留原条目的中文和顺序；空项丢弃。
    const tags = value
      .split(/[，,]/)
      .map((tag) => tag.trim())
      .filter((tag) => tag.length > 0)
    emit('update:meta', { ...props.meta, tags })
  },
})

function patch<K extends keyof ArticleMeta>(key: K, value: ArticleMeta[K]): void {
  emit('update:meta', { ...props.meta, [key]: value })
}

function hasError(field: string): boolean {
  return props.fieldsWithError.includes(field)
}

const updatedDateHint = computed(() =>
  props.updatedDateIsSet
    ? '该字段只在你确认更新已发布文章时改写，自动保存不会刷新它。'
    : '可选。仅在你确认更新已发布文章的元数据时填写。',
)
</script>

<template>
  <section class="article-meta panel" aria-label="文章元数据">
    <div class="meta-grid">
      <div class="field field-wide">
        <label for="meta-title">标题 <span class="required" aria-hidden="true">*</span></label>
        <input
          id="meta-title"
          type="text"
          :value="meta.title"
          :aria-invalid="hasError('title')"
          :disabled="disabled"
          @input="patch('title', ($event.target as HTMLInputElement).value)"
        />
        <p v-if="hasError('title')" class="field-error error-text">标题不能为空</p>
      </div>

      <div class="field field-wide">
        <label for="meta-description">摘要 <span class="required" aria-hidden="true">*</span></label>
        <textarea
          id="meta-description"
          rows="2"
          :value="meta.description"
          :aria-invalid="hasError('description')"
          :disabled="disabled"
          @input="patch('description', ($event.target as HTMLTextAreaElement).value)"
        />
        <p v-if="hasError('description')" class="field-error error-text">摘要不能为空</p>
      </div>

      <div class="field">
        <label for="meta-pubdate">发布日期</label>
        <input
          id="meta-pubdate"
          type="date"
          :value="meta.pubDate"
          :aria-invalid="hasError('pubDate')"
          :disabled="disabled"
          @change="patch('pubDate', ($event.target as HTMLInputElement).value)"
        />
        <p v-if="hasError('pubDate')" class="field-error error-text">
          请选择真实存在的日期（YYYY-MM-DD）
        </p>
        <p v-else class="subtle">网站按发布日期倒序排列；网站以 UTC 日期格式展示。</p>
      </div>

      <div class="field">
        <label for="meta-updated">更新日期（可选）</label>
        <input
          id="meta-updated"
          type="date"
          :value="meta.updatedDate ?? ''"
          :aria-invalid="hasError('updatedDate')"
          :disabled="disabled"
          @change="emit('set-updated-date', ($event.target as HTMLInputElement).value)"
        />
        <p v-if="hasError('updatedDate')" class="field-error error-text">
          更新日期需为真实存在的日期
        </p>
        <p v-else class="subtle">{{ updatedDateHint }}</p>
        <button
          v-if="updatedDateIsSet"
          type="button"
          class="ghost small"
          :disabled="disabled"
          @click="emit('clear-updated-date')"
        >
          清除更新日期
        </button>
        <p v-if="publishedOnSite && updatedDateIsSet" class="subtle">
          本文已在网站公开，修改该字段后需重新发布才会生效。
        </p>
      </div>

      <div class="field">
        <label for="meta-tags">标签</label>
        <input
          id="meta-tags"
          type="text"
          :value="tagsText"
          :aria-invalid="hasError('tags')"
          :disabled="disabled"
          placeholder="用中文逗号或英文逗号分隔"
          @input="tagsText = ($event.target as HTMLInputElement).value"
        />
        <p v-if="hasError('tags')" class="field-error error-text">标签不能为空</p>
        <p v-else class="subtle">标签可以为空。不新增「分类」字段，也不自动添加标签。</p>
      </div>

      <div class="field">
        <label for="meta-id">网站 URL 标识（固定）</label>
        <div class="url-row">
          <input id="meta-id" type="text" :value="articleId" readonly />
          <button type="button" :disabled="disabled" @click="emit('edit-url')">更改 URL…</button>
        </div>
        <p class="subtle">
          改标题不会改变 URL。更改 URL 是独立操作，会让旧链接失效。
        </p>
      </div>
    </div>

    <footer class="meta-footer">
      <span class="subtle">正文约 {{ wordCount }} 字</span>
      <span class="subtle">{{ saveStateText }}</span>
      <span v-if="meta.draft" class="badge neutral">工作稿（站点不公开）</span>
      <span v-else class="badge info">已标记为公开</span>
    </footer>

    <p v-if="saveError" class="save-error error-text" role="alert">
      {{ saveError.message }}
      <span v-if="saveError.detail" class="subtle">（{{ saveError.detail }}）</span>
    </p>
  </section>
</template>

<style scoped>
.article-meta {
  padding: 14px;
}

.meta-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
  gap: var(--gl-gap);
}

.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.field-wide {
  grid-column: 1 / -1;
}

.field label {
  font-size: 13px;
  font-weight: 600;
  color: var(--gl-text-muted);
}

.required {
  color: var(--gl-danger);
}

.field-error {
  font-size: 12px;
  margin: 0;
}

.url-row {
  display: flex;
  gap: 8px;
}

.url-row input {
  font-family: var(--gl-font-mono);
  font-size: 12px;
  background: var(--gl-surface-muted);
}

.meta-footer {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  margin-top: 12px;
  padding-top: 10px;
  border-top: 1px solid var(--gl-border);
}

button.small {
  min-height: var(--gl-hit);
  padding: 0 10px;
  font-size: 12px;
  align-self: flex-start;
}

.save-error {
  margin: 8px 0 0;
  font-size: 13px;
}
</style>

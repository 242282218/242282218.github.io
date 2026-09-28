<script setup lang="ts">
/**
 * 预览样式抽屉（§3.4）。
 *
 * 默认显示**站点原貌**；这里的每一项都是本机阅读偏好，只影响软件内的即时预览，
 * **不改变网站文章**。站点没有的可发布主题色、链接色、引用背景、标题样式、
 * 图注、自定义 CSS、字体系列与代码主题切换一律不提供。
 */
import { computed } from 'vue'
import { isSiteDefaultStyles, type PreviewStyles } from '@/composables/usePreviewStyles'

const props = defineProps<{
  open: boolean
  styles: PreviewStyles
}>()

const emit = defineEmits<{
  (event: 'update:styles', value: PreviewStyles): void
  (event: 'close'): void
}>()

/** 站点 `.prose` 基准值：字号 16px、行高 2.05、段落下边距 21px。 */
const SITE_BASELINE = { fontSize: 16, lineHeight: 2.05, paragraphSpacing: 21 } as const

const FONT_SIZES = [14, 15, 16, 17, 18] as const
const LINE_HEIGHTS = [1.5, 1.65, 1.75, 1.9, 2.05] as const
/** 段间距用缩放系数表达，1.0 即站点原貌。 */
const SPACING_SCALES = [0.75, 1, 1.25, 1.5] as const

/** 与预览实际使用的基准判断同源，避免两处比较逻辑漂移。 */
const isSiteDefault = computed(() => isSiteDefaultStyles(props.styles))

function patch(next: Partial<PreviewStyles>): void {
  emit('update:styles', { ...props.styles, ...next })
}

function resetToSite(): void {
  emit('update:styles', {
    fontSize: null,
    lineHeight: null,
    paragraphSpacingScale: 1,
    width: 'site',
  })
}
</script>

<template>
  <aside v-if="open" class="style-drawer" aria-label="预览样式">
    <header class="drawer-header">
      <strong>预览样式</strong>
      <button type="button" class="ghost small" @click="emit('close')">关闭</button>
    </header>

    <p class="notice" role="note">
      以下设置<strong>仅影响软件内的即时预览</strong>，不改变网站文章。默认显示站点原貌。
    </p>

    <section class="group">
      <h3>字号</h3>
      <p class="subtle">站点当前：{{ SITE_BASELINE.fontSize }}px</p>
      <div class="options">
        <button
          type="button"
          :class="['option', { active: styles.fontSize === null }]"
          @click="patch({ fontSize: null })"
        >
          站点 {{ SITE_BASELINE.fontSize }}px
        </button>
        <button
          v-for="size in FONT_SIZES"
          :key="size"
          type="button"
          :class="['option', { active: styles.fontSize === size }]"
          @click="patch({ fontSize: size })"
        >
          {{ size }}px
        </button>
      </div>
    </section>

    <section class="group">
      <h3>行距</h3>
      <p class="subtle">站点当前：{{ SITE_BASELINE.lineHeight }}</p>
      <div class="options">
        <button
          type="button"
          :class="['option', { active: styles.lineHeight === null }]"
          @click="patch({ lineHeight: null })"
        >
          站点 {{ SITE_BASELINE.lineHeight }}
        </button>
        <button
          v-for="height in LINE_HEIGHTS"
          :key="height"
          type="button"
          :class="['option', { active: styles.lineHeight === height }]"
          @click="patch({ lineHeight: height })"
        >
          {{ height }}
        </button>
      </div>
    </section>

    <section class="group">
      <h3>段间距</h3>
      <p class="subtle">站点当前：{{ SITE_BASELINE.paragraphSpacing }}px（缩放系数）</p>
      <div class="options">
        <button
          v-for="scale in SPACING_SCALES"
          :key="scale"
          type="button"
          :class="['option', { active: styles.paragraphSpacingScale === scale }]"
          @click="patch({ paragraphSpacingScale: scale })"
        >
          {{ scale === 1 ? '站点原貌 ×1' : `×${scale}` }}
        </button>
      </div>
    </section>

    <section class="group">
      <h3>预览列宽</h3>
      <p class="subtle">
        桌面默认使用站点文章列宽（约 760px）；手机列模拟 375px 视口，
        并按站点规则计入页面内边距。
      </p>
      <div class="options">
        <button
          type="button"
          :class="['option', { active: styles.width === 'site' }]"
          @click="patch({ width: 'site' })"
        >
          站点桌面列
        </button>
        <button
          type="button"
          :class="['option', { active: styles.width === 'mobile' }]"
          @click="patch({ width: 'mobile' })"
        >
          375px 手机
        </button>
      </div>
    </section>

    <button type="button" class="primary block" :disabled="isSiteDefault" @click="resetToSite">
      还原站点基准
    </button>
  </aside>
</template>

<style scoped>
.style-drawer {
  width: 280px;
  flex: 0 0 280px;
  display: flex;
  flex-direction: column;
  gap: 16px;
  padding: 12px;
  overflow-y: auto;
  background: var(--gl-surface);
  border-left: 1px solid var(--gl-border);
}

.drawer-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.notice {
  margin: 0;
  padding: 8px 10px;
  background: var(--gl-accent-soft);
  border-radius: var(--gl-radius-sm);
  font-size: 12px;
  line-height: 1.6;
}

.group h3 {
  margin: 0 0 2px;
  font-size: 13px;
}

.group p {
  margin: 0 0 6px;
  font-size: 12px;
}

.options {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}

.option {
  min-height: 28px;
  padding: 0 10px;
  background: var(--gl-surface);
  border: 1px solid var(--gl-border);
  border-radius: 6px;
  font-size: 12px;
  color: var(--gl-text-muted);
  cursor: pointer;
}

.option.active {
  background: var(--gl-accent-soft);
  border-color: var(--gl-accent);
  color: var(--gl-accent);
}

.block {
  width: 100%;
}

button.small {
  min-height: 28px;
  padding: 0 10px;
  font-size: 12px;
}
</style>

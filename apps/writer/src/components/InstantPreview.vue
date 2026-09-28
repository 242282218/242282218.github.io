<script setup lang="ts">
/**
 * 即时预览（§3.3）。
 *
 * 在**同源 iframe**里渲染，宽度由本组件精确指定（桌面 1000px 视口 / 手机 375px）。
 * 为什么用 iframe 而不是容器模拟：站点样式里的媒体查询是按**视口**判断的，
 * 在容器里它们会对齐软件窗口宽度，桌面模式下窗口一窄就会误触发 `.shell`
 * 的移动端规则。iframe 有自己的视口，媒体查询因此与真实页面等价。
 *
 * iframe 内只写入三样东西：站点正文样式的受控快照、净化后的正文 HTML、
 * 本机预览覆盖变量；没有脚本、没有外部资源。
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import previewArticleCss from '@/styles/preview-article.css?inline'
import { renderPreviewHtml } from '@/services/preview-render'
import { SITE_PROSE_BASELINE, type PreviewStyles } from '@/composables/usePreviewStyles'

const props = defineProps<{
  markdown: string
  /** 文章标题与摘要：与站点文章页头部保持一致的结构。 */
  title: string
  description: string
  pubDate: string
  tags: string[]
  styles: PreviewStyles
}>()

const frameRef = ref<HTMLIFrameElement | null>(null)
const sanitizedNotice = ref(false)

/**
 * 预览内容页的骨架。
 *
 * 结构与站点文章页一致：`.shell > .article > (.article-header + .prose)`，
 * 这样快照里的选择器才能命中同一批元素。
 */
function buildDocument(bodyHtml: string): string {
  return `<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8" />
<meta name="color-scheme" content="light" />
<style>${previewArticleCss}</style>
<style>${overrideCss()}</style>
</head>
<body>
<div class="shell">
  <article class="article">
    <header class="article-header">
      <p class="post-meta">${escapeText(props.pubDate)}${
        props.tags.length > 0 ? ` · ${props.tags.map(escapeText).join(' · ')}` : ''
      }</p>
      <h1>${escapeText(props.title)}</h1>
      <p class="article-description">${escapeText(props.description)}</p>
    </header>
    <div class="prose" data-site-preview>${bodyHtml}</div>
  </article>
</div>
</body>
</html>`
}

/**
 * 本机预览覆盖（§3.4）。
 *
 * 显式覆盖 `.prose` 自身的字号/行高/段距，而不是只改外层继承值——快照里
 * `.prose` 自己声明了这两个属性，只靠继承会被它覆盖掉。
 *
 * 取值为**字面量**而非 CSS 变量：iframe 是独立文档，自定义属性不跨文档边界。
 * 变量定义在父文档里（例如预览容器的 inline style），iframe 内部读不到，
 * `var()` 会静默退化成 fallback，表现为「样式抽屉点了没反应」。
 */
function overrideCss(): string {
  const fontSize = props.styles.fontSize ?? SITE_PROSE_BASELINE.fontSizePx
  const lineHeight = props.styles.lineHeight ?? SITE_PROSE_BASELINE.lineHeight
  const rules: string[] = [
    `[data-site-preview].prose {font-size: ${fontSize}px;line-height: ${lineHeight};}`,
  ]
  if (props.styles.paragraphSpacingScale !== 1) {
    rules.push(
      `[data-site-preview].prose p {` +
        `margin-bottom: calc(${SITE_PROSE_BASELINE.paragraphMarginPx}px * ${props.styles.paragraphSpacingScale});` +
        `}`,
    )
  }
  return rules.join('\n')
}

function escapeText(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
}

/** 渲染并写入 iframe。 */
function render(): void {
  // 每次重绘前补测一次：面板宽度可能因分隔条拖动或视图切换而变化，而那些变化
  // 不一定伴随 window resize（容器不依赖 ResizeObserver，见 `measureCanvas`）。
  measureCanvas()
  const frame = frameRef.value
  if (!frame) return
  const doc = frame.contentDocument
  if (!doc) return
  const { html, sanitized } = renderPreviewHtml(props.markdown)
  sanitizedNotice.value = sanitized
  doc.open()
  doc.write(buildDocument(html))
  doc.close()
}

onMounted(() => {
  render()
})

// 正文或样式变化时整体重绘：预览是只读投影，重建比增量打补丁更可靠，
// 也不会把滚动位置之外的编辑器状态带进来。
//
// 头部要渲染的字段（标题、摘要、日期、标签）必须全部在这里列出：漏掉任何一项
// 都会让预览显示旧值——只改发布日期或标签时界面看起来「没反应」。
watch(
  () => [
    props.markdown,
    props.styles,
    props.title,
    props.description,
    props.pubDate,
    ...props.tags,
  ],
  () => render(),
  { deep: true },
)

/**
 * 预览视口宽度。
 *
 * 桌面模式用站点 `.shell` 的上限（1000px）作为**视口**，让 `.article` 自己在
 * 里面收到 760px 阅读列——这才是真实桌面页的样子。直接用 760px 会让 iframe
 * 视口落到 `@media (max-width: 780px)` 区间，误触发站点的平板规则。
 * 手机模式模拟 375px 视口，而不是把正文固定成 375px。
 */
const viewportPx = computed(() => (props.styles.width === 'mobile' ? 375 : SITE_PROSE_BASELINE.shellMaxWidthPx))
const viewportWidth = computed(() => `${viewportPx.value}px`)

/**
 * 把「真实视口宽度」缩放到当前预览列里显示。
 *
 * 桌面视口固定 1000px，而双屏时预览列通常只有 500–700px；若不缩放，右侧会被
 * 裁掉、用户得横向滚动。这里按容器宽度等比缩放整张页面（与浏览器「设备模拟」
 * 同法）：媒体查询仍按 iframe 自己的视口判断，因此排版语义不变，只是整体缩小。
 * 容器比视口宽（或手机模式）时比例为 1，不做任何缩放。
 */
const canvasRef = ref<HTMLElement | null>(null)
const canvasWidth = ref(0)
/** 小于该宽度视为「尚未完成布局」，不用它计算比例（否则会算出一个极小的比例）。 */
const MIN_MEASURABLE_WIDTH = 120

const scale = computed(() => {
  if (canvasWidth.value < MIN_MEASURABLE_WIDTH) return 1
  return Math.min(1, canvasWidth.value / viewportPx.value)
})

/** 缩放后的舞台尺寸与位置：缩放到预览列宽，并在更宽的列里居中。 */
const stageStyle = computed(() => {
  const renderedWidth = viewportPx.value * scale.value
  const offset = canvasWidth.value > renderedWidth ? (canvasWidth.value - renderedWidth) / 2 : 0
  return {
    width: `${renderedWidth}px`,
    left: `${offset}px`,
    // iframe 未缩放的 CSS 高度要放大 1/s，缩放后才等于容器高度。
    '--preview-inverse-scale': `${100 / scale.value}%`,
  }
})

/**
 * 量一次容器宽度。
 *
 * 三条路径都要有，缺一不可：
 * 1. `ResizeObserver` —— 真实窗口里最可靠：分隔条拖动、视图模式切换、面板折叠
 *    都会改变容器宽度，而它们**不一定**伴随 `window.resize`；
 * 2. `window.resize` 与若干补测计时器 —— 兜底。`ResizeObserver` 的回调由
 *    `requestAnimationFrame` 驱动，在 rAF 不触发的宿主里一次都不回调（本机内置
 *    浏览器夹具实测如此），此时只剩这条路径；
 * 3. 每次重绘前补测（见 `render`）。
 */
function measureCanvas(): void {
  const width = canvasRef.value?.clientWidth ?? 0
  if (width >= MIN_MEASURABLE_WIDTH) {
    canvasWidth.value = width
  }
}

let measureTimers: number[] = []
let resizeObserver: ResizeObserver | null = null

function scheduleMeasures(): void {
  for (const timer of measureTimers) window.clearTimeout(timer)
  // 布局稳定前可能量到过渡宽度，补测几次；都通过 setTimeout，不依赖 rAF。
  measureTimers = [0, 120, 400].map((delay) =>
    window.setTimeout(() => {
      measureCanvas()
    }, delay),
  )
}

onMounted(() => {
  measureCanvas()
  scheduleMeasures()
  window.addEventListener('resize', measureCanvas)
  const canvas = canvasRef.value
  if (canvas && typeof ResizeObserver === 'function') {
    resizeObserver = new ResizeObserver(() => {
      measureCanvas()
    })
    resizeObserver.observe(canvas)
  }
})

onBeforeUnmount(() => {
  for (const timer of measureTimers) window.clearTimeout(timer)
  measureTimers = []
  resizeObserver?.disconnect()
  resizeObserver = null
  window.removeEventListener('resize', measureCanvas)
})
</script>

<template>
  <section class="instant-preview" aria-label="即时预览">
    <header class="preview-header">
      <strong>即时预览</strong>
      <span class="subtle">
        {{ styles.width === 'mobile' ? '375px 视口' : '站点桌面列约 760px' }}
      </span>
    </header>

    <div ref="canvasRef" class="canvas">
      <div class="stage" :style="stageStyle">
        <iframe
          ref="frameRef"
          class="preview-frame"
          :title="`即时预览：${title}`"
          :style="{
            width: viewportWidth,
            height: 'var(--preview-inverse-scale, 100%)',
            transform: `scale(${scale})`,
          }"
          sandbox="allow-same-origin"
        />
      </div>
    </div>

    <p class="footnote subtle">
      本站样式的受控快照渲染，用于写作时看近似排版；<strong>不是</strong>网站最终效果。
      <template v-if="sanitizedNotice">
        本次渲染净化了不安全的内嵌内容（脚本、远端图片或危险属性已移除）。
      </template>
      不打包 webfont，离线时的字形与换行可能与线上不同；代码块不做语法高亮，
      本地图片也不会在预览里加载（预览完全离线，请看「网站预览」确认图片效果）。
    </p>
  </section>
</template>

<style scoped>
.instant-preview {
  display: flex;
  flex-direction: column;
  min-height: 0;
  flex: 1;
  background: var(--gl-surface-muted);
}

.preview-header {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 8px;
  padding: 8px 12px;
  background: var(--gl-surface);
  border-bottom: 1px solid var(--gl-border);
  font-size: 13px;
}

/* 视口宽度由 iframe 自己声明（桌面 1000px = 站点 .shell 上限）；容器更窄时
   由 .stage 等比缩小整张页面，而不是把桌面布局挤成移动端。 */
.canvas {
  flex: 1;
  min-height: 0;
  overflow: auto;
  background: #f8fafb;
  position: relative;
}

/* 舞台绝对定位：它绝不会反向影响 `.canvas` 的宽度测量（避免「缩放→容器变窄→
   更小缩放」的反馈循环）。宽度由内联 style 按比例给定。 */
.stage {
  position: absolute;
  top: 0;
  left: 0;
  height: 100%;
}

.preview-frame {
  border: none;
  display: block;
  transform-origin: top left;
}

.footnote {
  margin: 0;
  padding: 8px 12px;
  background: var(--gl-surface);
  border-top: 1px solid var(--gl-border);
  font-size: 12px;
  line-height: 1.6;
}
</style>

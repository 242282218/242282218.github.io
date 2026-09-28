<script setup lang="ts">
/**
 * 网站预览入口（真实 Astro 服务）。
 *
 * 即时预览已由 `InstantPreview.vue` 承担（站点样式快照的本机近似），
 * 本组件只负责启动**真实的 Astro 服务**——它是网页最终效果的判据，
 * 不由即时预览替代（见交付说明与 §3.3）。
 */
import { computed } from 'vue'
import type { PreviewDependencyStatus } from '@/types/article'

const props = defineProps<{
  previewUrl: string | null
  previewBanner: string
  previewNotice: string | null
  starting: boolean
  /** 预览依赖的准备状态；缺依赖时启动预览会被拒绝。 */
  dependency: PreviewDependencyStatus
  /** 是否正有一个用户触发的依赖准备在等待。 */
  preparingDependencies: boolean
}>()

const emit = defineEmits<{
  (event: 'start-preview', simulatePublic: boolean): void
  (event: 'stop-preview'): void
  (event: 'open-preview'): void
  (event: 'prepare-dependencies'): void
}>()

/** 依赖状态的中文说明，以及是否应显示「准备依赖」按钮。 */
const dependencyNotice = computed<{ text: string; showPrepare: boolean }>(() => {
  switch (props.dependency.kind) {
    case 'ready':
      return { text: '预览依赖已就绪。', showPrepare: false }
    case 'preparing':
      return {
        text: `正在准备预览依赖（任务 ${props.dependency.taskId}）…准备期间可继续编辑。`,
        showPrepare: false,
      }
    case 'failed':
      return { text: `准备预览依赖失败：${props.dependency.reason}`, showPrepare: true }
    case 'missing':
      return {
        text: '尚未准备预览依赖。启动预览不会自动下载依赖，请先点「准备预览依赖」。',
        showPrepare: true,
      }
  }
})
</script>

<template>
  <section class="site-preview" aria-label="网站预览">
    <header class="preview-header">
      <strong>网站预览（真实服务）</strong>
    </header>

    <div class="site-preview-body">
      <p class="subtle">
        使用最新可获取的站点代码，在隔离的临时目录中覆盖当前文章内容后启动，
        只绑定本机 127.0.0.1，不会推送、不会修改正式仓库。
        这是判断网页最终效果的依据，上面的即时预览只是写作时的近似。
      </p>

      <p class="subtle dependency" role="status">{{ dependencyNotice.text }}</p>

      <div v-if="dependencyNotice.showPrepare" class="preview-actions">
        <button
          type="button"
          :disabled="preparingDependencies"
          @click="emit('prepare-dependencies')"
        >
          {{ preparingDependencies ? '正在准备依赖…' : '准备预览依赖' }}
        </button>
      </div>

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
    </div>
  </section>
</template>

<style scoped>
.site-preview {
  flex: 0 0 auto;
  background: var(--gl-surface);
  border-top: 1px solid var(--gl-border);
}

.preview-header {
  display: flex;
  align-items: baseline;
  gap: 8px;
  padding: 8px 12px;
  border-bottom: 1px solid var(--gl-border);
  font-size: 13px;
}

.site-preview-body {
  padding: 10px 12px;
  font-size: 12px;
}

.preview-actions {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
  margin-top: 8px;
}

.preview-live {
  margin-top: 10px;
}

.dependency {
  margin-top: 8px;
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
  font-size: 12px;
}

.mono {
  font-family: var(--gl-font-mono);
  font-size: 12px;
  overflow-wrap: anywhere;
}
</style>

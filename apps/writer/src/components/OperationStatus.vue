<script setup lang="ts">
/**
 * 操作状态与失败处理。
 *
 * 明确区分「已提交发布」与「网站已上线」；查询不到部署结论时显示
 * 「部署状态待确认」并提供运行记录链接，绝不凭推送成功猜测上线。
 */
import { computed } from 'vue'
import type { ArticleStatus, DeploymentStatus, SiteState } from '@/types/article'

const props = defineProps<{
  status: ArticleStatus | null
  deployment?: DeploymentStatus | null
  /** 上一次操作的失败信息。 */
  lastError?: { message: string; detail?: string } | null
  busy?: boolean
  /** 正在核对远端状态。 */
  checking?: boolean
}>()

const emit = defineEmits<{
  (event: 'check-deployment'): void
  (event: 'open-run', url: string): void
}>()

const SITE_LABELS: Record<SiteState, string> = {
  // 未知必须与「尚未发布」分开：核对失败时不能说「尚未发布到网站」。
  unverified: '网站状态待核对',
  'never-published': '尚未发布到网站',
  'live-old-version': '网站仍在展示上次发布的版本',
  'publication-submitted': '已提交发布，部署结果待确认',
  deploying: '正在部署',
  'live-current-version': '网站已是当前版本',
  'deploy-failed': '已提交但部署失败',
  withdrawn: '已从网站撤下',
}

const siteText = computed(() =>
  props.status ? SITE_LABELS[props.status.site] : '尚未选择文章',
)

/**
 * 未核对时展示原因，让用户知道「不知道」是因为没查、还是查询失败。
 *
 * 这里刻意不显示「已同步」「从未发布」等肯定结论——未知不等于否定。
 */
const unverifiedReason = computed(() => {
  if (!props.status) return null
  if (props.status.remoteSync !== 'unverified' && props.status.site !== 'unverified') return null
  return (
    props.status.writing.reason ??
    props.status.main.reason ??
    '尚未核对远端状态；打开文章或手动刷新后可获得结论'
  )
})

/** 部署结论是否来自真实查询。 */
const deploymentChecked = computed(() => props.deployment?.checked === true)

const deploymentDetail = computed(() => {
  const status = props.deployment
  if (!status) return null
  if (!status.checked) {
    return status.notice ?? '部署状态待确认：无法读取工作流结果，请打开运行记录查看'
  }
  switch (status.state) {
    case 'live-current-version':
      return '站点构建与部署均已成功，网站已更新到本次提交。'
    case 'deploy-failed':
      return '推送已成功，但站点构建或部署失败。已发布的提交不会被自动回滚。'
    case 'deploying':
      return '推送已完成，站点正在构建与部署。'
    default:
      return '推送已完成，尚未取得部署结论。'
  }
})

/** 区分「本地已保存」与「远程已存」的说明。 */
const localText = computed(() => {
  if (!props.status) return ''
  return props.status.locallySaved ? '本地已保存到磁盘' : '尚未保存到磁盘'
})

const remoteText = computed(() => {
  if (!props.status) return ''
  switch (props.status.remoteSync) {
    case 'unverified':
      return '写作分支状态待核对'
    case 'local-only':
      return '尚未同步到写作分支'
    case 'saving':
      return '正在同步到写作分支'
    case 'saved':
      return '写作分支已存'
    case 'conflict':
      return '写作分支上有冲突待处理'
    case 'failed':
      return '同步失败，本地改动仍在'
  }
})
</script>

<template>
  <section class="operation-status panel" aria-label="操作状态">
    <div class="status-row">
      <span class="status-item">
        <strong>本地</strong>
        <span class="subtle">{{ localText || '—' }}</span>
      </span>
      <span class="status-item">
        <strong>写作分支</strong>
        <span class="subtle">{{ remoteText || '—' }}</span>
      </span>
      <span class="status-item">
        <strong>网站</strong>
        <span class="subtle">{{ siteText }}</span>
      </span>
      <span v-if="busy" class="badge info">操作进行中…</span>
      <span v-if="checking" class="badge info">正在核对远端…</span>
    </div>

    <p v-if="unverifiedReason" class="subtle unverified-note">
      {{ unverifiedReason }}
    </p>

    <div v-if="deployment" class="deployment-row">
      <span :class="['badge', deploymentChecked ? (deployment.state === 'live-current-version' ? 'ok' : deployment.state === 'deploy-failed' ? 'danger' : 'info') : 'warn']">
        {{ deploymentChecked ? '已核实部署结果' : '部署状态待确认' }}
      </span>
      <span class="subtle">{{ deploymentDetail }}</span>
      <button
        v-if="deployment.runUrl"
        type="button"
        class="ghost small"
        @click="emit('open-run', deployment.runUrl)"
      >
        打开运行记录
      </button>
      <button type="button" class="ghost small" @click="emit('check-deployment')">
        重新核对
      </button>
    </div>

    <p v-if="lastError" class="error-text op-error" role="alert">
      {{ lastError.message }}
      <span v-if="lastError.detail" class="subtle">（{{ lastError.detail }}）</span>
    </p>
  </section>
</template>

<style scoped>
.operation-status {
  padding: 10px 14px;
}

.status-row {
  display: flex;
  gap: 20px;
  flex-wrap: wrap;
  align-items: baseline;
}

.status-item {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.status-item strong {
  font-size: 12px;
  color: var(--gl-text-muted);
}

.deployment-row {
  display: flex;
  gap: 10px;
  align-items: center;
  flex-wrap: wrap;
  margin-top: 10px;
  padding-top: 10px;
  border-top: 1px solid var(--gl-border);
}

button.small {
  min-height: var(--gl-hit);
  padding: 0 10px;
  font-size: 12px;
}

.op-error {
  margin: 8px 0 0;
  font-size: 13px;
}

.unverified-note {
  margin: 8px 0 0;
  font-size: 12px;
}
</style>

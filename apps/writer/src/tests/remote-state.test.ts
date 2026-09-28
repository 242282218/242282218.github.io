/**
 * A5 守护测试：保存离线、状态不撒谎、迟到结果不串篇。
 *
 * 变异验证方式：把 `flush()` 里的远端调用加回来、或让 `applyRemoteCheck`
 * 去掉序号/哈希校验，对应用例必须变红。
 */
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises } from '@vue/test-utils'
import { nextTick } from 'vue'
import type { ArticleStatus } from '@/types/article'

/** 后端替身：记录每次调用的命令名，用于断言「保存路径零远端调用」。 */
const calls: string[] = []

const articleStatus: ArticleStatus = {
  locallySaved: true,
  remoteSync: 'unverified',
  site: 'unverified',
  localBodyHash: 'hash-1',
  writing: { state: 'unverified', reason: '尚未核对' },
  main: { state: 'unverified', reason: '尚未核对' },
}

const content = {
  id: 'demo',
  meta: {
    title: '标题',
    description: '摘要',
    pubDate: '2026-09-27',
    tags: [] as string[],
    draft: true,
  },
  rawFrontMatter: 'title: "标题"',
  body: '正文',
  contentHash: 'hash-1',
  status: { ...articleStatus },
}

/** 可变的核对响应：默认新内容。 */
let checkResponse: { articleId: string; localBodyHash: string; status: ArticleStatus } = {
  articleId: 'demo',
  localBodyHash: 'hash-1',
  status: { ...articleStatus },
}

vi.mock('@/services/backend', async () => {
  const actual = await vi.importActual<typeof import('@/services/backend')>('@/services/backend')
  return {
    ...actual,
    backend: {
      listArticles: vi.fn(async () => {
        calls.push('list_articles')
        return []
      }),
      readArticle: vi.fn(async () => {
        calls.push('read_article')
        return { ...content }
      }),
      saveArticle: vi.fn(async () => {
        calls.push('save_article')
        return { ...content, contentHash: 'hash-2' }
      }),
      checkArticleRemote: vi.fn(async () => {
        calls.push('check_article_remote')
        return checkResponse
      }),
      discardRecovery: vi.fn(async () => {
        calls.push('discard_recovery')
      }),
      snapshotRecovery: vi.fn(async () => {
        calls.push('snapshot_recovery')
      }),
      listTrash: vi.fn(async () => {
        calls.push('list_trash')
        return []
      }),
      pendingRecovery: vi.fn(async () => {
        calls.push('pending_recovery')
        return []
      }),
      getPreferences: vi.fn(async () => {
        calls.push('get_preferences')
        return null
      }),
      connectionStatus: vi.fn(async () => {
        calls.push('connection_status')
        return null
      }),
    },
  }
})

const { useArticle } = await import('@/composables/useArticle')

/** 让自动保存的防抖计时器不再干扰：直接手动 flush。 */
function setup() {
  const view = useArticle()
  return view
}

/** 改正文并等到守卫把状态标为 dirty。 */
async function editBody(view: ReturnType<typeof useArticle>, body: string): Promise<void> {
  view.draftBody.value = body
  await nextTick()
  await nextTick()
}

beforeEach(() => {
  calls.length = 0
  checkResponse = { articleId: 'demo', localBodyHash: 'hash-1', status: { ...articleStatus } }
  vi.useFakeTimers({ shouldAdvanceTime: true })
})

describe('保存路径离线', () => {
  it('保存只写本地，不刷新列表、不核对远端', async () => {
    const view = setup()
    await view.openArticle('demo')
    calls.length = 0

    view.draftBody.value = '改过的正文'
    await editBody(view, '改过的正文')
    await view.flush()
    await flushPromises()

    expect(calls).toContain('save_article')
    expect(calls).not.toContain('list_articles')
    expect(calls).not.toContain('check_article_remote')
  })

  it('打开文章后只异步核对一次，且不刷新整个列表', async () => {
    const view = setup()
    await view.openArticle('demo')
    await flushPromises()

    expect(calls.filter((name) => name === 'check_article_remote')).toHaveLength(1)
    expect(calls).not.toContain('list_articles')
  })

  it('本地内容变化后，旧的远端结论被降级为待核对', async () => {
    const view = setup()
    await view.openArticle('demo')
    // 先给列表一条「已同步」的旧结论。
    view.articles.value = [
      {
        id: 'demo',
        title: '标题',
        description: '摘要',
        tags: [],
        pubDate: '2026-09-27',
        draft: true,
        imageCount: 0,
        source: 'workspace',
        status: {
          locallySaved: true,
          remoteSync: 'saved',
          site: 'live-current-version',
          localBodyHash: 'hash-1',
          writing: { state: 'present', bodyHash: 'hash-1' },
          main: { state: 'present', bodyHash: 'hash-1' },
        },
      },
    ]
    calls.length = 0

    await editBody(view, '新的正文')
    await view.flush()

    const entry = view.articles.value[0]!
    expect(entry.status.remoteSync).toBe('unverified')
    expect(entry.status.site).toBe('unverified')
    expect(entry.status.writing.reason).toBeTruthy()
  })
})

describe('状态不撒谎', () => {
  it('核对失败（未核对）时不显示为已同步', async () => {
    checkResponse = {
      articleId: 'demo',
      localBodyHash: 'hash-1',
      status: {
        locallySaved: true,
        remoteSync: 'unverified',
        site: 'unverified',
        localBodyHash: 'hash-1',
        writing: { state: 'unverified', reason: '写作分支状态待核对：网络不可用或核对超时' },
        main: { state: 'unverified', reason: '网站分支状态待核对：网络不可用或核对超时' },
      },
    }
    const view = setup()
    await view.openArticle('demo')
    await flushPromises()

    expect(view.current.value?.status.remoteSync).toBe('unverified')
    expect(view.current.value?.status.site).toBe('unverified')
    expect(view.current.value?.status.writing.reason).toContain('待核对')
  })

  it('迟到的核对结果不覆盖已切换到的另一篇文章', async () => {
    const view = setup()
    await view.openArticle('demo')
    // 切走：模拟用户在第一次核对返回前又打开了另一篇。
    view.current.value = { ...content, id: 'other', contentHash: 'hash-other' }
    view.articles.value = [
      {
        id: 'demo',
        title: '标题',
        description: '',
        tags: [],
        pubDate: '',
        draft: true,
        imageCount: 0,
        source: 'workspace',
        status: { ...articleStatus, localBodyHash: 'hash-1' },
      },
    ]

    // 现在让第一篇的迟到结果到达：它带的是 demo 的哈希。
    await flushPromises()

    // demo 的列表项可以被更新（内容哈希对得上），但当前文章必须仍是 other。
    expect(view.current.value?.id).toBe('other')
    expect(view.current.value?.status.remoteSync).toBe('unverified')
  })

  it('核对结果与当前磁盘内容不一致时整条丢弃', async () => {
    const view = setup()
    view.articles.value = [
      {
        id: 'demo',
        title: '标题',
        description: '',
        tags: [],
        pubDate: '',
        draft: true,
        imageCount: 0,
        source: 'workspace',
        // 磁盘上已经是 hash-3。
        status: { ...articleStatus, localBodyHash: 'hash-3' },
      },
    ]
    // 迟到结果基于旧内容 hash-1，不应写进列表。
    checkResponse = {
      articleId: 'demo',
      localBodyHash: 'hash-1',
      status: { ...articleStatus, remoteSync: 'saved', site: 'live-current-version' },
    }

    await view.checkRemote('demo')
    await flushPromises()

    expect(view.articles.value[0]!.status.remoteSync).toBe('unverified')
  })
})

describe('保存失败不得丢稿', () => {
  it('保存失败时拒绝切换文章，当前草稿留在编辑器里', async () => {
    const view = setup()
    await view.openArticle('demo')
    calls.length = 0

    // 让保存失败（磁盘满、权限被拒等）。
    const { backend } = await import('@/services/backend')
    ;(backend.saveArticle as unknown as ReturnType<typeof vi.fn>).mockRejectedValueOnce({
      code: 'io-failed',
      message: '磁盘写入失败',
    })

    view.draftBody.value = '还没保存的正文'
    await editBody(view, '还没保存的正文')

    // 用户此时点开另一篇文章：必须被拦下，而不是把未落盘的正文换掉。
    await expect(view.openArticle('other')).rejects.toMatchObject({ code: 'io-failed' })
    expect(view.current.value?.id).toBe('demo')
    expect(view.draftBody.value).toBe('还没保存的正文')
    expect(view.saveState.value).toBe('failed')
  })

  it('保存成功后仍可正常切换文章', async () => {
    const view = setup()
    await view.openArticle('demo')
    calls.length = 0
    view.draftBody.value = '保存好的正文'
    await editBody(view, '保存好的正文')

    await expect(view.openArticle('other')).resolves.toBeUndefined()
    // 已经读过新文章（替身返回的内容 id 固定，这里只断言确实发生了切换）。
    expect(calls).toContain('read_article')
    expect(view.saveState.value).not.toBe('failed')
  })
})

describe('并发核对限流', () => {
  it('连续打开多篇文章只保留最新一篇的核对，不积压任务', async () => {
    const view = setup()
    // 用一个可控的 Promise 让第一次核对挂住，模拟慢网络。
    let release: (() => void) | undefined
    const pending = new Promise<void>((resolve) => {
      release = resolve
    })
    const { backend } = await import('@/services/backend')
    const realCheck = backend.checkArticleRemote as unknown as ReturnType<typeof vi.fn>
    realCheck.mockImplementationOnce(async () => {
      calls.push('check_article_remote')
      await pending
      return { articleId: 'demo', localBodyHash: 'hash-1', status: { ...articleStatus } }
    })

    const first = view.checkRemote('demo')
    // 在第一次还没完成时连续请求另外几篇：只有最后一篇应被排队。
    void view.checkRemote('a')
    void view.checkRemote('b')
    void view.checkRemote('c')

    release!()
    await first
    await flushPromises()

    const checked = calls.filter((name) => name === 'check_article_remote')
    // 第一次 + 排队后实际执行的最后一篇（'c'）= 2 次；中间被覆盖的 a、b 不执行。
    expect(checked).toHaveLength(2)
  })
})

#!/usr/bin/env node
/**
 * 异常关闭（崩溃）恢复验收 —— 用**真实进程被强制结束**验证 A1 与阶段 5 的
 * 「异常关闭恢复测试」。
 *
 * 流程：
 *   1. 在全新临时数据目录中启动可执行文件的 `--crash-session`：它建立隔离仓库、
 *      写入一篇**已保存**的文章，再留下**未落盘**的编辑（只写恢复副本），然后挂起。
 *   2. 等它打印 `CRASH_SESSION_READY` 后，用 `taskkill /F`（非 Windows 用 SIGKILL）
 *      强制结束——进程没有任何机会执行优雅清理。
 *   3. 调用 `--inspect-recovery` 只读检查：恢复副本应含未保存正文、磁盘文章应仍是
 *      崩溃前的旧内容、`writing` 与 `main` 都不应有新提交。
 *   4. 调用 `--restore-recovery` 执行恢复：标题与正文都应取回；恢复后不再有待处理副本。
 *
 * 全程离线、全程在临时目录中，不接触用户配置的真实仓库。
 *
 * 用法：node scripts/test/crash-recovery.mjs
 */

import { execFileSync, spawn } from 'node:child_process'
import { existsSync, lstatSync, mkdtempSync, readdirSync, rmdirSync, unlinkSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const repoRoot = resolve(here, '..', '..')
const exePath = join(
  repoRoot,
  'apps',
  'writer',
  'src-tauri',
  'target',
  'release',
  'guanlanzhi-writer.exe',
)

const ARTICLE_ID = 'crash-session-article'
const UNSAVED_BODY_MARKER = '崩溃前尚未保存的正文'
const SAVED_TITLE = '崩溃会话：已保存的标题'
const UNSAVED_TITLE = '崩溃会话：未保存的标题'

const failures = []
function check(condition, message) {
  console.log(`${condition ? '  ✓' : '  ✗'} ${message}`)
  if (!condition) failures.push(message)
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

/** 运行可执行文件并返回 stdout（允许非零退出码，由调用方断言内容）。 */
function runCli(args, { allowFailure = false } = {}) {
  try {
    return execFileSync(exePath, args, { encoding: 'utf8', timeout: 120000 })
  } catch (error) {
    if (allowFailure) return `${error.stdout ?? ''}`
    throw error
  }
}

/** 强制结束一个进程树，不给它任何清理机会。 */
function forceKill(child) {
  if (process.platform === 'win32') {
    try {
      execFileSync('taskkill', ['/T', '/F', '/PID', String(child.pid)], { stdio: 'ignore' })
      return true
    } catch {
      return false
    }
  }
  try {
    process.kill(child.pid, 'SIGKILL')
    return true
  } catch {
    return false
  }
}

/** 递归删除目录（不依赖 fs.rm，其在部分 Node/Windows 组合下会静默失败）。 */
function removeDir(target) {
  if (!existsSync(target)) return
  if (lstatSync(target).isDirectory()) {
    for (const entry of readdirSync(target)) {
      removeDir(join(target, entry))
    }
    rmdirSync(target)
  } else {
    unlinkSync(target)
  }
}

async function main() {
  if (!existsSync(exePath)) {
    console.error(`未找到可执行文件：${exePath}\n请先运行 pnpm --dir apps/writer tauri build`)
    process.exit(1)
  }
  console.log(`崩溃恢复测试目标：${exePath}`)

  // 全新的数据目录，模拟「新装的软件第一次被使用」。
  const root = mkdtempSync(join(tmpdir(), 'guanlanzhi-crash-'))
  const dataDir = join(root, 'data')
  console.log(`临时数据目录：${dataDir}\n`)

  // ---------- 1. 启动崩溃会话 ----------
  console.log('[1/4] 启动崩溃会话（建立隔离仓库、写入已保存文章、留下未落盘编辑）')
  const child = spawn(exePath, ['--crash-session', dataDir], {
    stdio: ['ignore', 'pipe', 'pipe'],
  })

  let stdout = ''
  let stderr = ''
  child.stdout.on('data', (chunk) => {
    stdout += chunk.toString()
  })
  child.stderr.on('data', (chunk) => {
    stderr += chunk.toString()
  })

  // 等待就绪标记（首次要建仓库，给足时间）。
  const readyDeadline = Date.now() + 90000
  while (Date.now() < readyDeadline && !stdout.includes('CRASH_SESSION_READY')) {
    if (child.exitCode !== null) break
    await sleep(300)
  }

  if (!stdout.includes('CRASH_SESSION_READY')) {
    forceKill(child)
    await sleep(500)
    console.error(`崩溃会话未就绪。\nstdout:\n${stdout}\nstderr:\n${stderr}`)
    process.exit(1)
  }
  check(true, '崩溃会话已就绪（已保存文章 + 留下未落盘编辑）')

  // ---------- 2. 强制结束进程 ----------
  console.log('\n[2/4] 强制结束进程（无优雅清理机会）')
  const killed = forceKill(child)
  await sleep(2000)
  check(killed, '已对崩溃会话执行强制结束')
  check(child.exitCode !== null || child.signalCode !== null, '进程已退出（未存活）')

  // ---------- 3. 只读检查 ----------
  console.log('\n[3/4] 重新启动并检查恢复状态')
  const inspectRaw = runCli(['--inspect-recovery', dataDir, '--json'], { allowFailure: true })
  let inspection = null
  try {
    inspection = JSON.parse(inspectRaw)
  } catch {
    console.error(`无法解析检查结果：\n${inspectRaw}`)
    process.exit(1)
  }

  check(
    inspection.pendingArticleIds?.includes(ARTICLE_ID),
    `重开后提示了未保存的恢复副本（${JSON.stringify(inspection.pendingArticleIds)}）`,
  )
  check(
    inspection.pendingContainsUnsavedBody === true,
    '恢复副本中保留了崩溃前未保存的正文',
  )
  check(
    inspection.diskLacksUnsavedBody === true,
    '磁盘文章仍是崩溃前的旧内容（未保存内容确实没落盘）',
  )
  check(
    inspection.diskTitle === SAVED_TITLE,
    `磁盘标题仍是崩溃前已保存的值（实际：${inspection.diskTitle}）`,
  )
  // A1 的关键不变量：异常关闭不得让远端分支产生新提交。
  check(
    inspection.writingHead === null || inspection.writingHead === undefined,
    '`writing` 分支没有新提交（编辑不触发远端保存）',
  )
  check(
    typeof inspection.mainHead === 'string' && inspection.mainHead.length > 0,
    '`main` 分支保持原样（未被编辑影响）',
  )

  // ---------- 4. 执行恢复 ----------
  console.log('\n[4/4] 执行恢复并核对内容')
  const restoreRaw = runCli(['--restore-recovery', dataDir, ARTICLE_ID], { allowFailure: true })
  let restored = null
  try {
    restored = JSON.parse(restoreRaw)
  } catch {
    console.error(`无法解析恢复结果：\n${restoreRaw}`)
    process.exit(1)
  }

  check(restored.title === UNSAVED_TITLE, `标题已恢复（实际：${restored.title}）`)
  check(
    typeof restored.body === 'string' && restored.body.includes(UNSAVED_BODY_MARKER),
    '正文已恢复为崩溃前未保存的内容',
  )
  check(
    Array.isArray(restored.pendingAfter) && restored.pendingAfter.length === 0,
    '恢复后不再提示未保存内容',
  )
  check(
    restored.writingHead === null || restored.writingHead === undefined,
    '恢复只改本地，`writing` 仍无新提交',
  )
  check(
    restored.mainHead === inspection.mainHead,
    '恢复只改本地，`main` 头与恢复前一致',
  )

  // 数据目录里确实存在恢复区目录（结构可核对）。
  check(existsSync(join(dataDir, 'recovery')), '应用数据目录中存在 recovery 目录')

  // ---------- 清理 ----------
  try {
    removeDir(root)
    check(!existsSync(root), '清理了临时数据目录')
  } catch {
    console.log(`      （未能自动清理 ${root}，可手动删除）`)
  }

  if (failures.length > 0) {
    console.error(`\n崩溃恢复测试失败（${failures.length} 项）：`)
    for (const failure of failures) console.error(`- ${failure}`)
    process.exit(1)
  }
  console.log(
    '\n崩溃恢复测试通过：强制结束后重开，未保存内容被提示且可完整恢复；写作分支与网站分支均无新提交。',
  )
}

main().catch((error) => {
  console.error(`崩溃恢复测试异常：${error.message}`)
  process.exit(1)
})

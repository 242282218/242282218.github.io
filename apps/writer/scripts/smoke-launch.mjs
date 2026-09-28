/**
 * 安装包冒烟测试（阶段 5 验收）。
 *
 * 分两部分：
 *
 * 1. **可启动**：用 `GUANLANZHI_WRITER_DATA_DIR` 指向一个全新的临时数据目录，
 *    确认软件真的启动并完成后端初始化（创建配置、持有单实例锁），
 *    而不只是进程没有立刻退出。
 * 2. **完整通路**：调用同一个可执行文件的 `--self-test`，在本机隔离目录中
 *    真实走一遍「写作 → 插入图片 → 同步到写作分支 → 按篇发布到 main →
 *    预览流水线」。自检不连接真实仓库、不联网，因此可重复执行。
 *
 * 自检**不覆盖**的部分（GitHub 认证、真实推送、Pages 部署、界面与输入法）
 * 会在输出中明确列出，不会当作已通过。
 *
 * 用法：pnpm --dir apps/writer smoke
 */

import { execFileSync, spawn } from 'node:child_process'
import { existsSync, mkdtempSync, readdirSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { BUILD_HINT, exePath } from './lib/writer-exe.mjs'

const failures = []
function check(condition, message) {
  console.log(`${condition ? '  ✓' : '  ✗'} ${message}`)
  if (!condition) failures.push(message)
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

/** 第 2 部分：跑可执行文件内置的自检。 */
function runSelfTest() {
  console.log('\n[2/2] 完整通路自检（--self-test）')
  let stdout = ''
  try {
    stdout = execFileSync(exePath, ['--self-test'], {
      encoding: 'utf8',
      // 自检可能包含若干次 git 调用，给足时间。
      timeout: 180000,
    })
  } catch (error) {
    // 失败时退出码非零；仍然读取输出以便展示每一步的结果。
    stdout = `${error.stdout ?? ''}`
    if (!stdout) {
      check(false, `自检无法执行：${error.message}`)
      return
    }
  }

  console.log(
    stdout
      .split('\n')
      .filter((line) => line.trim())
      .map((line) => `      ${line}`)
      .join('\n'),
  )

  // 逐条核对关键步骤确实通过。
  for (const step of [
    '建立隔离测试仓库',
    '写作：新建、插图、本地保存',
    '同步到写作分支',
    '校验：同步不触碰 main',
    '按篇发布到 main',
    '预览流水线（隔离副本与覆盖）',
    '异常关闭后的恢复副本',
  ]) {
    const line = stdout
      .split('\n')
      .find((candidate) => candidate.includes(step) && candidate.includes('[通过]'))
    check(Boolean(line), `自检步骤通过：${step}`)
  }
  check(stdout.includes('结果：全部通过'), '自检整体结论为「全部通过」')

  // 未覆盖项必须诚实列出。
  for (const caveat of ['GitHub 认证', '网络推送', '预览服务的真实启动', '界面交互']) {
    check(stdout.includes(caveat), `自检明确列出未覆盖项：${caveat}`)
  }
}

async function main() {
  if (!existsSync(exePath)) {
    console.error(`未找到可执行文件：${exePath}\n${BUILD_HINT}`)
    process.exit(1)
  }
  console.log(`冒烟测试目标：${exePath}`)

  // ---------------- [1/2] 可启动 ----------------
  console.log('\n[1/2] 首次启动（全新数据目录）')
  const fakeRoot = mkdtempSync(join(tmpdir(), 'guanlanzhi-smoke-'))
  const appDataDir = join(fakeRoot, 'data')
  check(!existsSync(appDataDir), '启动前应用数据目录不存在（全新环境）')

  const child = spawn(exePath, [], {
    env: { ...process.env, GUANLANZHI_WRITER_DATA_DIR: appDataDir },
    stdio: ['ignore', 'pipe', 'pipe'],
    detached: false,
  })

  let stderr = ''
  child.stderr?.on('data', (chunk) => {
    stderr += chunk.toString()
  })

  let exitedEarly = false
  child.on('exit', () => {
    exitedEarly = true
  })

  await sleep(9000)

  check(!exitedEarly, '进程在启动后保持运行（未立刻崩溃退出）')
  check(existsSync(appDataDir), '创建了每用户应用数据目录')

  if (existsSync(appDataDir)) {
    const entries = readdirSync(appDataDir)
    check(entries.includes('config.json'), '写出了 config.json（后端初始化完成）')

    const configPath = join(appDataDir, 'config.json')
    if (existsSync(configPath)) {
      try {
        const config = JSON.parse(readFileSync(configPath, 'utf8'))
        check(config.repoLabel === 'guanlangzg/guanlangzg.github.io', '目标仓库固定为公开仓库')
        check(config.connected === false, '首次启动保持未连接状态（不擅自克隆）')
        check(!('token' in config) && !('pat' in config), '配置中没有任何凭据字段')
        check(config.schemaVersion >= 1, '配置带版本号（便于后续迁移）')
      } catch (error) {
        check(false, `config.json 可解析为 JSON（实际错误：${error.message}）`)
      }
    }

    check(entries.includes('instance.lock'), '持有工作区排他锁（单实例保护生效）')
  }

  check(!existsSync(join(appDataDir, 'workspace')), '未连接时不创建工作目录（未擅自克隆仓库）')

  if (stderr.trim()) {
    console.log(`      （进程 stderr 输出）\n      ${stderr.trim().split('\n').slice(0, 5).join('\n      ')}`)
  }

  child.kill()
  await sleep(1500)
  if (!exitedEarly) {
    child.kill('SIGKILL')
    await sleep(500)
  }

  try {
    if (process.platform === 'win32') {
      execFileSync('cmd', ['/c', 'rmdir', '/s', '/q', fakeRoot], { stdio: 'ignore' })
    } else {
      execFileSync('rm', ['-rf', fakeRoot], { stdio: 'ignore' })
    }
    check(!existsSync(fakeRoot), '清理了临时数据目录')
  } catch {
    console.log(`      （未能自动清理 ${fakeRoot}，可手动删除）`)
  }

  // ---------------- [2/2] 完整通路 ----------------
  runSelfTest()

  if (failures.length > 0) {
    console.error(`\n冒烟测试失败（${failures.length} 项）：`)
    for (const failure of failures) console.error(`- ${failure}`)
    process.exit(1)
  }
  console.log(
    '\n冒烟测试通过：软件可在全新的用户数据目录中启动，且核心通路（写作／同步／按篇发布／预览流水线）自检全部通过。',
  )
}

main()

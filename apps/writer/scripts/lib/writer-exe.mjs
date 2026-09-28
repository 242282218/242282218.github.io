/**
 * 定位软件可执行文件。
 *
 * Rust 构建产物**不在仓库目录内**：仓库根的 `.cargo/config.toml` 把
 * `target-dir` 指向 `../.build/writer-target`（即仓库的上一级），
 * 单次构建可达数 GB，放进博客目录会拖慢备份、杀毒扫描与全盘搜索。
 *
 * 因此这里不能靠相对路径拼，改为向 Cargo 询问真实位置：
 * `cargo metadata` 会返回应用配置后的 `target_directory`。
 * Cargo 不可用时退回与配置文件一致的位置，保证错误信息仍然可读。
 */

import { execFileSync } from 'node:child_process'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

/** `apps/writer/`，即软件包根目录。 */
export const writerRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..')

/** 仓库根目录（`apps/writer` 上两级）。 */
export const repoRoot = resolve(writerRoot, '..', '..')

const manifestPath = join(writerRoot, 'src-tauri', 'Cargo.toml')

/** 应用构建配置后的 Rust 构建产物目录。 */
export function resolveTargetDir() {
  try {
    const raw = execFileSync(
      'cargo',
      ['metadata', '--format-version', '1', '--no-deps', '--manifest-path', manifestPath],
      { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] },
    )
    const targetDirectory = JSON.parse(raw).target_directory
    if (typeof targetDirectory === 'string' && targetDirectory.length > 0) {
      return resolve(targetDirectory)
    }
  } catch {
    // 未安装 Cargo 或解析失败：退回配置文件声明的默认位置。
  }
  return join(repoRoot, '..', '.build', 'writer-target')
}

/** 未打包的可执行文件路径（支持 `--self-test` 等诊断子命令）。 */
export const exePath = join(
  resolveTargetDir(),
  'release',
  `guanlanzhi-writer${process.platform === 'win32' ? '.exe' : ''}`,
)

/** 构建缺失时给出的可操作提示。 */
export const BUILD_HINT = '请先运行 pnpm --dir apps/writer tauri build'

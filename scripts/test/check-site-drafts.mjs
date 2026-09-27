#!/usr/bin/env node
/**
 * 站点侧草稿过滤与图片输出集成检查。
 *
 * 覆盖方案 §6.2 与 §8.1 对网站端的要求：
 * - `draft: true` 不出现在文章路由、首页、列表与 RSS；
 * - 公开文章在这些位置都出现；
 * - 文章使用的 `public/blog/<article-id>/` 图片被输出到构建结果的对应路径。
 *
 * 实现方式：在临时目录中生成一篇文章样稿草稿与一篇公开样稿（明确标注为测试样稿），
 * 运行真实构建后断言产物，最后删除临时文件并恢复目录原状。
 *
 * 用法（在站点项目根目录执行）：
 *   node scripts/test/check-site-drafts.mjs
 */

import { execFileSync } from 'node:child_process'
import {
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmdirSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const siteRoot = resolve(here, '..', '..')

const PUBLISHED_ID = 'zz-test-published'
const DRAFT_ID = 'zz-test-draft'
const IMAGE_DIR = join(siteRoot, 'public', 'blog', PUBLISHED_ID)

/**
 * 与站点 `src/content.config.ts` 的 schema 一致的 front matter。
 *
 * 日期刻意取远期：首页只展示最近 3 篇（`PostList limit={3}`，按 pubDate 倒序），
 * 样稿必须在其中才能验证首页的草稿过滤。样稿在检查结束后会被删除。
 */
function frontMatter(title, description, draft) {
  return [
    '---',
    `title: "${title}"`,
    `description: "${description}"`,
    'pubDate: "2030-01-15"',
    'tags: [测试样稿]',
    `draft: ${draft}`,
    '---',
    '',
  ].join('\n')
}

function run(command, args) {
  return execFileSync(command, args, {
    cwd: siteRoot,
    stdio: 'pipe',
    encoding: 'utf8',
    shell: process.platform === 'win32',
  })
}

function read(path) {
  return readFileSync(path, 'utf8')
}

const failures = []
const notes = []

function check(condition, message) {
  if (condition) {
    notes.push(`  ✓ ${message}`)
  } else {
    failures.push(message)
    notes.push(`  ✗ ${message}`)
  }
}

/**
 * 递归删除文件或目录。
 *
 * 不使用 `fs.rmSync`：在本项目的 Windows + Node 24 环境中实测它会**静默失败**
 * （不抛错也不删除），导致样稿残留在文章目录。这里用 `unlinkSync`/`rmdirSync`
 * 递归实现，并对结果做断言。
 */
function removePath(target) {
  if (!existsSync(target)) return
  const stat = lstatSync(target)
  if (stat.isDirectory()) {
    for (const entry of readdirSync(target)) {
      removePath(join(target, entry))
    }
    rmdirSync(target)
  } else {
    unlinkSync(target)
  }
}

/** 清理本次运行产生的所有文件。 */
function cleanup() {
  const targets = [
    join(siteRoot, 'src', 'content', 'blog', `${PUBLISHED_ID}.md`),
    join(siteRoot, 'src', 'content', 'blog', `${DRAFT_ID}.md`),
    IMAGE_DIR,
    join(siteRoot, 'dist', 'blog', PUBLISHED_ID),
    join(siteRoot, 'dist', 'blog', DRAFT_ID),
  ]
  const leftovers = []
  for (const target of targets) {
    removePath(target)
    if (existsSync(target)) leftovers.push(target)
  }
  if (leftovers.length > 0) {
    throw new Error(`清理失败，以下路径仍然存在：\n${leftovers.join('\n')}`)
  }
}

function main() {
  let buildFailed = false
  try {
    // 先确保目录干净，避免上一次异常退出留下文件。
    cleanup()

    const publishedPath = join(siteRoot, 'src', 'content', 'blog', `${PUBLISHED_ID}.md`)
    const draftPath = join(siteRoot, 'src', 'content', 'blog', `${DRAFT_ID}.md`)

    // 最小合法 PNG（1×1 透明），验证 public/blog/<id>/ 图片随构建输出。
    const png = Buffer.from(
      'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==',
      'base64',
    )

    mkdirSync(IMAGE_DIR, { recursive: true })
    writeFileSync(join(IMAGE_DIR, 'figure-01.png'), png)

    writeFileSync(
      publishedPath,
      `${frontMatter('测试样稿：公开文章', '用于验证草稿过滤的生成式测试样稿。', false)}\n这是测试样稿正文，不对应真实观澜记录。\n\n![测试图](/blog/${PUBLISHED_ID}/figure-01.png)\n`,
    )
    writeFileSync(
      draftPath,
      `${frontMatter('测试样稿：草稿文章', '用于验证草稿过滤的生成式测试样稿。', true)}\n这篇草稿不应出现在任何公开位置。\n`,
    )

    notes.push('正在执行站点构建（pnpm build）…')
    run('pnpm', ['build'])

    const dist = join(siteRoot, 'dist')
    const home = read(join(dist, 'index.html'))
    const list = read(join(dist, 'blog', 'index.html'))
    const rss = read(join(dist, 'rss.xml'))
    const publishedPage = join(dist, 'blog', PUBLISHED_ID, 'index.html')

    // 1. 公开文章应出现在路由、首页、列表与 RSS。
    check(existsSync(publishedPage), `公开文章生成了静态路由 /blog/${PUBLISHED_ID}/`)
    check(home.includes(PUBLISHED_ID) || home.includes('公开文章'), '首页出现公开文章')
    check(list.includes(PUBLISHED_ID) || list.includes('公开文章'), '列表页出现公开文章')
    check(rss.includes(PUBLISHED_ID) || rss.includes('公开文章'), 'RSS 出现公开文章')

    // 2. 草稿不得出现在任何位置，也不得有路由。
    check(!existsSync(join(dist, 'blog', DRAFT_ID)), `草稿没有静态路由 /blog/${DRAFT_ID}/`)
    check(!home.includes(DRAFT_ID), '首页不含草稿')
    check(!list.includes(DRAFT_ID), '列表页不含草稿')
    check(!rss.includes(DRAFT_ID), 'RSS 不含草稿')
    check(!rss.includes('草稿文章'), 'RSS 不含草稿标题')

    // 3. 图片按 public/blog/<article-id>/ 输出。
    const imageOut = join(dist, 'blog', PUBLISHED_ID, 'figure-01.png')
    check(existsSync(imageOut), `图片输出到 /blog/${PUBLISHED_ID}/figure-01.png`)
    if (existsSync(imageOut)) {
      check(readFileSync(imageOut).equals(png), '图片内容与源文件逐字节一致')
    }
    check(read(publishedPage).includes(`/blog/${PUBLISHED_ID}/figure-01.png`), '文章页引用了图片路径')
  } catch (error) {
    buildFailed = true
    console.error('检查过程失败：')
    console.error(error.stdout ?? error.stderr ?? error.message)
  } finally {
    // 无论成功、断言失败还是异常，都必须移除样稿并重建一次，
    // 使 dist 与实际内容保持一致，不留测试产物。
    try {
      cleanup()
      run('pnpm', ['build'])
      cleanup()
    } catch (error) {
      buildFailed = true
      console.error('清理或重建时出错：')
      console.error(error.stdout ?? error.message)
    }
  }

  console.log(notes.join('\n'))

  if (buildFailed || failures.length > 0) {
    if (failures.length > 0) {
      console.error(`\n站点草稿过滤检查失败（${failures.length} 项）：`)
      for (const failure of failures) console.error(`- ${failure}`)
    }
    process.exit(1)
  }
  console.log('\n站点草稿过滤与图片输出检查通过。')
}

main()

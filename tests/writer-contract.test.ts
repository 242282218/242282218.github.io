/**
 * 站点 ↔ 写作软件契约测试。
 *
 * 写作软件（`apps/writer`）与站点在同一个仓库里，但它们是两个独立安装的项目：
 * 各自有 `node_modules`、各自的技术栈。二者之间靠一份**隐含契约**耦合：
 * 受管目录、front matter 字段、发布闸门脚本、分支名。契约里的任一项漂移，
 * 都要等到用户点「发布」走到构建闸门才会暴露，或者更糟——静默写坏站点。
 *
 * 因此把契约的两侧都从**源文件里解析出来**再比对，让漂移在 `pnpm test` 阶段
 * 就失败，而不是变成运行时事故。
 *
 * 少数失败信息会提示「更新契约」，指的是 `apps/writer/站点契约.md`。
 */

import assert from 'node:assert/strict';
import { existsSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');

function read(relativePath: string): string {
  const absolute = join(repoRoot, relativePath);
  assert.ok(existsSync(absolute), `契约文件缺失：${relativePath}`);
  return readFileSync(absolute, 'utf8');
}

/** 取出 Rust 源文件里 `const NAME: ... = "值";` 形式的字符串常量。 */
function rustStringConst(source: string, name: string): string {
  const match = new RegExp(`const\\s+${name}\\s*:[^=]*=\\s*"([^"]+)"`).exec(source);
  assert.ok(match, `未在写作软件源码中找到常量 ${name}；若已重命名，请同步更新站点契约.md 与本测试`);
  return match![1]!;
}

/** 取出 Rust 数组字面量里的全部字符串元素。 */
function rustStringArray(source: string, name: string): string[] {
  const match = new RegExp(`const\\s+${name}\\s*:[^=]*=\\s*\\[([^\\]]*)\\]`).exec(source);
  assert.ok(match, `未在写作软件源码中找到数组常量 ${name}；若已重命名，请同步更新站点契约.md 与本测试`);
  return [...match![1]!.matchAll(/"([^"]+)"/g)].map((entry) => entry[1]!);
}

const pathsRs = read('apps/writer/src-tauri/src/paths.rs');
const articleIoRs = read('apps/writer/src-tauri/src/article_io.rs');
const previewRs = read('apps/writer/src-tauri/src/preview.rs');
const syncRs = read('apps/writer/src-tauri/src/sync.rs');

const blogDirPrefix = rustStringConst(pathsRs, 'BLOG_DIR_PREFIX');
const imageDirPrefix = rustStringConst(pathsRs, 'IMAGE_DIR_PREFIX');

test('写作软件受管的内容目录与图片目录在站点里真实存在', () => {
  // 软件会直接读写这两个目录，且发布时只提交它们之下的路径。
  assert.ok(
    existsSync(join(repoRoot, blogDirPrefix)),
    `写作软件受管的文章目录不存在：${blogDirPrefix}（软件将无法列出任何文章）`,
  );
  assert.ok(
    existsSync(join(repoRoot, imageDirPrefix)),
    `写作软件受管的图片目录不存在：${imageDirPrefix}（软件将无法存放文章配图）`,
  );
});

test('写作软件写入的文章目录正是站点读取文章的位置', () => {
  // 这是最关键的一处耦合：软件按 BLOG_DIR_PREFIX 写盘，站点按 content
  // collection 的 loader base 读取。两侧一旦不一致，软件会「保存成功」但站点
  // 永远读不到那篇文章——静默失效，用户只会看到文章丢失。
  const loaderBase = /loader:\s*glob\(\s*\{\s*base:\s*'\.?\/?([^']+)'/.exec(
    read('src/content.config.ts'),
  );
  assert.ok(loaderBase, '未从 src/content.config.ts 解析出 content loader 的 base 路径');

  // 统一成「无前导 ./、无尾随 /」的形式再比较，避免写法差异造成误报。
  const normalize = (value: string) => value.replace(/^\.\//, '').replace(/\/+$/, '');
  assert.equal(
    normalize(loaderBase![1]!),
    normalize(blogDirPrefix),
    `写作软件把文章写入 "${blogDirPrefix}"，但站点从 "${loaderBase![1]}" 读取文章；` +
      `软件保存的文章将不会出现在网站上。请更新 apps/writer/站点契约.md 并修正其中一侧`,
  );
});

test('写作软件会改写的 front matter 字段都在站点 schema 里声明', () => {
  const managedFields = rustStringArray(articleIoRs, 'KNOWN_FIELDS');
  assert.ok(managedFields.length > 0, '写作软件的受管字段集为空，契约形同虚设');

  // 站点 schema 定义在 `schema: z.object({ ... })` 内。
  const schemaStart = read('src/content.config.ts').indexOf('z.object({');
  assert.ok(schemaStart >= 0, '未在 src/content.config.ts 中找到 z.object schema');
  const schemaBlock = read('src/content.config.ts').slice(schemaStart);

  const declaredFields = [...schemaBlock.matchAll(/^\s*([A-Za-z_$][\w$]*)\s*:\s*z\./gm)].map(
    (entry) => entry[1]!,
  );
  assert.ok(declaredFields.length > 0, '未从站点 schema 解析出任何字段，解析规则需要更新');

  for (const field of managedFields) {
    assert.ok(
      declaredFields.includes(field),
      `写作软件会改写 front matter 字段 "${field}"，但站点 schema 未声明它；` +
        `两侧契约已漂移，请更新 src/content.config.ts 或 apps/writer/站点契约.md`,
    );
  }
});

test('写作软件发布前的构建闸门要求站点提供 test / check / build 三个脚本', () => {
  // 软件的发布闸门会逐个运行这些脚本，任一失败即停止发布。
  const required = [...previewRs.matchAll(/for\s+script\s+in\s+\[([^\]]+)\]/g)].flatMap((entry) =>
    [...entry[1]!.matchAll(/"([^"]+)"/g)].map((script) => script[1]!),
  );
  assert.ok(
    required.length > 0,
    '未从写作软件的发布闸门中解析出所需脚本，解析规则需要更新',
  );

  const siteScripts = JSON.parse(read('package.json')).scripts as Record<string, string>;
  for (const script of required) {
    assert.ok(
      typeof siteScripts[script] === 'string',
      `写作软件的发布闸门会运行 "pnpm ${script}"，但站点 package.json 未定义该脚本；` +
        `发布将会在闸门处失败`,
    );
  }
});

test('写作软件发布到的分支与站点部署工作流监听的分支一致', () => {
  const mainBranch = rustStringConst(syncRs, 'MAIN_BRANCH');
  const deploying = read('.github/workflows/deploy.yml');
  const branches = [...deploying.matchAll(/branches:\s*\[([^\]]+)\]/g)].flatMap((entry) =>
    [...entry[1]!.matchAll(/([A-Za-z0-9._/-]+)/g)].map((branch) => branch[1]!),
  );
  assert.ok(branches.length > 0, '未从部署工作流中解析出监听分支');
  assert.ok(
    branches.includes(mainBranch),
    `写作软件把文章发布到 "${mainBranch}"，但部署工作流未监听该分支；` +
      `网站将不会更新（工作流实际监听：${branches.join(', ')}）`,
  );

  // 写作分支不能是部署分支，否则未发布的草稿会直接上线。
  const writingBranch = rustStringConst(syncRs, 'WRITING_BRANCH');
  assert.ok(
    !branches.includes(writingBranch),
    `写作分支 "${writingBranch}" 被部署工作流监听；远程草稿会直接上线，违反软件对用户的公开性承诺`,
  );
});

test('站点类型检查排除写作软件目录', () => {
  // 不排除会让 `astro check` 扫描 apps/writer 的 Vue 文件并报 @astrojs/vue 缺失。
  const tsconfig = read('tsconfig.json');
  const excludeMatch = /"exclude"\s*:\s*\[([^\]]*)\]/.exec(tsconfig);
  assert.ok(excludeMatch, '未在根 tsconfig.json 中找到 exclude 列表');
  assert.ok(
    /"apps"/.test(excludeMatch![1]!),
    '根 tsconfig.json 的 exclude 未包含 "apps"；astro check 会扫描写作软件前端并报错',
  );
});

test('写作软件自带的验收脚本存在且可从包内运行', () => {
  // 这些脚本按包内路径解析可执行文件，不能依赖从仓库根调用。
  for (const script of ['scripts/smoke-launch.mjs', 'scripts/crash-recovery.mjs']) {
    const absolute = join(repoRoot, 'apps/writer', script);
    assert.ok(existsSync(absolute), `写作软件缺少验收脚本：apps/writer/${script}`);
  }

  const writerScripts = JSON.parse(read('apps/writer/package.json')).scripts as Record<
    string,
    string
  >;
  for (const name of ['smoke', 'crash-recovery']) {
    assert.ok(
      typeof writerScripts[name] === 'string',
      `apps/writer/package.json 缺少 "${name}" 脚本入口`,
    );
  }
});

test('写作软件的构建产物不在仓库目录内', () => {
  // 单次构建可达数 GB；落在仓库里会拖慢备份、杀毒扫描与全盘搜索。
  const cargoConfig = read('.cargo/config.toml');
  const targetDir = /target-dir\s*=\s*"([^"]+)"/.exec(cargoConfig);
  assert.ok(targetDir, '未在 .cargo/config.toml 中找到 target-dir 设置');

  // Cargo 以 `.cargo/` 所在目录（仓库根）为基准解析 target-dir 的相对路径。
  const resolvedTarget = resolve(repoRoot, targetDir![1]!);
  assert.ok(
    !resolvedTarget.startsWith(repoRoot + sep),
    `写作软件的构建产物仍落在仓库目录内：${resolvedTarget}；` +
      `target-dir 应指向仓库之外（相对仓库根解析）`,
  );

  // 目录存在时确认它确实是个目录（配置写对了但路径指向文件是隐性错误）。
  if (existsSync(resolvedTarget)) {
    assert.ok(statSync(resolvedTarget).isDirectory(), `target-dir 指向的不是目录：${resolvedTarget}`);
  }
});

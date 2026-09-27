# 项目经验

- 本站博客名是「观澜志」，作者网名是「观澜」，GitHub 用户名是 `guanlangzg`。页面标题、品牌与署名使用中文名称，账号名仅用于仓库、网址和 GitHub 链接；不要推断真实姓名。
- 对外定位为个人学习记录，文案克制、不张扬，不称为「技术博客」，避免履历式、作品集式的自我包装。视觉参考 https://brittanychiang.com/#projects 的清晰层级、轻量条目和留白，但沿用本站近白底、深蓝黑文字与蓝色强调，不照搬深色背景或项目内容。

- 发布前运行 `pnpm test && pnpm check && pnpm build`。文章目录只有 `.gitkeep` 时，Astro 会提示集合为空，但类型检查仍为 0 errors / warnings / hints，且会构建首页、文章列表、项目、关于和 RSS。
- Markdown 放入 `src/content/blog/`；`draft: true` 不生成文章静态路由，也不进入首页、列表和 RSS。可用临时公开文章与草稿执行构建验证，发布前删除它们并再次构建。
- GitHub 账号已更名为 `guanlangzg`；用户站点目标为 `guanlangzg/guanlangzg.github.io`，Astro `site` 对应 `https://guanlangzg.github.io`。此前的 `242282218.github.io` 是旧地址，不要将其当作可自动跳转的新站点。
- 本地预览底部的黑色 Menu / Inspect / Audit / Settings 浮层是 Astro Dev Toolbar，不属于站点页面；在 `astro.config.mjs` 设置 `devToolbar: { enabled: false }` 可关闭它，正式构建原本不会包含该工具栏。
- 「观澜」标签图以用户提供的原图为准。裁成 `public/guanlan-logo.png` 的 1000×1000 正方形后，缩放生成 `public/favicon-{16,32,48,64}.png`；不要用旧绘制脚本或重新生成图替换用户原图。小尺寸须保留“观”、水势及朱印的整体关系。

## 本地写作软件（apps/writer）

- 软件位于 `apps/writer/`（Tauri 2 + Vue 3 + Rust），后端只注册 `src-tauri/src/ipc.rs` 中的业务命令；不开放任意 shell／任意文件系统读写／任意 Git 命令。
- 常用命令：`cargo test --manifest-path apps/writer/src-tauri/Cargo.toml`（当前 217 项）、`pnpm --dir apps/writer typecheck`、`pnpm --dir apps/writer test`（当前 68 项）、`pnpm --dir apps/writer build`、`pnpm --dir apps/writer tauri build`（NSIS 安装包）。
- `pnpm test`/`pnpm check`/`pnpm build` 是站点命令；`apps/writer` 有独立脚本，两者不可混用。
- 站点 `tsconfig.json` 必须 `exclude: ["apps"]`，否则 `astro check` 会扫描 `apps/writer` 的 Vue 文件并报 `@astrojs/vue` 缺失。
- Tauri 的 Rust crate 与 `@tauri-apps/api` 需同 minor：`tauri` 解析到 2.11.x 时，`@tauri-apps/api` 用 `2.11.1`，否则 `tauri build` 报版本不匹配。
- **Node 24 的 `fs.rmSync` 在本机 Windows 上会静默失败**（不抛错也不删除）。脚本清理文件要用 `unlinkSync`／`rmdirSync` 递归实现，并对结果断言；不要用 `rmSync` 做清理。
- 在浏览器里直接调试软件前端时，`window.__TAURI_INTERNALS__` 缺失会让 `invoke` 抛 TypeError；前端已识别这种情况并提示「请在软件窗口中操作」。界面夹具在 `apps/writer/harness/`，用假 IPC 桥渲染真实 App 外壳，可验证三栏、冲突、删除确认与回收区。
- Vditor 4.0.0 依赖真实排版引擎，**在 jsdom 中无法初始化**（`getValue` 会因 `currentMode` 未就绪抛错）。`sv↔ir` 往返验收用 `harness/vditor-roundtrip.html` 在真实浏览器中跑，判定规则（`evaluateRoundTrip`）另有单元测试。
- 实测 Vditor `ir` 模式会重排 GFM 表格（单元格补空白、分隔行加长）并在代码块后补空行，单元格内容不变；判定规则把这两种情况归为可接受规范化，其余差异（围栏数、表格行数、行数变化）才回退源码模式。
- 站点草稿过滤与图片输出的端到端检查：`node scripts/test/check-site-drafts.mjs`（临时样稿 + 真实构建，断言后自动清理）。
- 安装包冒烟：`node scripts/test/smoke-launch.mjs`。软件支持 `GUANLANZHI_WRITER_DATA_DIR` 环境变量指定数据目录，用于在不污染真实用户数据的前提下验证首次启动。
- 崩溃恢复验收：`node scripts/test/crash-recovery.mjs`。用 `--crash-session` 起一个进程留下未落盘编辑，再用 `taskkill /T /F` 真实强杀，然后用 `--inspect-recovery` / `--restore-recovery` 核对恢复结果与「两分支无新提交」。这是 A1 与阶段 5「异常关闭恢复测试」的唯一有效证据——进程内的单元测试证明不了「被外部杀掉之后留下什么」。
- 安装包内置自检：`guanlanzhi-writer.exe --self-test [--json]`。它在系统临时目录自建 bare 远端与工作副本（全程离线、不碰用户仓库），走完「写作→插图→同步→按篇发布→预览流水线→崩溃恢复」并逐步断言，同时打印覆盖与未覆盖清单。
- **Windows 上外部命令必须解析可执行文件**：`pnpm`、`corepack` 是 `.cmd` 垫片，`std::process::Command::new("pnpm")` 会失败并被误报成「未安装」。统一用 `util::program_command()` / `resolve_program()`（按 PATH 依次尝试 exe/cmd/bat/com）。
- **不要在预览副本上用 `std::fs::remove_dir_all`**：副本里的 `node_modules` 是指向真实依赖的目录联接，Windows 上可能跟随链接删掉链接目标。统一用 `util::remove_dir_all_no_follow()`（遇联接只删链接本身）。
- 终止预览服务要用**进程树**：`Child::kill()` 只终止 `pnpm.cmd` 垫片，真正监听端口的 node 会变孤儿；Windows 上用 `taskkill /T /F /PID`。
- Vditor 的 `after` 回调不要闭包引用构造中的实例变量（可能同步触发并抛 TDZ）；改为读 `instance.value` 并做幂等收尾。`useVditor` 提供 `loadVditor` 注入口，便于用替身测模式切换与往返判定。
- Vue 组件用 `defineExpose` 暴露方法时，必须同时在模板上写 `ref="..."`，否则调用点全部静默空转（类型检查发现不了）。这类问题只在真实浏览器里驱动一次交互才会暴露。
- 崩溃恢复的三条设计约束：编辑时用比自动保存更短的防抖把内存内容写入恢复区；`pending_recovery` 只返回与磁盘**不同**的条目（避免「已保存却提示未保存」）；恢复前先校验副本可解析，失败则拒绝覆盖。

## 写作软件复审修复（2026-09-27）

首轮交付后的多维度复审发现并修复了一批缺陷，结论与测试见 `docs/观澜志本地写作软件-交付说明.md` §1.1。可复用的经验：

- **不要用「存在性」代替「语义」做守卫**。`main_commit: Option` 曾被当成「是否发布过」，但它是仓库级 `main` 头、任何文章都有值；正确判据是「该文章是否存在于 `main` 树」。同理，检查远端地址一致性时不要写自比表达式（`a != X && a != a` 恒假）。写完守卫要问一句「这个条件有没有可能恒真/恒假」。
- **凡是有「校验」和「副作用」两步的，先校验后副作用**。`workspace.save` 曾先 `atomic_write` 再解析校验，渲染出坏 YAML 时坏文件已落盘。校验必须作用在**将要写入的字节**上。
- **`git add -- <path>` 不关闭 pathspec 通配**。`--` 只终止选项解析；含 `[`、`*` 的文件名会被展开成多个匹配。要按字面路径暂存内容，用 `hash-object -w` + `update-index --cacheinfo`（本项目 sync/publish/trash 三处统一如此）。
- **不受信 ID 与文章 ID 同等对待**。`op_id`、`snapshot id` 这类会拼成路径或命令参数的字符串，必须有自己的白名单校验器（`paths::validate_operation_id`），并在使用前确认它在索引里真实存在。
- **「本机生成的临时资源也要进仓库」时要检查 `.gitignore`**。`dist/` 规则会连带忽略 `public/<lib>/dist/...`；用 `git check-ignore -v <path>` 验证文件真的可被提交，别只看 `git status`（未跟踪目录整体显示为 `??`）。
- **前端的「声明未使用」是一类系统性缺陷**：`defineExpose` 无 `ref`、`prop` 未绑定、`useVditor` 选项未传、偏好无消费者——都属于同一族。类型检查与单测都发现不了，必须在真实浏览器/Tauri 窗口里驱动一次交互。修完一处要顺手排查同类（grep `defineProps`/`defineExpose`/`options` 的所有字段是否有消费点）。
- **发布类操作要有构建闸门**。`PublishEngine.verify_build` 在生产构建恒开、单元测试默认关（夹具站点没有 `node_modules`），另有用例通过 `with_build_check` 专门验证闸门本身；夹具的 `package.json` 需要提供 `test`/`check`/`build` 三个脚本（用 `node -e "process.exit(0)"` 占位，不联网、不产生真实构建产物）。
- **Vditor 默认从 unpkg CDN 注入脚本**（`Constants.CDN`），与 `script-src 'self'` 的 CSP 冲突且离线不可用。修法是把 `node_modules/vditor/dist` 的 Lute／i18n／icons／highlight 主题复制到 `public/vditor/`，并给 Vditor 传 `cdn` 与 `_lutePath`；只保留 UI 实际提供的代码主题（github/monokai/native），别整包 21.9 MB 全拷。本机显示偏好（字号/行距/预览宽度）要写成 CSS 变量并覆盖到 **Vditor 自身的元素**上——它在 `.vditor-sv`/`.vditor-reset` 上硬编码了 16px，仅靠祖先继承会被覆盖。
- **弹窗统一用 `ModalDialog.vue`**：它用 `<Teleport to="body">` 把遮罩移出 `#app`，因此可以对 `#app` 加 `inert` 屏蔽背景而不会连弹窗一起屏蔽（这是关键——若不用 Teleport，`inert` 会把对话框自己也禁用）。同时提供 `role="dialog"`/`aria-modal`/Esc/焦点陷阱/移焦与焦点归还。新增弹窗请直接用它，不要再手写 `.overlay` + `.dialog`。
- **`cmd /c` 的参数数组不提供保护**：cmd 会在执行时二次解析命令行，`&`、`|`、`^`、`%`、`(`、`)`、`!` 都是元字符。`mklink` 是 cmd 内建命令只能走 `cmd /c`，因此拼进去的路径必须先过 `util::cmd_args_are_literal`，不通过就回退到不经 cmd 的实现。`taskkill /PID <数字>` 不受影响。
- **回收区恢复要按记录的确切文件名读取**：正文副本名经 `sanitize_image_basename` 会压平/截断（`观澜/记录` → `观澜`），不同文章可能同名；因此 `TrashEntry.markdown_backup_name` 记录了实际文件名，不要再在目录里「找任意一个 `.md`」。
- **`hasUnsavedChanges` 必须含 `failed`**：保存失败同样意味着磁盘上没有最新内容，关窗前要提示；否则用户会以为已保存。
- **测试覆盖缺口往往源于命名误导**：`push_article(..., adopt_remote)` 的形参名与调用方实参 `adopt_local` 语义相反，直接导致「唯一会覆盖远端的分支」长期零测试。改名后立刻补了用例（未确认不动作 → 确认后覆盖 → 以最新远端头快进 → 不误伤他处文章）。
- **长度限制要按字符计，不要按字节**。`MAX_SEGMENT_LEN` 曾用 `segment.len()`（字节），中文 UTF-8 每字 3 字节，约 34 个汉字就被判「超长」——而这类名字在 Windows 上完全合法。改用 `chars().count()`。凡是与文件名/路径段长度相关的校验都适用这一条。
- **列表扫描不能因单个条目失败而整体失败**。`workspace::scan` / `check_id_available` 曾用 `?` 把 ID 校验错误向上抛，一个超长中文文件名就让软件列不出任何文章、也无法新建。正确做法是逐条容错：不合法的名字单独构造错误条目（带 `loadError`、保留路径作标识），其余条目照常处理。
- **损坏的索引文件要留档再重置**。`VersionIndex::load_or_default` / `load_trash` 曾解析失败即返回空集合且不留档，下一次保存就把空集合写回，用户版本基线/可恢复条目永久丢失。新增 `preserve_corrupt`：**读到了字节却无法解析**时把原文件改名为 `.bak`；读取本身失败（不存在、被占用、权限）**不**留档，否则会把好文件误改名。
- **弹窗统一用 `ModalDialog.vue`**（见下）：新增弹窗不要手写 `.overlay` + `.dialog`。
- **字符串路径校验挡不住符号链接/目录联接**。`paths::validate_*` 只看名字（`..`、绝对路径、保留名）；受管目录里若存在指向外部的链接，`root.join(rel)` 之后的所有读写都会跟随链接落到仓库之外，可覆盖/删除任意文件。防护是 `paths::verify_no_link_escape(root, rel)`：从根逐段 `symlink_metadata`，任一已存在段是链接类对象就拒绝。**新增任何受管读写删入口都要调用它**。注意 Windows 目录联接的 `file_type()` 表现为普通目录，必须查 metadata 的 `FILE_ATTRIBUTE_REPARSE_POINT`（`util::is_link_like`），只判 `is_symlink()` 会漏。
- **cmd 内建命令（`mklink`）的参数里，正斜杠会被当成开关**。`src/content/blog` 会被 cmd 读成 `/content` 并报「无效语法」，导致 `mklink` 失败——本项目中它曾让预览复用 `node_modules` 静默失效。凡是要拼进 `cmd /c` 的路径，先过 `util::cmd_path_arg`（规范化反斜杠 + 拒绝元字符）。
- **哈希基准必须全链路一致**。状态层曾用 `render()` 的规范化重建结果算本地哈希，而同步基线用磁盘原始字节；围栏行带尾随空格时 `render() != 原文`，`local_hash == writing_hash` 永不成立，已同步文章恒显示「未同步」。规则：**本地内容哈希一律取磁盘原文**（`util::hash_file` / 读出的原始字符串），`render()` 只用于「写盘后」的返回值（此时它就是磁盘内容）。
- **写安全相关的回归测试要真建链接**。用 `cmd /c mklink /J`（Windows）或 `std::os::unix::fs::symlink` 建真实目录联接，断言读/写/删/枚举被拒**且仓库外文件逐字节未变**；能创建失败时打印 `[跳过]` 并 return（CI 环境可能不允许）。这类测试必须做变异验证（临时去掉 guard 确认会失败），否则「通过」可能只是因为夹具没建成链接。


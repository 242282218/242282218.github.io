# 项目经验

本轮写作软件复审的逐条修复结论在 `docs/观澜志本地写作软件-交付说明.md` §1.1，仓库边界在 §1.2，二阶段施工的修复与未验证项在 §1.3 与 §5.1；此处只留可复用的命令、约束与排障结论。

## 站点（观澜志）

- 博客名「观澜志」，作者署名「观澜」，GitHub 用户名 `guanlangzg`。页面标题、品牌与署名用中文名，账号名只用于仓库、网址与链接；不要推断真实姓名。
- 对外定位是个人学习记录：文案克制，不称「技术博客」，避免履历式或作品集式包装。视觉参考 https://brittanychiang.com/#projects 的层级、轻量条目与留白，但沿用本站近白底、深蓝黑文字与蓝色强调，不照搬其深色背景与项目内容。
- 站点 URL 与 Astro `site` 都是 `https://guanlangzg.github.io`；`242282218.github.io` 是更名前旧地址，不会自动跳转。
- 文章是 `src/content/blog/` 下的普通 Markdown + 简洁 front matter；`draft: true` 不进文章路由、首页、列表与 RSS。
- 站点 `tsconfig.json` 必须 `exclude: ["apps"]`，否则 `astro check` 会去扫描 `apps/writer` 的 Vue 文件并报 `@astrojs/vue` 缺失。
- 本地预览底部黑色 Menu / Inspect / Audit / Settings 浮层是 Astro Dev Toolbar（`astro.config.mjs` 中 `devToolbar: { enabled: false }` 关闭），不属于站点页面，正式构建不含它。
- 「观澜」标签图以用户原图为唯一来源：缩成 1000×1000 的 `public/guanlan-logo.png` 后导出 `public/favicon-{16,32,48,64}.png`。不要用旧绘制脚本或另生成的图替换原图；小尺寸要保住「观」、水势与朱印的整体关系。

## 站点 ↔ 软件（apps/writer）

- **依赖是单向的**：软件依赖站点（受管目录 `src/content/blog`、front matter 字段、发布闸门要跑的三个脚本、`writing` 分支），站点不依赖软件；软件运行时在本机应用数据目录自建 clone，也不依赖这个仓库。硬编码站点路径与分支说明它是本博客专用工具，等它要服务第二个站点或有独立发布节奏时再拆仓。
- **契约是机器检查，不是文档约定**：`tests/writer-contract.test.ts` 从两侧源文件解析真实值（Rust 常量用正则，站点侧读 `content.config.ts` / `package.json` / 工作流）再比对，漂移在 `pnpm test` 阶段就失败；比较路径先归一化（去掉 `./` 与尾斜杠）。人读的契约在 `apps/writer/站点契约.md`。
- **最隐蔽的耦合是目录字面值**：软件写 `src/content/blog/`，站点 `glob({ base: './src/content/blog' })` 读同一处；不一致时软件「保存成功」而站点永远读不到，用户只看到文章丢失——存在性检查抓不到（目录确实存在），必须比对两侧字面值。
- 新增的守护测试必须做变异验证（逐项制造漂移确认能变红），否则「通过」可能只是断言没生效。
- 两边保持独立安装，不合并 pnpm workspace（技术栈与依赖图不同，收益不抵复杂度）。

## 本地写作软件（apps/writer）

- Tauri 2 + Vue 3 + Rust。后端只注册 `src-tauri/src/ipc.rs` 中的业务命令，不开放任意 shell、任意文件系统读写或任意 Git 命令。
- 命令（2026-09-28 复核：Rust 239 项、前端 99 项／9 文件）：
  - Rust：`cargo test --manifest-path apps/writer/src-tauri/Cargo.toml`
  - 前端：`pnpm --dir apps/writer typecheck` / `test`（vitest）/ `build`
  - 打包与进程级验收：`pnpm --dir apps/writer tauri build`，然后 `smoke`（`scripts/smoke-launch.mjs`）、`crash-recovery`（`scripts/crash-recovery.mjs`）
  - 安装包自检：`guanlanzhi-writer.exe --self-test [--json]`，在系统临时目录自建 bare 远端与工作副本，全程离线走完写作→插图→同步→按篇发布→预览流水线→崩溃恢复并逐步断言。
  - 站点命令是 `pnpm test` / `pnpm check` / `pnpm build`，与软件命令不可混用。
- **专属脚本跟着项目走，跨界检查留在站点根**：软件脚本放 `apps/writer/scripts/`（只依赖自己的产物），`scripts/test/check-site-drafts.mjs` 用真实站点构建验证草稿过滤与图片输出，留在站点根。**不要写死可执行文件路径**，用 `apps/writer/scripts/lib/writer-exe.mjs` 向 `cargo metadata` 的 `target_directory` 取。
- 崩溃恢复只有进程级验收算数：`crash-recovery` 用 `--crash-session` 留下未落盘编辑，`taskkill /T /F` 真实强杀，再用 `--inspect-recovery` / `--restore-recovery` 核对恢复结果与「两分支无新提交」。进程内单测证明不了「被外部杀掉之后留下什么」。
- 软件支持 `GUANLANZHI_WRITER_DATA_DIR` 指定数据目录，可在不污染真实用户数据的前提下验证首次启动。

### 平台与进程（Windows）

- **Node 24 的 `fs.rmSync` 在本机 Windows 上静默失败**（不报错也不删除）。脚本清理要用 `unlinkSync` / `rmdirSync` 递归实现并对结果断言。
- **外部命令必须解析可执行文件**：`pnpm`、`corepack` 是 `.cmd` 垫片，`Command::new("pnpm")` 会失败并被误报成「未安装」。统一走 `util::program_command()` / `resolve_program()`（按 PATH 依次尝试 exe/cmd/bat/com）。
- **`cmd /c` 的参数数组不提供保护**：`&`、`|`、`^`、`%`、`(`、`)`、`!` 都是 cmd 元字符，`mklink` 是内建命令只能走 `cmd /c`，拼入的路径必须先过 `util::cmd_args_are_literal`，不通过就退回不经 cmd 的实现。同因，cmd 会把 `src/content/blog` 中的正斜杠读成开关而使 `mklink` 失败（曾让预览复用 `node_modules` 静默失效），路径先过 `util::cmd_path_arg`。`taskkill /PID <数字>` 不受影响。
- **打开软件弹黑窗的机制线索在子进程**：release 主程序已是 GUI 子系统（`main.rs` 的 `windows_subsystem = "windows"`），但启动路径会执行 git/node/pnpm 探测，直接创建的 `Command` 尚未统一设置 `CREATE_NO_WINDOW`。修复时在 Windows release 窗口逐个确认来源，再给需要静默的子进程统一设置该标志；`pnpm` 可能经 `.cmd` 由 cmd 执行。不能给所有子进程机械设置 `Stdio::null()`：`publish.rs` 存在有意使用的 stdin 管道。CLI 诊断命令需实测 `AttachConsole(ATTACH_PARENT_PROCESS)` 及输出/退出码。
- 终止预览服务要杀**进程树**：`Child::kill()` 只终止 `pnpm.cmd` 垫片，真正监听端口的 node 变孤儿，用 `taskkill /T /F /PID`。
- **不要在预览副本上用 `remove_dir_all`**：副本里的 `node_modules` 是指向真实依赖的目录联接，Windows 上会跟随链接删掉链接目标；统一用 `util::remove_dir_all_no_follow()`。
- 构建产物不落在仓库里：仓库根 `.cargo/config.toml` 设 `target-dir = "../.build/writer-target"`（原本 `src-tauri/target/` 有 8.6 GB，拖慢备份与全盘搜索）。**`target-dir` 的相对路径以 `.cargo` 所在目录为基准解析**，不是 manifest 也不是 cwd——放仓库根才能同时覆盖「仓库根跑 `cargo test --manifest-path …`」与「`apps/writer` 跑 `tauri build`」。
- **搬 target 目录会让 tauri 的 build script 缓存失效**（本次实测复现）：`tauri` crate 的 build script 把 build 目录的**绝对路径**烘焙进 `output` 与 `root-output`。搬走后这些路径指向已删除的旧目录，但 Cargo 指纹只看输入、不检测 target 被移动，于是**一直命中缓存**不报错；直到某个输入（如 `capabilities/default.json`）变化使 build script 重新执行，才报「找不到 …/target/…/permissions/….toml」。修法是 `cargo clean -p tauri`（只清该 crate，release 产物不受影响），不要 `cargo clean` 全清。
- 同盘符 `Move-Item` 搬 `target/` 是重命名，缓存可完整保留；但**搬后 release 缓存失效**：tauri 构建脚本把旧绝对路径烘焙进 `build/tauri-*/out/`，Cargo 指纹只看输入、不检测 target 目录被移动，于是复用陈旧产物、`tauri build` 报「找不到权限文件」。修法是 `cargo clean --release`（debug 缓存不受影响）后重建。**验证搬移没坏必须真跑一次 `tauri build`，只跑 `cargo test`（debug）会漏。**

### 卡顿的结构性来源（2026-09-27 定位，2026-09-28 已修复——下列保留的是「怎么定位」）

> 这三条的具体修法已完成（保存路径离线、状态三态协议、重型命令移入 blocking 池），
> 见交付说明 §1.3。留下的是**定位方法**，不是待办。

- **后端本来就是 Rust，换语言不解决重复工作**：38 个业务 `#[tauri::command]` 是同步 `fn`；锁定的 Tauri 2.11.6 同步包装层直接调用命令，可能阻塞主线程。联网 `git fetch`、逐篇 `git show`、扫描及预览依赖安装在 IPC 路径上；实际卡顿程度仍须真实窗口测量。优先削减保存后重复工作，剩余重型命令按热点移到 blocking 池，而非全量改 async。
- **自动保存会触发重型列表刷新，是优先排查的单点**：`useArticle.ts` 保存成功后 `await refreshList()` → `list_articles` → `remote_snapshots` 尝试两分支 `fetch`，对每篇可读文章在两分支各尝试一次 `git show`，并叠加工作区扫描。实际子进程次数随分支、文章及错误情况变化；默认 800ms 保存防抖。**远端快照应与本地保存脱钩**：打开文章后异步核对该篇、手动刷新或同步发布预检时再读取远端；核对须有超时，未知/失败/确认不存在要区分。旧状态不能冒充当前已同步或已上线。
- **恢复快照的 fsync 取舍尚未获确认**：`write_recovery` 走 `atomic_write`（含 `sync_all`），编辑时约每 400ms 一次。强杀后仍能恢复不能证明断电耐久性；应先测量瓶颈，确需降低写盘耐久性时单独说明损失窗口并确认，正文文件仍须保持耐久。
- **`opt-level = "s"` 是按体积优化**，是否拖慢当前哈希/解析/扫描热点须用 release 基准对照，不能预设为根因。改 profile 后正常增量 `tauri build` 验证；前文 `cargo clean --release` 仅针对移动 target 后的绝对路径残留。
- 文档里写过的取舍要用代码复核：交付说明 §7 曾称「避免列表刷新时对每篇文章发起多次 Git 读取」，而当时 Markdown 路径上每篇在两分支都可能尝试 `git show`。2026-09-28 复核时该取舍已落实（`remote_snapshots` 不再隐式 fetch，只读仍可证明有效的缓存）。**教训是通用的：文档声称的优化必须在代码里指得出对应实现。**

### 站点 ↔ 软件的排版一致性

- 站点正文样式入口是 `src/styles/global.css`，由 `src/layouts/BaseLayout.astro` 引入；真实文章页还受 `.shell/.article`、全局标签规则与媒体查询影响，不能只抽 `:root/.prose` 就认为排版一致。
- `astro.config.mjs` 无 `markdown` 字段；锁定的 Astro 7.3.5 默认处理器是 Sätteri，含 GFM/智能标点与 Shiki `github-dark` 高亮。其他解析器应对照真实 Astro 页面检查标题锚点、图片、表格与代码，而不是承诺逐字节一致。
- `.prose pre` 的 `background:#e9f1f6 !important; color:#25435b` 覆盖代码块背景，**不覆盖 Shiki 子元素的内联语法颜色**；预览需要同时对照背景与前景高亮。
- 站点字体栈含从 Google Fonts 加载的 Noto Sans SC 与系统回退；离线时可能用微软雅黑，实际字形/换行取决于网络及 WebView，须用页面对照，不能预设差异幅度。

### 后端不变量

- **守卫要看语义，不看存在性**。`main_commit: Option` 曾被当成「是否发布过」，但它其实是仓库级 `main` 头、任何文章都有值；正确判据是「该文章是否存在于 `main` 树」。写完守卫要自问「这条件有没有可能恒真或恒假」（别写成 `a != X && a != a` 这种恒假式）。
- **先校验、后副作用**，且校验要作用在**将要写入的字节**上：`workspace.save` 曾先 `atomic_write` 再解析校验，渲染出坏 YAML 时坏文件已落盘。
- **哈希基准必须全链路一致**：本地内容哈希一律取**磁盘原文**（`util::hash_file`），`render()` 只用于写盘后的返回值。状态层混用会让已同步文章永远显示「未同步」（围栏行带尾随空格时 `render() != 原文`）。
- **字符串路径校验挡不住符号链接/目录联接**：`paths::validate_*` 只看名字，受管目录里若有指向外部的链接，`root.join(rel)` 之后的所有读写删都会落到仓库之外。受管读写删入口都要先调 `paths::verify_no_link_escape(root, rel)`（从根逐段 `symlink_metadata`）。Windows 目录联接的 `file_type()` 表现为普通目录，必须查 metadata 的 `FILE_ATTRIBUTE_REPARSE_POINT`（`util::is_link_like`），只判 `is_symlink()` 会漏。
- **不受信 ID 与文章 ID 同等对待**：`op_id`、`snapshot id` 这类会拼成路径或命令参数的字符串要有自己的白名单校验器（`paths::validate_operation_id`），并在使用前确认它真在索引里。
- **按字面路径暂存内容不能用 `git add -- <path>`**：`--` 只终止选项解析、不关闭 pathspec 通配，含 `[`、`*` 的文件名会被展开成多个匹配。要用 `hash-object -w` + `update-index --cacheinfo`（sync/publish/trash 三处统一如此）。
- 回收区恢复按记录的确切文件名读：正文副本名经 `sanitize_image_basename` 会压平/截断（`观澜/记录` → `观澜`），不同文章可能同名，所以看 `TrashEntry.markdown_backup_name`，不要在目录里「找任意一个 `.md`」。
- **损坏的索引要留档再重置**：`VersionIndex::load_or_default` / `load_trash` 曾解析失败即返回空集且不留档，下次保存把空集写回，版本基线与可恢复条目永久丢失。规则是**读到了字节却解析失败**时把原文件改名 `.bak`；读取本身失败（不存在、被占用、权限）**不**留档，否则会误改好文件。
- **列表扫描逐条容错，不要一条失败整体失败**：`workspace::scan` / `check_id_available` 曾用 `?` 上抛，一个超长中文文件名就让软件列不出任何文章、也新建不了。不合法条目单独构造（带 `loadError`、保留路径作标识），其余照常处理。
- **「保存失败就停下」要覆盖所有后续动作，不只远端操作**：`openArticle` 曾在 `await flush()` 后无条件读入新文章，而 `flush()` 把失败降级为界面状态**不抛错**——于是保存失败后切篇会把上一篇未落盘的正文替换掉（只留恢复副本）。修法与 `flushBeforeRemote` 同源：`flush()` 后检查 `saveState === 'failed'` 就抛错中断。**排查口径**：凡是 `await flush()` 之后还要动当前文章状态的地方，都要问一句「失败了会怎样」。
- **输入法组合态（IME）要接到交互外壳，不能只在编辑器内保护**：编辑器在组合期间不发 `update:modelValue` 还不够——外壳若在组合中切篇文章，整体替换文档会打断输入法会话。正确做法是编辑器 `compositionstart` **立即**通知外壳（不等第一次 `compositionupdate`），外壳在组合期间拒绝切篇并排队，组合结束后自动执行；编辑器另在组合中**挂起**外部内容替换，结束时落地新内容、丢弃属于旧文章的合成文本。
- **`git_with_timeout` 必须并发读管道**：先等子进程退出再读 stdout/stderr，在输出超过管道缓冲（Windows 默认 64 KiB）时会与子进程互等，最终被误报成「远端核对超时」。夹具要有**大输出**样本（如 4000 个已跟踪文件的 `git ls-files`），且超时值设在几十秒——无并发读取时用例会干脆利落地在超时后变红，而不是无限挂住。
- **「既有 Git 仓库」的信任判据是「谁建的」，不是「像不像开发目录」**：只校验 `origin` 会放行同一仓库的个人 clone 与 linked worktree（它们没有 `.zcode` 之类开发标记），而软件随后会在那里同步/发布/删除——直接推真实远端、删真实文件。判据改为「只有应用数据目录内的仓库才采用」。
- **字符串前缀检查挡不住链接**：`fs::write` 会跟随符号链接/目录联接，用户可选一个指向工作区的链接作为「导出目标」而通过「不在工作区内」的判断。要从最近的存在祖先逐段 `symlink_metadata` 复核（`util::verify_output_path_not_link`）。**夹具注意**：Windows 上 `symlink_file` 需要开发者模式/管理员权限（实测报 `os error 1314`），测试会走「跳过」分支而**静默通过**；目录联接（`mklink /J`）无需特权，是可靠的夹具类型。
- **长度校验按字符计，不按字节**：`MAX_SEGMENT_LEN` 用 `segment.len()` 时中文约 34 字就被判「超长」（UTF-8 每字 3 字节），而这类名字在 Windows 上完全合法；改用 `chars().count()`。文件名与路径段的长度校验都适用。
- **发布类操作要有构建闸门**：`PublishEngine.verify_build` 生产构建恒开、单元测试默认关（夹具站点没有 `node_modules`），另有 `with_build_check` 用例专门验证闸门本身；夹具 `package.json` 用 `node -e "process.exit(0)"` 占位 `test`/`check`/`build`。

### 前端

- **「声明未使用」是一族系统性缺陷**：`defineExpose` 没在模板上写对应 `ref="..."`（调用点全部静默空转）、`prop` 未绑定、`useVditor` 选项未传、偏好无消费者，都属于同一族。类型检查与单测都发现不了，必须在真实浏览器或 Tauri 窗口里驱动一次交互；修完一处要 grep 同类字段是否都有消费点。**2026-09-27 二阶段复审该族再次复发**：`usePreviewStyles` 暴露的 `cssVariables`（样式抽屉的字号/行距）零消费者、`EditorContextMenu` 的 `export-markdown` 事件零监听、`theme.css` 的 `:root.dark` 零触发点。结论：**给组件加 emit/expose/computed 时，同一批改动里必须落一个消费点并写一条断言**，否则下一轮复审还会以不同形式出现。
- **验证「消费点」的测试不能只看源码文本**：`expect(source).toMatch(/deleteSlashTrigger\(props\.handle\.view/)` 这类断言在 `if (false && …)` 下**照样通过**（实测）。要**挂载真实组件、派发真实事件、断言可观察结果**（如「正文里不再残留过滤词」），否则守护形同虚设。
- **弹窗统一用 `ModalDialog.vue`**：它用 `<Teleport to="body">` 把遮罩移出 `#app`，因此可对 `#app` 加 `inert` 屏蔽背景而不会连弹窗一起屏蔽（不用 Teleport 时 `inert` 会把对话框自己也禁用）；并已提供 `role="dialog"` / `aria-modal` / Esc / 焦点陷阱 / 移焦与焦点归还。新弹窗不要再手写 `.overlay` + `.dialog`。
- **面板分隔条用 reka-ui 的 Splitter 原语，比例交给 `autoSaveId`**：`SplitterGroup`/`SplitterPanel`/`SplitterResizeHandle`（`reka-ui@2.10.5` 有这三个导出）自带拖动、键盘与触屏交互，并把比例写进 `localStorage`；**不要再自存一份宽度**，两处各存一份会立刻不一致。条件渲染面板必须给 `order`（reka-ui 用它排序）。面板自身是 `flex` 元素，内容容器要显式 `flex: 1` 才撑满。
- **外壳的「双屏」必须是横向 group**：编辑与预览用 `direction="horizontal"` 并排，且 `.editor-row` 不能再声明 `flex-direction: column`——曾经是纵向堆叠，与施工单的三段式不符，只看 DOM 存在与否发现不了，必须读 `getBoundingClientRect` 的 x/宽。
- **窄屏分支要单独检查功能可达性**：改三栏布局时容易让窄屏只剩编辑器（用户无法选文章）。窄屏应保留整宽列表 + 折叠按钮；写布局改动后要**分别**核对窄屏与桌面两条分支。
- 在浏览器里直接调试前端时没有 `window.__TAURI_INTERNALS__`，`invoke` 会抛 TypeError（前端已识别并提示「请在软件窗口中操作」）。界面夹具在 `apps/writer/harness/`，用假 IPC 桥渲染真实 App 外壳，可验证三栏、冲突、删除确认与回收区。
- ~~Vditor 4.0.0 依赖真实排版引擎，在 jsdom 中无法初始化~~（**Vditor 已于二阶段退役**，以下 Vditor 条目仅作历史记录，不再适用于当前代码）。当时 `getValue` 会因 `currentMode` 未就绪抛错，`sv↔ir` 往返验收只能在真实浏览器跑。- ~~实测 Vditor `ir` 模式会重排 GFM 表格~~ 判定规则把表格填充与空行差异归为可接受规范化。**这句话的现时意义**：新内核（CodeMirror）不重排 Markdown，因此该判据已被「打开→不编辑→保存保持磁盘原文」取代——**换成不重排的内核后，旧判据要整体作废而不是保留**。
- ~~Vditor 默认从 unpkg CDN 注入脚本~~ 当时的修法是把 `node_modules/vditor/dist` 复制到 `public/vditor/dist` 并传 `cdn`/`_lutePath`。**通用教训**：从 CDN 注入脚本的第三方编辑器与 `script-src 'self'` CSP 冲突且离线不可用，只能随构建分发；退役该编辑器后要连这份分发资源与其 `.gitignore` 例外一起删。
- 本机显示偏好（字号 / 行距）当时要覆盖到 **Vditor 自身元素**上，因为它在 `.vditor-sv` / `.vditor-reset` 硬编码了 16px。**换成 CodeMirror 后同一条约束依然成立但要落到 `.cm-content` 上**；而预览侧的字号/行距见下面「站点样式快照」一节——那里是**另一个坑**（跨 iframe 边界）。
- ~~Vditor 的 `after` 回调不要闭包引用构造中的实例变量~~ **通用教训**：异步/可能同步触发的初始化回调里不要闭包引用构造中的实例变量，改为读 `instance.value` 并做幂等收尾。
- 崩溃恢复的三条设计约束：编辑时用比自动保存更短的防抖把内存内容写入恢复区；`pending_recovery` 只返回与磁盘**不同**的条目（避免「已保存却提示未保存」）；恢复前先校验副本可解析，失败则拒绝覆盖。
- `hasUnsavedChanges` 必须含 `failed`：保存失败同样意味着磁盘上没有最新内容，关窗前要提示，否则用户会以为已保存。

### 测试

- **覆盖缺口常源于命名误导**：`push_article(..., adopt_remote)` 的形参名与调用方实参 `adopt_local` 语义相反，直接导致「唯一会覆盖远端的分支」长期零测试；改名后立刻补了用例。看到零测试的分支先怀疑命名。
- **写安全相关的回归测试要真建链接**：用 `cmd /c mklink /J`（Windows）或 `std::os::unix::fs::symlink` 建真实目录联接，断言读/写/删/枚举被拒**且仓库外文件逐字节未变**；创建失败时打印 `[跳过]` 并 return（CI 环境可能不允许）。这类测试必须做变异验证（临时去掉 guard 确认会失败），否则「通过」可能只是因为夹具没建成链接。
- **「跳过」分支必须自己验证夹具能建成**：导出逃逸测试最初用 `symlink_file`，本机因权限建不成、静默 `[跳过]` **并通过**——删掉被守护的代码也照样绿。判断夹具可用性要在测试外先探测一次（写个一次性小程序打印创建结果），别假设「能编译就能建链接」。
- **jsdom 缺 `Range.getClientRects` 会把 CodeMirror 的测试输出淹没**：每次渲染都刷 `TypeError: textRange(...).getClientRects is not a function`，虽不致命但真失败会被埋掉。在 `vitest.config.ts` 的 `setupFiles` 里补 `Range.getClientRects` / `getBoundingClientRect`（返回空矩形是安全下界）与 `ResizeObserver` 空实现，测试输出立刻干净。
- **「本机生成的临时资源也要进仓库」时要检查 `.gitignore`**：`dist/` 规则会连带忽略 `public/<lib>/dist/...`；用 `git check-ignore -v <path>` 验证文件真可被提交（`git status` 对未跟踪目录整体只显示 `??`，看不出来）。

### 编辑器内核与 Markdown 原文（2026-09-27 二阶段）

- **CodeMirror 6 在 `EditorState` 内部固定用 `\n` 存行，不保留 CRLF**：`Text.of` 不接收分隔符，`EditorState.lineSeparator` 只影响**变更解析**、不影响文档存储与 `toString()`。实测 `\r\n` 会被静默抹掉，因此「打开→不编辑→保存字节不变」必须在编辑器**外面**转换（进出两侧各一次，`setValue` 时切换分隔符风格）。这条只能靠读源码确认（`Text.of(string.split(facet || DefaultSplit))`），不要假设 facet 能控制往返。
- **Vditor 退役后 `sv↔ir` 往返判定不再是验收标准**：CodeMirror 编辑原文、不重排 Markdown，新判据是「打开→不编辑→保存保持磁盘原文」。`vditor-roundtrip` 的 17 项测试与专属夹具一并删除，替代断言在 `src/tests/editor-codemirror.test.ts`（含 CRLF 与围栏尾随空格样本）。
- **菜单栏、`/` 斜杠命令、右键菜单必须共用同一批命令函数**（`@/editor/commands`）：三条入口各自实现会让「加粗」在不同路径下行为分叉。格式化一律做**切换**语义（已包裹再点一次移除），标题级别是替换而非叠加前缀。
- 代码块语言按需静态导入并走 `codeLanguages` 的**回调形式**：`LanguageDescription[]` 要求 `load`/`loadFunc` 懒加载描述符，而 `@codemirror/language-data` 整包会把几十种语言的解析器都打进产物。

### 子进程静默与 CLI 诊断（Windows）

- **黑窗的来源是软件启动的控制台子进程，不是主程序自己**：release 已是 GUI 子系统。修法是给每个控制台子进程设 `CREATE_NO_WINDOW`，并把它收口到**唯一入口**（`util::program_command` 内部调用 `hide_console`）。`creation_flags` 是**整体赋值**，散落使用会互相覆盖这一位，因此要断言「整个文件只有一处 `creation_flags` 写入」。
- **守护测试要扫源码，不要靠人工核对**：`Command::new` 的裸调用点会随重构重新出现。用一个测试遍历生产源码（`#[cfg(test)]` 之前的部分）统计 `Command::new` 命中数，只给 `util.rs` 开白，新增违规点立刻变红。注意测试文件自己要读源码时，`split("#[cfg(test)]")` 必须在**自己的文件**里也生效，否则会把断言文本算进去。
- **`std::process::Command` 没有读取 `creation_flags` 的公开接口**（`get_creation_flags` 不存在），运行时断言标志位不可行；改为对「唯一入口必须调用 `hide_console`」做静态断言。
- **`CREATE_NO_WINDOW` 不影响管道**：`git hash-object -w --stdin` 这类依赖 stdin 传内容的调用在静默后仍可用，可以用一次真实调用断言（比只读代码更可信）。
- **GUI 子系统的 CLI 要显式 `AttachConsole(ATTACH_PARENT_PROCESS)` 并重绑 stdout/stderr 到 `CONOUT$`**，且必须早于任何输出（Rust 的 `Stdout` 首次使用时缓存句柄），退出前显式 flush。父进程没有控制台时要静默失败，不能影响双击启动。

### 状态不撒谎：未知 ≠ 否定

- **`Option<T>` 表达不了「没查过 / 查了失败 / 确认不存在」**：旧实现用 `.ok().flatten()` 把三者混成 `None`，界面于是把未知显示成「已同步」或「从未发布」。正确做法是显式的三态（`unverified` / `absent` / `present`），并把**原因**（超时、断网、认证失败）与**所依据的远端头**一起带出来。
- **两个远端分支必须分别核对**：只成功一支不能推定另一支。测试要覆盖「一支失败时另一支结论照常生效」。
- **缓存只在能证明有效时复用**：先能读到当前跟踪头、且与记录的头一致，才允许用旧结论；否则降级为未核对。远端可能已前进，「上次查过」不是「现在成立」。
- **把网络检查从保存路径上摘掉，收益立刻可见**：`flush()` 里那句 `await refreshList()` 会让每次自动保存都触发两分支 `git fetch` + 逐篇 `git show` + 全工作区扫描。移除后保存只写本地，用保存返回值更新本地字段即可——但**不要照搬服务端返回的默认快照**当远端事实。
- **异步化必须配迟到结果丢弃**：请求序号 + 本地内容哈希双重校验，任一不过整条丢弃。状态是整体替换而不是逐字段合并，否则会出现「新正文 ＋ 旧远端结论」。

### 站点样式快照（预览用）

- **预览样式只能来自构建期抽取的受控快照，不能在运行时读工作区 CSS**：工作区内容可变，`@import`/`url()`/逃逸选择器会引入资源加载与样式注入风险，而 webview 具备本地命令权限。
- **抽取要按选择器白名单解析，不做字符串替换**，并且**不要**把站点选择器改写成「预览根元素的后代」：站点文章页结构是 `.shell > .article > .prose`，`.shell`/`.article` 是 `.prose` 的**祖先**，前缀式限定永远匹配不到，会静默丢掉外壳与头部样式。
- **媒体查询按视口判断，容器模拟会错**：用同源 iframe（宽度精确可控）而不是容器，否则窗口一窄就会误触发站点的移动端规则。
- **`.prose` 自己声明了 `font-size`/`line-height`**：本机预览覆盖必须显式写到 `.prose` 自身，只改外层继承值会被它覆盖。
- **CSS 自定义属性不跨 iframe 边界**：预览样式的覆盖变量若定义在父文档（例如 `.canvas` 的 inline style），iframe 内部读不到——它是**独立文档**，继承链不会跨过去。要么把变量值作为字面量写进 iframe 文档，要么在 iframe 的 `<style>`/根元素上定义。判断这类没生效的覆盖，直接打印 `iframe.contentDocument` 里覆盖块的 `textContent` 与「全文档有无 inline style」，比读 Vue 源码快得多。
- **iframe 预览的「视口宽度」不等于阅读列宽**：桌面站点用 `.shell`（1000px）作视口、`.article` 自己在里面收 760px；若把 iframe 视口设成 760px，会命中站点 `@media (max-width: 780px)` 的平板规则（`.shell`/`.article` 内边距都变），与真实桌面页不符。验证方法是读 iframe 内部的 `documentElement.clientWidth` 与 `.article` 的 `padding-top`（桌面 78px / 平板 59px）。视口必须保持 1000px，容器更窄时用 `transform: scale()` **等比缩小整张页面**（媒体查询语义不变），不要改视口宽度。
- **`ResizeObserver` 的回调由 `requestAnimationFrame` 驱动**：在本机内置浏览器夹具里 `rAF` 不触发（实测注册后 600ms 零回调），只靠它量容器宽度会让比例停在挂载瞬间的过渡值（实测 `transform: matrix(0.02,…)`，iframe 缩成 20px）。测量要**多路兜底**：`ResizeObserver` + `window.resize` + 挂载后补测 + 每次重绘前补测；并把被缩放的舞台**绝对定位**，避免「缩放→容器变窄→更小缩放」的反馈循环。
- **「声明未使用」的排查口径再收紧一条**：给 iframe 写的 `var(--x, fallback)` 是有 fallback 的，所以少了定义**不会报错也不会报 eslint**，只会静默用 fallback 值。凡是 `var()` 写法，都要确认变量在**同一个文档**里真有定义。
- **深色模式（或任何主题开关）要检查「触发点」而不是「变量声明」**：`:root.dark { … }` 写在 CSS 里只说明变量存在，必须同时有代码给根元素加 `dark` 类。排查手法是 grep `classList.add` / `prefers-color-scheme` / `dark`，看有没有消费者；只有声明没有触发点等于死代码。
- **给偏好加新字段必须配 `#[serde(default)]`**：`load_config` 把反序列化失败判为「配置损坏」并**重置整份配置**（还把原文件改名 `.bak`）。少一个 default 就会让旧版 `config.json` 触发这条路径，用户仓库地址与其他偏好一并丢失。加字段时同时写一个「抹掉该字段仍能加载其余偏好」的测试。

### 前端布局

- **面板数量会变的容器不要用固定 `grid-template-columns`**：三段式改造后 `.workspace` 有三个子元素（列表、折叠按钮、编辑列，抽屉开启时四个），多出来的元素被排进隐式行，把状态栏顶到页面中间、整页可滚动。改用 flex，并让显示与否由同一个状态决定。
- **只看 DOM 快照抓不到布局问题**：状态栏「在 DOM 里存在且在视口内」与「被钉在页面底部」是两件事，必须读几何（`getBoundingClientRect` + `body.scrollHeight`）或看图。
- **`reka-ui` 引入但零消费者**（2026-09-28 复核仍然如此）：二阶段按施工单锁定了版本，但既有弹窗继续用已验证的 `ModalDialog.vue`，新写的菜单栏/斜杠/右键菜单都是轻量自写组件。施工单同段明确允许不替换，所以这不是缺陷，但**引入一个依赖就要在交付说明里如实标注「尚无消费者」**，否则下一轮会以为面板分隔条等功能已经用它实现了。面板分隔条拖动目前仍未实现。

### 测试与验证

- **Vite 会把测试里的 `new URL('../..', import.meta.url)` 当资源引用改写**：跨出工程根就抛 `The URL must be of scheme file`。读工程外文件要用 `fileURLToPath(import.meta.url)` + `path.resolve`。
- **不要用 PowerShell 的 `Set-Content` 改仓库源码文件**：本机默认按 ANSI（GBK）写回，中文注释与界面文案会**静默变成乱码**（文件本身仍是合法字节序列，只有 Vite 构建时才报 `stream did not contain valid UTF-8`；`vue-tsc` 因为读的是内存 AST 反而不报错，所以「类型检查通过」不能证明文件编码正确）。要改源码用编辑工具；已经损坏的文件可用 `[Text.Encoding]::GetEncoding(936).GetString(bytes)` 再以 UTF-8 写回修复。批量改动后可用「逐字节 UTF-8 序列校验」快速扫哪些文件坏了。
- **CodeMirror 在 jsdom 里会因缺 `Element.getClientRects` 报错**：它做 DOM 测量时会调用该 API。在测试环境补一个 polyfill，而不是放弃真实组件测试。
- **并发跑两轮 cargo 会让链接阶段报 `LNK1104 无法打开文件 ...exe`**：那是测试二进制被另一个 cargo 进程占用，不是代码问题。看到链接器「无法打开自己刚构建的 exe」先查并发进程，别去改代码。
- **变异验证脚本匹配补丁前先归一化换行**：Windows 上是 CRLF，直接 `includes` 会匹配不到，脚本会把「补丁没生效」误报成「测试没变红」。
- **用备份文件恢复被变异过的源码后必须显式 touch**：`Move-Item` 会保留备份的**旧 mtime**，而变异写入的 mtime 更新；恢复后源文件 mtime 比编译产物还旧，cargo 认为没变化，于是**继续跑变异版本的二进制**，让你看到一堆莫名其妙的失败。恢复源码后要么改 `LastWriteTime = Get-Date`，要么 `cargo clean -p <crate>`。排查手法：单独跑该用例却通过、全量跑却失败时，先怀疑跑的是陈旧产物。
- **概率性的并发守护测试不算守护测试**：靠「后台线程改盘 + 若干轮核对」去撞微秒级的两次读取窗口，把实现改回错误版本后它**照样通过**（实测）。正确做法是把不变量做成**纯函数**（一份输入 → 一条输出）后直接断言同源，确定性、瞬间失败；并发用例只当冒烟，并加前置断言确认两个状态都被观察到过。开大轮数（80 轮 114 秒）既不快也不能保证命中。
- PowerShell 5.1 按 ANSI 读 `.ps1`：脚本里**不要写非 ASCII 字面量**（中文路径会变乱码导致 `PathNotFound`），用 `(Get-Location)` 推导。

## 工具与环境

- **Bash 工具的沙箱会拦掉出站网络**（`git clone`、`curl` 对 github.com 直接连接超时，`Invoke-WebRequest` 报错）。同一命令加 `dangerouslyDisableSandbox: true` 后 `github.com`、`md.doocs.org`、`registry.npmmirror.com` 全部 200。**判断「没网」前先换掉沙箱重测一次**，否则会把可达的地址误判为不可达，进而向用户索要本可自己取到的资料。
- 复刻类任务先把参考源码取到仓库**同级目录**（如 `D:\PROJECT_ZZZZZZZZZ\doocs-md-ref`）并在文档里记下 commit，不要放进仓库；`doocs/md` 是 WTFPL v2，可自由借鉴，但作者姓名/邮箱/二维码等身份信息不得迁入。


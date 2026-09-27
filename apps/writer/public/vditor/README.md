# Vditor 运行时资源（随前端分发）

本目录是从 `node_modules/vditor/dist` 复制出来的运行时资源，用于让即时排版在
**不访问网络**的情况下工作，并避免向拥有本地命令权限的 webview 注入第三方脚本。

## 为什么需要复制

Vditor 默认从 `https://unpkg.com/vditor@<version>` 注入 `<script>` 加载 Lute 排版
引擎、i18n 与图标。这会带来两个问题：

1. 离线时编辑器无法工作（方案 §6.1 要求即时排版「低延迟、离线可用」）；
2. 把第三方远程脚本带进了具备业务命令权限的 webview，与 `tauri.conf.json` 的
   `script-src 'self'` CSP 冲突（方案 §7.4）。

`src/components/MarkdownEditor.ts` 因此显式传入 `cdn` 与 `_lutePath`，指向本目录。

## 包含内容（约 4.7 MB，32 个文件）

| 路径 | 用途 |
|---|---|
| `dist/js/lute/lute.min.js` | Markdown↔HTML 排版引擎（必需） |
| `dist/js/i18n/{zh_CN,en_US}.js` | 界面文案（只保留中文与英文） |
| `dist/js/icons/material.js` | 工具栏图标（适配层设置了 `icon: 'material'`） |
| `dist/js/highlight.js/highlight.min.js` | 代码高亮 |
| `dist/js/highlight.js/styles/{github,monokai,native}.min.css` | 三个代码主题（与界面下拉项一一对应） |
| `dist/css/content-theme/*.css` | 预览内容主题 |
| `dist/images/emoji/*` | 表情短代码 |

## 更新方式

升级 `vditor` 依赖后，把上述子集重新从 `node_modules/vditor/dist` 复制过来，并
在真实 Tauri 窗口中确认编辑器渲染、模式切换与外观设置仍然生效。

**注意**：本目录路径中含 `dist`，因此根目录与 `apps/writer` 的 `.gitignore` 使用
`/dist/` 并加 `!public/vditor/` 例外。改动忽略规则后请用
`git check-ignore -v <某个文件>` 确认资源确实可被提交。

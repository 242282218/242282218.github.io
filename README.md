# 242282218 的技术手记

一个中文个人主页与技术博客。站点使用 Astro 静态构建，文章保存为 Markdown，通过 GitHub Actions 发布到 GitHub Pages。

## 本地运行

需要 Node.js 22.12+ 和 pnpm 10。运行 `pnpm install --frozen-lockfile`，再运行 `pnpm dev`；发布前运行 `pnpm test && pnpm check && pnpm build`。

## 新增文章

在 `src/content/blog/` 添加一个例如 `hello-world.md` 的文件：

```md
---
title: 第一次记录
description: 这篇文章会记录什么，写成一句话。
pubDate: 2026-09-26
tags: [学习笔记]
draft: true
---

从这里开始写正文。确认内容可以公开后，将 `draft` 改为 `false`。
```

文件名决定文章地址，例如 `hello-world.md` 对应 `/blog/hello-world/`。正文可使用普通 Markdown 图片语法；将图片放在 `public/images/`，通过 `/images/文件名.webp` 引用。不要提交密钥或私人笔记。文章按发布日期倒序排列，草稿不会生成公开页面或进入 RSS。

## 替换个人资料

页面文案分别位于 `src/pages/index.astro`、`src/pages/about.astro`、`src/pages/projects.astro`，站点标题及导航位于 `src/layouts/BaseLayout.astro`。在准备好真实项目资料前，页面会显示空状态，不展示虚构项目。站点不依赖旧域名。

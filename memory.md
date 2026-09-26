# 项目经验

- 发布前运行 `pnpm test && pnpm check && pnpm build`。文章目录只有 `.gitkeep` 时，Astro 会提示集合为空，但类型检查仍为 0 errors / warnings / hints，且会构建首页、文章列表、项目、关于和 RSS。
- Markdown 放入 `src/content/blog/`；`draft: true` 不生成文章静态路由，也不进入首页、列表和 RSS。可用临时公开文章与草稿执行构建验证，发布前删除它们并再次构建。
- GitHub Pages 目标为用户站点仓库 `242282218/242282218.github.io`，Astro `site` 对应 `https://242282218.github.io`；不要沿用旧域名或旧仓库历史。

# 项目经验

- 发布前运行 `pnpm test && pnpm check && pnpm build`。文章目录只有 `.gitkeep` 时，Astro 会提示集合为空，但类型检查仍为 0 errors / warnings / hints，且会构建首页、文章列表、项目、关于和 RSS。
- Markdown 放入 `src/content/blog/`；`draft: true` 不生成文章静态路由，也不进入首页、列表和 RSS。可用临时公开文章与草稿执行构建验证，发布前删除它们并再次构建。
- GitHub 账号已更名为 `guanlangzg`；用户站点目标为 `guanlangzg/guanlangzg.github.io`，Astro `site` 对应 `https://guanlangzg.github.io`。此前的 `242282218.github.io` 是旧地址，不要将其当作可自动跳转的新站点。

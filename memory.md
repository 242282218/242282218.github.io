# 项目经验

- 本站博客名是「观澜志」，作者网名是「观澜」，GitHub 用户名是 `guanlangzg`。页面标题、品牌与署名使用中文名称，账号名仅用于仓库、网址和 GitHub 链接；不要推断真实姓名。
- 对外定位为个人学习记录，文案克制、不张扬，不称为「技术博客」，避免履历式、作品集式的自我包装。视觉参考 https://brittanychiang.com/#projects 的清晰层级、轻量条目和留白，但沿用本站近白底、深蓝黑文字与蓝色强调，不照搬深色背景或项目内容。

- 发布前运行 `pnpm test && pnpm check && pnpm build`。文章目录只有 `.gitkeep` 时，Astro 会提示集合为空，但类型检查仍为 0 errors / warnings / hints，且会构建首页、文章列表、项目、关于和 RSS。
- Markdown 放入 `src/content/blog/`；`draft: true` 不生成文章静态路由，也不进入首页、列表和 RSS。可用临时公开文章与草稿执行构建验证，发布前删除它们并再次构建。
- GitHub 账号已更名为 `guanlangzg`；用户站点目标为 `guanlangzg/guanlangzg.github.io`，Astro `site` 对应 `https://guanlangzg.github.io`。此前的 `242282218.github.io` 是旧地址，不要将其当作可自动跳转的新站点。
- 本地预览底部的黑色 Menu / Inspect / Audit / Settings 浮层是 Astro Dev Toolbar，不属于站点页面；在 `astro.config.mjs` 设置 `devToolbar: { enabled: false }` 可关闭它，正式构建原本不会包含该工具栏。
- 「观澜」标签图以用户提供的原图为准。裁成 `public/guanlan-logo.png` 的 1000×1000 正方形后，缩放生成 `public/favicon-{16,32,48,64}.png`；不要用旧绘制脚本或重新生成图替换用户原图。小尺寸须保留“观”、水势及朱印的整体关系。

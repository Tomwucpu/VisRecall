---
name: uniform-item-cards
overview: 把「全部素材」的素材卡片改成固定宽度（236px）的等高卡片网格，缩略图用 contain 完整显示并纯色留白，使卡片尺寸不随窗口/详情面板变化而任意拉伸。
design:
  architecture:
    framework: react
  styleKeywords:
    - 固定尺寸网格
    - 等高卡片
    - 完整缩略图
    - 水墨极简
    - 克制动效
  fontSystem:
    fontFamily: Inter, Segoe UI, PingFang SC
    heading:
      size: 13px
      weight: 500
    subheading:
      size: 10px
      weight: 400
    body:
      size: 13px
      weight: 400
  colorSystem:
    primary:
      - "#151514"
      - "#5C5C56"
      - "#93938B"
    background:
      - "#FBFBF8"
      - "#FFFFFF"
      - "#EFEEE8"
      - "#242424"
      - "#161616"
    text:
      - "#151514"
      - "#5C5C56"
      - "#93938B"
      - "#F2F2EE"
    functional:
      - "#A84236"
      - "#A9702F"
      - "#4D6B5B"
      - "#46506B"
todos:
  - id: grid-fixed-tracks
    content: 将 .grid 改为 236px 固定轨道 Grid 并删除列数媒体查询
    status: completed
  - id: card-uniform-height
    content: 卡片改 flex 等高布局，缩略图改 contain 并去掉 hover 放大
    status: completed
    dependencies:
      - grid-fixed-tracks
  - id: sync-comments
    content: 同步 ItemGrid 与样式注释，清理 break-inside 等失效声明
    status: completed
    dependencies:
      - grid-fixed-tracks
  - id: verify-layout
    content: 跑 typecheck、lint、test 并核对多尺寸与详情开合下的卡片表现
    status: completed
    dependencies:
      - card-uniform-height
      - sync-comments
---

## 产品概述
针对「全部素材」页的素材卡片做一次布局规整：卡片尺寸固定、不再随窗口宽度或详情面板开合而随意拉伸，缩略图在固定卡片内完整可见。

## 核心功能
- 卡片统一为固定宽度 236px、等高，间距 16px，形成行列对齐的网格（替换当前随宽度变化的多栏瀑布流）。
- 窗口变宽只增加列数，卡片本身尺寸不变；详情面板打开或关闭只影响列数，不影响卡片大小。
- 图片在固定缩略区内完整显示（等比缩小到完全可见，不裁切），非 16:10 比例产生的空白用现有纯色底填充，图片居中并留少量内边距。
- 取消 hover 时图片放大裁边的效果，保留卡片抬起、描边与阴影反馈；选中态视觉不变。
- 文本/链接类素材（无缩略图）使用类型占位，卡片尺寸与图片卡完全一致。
- 容器宽度小于 236px 的极端情况（窗口最小宽 + 详情面板打开）下该列等比收缩到可用宽度，不出现横向裁切或溢出。


## 技术栈
- 沿用现有前端：React 19 + TypeScript + Vite，样式为单一全局 CSS（`src/styles/globals.css`）+ 设计令牌（`src/styles/tokens.css`）。
- 不引入任何新依赖，不改后端与数据契约，不改卡片 DOM 结构。

## 实现方案
### 现状与问题
- `.grid` 使用 `column-count: 2`（1700px 以上 3 列、1180px 以下 1 列）的 CSS 多栏瀑布流，卡片宽度等于列宽，因此主区变宽/变窄（尤其是详情面板 `is-hidden` 滑出前后）卡片会被整体拉伸。
- `.card-thumb > img` 为 `object-fit: cover`，配合 `.card:hover .card-thumb > img { scale(1.025) }`，图片被裁切，无法完整展示。
- 缩略图由后端按长边 480 等比生成、未裁切（`crates/app-core/tests/thumbnail.rs::thumbnail_long_edge_is_480`），所以前端改成 `contain` 即可真正看到整张图。

### 关键决策
1. **布局改用固定轨道 CSS Grid**：`grid-template-columns: repeat(auto-fill, minmax(min(236px, 100%), 236px))`。常态下每条轨道恒为 236px，列数由可用宽度自动决定；当容器窄于 236px 时轨道收缩为 100%，避免 `.grid-scroll` 的 `overflow-x: hidden` 把卡片裁掉。`min()` 兜底比新增多个断点媒体查询更简单且无跳跃。
2. **保留左对齐、分布剩余空间**：`justify-content: space-between`，首列贴住容器左内边距、末列贴住右内边距，列宽恒定，剩余宽度均摊到列间距，避免右侧出现大片空白。
3. **等高靠 Grid 默认拉伸 + 卡片内部 flex**：`.card` 改为 `display: flex; flex-direction: column; height: 100%`，缩略区固定 `aspect-ratio: 16 / 10`（236 × 147.5），文字区 `flex: 1`。同一行卡片天然等高，不写死像素高度，字号或字体回退变化时不会被裁切。
4. **图片完整展示**：`object-fit: contain` + 图片自留 6px 内边距（对应「可以适当放小」），空白区域由 `.card-thumb` 的 `var(--thumb)` 纯色底填充；删除 hover 放大规则，避免 `contain` 下放大后四边被裁。
5. **清理旧规则**：删除 `.grid` 的两条 `column-count` 媒体查询、`.card` 的 `margin: 0 0 16px`（改由 grid 的 `row-gap` 控制）、`.grid-cell` 的 `break-inside` 与 `.grid-more` 的 `break-inside`（不再处于多栏上下文）。

### 性能与稳定性
- Grid 布局由浏览器一次完成轨道计算，行为等价于原多栏布局，无额外 JS、无重排风险；列数变化只发生在容器尺寸变化时。
- 图片仍保留 `loading="lazy"` / `decoding="async"`，改为 `contain` 不增加解码成本；每个缩略图解码后最多缩放到 236×147，GPU 合成开销小于原来全宽卡片。
- 卡片宽度固定后，`will-change: transform` + hover 位移的合成层面积变小，滚动更稳。

### 边界情况
- 窗口最小宽 860、侧栏 238、详情列 370 同时存在时主区仅约 200px：`min(236px, 100%)` 保证只有一列且等比收缩，不横向溢出。
- 无缩略图/加载失败：`.thumb-fallback` 仍 `inset: 0` 铺满缩略区，尺寸与图片卡一致。
- `parsing/pending` 的 `.shimmer` 与 `.badge` 为缩略区绝对定位子元素，`contain` 不影响其层级与位置。
- 「加载更多」按钮仍是 `ul.grid` 的兄弟节点，保持整行居中，无需跨列处理。

## 实施要点
- 改动集中在 `src/styles/globals.css` 的 `.grid` / `.grid-cell` / `.card` / `.card-thumb` / `.card-thumb > img` / `.grid-more` 一段，按现有令牌与 `--ease` 书写，不新增色值。
- 同步修正 `src/features/library/ItemGrid.tsx` 顶部注释（「CSS 多栏瀑布流」已不成立），避免文档与实现漂移；`ItemCard.tsx` 无需改结构。
- 现有测试不依赖网格尺寸（`ItemCard.test.tsx` 只断言文案与占位、`ItemGrid.test.tsx` 断言列表与错误态、`App.test.tsx` 断言卡片可点击），改动应保持全绿。
- 校验命令：`pnpm typecheck`、`pnpm lint`、`pnpm test`；本机终端 `NODE_ENV=production` 会导致 React 生产构建、测试报 `React.act is not a function`，跑测试前需 `$env:NODE_ENV="test"`。

## 架构设计
纯表现层调整，数据流与组件层级不变：`App`（选中态/详情开合）→ `ItemGrid`（列表容器与加载更多）→ `ItemCard`（单卡展示）；变化只发生在 `ItemGrid` 输出的 `.grid` 布局与 `.card` 内部尺寸约束上。

## 目录结构
```
src/
├── styles/
│   └── globals.css                  # [MODIFY] 网格与卡片样式：.grid 改固定轨道 Grid（236px，auto-fill + min() 兜底，space-between，16px 间距），删除 column-count 媒体查询；.grid-cell 去掉 break-inside，改 min-width: 0；.card 去 margin、改 flex 列 + height: 100%；.card-thumb > img 改 object-fit: contain + 6px 内边距，删除 hover 放大；.grid-more 去掉 break-inside
└── features/
    └── library/
        └── ItemGrid.tsx             # [MODIFY] 仅更新文件/组件注释（固定网格而非多栏瀑布流），渲染结构保持不变
```


## 设计风格
延续现有水墨极简风，只把「全部素材」的卡片从随宽度拉伸的瀑布流改为尺寸恒定的规整网格，让一屏排列更整齐、缩略图完整可辨。

## 布局与卡片
- 卡片固定 236px 宽、等高（缩略区 16:10，约 236×147；文字区两行固定结构），间距 16px，左对齐并均摊剩余宽度。
- 缩略区为纯色底（浅色 #EFEEE8 / 深色 #242424），图片居中完整显示并留 6px 内边距，形成统一的「画框」观感。
- 文字区：单行标题（超出省略）+ 单行元信息（色点、类型、来源、时间），层级与字号沿用现有令牌。
- 卡片圆角 10px、1px 描边；hover 抬起 2px + 阴影 + 描边加深，不再放大图片；选中态内描边不变。

## 响应式
列数随容器宽度自动增减：约 3–4 列（默认窗口），详情面板开合只改变列数；极端窄宽度下该列等比收缩，不裁切、不横向滚动。

## 动效
保留 `cardIn` 入场（0.36s，`--ease`）与 hover 的 0.24s 过渡；`prefers-reduced-motion` 下自动降级为瞬时。

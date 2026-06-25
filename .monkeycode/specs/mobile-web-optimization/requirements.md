# 移动端网页优化 — 需求文档

## 引言

为 GitLab RS（基于 Rust Workhorse）实现移动端网页体验优化。核心技术约束：**移动端与 PC 端 URL 完全一致**，确保 `git clone`、Issue 分享链接等场景下不产生歧义。

## 方案调研结论

| 方案 | 同 URL | 移动端轻量 | 维护成本 | 结论 |
|------|--------|------------|----------|------|
| 纯响应式（CSS media query） | 是 | 否（仍需下载全部资源） | 低 | 不满足带宽优化目标 |
| 独立移动端子域名 | 否 | 是 | 高 | 违反"同 URL"约束 |
| 前端 JS 检测 + 动态加载 | 是 | 中等 | 中 | 首屏仍需加载检测代码，改进有限 |
| **服务端自适应交付（推荐）** | 是 | 是 | 中 | 由 Workhorse 检测设备并在不改变 URL 的前提下优化内容交付 |

**推荐方案**：服务端自适应交付（Server-side Adaptive Delivery）。Rust Workhorse 在反向代理层根据请求特征（User-Agent、Viewport-Width 等）动态优化响应内容，前端配合少量 CSS/JS 增强。

## 术语表

- **Workhorse**：Rust 重写的 GitLab Workhorse 反向代理，替代 Nginx + Go Workhorse
- **自适应交付**：服务器根据客户端能力（设备类型、网络、屏幕）动态调整响应内容的技术
- **Pre-compressed Assets**：预先生成的 .gz / .br 压缩静态资源
- **Viewport-Width**：客户端通过 HTTP Header 或 JS 上报的视口宽度

## 需求

### R1: 设备检测与响应分类

**User Story:** AS 系统管理员，I want Workhorse 自动识别访问设备类型，so that 移动端和桌面端都能获得适合的响应内容，且 URL 完全一致。

**Acceptance Criteria:**

1. WHEN Workhorse 收到 HTTP 请求，THE Workhorse SHALL 解析 User-Agent 请求头，将设备初步分类为 `mobile`、`tablet`、`desktop`
2. WHEN 设备被初步分类后，THE Workhorse SHALL 在 HTML 响应中注入视口检测脚本，由前端 JS 上报实际 `Viewport-Width` 并进行二次确认
3. WHEN 前端 JS 上报视口宽度 ≤ 768px，THE Workhorse SHALL 将设备分类更新为 `mobile`，后续请求使用 Cookie `gitlab_device=mobile` 标识
4. WHEN 设备被分类为 `mobile` 或 `tablet`，THE Workhorse SHALL 在转发给 Rails 的请求中添加 `X-Gitlab-Device: mobile` 请求头
5. IF User-Agent 缺失或无法识别，THE Workhorse SHALL 默认分类为 `desktop`，依赖前端 JS 上报修正

### R2: 静态资源按需交付

**User Story:** AS 移动端用户，I want 浏览器不下载不需要的桌面端大资源，so that 页面加载更快、流量消耗更少。

**Acceptance Criteria:**

1. WHEN Workhorse 返回静态资源（/assets/*），且设备为 `mobile`，THE Workhorse SHALL 优先返回 `.br`（Brotli）或 `.gz`（Gzip）预压缩版本
2. WHEN Workhorse 返回 HTML 页面，且设备为 `mobile`，THE Workhorse SHALL 在 HTML 中注入移动端优化标记（viewport meta、资源预加载提示）
3. WHEN Workhorse 处理 CSS 请求，且设备为 `mobile`，THE Workhorse SHALL 在响应中附加移动端补充样式（注入移动优化的 CSS 规则）
4. IF 预压缩资源不存在，THE Workhorse SHALL 回退到原始资源并在响应中应用 on-the-fly 压缩

### R3: 图片自适应优化

**User Story:** AS 移动端用户，I want 图片按屏幕分辨率自动缩放，so that 不会因加载高清大图浪费流量和加载时间。

**Acceptance Criteria:**

1. WHEN Workhorse 检测到请求中包含 `Viewport-Width` 请求头（由前端 JS 设置），THE Workhorse SHALL 将图片 URL 中的宽度参数限制为不超过视口宽度
2. WHEN Workhorse 接收图片请求（`/uploads/` 或 Rails 返回的图片代理），且客户端 Accept 头包含 `image/webp`，THE Workhorse SHALL 将图片实时转换为 WebP 格式并返回
3. WHEN Workhorse 实时转换 WebP，THE Workhorse SHALL 缓存转换结果到内存缓存（LRU，最大 500MB），避免重复转换
4. WHEN 设备为 `mobile` 且图片原始尺寸超过视口 2 倍，THE Workhorse SHALL 将图片缩放至合适分辨率后返回
5. IF 内存缓存未命中且转换耗时超过 500ms，THE Workhorse SHALL 返回原始图片并在后台异步生成 WebP 缓存

### R4: 页面 HTML 移动端优化

**User Story:** AS 移动端用户，I want 页面在手机上易于阅读和操作，so that 无需缩放即可正常使用 GitLab 功能。

**Acceptance Criteria:**

1. WHEN Workhorse 处理来自 Rails 的 HTML 响应，且设备为 `mobile`，THE Workhorse SHALL 在 `</head>` 前注入 `<meta name="viewport" content="width=device-width, initial-scale=1">`
2. WHEN Workhorse 处理 HTML 响应，且设备为 `mobile`，THE Workhorse SHALL 注入内联移动端适配 CSS（触摸友好的按钮尺寸、隐藏桌面端侧栏）
3. WHEN Workhorse 处理 HTML 响应，且设备为 `mobile`，THE Workhorse SHALL 对非首屏资源添加 `loading="lazy"` 属性
4. WHEN Workhorse 处理 HTML 响应，且设备为 `mobile`，THE Workhorse SHALL 对 `<img>` 标签添加 `decoding="async"` 属性

### R5: 网络层优化

**User Story:** AS 所有用户，I want 页面资源被高效缓存和传输，so that 重复访问时几乎瞬间加载。

**Acceptance Criteria:**

1. THE Workhorse SHALL 对所有 `/assets/` 路径的响应添加 `Cache-Control: public, max-age=31536000, immutable` 响应头
2. THE Workhorse SHALL 对所有静态资源响应启用 Brotli 压缩（优先级高于 Gzip）
3. THE Workhorse SHALL 支持 HTTP/2 Server Push，对 HTML 页面首屏关键资源（CSS、JS）主动推送
4. WHEN 客户端支持 HTTP/2，THE Workhorse SHALL 启用 HPACK 头部压缩

### R6: 前端性能指标收集

**User Story:** AS 开发者，I want 收集真实用户性能数据，so that 持续优化加载体验。

**Acceptance Criteria:**

1. THE Workhorse SHALL 在 HTML 响应中注入 Web Vitals 性能监控脚本（LCP、FID、CLS）
2. WHEN 性能指标超出阈值（LCP > 2.5s, FID > 100ms, CLS > 0.1），THE Workhorse SHALL 记录 WARN 级别日志
3. THE Workhorse SHALL 通过 `/-/metrics` 端点暴露页面加载性能相关指标

### R7: 移动端专用 CSS（全面适配）

**User Story:** AS 移动端用户，I want GitLab 所有页面在手机上布局合理，so that 核心工作流（查看 Issue、浏览代码、提交 MR）无需横向滚动且操作便捷。

**Acceptance Criteria:**

1. THE 系统 SHALL 提供移动端全面适配 CSS 文件（`mobile-full.css`），由 Workhorse 在移动端请求的 HTML `</head>` 前注入
2. THE 移动端 CSS SHALL 确保所有页面元素在 375px 宽度（iPhone SE）下不产生横向滚动条
3. THE 移动端 CSS SHALL 覆盖以下页面的布局适配：登录/注册页、项目首页、Issue 列表/详情、MR 列表/详情、代码浏览、个人设置、Admin 面板
4. THE 移动端 CSS SHALL 将全局导航栏改为底部固定 TabBar（项目、Issue、MR、CI/CD、设置）
5. THE 移动端 CSS SHALL 将触控目标最小尺寸设置为 44x44 CSS 像素
6. THE 移动端 CSS SHALL 将数据表格（Issue 列表、MR 列表）改为卡片布局
7. THE 移动端 CSS SHALL 隐藏桌面端侧栏（sidebar），改为顶部汉堡菜单触发抽屉式导航
8. THE 移动端 CSS SHALL 将代码 Diff 视图改为单列纵向布局

# 遗留问题清单

## 头像缓存问题

**问题描述**：用户通过网页表单上传新头像后，页面刷新仍显示旧头像

**当前状态**：已添加缓存失效逻辑（POST /-/user_settings/profile 成功时清除缓存），但用户反馈仍未立即生效

**临时解决方案**：在头像上传提示文本后添加了注释"头像更新后需刷新页面或重新登录才能看到变化"

**根本原因分析**：
1. GitLab Rails 的头像 URL 包含版本号（如 `?v=1234`），浏览器会缓存带版本号的 URL
2. 即使服务端缓存失效，浏览器缓存仍可能导致显示旧头像
3. Rails 的 `user.avatar.url` 返回的 URL 包含时间戳版本号

**待优化方案**：
- [ ] 研究 Rails 头像 URL 版本号机制，找到强制刷新的方法
- [ ] 考虑在头像 URL 后添加随机参数（如 `?t=<timestamp>`）绕过浏览器缓存
- [ ] 或在 HTML 注入中修改头像 img 标签的 src 属性

---

## 其他遗留问题

### 0. 多语言（zh-CN）补全 — 下一版本

**状态**：已完成的 CI/校验文案修复随 19.3.1（2026-10-17）发布；以下残留项暂缓，
留到下一版本处理。扫描工具已入库，可直接复跑。

| 文件路径 | key / 位置 | 待办说明 |
| --- | --- | --- |
| `docker/gitlab-rails-overrides/config/initializers/gitlab_rs_ci_i18n.rb`（及 `-server` 副本） | `GitlabRs::CiI18n::ValidationMessages#message` | 去掉 `:invalid` 特判：改为在 po 中补 `activerecord.errors.messages.invalid`，并确认所有模型走同一路径 |
| 同上 | `GitlabRs::CiI18n::{CommitStatusMessages,PipelineMessages,FailedStatusMessages,CompletionReasonMessages,PlayManualMessages}` | 用 `scripts/i18n/scan_hardcoded.py` 对新版本 CI 代码复扫，新增/变更的状态文案按 msgid 补翻 |
| `locale/zh_CN/validation_messages.po.fragment` | `activerecord.attributes.key.fingerprint_sha256` | 现在显示 `Fingerprint sha256`，导致“Fingerprint sha256无法生成”混排；补属性名译文 |
| 同上 | `activerecord.attributes.application_setting.domain_denylist` | 现在显示 `Domain denylist`，导致“Domain denylist启用...”混排；补属性名译文 |
| 同上 | `activerecord.errors.messages.invalid` | Rails 的 activerecord 作用域返回英文 `is invalid`，当前靠 initializer 特判绕过；补 po 条目后移除特判 |
| `locale/zh_CN/ci_messages.po.fragment` | `CI ID token issuance disabled for this project path` 等 116 条 | 仅在 en/ja 回归时复核；新增状态类文案继续追加到此 fragment |
| `app/services/**`、`app/controllers/**`（上游） | `ServiceResponse.error(message:)`、`render json: { message: ... }` | 这些文案不经过 `ActiveModel::Error`，需要逐处确认是否用户可见，可见的用 `_()`/`s_()` 包裹 |
| `scripts/i18n/scan_missing.py` | 全量 `app/` 扫描 | 下一版本先跑一遍并对候选分级：用户可见 / 仅日志 / 仅内部异常；只翻用户可见项 |
| `public/assets/locale/zh_CN/app-*.js.gz` | 前端翻译包 | 服务端修复不影响该独立包；如需补前端文案，按上游前端 i18n 流程更新 |

### 1. SafeLine WAF 拦截

**问题描述**：SafeLine WAF 拦截外部 HTTPS 访问（9071 端口）

**影响**：无法通过 `https://bak.toarujs.com:9071` 正常访问 GitLab

**待办**：
- [ ] 配置 SafeLine WAF 白名单允许 git HTTP 请求通过 9071

### 2. Docker Hub 推送被墙

**问题描述**：Docker Hub (registry-1.docker.io) 不可达

**影响**：无法推送 Docker 镜像到 Docker Hub

**待办**：
- [ ] 解决 Docker Hub 网络问题后推送镜像
- [ ] 或考虑使用其他镜像仓库（如阿里云、腾讯云）

### 3. lang_switcher 注入导致前端报错

**问题描述**：`.reduce is not a function` 前端报错

**当前状态**：临时禁用 lang_switcher 注入

**待办**：
- [ ] 调查 `.reduce is not a function` 是否与 lang_switcher 注入相关
- [ ] 修复后重新启用 lang_switcher 注入

### 4. 新用户注册需要手动开启

**问题描述**：`ApplicationSetting.last.update_columns(signup_enabled: true)` 绕过加密密钥错误

**影响**：每次重启后可能需要手动开启注册

**待办**：
- [ ] 研究如何永久解决加密密钥问题
- [ ] 或在 entrypoint.sh 中自动开启注册

---

## 已修复问题

### ✅ Git clone 根因修复
- Gitaly sidechannel 非对称 pktline 帧协议

### ✅ 23 个 CVE 安全审查与修复

### ✅ Docker 镜像构建与部署
- `toarujs/gitlab-rs:latest`

### ✅ 代码推送
- GitHub `main` + 自建 GitLab `master`

### ✅ README 更新
- 中文版含功能、CLI 参数、Docker 部署、基准测试

### ✅ 基准测试
- Rust 启动快 13.6x，内存省 63%，HTTP 延迟快 27x

### ✅ HTML 注入修复
- 移除硬编码 `bak.toarujs.com:9071` URL

### ✅ X-Sendfile 头部 bug 修复
- 文件内容返回 0 字节问题

### ✅ 头像缓存机制
- 实现 `CacheState::remove_by_prefix()`，头像上传成功后清除对应缓存条目

### ✅ 头像加速上传
- 缓冲请求体到临时文件，用 JSON 元数据转发给 Rails

### ✅ MaximumSize==0 与 tmp/work 属主
- Rails `MaximumSize: 0` 视为不限大小
- 上传加速同时 chown `TempPath` 父目录和同级 `tmp/work`
- 9071 实测 generic/LFS/pypi 上传成功，`tmp/work` 属主为 `git:git`

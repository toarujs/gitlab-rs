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

**状态**：CI/校验文案已随 19.3.1（2026-10-17）发布；2026-10-19 继续补齐
`ServiceResponse` / JSON `message` 运行时翻译与用户可见校验文案。扫描工具
已入库，可直接复跑。生产镜像未变更。

**本轮已完成（2026-10-17）**：

- 补齐 `activerecord.errors.messages.invalid`，移除 `ValidationMessages#message`
  的 `:invalid` 特判；`Key`、`Ci::Variable`、`Project`、`Group`、`User`、
  `PersonalAccessToken` 的 `:invalid` 均走通用翻译路径。
- 补齐 `activerecord.attributes.key.fingerprint_sha256`（`指纹（SHA256）`）与
  `activerecord.attributes.application_setting.domain_denylist`（`域名拒绝列表`），
  修复 `Fingerprint sha256无法生成`、`Domain denylist启用...` 的中英混排。
- 复扫 CI 硬编码文案：`scan_hardcoded.py` 输出 93 行候选，其中 12 条不在 po，
  均为 CSS 类名、Ruby 类名或多行字面量片段；对应用户可见文案的完整 msgid 已全部存在。
- 验证：金丝雀 `Key#errors.full_messages` 为
  `["组织必须存在", ..., "键是无效的", "指纹（SHA256）无法生成"]`；en/ja 仍回退英文。
- 金丝雀验证镜像 `toarujs/gitlab_rs_test:i18n-todo-20261017`（`8f11fcf2488a`），
  通过 `/dockerbuild/gitlab-rs-cs/docker-compose.i18n-todo.yaml` 重建后
  `/-/readiness`、`/-/health` 均为 200；生产未变更。

**本轮已完成（2026-10-19）**：

- 全量 `scan_missing.py` 候选从 700 条降到 356 条。`triage_candidates.py`
  分级后：`service_response` 236 条全部入库并运行时翻译；`validation`
  用户可见项已翻；`api_response` 中完整 msgid 的 JSON `message` 已翻。
- initializer 新增 `ServiceResponseMessages`（翻译 `ServiceResponse#message` /
  `to_h[:message]` / `errors`）与 `RenderJsonMessages`（翻译
  `render json: { message:/error:/errors: }` 中的字符串，不改其它 JSON 字段）。
- 新建 `locale/zh_CN/service_messages.po.fragment`（303 条）；校验 fragment
  补属性名 `activerecord.attributes.member.access_level`=`访问级别`，并把
  自带属性名的校验文案改成纯消息，避免 `时区时区无效` 一类重复。
- 验证（金丝雀 `i18n-todo6-20261019`=`45f6e0c0ff3e`）：
  `ServiceResponse.error(message: "You don't have permissions to archive this project.")`
  → `您没有归档此项目的权限。`；数组消息同步翻译；未知 msgid 原样返回；
  en 回退英文；`Member` 校验 `访问级别不在列表中`；JSON helper
  `{message: "Invalid parameters"}` → `参数无效`。`/-/readiness`、`/-/health`
  均为 200。

| 文件路径 | key / 位置 | 待办说明 |
| --- | --- | --- |
| 同上 | `GitlabRs::CiI18n::{CommitStatusMessages,PipelineMessages,FailedStatusMessages,CompletionReasonMessages,PlayManualMessages}` | 用 `scripts/i18n/scan_hardcoded.py` 对新版本 CI 代码复扫，新增/变更的状态文案按 msgid 补翻 |
| `locale/zh_CN/ci_messages.po.fragment` | `CI ID token issuance disabled for this project path` 等 116 条 | 仅在 en/ja 回归时复核；新增状态类文案继续追加到此 fragment |
| `app/services/**`、`app/controllers/**`（上游） | `ServiceResponse.error(message:)`、`render json: { message: ... }` | 已用 initializer 运行时翻译 + `service_messages.po.fragment`；新增 msgid 追加到该 fragment 即可，无需再逐处 `_()` 包裹 |
| `scripts/i18n/scan_missing.py` | 全量 `app/` 扫描 | 2026-10-19 复扫 356 行。可跳过：log 166、worker/审计日志、多行字面量片段、CSS/类名、registry 协议码 `NAME_UNKNOWN`/`UNAVAILABLE`。剩余 other 中的 hash 风格 `error(message:)` 未走 `ServiceResponse`，如需补再追加 msgid |
| `public/assets/locale/zh_CN/app-*.js.gz` | 前端翻译包 | 服务端修复不影响该独立包；如需补前端文案，按上游前端 i18n 流程更新 |

### 1. SafeLine WAF 拦截

**问题描述**：SafeLine WAF 拦截外部 HTTPS 访问（9071 端口）

**影响**：无法通过 `https://bak.toarujs.com:9071` 正常访问 GitLab

**待办**：
- [ ] 配置 SafeLine WAF 白名单允许 git HTTP 请求通过 9071

### 2. Docker Hub 推送

**状态**：已完成（2026-10-17）

- `registry-1.docker.io` 与 `auth.docker.io` 可正常访问，登录凭据位于
  `/root/.docker/config.json`。
- `toarujs/gitlab-rs` 已推送 `19.3.1`、`19.3.1-rs.20261017`、`latest`
  三个标签，远端 manifest 为
  `sha256:93cffdb7aa5749edc74bc98d3cd5596774f7d602d4fa849240b36296b4163981`，
  config digest 与本地生产镜像 `sha256:d35e89d43311...` 一致。

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

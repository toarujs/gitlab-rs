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

 # Rails / Puma 热点加速 — 需求文档

 ## 引言

 gitlab-rs 已用 Rust Workhorse 替换 Go Workhorse 与 nginx。Web 请求在 Workhorse 之后仍进入 Puma 上的 GitLab Rails（CE 19.3.1）。生产部署 `puma["worker_processes"] = 8`。项目列表、仓库 Files API、CI job log 等读路径的延迟与内存主要花在这一层。

 本需求描述：在保留官方 Rails 行为的前提下，把可加速的热点读路径从 Puma 前移到 Workhorse，并能量化效果。

 ## 术语表

 - **Workhorse**：本仓库中的 Rust HTTP 前端，监听 `:80`，经 unix socket 转发到 Puma
 - **Puma**：运行 GitLab Rails 的 Ruby 应用服务器
 - **Rails**：GitLab CE 19.3.1 应用（权限、业务规则、写路径的权威实现）
 - **热点路径**：生产上占用 Puma CPU 或墙钟时间最多的 HTTP 接口
 - **加速**：Workhorse 完成鉴权校验与数据读取后直接响应，请求不再进入 Puma 业务 action
 - **回源**：加速未命中或失败时，Workhorse 把原请求转给 Puma

 ## 需求

 ### R1: 基线可观测

 **User Story:** AS 运维，I want 看到每个请求在 Workhorse 与 Puma 各花多少时间，so that 加速前后有可对比的数字。

 #### Acceptance Criteria

 1. WHEN Workhorse 代理请求到 Puma，THE Workhorse SHALL 记录方法、路径模板、HTTP 状态码、Workhorse 耗时、Puma 耗时
 2. WHEN 同一路径在 5 分钟内超过 50 次，THE Workhorse SHALL 在指标中按路径模板聚合 p50 与 p95
 3. WHEN 加速路径生效，THE Workhorse SHALL 用独立指标计数 `accelerated` 与 `fallback_to_puma`

 ### R2: 项目列表

 **User Story:** AS 登录用户，I want 仪表盘项目列表在 1 秒内画出，so that 打开 GitLab 首页不再空等。

 #### Acceptance Criteria

 1. WHEN 已登录用户请求项目列表（Web 仪表盘或 `GET /api/v4/projects`），THE 系统 SHALL 在 p95 小于 1000ms 内返回该用户可见项目（以生产 8 worker、单实例为测量环境）
 2. WHEN 加速项目列表，THE 系统 SHALL 返回与当前 Rails 相同的可见性规则结果（private / internal / public、成员关系）
 3. IF Gitaly 或 PostgreSQL 不可用，THE 系统 SHALL 返回与当前 Rails 相同类别的错误（5xx 或页面错误），并在指标中计 `fallback_to_puma` 或 `accelerate_error`

 ### R3: 仓库 Files API

 **User Story:** AS 开发者，I want 打开仓库文件树和文件内容不再看到 `file is missing`，so that 浏览代码可用。

 #### Acceptance Criteria

 1. WHEN 已授权用户请求仓库文件或目录（Web 或 `GET /api/v4/projects/:id/repository/files/*`），THE 系统 SHALL 返回文件内容或目录条目，状态码为 200
 2. WHEN 该路径走加速，THE 系统 SHALL 使用与 Rails 相同的仓库 ACL（Guest 不能读 private 项目）
 3. IF blob 超过 Workhorse 配置的内存上限，THE 系统 SHALL 回源 Puma 或改为流式发送，避免 Workhorse OOM

 ### R4: CI job log

 **User Story:** AS 开发者，I want 实时查看 job 日志，so that 不用等整段日志从 Rails 缓冲完。

 #### Acceptance Criteria

 1. WHEN 已授权用户请求 job trace（Web 或 `GET /api/v4/projects/:id/jobs/:job_id/trace`），THE 系统 SHALL 在首字节 500ms 内开始返回日志（生产单实例测量）
 2. WHEN job 仍在运行，THE 系统 SHALL 支持增量拉取（Range 或 GitLab 现有 poll 参数）
 3. IF 日志文件不在本地 artifacts 目录，THE 系统 SHALL 回源 Puma

 ### R5: 写路径仍以 Rails 为准

 **User Story:** AS 系统，I want 创建/更新/删除仍由 Rails 执行，so that 权限、回调、Sidekiq 副作用不丢。

 #### Acceptance Criteria

 1. WHEN 请求方法为 POST、PUT、PATCH 或 DELETE，且该路径未显式列入加速白名单，THE Workhorse SHALL 把请求转给 Puma
 2. WHEN 加速白名单仅覆盖读路径，THE 系统 SHALL 保持现有 artifacts 上传加速不变

 ### R6: 失败回源

 **User Story:** AS 用户，I want 加速出错时页面仍能打开，so that 新代码不会把 GitLab 整站打挂。

 #### Acceptance Criteria

 1. IF 加速逻辑 panic、超时（默认 3s）或返回不可用，THE Workhorse SHALL 将原请求转给 Puma
 2. WHEN 发生回源，THE Workhorse SHALL 打 warn 日志，字段包含路径模板与原因
 3. WHEN Puma 也失败，THE Workhorse SHALL 把 Puma 的状态码与响应体传回客户端

 ### R7: 版本对齐

 **User Story:** AS 运维，I want 行为继续对齐 GitLab CE 19.3.1，so that 升级官方补丁时加速层可逐项回归。

 #### Acceptance Criteria

 1. THE 系统 SHALL 以 GitLab CE 19.3.1 的 REST/Web 响应字段为加速路径的对照基线
 2. WHEN 官方 CE 小版本升级，THE 维护者 SHALL 用对照测试覆盖 R2、R3、R4 三条路径后再标记加速启用

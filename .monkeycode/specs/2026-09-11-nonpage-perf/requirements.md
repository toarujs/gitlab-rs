# Requirements Document

## Introduction

GitLab CE 19.3.1 现网入口已是 Rust Workhorse。页面（HTML、GraphQL、Web IDE、HTML 注入）本轮保持现状。本需求覆盖仍打满 Puma 的非页面路径：CI `jobs/request`、仓库 REST 读、LFS 传输、Git pack 出站压缩。

已交付且本轮不重做：Git smart HTTP 流式、`GET /api/v4/projects`、Files GET、job trace GET、artifacts 上传加速。

部署门禁与既有加速一致：先 9071，用户确认后再动 9061。本轮不 `git push`，直到用户明确允许。

## Glossary

- **Workhorse**：本仓库 Rust HTTP 前端，监听 `:80`
- **Puma**：运行 GitLab Rails 的 Ruby 应用服务器（现网 8 worker × 8 thread）
- **Rails**：GitLab CE 19.3.1，写路径与权限权威
- **加速**：Workhorse 完成鉴权与读数后直接响应，请求不再进入 Puma 业务 action
- **回源**：加速未命中或失败时，Workhorse 把原请求转给 Puma
- **CI 长轮询**：Runner 调用 `POST /api/v4/jobs/request` 时，Workhorse 在 Redis 上等待新 job 通知，空闲期少打 Puma
- **页面路径**：HTML 文档、`/api/graphql`、Web IDE 资源、HTML 注入与 nls 翻译
- **9071**：金丝雀实例 `/dockerbuild/gitlab-rs-cs`
- **9061**：生产实例 `/compose/gitlab`

## Requirements

### R1: 页面路径保持现状

**User Story:** AS 产品，I want 本轮性能改动不碰页面渲染，so that 仪表盘与 Web IDE 行为稳定。

#### Acceptance Criteria

1. WHEN 请求的 Content 为 HTML 文档、路径为 `/api/graphql`、或路径匹配 Web IDE 资源，THE Workhorse SHALL 继续使用当前处理逻辑（含现有 HTML 注入与 nls）
2. WHEN 本轮合并代码，THE 维护者 SHALL 用 `git diff` 确认 `src/html_injection.rs`、`src/webide_nls.rs` 与 GraphQL handler 无行为变更

### R2: CI 空闲轮询少打 Puma

**User Story:** AS 运维，I want runner 空闲时不再每个间隔都占满一次 Puma worker，so that CI 轮询与用户 API 抢不到同一批 Ruby 线程。

#### Acceptance Criteria

1. WHEN Runner 发送 `POST /api/v4/jobs/request` 且 Rails 返回 204，THE Workhorse SHALL 在配置的长轮询窗口内等待 Redis 上的新 job 通知后再向 Rails 重试，窗口默认 50 秒
2. WHEN Redis 在窗口内发出该 runner 的新 job 通知，THE Workhorse SHALL 在 100ms 内再次向 Rails 请求 job
3. WHEN 窗口结束仍无 job，THE Workhorse SHALL 向 Runner 返回 204
4. IF Redis 不可用，THE Workhorse SHALL 把该次 `jobs/request` 回源 Puma，行为与当前直转一致

### R3: 仓库 REST 读加速

**User Story:** AS CI 脚本与 API 客户端，I want 树、提交列表、归档下载不进 Puma，so that clone 之外的读仓库调用也能在百毫秒级返回。

#### Acceptance Criteria

1. WHEN 已授权请求 `GET /api/v4/projects/:id/repository/tree`，THE Workhorse SHALL 在 p95 小于 500ms 内返回与 CE 19.3.1 字段对齐的目录条目（9071 单实例、热缓存仓库测量）
2. WHEN 已授权请求 `GET /api/v4/projects/:id/repository/commits`（列表，无请求体），THE Workhorse SHALL 返回与 CE 19.3.1 对齐的提交数组，默认分页 20，最大 100
3. WHEN 已授权请求 `GET /api/v4/projects/:id/repository/archive` 或 `archive.:format`，THE Workhorse SHALL 流式返回归档，禁止把整个归档读进 Workhorse 内存
4. WHEN 上述三路径走加速，THE Workhorse SHALL 使用与现有 Files GET 相同的 `project_authorizations` 可读判定
5. IF session 无法解码、查询参数超出白名单、或 Gitaly/PostgreSQL 失败，THE Workhorse SHALL 回源 Puma

### R4: LFS 对象流式

**User Story:** AS 开发者，I want LFS 对象上传与下载绕开 Puma 缓冲，so that 大文件不占 Ruby 内存。

#### Acceptance Criteria

1. WHEN LFS Batch API 授权成功且对象位于本地或对象存储，THE Workhorse SHALL 按授权结果流式上传或下载该对象
2. WHEN 对象超过 5GiB，THE Workhorse SHALL 返回 413
3. IF 授权失败或存储路径缺失，THE Workhorse SHALL 回源 Puma

### R5: Git pack 出站不再二次压缩

**User Story:** AS git 客户端，I want pack 数据按 Git 原编码离开 Workhorse，so that CPU 不花在对已压缩字节再做 zstd/br。

#### Acceptance Criteria

1. WHEN 响应路径为 `/:namespace/:project.git/git-upload-pack` 或 `git-receive-pack`，THE Workhorse SHALL 原样转发 pack 字节，跳过 CompressionLayer
2. WHEN 响应 `Content-Type` 为 `application/x-git-upload-pack-result` 或 `application/x-git-receive-pack-result`，THE Workhorse SHALL 跳过 CompressionLayer
3. WHEN 其他 JSON/HTML 响应，THE Workhorse SHALL 保持现有压缩行为

### R6: 写路径仍以 Rails 为准

**User Story:** AS 系统，I want 创建 pipeline、更新 job 状态、推送以外的写 API 仍由 Rails 执行，so that Sidekiq 副作用不丢。

#### Acceptance Criteria

1. WHEN 请求方法为 POST、PUT、PATCH 或 DELETE，且路径未列入本需求加速白名单，THE Workhorse SHALL 把请求转给 Puma
2. WHEN 本轮交付，THE 系统 SHALL 保持现有 artifacts 上传加速与 Git receive-pack 流式行为

### R7: 失败回源与可观测

**User Story:** AS 运维，I want 加速失败时客户端仍拿到结果，并且能用指标对比 Puma 耗时，so that 9071 能验收。

#### Acceptance Criteria

1. IF 加速逻辑超时（默认 3s，归档流式除外）或返回不可用，THE Workhorse SHALL 将原请求转给 Puma
2. WHEN 发生回源，THE Workhorse SHALL 打 warn 日志，字段包含路径模板与原因
3. WHEN 加速命中、回源或出错，THE Workhorse SHALL 递增 `gitlab_rs_hotpath_result_total{path,result}`，`result` 为 `hit`、`fallback` 或 `error`
4. WHEN 9071 验收，THE 维护者 SHALL 给出 `jobs/request` 与 `repository/tree` 在加速开关开/关下的 p95 对照

### R8: 版本与部署

**User Story:** AS 运维，I want 行为继续对齐 GitLab CE 19.3.1，并且先在 9071 验证，so that 9061 用户不受半成品影响。

#### Acceptance Criteria

1. THE 系统 SHALL 以 GitLab CE 19.3.1 的 REST 响应字段为 R3、R4 的对照基线
2. WHEN 本轮镜像准备就绪，THE 维护者 SHALL 只热更新 9071 的 web 容器
3. WHEN 用户确认 9071 后，THE 维护者 SHALL 再把同一镜像 ID 用于 9061

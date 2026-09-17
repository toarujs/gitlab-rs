 # Rails / Puma 热点加速

 Feature Name: rails-puma-accelerate
 Updated: 2026-09-10

 ## Description

 不替换 GitLab Rails，也不先换掉 Puma。在 Rust Workhorse 里按 artifacts 已验证的 accelerate 模式，把三条读路径从 Puma 前移：项目列表、仓库 Files API、CI job log。Puma 继续承担写路径、权限权威、长尾页面。

 生产对照：`toarujs/gitlab-rs:19.3.1`，Puma 8 worker，Workhorse 为 PID 1。

 ## Architecture

 ```mermaid
 graph TD
     Client["Client / Runner"] --> WH["Rust Workhorse :80"]
     WH --> Accel["HotPath accelerator"]
     WH --> Puma["Puma + GitLab Rails"]
     Accel --> PG["PostgreSQL 17"]
     Accel --> Redis["Redis 7"]
     Accel --> Gitaly["Gitaly Go"]
     Puma --> PG
     Puma --> Redis
     Puma --> Gitaly
     Accel -->|"miss or error"| Puma
 ```

 请求进入 Workhorse 后先匹配加速白名单。命中则：校验 session/job token → 读 Redis/PG/Gitaly → 直接响应。未命中或失败则原样回源 Puma unix socket。

 ## Components and Interfaces

 ### 1. HotPath router

 位置：`src/routes/` 新增模块，由 `src/main.rs` 在 catch-all 之前注册。

 白名单（第一批）：

 | 路径 | 方法 | 数据来源 |
 |------|------|----------|
 | `/api/v4/projects` | GET | PostgreSQL + Redis session |
 | `/api/v4/projects/:id/repository/files/*` | GET | Gitaly + ACL in PostgreSQL |
 | `/api/v4/projects/:id/jobs/:job_id/trace` | GET | artifacts 目录或 Puma |

 Web 页（`/` 仪表盘）第一期仍回源 Puma；API 变快后，前端 JSON 已受益。仪表盘 HTML 作为第二期。

 ### 2. Auth gate

 复用 Workhorse 已有 session cookie / `_gitlab_session` 解密能力；没有则回源，避免在 Rust 里重写整套 Warden。

 Job log 使用 CI job token，对齐 `/api/v4/jobs/:id/artifacts` 已走通的 token 校验。

 ### 3. ACL reader

 只读 PostgreSQL：`members`、`project_authorizations`、`namespaces`、`projects`。不在 Rust 里重写 DeclarativePolicy 全树；用 GitLab 已物化的 `project_authorizations` 表判断 `can_read_project`。

 ### 4. Gitaly client

 Files API 走现有 `src/gitaly/` sidechannel / blob RPC。JSON 字段对齐 CE 19.3.1 `RepositoryFiles` 序列化（`file_name`、`content` base64、`encoding`、`blob_id`、`commit_id`、`last_commit_id`）。

 ### 5. Metrics

 Prometheus 计数器/直方图：`gitlab_rs_hotpath_duration_seconds{path,result}`，`result` 为 `hit` / `fallback` / `error`。

 ## Data Models

 ```text
 HotPathDecision {
   path_template: String
   user_id: Option<i64>
   result: hit | fallback | error
   reason: String
 }

 ProjectListItem {
   id, name, path_with_namespace, visibility, last_activity_at
 }

 RepositoryFileBody {
   file_name, file_path, size, encoding, content, blob_id, commit_id, last_commit_id
 }
 ```

 Session：继续用 Rails cookie store；Rust 只读 `user_id`。读失败 → 回源。

 ## Correctness Properties

 - 加速响应的可见项目集合等于同一用户打 Puma 的集合（对照测试，抽样 20 个用户）
 - Guest 读 private 仓库 Files API 得到 404，与 Rails 一致
 - 加速超时默认 3s，超时后回源，客户端只收到一次响应
 - 写路径（POST/PUT/PATCH/DELETE，除已有 artifacts 上传）继续进 Puma

 ## Error Handling

 | 场景 | 处理 |
 |------|------|
 | session 无法解码 | 回源 Puma |
 | PG 连接失败 | 回源 Puma，计 error |
 | Gitaly 超时 | 回源 Puma；若回源也失败，返回 Puma 状态码 |
 | 文件超过内存上限 | 流式发送或回源，禁止整包读入 |
 | panic | catch 后回源；回源失败则 502 |
 | Git 授权 JSON 字段为 null | 当空字符串/空 map，继续 Gitaly |
 | Git 502 | 日志带 `error_class`（json_decode / missing_repo_fields / gitaly_connect / sidechannel 等），响应体不回传 Workhorse JSON |

 ## Test Strategy

 1. 单元：ACL 查询、Files JSON 字段、trace Range 解析
 2. 对照：对同一 cookie 分别打加速与强制回源，diff JSON（忽略 `created_at` 之外的易变字段）
 3. 生产：bak.toarujs.com 上对比加速开关开/关的 p95（项目列表、Files、job log）
 4. 回归：`cargo test --lib`；artifacts 上传 201 仍须保持

 ## Phases

 1. 指标：给 Puma 回源加上路径模板与耗时（约 0.5 天）
 2. Files GET 加速，修现有 `file is missing`（约 2 天）
 3. job log / trace 加速（约 1 天）
 4. `GET /api/v4/projects` 加速（约 2 天）
 5. 仪表盘 HTML 是否继续抽，看指标再定

 ## 明确不做

 - 用 Rust 重写 gitlab-rails
 - 替换 Puma 进程模型（第一期）
 - 在 Rust 里实现完整 DeclarativePolicy

 ## References

 [^1]: (Filename) - artifacts 加速实现 `src/upload/accelerate.rs`
 [^2]: (Filename) - 代理与回源 `src/proxy/mod.rs`
 [^3]: (Filename) - 路由注册 `src/main.rs`
 [^4]: (Filename) - 需求 `.monkeycode/specs/2026-09-08-rails-puma/requirements.md`

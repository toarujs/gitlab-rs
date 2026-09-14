# Requirements Document

## Introduction

现网 9061 与 9071 的 CI 由官方 `gitlab/gitlab-runner:19.3.1` 执行，executor 均为 `docker`，通过挂载 `/var/run/docker.sock` 在 `gitlab-bridge` / `gitlab-rs-cs_gitlab-rs-net` 上跑 job。本需求用 Rust 实现可替换该进程的 Runner，协议与 `config.toml` 对齐 GitLab CE 19.3.1。

部署门禁：只在 9071 替换 runner 容器。用户复测通过后，再把同一镜像用于 9061，并推送源码。9071 复测完成前，9061 runner 与 git 远程保持不动。

现网正在使用的能力（9071 第一门禁必须覆盖）：docker executor、`jobs/request` 长轮询、git clone（`clone_url`）、脚本执行、trace 上传、job 终态。官方 runner 其余 executor 与高级能力按里程碑补齐，未实现的 executor 在启动时以非零退出拒绝，避免静默降级。

## Glossary

- **Rust Runner**：本仓库将实现的 Rust CI 执行器进程，用于替换官方 `gitlab-runner`
- **官方 Runner**：`gitlab/gitlab-runner:19.3.1`（git revision `a16f5092`）
- **GitLab API**：GitLab CE 19.3.1 的 runner job API（`/api/v4/jobs/*`）
- **config.toml**：官方 Runner 的配置文件，默认 `/etc/gitlab-runner/config.toml`
- **docker executor**：用 Docker Engine 按 job 镜像创建容器并执行脚本
- **9071**：金丝雀实例，compose `/dockerbuild/gitlab-rs-cs`，runner 容器 `gitlab-rs-cs-runner`
- **9061**：生产实例，compose `/compose/gitlab`，runner 容器 `gitlab-runner`
- **Job**：GitLab 分配给 runner 的一次 CI 任务
- **Trace**：job 日志，经 `PATCH /api/v4/jobs/:id/trace` 增量上传
- **Helper**：官方 `gitlab-runner-helper` 镜像，用于容器内 clone 与 artifacts

## Requirements

### R1: 进程级替换

**User Story:** AS 运维，I want 用 Rust Runner 容器替换官方 runner 容器并继续读同一份 `config.toml`，so that 现网 compose 与注册 token 不用重做。

#### Acceptance Criteria

1. WHEN 运维把 9071 compose 中 runner 服务的镜像换成 Rust Runner 镜像，THE Rust Runner SHALL 读取 `/etc/gitlab-runner/config.toml` 中的 `url`、`clone_url`、`token`、`executor`、`concurrent` 与 `[runners.docker]` 字段后开始轮询
2. WHEN `config.toml` 中 `executor` 为 `docker`，THE Rust Runner SHALL 使用该 runner 段的 `image`、`network_mode`、`pull_policy`、`volumes`、`privileged` 创建 job 容器
3. WHEN `config.toml` 中 `executor` 为当前里程碑未实现的类型，THE Rust Runner SHALL 以非零状态退出，并在 stderr 打印该 executor 名称

### R2: Job 协议

**User Story:** AS GitLab，I want Rust Runner 使用与官方 19.3.1 相同的 job API，so that Rails 侧无需改协议。

#### Acceptance Criteria

1. WHEN Rust Runner 空闲且 `concurrent` 未满，THE Rust Runner SHALL 向 `url` 发送 `POST /api/v4/jobs/request`，请求头携带 runner token
2. WHEN GitLab 返回 204，THE Rust Runner SHALL 按 `check_interval`（为 0 时使用官方默认间隔）再次请求
3. WHEN GitLab 返回 201 与 job payload，THE Rust Runner SHALL 执行该 job，并通过 `PUT /api/v4/jobs/:id` 上报 `running`、`success` 或 `failed`
4. WHILE job 在执行，THE Rust Runner SHALL 将脚本输出以官方兼容的 Content-Range 增量写入 `PATCH /api/v4/jobs/:id/trace`

### R3: Docker 执行

**User Story:** AS 开发者，I want `.gitlab-ci.yml` 里的 docker job 在 9071 上跑出与官方 runner 相同的 success/failed，so that 现有流水线可继续用。

#### Acceptance Criteria

1. WHEN job payload 指定 `image`，THE Rust Runner SHALL 按 `pull_policy` 拉取或复用该镜像，并在 `network_mode` 指定的网络中启动容器
2. WHEN job 含 `services`，THE Rust Runner SHALL 启动对应 service 容器，并把 hostname 提供给 job 容器
3. WHEN job 脚本退出码为 0，THE Rust Runner SHALL 将 job 状态更新为 `success`
4. WHEN job 脚本退出码非 0 或容器无法启动，THE Rust Runner SHALL 将 job 状态更新为 `failed`，并在 trace 中写入失败原因
5. WHEN job 结束，THE Rust Runner SHALL 删除本次创建的 job 容器与 service 容器

### R4: 仓库检出

**User Story:** AS job，I want 在容器工作目录拿到当前 commit 的仓库，so that 脚本能编译和测试。

#### Acceptance Criteria

1. WHEN job payload 含 git 信息，THE Rust Runner SHALL 使用 `clone_url` 与 job 提供的凭证检出指定 SHA 到工作目录
2. WHEN `.gitlab-ci.yml` 设置 `GIT_STRATEGY=clone` 或未设置（现网默认），THE Rust Runner SHALL 执行完整 clone 后再 checkout 该 SHA
3. IF clone 或 checkout 失败，THE Rust Runner SHALL 将 job 标记为 `failed` 并在 trace 中写入 git 错误

### R5: 变量与脚本阶段

**User Story:** AS 开发者，I want `before_script` / `script` / `after_script` 与 CI 变量在容器内生效，so that 现有 `.gitlab-ci.yml` 不用改。

#### Acceptance Criteria

1. WHEN job payload 含 variables，THE Rust Runner SHALL 把文件类型以外的变量注入 job 容器环境
2. WHEN job 含 `before_script` 与 `script`，THE Rust Runner SHALL 按该顺序在容器内执行
3. WHEN `script` 失败且存在 `after_script`，THE Rust Runner SHALL 仍执行 `after_script`，最终状态保持 `failed`
4. WHEN 变量标记为 masked，THE Rust Runner SHALL 在写入 trace 前用 `[MASKED]` 替换对应明文

### R6: Artifacts

**User Story:** AS 开发者，I want job 的 `artifacts` 能上传并被后续 job 下载，so that 构建产物可传递。

#### Acceptance Criteria

1. WHEN job 成功且 payload 含 artifacts 路径，THE Rust Runner SHALL 将匹配文件打包上传到 `POST /api/v4/jobs/:id/artifacts`
2. WHEN job payload 含依赖 artifacts，THE Rust Runner SHALL 在脚本开始前下载并解压到工作目录
3. IF artifacts 上传失败，THE Rust Runner SHALL 将 job 标记为 `failed` 并在 trace 中写入上传错误

### R7: 并发与关闭

**User Story:** AS 运维，I want runner 遵守 `concurrent` 并在停容器时把进行中的 job 收尾，so that 热更新不会丢半截日志。

#### Acceptance Criteria

1. WHILE 正在运行的 job 数量等于 `concurrent`，THE Rust Runner SHALL 停止发起新的 `jobs/request`
2. WHEN 进程收到 SIGTERM，THE Rust Runner SHALL 停止接新 job，等待进行中 job 完成或达到 `shutdown_timeout` 后再退出
3. WHEN `shutdown_timeout` 为 0，THE Rust Runner SHALL 等到进行中 job 结束后再退出

### R8: 9071 门禁

**User Story:** AS 运维，I want 只在 9071 替换 runner 并跑通与现网相同的 docker smoke job，so that 9061 在复测前保持官方 runner。

#### Acceptance Criteria

1. WHEN Rust Runner 镜像在 9071 上替换 `gitlab-rs-cs-runner`，THE 系统 SHALL 保持 9061 容器 `gitlab-runner` 仍使用 `gitlab/gitlab-runner:latest`
2. WHEN 9071 上创建一个仅含 `script: echo ok`、镜像 `ubuntu:latest` 的项目并触发 pipeline，THE job SHALL 在 60 秒内变为 `success`
3. WHEN 该门禁 job 结束，THE 测试项目 SHALL 被删除
4. WHEN 用户尚未在 9071 复测确认，THE 操作者 SHALL 保持 9061 runner 镜像与 git 远程不被更新

### R9: 其余官方能力（后续里程碑）

**User Story:** AS 运维，I want 后续把 shell、ssh、kubernetes、cache 后端与 session server 补齐，so that Rust Runner 能完整替换官方 19.3.1。

#### Acceptance Criteria

1. WHEN docker executor 门禁在 9071 通过，THE 后续里程碑 SHALL 依次实现 `shell` executor、`ssh` executor、`kubernetes` executor
2. WHEN 实现 cache，THE Rust Runner SHALL 支持现网已出现的 `[runners.cache]` 段以及 docker volume `/cache`
3. WHEN 实现 session server，THE Rust Runner SHALL 监听 `config.toml` 中 `[session_server]` 并提供与官方兼容的 job 终端会话

### R10: 可观测

**User Story:** AS 运维，I want 看到 request 间隔、job 耗时和失败原因，so that 金丝雀出问题能对比官方 runner。

#### Acceptance Criteria

1. WHEN Rust Runner 完成一次 `jobs/request`，THE Rust Runner SHALL 记录 HTTP 状态码与耗时
2. WHEN job 结束，THE Rust Runner SHALL 记录 job id、终态、执行耗时、失败原因（若有）
3. WHEN 调用 GitLab API 失败，THE Rust Runner SHALL 记录路径、状态码与错误体摘要

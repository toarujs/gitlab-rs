# 需求实施计划

- [x] 1. 基线可观测：Puma 回源记录路径模板与耗时
  - 新增 `src/hotpath/mod.rs`：把具体路径收成模板（数字 id、sha、files/*）
  - 在 `src/metrics/mod.rs` 增加 `gitlab_rs_hotpath_duration_seconds{path,result}` 与 `gitlab_rs_hotpath_result_total{result}`
  - 代理到 Puma 时记录 method、path_template、status、workhorse_ms、puma_ms
  - 同一模板 5 分钟内超过 50 次后，按模板聚合 p50/p95 并写入 histogram
  - 对应 R1
  - [x] 1.1 为 path_template 与窗口分位数写单元测试

- [x] 2. Files GET 加速
  - 白名单 `GET /api/v4/projects/:id/repository/files/*`
  - session 解码失败则回源 Puma
  - 用 `project_authorizations` 判断可读；Guest 读 private 返回 404
  - Gitaly blob JSON 字段对齐 CE 19.3.1
  - 超过内存上限则回源或流式发送
  - 对应 R3、R5、R6、R7

- [x] 3. 检查点 - 确保所有测试通过
  - 确保所有测试通过,如有疑问请询问用户

- [x] 4. CI job log / trace 加速
  - 白名单 `GET /api/v4/projects/:id/jobs/:job_id/trace`
  - 本地 artifacts 存在则直接读，支持 Range；不在本地则回源
  - 对应 R4、R6

- [ ] 5. `GET /api/v4/projects` 加速
  - 读 `project_authorizations` + `projects`，可见性与 Rails 一致
  - PG 失败回源
  - 对应 R2、R6

- [ ] 6. 检查点 - 确保所有测试通过
  - 确保所有测试通过,如有疑问请询问用户

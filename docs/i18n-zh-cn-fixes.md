# zh-CN 硬编码文案修复清单

日期：2026-10-17

## 背景

这些文案在 GitLab 上游 Ruby 代码中直接写死为英文字符串，未经过 gettext，
因此即使界面语言为简体中文也始终显示英文。修复采用运行时 prepend
initializer，不替换上游源文件：

- `docker/gitlab-rails-overrides-server/config/initializers/gitlab_rs_ci_i18n.rb`
- `docker/gitlab-rails-overrides/config/initializers/gitlab_rs_ci_i18n.rb`（CE 副本）
- 翻译真源：`locale/zh_CN/ci_messages.po.fragment`（116 条）
- 翻译真源：`locale/zh_CN/validation_messages.po.fragment`（校验文案）
- 翻译真源：`locale/zh_CN/service_messages.po.fragment`（303 条）
- 合并工具：`scripts/i18n/merge_po.py`

## CI/CD 作业与流水线（116 条）

| 页面/接口 | 文案位置 | 条数 | 代表英文原文（msgid） | 修复后 | 状态 |
| --- | --- | ---: | --- | --- | --- |
| 作业详情页红色告警条 | `CommitStatusPresenter::CALLOUT_FAILURE_MESSAGES` | 21 | `There has been a timeout failure or the job got stuck. Check your timeout limits or try again` | 发生超时失败或作业卡住。请检查超时限制后重试 | 已修复 |
| 作业状态徽章 tooltip | `Gitlab::Ci::Status::Build::Failed#description` + `REASONS` | 29 | `stuck or timeout failure` | 作业卡住或超时 | 已修复 |
| 失败原因短标签 | `Gitlab::Ci::Status::Build::Failed::REASONS` | 49 | `no matching runner available` | 没有可用的匹配 Runner | 已修复 |
| 流水线失败原因 | `Ci::PipelinePresenter#failure_reasons` | 12 | `The pipeline failed due to an error on the CI/CD configuration file.` | 流水线因 CI/CD 配置文件错误而失败。 | 已修复 |
| 作业/流水线自动取消标题 | `CommitStatusPresenter` / `Ci::PipelinePresenter#status_title` | 2 | `Pipeline is redundant and is auto-canceled by Pipeline #%{id}` | 流水线是冗余的，已被流水线 #%{id} 自动取消 | 已修复 |
| 空流水线规则提示 | `Ci::HasCompletionReason#rules_failure_message` 等 | 2 | `The resulting pipeline would have been empty. Review the %{rules_link} configuration.` | 生成的流水线将为空。请检查 %{rules_link} 配置。 | 已修复 |
| 告警条修复链接 | `CommitStatusPresenter#help_page_link` | 1 | `How do I fix it?` | 如何修复？ | 已修复 |

## 表单与校验（83 条）

| 页面/模块 | 文案位置 | 条数 | 代表英文原文（msgid） | 修复后 | 状态 |
| --- | --- | ---: | --- | --- | --- |
| SSH 密钥 / 部署密钥 | `SshKeyValidator` | 3 | `is not a valid SSH key` | 不是有效的 SSH 密钥 | 已修复 |
| 签名密钥 / 证书 | `RsaKeyValidator`、证书校验器 | 6 | `is not a valid RSA key` | 不是有效的 RSA 密钥 | 已修复 |
| 通用校验器 | `duration`、`color`、`emoji`、`SHA` 等 | 26 | `is not a correct duration` | 不是有效的时长 | 已修复 |
| CI/CD 变量 | `Ci::HidableVariable` | 3 | `Only new variables can be set as masked and hidden.` | 只有新建变量才能设置为掩码和隐藏。 | 已修复 |
| Runner 表单 | `Ci::Runner`、`Ci::RunnerManager` | 11 | `needs to be assigned to at least one project` | 需要至少分配给一个项目 | 已修复 |
| 流水线/作业令牌/制品 | `Ci::Pipeline`、`Ci::JobArtifact` 等 | 6 | `exceeds the allowable number of group links` | 超出允许的群组关联数量 | 已修复 |
| 路径/名称/环境/变量 key | `Gitlab::PathRegex`、`Gitlab::Regex`、CI 变量 key | 14 | `can only include non-accented letters, digits, '_', '-' and '.'. ...` | 只能包含非重音字母、数字、'_'、'-' 和 '.'。... | 已修复 |
| 管理后台/集成/个人资料 | `ApplicationSetting`、Slack、Confluence、ProjectLabel、UserDetail、Cells | 9 | `Incremental logging cannot be turned on without configuring object storage for artifacts.` | 未配置制品对象存储时，无法开启增量日志。 | 已修复 |
| 通用 `:invalid` 校验 | `ActiveModel::Error#message` | — | `is invalid` | 是无效的 | 已修复 |
| 模型属性名 | `activerecord.attributes.*` | 2 | `Fingerprint sha256` / `Domain denylist` | 指纹（SHA256）/ 域名拒绝列表 | 已修复 |

## 修复前后对比

| 场景 | 修复前 | 修复后 |
| --- | --- | --- |
| 作业详情告警条（`/-/jobs/1.json` 的 `callout_message`） | `There has been a timeout failure or the job got stuck...` | `发生超时失败或作业卡住。请检查超时限制后重试` |
| 作业状态徽章（`status.tooltip`） | `失败 - (stuck or timeout failure)` | `失败 - (作业卡住或超时)` |
| 流水线失败原因 | 英文 | 中文 |
| CI/CD 变量掩码/隐藏校验 | 英文 | 中文 |
| Runner 分配校验 | 英文 | 中文 |
| 项目/群组路径校验 | 英文 | 中文 |
| 通用 `is invalid` | `is invalid` | `是无效的` |
| SSH 密钥指纹校验 | `Fingerprint sha256无法生成` | `指纹（SHA256）无法生成` |
| 应用设置域名拒绝列表 | `Domain denylist启用...` | `域名拒绝列表启用...` |

英文（`en`）与日文（`ja`）回退保持原文，不受影响。

## 验收方式

在金丝雀 9071 与生产 9061 均执行：

```bash
docker exec <container> gitlab-rails runner \
  "Gitlab::I18n.with_locale('zh_CN') { puts Ci::BuildPresenter.new(Ci::Build.find(1), current_user: User.find_by_username('root')).callout_failure_message }"
```

并通过 Rails integration session 登录后抓取 `/-/jobs/1.json`，确认
`callout_message`、`status.tooltip`、`status.text` 均为中文。

模型属性与 `:invalid` 的回归验证：在金丝雀 `/-/user_settings/ssh_keys`
提交无效 SSH 密钥，表单错误应显示 `键类型被禁止。必须为 ...`、
`键是无效的`、`指纹（SHA256）无法生成`；页面中不应再出现
`Fingerprint sha256` 或 `is invalid`。

`ServiceResponse` 与 JSON 错误文案（2026-10-19）：

```bash
docker exec <container> gitlab-rails runner '
I18n.with_locale(:zh_CN) do
  puts ServiceResponse.error(message: "Branch was deleted").message
  p GitlabRs::CiI18n.localize_json_messages({ message: "Invalid parameters" })
end
'
```

期望分别为 `分支已删除` 与 `{message: "参数无效"}`；`I18n.with_locale(:en)` 仍为英文。

## 残留与不在范围

- 其他模型属性名在 po 未覆盖时仍可能显示英文；新增属性名可按
  `activerecord.attributes.<model>.<attribute>` 追加 msgid。
- `ServiceResponse.error(message:)` 与 `render json: { message: }` 已用
  initializer 运行时翻译；audit log、后台任务日志、worker 进度、多行
  字面量片段、registry 协议码仍保持英文。
- 前端 `public/assets/locale/zh_CN/app-*.js.gz` 是独立翻译包，本次未修改。

## ServiceResponse 与 JSON 错误（303 条，2026-10-19）

| 页面/接口 | 文案位置 | 条数 | 代表英文原文（msgid） | 修复后 | 状态 |
| --- | --- | ---: | --- | --- | --- |
| 服务对象错误/成功消息 | `ServiceResponse#error` / `#message` | 236 | `You don't have permissions to archive this project.` | 您没有归档此项目的权限。 | 已修复 |
| 控制器 JSON `message` | `render json: { message: ... }` | 若干 | `Invalid parameters` | 参数无效 | 已修复 |
| 校验补充与属性名 | `errors.add` / `activerecord.attributes.member.access_level` | 若干 | `timezone is not valid` / `Access level` | 无效 / 访问级别 | 已修复 |
| 其它用户可见 hash 风格错误 | `Result.new(error_message:)`、legacy `error(message:)` | 若干 | `Work item not found` | 未找到工作项 | 已入库 |

# Requirements Document

## Introduction

管理员在「新用户」页生成一次性邀请链接。可生成匿名链接，也可指定电子邮件；二者都要验证电子邮件可用。生成时可指定 1 至 720 小时过期。可选电子邮件服务白名单，默认关闭。

## Glossary

- **Invite link**: 一次性、有过期时间的注册 URL
- **Invitee**: 打开邀请链接并提交注册表单的人
- **Access level**: Regular 或 Administrator
- **Anonymous invite**: 未绑定电子邮件的邀请；受邀者在验证步骤自行填写地址
- **Bound invite**: 绑定单一电子邮件的邀请；验证步骤使用该地址且不可更改
- **Email service allowlist**: 管理员配置的电子邮件域名列表；开启后仅列表中的域名可通过邀请验证
- **Batch receipt**: 批量生成结果清单，逐条标注成功、跳过原因、是否已发信
- **Expiry hours**: 管理员指定的邀请有效小时数，整数，范围为 1 至 720

## Requirements

### Requirement 1

**User Story:** AS 管理员, I want 在新用户页「帐户」旁生成一次性邀请链接, so that 不必代填姓名邮箱密码

#### Acceptance Criteria

1. WHEN 管理员打开 `/admin/users/new`, THE 系统 SHALL 在「帐户」标题旁显示「一次性邀请链接」按钮
2. WHEN 管理员点击该按钮, THE 系统 SHALL 展示权限选项（Regular / Administrator、External）并提供生成动作
3. WHEN 管理员生成链接且未填写过期小时数, THE 系统 SHALL 返回可复制的绝对 URL，并将有效期设为 24 小时
4. WHEN 管理员在已生成的链接旁点击刷新, THE 系统 SHALL 再生成一条新的一次性链接，沿用当前填写的过期小时数，且不使旧链接失效

### Requirement 2

**User Story:** AS 受邀者, I want 打开邀请链接后自己注册, so that 密码只有自己知道

#### Acceptance Criteria

1. WHEN 受邀者打开未使用且未过期的邀请链接, THE 系统 SHALL 进入电子邮件验证步骤
2. WHEN 受邀者完成邮箱验证并提交合法帐户表单, THE 系统 SHALL 创建对应用户、套用管理员预置权限，并使该链接立即失效
3. IF 链接不存在、已使用或已过期, THE 系统 SHALL 展示邀请无效说明

### Requirement 4

**User Story:** AS 受邀者, I want 先验证电子邮件再完善帐户, so that 该邮箱确实由我控制

#### Acceptance Criteria

1. WHEN 受邀者打开有效邀请链接, THE 系统 SHALL 先展示电子邮件验证步骤
2. WHEN 受邀者提交合法且可用的电子邮件, THE 系统 SHALL 向该地址发送 6 位验证码，有效期 10 分钟
3. WHEN 受邀者提交正确验证码, THE 系统 SHALL 进入完善帐户步骤，邮箱只读
4. IF 验证码错误、过期或电子邮件不可用, THE 系统 SHALL 展示错误并保留邀请链接
5. WHEN 受邀者在已验证邮箱后提交合法帐户表单, THE 系统 SHALL 创建用户并使邀请链接失效

### Requirement 3

**User Story:** AS 运维, I want 官方 CE 切到自改只改镜像名, so that 升级路径保持 drop-in

#### Acceptance Criteria

1. THE 系统 SHALL 用 Redis 保存邀请令牌
2. THE 系统 SHALL 仅管理员会话可生成邀请链接
3. WHEN 从自改切回官方 CE, THE 运维 SHALL 先运行 `scripts/cleanup-gitlab-rs-overlay.sh` 再改 compose `image:`

### Requirement 5

**User Story:** AS 管理员, I want 生成时填写过期小时数, so that 短期活动和长期入职使用不同有效期

#### Acceptance Criteria

1. WHEN 管理员生成单条或批量邀请, THE 系统 SHALL 接受 1 至 720 的整数作为过期小时数
2. IF 过期小时数缺失或空白, THE 系统 SHALL 将过期小时数视为 24
3. IF 过期小时数不是 1 至 720 的整数, THE 系统 SHALL 拒绝生成并展示错误
4. WHEN 邀请已超过管理员指定的过期时间, THE 系统 SHALL 将邀请视为无效并展示邀请无效说明

### Requirement 6

**User Story:** AS 管理员, I want 一次生成多条匿名邀请和指定邮箱邀请, so that 不必逐条点击

#### Acceptance Criteria

1. WHEN 管理员提交批量邀请并指定匿名条数, THE 系统 SHALL 生成对应数量的匿名邀请，每条拥有独立 URL
2. WHEN 管理员提交批量邀请并提供电子邮件列表, THE 系统 SHALL 为每个通过校验的地址生成一条绑定邀请
3. WHEN 批量生成完成, THE 系统 SHALL 向管理员展示每条邀请的 URL、类型（匿名或绑定）、绑定地址（若有）和过期时间
4. WHEN 批量生成完成, THE 系统 SHALL 提供批量回执，逐条标注成功或跳过原因以及是否已发信
5. IF 列表中某条电子邮件未通过校验, THE 系统 SHALL 跳过该地址、继续处理其余项，并在批量回执中标注跳过原因
6. IF 匿名条数为 0 且电子邮件列表为空, THE 系统 SHALL 拒绝生成并展示错误
7. THE 系统 SHALL 不限制单次匿名条数与电子邮件列表长度

### Requirement 7

**User Story:** AS 管理员, I want 既可发匿名链接也可指定邮箱, so that 当面分享和定向邀请都能用

#### Acceptance Criteria

1. WHEN 管理员未指定电子邮件, THE 系统 SHALL 生成匿名邀请
2. WHEN 管理员指定电子邮件且该地址通过校验, THE 系统 SHALL 生成绑定到该地址的邀请
3. WHEN 受邀者打开绑定邀请, THE 系统 SHALL 在验证步骤展示绑定地址，并阻止更改该地址
4. WHEN 受邀者打开匿名邀请, THE 系统 SHALL 允许受邀者填写电子邮件
5. WHEN 绑定邀请创建成功, THE 系统 SHALL 向管理员展示可复制 URL
6. WHEN 绑定邀请创建成功且管理员选择发送邮件, THE 系统 SHALL 向绑定地址发送包含邀请链接的邮件
7. WHEN 管理员打开生成弹层, THE 系统 SHALL 将发送邮件选项设为开启

### Requirement 8

**User Story:** AS 管理员, I want 校验电子邮件可用并可选限制邮箱域名, so that 减少已占用邮箱和临时邮箱

#### Acceptance Criteria

1. WHEN 管理员或受邀者提交电子邮件, THE 系统 SHALL 在该地址已被实例用户使用时拒绝并展示错误
2. THE 系统 SHALL 在官方 Admin Settings 的注册设置中提供电子邮件服务白名单，默认关闭
3. WHILE 电子邮件服务白名单关闭, THE 系统 SHALL 接受符合格式的电子邮件（仍执行占用校验）
4. WHILE 电子邮件服务白名单开启, THE 系统 SHALL 仅接受域名位于白名单中的电子邮件
5. IF 白名单开启且电子邮件域名不在白名单中, THE 系统 SHALL 拒绝该地址并展示错误

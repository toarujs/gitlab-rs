# Requirements Document

## Introduction

管理员在「新用户」页生成一次性邀请链接。受邀者用该链接自行填写帐户信息完成注册；管理员只预置访问级别等权限。

## Glossary

- **Invite link**: 一次性、有过期时间的注册 URL
- **Invitee**: 打开邀请链接并提交注册表单的人
- **Access level**: Regular 或 Administrator

## Requirements

### Requirement 1

**User Story:** AS 管理员, I want 在新用户页「帐户」旁生成一次性邀请链接, so that 不必代填姓名邮箱密码

#### Acceptance Criteria

1. WHEN 管理员打开 `/admin/users/new`, THE 系统 SHALL 在「帐户」标题旁显示「一次性邀请链接」按钮
2. WHEN 管理员点击该按钮, THE 系统 SHALL 展示权限选项（Regular / Administrator、External）并提供生成动作
3. WHEN 管理员生成链接, THE 系统 SHALL 返回可复制的绝对 URL，令牌有效期为 7 天

### Requirement 2

**User Story:** AS 受邀者, I want 打开邀请链接后自己注册, so that 密码只有自己知道

#### Acceptance Criteria

1. WHEN 受邀者打开未使用且未过期的邀请链接, THE 系统 SHALL 展示名称、用户名、电子邮件、密码表单
2. WHEN 受邀者提交合法表单, THE 系统 SHALL 创建对应用户、套用管理员预置权限，并使该链接立即失效
3. IF 链接不存在、已使用或已过期, THE 系统 SHALL 展示邀请无效说明

### Requirement 3

**User Story:** AS 运维, I want 该功能以 overlay 交付且不新增 SQL 表, so that 官方 CE 升级仍可只改镜像名

#### Acceptance Criteria

1. THE 系统 SHALL 用 Redis 保存邀请令牌
2. THE 系统 SHALL 仅管理员会话可生成邀请链接

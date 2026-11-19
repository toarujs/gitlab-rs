# Requirements Document

## Introduction

从 GitHub、Gitee 或其他 Git URL 导入到本实例的项目，在项目标题旁提供与 Gitee 相同的同步入口：一键从上游仓库强制拉取并更新本地引用。维护者不必再手动 `git remote` / `git fetch`。

## Glossary

- **Imported project**: 通过 Git URL、GitHub、Gitee 或其他 Git 导入器创建、且系统记录了上游仓库地址的项目
- **Upstream URL**: 导入来源的 Git 地址，例如 `https://github.com/toarujs/index-tts.git`
- **Sync button**: 项目标题旁的圆形刷新按钮
- **Force sync**: 从 Upstream URL 拉取全部分支与标签，并用上游引用覆盖本仓库对应引用
- **Sync operator**: 对该项目拥有 Owner 权限的已登录用户
- **Sync job**: 一次强制同步任务，含排队、进行中、成功、失败四种状态

## Requirements

### Requirement 1

**User Story:** AS 项目维护者, I want 在导入项目的标题旁看到同步按钮, so that 能立刻识别该项目可以拉上游

#### Acceptance Criteria

1. WHEN 当前项目存在可用的 Upstream URL 且当前用户是该项目的 Owner, THE 系统 SHALL 在项目标题右侧显示 Sync button
2. WHEN 指针悬停在 Sync button 上, THE 系统 SHALL 显示文案「点击从 {Upstream URL} 强制同步。」
3. IF 当前项目没有 Upstream URL, THE 系统 SHALL 保持项目标题区域不含 Sync button
4. IF 当前用户对项目的权限低于 Owner, THE 系统 SHALL 保持项目标题区域不含 Sync button

### Requirement 2

**User Story:** AS 项目维护者, I want 点击按钮后从上游强制同步, so that 本地仓库与上游引用对齐

#### Acceptance Criteria

1. WHEN 项目 Owner 点击 Sync button 且当前没有进行中的 Sync job, THE 系统 SHALL 创建 Sync job 并从 Upstream URL 拉取全部分支与标签
2. WHEN Force sync 成功, THE 系统 SHALL 将本仓库中与上游同名的分支和标签更新为上游对应提交，并提示同步成功
3. WHEN Force sync 正在执行, THE 系统 SHALL 将 Sync button 置于进行中状态，并拒绝同一项目的第二个 Sync job
4. IF Upstream URL 不可达、认证失败或 Git 拉取失败, THE 系统 SHALL 将 Sync job 标为失败，并展示失败原因
5. IF 当前用户对项目的权限低于 Owner, THE 系统 SHALL 拒绝同步请求并返回 403

### Requirement 3

**User Story:** AS 项目维护者, I want 导入完成后仍能同步, so that 导入器清掉临时字段后按钮还在

#### Acceptance Criteria

1. WHEN 项目导入成功且导入器提供了来源 Git 地址, THE 系统 SHALL 将该地址保存为该项目的 Upstream URL
2. WHEN 页面渲染导入项目, THE 系统 SHALL 使用已保存的 Upstream URL 决定是否显示 Sync button
3. WHEN 运维从自改镜像切回官方 CE, THE 运维 SHALL 先运行 `scripts/cleanup-gitlab-rs-overlay.sh` 再改 compose `image:`

### Requirement 4

**User Story:** AS 运维, I want 该能力作为 gitlab-rs overlay, so that 官方 CE 与自改仍只改镜像名切换

#### Acceptance Criteria

1. THE 系统 SHALL 用 overlay initializer、页面脚本和已有项目数据保存 Upstream URL 与 Sync job 状态
2. THE 系统 SHALL 仅项目 Owner 会话可创建 Sync job
3. WHEN 清理 overlay 数据, THE 系统 SHALL 删除本功能写入的 Redis 键，并保留项目 Git 历史与导入产生的用户数据

## Locked Decisions

- Date: 2026-11-19
- Force sync: 上游同名分支与标签覆盖本地对应引用
- Permission: 仅 Owner 可见并可点击
- Private upstream: 复用导入时保存的凭据；缺失则失败提示；第一版不提供填 Token 表单

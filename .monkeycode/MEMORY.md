# 用户指令记忆

## 条目

推送分支规则
- Date: 2026-06-30
- Context: 用户指定代码推送分支策略
- Instructions:
  - GitHub (remote: github): 推送到 main 分支
  - 自建 GitLab (remote: origin): 推送到 master 分支

[生产环境部署]
 - Date: 2026-09-09
- Context: Discovered by Agent while diagnosing bak.toarujs.com dashboard projects load failure
- Category: Operations & Deployment
- Instructions:
  - SSH: `root@bak.toarujs.com:22`
  - 源码目录: `/dockerbuild/gitlab-rs`
  - 当前启动目录: `/dockerbuild/gitlab-rs-cs`
  - 容器: `gitlab-rs-cs`，镜像 `toarujs/gitlab-rs:latest`（compose：`/dockerbuild/gitlab-rs-cs/docker-compose.yaml`）
  - 对外 HTTPS: `https://bak.toarujs.com:9071`（SafeLine）；容器 HTTP: `9070->80`；SSH: `9078->22`
  - 原版实例已切 rs：目录 `/compose/gitlab`，容器 `gitlab`，镜像 `toarujs/gitlab-rs:latest`；HTTPS `:9061`；HTTP `9060->80`；SSH `9068->22`
  - `/compose/gitlab/.env` 提供 `GITLAB_HOSTNAME` `EXTERNAL_URL` `HTTP_PORT` `SSH_PORT` `DB_USER` `DB_PASSWORD` `ROOT_PASSWORD`，yaml 用这些变量写 `GITLAB_OMNIBUS_CONFIG`
  - 项目列表打不开时优先查 Gitaly：`docker exec gitlab-rs-cs gitlab-ctl status gitaly`，以及 `/var/opt/gitlab/git-data/repositories/+gitaly` 是否为 `git:git`
  - Gitaly hook 正确配置键：`gitlab_rails['internal_api_url'] = "http://127.0.0.1:80"`（Rust Workhorse TCP :80）
  - `entrypoint-server.sh` 在写入 `gitlab.rb` 后会追加该键，避免 `GITLAB_OMNIBUS_CONFIG` 覆盖后 reconfigure 回到死 socket `http+unix://%2Fvar%2Fopt%2Fgitlab%2Fgitlab-workhorse%2Fsockets%2Fsocket`

[GitHub 与 Gitee 分仓]
- Date: 2026-09-09
- Context: 用户说明两边不在相同仓库，要求把远程需要的代码拷到工作空间再推 GitHub
- Category: Workflow & Collaboration
- Instructions:
  - 本地 `/workspace` 的 `origin` 是 GitHub `toarujs/gitlab-rs`
  - 远程 `/dockerbuild/gitlab-rs` 的 `origin` 是 Gitee `toarujianshang/gitlab-rs`
  - 两边不要互相 `git pull`；从远程拷需要的源码到工作区，在 GitHub 仓提交并推送

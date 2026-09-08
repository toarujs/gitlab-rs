# 用户指令记忆

## 条目

推送分支规则
- Date: 2026-06-30
- Context: 用户指定代码推送分支策略
- Instructions:
  - GitHub (remote: github): 推送到 main 分支
  - 自建 GitLab (remote: origin): 推送到 master 分支

[生产环境部署]
- Date: 2026-09-08
- Context: Discovered by Agent while diagnosing bak.toarujs.com dashboard projects load failure
- Category: Operations & Deployment
- Instructions:
  - SSH: `root@bak.toarujs.com:22`
  - 源码目录: `/dockerbuild/gitlab-rs`
  - 当前启动目录: `/dockerbuild/gitlab-rs-cs`
  - 容器: `gitlab-rs-cs`，镜像 `toarujs/gitlab-rs:19.3.1`
  - 对外 HTTPS: `https://bak.toarujs.com:9071`（SafeLine）；容器 HTTP: `9070->80`；SSH: `9078->22`
  - 项目列表打不开时优先查 Gitaly：`docker exec gitlab-rs-cs gitlab-ctl status gitaly`，以及 `/var/opt/gitlab/git-data/repositories/+gitaly` 是否为 `git:git`
  - Gitaly/gitlab-shell 的 `gitlab_url` 默认指向已停掉的 Go Workhorse unix socket；写仓库/跑 hook 前要改成 `http://127.0.0.1:80`（Rust Workhorse），然后 `gitlab-ctl restart gitaly`
  - 该修改在 `gitlab-ctl reconfigure` 后会被 Omnibus 覆盖

# 用户指令记忆

## 条目

推送分支规则
- Date: 2026-06-30
- Context: 用户指定代码推送分支策略
- Instructions:
  - GitHub (remote: github): 推送到 main 分支
  - 自建 GitLab (remote: origin): 推送到 master 分支

[回复语言]
- Date: 2026-09-26
- Context: 用户要求后续用中文回复
- Instructions:
  - 思考过程和回复都用中文

[生产环境部署]
 - Date: 2026-09-09
- Context: Discovered by Agent while diagnosing bak.toarujs.com dashboard projects load failure
- Category: Operations & Deployment
- Instructions:
  - SSH: `root@bak.toarujs.com:22`
  - 源码目录: `/dockerbuild/gitlab-rs`
  - 当前启动目录: `/dockerbuild/gitlab-rs-cs`
  - 容器: `gitlab-rs-cs`，镜像钉 `toarujs/gitlab_rs_test:latest`（compose：`/dockerbuild/gitlab-rs-cs/docker-compose.yaml`）
  - 对外 HTTPS: `https://bak.toarujs.com:9071`（SafeLine）；容器 HTTP: `9070->80`；SSH: `9078->22`
  - 生产实例：目录 `/compose/gitlab`，容器 `gitlab`；HTTPS `:9061`；HTTP `9060->80`；SSH `9068->22`
  - 镜像标签约定：9061 用 `toarujs/gitlab-rs:<原版CE版本号>` 或 `toarujs/gitlab-rs:latest`（compose 钉 `19.3.1`）；9071 用独立仓库 `toarujs/gitlab_rs_test:latest`
  - 9061 与 9071 禁止用同一 compose 标签互相跟随。只热更新 9071；确认后再把同一镜像 ID 打成 `toarujs/gitlab-rs:19.3.1` 和 `:latest` 再更新 9061
   - 2026-09-11..09-15 9071/9061 overlay 演进链（均已被后续 CE 19.3.3 吸收取代，保留回退 tag）：9061 于 09-11 升 `be758bb7bb4e`、09-12 升 `8163dc3493f6`（回退 `toarujs/gitlab-rs:pre-unixfix`=`be758bb7bb4e`）；9071 web 依次 `a3c94b808119`(`pre-nls`)→`55afb416d764`(`pre-packskip`)→`8163dc3493f6`(`pre-unixfix`)→`4ca0eab60a99`(`pre-htmlcache`)→`b320e905228c`(`pre-longpoll`，CI 长轮询)；9071 runner `5c5c184cea30`(`pre-longpoll`=`ececc8658444`，`JOB_REQUEST_TIMEOUT=70s`)→`3b274386e582`(`pre-decodefix`，register null 解码修复)。回退：compose 钉对应 tag 后 `up -d --no-deps --force-recreate web`（runner 同理）
     - 9071 runner 构建流程：改 `/dockerbuild/gitlab-rs/runner/src` → `cd /dockerbuild/gitlab-rs/runner && /root/.cargo/bin/cargo build --release -j 16` → 复制 `target/release/gitlab-runner` 到 `/dockerbuild/gitlab-rs/overlay-build-runner/gitlab-runner` → `docker build -t toarujs/gitlab-rs-runner:latest /dockerbuild/gitlab-rs/overlay-build-runner` → `up -d --no-deps --force-recreate runner`
    - 9071 Redis 以 `/var/opt/gitlab/gitlab-rails/etc/resque.yml` 的 `production.url`（`redis://redis/`）为准。`/var/opt/gitlab/redis/redis.socket` 是 omnibus 残留，不能连
    - 热更新只 `docker compose up -d --no-deps --force-recreate web`，不动 db/redis/runner
  - 2026-09-11：9061 已加 instance runner。容器 `gitlab-runner`，镜像 `gitlab/gitlab-runner:latest`；docker executor，`network_mode=gitlab-bridge`，`url/clone_url=http://web`。启动用 `docker compose -f /compose/gitlab/docker-compose.yaml up -d --no-deps runner`
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

[9071 包上传与 LFS 加速]
- Date: 2026-09-17
- Context: Discovered by Agent while making package-manager uploads and Git LFS work on the 9071 canary
- Category: Operations & Deployment / Troubleshooting & Debugging
- Instructions:
  - 上传加速两种协议：`src/upload/request_body.rs`（原始 body 缓冲后改写成签名 form，maven/npm/conan/generic/ml_models/debian/rpm/rubygems/terraform/LFS 走这条）；`src/upload/accelerate.rs`（mime multipart，nuget/pypi/helm 走这条）
  - LFS 对象上传 `PUT <repo>.git/gitlab-lfs/objects/<64hex-oid>/<size>`（`application/octet-stream`）必须走 request_body 加速，否则 Rails `upload_finalize` 读不到 `params[:file]`，返回 422
  - `proxy_handler` 对 LFS 路径（`/info/lfs/` 与 `/gitlab-lfs/`）禁止路由到 Gitaly，固定走 Rails（对齐 Go workhorse 的 `git_lfs_objects` → railsBackend）
  - `accelerate_request_body` 末尾对 `proxy_handler` 的调用用 `Box::pin`：LFS 会从 `proxy_handler` 内部递归进来，否则 async future 无限大
  - LFS 上传 500 `Errno::EACCES ... shared/lfs-objects/tmp/work/...` 时，是 `lfs-objects/tmp/work` 属主为 root；Rails 以 git 运行，需 `chown -R git:git /var/opt/gitlab/gitlab-rails/shared/lfs-objects/tmp`（`./data` 是 bind mount，改一次即持久）
  - ml_models 上传接口只认 `Authorization: Bearer`；上传前要先经 MLflow API 建 registered-model 与 model-version，put 路径用 version id：`/api/v4/projects/:id/packages/ml_models/<model_version_id>/files/<name>`
  - nuget 上传认 `X-Nuget-Apikey` 或 HTTP Basic；pypi 用 `PRIVATE-TOKEN`；helm 仅 HTTP Basic（`oauth2:<PAT>`）
  - LFS E2E 脚本 payload 要每次带随机值，否则 oid 已存在时 batch 合理地不返回 upload action
  - 镜像回退点：`toarujs/gitlab_rs_test:pre-lfs`=`2a2859a91cf6`（multipart 后、LFS 前）；LFS 加速后 `latest`=`12eb38113cee`
  - 9071 未部署 container registry（compose 无端口、`gitlab.rb` 无配置、`/var/opt/gitlab/registry` 不存在）
  - 代码已推 GitHub `origin/260914-feat-ci-long-polling` @ `aa0f166`。9061 web 仍 `8163dc3493f6`，等用户确认后再打 `toarujs/gitlab-rs:19.3.1`/`:latest`
  - 2026-09-21：`MaximumSize==0` 按不限大小处理（`u64::MAX`）；`chown_upload_tmp` 覆盖 `TempPath` 父目录与同级 `tmp/work`。9071 web overlay `toarujs/gitlab_rs_test:latest`=`0271208ed246`（FROM `pre-maxsize`=`12eb38113cee`）。9061 仍为 `8163dc3493f6` + 官方 runner。回退：compose 钉 `pre-maxsize` 后 `up -d --no-deps --force-recreate web`
   - 2026-09-21：9071 web overlay `toarujs/gitlab_rs_test:latest`=`367f5830816e`（FROM `pre-maxsize`=`12eb38113cee`）。回退 tag `pre-quotfix`=`0271208ed246`（含 MaximumSize 修复）。管理员页 Workhorse 版本来自请求头 `Gitlab-Workhorse`.split('-').first，必须只发版本号（如 `0.1.0`），不能发 `gitlab-workhorse-rs/0.1.0`。二进制要同时支持 `-version` 与 `--version`，输出 `gitlab-workhorse <ver>`。Web IDE fallback 在 `application_settings.vscode_extension_marketplace` JSON 的 `single_origin_fallback_enabled`。CI 表是 `p_ci_pipelines` / `p_ci_builds`（无 `ci_pipelines`）。查询 postgres 用 `docker exec gitlab-rs-cs-db psql -U gitlab -d gitlabhq_production`。
   - 9071 验收走宿主机 `http://127.0.0.1:9070`（外网 HTTPS 9071 仍经 SafeLine）。LFS batch/object 用 HTTP Basic `oauth2:<PAT>`；`PRIVATE-TOKEN` 打 `.git/info/lfs` 会被 GitLab 当成 Git HTTP 要 Basic
  - 远程编译：`cd /dockerbuild/gitlab-rs && /root/.cargo/bin/cargo test --release --lib upload::request_body && cargo build --release -j 16`，再复制 `target/release/gitlab-workhorse-rs` 到 `/dockerbuild/gitlab-rs/overlay-build/`

[Agent 环境连 bak]
- Date: 2026-09-21
- Context: Discovered by Agent while connecting to homeserver bak.toarujs.com from the coding environment
- Category: Environment Configuration / Troubleshooting & Debugging
- Instructions:
  - SSH 密钥：`/workspace/.monkeycode-tmp-files/c91aed91-bak-toarujs-key-1.pem`，加 `-o IdentitiesOnly=yes`
  - 2026-09-21：本环境直连 `root@bak.toarujs.com:22` 已通（`27.189.146.168`）
  - 本环境 TCP/22 不稳定：GitHub:22 有时是真 SSH（`Permission denied (publickey)`），有时 `kex_exchange_identification`
  - `ssh.github.com:443` 能拿到真实 SSH banner
  - `bak.toarujs.com` DNS 已从 `27.189.147.114` 变为 `27.189.146.168`；两 IP 的 :22 都是 Connection established 后立刻被掐
  - 跳板 `8.155.1.108`（www.toarujs.com）:443 是真 HTTPS（Tengine）；:22 同样 kex 被掐，不能 `-J`
  - 无 HTTP_PROXY，无 corkscrew/ncat/socat/proxytunnel/cloudflared
  - `certs/` 含站点私钥，禁止 git add

[GitLab 19.3.1 安全补丁目标]
- Date: 2026-09-24
- Context: Discovered by Agent while checking public CVEs against 9071/9061 running versions
- Category: Operations & Deployment
- Instructions:
   - 9071 Rails `VERSION=19.3.3`（web `toarujs/gitlab_rs_test:latest`=`cae3cc5302f0`）；9061 Rails `VERSION=19.3.1`（`toarujs/gitlab-rs:19.3.1`=`8163dc3493f6`）
  - 官方 19.3.2（2026-09-10）修 CVE-2026-85706（CVSS 10.0，CISA KEV，commits API 未认证读文件）；官方 19.3.3 / 19.4.1（2026-09-23）修 CVE-2026-89078、CVE-2026-93577（认证用户 CI 正则 RCE，CVSS 9.9）
  - 9071 已升到 CE **19.3.3**；9061 仍 19.3.1。升 CE 含数据库 migration，单节点会停机
   - 对照公开公告做版本映射；不要在实例上复现这些 CVE
   - 2026-09-24：Workhorse 已重构 CVE 面（不升 CE）：`src/cve_guard.rs` 对 `POST/PUT/PATCH/DELETE` 的 `/api/v4/projects/:id/repository/commits|files`（含 `.json`/尾斜杠/百分号解码）要求 API 凭据并拒绝客户端 `file.path`/`metadata.path`；`forward_request_headers` 与 unix 回源丢掉客户端 send-data / x-sendfile / workhorse JWT 头；sendfile/X-Sendfile 只允许 `/var/opt/gitlab` 与 Rails `public/`；sendurl 解析失败即拒绝，并拦 IPv6 loopback/十进制 IP。Rails overlay：`docker/gitlab-rails-overrides/config/initializers/gitlab_rs_cve_guards.rb`（UploadedFile 路径收紧 + UntrustedRegexp 4KiB 上限）。CI 正则 RCE（89078/93577）的 Onigmo 引擎仍要 CE **19.3.3**。Workhorse 守卫随二进制 overlay 生效；Rails initializer 要进镜像 COPY 或拷进容器 `config/initializers/`。
   - 2026-09-26：9071 已 overlay CVE 守卫。web `toarujs/gitlab_rs_test:latest`=`715dc30d642d`（FROM `pre-cveguard`=`367f5830816e`，含 Workhorse 二进制 + Rails `gitlab_rs_cve_guards.rb`）。9061 仍为 `toarujs/gitlab-rs:19.3.1`=`8163dc3493f6`，未动。回退 9071：compose 钉 `pre-cveguard` 后 `up -d --no-deps --force-recreate web`。远程编译：`cd /dockerbuild/gitlab-rs && cargo test --lib -- cve_guard` 后 `cargo build --release -j 16`，二进制拷到 `/dockerbuild/gitlab-rs/overlay-build/`。无认证 POST `/api/v4/projects/1/repository/commits` 与 `commits.json` 由 Workhorse 直接 401（`text/plain`）。
   - 2026-09-28：9071 已吸收官方 CE **19.3.3**（`gitlab/gitlab-ce:19.3.3-ce.0`）。web `toarujs/gitlab_rs_test:latest`=`cae3cc5302f0`（同 tag `:19.3.3`）。回退 tag `pre-ce1933`=`715dc30d642d`。构建上下文 `/dockerbuild/gitlab-rs/ce1933-build/`。代码已推 GitHub `origin/260914-feat-ci-long-polling` @ `0aaa317`。9061 未动。确认后再把同一 ID 打成 `toarujs/gitlab-rs:19.3.3` 与 `:latest` 升 9061。回退 9071：compose 钉 `pre-ce1933` 后 `up -d --no-deps --force-recreate web`。
    - 2026-09-28：9071 Workhorse overlay 加速 `GET /repository/tree`、`/repository/commits`（`latest`=`99900f6a920b`）与流式 `GET /repository/archive`（`latest`=`221f121ecd8f`，回退 `pre-archive`=`99900f6a920b`），均 FROM `pre-reporest`=`cae3cc5302f0`（CE 19.3.3）；9061 仍 `8163dc3493f6`。改 `/dockerbuild/gitlab-rs/src/hotpath/repo.rs` 后 `cargo test --lib hotpath::repo` → `cargo build --release -j 16` → 二进制拷到 overlay-build。验收 `http://127.0.0.1:9070` + 对照头 `X-Gitlab-Rs-Skip-Accel`（archive 另用 Redis session `_gitlab_session`）：公开项目 2 tar.gz/zip/`sha=HEAD` 的 status/Content-Type/Content-Disposition/包体一致，私有 1 两边 404。未 git push。
    - 2026-09-29/10-01：9071 加速 LFS 对象流式 GET/PUT（`latest`=`7c945cfe4337`，回退 `pre-lfsstream`=`221f121ecd8f`）与 `GET /repository/commits/:sha`（回退 `pre-commitshow`；新增 Gitaly `FindCommits` 单条查询，含 stats/trailers/project_id，命中 pipeline 回源 Rails），均 FROM `pre-reporest`。远程 `cargo test --lib git_lfs`（4）/`hotpath::repo`（23）后 `cargo build --release -j 16`。验收 `http://127.0.0.1:9070`+Redis session：项目 13 LFS oid GET 200/sha256 对齐/`X-Sendfile streamed`、PUT 5368709121 字节 413；项目 2 `HEAD` commit 与 `X-Gitlab-Rs-Skip-Accel` 全字段一致（时间含 `.000`，`stats=false` 省略 stats）。9061 未动，未 git push。
    - 2026-10-04：9071 Workhorse overlay 给 files/tree/commits/archive/commit-show 加上匿名公开仓与 PAT 鉴权。web `toarujs/gitlab_rs_test:latest`=`f7c543a29a12`（FROM `pre-reporest`=`cae3cc5302f0`）。回退 tag `pre-guestauth`=`5a30d5a822a9`（commit-show 时间戳修复，仍要求 session）。JOB-TOKEN / deploy-token 回源 Rails。PG `projects.id` / `users.id` / `project_authorizations.*` / `personal_access_tokens.user_id` 均为 bigint，SQL 参数必须绑 i64。files 走与 repo 相同的 `gitlab.yml` unix Gitaly 与 `.git` relative path。验收 `http://127.0.0.1:9070`：公开项目 2 无 cookie 的 tree/commits/commit-show/files 与 `X-Gitlab-Rs-Skip-Accel` JSON 相等；私有项目 1 两边 404；非法 `glpat-` 401。`GET /api/v4/projects` 与 job trace 仍要 session。9061 未动，未 git push。
    - 2026-10-05：9071 Workhorse overlay 给 `GET /api/v4/projects` 接匿名/PAT；job trace 接 PAT/session，匿名直接 401（Rails `lib/api/ci/jobs.rb` `authenticate!`）。web `toarujs/gitlab_rs_test:latest`=`e08ca919e0ff`（FROM `pre-reporest`=`cae3cc5302f0`）。回退 tag `pre-listauth`=`f7c543a29a12`。验收 `http://127.0.0.1:9070`：无 cookie 项目列表 id `[38,13,9,5,4,2]` 与 skip-accel 相同；`membership=true` 空数组；非法 `glpat-` 401；公开/私有 job trace 匿名两边 401。9061 未动，未 git push。
    - 2026-10-08：9071 只改 `/dockerbuild/gitlab-rs-cs/docker-compose.yaml` 与 `config/gitlab.rb`，`puma["worker_processes"]` 8→4，镜像仍 `e08ca919e0ff`。容器 healthy，4 个 cluster worker，内存约 8GiB→3.5GiB。9061 仍 8 worker / ~7.5GiB。回退：两处改回 8 后 `cd /dockerbuild/gitlab-rs-cs && docker compose up -d --no-deps --force-recreate web`。
    - 2026-10-09：9071 Workhorse overlay 把匿名/`simple=true` 的 `GET /api/v4/projects` 补齐 BasicProjectDetails（20 字段，含 namespace/web_url/urls/topics）。登录且非 simple 的全量 Project entity 回源 Rails。web `toarujs/gitlab_rs_test:latest`=`143c0785a488`（FROM `pre-reporest`=`cae3cc5302f0`）。回退 tag `pre-listjson`=`e08ca919e0ff`。验收 `http://127.0.0.1:9070`：无 cookie 项目列表 JSON 与 skip-accel 全字段相等；tree/files 仍相等。19.3.3 新格式 PAT（`glpat-....01....`）本地 digest 对不上会 401，跳过加速仍 200。9061 未动，未 git push。
    - 2026-10-09：9071 Workhorse overlay 修 PAT digest（`Gitlab::CryptoHelper.sha256`，盐来自容器内 `secrets.yml` 的 `db_key_base[0..31]`）、tree `TREES_FIRST`、加速 `GET .../repository/files/:path/raw`。web `toarujs/gitlab_rs_test:latest`=`27f625e5291f`（FROM `pre-reporest`=`cae3cc5302f0`）。回退 tag `pre-rawpat`=`143c0785a488`。验收 `http://127.0.0.1:9070`：项目 13 `ref=master` tree JSON 与 skip-accel 相等（trees-first）；README raw 包体相等；新格式 PAT（含 `.`）simple 项目列表/tree/私有仓 200 且与 skip-accel 相等。`lfs=true` 的 raw 仍回源。9061 未动，未 git push。
    - 2026-10-09：9071 Workhorse overlay 加速匿名 `GET /api/v4/projects/:id`（20 字段，与列表 JSON 全等；登录全量实体回源）。web `toarujs/gitlab_rs_test:latest`=`f619b27e22a4`（FROM `pre-reporest`=`cae3cc5302f0`）。回退 tag `pre-projectshow`=`27f625e5291f`。验收 `http://127.0.0.1:9070`：项目 13 数字 id 与 `toaru%2Findextts2.5-byond` JSON 与 skip-accel 全等；私有项目 1 / 不存在 99999 两边 404；`statistics=true` 回源仍相等；tree 回归相等。9061 未动，未 git push。
    - 2026-10-10：9071 Workhorse overlay 加速 `GET /api/v4/projects/:id/repository/branches`。web `toarujs/gitlab_rs_test:latest`=`603e08275147`（FROM `pre-reporest`=`cae3cc5302f0`）。回退 tag `pre-branches`=`f619b27e22a4`。远程 `cargo test --lib -- hotpath::` 95 passed 后 `cargo build --release -j 16`。验收 `http://127.0.0.1:9070`：项目 13 匿名 branches JSON 与 skip-accel 全等；编码路径可用；私有 1 / 不存在 99999 两边 404；`search=` 与 `/branches/master` 仍回源；tree/commits/project show 回归相等。9061 未动，未 git push。
    - 2026-10-11：9071 Workhorse overlay 加速 branches `can_push`。web `toarujs/gitlab_rs_test:latest`=`8e8266186a53`（FROM `pre-reporest`=`cae3cc5302f0`）。回退 tag `pre-canpush`=`603e08275147`。远程 `cargo test --lib -- hotpath::` 96 passed。代码已推 GitHub `origin/260914-feat-ci-long-polling`。9061 未动。
    - 2026-10-14 修正 `can_push` 语义（推翻 2026-10-11 的“admin 不再直接 true”）：实测 Rails 对**非保护分支**下 admin（无项目成员行）返回 `can_push=true`，因为 API 走 `:push_code` 能力，admin 全局持有；**保护分支**仍按成员 access_level 判定（admin 无匹配 push 规则时为 false）。因此 `can_push_to_branch` 只对未保护分支套 admin=owner 覆盖，保护分支保持 membership 计算。
    - 2026-10-11/12：9071 Rails overlay 项目页「压缩历史」。入口：树页/文件页 `#js-repository-blob-header-app` 旁按钮；session 中间件 `/-/gitlab-rs/projects/:id/compress_history`（CSRF）是前端实际入口，Grape `POST /api/v4/projects/:id/repository/compress_history` 改后必须重启 Puma（`after_initialize` 才挂）。`mode=gc` 走 housekeeping `:eager`（Maintainer+，lease 24h，重复 409）；`mode=squash` 需 Owner + `confirm_phrase=path_with_namespace`，用 `gitaly git commit-tree` 建无父提交再 `write_ref`（幂等返回 201 `status=noop`）。JSON body + `X-CSRF-Token` 头可用，匿名 422（CSRF 先于登录）。JS 用 MutationObserver 常驻防 Vue 冲掉按钮；HAML `_compress_history_mount.html.haml`。`gitlab-ctl hup puma` 整进程重启，preload ~2 分钟期间 9070 返回 502。回退 tag `pre-compress`=`8e8266186a53`，`latest`=`68b0235b376a`。测试仓 id 148/149（丢弃）保留；临时 PAT id=43 已 revoke。
    - 2026-10-14 squash 破坏性实测通过：用丢弃仓 `root/squash-e2e-<ts>`（id 149）经 `git push` 造 4 个提交，`POST /api/v4/projects/:id/repository/compress_history` `mode=squash` + `confirm_phrase=path_with_namespace` 返回 201 `{"mode":"squash","status":"ok","sha":...,"old_sha":...,"branch":"master"}`；之后 4→1 个无父 root 提交，`README.md`/`file1-3.txt` 全部保留且内容不变，重新 `git clone` + `git fsck` 干净。两条丢弃仓（148、149）保留未删。
    - 2026-10-14：9071 web overlay 收敛 job trace 契约。`src/hotpath/trace.rs` 去掉自造的 `Range`/`offset` 语义，改为对齐上游 `byte_offset`（钳到 EOF）/`byte_limit`（1..512000，越界回源 400）；响应改 `content-type: text/plain`（去 charset）、`content-disposition: infile; filename="<id>.log"`，GET 带 content-length、HEAD 与上游一样不带。web `toarujs/gitlab_rs_test:latest`=`c046a6f0568a`（FROM `pre-reporest`=`cae3cc5302f0`）。回退 tag `pre-tracecontract`=`68b0235b376a`。回退：compose 钉 `pre-tracecontract` 后 `up -d --no-deps --force-recreate web`。同镜像已烘入 compress_history gc 审计事件修复（`manually_trigger_housekeeping`），overlay-build 里的 `gitlab_rs_compress_history.rb` 已是修复版（旧版 `gitlab_rs_compress_history_gc` 无 YML 定义会 500）。验收 `http://127.0.0.1:9070` 项目 14 job 19（PAT）：全量/`byte_offset`/`byte_limit`/`byte_offset+byte_limit`/EOF 空 200/`byte_limit=0/-5/512001`→400/`byte_offset=-1`→400/非法整数→400，accel 与 `X-Gitlab-Rs-Skip-Accel` 全部 status+body 相等；`Range` 与 legacy `offset` 两边都忽略（200 全量）。projects/project/tree/commits/branches/files raw/archive 回归相等。9061 未动，未 git push。
    - 9071 基于 Workhorse 热更新流程（Rust 进程是容器 PID 1，不能只重启 runit 服务）：改 `/workspace` 源码 → scp 到 `/dockerbuild/gitlab-rs` 同名路径（overlay 文件同步到 `/dockerbuild/gitlab-rs/overlay-build/`）→ `cargo test --release --lib <module>` 后 `cargo build --release -j 16` → 拷 `target/release/gitlab-workhorse-rs` 到 overlay-build → `docker tag toarujs/gitlab_rs_test:latest <rollback>` → `docker build -t toarujs/gitlab_rs_test:latest /dockerbuild/gitlab-rs/overlay-build` → `cd /dockerbuild/gitlab-rs-cs && docker compose up -d --no-deps --force-recreate web`（约 3 分钟到 healthy）。

[9071 commits/files 写 API 的 body-upload（已实现）]
- Date: 2026-10-14
- Category: Operations & Deployment / Troubleshooting & Debugging
- Instructions:
  - 已实现：`src/upload/request_body.rs` 把 `POST /repository/commits`、`POST/PUT /repository/files/:path` 接入 Workhorse request-body 上传（先跑 `cve_guard` 再缓冲，预授权 `<path>/authorize` 拿 TempPath，改写 `file.*` 表单/JWT 回源）；`main.rs` 给 `/repository/commits/:sha` 补 `.post()`。兼容 CVE guard 用 request-extension 标记 `InternalWorkhorseUpload`（仅内部 rewrite 放行，客户端原始 body 仍受 `guard_repository_write` 约束）。
  - 验收（丢弃仓 149，PAT）：commits POST 201、files POST 201 / PUT 200 / DELETE 204、1MiB 包体 raw 往返一致、未认证 401、客户端 `file.path` 注入 400、`git clone` + `git fsck` 干净。
  - 排障关键：commits POST 曾返回 Rails 500（HTML），根因是自有 Rails overlay `gitlab_rs_cve_guards.rb` 的 `ALLOWED_PREFIXES` 未含子路径 uploader 落盘目录 `/opt/gitlab/embedded/service/gitlab-rails/public/uploads/tmp`（`Repositories::CommitsUploader.workhorse_local_upload_path`），`UploadedFile.from_params` 抛 `ArgumentError`。up 目录白名单需覆盖该前缀。
  - overlay 部署坑：`/dockerbuild/gitlab-rs/overlay-build/Dockerfile` 必须为每个新增/改动的 Rails override 文件显式 `COPY` 到容器对应路径；仓库内 `docker/Dockerfile.*` 已有该 COPY，但 overlay-build 的 Dockerfile 是独立维护的，缺 COPY 会导致改了 overlay 文件却未进镜像（曾出现 overlay-build 里 `gitlab_rs_cve_guards.rb` 与容器内不一致）。
  - 丢弃仓/测试：公开项目 2,4,5,9,13,14,38；`root/squash-e2e-1791955079`（id 149）。直接 `POST /api/v4/projects/:id/repository/commits/authorize`（无 workhorse 头）gitlab-rs 故意回 502 `Refusing to leak workhorse JSON to client`，属预期防护，不是回归。
  - 镜像：本次 `latest`=`d68836991372`（含 guard 白名单修复 + can_push 修正 + commits/branches title/message 修正）；回退 tag `pre-canpush`=`10d57fc7974e`（guard 修复版）、`pre-bodyupload`=`c046a6f0568a`。回退：compose 钉 tag 后 `up -d --no-deps --force-recreate web`。
  - Read 加速契约修正：分支/提交列表的 commit `title` 必须取原始 message 的**首个物理行**（Gitaly `subject` 是 git `%s`，会把无空行的首段折行合并，导致带 inline `Co-authored-by` 时与 Rails 不一致）；`message` 必须为原始 message **逐字节原样**（含/不含结尾换行都要保留，Gitaly `body` 即完整原始 message，不要补 `\n`）。验收 accel 与 `X-Gitlab-Rs-Skip-Accel` 在 7 个项目 branches/commits 全字段相等。
  - 临时绕过（已不必要）：`git push`（smart HTTP，PAT `oauth2:<token>`）造提交/文件。

[2026-10-15 复核：trace 契约的路由边界与“是否已部署”判定]
- Date: 2026-10-15
- Category: Troubleshooting & Debugging / Operations & Deployment
- Instructions:
  - trace 加速只挂 **API 路由** `GET/HEAD /api/v4/projects/:id/jobs/:id/trace`（`main.rs` → `routes::artifacts::handle_job_trace` → `hotpath::trace`）。**Web 路由** `/-/jobs/:id/raw` 由 Rails `Projects::JobsController#raw` 直接处理，上游本就不读 `byte_offset`/`byte_limit`（`send_file`/`send_data` 返回全量）。复测这两个参数必须用 API 路由，否则永远看到全量日志，与是否部署无关。
  - 判定 9071 “是否已部署最新代码”的可靠做法（不要只看 `git push`）：① `docker inspect gitlab-rs-cs --format '{{.Image}}'` 应等于 `docker inspect toarujs/gitlab_rs_test:latest --format '{{.Id}}'`；② 容器内 `/opt/gitlab/embedded/bin/gitlab-workhorse` 的 md5 应等于 `/dockerbuild/gitlab-rs/overlay-build/gitlab-workhorse-rs`；③ 容器内 overlay 文件（`gitlab_rs_cve_guards.rb`、`gitlab_rs_compress_history.rb`、`app/views/projects/*`、`public/gitlab-rs-compress-history.js`）md5 应与本地仓库一致；④ 容器 `StartedAt` 应晚于 `latest` 镜像 `Created`。四项全对即为最新，无需重建。
  - 无预留凭据时造短时 PAT 做验收：`docker exec gitlab-rs-cs gitlab-rails runner "u=User.find_by_username('root'); t=u.personal_access_tokens.create!(name:'<name>', scopes:['api'], expires_at:1.day.from_now); puts t.token"`，取输出里的 `glpat...`；用完 `PersonalAccessToken.where(name:'<name>').each(&:revoke!)`。PAT 明文只用于本次 curl，勿写入仓库或回复。
  - compress_history 两条路由验收要点：`mode=gc` 走 `Repositories::HousekeepingService :eager`，正常返回 201 `{"mode":"gc","status":"scheduled"}`，重复调用 housekeeping lease 24h 内返回 409，缺 `mode` 返回 400。`mode=squash` 需 Owner + `confirm_phrase=path_with_namespace`，201 返回 `{mode,status,sha,old_sha,branch}`；重写后用 `git fetch` 复核 ref 移动、`rev-list --parents -n1` 应无父（root commit）、`HEAD^{tree}` 与旧一致、文件内容逐字节不变、`git fsck --strict` 干净。squash 后 `Repositories::HousekeepingService(:prune)` 只把旧提交变为 unreachable（`git fsck` 显示 dangling），物理删除受 Git 默认 ~2 周 prune 宽限期约束，不会立即消失，这是预期而非缺陷。
  - 部署指纹（防止“推了代码但没部署”再次浪费时间）：`build.rs` 把 git SHA 编译进二进制（优先读 `GITLAB_RS_GIT_SHA` 环境变量，其次本地 `git rev-parse HEAD`），`/version` 与 `/health` 的 `git_sha` 字段及启动日志 `Build git SHA:` 都会带出。**在 9071 构建时必须显式传** `GITLAB_RS_GIT_SHA=<commit> cargo build`，因为 `/dockerbuild/gitlab-rs/.git` 停留在旧 HEAD（源码是手动同步的），fallback 会嵌入错误的 SHA。核验：`curl -s http://127.0.0.1:9070/version`。
  - accel 与 `X-Gitlab-Rs-Skip-Accel` 的对照脚本固化为仓库内 `scripts/verify-accel-parity.sh`（`BASE`/`TOKEN`/`PROJECT_ID`/`JOB_ID` 环境变量；失败非零退出），覆盖 trace 契约与 project/tree/branches/commits 读加速。历史上两个真实缺陷（branches `can_push`、commits `title`/`message`）都是靠这个对照发现的，常规单测覆盖不到 Rails 契约，应纳入 CI 对着运行实例跑。

[2026-10-15 热路径连接复用：最大性能瓶颈]
- Date: 2026-10-15
- Category: Build Methods / Troubleshooting & Debugging / Operations & Deployment
- Instructions:
  - 实测定位：`docker logs gitlab-rs-cs` 解析 `path_template`/`duration_ms`/`puma_ms`，`/` 是每 60s 一次健康检查（监控噪声），真实用户瓶颈是 API。用 `X-Gitlab-Rs-Skip-Accel` 做 A/B 基准确认：**加速层普遍等于或慢于回源 Puma**（branches 163ms vs 79ms）。根因是每个加速请求都用 `tokio_postgres::connect` 新建 1~3 个 PG 连接（auth/project/ACL/protected_rules）、并用 `GitalyClient::connect` 每次重新拨号 1~3 个 gRPC Channel（`branches` 一次要 3+3 个连接），而 Puma 有常热连接池。
  - 修复：新增 `src/hotpath/pg.rs` 进程级 PG 连接池（`PgConn` RAII guard，`Deref` 到 `Client`，drop 时归还，稳定空闲连接上限 16，`is_closed()` 剔除坏连接）；各模块 `pg_client` 改为调用 `pg::get`。`src/gitaly/mod.rs` 增加按 address 缓存的 `Channel`（`channel_cache`），`GitalyClient::connect` 复用（tonic Channel 多路复用且自动重连；不要用会序列化的 `GitalyPool`）。改动仅连接生命周期，不触碰序列化，故响应内容不变。对照脚本相应改为：JSON 端点解析后比较，trace 仍逐字节 `cmp`（两者语义相等但键序不同，旧 `cmp` 会假失败）。
  - 效果（9071 实测）：`trace byte_limit=120` 126ms→12ms、`tree` 116→24ms、`branches` 163→39ms、`commits` 108→14ms、`files raw` 93→24ms；`project show` 不变（`/api/v4/projects/:id` 本就是设计上「full project entity not accelerated」回源）。压 150 请求后 `pg_stat_activity` 总数稳定（54→54），无连接泄漏。本地 `cargo check --release` 通过、`cargo test --release` 339 passed。
  - 2026-10-16 会话鉴权缺陷：登录用户所有 API 请求日志 `reason: session decode failed` 全量回源（tree 匿名 21ms vs 会话 189ms）。根因：Rack `SessionId#private_id = "2::"+SHA256.hexdigest(public_id)`（rack-2.2.23），Redis 键 `session:gitlab:2::<64hex>`；旧代码拿原始 cookie 值查键永不命中。修复 `src/hotpath/session.rs::redis_session_keys` 派生 sha256 键并保留 legacy 键兜底，`src/redis/mod.rs` 增 `RedisClient::shared(url)` 进程级复用（`ConnectionManager` 多路复用+自动重连，`connect()` 幂等），`auth.rs` 改调。实测会话 tree 189→21ms（`hotpath_result_total` hit 18 / fallback 3），对照脚本 `ALL_OK`，本地 340 passed。
  - 镜像与指纹：`latest`=`3886b5004c46`（`git_sha`=`b4730519...`，会话键修复），回退 tag `pre-sesskey`=`8fc0aaf46f66`（池化版 `d25f06e`）、`pre-pooling`=`d68836991372`。回退：compose 钉对应 tag 后 `up -d --no-deps --force-recreate web`。金丝雀 compose 实际在 `/dockerbuild/gitlab-rs-cs/docker-compose.yaml`（image `toarujs/gitlab_rs_test:latest`），源码构建在 `/dockerbuild/gitlab-rs`；`GITLAB_RS_GIT_SHA=<commit> cargo build --release -j 48` 后 `docker build -t toarujs/gitlab_rs_test:latest /dockerbuild/gitlab-rs/overlay-build` 再重建 `web`。`/api/graphql` 非加速域（会话 113ms vs 匿名 45ms，开销在 Puma session/execution，Workhorse 层无法复刻）。
  - CI 长轮询 `GET/POST /api/v4/jobs/request`（`src/routes/ci_long_polling.rs`，默认时长 50s）实测四态（用 `Ci::Runner.first.token` 造 body `{"token":..,"last_update":..}`）：① `last_update` 与 Redis `runner:build_queue:<token>` 不符 → `AlreadyChanged` → 回源 Rails（204 + `x-runtime`）；② 相符且无变更 → 前端驻留整段时长后由 Workhorse 直接返回 204（无 `x-runtime`）；③ 相符期间 `SET` 新值并 `PUBLISH workhorse:notifications:<key>` → 立即 `SeenChange` → 回源；④ 非法 token → 403。Redis 容器 `gitlab-rs-cs-redis`。
  - 2026-10-16 `/api/v4/user` 加速：新增 `src/hotpath/user.rs` + `src/routes/user.rs`，`main.rs` 挂 `GET /api/v4/user`（`PUT`/`DELETE` 仍回源）。entity 按查看者分叉：admin 用 `Entities::UserWithAdmin`（42 字段，多 `is_admin`/`note`/`namespace_id`/`created_by`），非 admin 用 `UserPublic`（38 字段），用 `#[serde(flatten, skip_serializing_if)]` 实现。回退条件（宁可回源不猜）：上传头像、`user_preferences.timezone` 非空（`local_time` 依赖墙上时钟）、bot（`user_type != 0`，`redacted_name` 依赖查看者权限）、`locked_at` 非空、`commit_email == '_private'`、forti/duo OTP 开启。`can_create_group`=`users.can_create_group && allow_user_to_create_group_and_project?`（`allow_project_creation_for_guest_and_below || admin || highest_role > GUEST`）；gravatar 走 `GravatarService` 默认 size 40×scale 2 → `?s=80&d=identicon`（与 projects 的 `gravatar_url()` 同值）。验收：root（admin）与 e2e-maint（非 admin）accel 与 `X-Gitlab-Rs-Skip-Accel` JSON 全等；匿名 401；toaru（有上传头像）两边全等（走回退）；`?foo=bar` 回源、尾斜杠两边 404。实测 root 8.9ms vs 回源 150.8ms（~17×）。`cargo test --release` 344 passed；`scripts/verify-accel-parity.sh` 增 `current user (admin)`，`ALL_OK`。镜像 `latest`=`adab5d6e595f`（`git_sha`=`61f168a4...`），回退 tag `pre-useraccel`=`3886b5004c46`。临时 PAT id 56/57/58 已 revoke。

[2026-10-16 容器 PID 1 不回收子进程导致 PID 耗尽]
- Date: 2026-10-16
- Context: Discovered by Agent while diagnosing the 9061 production `gitlab` container (`sh: Cannot fork` / sshd `fork: Resource temporarily unavailable`)
- Category: Troubleshooting & Debugging / Operations & Deployment
- Instructions:
  - 根因：`docker/entrypoint-server.sh`（= 镜像 `/assets/init-container`）结尾 `exec ... gitlab-workhorse`，Rust workhorse 成为容器 **PID 1**。容器内 `runsvdir` 监督 sshd，sshd 每次会话 re-exec 后中间父进程退出，会话进程被 orphan 并 reparent 到 PID 1；PID 1 必须回收，而修改前 Rust workhorse 无 `waitpid`/`SIGCHLD`/subreaper，僵尸永久累积。宿主曾达 85862 个 sshd 僵尸（父=workhorse），容器 pids cgroup `pids.current==pids.max==86308` 打满，任何 fork 失败，约 5 个/分钟泄漏（12 天累积）。
  - 修复：`Cargo.toml` 加 `libc`；`src/main.rs` 新增 `spawn_child_reaper()`——设 `PR_SET_CHILD_SUBREAPER` 并起后台线程循环 `waitpid(-1, WNOHANG)`（空转退避 1s）。workhorse 自身不 fork，全局回收安全。线程名 `child-reaper`，可经 `/proc/<pid>/task/*/comm` 核验。
  - 验证（9071 金丝雀）：PID 1 = `gitlab-workhors`；制造 150 个 orphan 到 PID 1 的 `( sleep & )`，t+2s 确认 ppid=PID1，t+12s 僵尸=0；40 次真实 SSH 连接后僵尸=0；`/health`=200、匿名 `GET /api/v4/user`=`{"message":"401 Unauthorized"}`。镜像 `toarujs/gitlab_rs_test:latest`=`ea8d118dbb35`（二进制 md5 `de8791407727e70322e8a7a7cbe76d2b`），回退 tag `pre-pidreap`=`adab5d6e595f`。重启容器可临时清除已有僵尸，但根治必须靠此修复。
  - 部署：同步 `Cargo.toml`/`Cargo.lock`/`src/main.rs` 到 `/dockerbuild/gitlab-rs` → `GITLAB_RS_GIT_SHA=<commit> cargo build --release -j 48` → 二进制拷到 `/dockerbuild/gitlab-rs/overlay-build/` → `docker tag toarujs/gitlab_rs_test:latest pre-pidreap` → `docker build -t toarujs/gitlab_rs_test:latest /dockerbuild/gitlab-rs/overlay-build` → `cd /dockerbuild/gitlab-rs-cs && docker compose up -d --no-deps web`。
  - 2026-10-16 9061 生产应急 + 最小变更上线：泄漏速率实测约 43 个/分钟，`docker restart gitlab` 只能临时清零（20 分钟内又涨回 4xx）。生产 `toarujs/gitlab-rs:19.3.1` 的 tag 早已被清理，运行中镜像 ID `8163dc3493f6`（现仅挂 `toarujs/gitlab_rs_test:pre-htmlcache`）；compose 却仍钉 `toarujs/gitlab-rs:19.3.1`，直接 `up` 会找不到镜像。最小变更=以运行镜像为基座只换二进制（不动 Rails 层、不升 CE）：`docker tag 8163dc3493f6 toarujs/gitlab-rs:pre-pidreap`（回退）→ `/dockerbuild/gitlab-rs/overlay-build-prod/Dockerfile` `FROM toarujs/gitlab-rs:pre-pidreap` + COPY 新二进制 → `docker build -t toarujs/gitlab-rs:19.3.1 -t toarujs/gitlab-rs:latest` → `cd /compose/gitlab && docker compose up -d --no-deps web`。**构建必须显式传 `GITLAB_RS_GIT_SHA=<40位commit>`**（`/dockerbuild/gitlab-rs/.git` HEAD 是旧的 `810e8fa`），否则 `/version` 指纹错误。上线后：PID 1=`gitlab-workhors`、`child-reaper` 线程在、`/version` git_sha=`29f8ce79...`、二进制 md5 `7371d355b14f436612d091abaf553c5b`、Rails VERSION 仍 19.3.1；120 orphan 与 50 次真实 SSH 会话后僵尸=0；`BASE=http://127.0.0.1:9060 scripts/verify-accel-parity.sh`（项目 6/job 12）`ALL_OK`。新 `latest`=`54b3163e128d`。注意：此二进制含 09-14 以来全部 Workhorse 侧功能（长轮询/trace/连接池/会话键/user 加速等），生产 Rails 层仍是 19.3.1（缺 Rails 侧 overlay，如 compress_history/cve_guards initializer）。
  - 2026-10-17 路由透明性修复（commit `7d19e5d`）：Axum 对已注册路径只匹配显式方法，未注册方法直接 405，**不会落到 `/api/*path` catch-all**。实测 `POST /api/v4/projects/:id/repository/branches` 经 workhorse 405、直连 Puma 201，已影响生产。修复：给 repository tree/branches/archive* 与 LFS objects 的 `get()`/`put()` 方法路由加 `.fallback(proxy::proxy_handler)`，让未注册方法交由 Rails 判定；新增同类加速路由时必须检查是否同样需要 fallback。金丝雀完整验证 branches/files/commits 写矩阵全通过；生产用 `POST branches` 非法 ref=400、有效 ref=201、DELETE=204 验证，`scripts/verify-accel-parity.sh` 项目 6/job 12 `ALL_OK`。
  - 2026-10-17 部署指纹：二进制 md5 `20e1a3c2ba42c748354297a01f65dbf3`、`git_sha=7d19e5dd299080ab46b80f350fef46ee3c18063b`。金丝雀 `latest`=`1c191b465516`（回退 `pre-methodfallback`=`dabe948cb1d7`）；生产 `19.3.1`/`latest`=`d99de44f12e9`（回退 `pre-methodfallback`=`54b3163e128d`）。两端 PID 1 均为 workhorse、`child-reaper` 线程在、9 个 runit 服务 running。
  - 2026-10-17 产物下载 500（`send-data:artifacts-entry:`）两个缺陷：① Rails `Gitlab::Workhorse.send_artifacts_entry` 用 `Base64.encode64` 编码 `Entry`（每 60 字符换行、末尾带 `\n`），Go 的 base64 解码会忽略换行，Rust `STANDARD.decode` 不会，旧代码甚至把 base64 串直接当 zip entry 名 → 一律 404/500。修复 `src/senddata/artifacts_entry.rs`：先剔除所有空白再 base64 解码为 UTF-8 路径（含换行与长路径单测）。② `src/senddata/mod.rs::intercept_send_data` 把注入器返回的任意 `StatusCode` 都改写成 500；改为透传（500 时仍回 `senddata injection failed` 文案），使缺失条目返回 404、与 Go workhorse 一致。验收（9070/9060 均通过）：job 19 `artifacts/tree`=200、`artifacts/out.txt`=200 内容 38B 正确、缺失条目=404、`artifacts` zip=200 238B；job 39 嵌套与 30KB 条目 200 且字节一致；`scripts/verify-accel-parity.sh` 两端 `ALL_OK`。
  - 2026-10-17 产物 500 的数据侧：job 19（项目 14）archive 存在但 `Ci::JobArtifact file_type=metadata` 缺失，`build.rb artifacts_metadata_entry` 触发 `NoMethodError: private method 'open' called for nil`。临时 PAT 经 JOB-TOKEN 向运行中 job 39 上传 archive+metadata 均 201 且成对入库，证明当前上传链路不丢 metadata；存量用 `/tmp/repair_artifact_metadata.rb` 读 zip entry 生成 gzip metadata 修复。生产 9061 `archives=0 metadata=0`，无产物数据可复现。
  - 2026-10-17 产物修复部署指纹：二进制 md5 `19b0188993d43949edd95b9f0ea49b39`、`git_sha=2f25337ff253715e375d648f5c65600b5c676b74`（commit `12cf1a2` base64 + `2f25337` 状态码）。金丝雀 `latest`=`0bead555116d`（回退 `pre-artentry`=`1c191b465516`、`pre-artentry-status`=`b6ade0ab0bde`）；生产 `19.3.1`/`latest`=`3db022cf3211`（回退 `pre-artentry`=`d99de44f12e9`、`pre-artentry-status`=`a57b93651289`）。两端 PID 1 = workhorse、9 服务 running、僵尸 0。

[2026-10-17 zh-CN 硬编码 CI/校验文案修复]
- Date: 2026-10-17
- Category: Troubleshooting & Debugging / Operations & Deployment
- Instructions:
  - 漏翻根因：上游把 CI 失败文案写死在 Ruby 类里（`CommitStatusPresenter::CALLOUT_FAILURE_MESSAGES`、`Ci::PipelinePresenter#failure_reasons`、`Gitlab::Ci::Status::Build::Failed::REASONS`、`Ci::HasCompletionReason#rules_failure_message`、`PlayManual#action_title`），不经过 gettext；校验器与 `Gitlab::Regex`/`Gitlab::PathRegex` 的 `errors.add`/`message:` 同样是纯英文。
  - 修复方式：运行时 initializer（`docker/gitlab-rails-overrides{,-server}/config/initializers/gitlab_rs_ci_i18n.rb`）用 `config.to_prepare` 给上述类 prepend 模块，用 `s_()` 翻译 `super` 结果；另给 `ActiveModel::Error#message` prepend，统一翻译 `errors.add` 的纯字符串与 `message:`，并对已插值的 `must be at least N bits`、`type is forbidden. Must be ...` 做正则回填，对 `:invalid` 修复 Rails 取到 activerecord 英文兜底的问题。翻译真源是 `locale/zh_CN/{ci_messages,validation_messages}.po.fragment`，用 `scripts/i18n/merge_po.py` 按 msgid 合并进 3 份 `gitlab.po`。
  - 部署：i18n 只改 Rails 层，不重建二进制。以 `toarujs/gitlab-rs:pre-i18n` 为基座做 overlay（COPY initializer + `locale/zh_CN/gitlab.po`），`docker build` 后 `docker compose up -d --no-deps web`，约 200s healthy。
  - 验收：9061 `/-/jobs/1.json` 的 `callout_message`=中文、`status.tooltip`=`失败 - (作业卡住或超时)`、`status.text`=`已失败`；en/ja 回退仍英文；Group/Project/User 表单路径与名称校验、CI 变量 key/掩码校验均中文。
  - 指纹：po md5 `9bcd196a6f3141b19ccf1a80e0305684`；生产 `19.3.1`/`latest`=`d35e89d43311`（回退 `pre-i18n`=`3db022cf3211`），金丝雀 `latest`=`0580045286e6`。残留：模型属性名若 po 未覆盖仍可能英文（如 `Fingerprint sha256`），`ServiceResponse.error(message:)` 一类 API 错误文案不在本次范围。

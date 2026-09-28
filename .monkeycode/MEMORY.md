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
  - 2026-09-11：9061 已升到与 9071 相同内容 `be758bb7bb4e`（含 Git smart HTTP）。回退点仍是本地 tag `toarujs/gitlab-rs:pre-githttp`（`4164214b1d8c`）
   - 2026-09-11：9071 金丝雀 `toarujs/gitlab_rs_test:latest`=`a3c94b808119`（Web IDE `nls.messages.js` 按 cookie `preferred_language` 翻译）。9061 仍为 `be758bb7bb4e`。Hub 已无 `pre-githttp` tag，overlay 用本地 `toarujs/gitlab_rs_test:pre-nls`=`be758bb7bb4e`
   - 2026-09-11：9071 web 已 overlay pack 跳过二次压缩：`toarujs/gitlab_rs_test:latest`=`55afb416d764`（FROM `pre-packskip`=`a3c94b808119`）。9061 仍为 `be758bb7bb4e` + 官方 runner。回退：compose 钉 `pre-packskip` 后 `up -d --no-deps --force-recreate web`
   - 2026-09-12：9071 web overlay unix 代理超时/禁连接复用/不缓存 `/api/`：`toarujs/gitlab_rs_test:latest`=`8163dc3493f6`（FROM `pre-unixfix`=`55afb416d764`）。9061 仍为 `be758bb7bb4e` + 官方 runner。回退：compose 钉 `pre-unixfix` 后 `up -d --no-deps --force-recreate web`
   - 2026-09-12：9061 web 已升到同一 ID `toarujs/gitlab-rs:19.3.1`=`8163dc3493f6`；回退 tag `toarujs/gitlab-rs:pre-unixfix`=`be758bb7bb4e`。runner 仍官方 `gitlab/gitlab-runner:latest`
    - 2026-09-13：9071 web overlay 只缓存静态资源（登录 HTML / nls.messages.js / `Cache-Control: private` 不进缓存）：`toarujs/gitlab_rs_test:latest`=`4ca0eab60a99`（FROM `pre-htmlcache`=`8163dc3493f6`）。prometheus/alertmanager 数据目录属主 `gitlab-prometheus`。回退：compose 钉 `pre-htmlcache` 后 `up -d --no-deps --force-recreate web`
     - 2026-09-14：9071 CI 长轮询 overlay：web `toarujs/gitlab_rs_test:latest`=`b320e905228c`（FROM `pre-longpoll`=`4ca0eab60a99`）；runner `toarujs/gitlab-rs-runner:latest`=`5c5c184cea30`（FROM `pre-longpoll`=`ececc8658444`，`JOB_REQUEST_TIMEOUT=70s`）。9061 仍为 `8163dc3493f6` + 官方 runner。回退 web：compose 钉 `pre-longpoll` 后 `up -d --no-deps web`；回退 runner：钉 `toarujs/gitlab-rs-runner:pre-longpoll` 后 `up -d --no-deps --force-recreate runner`
     - 2026-09-15：9071 runner 修复 register payload 显式 null 解码失败（`job request failed error=decode job`）：`toarujs/gitlab-rs-runner:latest`=`3b274386e582`（FROM `pre-decodefix`=`5c5c184cea30`）。实测 pipeline `created→pending→running→success` 跑通。回退 runner：钉 `toarujs/gitlab-rs-runner:pre-decodefix` 后 `up -d --no-deps --force-recreate runner`
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
    - 2026-09-28：9071 已吸收官方 CE **19.3.3**（`gitlab/gitlab-ce:19.3.3-ce.0`）。web `toarujs/gitlab_rs_test:latest`=`cae3cc5302f0`（同 tag `:19.3.3`）。回退 tag `pre-ce1933`=`715dc30d642d`。构建上下文 `/dockerbuild/gitlab-rs/ce1933-build/`。9061 未动。确认后再把同一 ID 打成 `toarujs/gitlab-rs:19.3.3` 与 `:latest` 升 9061。回退 9071：compose 钉 `pre-ce1933` 后 `up -d --no-deps --force-recreate web`。

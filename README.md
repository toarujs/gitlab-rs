# gitlab-rs

GitLab CE + Rust Workhorse 替换方案。用 Rust 重写的 HTTP 前端代理替代 Go 版 gitlab-workhorse 和 nginx，单一二进制直接处理 HTTP 流量，通过 unix socket 转发到 Puma。

## 官方版本

对应官方 GitLab CE **19.3.1**（Docker 基础镜像 `gitlab/gitlab-ce:19.3.1-ce.0`，2026-08-26 安全补丁）。

| 项 | 值 |
|----|----|
| 官方 CE | 19.3.1 |
| 镜像 tag | `19.3.1-ce.0` |
| 上次 rs 打包 | 2026-07-01 |
| 本次对齐 | 2026-09-08 |

已关闭官方 Version Check / Usage Ping：实例不会请求 `version.gitlab.com`，也不会显示官方更新通知。

## 架构

```
Internet / WAF
    │
    ▼
┌─────────────────────────┐
│  Rust Workhorse (PID 1)  │  :80
│  - 反向代理              │
│  - Gitaly sidechannel    │
│  - 缓存 / 限流           │
│  - HTML 注入             │
└───────────┬─────────────┘
            │ unix socket
            ▼
┌─────────────────────────┐
│  Puma (Rails)            │  48 workers
│  - GitLab CE            │
│  - 仅监听 unix socket   │
└─────────────────────────┘
            │
    ┌───────┴───────┐
    ▼               ▼
┌────────┐    ┌──────────┐
│  PG 17  │    │ Redis 7  │
└────────┘    └──────────┘
```

## 目录结构

```
/
├── src/                    # Rust workhorse 源码
├── docker/
│   ├── Dockerfile-server   # 手动构建 —— 需预先 cargo build
│   ├── Dockerfile-oneclick # 一键构建 —— git clone + cargo build 全自动
│   ├── entrypoint-server.sh
│   └── gitlab-rails-overrides/
├── docker-compose.yaml     # 部署编排
├── .env.example            # 环境变量模板
├── Cargo.toml
└── Cargo.lock
```

## 构建

### 一键构建（推荐）

无需本地 Rust 环境，Docker 内完成克隆、编译、打包：

```bash
# 公开仓库
docker build -f docker/Dockerfile-oneclick -t toarujs/gitlab-rs:latest .

# 私有仓库
docker build -f docker/Dockerfile-oneclick \
  --build-arg REPO_URL=https://gitlab.example.com/user/repo.git \
  --build-arg GIT_USERNAME=oauth2 \
  --build-arg GIT_PASSWORD=glpat-xxx \
  --build-arg REPO_BRANCH=main \
  -t toarujs/gitlab-rs:latest .
```

| ARG | 默认值 | 说明 |
|-----|--------|------|
| `REPO_URL` | `https://github.com/toarujs/gitlab-rs.git` | 源码仓库 |
| `REPO_BRANCH` | `main` | 分支名 |
| `FALLBACK_REPO_URL` | `https://bak.toarujs.com:9061/toaru/gitlab-rust.git` | GitHub 不可达时自动回退 |
| `GIT_USERNAME` | (空) | 私有仓库用户名 |
| `GIT_PASSWORD` | (空) | 私有仓库密码/Token |

### 手动构建

适用于已有本地 Rust 工具链的场景：

```bash
cargo build --release
cp target/release/gitlab-workhorse-rs docker/
docker build -f docker/Dockerfile-server -t toarujs/gitlab-rs:latest docker/
```

## 部署

### 新部署

```bash
# 提取 compose 模板（一键构建镜像已内置，可选）
docker run --rm toarujs/gitlab-rs:latest cat /assets/docker-compose.yaml > docker-compose.yaml
docker run --rm toarujs/gitlab-rs:latest cat /assets/.env.example > .env

# 编辑 .env 填写实际值
vim .env

# 创建数据目录
mkdir -p config data logs postgres redis

# 启动
docker compose up -d
```

等待 2-3 分钟（`gitlab-ctl reconfigure` + Puma 预加载），访问 `http://<host>:<HTTP_PORT>` 看到登录页即部署成功。

### .env 配置

```bash
GITLAB_HOSTNAME=bak.toarujs.com
EXTERNAL_URL=https://bak.toarujs.com:9071
HTTP_PORT=9070
SSH_PORT=9022
DB_USER=gitlab
DB_PASSWORD=<your-password>
ROOT_PASSWORD=<root-password>
```

### 容器端口

| 端口 | 用途 |
|------|------|
| `HTTP_PORT` | HTTP（Rust workhorse 直接处理） |
| `SSH_PORT` | SSH（git clone） |

## 从官方 CE 移植

适用：原版已经是 `web + postgres + redis`，数据目录为 `config/` `data/` `logs/` `postgres/` `redis/`。把官方 yaml 换成 rs yaml，沿用同一套数据卷。

官方一体机（Postgres 打在 `data/` 里）先拆库，不能只换 yaml。

### 移植前记下这 5 项

从原版 `docker-compose.yaml` 抄下来，写进新 `.env`，端口和 `external_url` 必须和 WAF/反代一致：

```bash
GITLAB_HOSTNAME=<原 hostname>
EXTERNAL_URL=<原 external_url，含 https 和端口>
HTTP_PORT=<原 HTTP 宿主机端口>
SSH_PORT=<原 SSH 宿主机端口>
DB_USER=<原 db_username>
DB_PASSWORD=<原 db_password>
ROOT_PASSWORD=<任意，已有实例不会改 root 密码>
```

镜像用本机已有的 `toarujs/gitlab-rs:19.3.1`（或先按上文构建）。yaml 里的 `image:` 与这个 tag 对齐。

### 原地切换（推荐）

在原版 compose 目录执行：

```bash
cp docker-compose.yaml docker-compose.yaml.ce.bak
cp config/gitlab.rb config/gitlab.rb.ce.bak

# 放入 rs 的 compose，再按上一步改 .env
cp /path/to/gitlab-rs/docker-compose.yaml ./docker-compose.yaml

# image tag 改成已构建的 rs 镜像
# 例如：image: toarujs/gitlab-rs:19.3.1

docker compose up -d
```

rs yaml 会关掉容器内 nginx 和 Go workhorse，由 Rust workhorse 听 `:80`。entrypoint 会用 `GITLAB_OMNIBUS_CONFIG` 覆盖 `gitlab.rb`；`gitlab-secrets.json` 仍在 `config/`，会话和 CSRF 密钥保留。

等 `gitlab-ctl reconfigure` 结束（约 2-3 分钟），打开原来的项目页，确认列表能出来。

entrypoint 会在 Gitaly 启动前检查 `/var/opt/gitlab/git-data`：`+gitaly` / `@hashed` 不是 `git` 时自动 `chown -R git:git`。用 root 拷过数据不用再手敲 chown。

Gitaly 仍起不来、GraphQL 500、页面提示「您的项目无法加载」时：

```bash
docker compose exec web gitlab-ctl status gitaly
docker compose exec web ls -ld /var/opt/gitlab/git-data/repositories/+gitaly
```

`+gitaly` 必须是 `git git`。

### 复制到新目录

```bash
mkdir -p /new-deploy
cp -a /old-deploy/config /old-deploy/data /old-deploy/logs /old-deploy/postgres /old-deploy/redis /new-deploy/
cp /path/to/gitlab-rs/docker-compose.yaml /new-deploy/

# 在 /new-deploy 写 .env，端口不要和旧实例冲突
cd /new-deploy
docker compose up -d
```

`cp -a` 若由 root 执行，`+gitaly` 会变成 `root:root`。entrypoint 启动时会自动改回 `git:git`。

### 切过去之后 yaml 里必须有

```ruby
nginx["enable"] = false
gitlab_workhorse["enable"] = false
puma["socket"] = "/var/opt/gitlab/gitlab-rails/sockets/gitlab.socket"
postgresql["enable"] = false
redis["enable"] = false
```

只改 `image:`、其余仍用官方 nginx 配置时，rs 镜像里没有 nginx 二进制，reconfigure 会失败。

## 数据卷

所有数据使用 bind mount，全部在 compose 所在目录下：

| 目录 | 挂载路径 | 内容 |
|------|----------|------|
| `./config/` | `/etc/gitlab` | gitlab.rb, gitlab-secrets.json, SSH 密钥 |
| `./data/` | `/var/opt/gitlab` | 仓库、上传、构建产物 |
| `./logs/` | `/var/log/gitlab` | 日志 |
| `./postgres/` | `/var/lib/postgresql/data` | 数据库 |
| `./redis/` | `/data` | 缓存/会话 |

## 术语说明

| 名称 | 全称 | 说明 |
|------|------|------|
| CE | Community Edition | 社区版 |
| EE | Enterprise Edition | 企业版 |
| Puma | — | GitLab 使用的 Ruby 应用服务器 |
| Rails | Ruby on Rails | GitLab 后端 Web 框架 |
| Omnibus | — | GitLab 的一体化打包方案 |
| Sidekiq | — | GitLab 的后台任务处理器 |
| Gitaly | — | Git 仓库存储服务 |
| LFS | Large File Storage | 大文件存储 |
| WAF | Web Application Firewall | Web 应用防火墙，如 SafeLine |
| snowplow | — | 用户行为分析埋点系统 |
| CSRF | Cross-Site Request Forgery | 跨站请求伪造 |
| PWA | Progressive Web App | 渐进式 Web 应用 |

## 许可证

继承官方 GitLab CE 许可证（MIT Expat，Copyright (c) 2011-present GitLab Inc.）。完整文本见 `LICENSE`。

# gitlab-rs

GitLab CE + Rust Workhorse 替换方案。用 Rust 重写的 HTTP 前端代理替代 Go 版 gitlab-workhorse 和 nginx，单一二进制直接处理 HTTP 流量，通过 unix socket 转发到 Puma。

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

### 数据迁移

将旧 GitLab 部署的数据目录复制到 compose 所在目录即可：

```bash
# 旧数据路径示例
cp -a /old-deploy/config/ ./config/
cp -a /old-deploy/data/   ./data/
cp -a /old-deploy/logs/   ./logs/
cp -a /old-deploy/postgres/ ./postgres/
cp -a /old-deploy/redis/  ./redis/

docker compose up -d
```

数据迁移后 entrypoint 会自动用 `GITLAB_OMNIBUS_CONFIG` 覆盖 `gitlab.rb`，`gitlab-secrets.json` 保留原有密钥（CSRF/会话不会失效）。

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

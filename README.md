# GitLab Workhorse RS

GitLab Workhorse 的 Rust 重写版本。直接替代 GitLab 官方的 Go Workhorse -- 位于客户端与 GitLab Rails / Gitaly 之间的智能 HTTP 代理。

## 为什么用 Rust

- 更低的内存占用
- 更高的 CPU 效率
- 零成本抽象的协议处理
- 安全的异步并发 (tokio/async-await)

## 功能特性

- HTTP 反向代理至 GitLab Rails 后端
- Git Smart HTTP 协议 (`git clone/push`)，通过 Gitaly gRPC + sidechannel
- 大文件上传处理 (multipart，直达对象存储)
- WebSocket 代理 (ActionCable)
- 图片缩放与 WebP 转换
- CI 制品处理 (ZIP 归档)
- 包仓库代理 (Maven, NPM, PyPI, Debian, Helm 等)
- API 速率限制、请求体大小限制、过载保护
- Prometheus 指标导出
- 基于 JWT 的 Rails 共享密钥认证
- Gitaly 回调 socket 用于 hook 校验

## 性能对比

与 Go 原版 Workhorse 在同环境下的基准测试 (GitLab CE 19.0.2, 500 次 HTTP 健康检查):

| 指标 | Rust | Go | 提升 |
|------|------|-----|------|
| 启动时间 | 197 ms | 2,677 ms | **13.6x** |
| 空闲内存 (RSS) | 20,668 KB | 57,420 KB | **省 64%** |
| 加戴内存 (RSS) | 23,432 KB | 62,888 KB | **省 63%** |
| HTTP 延迟 (最小) | 6 ms | 42 ms | **7x** |
| HTTP 延迟 (最大) | 146 ms | 2,755 ms | **19x** |
| HTTP 延迟 (平均) | 11 ms | 296 ms | **27x** |

## 快速开始

```bash
# 构建
cargo build --release

# 运行（最小配置）
./target/release/gitlab-workhorse-rs \
    --auth-backend http://localhost:8080 \
    --secret-path ./.gitlab_workhorse_secret

# 运行（连接 Gitaly）
./gitlab-workhorse-rs \
    --listen-addr 0.0.0.0:8181 \
    --secret-path /path/to/.gitlab_workhorse_secret \
    --auth-socket /var/opt/gitlab/gitlab-rails/sockets/gitlab.socket \
    --document-root /opt/gitlab/embedded/service/gitlab-rails/public \
    --gitaly-addr unix:/var/opt/gitlab/gitaly/gitaly.socket \
    --gitaly-token "$(cat /var/opt/gitlab/gitaly/.gitlab_secret)" \
    --log-format json
```

## 命令行参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--listen-addr` | `localhost:8181` | 监听地址 |
| `--listen-network` | `tcp` | 网络类型: tcp, tcp4, tcp6, unix |
| `--secret-path` | `./.gitlab_workhorse_secret` | 与 Rails 共享的密钥文件 |
| `--auth-backend` | `http://localhost:8080` | Rails 后端地址 |
| `--auth-socket` | 空 | Rails 后端 Unix socket（优先） |
| `--document-root` | `public` | 静态文件根目录 |
| `--gitaly-addr` | 空 | Gitaly gRPC 地址 |
| `--gitaly-token` | 空 | Gitaly 认证令牌 |
| `--gitaly-callback-socket` | 空 | Gitaly 回调 Unix socket |
| `--log-format` | `text` | 日志格式: text, json, structured, none |
| `--log-file` | 空 | 日志文件路径 |
| `--config` | 空 | TOML 配置文件路径 |
| `--development-mode` | false | 开发模式 |
| `--api-limit` | 0 | API 限流 (0 = 不限制) |
| `--prometheus-listen-addr` | 空 | Prometheus 指标端点 |
| `--proxy-headers-timeout` | 300s | 代理请求头超时 |
| `--shutdown-timeout` | 60s | 优雅关闭超时 |
| `--version` | | 打印版本 |

## Docker

```bash
# 构建镜像
docker build -t gitlab-rs -f docker/Dockerfile-server docker/

# 或从 Docker Hub 拉取
docker pull toarujs/gitlab-rs:latest
```

Docker 部署采用三容器架构 (workhorse + PostgreSQL + Redis)。完整编排文件见 `docker/docker-compose-server.yml`。

## 架构

```
客户端 --> Workhorse RS --> Rails (认证预处理)
                |
                +---------> Gitaly (Git 操作，gRPC)
                |
                +---------> 对象存储 (直传上传/下载)
                |
                +---------> 本地磁盘 (静态文件、仓库)
```

核心模块：

| 模块 | 功能 |
|------|------|
| `proxy/` | HTTP/WebSocket 反向代理，带熔断器 |
| `gitaly/` | Gitaly gRPC 客户端，支持 sidechannel (yamux 多路复用) |
| `senddata/` | 响应注入器 (sendfile, sendurl, git archive, image resizer) |
| `upload/` | Multipart 文件上传，带进度追踪 |
| `git/` | Git Smart HTTP 协议处理 |
| `secret/` | JWT/HMAC 共享密钥认证 |
| `ratelimit/` | API 速率限制 |
| `imageresizer/` | 图片缩放与 WebP 转换 |

## 开源许可

与 GitLab 一致。详见 [LICENSE](LICENSE)。

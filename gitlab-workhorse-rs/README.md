# GitLab Workhorse RS

GitLab Workhorse rewritten in Rust. A drop-in replacement for GitLab's Go Workhorse -- the smart HTTP proxy that sits between clients and GitLab Rails / Gitaly.

## Why Rust

- Lower memory footprint
- Better CPU efficiency
- Zero-cost abstractions for protocol handling
- Safe concurrency with async/await (tokio)

## Features

- HTTP reverse proxy to GitLab Rails backend
- Git Smart HTTP protocol (`git clone/push`) via Gitaly gRPC with sidechannel
- Large file upload handling (multipart, direct to object storage)
- WebSocket proxy (ActionCable)
- Image resizing and WebP conversion
- CI artifacts processing (ZIP archives)
- Package registry proxy (Maven, NPM, PyPI, Debian, Helm, etc.)
- Rate limiting, body size limiting, load shedding
- Prometheus metrics export
- JWT-based auth with Rails shared secret
- Gitaly callback socket for hook validation

## Quick Start

```bash
# Build
cargo build --release

# Run (minimum)
./target/release/gitlab-workhorse-rs \
    --auth-backend http://localhost:8080 \
    --secret-path ./.gitlab_workhorse_secret

# Run with Gitaly
./gitlab-workhorse-rs \
    --listen-addr 0.0.0.0:8181 \
    --secret-path /path/to/.gitlab_workhorse_secret \
    --auth-socket /var/opt/gitlab/gitlab-rails/sockets/gitlab.socket \
    --document-root /opt/gitlab/embedded/service/gitlab-rails/public \
    --gitaly-addr unix:/var/opt/gitlab/gitaly/gitaly.socket \
    --gitaly-token "$(cat /var/opt/gitlab/gitaly/.gitlab_secret)" \
    --log-format json
```

## CLI Options

| Option | Default | Description |
|--------|---------|-------------|
| `--listen-addr` | `localhost:8181` | Listen address |
| `--listen-network` | `tcp` | Network type: tcp, tcp4, tcp6, unix |
| `--secret-path` | `./.gitlab_workhorse_secret` | Shared secret file with Rails |
| `--auth-backend` | `http://localhost:8080` | Rails backend URL |
| `--auth-socket` | (empty) | Rails backend Unix socket (preferred) |
| `--document-root` | `public` | Static files root |
| `--gitaly-addr` | (empty) | Gitaly gRPC address |
| `--gitaly-token` | (empty) | Gitaly auth token |
| `--gitaly-callback-socket` | (empty) | Gitaly callback Unix socket |
| `--log-format` | `text` | text, json, structured, none |
| `--log-file` | (empty) | Log file path |
| `--config` | (empty) | TOML config file path |
| `--development-mode` | false | Enable development mode |
| `--api-limit` | 0 | API rate limit (0 = unlimited) |
| `--prometheus-listen-addr` | (empty) | Prometheus metrics endpoint |
| `--proxy-headers-timeout` | 300s | Proxy headers timeout |
| `--shutdown-timeout` | 60s | Graceful shutdown timeout |
| `--version` | | Print version and exit |

## Docker

```bash
# Build image
docker build -t gitlab-rs -f docker/Dockerfile-server docker/

# Or pull from Docker Hub
docker pull toarujs/gitlab-rs:latest
```

Docker setup uses a three-container architecture (workhorse + PostgreSQL + Redis). See `docker/docker-compose-server.yml` for the full compose file.

## Architecture

```
Client --> Workhorse RS --> Rails (auth preprocessing)
               |
               +---------> Gitaly (git operations via gRPC)
               |
               +---------> Object Storage (direct upload/download)
               |
               +---------> Local Disk (static files, repositories)
```

Key modules:

| Module | Purpose |
|--------|---------|
| `proxy/` | HTTP/WebSocket reverse proxy with circuit breaker |
| `gitaly/` | Gitaly gRPC client with sidechannel (yamux multiplexing) |
| `senddata/` | Response injectors (sendfile, sendurl, git archives, image resizer) |
| `upload/` | Multipart file upload with progress tracking |
| `git/` | Git Smart HTTP protocol handler |
| `secret/` | JWT/HMAC shared secret authentication |
| `ratelimit/` | API rate limiting |
| `imageresizer/` | Image scaling and WebP conversion |

## License

Same as GitLab. See [LICENSE](LICENSE).

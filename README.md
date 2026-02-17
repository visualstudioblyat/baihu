<p align="center">
  <img src="baihu.svg" alt="Baihu" width="128" height="128">
</p>

<h1 align="center">Baihu</h1>

<p align="center">
  <strong>AI with teeth.</strong>
</p>

<p align="center">
  <a href="https://github.com/visualstudioblyat/baihu/releases"><img alt="GitHub Release" src="https://img.shields.io/github/v/release/visualstudioblyat/baihu?style=flat-square&color=6366f1"></a>
  <a href="https://github.com/visualstudioblyat/baihu/blob/main/LICENSE"><img alt="License" src="https://img.shields.io/github/license/visualstudioblyat/baihu?style=flat-square&color=6366f1"></a>
  <a href="https://github.com/visualstudioblyat/baihu/stargazers"><img alt="Stars" src="https://img.shields.io/github/stars/visualstudioblyat/baihu?style=flat-square&color=6366f1"></a>
  <a href="https://github.com/visualstudioblyat/baihu/issues"><img alt="Issues" src="https://img.shields.io/github/issues/visualstudioblyat/baihu?style=flat-square&color=6366f1"></a>
</p>

<p align="center">
  <a href="https://github.com/visualstudioblyat/baihu/issues">Report Bug</a> ·
  <a href="https://github.com/visualstudioblyat/baihu/discussions">Feature Request</a>
</p>

---

## Why I Built This

Every AI CLI I tried needed Python, Node, or Docker and 400MB of RAM. Most had real security bugs — UUID v4 for key generation, no zeroize, half-written config files, SSRF-able provider endpoints.

One binary. No runtime. 100% Rust. 482 tests.

## What's In It

- **5 Built-in Providers.** OpenRouter, Anthropic, OpenAI, Ollama, and any OpenAI-compatible API with `custom:https://your-endpoint.com`. Groq, Mistral, xAI, DeepSeek, Together, and others work through the custom provider.
- **7 Chat Channels.** Telegram, Discord, Slack, iMessage, Matrix, WhatsApp, Webhooks. All run simultaneously through the daemon. Implement the `Channel` trait to add your own.
- **Custom Memory Engine.** No Pinecone, no Elasticsearch, no LangChain. SQLite with FTS5 + BM25 keyword search, vector cosine similarity, weighted hybrid merge, embedding cache with LRU eviction. Large entries get LZ4 compressed automatically (anything over 1KB). All custom, zero external dependencies.
- **Encrypted Secrets.** API keys encrypted with ChaCha20-Poly1305 AEAD. Keys generated from OS CSPRNG, not UUID. Secret key material wrapped with `Zeroizing<Vec<u8>>` so it's zeroed on drop. On Windows, the key file itself is envelope-encrypted with DPAPI bound to your login session. Fresh nonce per encryption. Poly1305 tag prevents tampering.
- **Atomic Everything.** Config saves, secret key writes, daemon state flushes all go through write-tmp, fsync, rename. If the process dies mid-write you get the old file, not a corrupt one. The daemon grabs an exclusive file lock on startup so you can't accidentally run two instances and corrupt state.
- **Gateway Pairing.** Localhost-only by default. 6-digit OTP on first connect, bearer tokens after. Constant-time comparison that doesn't leak length info. Brute force lockout after 5 attempts. Refuses to bind 0.0.0.0 without a tunnel.
- **SSRF Protection.** Provider URLs are validated against private IP ranges (127.x, 10.x, 172.16-31.x, 192.168.x, 169.254.x, CGNAT, IPv6 loopback/link-local) before any request goes out. Custom redirect policy validates every 3xx hop to block redirect-to-localhost attacks. Ollama is intentionally exempt because it's supposed to be local.
- **Filesystem Sandbox.** Path jail, symlink escape detection, null byte injection blocked, command allowlisting, system directory protection. On Windows, shell commands run inside a Job Object with KILL_ON_JOB_CLOSE and a 256MB memory limit. Default: supervised + workspace-only.
- **Retry with Jitter.** Provider calls and daemon components use exponential backoff with +/-25% random jitter to prevent thundering herd on mass restart. Response caching with DashMap (60s TTL) so identical prompts don't burn API credits.
- **Heartbeat & Scheduler.** Periodic tasks from HEARTBEAT.md, cron scheduling, skills loader, 74 integrations registry.
- **Setup Wizard.** `baihu onboard` gets you running in under 60 seconds. Live connection testing, secure defaults.

## Tech Stack

| Layer | Tech |
|-------|------|
| Binary Size | ~4.5MB (.exe) / ~3.4MB (unix) |
| Language | Rust, 100% |
| Allocator | mimalloc (Mozilla/Microsoft) |
| Mutex | parking_lot (1 byte vs 40) |
| Concurrency | tokio JoinSet structured concurrency |
| Memory | SQLite + FTS5 + vector cosine similarity + LZ4 |
| Encryption | ChaCha20-Poly1305 AEAD + DPAPI (Windows) |
| Secrets | Zeroize on drop, CSPRNG key gen, atomic writes |
| HTTP | axum + tower, SSRF-validated provider URLs |
| Caching | DashMap concurrent hashmap, 60s TTL |
| Build | opt-level=z, LTO, panic=abort, codegen-units=1 |

## Quick Start

```bash
git clone https://github.com/visualstudioblyat/baihu.git
cd baihu
cargo build --release
cargo install --path . --force

baihu onboard --interactive
baihu agent -m "hello"
```

Or with Gemini:

```toml
# ~/.baihu/config.toml
default_provider = "custom:https://generativelanguage.googleapis.com/v1beta/openai"
default_model = "gemini-2.5-flash"
api_key = "your-google-api-key"
```

## Commands

| Command | What it does |
|---------|-------------|
| `baihu agent -m "..."` | Single message |
| `baihu agent` | Interactive chat |
| `baihu daemon` | Full runtime (gateway + channels + heartbeat + scheduler) |
| `baihu gateway` | Webhook server |
| `baihu doctor` | System diagnostics |
| `baihu status` | Full status |
| `baihu onboard` | Setup wizard |
| `baihu channel start` | Start all chat channels |
| `baihu cron add/list` | Scheduled tasks |
| `baihu service install/start/stop` | OS service management |

## Architecture

Every subsystem is a trait. Swap implementations with a config change, zero code changes.

| Subsystem | Trait | Ships with | Extend |
|-----------|-------|------------|--------|
| AI Models | `Provider` | 5 providers + custom | `custom:https://your-api.com` |
| Channels | `Channel` | CLI, Telegram, Discord, Slack, iMessage, Matrix, WhatsApp, Webhook | Any messaging API |
| Memory | `Memory` | SQLite hybrid search + LZ4 compression | Any persistence backend |
| Tools | `Tool` | shell, file_read, file_write, memory_store, memory_recall, browser, composio | Any capability |
| Observability | `Observer` | noop, log, multi | Prometheus, OTEL |
| Security | `SecurityPolicy` | Pairing, sandbox, allowlists, SSRF, encrypted secrets, DPAPI, zeroize | - |
| Tunnel | `Tunnel` | Cloudflare, Tailscale, ngrok, custom | Any tunnel binary |

## Building from Source

```bash
cargo build              # dev build
cargo build --release    # release (~3.4MB)
cargo test --lib         # 482 tests
cargo clippy             # lint (0 warnings)
```

## Contributing

Baihu is open source under the [MIT](LICENSE) license. Contributions welcome. Open an issue or submit a PR.

See [CONTRIBUTING.md](CONTRIBUTING.md).

## Roadmap

- [x] ~~5 built-in providers + any OpenAI-compatible API~~
- [x] ~~7 chat channels (Telegram, Discord, Slack, iMessage, Matrix, WhatsApp, Webhook)~~
- [x] ~~SQLite hybrid memory (FTS5 + BM25 + vector cosine similarity)~~
- [x] ~~ChaCha20-Poly1305 encrypted secrets with CSPRNG + zeroize~~
- [x] ~~DPAPI envelope encryption (Windows)~~
- [x] ~~Atomic file writes (crash-safe config, secrets, daemon state)~~
- [x] ~~SSRF mitigation on provider URLs~~
- [x] ~~Gateway pairing with OTP + bearer tokens~~
- [x] ~~Filesystem sandbox with symlink escape detection~~
- [x] ~~Windows Job Object sandboxing for shell commands~~
- [x] ~~LZ4 compression for large memory entries~~
- [x] ~~DashMap response caching with TTL~~
- [x] ~~Exponential backoff with jitter~~
- [x] ~~Daemon single-instance file locking~~
- [x] ~~Heartbeat engine + cron scheduler~~
- [x] ~~74 integrations registry~~
- [x] ~~OS service management (systemd, launchd)~~
- [x] ~~Setup wizard (`baihu onboard`)~~
- [x] ~~Tunnel support (Cloudflare, Tailscale, ngrok)~~
- [ ] Linux Landlock filesystem isolation for shell commands
- [ ] Governor rate limiting on provider calls
- [ ] Plugin system (hot-loadable skills from `~/.baihu/skills/`)
- [ ] Web UI dashboard
- [ ] Voice channels
- [ ] Cross-platform installers (Homebrew, AUR, Scoop)

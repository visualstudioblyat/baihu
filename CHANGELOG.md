# changelog

## [unreleased] - the refactor

i took apart openclaw and rebuilt it as baihu in about 10 hours. heres everything that changed and why.

### naming

- `bh_` token prefix across all pairing tokens, matrix transaction ids, test fixtures

### performance

- **mimalloc global allocator** - swapped the default rust allocator for mimalloc. better throughput on small allocations which is the main pattern in async servers. two lines of code, measurable improvement.
- **parking_lot::Mutex** - replaced every `std::sync::Mutex` in the codebase (health, memory/sqlite, security/pairing, security/policy). parking_lot is 1 byte vs 40 on windows, no poisoning ceremony, returns the guard directly instead of a Result. cleaned up a ton of `.unwrap_or_else(PoisonError::into_inner)` noise.
- **JoinSet for daemon** - upgraded the daemon from `Vec<JoinHandle>` to tokio `JoinSet`. structured concurrency with automatic lifecycle management. cleaner shutdown, less code.
- **build profile** - `opt-level=z` (size), `lto = true` (cross-crate optimization), `panic = "abort"` (no unwinding), `codegen-units = 1` (better optimization). the binary is ~3.4MB.

### security

- **chacha20-poly1305 migration** - legacy `enc:` (xor cipher) secrets auto-migrate to `enc2:` (chacha20-poly1305 aead) on decrypt. xor cipher kept for backward compat but deprecated.
- `SecretStore::decrypt_and_migrate()` - decrypt + upgrade in one call
- `SecretStore::needs_migration()` - check if a value is legacy
- `SecretStore::is_secure_encrypted()` - check if enc2 format
- **ssrf redirect protection** - custom reqwest redirect policy validates each 3xx hop against private ip ranges. blocks redirect-to-localhost attacks where an attacker-controlled url returns 302 to `http://127.0.0.1`. all external providers (openrouter, anthropic, openai, compatible) use the ssrf-safe client. ollama exempt (intentionally local).
- **encrypted gateway tokens** - paired bearer tokens are now encrypted through `SecretStore` before writing to config.toml. decrypted transparently on load. prevents casual exposure in config files.
- **config file permissions** - config.toml permissions checked on load, auto-fixed to 0600 (owner-only) on unix. set on every save.
- **dpapi catch_unwind** - windows dpapi `protect()` and `unprotect()` ffi calls wrapped in `std::panic::catch_unwind`. prevents undefined behavior from panics unwinding across ffi boundaries.

### evaluated and skipped

some optimizations from the research docs were evaluated and deliberately not applied:

- **dashmap** - searched for `Arc<Mutex<HashMap>>` patterns, found none. no targets.
- **rkyv** - zero-copy deserialization. doesnt fit because sqlite stores text columns, not binary blobs. would need schema migration for marginal gain.
- **bumpalo** - arena allocator. doesnt mix well with tokio async (futures must be Send + static, arena refs are not).
- **tantivy** - full search engine. conflicts with the "smallest binary" goal. fts5 already handles it.
- **smallvec/compact_str** - deps added to Cargo.toml but struct changes deferred. touching dozens of serde-derived structs across public apis for marginal gain wasnt worth the blast radius.

## [0.1.0] - 2025-02-13

### added

- core architecture with trait-based pluggable subsystems
- openrouter provider (access claude, gpt-4, llama, gemini via single api)
- cli channel with interactive and single-message modes
- observability (noop, log, multi)
- security sandbox (workspace scoping, command allowlisting, path traversal blocking, autonomy levels, rate limiting)
- tools (shell, file_read, file_write)
- sqlite memory with full-text search
- heartbeat engine
- native runtime adapter
- toml config with sensible defaults
- onboarding wizard
- github actions ci
- 159 tests
- 3.1MB release binary

[0.1.0]: https://github.com/visualstudioblyat/baihu/releases/tag/v0.1.0

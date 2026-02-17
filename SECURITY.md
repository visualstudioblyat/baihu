# security

## reporting vulnerabilities

dont open a public issue. use [github security advisories](https://github.com/visualstudioblyat/baihu/security/advisories/new) or reach out to the maintainers directly.

include what the vulnerability is, how to reproduce it, and what the impact is. well acknowledge within 48 hours and aim to fix critical issues within 2 weeks.

## how security works in baihu

security isnt a single layer here, its everywhere. heres what we do and why.

### autonomy levels

three modes, defaulting to the safest useful one:

- **readonly** - can observe but cant act. no shell, no file writes
- **supervised** - can act within allowlists (this is the default)
- **full** - autonomous within workspace sandbox

### workspace scoping

`workspace_only = true` by default. this means:

- all file operations are confined to the workspace directory
- absolute paths are rejected
- path traversal (`../../../etc/passwd`) is blocked
- symlink escape detection via canonical path verification
- null byte injection blocked (prevents c-level path truncation)

14 system directories are always blocked: `/etc`, `/root`, `/home`, `/usr`, `/bin`, `/sbin`, `/lib`, `/opt`, `/boot`, `/dev`, `/proc`, `/sys`, `/var`, `/tmp`

4 sensitive dotfile paths are always blocked: `~/.ssh`, `~/.gnupg`, `~/.aws`, `~/.config`

### command allowlisting

only explicitly approved commands can execute. the default list is: `git`, `npm`, `cargo`, `ls`, `cat`, `grep`, `find`, `echo`, `pwd`, `wc`, `head`, `tail`

we also block:
- subshell operators (backticks, `$(`, `${`) that hide arbitrary execution
- output redirections (`>`, `>>`) that could write outside workspace
- command chaining (`&&`, `||`, `;`, `|`) is validated per-segment
- env var prefix bypass (`FOO=bar rm -rf /`) is caught

### gateway pairing

the gateway binds to `127.0.0.1` by default. it refuses to bind to `0.0.0.0` unless you have a tunnel configured or explicitly set `allow_public_bind = true`.

on first connect:
1. server prints a 6-digit pairing code to the terminal
2. client sends code via `X-Pairing-Code` header on `POST /pair`
3. server responds with a bearer token
4. all subsequent requests require `Authorization: Bearer <token>`

brute force protection: 5 wrong attempts triggers a 5-minute lockout. the pairing code comparison uses constant-time equality to prevent timing attacks.

### encrypted secrets

api keys in config are encrypted with chacha20-poly1305 aead, not stored as plaintext. the encryption key lives in `~/.baihu/.secret_key` with restrictive file permissions (0600 on unix, icacls restricted on windows).

each encryption generates a fresh random nonce so the same plaintext produces different ciphertext every time. the poly1305 tag prevents tampering.

legacy xor-encrypted secrets (`enc:` prefix) are auto-migrated to the secure format (`enc2:`) on decrypt. the xor cipher is kept for backward compat but its deprecated.

### ssrf redirect protection

provider urls are validated before requests go out (blocks private ips, cloud metadata endpoints, etc). additionally, a custom redirect policy validates every 3xx hop. this prevents redirect-to-localhost attacks where an attacker url returns `302 -> http://127.0.0.1/...` to bypass the initial url check. ollama is intentionally exempt since its meant to be local.

### config file hardening

config.toml is checked for permissions on load. on unix, it auto-fixes to 0600 (owner read/write only) and warns if it was too permissive. paired gateway tokens are encrypted through chacha20-poly1305 before writing to disk so they dont sit as plaintext in config files.

### rate limiting

sliding window rate limiter caps actions per hour. configurable via `max_actions_per_hour` in config. prevents runaway agent loops.

### channel allowlists

consistent across telegram, discord, slack, whatsapp:
- empty allowlist = deny all (safe default)
- `"*"` = allow all (explicit opt-in)
- otherwise exact-match list

## testing

security is covered by automated tests:

```bash
cargo test --lib -- security
cargo test --lib -- tools::shell
cargo test --lib -- tools::file_read
cargo test --lib -- tools::file_write
```

## container security

docker images follow cis docker benchmark:
- runs as uid 65534 (distroless nonroot)
- `gcr.io/distroless/cc-debian12:nonroot` base (no shell, no package manager)
- supports `--read-only` filesystem with `/workspace` volume mount

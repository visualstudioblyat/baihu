# contributing

## setup

```bash
git clone https://github.com/visualstudioblyat/baihu.git
cd baihu
cargo build
cargo test --lib
```

theres a pre-push hook that runs fmt, clippy, and tests. enable it:

```bash
git config core.hooksPath .githooks
```

skip it when you need to:

```bash
git push --no-verify
```

ci runs the same checks so itll catch it either way.

## how the codebase works

everything is a trait. if you want to add a new provider, channel, tool, or observer you just implement the trait and register it in the factory.

```
src/
  providers/       # llm backends      -> Provider trait
  channels/        # messaging          -> Channel trait
  observability/   # metrics/logging    -> Observer trait
  tools/           # agent capabilities -> Tool trait
  memory/          # persistence        -> Memory trait
  security/        # sandbox + policy   -> SecurityPolicy
  tunnel/          # tunnel providers   -> Tunnel trait
```

## adding a provider

create `src/providers/your_provider.rs`, implement `Provider`, register it in `src/providers/mod.rs`:

```rust
"your_provider" => Ok(Box::new(your_provider::YourProvider::new(api_key))),
```

same pattern for channels, tools, observers. look at an existing one for the shape.

## code style

- no verbose doc comments. if the function name says it, dont repeat it in a comment
- comments lowercase and lazy: `// skip if no team`, `// grab cached key`
- early returns over deep nesting
- no unnecessary abstractions
- `parking_lot::Mutex` not `std::sync::Mutex`
- `?` and `anyhow` for errors, no `.unwrap()` in production code
- keep deps minimal. every crate adds to binary size

## pr checklist

- `cargo fmt` passes
- `cargo clippy -- -D warnings` clean
- `cargo test --lib` passes (482 tests, 2 pre-existing windows failures are known)
- new code has inline `#[cfg(test)]` tests
- no new deps unless you really need them
- follows existing patterns

## commit style

```
feat: add anthropic provider
fix: path traversal edge case with symlinks
refactor: swap std mutex for parking_lot
test: add brute force lockout tests
```

## reporting issues

- bugs: include os, rust version, steps to reproduce
- features: describe the use case, which trait to extend
- security: see [SECURITY.md](SECURITY.md)

## license

contributions are MIT licensed.

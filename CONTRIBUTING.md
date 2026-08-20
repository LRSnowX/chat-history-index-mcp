# Contributing

Open an issue before implementing a new provider parser. Native provider support requires a real, redacted fixture and tests; format guesses are not accepted.

Before submitting a change:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Never commit exports, transcripts, indexes, WAL files, tokens, private hostnames, or provider credentials.

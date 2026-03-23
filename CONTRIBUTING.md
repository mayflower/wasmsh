# Contributing to wasmsh

## Getting Started

```bash
git clone https://github.com/user/wasmsh
cd wasmsh
cargo test --workspace
```

## Development Workflow

```bash
just check    # pre-push: fmt + clippy + tests
just ci       # full CI locally
```

## Adding Features

- **New builtin**: See [docs/guides/adding-builtins.md](docs/guides/adding-builtins.md)
- **New utility**: See [docs/guides/adding-utilities.md](docs/guides/adding-utilities.md)
- **New shell feature**: Add to the parser, expand, and browser runtime

## Writing Tests

Every behavioral change needs a TOML test case in `tests/suite/`. See [docs/tutorials/writing-tests.md](docs/tutorials/writing-tests.md).

## Code Style

- `cargo fmt --all` before committing
- `cargo clippy --workspace --all-targets` must pass with zero warnings
- No `unsafe` code
- No GPL dependencies

## Clean-Room Rules

- Do not copy code from Bash, BusyBox, or any GPL project
- Do not copy test cases from GPL test suites
- Behavioral compatibility is achieved through specification reading and black-box testing
- Document provenance in commit messages for non-trivial implementations

## Pull Request Process

1. Create a feature branch
2. Make your changes with tests
3. Run `just ci` locally
4. Submit a PR with a clear description
5. All CI checks must pass

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).

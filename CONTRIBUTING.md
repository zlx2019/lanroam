# Contributing Guide

Thank you for contributing. Please read the following guidelines before submitting changes.

## Repository Layout

| Path | What it is |
|------|------------|
| `deps/lan-kit` | Shared LAN foundation: app profile, device identity, mutual TLS 1.3, discovery, framing. Knows nothing about Lanroam |
| `deps/lanroam-input` | Keyboard and mouse: capture, injection, key maps, screen geometry, edge switching. No networking |
| `deps/lanroam-core` | The engine: desk groups, screen layout, QUIC transport, protocol, input routing. No UI |
| `deps/lanroam-cli` | Command-line node for trying things out, debugging the protocol and testing between machines |
| `scripts/windows` | Fetches the latest Windows dev build onto a test machine |

Each layer only depends on the ones listed above it. `lan-kit` is shared with the sibling apps ([Deskmate](https://github.com/zlx2019/deskmate), [Lanecho](https://github.com/zlx2019/lanecho)), so keep app-specific logic out of it.

## Development Environment

### Rust Toolchain

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

The project pins its Rust version in `rust-toolchain.toml`. After entering the project directory, `rustup` will automatically install the required toolchain and components when needed.

### Development Tools

```bash
cargo install --locked cargo-deny     # Dependency security and license auditing
cargo install --locked typos-cli      # Spell checking
cargo install --locked git-cliff      # Changelog generation
cargo install --locked cargo-nextest  # Enhanced test runner
pip install pre-commit                # Git pre-commit checks
```

### Enable Pre-Commit

```bash
pre-commit install
```

After installation, each `git commit` runs fmt, deny, typos, check, clippy, test and doc checks. The commit only succeeds when all of them pass.

Note that pre-commit stashes unstaged changes while it runs, which silently discards any fixes `cargo fmt` makes during the hook. Run `cargo fmt --all` yourself before committing.

### Operating System Permissions

On macOS, capturing and injecting input needs **Accessibility** and **Input Monitoring** (System Settings > Privacy & Security), granted to the app that starts Lanroam — your terminal when you use the CLI. Windows needs no extra permissions, but its firewall has to let the LAN reach the port (see `scripts/windows/update.ps1`).

## Local Checks

Before submitting changes, make sure the same checks CI runs pass:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --all-features
cargo nextest run --workspace --all-features --no-tests pass
cargo deny check
typos
```

Platform input code is compiled only on its own OS, and CI lints it on macOS and Windows as well. `lanroam-input` has no C dependencies, so its Windows code can be linted from macOS or Linux too:

```bash
rustup target add x86_64-pc-windows-msvc
cargo clippy -p lanroam-input --all-targets --target x86_64-pc-windows-msvc -- -D warnings
```

Tests that need OS permissions (posting input on macOS) are ignored by default; run them with `cargo nextest run --run-ignored only` from a terminal that has them.

## Testing Between Machines

The engine tests run several nodes in one process, but input capture and injection can only really be checked on real machines. Changes to `lanroam-input` or to how input is routed should be tried between a Mac and a Windows PC:

- Run `cargo run -p lanroam-cli -- run` on each machine; `--dry-run` prints received input instead of injecting it.
- Every push to `main` publishes `lanroam-cli.exe` to the rolling [`dev` pre-release](https://github.com/zlx2019/lanroam/releases/tag/dev); the README shows how to fetch it on Windows.

## Conventions

- **Protocol changes stay compatible within a major version.** `PROTOCOL_VERSION` is `major.minor`: a new message or field is additive, bumps the minor version and must be safe for an older peer to skip; only a major bump refuses to talk to older peers. Add a test that an older peer still gets along.
- **No key stays pressed.** Every path that ends control — leaving a device, a lost connection, preemption, pausing — must release the keys and buttons still held on the other side. Lanroam's own hotkeys are handled on the device that captured them and never forwarded.
- **Blocking work stays off the async runtime.** Platform input, display queries and file I/O run on their own threads or through `spawn_blocking`.
- **Errors**: `thiserror` in the libraries, `anyhow` only in binaries; no `unwrap` outside tests.
- **Comments** are written in English, and public items are documented (CI builds the docs with warnings denied).

## Commit Convention

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/):

| Type | Description |
|------|-------------|
| `feat:` | New feature |
| `fix:` | Bug fix |
| `docs:` | Documentation change |
| `refactor:` | Refactor that is neither a feature nor a bug fix |
| `perf:` | Performance improvement |
| `test:` | Test-related change |
| `chore:` | Build, toolchain, or miscellaneous maintenance |
| `ci:` | CI workflow change |

Use a scope for the crate or area when it helps, e.g. `fix(input): ...`; security fixes use `fix(security): ...` and get their own section in the changelog. The changelog is generated by git-cliff from commit history, so keep commit messages consistent.

## Branches And Pull Requests

- Create feature branches from `main`.
- Keep each pull request focused on a single topic.
- Pull requests must pass CI: lint (fmt, clippy, doc), platform clippy on macOS and Windows, tests on Linux, macOS and Windows, deny and typos.
- A bug fix should come with a test that fails without it. If the defect cannot be reproduced in a test (input on real hardware, for example), say how you verified it in the pull request instead.
- Report security issues privately as described in [SECURITY.md](./SECURITY.md), not in a pull request or issue.

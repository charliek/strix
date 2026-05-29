# Installation

## Build from source

strix is written in Rust and pins its toolchain in `rust-toolchain.toml`
(currently **1.96.0**). If you use [`mise`](https://mise.jdx.dev/) or `rustup`,
the correct toolchain installs automatically.

```bash
git clone https://github.com/charliek/strix
cd strix
cargo build --release
```

The binary is written to `target/release/strix`. Copy it somewhere on your
`PATH`:

```bash
install -m 0755 target/release/strix ~/.local/bin/strix
```

## Requirements

| Requirement | Notes                                                          |
|-------------|----------------------------------------------------------------|
| Rust 1.96.0 | Pinned via `rust-toolchain.toml`; rustup/mise auto-install it. |
| `git`       | Used for staging mutations (stage / unstage / reset).          |
| A truecolor terminal | Themes use 24-bit RGB. Ghostty, iTerm2, Alacritty, WezTerm, and most modern terminals qualify. |

## Verify

```bash
strix --version
strix --help
```

Prebuilt binaries are not published yet; build from source for now.

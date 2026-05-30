# Installation

strix publishes prebuilt binaries for macOS and Linux (`amd64` and `arm64`) on
every release. **Homebrew** is the recommended path on macOS; **apt** on Linux.
Building from source is fully supported too.

## macOS (Homebrew)

```bash
brew install charliek/tap/strix
```

Upgrade later with:

```bash
brew update && brew upgrade strix
```

## Linux (apt)

Add the signed apt repository once, then install:

```bash
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://apt.stridelabs.ai/pubkey.gpg | \
  sudo tee /etc/apt/keyrings/apt-charliek.gpg > /dev/null
echo 'deb [signed-by=/etc/apt/keyrings/apt-charliek.gpg] https://apt.stridelabs.ai noble main' | \
  sudo tee /etc/apt/sources.list.d/apt-charliek.list
sudo apt update
sudo apt install strix
```

After that, `sudo apt update && sudo apt upgrade` picks up new releases. Tested on
Ubuntu 24.04+ and Pop!_OS 24.04; architectures `amd64` and `arm64`.

## Linux (`.deb`, no apt repo)

Prefer a one-off download over adding the repository? Grab the `.deb` for your
architecture from the [latest release](https://github.com/charliek/strix/releases/latest):

```bash
ARCH=$(dpkg --print-architecture)        # amd64 or arm64
VERSION=0.0.1                            # latest from the releases page
curl -fLO "https://github.com/charliek/strix/releases/download/v${VERSION}/strix_${VERSION}_${ARCH}.deb"
sudo apt install -y "./strix_${VERSION}_${ARCH}.deb"
```

## Build from source

strix is written in Rust and pins its toolchain in `rust-toolchain.toml`
(currently **1.96.0**). With [`mise`](https://mise.jdx.dev/) or `rustup`, the
correct toolchain installs automatically.

```bash
git clone https://github.com/charliek/strix
cd strix
cargo build --release
install -m 0755 target/release/strix ~/.local/bin/strix
```

## Requirements

| Requirement | Notes |
|-------------|-------|
| `git` on `PATH` | Required at runtime — strix shells out to `git` for staging mutations (stage / unstage / reset). The apt and Homebrew packages declare this dependency. |
| A truecolor terminal | Themes use 24-bit RGB. Ghostty, iTerm2, Alacritty, WezTerm, and most modern terminals qualify. |
| Rust 1.96.0 | **Only when building from source.** Pinned via `rust-toolchain.toml`; rustup/mise auto-install it. The prebuilt binaries have no build-time requirement. |

## Verify

```bash
strix --version
strix --help
```

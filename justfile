# pp - Fast repository locator

bin_name := "pp"
install_dir := env("HOME") / ".local/bin"
fish_func_dir := env("HOME") / ".config/fish/functions"
fish_comp_dir := env("HOME") / ".config/fish/completions"
bash_comp_dir := env("HOME") / ".local/share/bash-completion/completions"
zsh_comp_dir := env("HOME") / ".local/share/zsh/site-functions"

# Default: build release binary
default: build

# Build release binary
build:
    cargo build --release

# Build static binary via musl + mimalloc
build-static:
    cargo build --release --target x86_64-unknown-linux-musl -p pp-cli

# Build static binary recompiling core, alloc, std from source (nightly)
build-std:
    RUSTFLAGS="-C panic=abort -C link-arg=-s" cargo +nightly build --release --target x86_64-unknown-linux-musl -Z build-std=core,alloc,std -p pp-cli

# Build the FFI cdylib for THIS machine (-C target-cpu=native).
# For a portable build (any x86-64) use `just nvim-portable`.
build-nvim-native:
    RUSTFLAGS="-C target-cpu=native" cargo build -p pp-nvim --release
    @mkdir -p build
    cp target/release/libpp_nvim.so build/libpp_nvim.so
    @echo "✓ libpp_nvim.so (native) → build/libpp_nvim.so"

# Portable FFI cdylib — runs on any x86-64 CPU.
nvim-portable:
    cargo build -p pp-nvim --release
    @mkdir -p build
    cp target/release/libpp_nvim.so build/libpp_nvim.so
    @echo "✓ libpp_nvim.so (portable) → build/libpp_nvim.so"

# Run clippy with pedantic lints
clippy:
    cargo clippy --workspace --all-targets -- -W clippy::pedantic

# Sandboxed Lua test suites (fake $HOME, never touches your config)
test-lua:
    ./tests/run.sh

# ── install / uninstall ──────────────────────────────────────────────

install: build install-bin install-fish install-completions
    @echo "✓ pp installed — restart your shell or: source {{fish_func_dir}}/pp.fish"

# Build and install static musl + mimalloc binary + shell integrations
install-static: build-static
    @mkdir -p {{install_dir}}
    cp -f target/x86_64-unknown-linux-musl/release/{{bin_name}} {{install_dir}}/{{bin_name}}
    @echo "✓ Static binary (musl + mimalloc) → {{install_dir}}/{{bin_name}}"
    @just install-fish
    @just install-completions
    @echo "✓ pp static installed — restart your shell or: source {{fish_func_dir}}/pp.fish"

install-bin:
    @mkdir -p {{install_dir}}
    cp target/release/{{bin_name}} {{install_dir}}/{{bin_name}}
    @echo "✓ Binary → {{install_dir}}/{{bin_name}}"

install-fish:
    @mkdir -p {{fish_func_dir}}
    cp scripts/pp.fish {{fish_func_dir}}/pp.fish
    @echo "✓ Fish function → {{fish_func_dir}}/pp.fish"

install-completions: install-fish-completions install-bash-completions install-zsh-completions

install-fish-completions:
    @mkdir -p {{fish_comp_dir}}
    target/release/{{bin_name}} generate fish > {{fish_comp_dir}}/pp.fish
    @echo "✓ Fish completions → {{fish_comp_dir}}/pp.fish"

install-bash-completions:
    @mkdir -p {{bash_comp_dir}}
    target/release/{{bin_name}} generate bash > {{bash_comp_dir}}/pp
    @echo "✓ Bash completions → {{bash_comp_dir}}/pp"

install-zsh-completions:
    @mkdir -p {{zsh_comp_dir}}
    target/release/{{bin_name}} generate zsh > {{zsh_comp_dir}}/_pp
    @echo "✓ Zsh completions → {{zsh_comp_dir}}/_pp"

uninstall:
    rm -f {{install_dir}}/{{bin_name}}
    rm -f {{fish_func_dir}}/pp.fish
    rm -f {{fish_comp_dir}}/pp.fish
    rm -f {{bash_comp_dir}}/pp
    rm -f {{zsh_comp_dir}}/_pp
    @echo "✓ pp uninstalled"

# ── operational (expect binary to already exist) ─────────────────────

# Rebuild the repository index
index:
    target/release/{{bin_name}} index

# Clear the repository index
clear:
    target/release/{{bin_name}} clear

# Run pp interactively
run:
    target/release/{{bin_name}}

# List indexed repositories
list:
    target/release/{{bin_name}} list

# Clean build artifacts
clean:
    cargo clean

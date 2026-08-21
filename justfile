# pp - Fast repository locator
# Justfile for building, installing, and managing pp

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

# Build the nvim FFI library (cdylib) and copy it into build/.
# Native by default (-C target-cpu=native): the lib is built per machine, so
# march=native is the right default (and what lazy.nvim's build hook uses).
# For a portable build (runs on any x86-64 CPU) use `just nvim-portable`.
nvim: build-nvim-native

# Build with -C target-cpu=native (optimal for THIS machine; not portable).
build-nvim-native:
    RUSTFLAGS="-C target-cpu=native" cargo build -p pp-nvim --release
    @mkdir -p build
    cp target/release/libpp_nvim.so build/libpp_nvim.so
    @echo "✓ libpp_nvim.so (target-cpu=native) installed to build/libpp_nvim.so"

# Portable build: runs on any x86-64 CPU, but leaves some performance on the
# table. (Note: if your global ~/.cargo/config.toml tunes release builds —
# e.g. target-cpu=znver4 — that applies here too.)
build-nvim-portable:
    cargo build -p pp-nvim --release
    @mkdir -p build
    cp target/release/libpp_nvim.so build/libpp_nvim.so
    @echo "✓ libpp_nvim.so (portable) installed to build/libpp_nvim.so"

# Run clippy with pedantic lints on the whole workspace
clippy:
    cargo clippy --workspace --all-targets -- -W clippy::pedantic

# Run sandboxed Lua/nvim test suites (fake $HOME + repos in tests/sandbox;
# never touches your real nvim config, state, or repository index)
test-lua:
    ./tests/run.sh

# Build and install binary + shell integrations
install: build install-bin install-fish install-completions
    @echo "✓ pp installed successfully"
    @echo "  Restart your shell or run: source {{fish_func_dir}}/pp.fish"

# Install binary to ~/.local/bin
install-bin:
    @mkdir -p {{install_dir}}
    cp target/release/{{bin_name}} {{install_dir}}/{{bin_name}}
    @echo "✓ Binary installed to {{install_dir}}/{{bin_name}}"

# Install fish function wrapper (for cd support)
install-fish:
    @mkdir -p {{fish_func_dir}}
    cp scripts/pp.fish {{fish_func_dir}}/pp.fish
    @echo "✓ Fish function installed to {{fish_func_dir}}/pp.fish"

# Generate and install shell completions for fish, bash, zsh
install-completions: install-fish-completions install-bash-completions install-zsh-completions

# Install fish completions
install-fish-completions:
    @mkdir -p {{fish_comp_dir}}
    target/release/{{bin_name}} generate fish > {{fish_comp_dir}}/pp.fish
    @echo "✓ Fish completions installed to {{fish_comp_dir}}/pp.fish"

# Install bash completions
install-bash-completions:
    @mkdir -p {{bash_comp_dir}}
    target/release/{{bin_name}} generate bash > {{bash_comp_dir}}/pp
    @echo "✓ Bash completions installed to {{bash_comp_dir}}/pp"

# Install zsh completions
install-zsh-completions:
    @mkdir -p {{zsh_comp_dir}}
    target/release/{{bin_name}} generate zsh > {{zsh_comp_dir}}/_pp
    @echo "✓ Zsh completions installed to {{zsh_comp_dir}}/_pp"

# Uninstall binary and shell integrations
uninstall:
    rm -f {{install_dir}}/{{bin_name}}
    rm -f {{fish_func_dir}}/pp.fish
    rm -f {{fish_comp_dir}}/pp.fish
    rm -f {{bash_comp_dir}}/pp
    rm -f {{zsh_comp_dir}}/_pp
    @echo "✓ pp uninstalled"

# Rebuild index
index: build
    target/release/{{bin_name}} index

# Clear index
clear: build
    target/release/{{bin_name}} clear

# Run pp interactively (for testing)
run: build
    target/release/{{bin_name}}

# Run pp list (for testing)
list: build
    target/release/{{bin_name}} list

# Clean build artifacts
clean:
    cargo clean

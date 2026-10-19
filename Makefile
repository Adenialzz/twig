.PHONY: build install check

build:
	cargo build --release --locked

install:
	cargo install --path . --locked
	@printf '%s\n' '安装完成。请确保 ~/.cargo/bin 在 PATH 中。' '启用 Zsh 补全：在 ~/.zshrc 的 compinit 初始化之后加入：' 'source <(COMPLETE=zsh twig)' '已配置过补全时，重新打开终端即可。'
	@printf '%s\n' '使用 PR/MR 前，请按需自行安装 gh 或 glab，并登录平台：gh auth login / glab auth login。'

check:
	cargo fmt --check
	cargo clippy --locked --all-targets -- -D warnings
	cargo test --locked

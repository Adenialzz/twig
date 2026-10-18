.PHONY: build install check

build:
	cargo build --release --locked

install:
	@set -eu; \
	missing=''; \
	for tool in gh glab; do \
		if ! command -v "$$tool" >/dev/null 2>&1; then missing="$$missing $$tool"; fi; \
	done; \
	if [ -n "$$missing" ]; then \
		if ! command -v brew >/dev/null 2>&1; then \
			printf '%s\n' "缺少工具：$$missing。请手动安装并加入 PATH，或安装 Homebrew 后重试。" >&2; \
			exit 1; \
		fi; \
		brew install $$missing; \
	fi
	cargo install --path . --locked
	@printf '%s\n' '安装完成。请确保 ~/.cargo/bin 在 PATH 中。' '启用 Zsh 补全：在 ~/.zshrc 的 compinit 初始化之后加入：' 'source <(COMPLETE=zsh twig)' '已配置过补全时，重新打开终端即可。'
	@printf '%s\n' '使用 PR/MR 前，请按需登录平台：gh auth login / glab auth login。'

check:
	cargo fmt --check
	cargo clippy --locked --all-targets -- -D warnings
	cargo test --locked

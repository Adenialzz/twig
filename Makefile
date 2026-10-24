.PHONY: build install check

build:
	cargo build --release --locked

install:
	cargo install --path . --locked
	install -d "$(HOME)/.twig/controller/workspace/.agents/skills/twig-controller"
	install -m 644 skills/twig-controller/SKILL.md "$(HOME)/.twig/controller/workspace/.agents/skills/twig-controller/SKILL.md"
	@printf '%s\n' '安装完成。请确保 ~/.cargo/bin 在 PATH 中。' '启用 Zsh 补全：在 ~/.zshrc 的 compinit 初始化之后加入：' 'source <(COMPLETE=zsh twig)' '已配置过补全时，重新打开终端即可。'

check:
	cargo fmt --check
	cargo clippy --locked --all-targets -- -D warnings
	cargo test --locked

PREFIX ?= $(HOME)/.local
BINDIR := $(PREFIX)/bin
AUTOSTART_DIR := $(HOME)/.config/autostart
CONFIG_DIR := $(HOME)/.config/omarchy-omaframe
PKG := omaframe

.PHONY: all build test install uninstall run clean fmt versions rollback

all: build

build:
	cargo build --release

test:
	cargo test

fmt:
	cargo fmt

run: build
	./target/release/$(PKG)

## 安装：只写用户目录，不需要 root，不改 hypr/omarchy 配置
install: build
	install -Dm755 target/release/$(PKG) $(BINDIR)/$(PKG)
	install -Dm644 packaging/omaframe.desktop $(AUTOSTART_DIR)/$(PKG).desktop
	@echo "已安装："
	@echo "  程序      $(BINDIR)/$(PKG)"
	@echo "  登录自启  $(AUTOSTART_DIR)/$(PKG).desktop"
	@echo "  配置目录  $(CONFIG_DIR)"
	@echo
	@echo "试用：$(BINDIR)/$(PKG)          启动"
	@echo "      $(BINDIR)/$(PKG) settings 打开设置"

uninstall:
	rm -f $(BINDIR)/$(PKG) $(AUTOSTART_DIR)/$(PKG).desktop
	@echo "已卸载（配置保留在 $(CONFIG_DIR)）"

## 版本管理：一键回滚 / 切换
versions:
	@scripts/rollback.sh --list

rollback:
	@scripts/rollback.sh $(REF)

clean:
	cargo clean

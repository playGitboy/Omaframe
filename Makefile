PREFIX ?= $(HOME)/.local
BINDIR := $(PREFIX)/bin
AUTOSTART_DIR := $(HOME)/.config/autostart
CONFIG_DIR := $(HOME)/.config/omarchy-omaframe
DATADIR := $(PREFIX)/share/omaframe
APPDIR := $(PREFIX)/share/applications
ICONDIR := $(PREFIX)/share/icons
PKG := omaframe

.PHONY: all build test install uninstall run clean fmt versions rollback restart-running

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
	@# autostart / 菜单入口：二进制装在 $(BINDIR)，但 systemd 的
	@# xdg-autostart 生成器只在**系统 PATH** 里解析 Exec，不搜 ~/.local/bin →
	@# Exec=omaframe 会被判"Exec binary does not exist"而静默丢弃整个单元。
	@# 本地安装必须写绝对路径（系统包仍用 packaging 里的裸名，见 PKGBUILD）。
	install -d $(AUTOSTART_DIR) $(APPDIR)
	sed 's|^Exec=omaframe$$|Exec=$(BINDIR)/$(PKG)|' packaging/omaframe.desktop \
		> $(AUTOSTART_DIR)/$(PKG).desktop
	sed 's|^Exec=omaframe$$|Exec=$(BINDIR)/$(PKG)|' packaging/omaframe-app.desktop \
		> $(APPDIR)/$(PKG).desktop
	chmod 644 $(AUTOSTART_DIR)/$(PKG).desktop $(APPDIR)/$(PKG).desktop
	@# 相框库必须一起装：frame_dir() 按 exe 同级/上级/编译期路径找 frame/，
	@# 换台机器三处都不存在 → 用户看到"相框库为空"。
	install -d $(DATADIR)/frame
	rm -f $(DATADIR)/frame/*.png          # 先清空，保证与源码目录一致
	install -Dm644 frame/*.png $(DATADIR)/frame/
	install -Dm644 packaging/omaframe.svg $(ICONDIR)/hicolor/scalable/apps/$(PKG).svg
	@echo "已安装："
	@echo "  程序      $(BINDIR)/$(PKG)"
	@echo "  相框库    $(DATADIR)/frame"
	@echo "  登录自启  $(AUTOSTART_DIR)/$(PKG).desktop"
	@echo "  菜单入口  $(APPDIR)/$(PKG).desktop（应用菜单里搜“桌面相框”）"
	@echo "  配置目录  $(CONFIG_DIR)"
	@echo
	@echo "注意：若程序已在运行，请 make restart-running（否则进程仍在跑旧二进制）"
	@echo "试用：$(BINDIR)/$(PKG)          启动"
	@echo "      $(BINDIR)/$(PKG) settings 打开设置"

## 重启运行中的实例（改了代码/装了新构建后必须做，否则进程还跑旧 inode）
restart-running:
	@if pgrep -x $(PKG) >/dev/null 2>&1; then \
		pkill -x $(PKG) || true; sleep 1; \
		setsid nohup $(BINDIR)/$(PKG) >/dev/null 2>&1 & \
		sleep 2; echo "已重启 $(PKG)（$(pgrep -x $(PKG) | head -1)）"; \
	else echo "$(PKG) 未在运行"; fi

uninstall:
	rm -f $(BINDIR)/$(PKG) $(AUTOSTART_DIR)/$(PKG).desktop
	rm -rf $(DATADIR)
	rm -f $(APPDIR)/$(PKG).desktop $(ICONDIR)/hicolor/scalable/apps/$(PKG).svg
	@echo "已卸载（配置保留在 $(CONFIG_DIR)）"

## 版本管理：一键回滚 / 切换
versions:
	@scripts/rollback.sh --list

rollback:
	@scripts/rollback.sh $(REF)

clean:
	cargo clean

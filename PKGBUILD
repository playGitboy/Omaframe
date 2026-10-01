# Maintainer: Omaframe contributors
# Contributor: playGitboy
pkgname=omaframe
pkgver=0.4.2
pkgrel=1
pkgdesc="Omarchy 桌面电子相框：把本地图片/视频以自适应 PNG 相框摆在桌面上"
arch=('x86_64' 'aarch64')
url="https://github.com/playGitboy/Omaframe"
license=('MIT')
# 依赖与仓库内 packaging/dependencies.conf 同源；
# scripts/check-release.sh 会校验二者一致，改依赖后两边都要更新。
depends=('gtk4' 'libadwaita' 'gtk4-layer-shell' 'gdk-pixbuf2' 'cairo' 'pango' 'glib2' 'libx11')
makedepends=('cargo' 'rust' 'pkgconf' 'git')
optdepends=('ffmpeg: 视频解码（缺失时仅显示图片）'
            'imagemagick: HEIC/HEIF 相框素材回退解码')
source=("$pkgname-$pkgver.tar.gz")
sha256sums=('SKIP')          # 由 scripts/check-release.sh 在发布时填入真实校验和
provides=("$pkgname")
conflicts=()
backup=()

# 从源码目录构建（yay 会先 clone 仓库）
build() {
  cd "$pkgname-$pkgver"
  # release profile 已是 lto+strip；这里用并行构建加速
  CARGO_TARGET_DIR="$srcdir/target" cargo build --release --locked
}

check() {
  cd "$pkgname-$pkgver"
  CARGO_TARGET_DIR="$srcdir/target" cargo test --release --locked
}

package() {
  cd "$pkgname-$pkgver"
  install -Dm755 "target/release/omaframe" "$pkgdir/usr/bin/omaframe"
  # 相框库必须随包安装：程序运行时在 /usr/share/omaframe/frame 查找
  install -d "$pkgdir/usr/share/omaframe/frame"
  install -Dm644 frame/*.png "$pkgdir/usr/share/omaframe/frame/"
  # XDG autostart（延迟启动，不拖慢登录）
  install -Dm644 packaging/omaframe.desktop "$pkgdir/etc/xdg/autostart/omaframe.desktop"
  # 系统菜单入口（autostart 项不会出现在应用菜单里）
  install -Dm644 packaging/omaframe-app.desktop "$pkgdir/usr/share/applications/omaframe.desktop"
  install -Dm644 packaging/omaframe.svg \
    "$pkgdir/usr/share/icons/hicolor/scalable/apps/omaframe.svg"
}

post_install() {
  echo "首次运行会在 ~/.config/omarchy-omaframe/ 生成配置。"
  echo "启动：omaframe    设置面板：omaframe settings"
}

post_remove() {
  # 只提示，不删用户配置
  echo "用户配置保留在 ~/.config/omarchy-omaframe/（如需清理请手动删除）"
}
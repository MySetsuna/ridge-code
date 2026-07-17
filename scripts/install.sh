#!/bin/sh
# RidgeCode 安装器(Unix:Linux / macOS)—— 零 cargo、零源码,只需一个独立二进制。
#
#   在线装最新版:  curl -fsSL https://raw.githubusercontent.com/MySetsuna/ridge-code/main/scripts/install.sh | sh
#   指定版本:      ... | sh -s -- --version v0.2.0
#   装本地已构建:  ./scripts/install.sh --local target/release/ridgecode
#
# 装到 $RIDGE_BIN_DIR(默认 ~/.local/bin);若不在 PATH,脚本会提示如何加。
set -eu

REPO="MySetsuna/ridge-code"
BIN="ridgecode"
BIN_DIR="${RIDGE_BIN_DIR:-$HOME/.local/bin}"
VERSION="latest"
LOCAL=""

while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --local)   LOCAL="$2";   shift 2 ;;
    --dir)     BIN_DIR="$2"; shift 2 ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done

mkdir -p "$BIN_DIR"

install_file() { # $1 = 源二进制路径
  install -m 0755 "$1" "$BIN_DIR/$BIN" 2>/dev/null || { cp "$1" "$BIN_DIR/$BIN"; chmod 0755 "$BIN_DIR/$BIN"; }
  echo "✓ 已安装: $BIN_DIR/$BIN"
}

if [ -n "$LOCAL" ]; then
  [ -f "$LOCAL" ] || { echo "本地文件不存在: $LOCAL" >&2; exit 1; }
  install_file "$LOCAL"
else
  # 探测目标三元组
  os="$(uname -s)"; arch="$(uname -m)"
  case "$os" in
    Linux)  plat="unknown-linux-gnu" ;;
    Darwin) plat="apple-darwin" ;;
    *) echo "不支持的系统: $os(可用 --local 装已构建二进制)" >&2; exit 1 ;;
  esac
  case "$arch" in
    x86_64|amd64) cpu="x86_64" ;;
    arm64|aarch64) cpu="aarch64" ;;
    *) echo "不支持的架构: $arch" >&2; exit 1 ;;
  esac
  target="${cpu}-${plat}"
  base="https://github.com/$REPO/releases"
  url="$base/latest/download/${BIN}-${target}.tar.gz"
  [ "$VERSION" = "latest" ] || url="$base/download/${VERSION}/${BIN}-${target}.tar.gz"

  tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
  echo "↓ 下载 $url"
  if command -v curl >/dev/null 2>&1; then curl -fSL "$url" -o "$tmp/a.tgz"
  elif command -v wget >/dev/null 2>&1; then wget -qO "$tmp/a.tgz" "$url"
  else echo "需要 curl 或 wget" >&2; exit 1; fi
  tar -xzf "$tmp/a.tgz" -C "$tmp"
  install_file "$(find "$tmp" -name "$BIN" -type f | head -1)"
fi

# PATH 提示
case ":$PATH:" in
  *":$BIN_DIR:"*) echo "现在可直接运行: $BIN" ;;
  *)
    echo ""
    echo "⚠ $BIN_DIR 不在 PATH。加入(选你的 shell):"
    echo "  echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.bashrc   # bash"
    echo "  echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.zshrc    # zsh"
    echo "  然后重开终端,或 source 之。"
    ;;
esac

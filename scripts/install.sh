#!/bin/sh
# RidgeCode 安装器(Unix:Linux / macOS)—— 零 cargo、零源码,只需一个独立二进制。
#
#   在线装最新版:  curl -fsSL https://raw.githubusercontent.com/MySetsuna/ridge-code/main/scripts/install.sh | sh
#   指定版本:      curl -fsSL https://raw.githubusercontent.com/MySetsuna/ridge-code/v0.5.22/scripts/install.sh | sh -s -- --version v0.5.22
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

download() {
  if command -v curl >/dev/null 2>&1; then
    curl -fSL "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$2" "$1"
  else
    echo "需要 curl 或 wget" >&2
    exit 1
  fi
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
  download "$url" "$tmp/a.tgz"
  checksum_url="${url%.tar.gz}.sha256"
  download "$checksum_url" "$tmp/a.sha256"
  expected="$(awk 'NF {print $1; exit}' "$tmp/a.sha256" | tr 'A-F' 'a-f')"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$tmp/a.tgz" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$tmp/a.tgz" | awk '{print $1}')"
  else
    echo "需要 sha256sum 或 shasum 以校验 Release 归档" >&2
    exit 1
  fi
  if [ "$expected" != "$actual" ]; then
    echo "SHA256 校验失败: expected=$expected actual=$actual" >&2
    exit 1
  fi
  echo "✓ SHA256 $actual"
  tar -xzf "$tmp/a.tgz" -C "$tmp"
  exe="$(find "$tmp" -name "$BIN" -type f | head -1)"
  [ -n "$exe" ] || { echo "归档里没找到 $BIN" >&2; exit 1; }
  install_file "$exe"
fi

# ---- 配置骨架:写 config.example.json 到安装目录 + 首次生成 ~/.ridge/config.json ----
EXAMPLE='{
  "provider": "openai",
  "model": "glm-4.6",
  "base_url": "https://open.bigmodel.cn/api/paas/v4",
  "api_key": "把你的 API Key 明文填这里即可直接启动;不想明文就删掉此行,改为设 RIDGE_API_KEY 环境变量",
  "budget_tokens": 200000,
  "skip_danger": false,
  "providers": [
    {
      "name": "kimi",
      "kind": "openai",
      "model": "kimi-k2",
      "base_url": "https://api.moonshot.cn/v1",
      "key_env": "MOONSHOT_KEY"
    }
  ],
  "mcp": []
}'
printf '%s\n' "$EXAMPLE" > "$BIN_DIR/config.example.json"
echo "✓ 示例配置: $BIN_DIR/config.example.json"

CFG="${RIDGE_CONFIG:-$HOME/.ridge/config.json}"
mkdir -p "$(dirname "$CFG")"
if [ ! -f "$CFG" ]; then
  printf '%s\n' '{
  "provider": "openai",
  "model": "glm-4.6",
  "base_url": "https://open.bigmodel.cn/api/paas/v4",
  "api_key": "",
  "budget_tokens": 200000,
  "skip_danger": false,
  "providers": [],
  "mcp": []
}' > "$CFG"
  echo "✓ 已生成配置: $CFG  —— 填顶层 api_key(或设 RIDGE_API_KEY)即可启动真实 LLM"
else
  echo "已有配置,未改动: $CFG(参照 $BIN_DIR/config.example.json 补 api_key)"
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

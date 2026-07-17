#!/bin/sh
# 本地打包 RidgeCode 当前平台的独立分发产物(Unix:Linux / macOS)。
# cargo build --release 后,把 ridgecode + README + install.sh 打成
# dist/ridgecode-<target>.tar.gz 并生成 .sha256。跨平台产物由 CI 出(打 v* 标签)。
set -eu
root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-dist}"

target="$(rustc -vV | sed -n 's/^host: //p')"
name="ridgecode-$target"
echo "构建 release（$target）…"
( cd "$root" && cargo build --release --locked --bin ridgecode -p agent )

stage="$(mktemp -d)/$name"; mkdir -p "$stage"
cp "$root/target/release/ridgecode" "$stage/"
cp "$root/README.md" "$stage/"
cp "$root/scripts/install.sh" "$stage/"
chmod +x "$stage/ridgecode" "$stage/install.sh"

mkdir -p "$root/$out"
tar="$root/$out/$name.tar.gz"
( cd "$(dirname "$stage")" && tar -czf "$tar" "$name" )

# SHA256(GNU/macOS 兼容)
if command -v sha256sum >/dev/null 2>&1; then
  ( cd "$root/$out" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256" )
else
  ( cd "$root/$out" && shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256" )
fi
rm -rf "$(dirname "$stage")"
echo "[OK] $tar"
echo "     $tar.sha256"

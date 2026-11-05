#!/usr/bin/env bash
# 重新下载表情素材（Twemoji 72×72 PNG，CC-BY 4.0）。
#
# 只在需要增删表情时跑：素材已经提交在 crates/gui/assets/emoji/ 下，
# 平时构建不需要网络。改完这个脚本后记得同步生成 emoji_data.rs：
#   bash scripts/fetch-emoji.sh
set -euo pipefail

cd "$(dirname "$0")/.."
OUT="crates/gui/assets/emoji"
mkdir -p "$OUT"

# 码位 字符（顺序即界面上的排列顺序，8 个一行）
LIST=$(cat <<'EOF'
1f600 😀
1f604 😄
1f601 😁
1f606 😆
1f605 😅
1f602 😂
1f642 🙂
1f643 🙃
1f609 😉
1f60a 😊
1f970 🥰
1f60d 😍
1f618 😘
1f61c 😜
1f914 🤔
1f928 🤨
1f610 😐
1f634 😴
1f62a 😪
1f62d 😭
1f624 😤
1f621 😡
1f973 🥳
1f92f 🤯
1f60e 😎
1f97a 🥺
1f633 😳
1f917 🤗
1f44d 👍
1f44e 👎
1f44c 👌
1f44f 👏
1f64c 🙌
1f64f 🙏
1f4aa 💪
1f91d 🤝
1f440 👀
2764 ❤
1f494 💔
1f525 🔥
2728 ✨
1f389 🎉
1f381 🎁
2615 ☕
1f375 🍵
1f37a 🍺
1f35c 🍜
1f319 🌙
2600 ☀
2b50 ⭐
2705 ✅
274c ❌
2753 ❓
2757 ❗
1f4af 💯
1f680 🚀
1f436 🐶
1f431 🐱
1f4a1 💡
1f3b5 🎵
EOF
)

BASE="https://cdn.jsdelivr.net/gh/twitter/twemoji@14.0.2/assets/72x72"
fail=0
while read -r code ch; do
  [ -z "$code" ] && continue
  out="$OUT/$code.png"
  for name in "$code" "$code-fe0f"; do
    code_http=$(curl -sS -m 30 -o "$out" -w "%{http_code}" "$BASE/$name.png")
    if [ "$code_http" = "200" ] && [ "$(head -c 4 "$out" | od -An -tx1 | tr -d ' \n')" = "89504e47" ]; then
      break
    fi
    rm -f "$out"
  done
  if [ ! -s "$out" ]; then
    echo "缺失：$code $ch"
    fail=$((fail + 1))
  fi
done <<< "$LIST"

if [ "$fail" -ne 0 ]; then
  echo "有 $fail 个表情没下载成功" >&2
  exit 1
fi

# 生成 Rust 素材表
{
  echo '//! 表情素材表（由 `scripts/fetch-emoji.sh` 生成，请勿手改）。'
  echo '//!'
  echo '//! 图片来自 Twemoji 14.0.2（Twitter, Inc. 及贡献者），授权 CC-BY 4.0，'
  echo '//! 署名见 `assets/emoji/ATTRIBUTION.md`。'
  echo ''
  echo 'pub const EMOJI: &[(&str, &[u8])] = &['
  while read -r code ch; do
    [ -z "$code" ] && continue
    printf '    ("%s", include_bytes!("../assets/emoji/%s.png")),\n' "$ch" "$code"
  done <<< "$LIST"
  echo '];'
} > crates/gui/src/emoji_data.rs

echo "完成：$(ls "$OUT"/*.png | wc -l) 张，$(du -sh "$OUT" | cut -f1)"

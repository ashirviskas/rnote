#!/usr/bin/env bash
# Trains the zhuyin columns with the fonts listed in README.md. Arguments are passed on (for example --cache DIR).
# FONTS is the directory the downloaded fonts are in.
set -euo pipefail
cd "$(dirname "$0")/../.."
F="${FONTS:-tmp/fonts}"; S=/usr/share/fonts; R=crates/rnote-engine/data/fonts; M=crates/rnote-ocr/models
exec python misc/zhuyin/train_head.py \
  --model $M/pp-ocrv5_mobile_rec.onnx --dict $M/ppocrv5_dict.txt --text-dir crates/rnote-ui/po \
  --fonts $F/Iansui-Regular.ttf $F/LXGWWenKaiTC-Regular.ttf $F/NotoSansCJKtc.ttf $F/NotoSerifCJKtc.ttf \
          $F/ChenYuluoyan-nohint.ttf \
          $S/abattis-cantarell-fonts/Cantarell-Regular.otf $S/adwaita-sans-fonts/AdwaitaSans-Regular.ttf \
          $S/google-carlito-fonts/Carlito-Regular.ttf $S/google-noto/NotoSans-Regular.ttf \
          $S/google-noto/NotoSerif-Regular.ttf $S/adobe-source-code-pro-fonts/SourceCodePro-Regular.otf \
          $R/GrapeNuts-Regular.ttf $R/TT2020Base-Regular.ttf \
  --test-fonts $F/jf-openhuninn-2.1.ttf $S/liberation-serif-fonts/LiberationSerif-Regular.ttf \
          $S/open-sans/OpenSans-Regular.ttf $S/google-crosextra-caladea-fonts/Caladea-Regular.ttf $R/Virgil.ttf \
  "$@"

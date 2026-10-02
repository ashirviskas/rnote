# Zhuyin for the recognition model

The PP-OCRv5 mobile recognition model has no classes for zhuyin (bopomofo): none of the 37 symbols and none of the tone
marks are in its dictionary, so it reads them as look-alike characters (`ㄅ` as `ク`, `ㄉ` as `分`, `ㄙ` as `ム`).
`train_head.py` adds them without retraining the network. It runs on a CPU.

## How

The model ends in a single linear layer: 120 features per frame go to one score per class, followed by a softmax.

- Everything before that layer stays frozen and the 18,385 existing class columns stay bit-for-bit as they are.
- 41 new columns are learned on rendered lines: 37 symbols and the tone marks `ˊ ˇ ˋ ˙`.
- Where the frozen model reads something inside a zhuyin glyph, that frame is trained towards the zhuyin class. Every
  other frame is trained to keep the class the model already gives it. Those are the lines without zhuyin: sentences
  in many languages from the translation files, look-alike characters, and lines that run through the whole
  dictionary, so the new classes learn to stay quiet on every character the model knows.
- The result is the same model file with a second output, `zhuyin`: the probabilities including the new classes. The
  first output is not touched; on real lines it is exactly equal to the published model's (largest difference 0.0).

`rnote-cli index --zhuyin`, or the experimental setting in the app, reads the second output. Without it nothing
changes.

## Running it

```bash
python -m venv venv && . venv/bin/activate
pip install numpy onnx onnxruntime fonttools pillow
pip install torch --index-url https://download.pytorch.org/whl/cpu

FONTS=<directory with the fonts below> misc/zhuyin/run.sh --out out/ --cache cache/
cp out/pp-ocrv5_mobile_rec.onnx out/zhuyin_dict.txt crates/rnote-ocr/models/
```

`--model` has to be the model as published (see `crates/rnote-ocr/models/README.md` for its source), not a file this
script has already written. Rendering the 17,000 training lines and the test lines takes about an hour on a 4-core
laptop; with them cached, training and testing take 2 minutes and 2.2 GB of memory. Afterwards raise `Index::VERSION`
in `crates/rnote-ocr/src/index.rs`, so existing indexes are built anew.

Fonts, all under the SIL Open Font License. `run.sh` holds the paths that were used, the system ones being Fedora's.

| Used for | Fonts |
|---|---|
| Training, with zhuyin | [Iansui](https://github.com/ButTaiwan/iansui) 1.020, [LXGW WenKai TC](https://github.com/lxgw/LxgwWenkaiTC) 1.522, Noto Sans CJK TC, Noto Serif CJK TC, [ChenYuluoyan](https://github.com/Chenyu-otf/chenyuluoyan_thin) 2.0 Thin (a handwriting font) |
| Training, other text only | Cantarell, Adwaita Sans, Carlito, Noto Sans, Noto Serif, Source Code Pro, and Grape Nuts and TT2020 from this repository |
| Testing only | [jf open-huninn](https://github.com/justfont/open-huninn-font) 2.1 (has zhuyin), Liberation Serif, Open Sans, Caladea, and Virgil from this repository |

Two fonts needed preparing: the Noto CJK TC faces were written out of their `.ttc` collections as single files with
`fontTools`, and ChenYuluoyan had its TrueType hinting removed, as FreeType refuses its hinting program.

## Results

Fonts and sentences that were not trained on, margin 1.0, 2 epochs.

| Lines | Before | With the `zhuyin` output |
|---|---|---|
| Zhuyin lines: character error rate | 72.5% | 11.0% |
| Zhuyin symbols read as the first reading | 0% | 92.3% |
| Zhuyin symbols among the kept readings | 0% | 98.0% |
| Sentences in other languages, 32,671 characters: first reading becomes a zhuyin symbol | – | 122 (0.37%) |
| … of those, the old reading is not kept either | – | 6 (0.02%) |
| Every character of the dictionary, 18,791 characters: first reading becomes a zhuyin symbol | – | 38 (0.20%) |
| … of those, the old reading is not kept either | – | 4 (0.02%) |

The recogniser keeps up to five readings per character and search matches on any of them. A character whose first
reading turns into a zhuyin symbol is therefore still found, unless its old reading drops out of the kept ones too.

Real lines, none of them used for training:

- 42 handwritten zhuyin symbols (seven practice rows from a real note): 0 right before, 38 as the first reading and
  all 42 among the kept readings after.
- 187 lines cut from real pages (printed Traditional Chinese and English, English handwriting): 7 read differently.
  Five of them are lines with ruby zhuyin, where junk characters turn into zhuyin symbols. The other two are
  handwritten lines: a leading `-` becomes `ㄧ`, and a `&` in an already misread word becomes `ㄆ`.

## What decided the settings

- **Only frames where the frozen model fires.** An earlier version also taught the new classes to fire in the middle
  of zhuyin glyphs the model reads nothing for. They then fired on blank frames of ordinary text: 52 of the 187 real
  lines changed, with stray symbols inserted into English sentences. The price is that such glyphs stay unread, in
  practice the neutral tone dot `˙`.
- **Lines through the whole dictionary.** Without them, characters that were rare in the training sentences were
  taken over (`使` read as `ㄑ`, `國` as `ㄖ`).
- **Few epochs.** The longer the columns are trained, the larger their weights and the more they fire on fonts they
  have not seen: 8 epochs changed 0.45% of the characters of unseen fonts where 2 epochs changed 0.35%, and got only
  22 of the 42 handwritten symbols right at the same margin, against 38.
- **Margin 1.0.** A zhuyin class has to beat the existing classes by this much. Without it about twice as many
  ordinary characters are taken over; larger margins cost handwritten symbols quickly.
- A head that also looks at neighbouring frames was tried and was no better on real lines.

The settings were chosen while looking at the test fonts, the real lines and the handwritten rows. The numbers above
are therefore optimistic, and the real sets are small.

## Limits

- Vertical zhuyin is not read. The model reads a line frame by frame from left to right; a vertical stack shares its
  frames. The small ruby zhuyin printed beside characters is such a stack: it comes out as one zhuyin symbol after
  most characters (`老ㄌ師ㄕ`), not as the syllable. Search skips those symbols for queries without zhuyin.
- Symbols that are the same shape as a character cannot be told apart without context: `ㄧ` and `一`, `ㄚ` and `Y`,
  `ㄨ` and `X`, `ㄒ` and `T`. Which one comes first depends on the neighbours; the other one is usually kept as a
  second reading.
- No handwritten zhuyin was used for training, only fonts.

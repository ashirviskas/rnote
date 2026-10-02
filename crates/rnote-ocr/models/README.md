# Text recognition models

PP-OCRv5 mobile models by the PaddleOCR authors, converted to ONNX by the RapidOCR project. Both are licensed under the
Apache License 2.0, see [LICENSE](LICENSE). The files are renamed. The detection model and the dictionary are otherwise
unmodified.

The recognition model has one change: a second output, `zhuyin`, with 41 extra classes for zhuyin symbols and tone
marks, listed in `zhuyin_dict.txt`. Its first output and all of its published weights are unchanged. How the extra
classes were made: [misc/zhuyin](../../../misc/zhuyin/README.md).

They are compiled into `rnote-cli` (see `src/recognize.rs`).

| File | Source (`https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.4.0/…`) | SHA-256 |
|---|---|---|
| `pp-ocrv5_mobile_det.onnx` | `onnx/PP-OCRv5/det/ch_PP-OCRv5_mobile_det.onnx` | `4d97c44a20d30a81aad087d6a396b08f786c4635742afc391f6621f5c6ae78ae` |
| `pp-ocrv5_mobile_rec.onnx` | `onnx/PP-OCRv5/rec/ch_PP-OCRv5_rec_mobile_infer.onnx`, as published: `5825fc7ebf84ae7a412be049820b4d86d77620f204a041697b0494669b1742c5` | `4c79768213f9d7a7d5ab521b31214017258ee7e497985f1d9534d670bfd3d62d` |
| `ppocrv5_dict.txt` | `paddle/PP-OCRv5/rec/ch_PP-OCRv5_rec_mobile_infer/ppocrv5_dict.txt` | `d1979e9f794c464c0d2e0b70a7fe14dd978e9dc644c0e71f14158cdf8342af1b` |
| `zhuyin_dict.txt` | written by `misc/zhuyin/train_head.py` | `690df421c459270d1877f5289daebd7fb94194b439f153da2983e18a9ac17f54` |

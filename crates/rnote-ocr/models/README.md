# Text recognition models

PP-OCRv5 mobile models by the PaddleOCR authors, converted to ONNX by the RapidOCR project. Both are licensed under the
Apache License 2.0, see [LICENSE](LICENSE). The files are unmodified, only renamed.

They are compiled into `rnote-cli` (see `src/recognize.rs`).

| File | Source (`https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.4.0/…`) | SHA-256 |
|---|---|---|
| `pp-ocrv5_mobile_det.onnx` | `onnx/PP-OCRv5/det/ch_PP-OCRv5_mobile_det.onnx` | `4d97c44a20d30a81aad087d6a396b08f786c4635742afc391f6621f5c6ae78ae` |
| `pp-ocrv5_mobile_rec.onnx` | `onnx/PP-OCRv5/rec/ch_PP-OCRv5_rec_mobile_infer.onnx` | `5825fc7ebf84ae7a412be049820b4d86d77620f204a041697b0494669b1742c5` |
| `ppocrv5_dict.txt` | `paddle/PP-OCRv5/rec/ch_PP-OCRv5_rec_mobile_infer/ppocrv5_dict.txt` | `d1979e9f794c464c0d2e0b70a7fe14dd978e9dc644c0e71f14158cdf8342af1b` |

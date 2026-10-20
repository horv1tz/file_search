#!/usr/bin/env python3
"""Скачивает модели распознавания текста (PaddleOCR PP-OCRv5, Apache-2.0) в assets/ocr/ и проверяет SHA-256.

Модели встраиваются в exe при сборке (см. build.rs), поэтому в репозитории хранится только словарь. Без моделей
программа собирается и работает, но распознавание сканов выключено (или читает модели из папки данных, см. README).
Запуск: python3 scripts/fetch_ocr_models.py
"""
import hashlib
import os
import sys
import urllib.request

BASE = "https://huggingface.co/PaddlePaddle"
FILES = {
    "det.onnx": (
        f"{BASE}/PP-OCRv5_mobile_det_onnx/resolve/main/inference.onnx",
        "a431985659dc921974177a95adcfbb90fd9e51989a5e04d70d0b75f597b6e61d",
    ),
    "rec.onnx": (
        f"{BASE}/cyrillic_PP-OCRv5_mobile_rec_onnx/resolve/main/inference.onnx",
        "5371ee1ddaa7983cc62d0818d99e982b6804638c85e4f960d59a574094e172e5",
    ),
}


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main():
    out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "assets", "ocr")
    os.makedirs(out, exist_ok=True)
    for name, (url, expected) in FILES.items():
        path = os.path.join(out, name)
        if os.path.exists(path) and sha256(path) == expected:
            print(f"{name}: уже есть")
            continue
        print(f"{name}: загрузка {url}")
        last = None
        for attempt in range(4):
            try:
                tmp = path + ".part"
                with urllib.request.urlopen(url, timeout=120) as r, open(tmp, "wb") as f:
                    while True:
                        chunk = r.read(1 << 20)
                        if not chunk:
                            break
                        f.write(chunk)
                os.replace(tmp, path)
                last = None
                break
            except Exception as e:  # сеть нестабильна: повторяем с паузой
                last = e
                import time

                time.sleep(2 ** attempt)
        if last is not None:
            sys.exit(f"{name}: не удалось скачать: {last}")
        actual = sha256(path)
        if actual != expected:
            os.remove(path)
            sys.exit(f"{name}: контрольная сумма не совпала ({actual}), файл удалён")
        print(f"{name}: готово ({os.path.getsize(path)} байт)")


if __name__ == "__main__":
    main()

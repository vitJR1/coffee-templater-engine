"""
Derives broken.docx (compile-error fixture) from invoice.docx using only the standard library:
the first {{/items}} in the body is removed (unclosed section) and the header uses an unknown
filter.

    python3 conformance/fixtures/make_broken.py
"""
import os
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))

with zipfile.ZipFile(os.path.join(HERE, "invoice.docx")) as src, zipfile.ZipFile(
    os.path.join(HERE, "broken.docx"), "w", zipfile.ZIP_DEFLATED
) as dst:
    for item in src.infolist():
        data = src.read(item.filename)
        if item.filename == "word/document.xml":
            data = data.replace(b"{{/items}}", b"", 1)
        elif item.filename.startswith("word/header") and item.filename.endswith(".xml"):
            data = data.replace(b"{{company}}", b"{{company | nope}}")
        info = zipfile.ZipInfo(item.filename, date_time=(1980, 1, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        dst.writestr(info, data)
print("broken.docx written to", HERE)

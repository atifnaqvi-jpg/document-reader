#!/usr/bin/env python3
"""Generate a small multi-page, text-layer PDF fixture.

Hand-built rather than produced by a PDF library: the fixture must be tiny, committed,
and stable byte-for-byte, and it needs a real text layer (not just a scanned image) so
PdfProvider::extract_text has something to find. Each page carries a distinct token
(ALPHA/BRAVO/CHARLIE) so per-page extraction can be asserted unambiguously.

Not a build dependency — run manually if the fixture ever needs regenerating.
"""
import pathlib

PAGES = [
    ("ALPHA", "Document Reader P1 fixture, page one."),
    ("BRAVO", "Second page mentions zephyrium so search can find it."),
    ("CHARLIE", "Third and final page of the fixture."),
]


def build() -> bytes:
    objects: list[bytes] = []

    # Placeholder ids resolved after we know the count.
    font_id = 3
    page_ids = [4 + i * 2 for i in range(len(PAGES))]

    # 1: Catalog, 2: Pages
    objects.append(b"<< /Type /Catalog /Pages 2 0 R >>")
    kids = " ".join(f"{pid} 0 R" for pid in page_ids)
    objects.append(
        f"<< /Type /Pages /Kids [{kids}] /Count {len(PAGES)} >>".encode()
    )
    objects.append(
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
    )

    for index, (token, sentence) in enumerate(PAGES):
        page_id = page_ids[index]
        content_id = page_id + 1
        objects.append(
            (
                f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
                f"/Resources << /Font << /F1 {font_id} 0 R >> >> "
                f"/Contents {content_id} 0 R >>"
            ).encode()
        )
        stream = (
            f"BT /F1 24 Tf 72 700 Td ({token}) Tj ET\n"
            f"BT /F1 12 Tf 72 660 Td ({sentence}) Tj ET\n"
        ).encode()
        objects.append(
            b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream
            + b"endstream"
        )

    out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
    offsets = [0]
    for number, body in enumerate(objects, start=1):
        offsets.append(len(out))
        out += f"{number} 0 obj\n".encode() + body + b"\nendobj\n"

    xref_at = len(out)
    count = len(objects) + 1
    out += f"xref\n0 {count}\n".encode()
    out += b"0000000000 65535 f \n"
    for offset in offsets[1:]:
        out += f"{offset:010d} 00000 n \n".encode()
    out += (
        f"trailer\n<< /Size {count} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n"
    ).encode()
    return bytes(out)


if __name__ == "__main__":
    import sys
    target = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else pathlib.Path("tests/fixtures/sample.pdf")
    target.write_bytes(build())
    print(f"wrote {target} ({target.stat().st_size} bytes)")

#!/usr/bin/env python3
"""Generate enterprise document fixtures for the file-parser integration tests.

Produces one sample per supported format family (DOCX, XLSX, PPTX, ODT, RTF,
CSV, EPUB) so the parser has at least one real exercise of every container
code path. The content is plain ASCII; the goal is to validate the conversion
end-to-end, not to look pretty.
"""
from __future__ import annotations

import csv
import struct
import sys
import zipfile
from pathlib import Path

FIXTURE_DIR = Path(__file__).resolve().parent
SAMPLES = FIXTURE_DIR / "samples"
SAMPLES.mkdir(exist_ok=True)


def write_docx() -> Path:
    """Create a minimal DOCX containing a heading, paragraph, and a table."""
    path = SAMPLES / "company-handbook.docx"
    document_xml = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr>
      <w:r><w:t>Ventorium Employee Handbook</w:t></w:r>
    </w:p>
    <w:p><w:r><w:t>Welcome to the engineering team. This handbook covers onboarding, security, and PTO policy.</w:t></w:r></w:p>
    <w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr>
      <w:r><w:t>Onboarding Checklist</w:t></w:r>
    </w:p>
    <w:tbl>
      <w:tblPr><w:tblW w:w="5000" w:type="pct"/></w:tblPr>
      <w:tblGrid><w:gridCol w:w="3000"/><w:gridCol w:w="6000"/></w:tblGrid>
      <w:tr><w:tc><w:p><w:r><w:t>Day 1</w:t></w:r></w:p></w:tc>
              <w:tc><w:p><w:r><w:t>Sign NDA, request laptop, meet manager.</w:t></w:r></w:p></w:tc></w:tr>
      <w:tr><w:tc><w:p><w:r><w:t>Week 1</w:t></w:r></w:p></w:tc>
              <w:tc><w:p><w:r><w:t>Complete security training, set up 2FA, join on-call rotation.</w:t></w:r></w:p></w:tc></w:tr>
    </w:tbl>
  </w:body>
</w:document>
"""
    content_types = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>
"""
    rels = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>
"""
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("[Content_Types].xml", content_types)
        zf.writestr("_rels/.rels", rels)
        zf.writestr("word/document.xml", document_xml)
    return path


def write_xlsx() -> Path:
    """Create a minimal XLSX with one worksheet of quarterly revenue data."""
    path = SAMPLES / "quarterly-revenue.xlsx"
    sheet_xml = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1"><c r="A1" t="inlineStr"><is><t>Quarter</t></is></c><c r="B1" t="inlineStr"><is><t>Revenue</t></is></c></row>
    <row r="2"><c r="A2" t="inlineStr"><is><t>Q1</t></is></c><c r="B2"><v>1200000</v></c></row>
    <row r="3"><c r="A3" t="inlineStr"><is><t>Q2</t></is></c><c r="B3"><v>1450000</v></c></row>
    <row r="4"><c r="A4" t="inlineStr"><is><t>Q3</t></is></c><c r="B4"><v>1620000</v></c></row>
    <row r="5"><c r="A5" t="inlineStr"><is><t>Q4</t></is></c><c r="B5"><v>1980000</v></c></row>
  </sheetData>
</worksheet>
"""
    workbook_xml = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets><sheet name="Revenue" sheetId="1" r:id="rId1"/></sheets>
</workbook>
"""
    workbook_rels = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>
"""
    content_types = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>
"""
    rels = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>
"""
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("[Content_Types].xml", content_types)
        zf.writestr("_rels/.rels", rels)
        zf.writestr("xl/workbook.xml", workbook_xml)
        zf.writestr("xl/_rels/workbook.xml.rels", workbook_rels)
        zf.writestr("xl/worksheets/sheet1.xml", sheet_xml)
    return path


def write_pptx() -> Path:
    """Create a minimal PPTX with two slides."""
    path = SAMPLES / "quarterly-review.pptx"
    slide_xml = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
       xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <p:cSld><p:spTree>
    <p:sp><p:nvSpPr><p:cNvPr id="1" name="Title"/><p:cNvSpPr/></p:nvSpPr>
      <p:spPr/><p:txBody>
        <a:p><a:r><a:t>{title}</a:t></a:r></a:p>
      </p:txBody></p:sp>
    <p:sp><p:nvSpPr><p:cNvPr id="2" name="Body"/><p:cNvSpPr/></p:nvSpPr>
      <p:spPr/><p:txBody>
        <a:p><a:r><a:t>{body}</a:t></a:r></a:p>
      </p:txBody></p:sp>
  </p:spTree></p:cSld>
</p:sld>
"""
    slides = [
        ("Quarterly Review", "Highlights from Q1 2026."),
        ("Roadmap", "Ship the new search ranking by end of Q2."),
    ]
    content_types = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
"""
    slides_xml = []
    for idx in range(1, len(slides) + 1):
        content_types += f'  <Override PartName="/ppt/slides/slide{idx}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>\n'
    content_types += "</Types>\n"
    presentation_rels = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
"""
    for idx in range(1, len(slides) + 1):
        presentation_rels += (
            f'  <Relationship Id="rId{idx + 1}" '
            f'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" '
            f'Target="slides/slide{idx}.xml"/>\n'
        )
    presentation_rels += "</Relationships>\n"
    rels_root = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>
"""
    presentation = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"
                xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <p:sldIdLst>"""
    for idx in range(1, len(slides) + 1):
        presentation += f'<p:sldId id="{256 + idx}" r:id="rId{idx + 1}"/>'
    presentation += """</p:sldIdLst></p:presentation>
"""
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("[Content_Types].xml", content_types)
        zf.writestr("_rels/.rels", rels_root)
        zf.writestr("ppt/_rels/presentation.xml.rels", presentation_rels)
        zf.writestr("ppt/presentation.xml", presentation)
        for idx, (title, body) in enumerate(slides, start=1):
            zf.writestr(f"ppt/slides/slide{idx}.xml", slide_xml.format(title=title, body=body))
    return path


def write_csv() -> Path:
    """Create a CSV with a small employee directory."""
    path = SAMPLES / "employee-directory.csv"
    with path.open("w", newline="", encoding="utf-8") as fh:
        writer = csv.writer(fh)
        writer.writerow(["name", "title", "team", "location"])
        writer.writerow(["Ada Lovelace", "Principal Engineer", "Platform", "Remote"])
        writer.writerow(["Grace Hopper", "Distinguished Engineer", "Compiler", "New York"])
        writer.writerow(["Linus Torvalds", "Maintainer", "Kernel", "Portland"])
    return path


def write_rtf() -> Path:
    """Create a minimal RTF document with one paragraph."""
    path = SAMPLES / "memo.rtf"
    path.write_text(
        r"{\rtf1\ansi\ansicpg1252\deff0"
        r"{\fonttbl{\f0 Helvetica;}}"
        r"\f0\fs24 "
        r"Engineering Memo\par "
        r"All hands on Friday at 3pm to discuss the migration timeline.}"
    )
    return path


def write_odt() -> Path:
    """Create a minimal OpenDocument Text file."""
    path = SAMPLES / "release-notes.odt"
    content_xml = """<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"
                          xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"
                          office:version="1.2">
  <office:body><office:text>
    <text:h>Release Notes 2026.04</text:h>
    <text:p>Adds dark mode, improves OCR throughput, and ships the new agent sandbox.</text:p>
  </office:text></office:body>
</office:document-content>
"""
    manifest_xml = """<?xml version="1.0" encoding="UTF-8"?>
<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"
                  manifest:version="1.2">
  <manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.text"/>
  <manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>
</manifest:manifest>
"""
    mimetype = "application/vnd.oasis.opendocument.text"
    with zipfile.ZipFile(path, "w", zipfile.ZIP_STORED) as zf:
        zf.writestr("mimetype", mimetype)
        zf.writestr("META-INF/manifest.xml", manifest_xml)
        zf.writestr("content.xml", content_xml)
    return path


def write_epub() -> Path:
    """Create a minimal EPUB 3 with a single chapter."""
    path = SAMPLES / "field-guide.epub"
    mimetype = "application/epub+zip"
    container = """<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>
"""
    opf = """<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bookid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="bookid">urn:ventorium:field-guide</dc:identifier>
    <dc:title>Field Guide</dc:title>
    <dc:language>en</dc:language>
  </metadata>
  <manifest>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="ch1"/></spine>
</package>
"""
    ch1 = """<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml">
  <head><title>Chapter 1</title></head>
  <body>
    <h1>Chapter 1: Onboarding</h1>
    <p>Welcome to the field guide. This chapter covers the first week of onboarding.</p>
  </body>
</html>
"""
    with zipfile.ZipFile(path, "w", zipfile.ZIP_STORED) as zf:
        zf.writestr("mimetype", mimetype)
        zf.writestr("META-INF/container.xml", container)
        zf.writestr("OEBPS/content.opf", opf)
        zf.writestr("OEBPS/ch1.xhtml", ch1)
    return path


def main() -> int:
    paths = [
        write_docx(),
        write_xlsx(),
        write_pptx(),
        write_csv(),
        write_rtf(),
        write_odt(),
        write_epub(),
    ]
    for p in paths:
        size = p.stat().st_size
        print(f"  {p.name:30s} {size:>8d} bytes")
    print(f"wrote {len(paths)} fixtures to {SAMPLES}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

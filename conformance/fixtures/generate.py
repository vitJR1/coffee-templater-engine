"""
Generates the template fixtures used by the test-suite with real Office libraries.

    python -m venv .venv && .venv/bin/pip install python-docx openpyxl python-pptx
    .venv/bin/python test/fixtures/generate.py
"""
import os

from docx import Document
from docx.shared import Pt
from openpyxl import Workbook
from pptx import Presentation
from pptx.util import Inches

HERE = os.path.dirname(os.path.abspath(__file__))


def make_docx():
    doc = Document()
    doc.sections[0].header.paragraphs[0].text = "{{company}} — confidential"
    doc.add_heading("Invoice {{number}}", level=1)

    # A tag split across runs with different formatting, as Word often does.
    p = doc.add_paragraph("Client: ")
    r = p.add_run("{{cli")
    r.bold = True
    p.add_run("ent.name}}")

    doc.add_paragraph("{{#items}}")
    doc.add_paragraph("- {{name}}: {{price | number: 2}}")
    doc.add_paragraph("{{/items}}")

    table = doc.add_table(rows=2, cols=3)
    for i, h in enumerate(["Name", "Qty", "Price"]):
        table.rows[0].cells[i].text = h
    table.rows[1].cells[0].text = "{{#items}}{{name}}"
    table.rows[1].cells[1].text = "{{qty}}"
    table.rows[1].cells[2].text = "{{price}}{{/items}}"

    doc.add_paragraph("Status: {{#if paid}}paid{{else}}not paid{{/if}}")
    doc.add_paragraph("Escaped: \\{{literal}}")
    doc.add_paragraph("Notes: {{notes}}")
    doc.add_paragraph("{{%logo}}")
    run = doc.add_paragraph().add_run("Total: {{total | currency: \"EUR\", \"de-DE\"}}")
    run.font.size = Pt(14)
    doc.save(os.path.join(HERE, "invoice.docx"))


def make_xlsx():
    wb = Workbook()
    ws = wb.active
    ws.title = "Report"
    ws["A1"] = "Report: {{title}}"
    ws.merge_cells("A1:D1")
    ws.append(["Name", "Qty", "Price", "Sum"])
    ws["A3"] = "{{#items}}{{name}}"
    ws["B3"] = "{{qty}}"
    ws["C3"] = "{{price}}"
    ws["D3"] = "=B3*C3"
    ws["E3"] = "{{/items}}"
    ws["A5"] = "Total"
    ws["D5"] = "=SUM(D3:D3)"
    ws["A6"] = "Escaped \\{{x}}"
    ws2 = wb.create_sheet("Info")
    ws2["A1"] = "Author"
    ws2["B1"] = "{{author}}"
    wb.save(os.path.join(HERE, "report.xlsx"))


def make_pptx():
    prs = Presentation()
    s1 = prs.slides.add_slide(prs.slide_layouts[0])
    s1.shapes.title.text = "{{title}}"
    s1.placeholders[1].text = "{{subtitle}}"

    s2 = prs.slides.add_slide(prs.slide_layouts[6])
    tb = s2.shapes.add_textbox(Inches(1), Inches(1), Inches(5), Inches(3))
    tf = tb.text_frame
    tf.text = "{{#points}}"
    for line in ["• {{.}}", "{{/points}}"]:
        tf.add_paragraph().text = line
    logo = s2.shapes.add_textbox(Inches(6), Inches(1), Inches(3), Inches(2))
    logo.text_frame.text = "{{%logo}}"

    rows = s2.shapes.add_table(2, 2, Inches(1), Inches(4.5), Inches(6), Inches(1)).table
    rows.cell(0, 0).text = "Name"
    rows.cell(0, 1).text = "Value"
    rows.cell(1, 0).text = "{{#rows}}{{name}}"
    rows.cell(1, 1).text = "{{value}}{{/rows}}"
    s2.notes_slide.notes_text_frame.text = "Speaker: {{speaker}}"
    prs.save(os.path.join(HERE, "deck.pptx"))


if __name__ == "__main__":
    make_docx()
    make_xlsx()
    make_pptx()
    print("fixtures written to", HERE)

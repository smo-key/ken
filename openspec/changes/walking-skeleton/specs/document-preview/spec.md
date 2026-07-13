# document-preview

## ADDED Requirements

### Requirement: In-app preview of common formats
Selecting a non-editable file in the Files screen SHALL preview it inside
the app: PDF pages (pdf.js), Word documents rendered as formatted HTML
(mammoth), Excel workbooks as a grid with per-sheet tabs (SheetJS), and
images natively.

#### Scenario: Preview a PDF
- **WHEN** the user selects a .pdf in the tree
- **THEN** its pages render in the content area with scrolling, without
  launching an external app

#### Scenario: Preview an Excel workbook
- **WHEN** the user selects a .xlsx with multiple sheets
- **THEN** a grid of the first sheet renders with tabs to switch sheets

### Requirement: Graceful fallback for other formats
Files without a dedicated preview (including .pptx in this change) SHALL
show file metadata plus any extracted text, and an "Open in default app"
action.

#### Scenario: Unsupported format
- **WHEN** the user selects a .pptx or unknown binary file
- **THEN** the app shows its metadata and extracted text (if any) with an
  "Open in default app" button, and never renders a blank pane

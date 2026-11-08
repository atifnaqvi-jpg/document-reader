# Document Reader — Capability Baseline

> **What this is:** the distilled capability facts that DocuPro's phase designs
> and plans cite as their parity baseline — a broad-surface commercial Windows
> desktop PDF reader/editor. Anchor numbering in this file is **stable and
> load-bearing**: designs cite `§2.x` here, so these anchors never renumber.
>
> The complete, evidence-grade record behind these facts (file inventories,
> string-level observations, endpoint evidence, patch deltas,
> licensing-gate notes) is kept as the product-identified research record in
> the private `docupro-research` repository under the same GitHub account. It
> is deliberately not part of this repository. Observations below were taken
> from completed feature-extraction passes — direct artifact observation,
> never marketing material.

## 2. Feature Catalog (the command/tool surface)

### 2.1 Viewing & reading
- Page display modes: Single Page, Continuous, Facing, Continuous Facing, Full
  Screen; fit-page/fit-width/fit-height/actual sizing; continuous zoom.
- Navigation panes: bookmarks, thumbnails, layers (OCG), attachments,
  signatures, articles, model tree, fields, content.
- Full-text find over indexed and non-indexed documents; search hits surfaced
  in bookmarks, attachments, and annotations.
- Reading aids: text-to-speech read-aloud, an accessibility reflow view,
  reading and full-screen modes, autoscroll.
- Light/dark themes; tabbed multi-document windows; a protected-view sandbox
  for untrusted documents.

### 2.2 Annotations & commenting
- Annotation types: sticky note, highlight, underline, strikeout, squiggly,
  text box, callout, typewriter, ink/pencil, eraser, line, arrow (seven head
  styles), polygon, polyline, rectangle, oval, cloud, stamp (dynamic and
  custom palettes), file attachment, sound, popup/reply chains, caret,
  redaction marks, measurement tools (distance/area/perimeter with snapping),
  connector lines.
- Comment list and summary view; comment search, sort, and filter; a status
  workflow (accepted/rejected/completed/cancelled); printable comment
  summaries; annotation migration between document versions.
- Shared and tracked reviews with deadlines; cloud comment synchronization is
  part of the baseline's online surface (§2.9).

### 2.3 Forms
- Form-field authoring: add/rename/duplicate fields, grid placement,
  tab-order rules (row/column/manual/structure/alpha), calculation order,
  required flags, per-field JavaScript, layout preview.
- Field types: text, checkbox, radio, list, dropdown, signature, barcode
  (PDF417 and QR).
- Data workflows: FDF/XML import and export, response collection with
  spreadsheet export, form distribution and return tracking.

### 2.4 Fill & Sign
- Automatic form-field detection, auto-fill, drawn/typed/image signatures,
  simple text and checkmark additions, and a hand-off to a hosted
  e-signature service.

### 2.5 Digital signatures & security
- Signature formats: PKCS#7 detached (the `/adbe.pkcs7.detached`
  PDF-standard subtype), PAdES profile attributes, X.509 RSA-SHA1
  (`/adbe.x509.rsa_sha1`).
  *Note: `/adbe.*` strings are ISO 32000–registered PDF specification
  identifiers (the PAdES/ESS profiles build on them), not product branding —
  any consumer of this baseline writes these exact dictionary subtypes.*
- Trust model: a vendor-run approved-trust-list program, EU qualified
  trustmarks, OS certificate stores as credential providers, identity
  directory providers, CRL/OCSP revocation checks, and a long-term-validation
  (LTV) provider.
- RFC 3161 timestamping.
- Certify vs sign: visible and placement signatures, invisible certification,
  signature validation badges (valid/modified/problem/unknown), and rollback
  signature states.
- Document protection: password encryption (open + permissions), reusable
  security policies, information-rights management.
- Sanitize / remove hidden information; a redaction tool with mark,
  search-pattern, and apply steps.

### 2.6 JavaScript engine
- Embedded ECMA-derived scripting with a debugger (breakpoints, console),
  document-level scripts, document actions, and action-wizard workflows.
  Host objects expose application, document, event, field, console, and
  utility namespaces; trusted functions exist for elevated operations.
- JavaScript-driven custom actions and batch-processing sequences.

### 2.7 Accessibility
- An auto-tagging engine (with header-detection and structure conversion), a
  tags tree, alternate text, and reading-order tools.
- An accessibility checker with rule profiles and an HTML report; a
  screen-reader simulation mode; form-fields-to-tags conversion.
- Structure-tree and dictionary validation, including preflight-style
  conformance checking (PDF/UA-style assertions).

### 2.8 Tools Center (the tool surface)
Edit PDF (text/image edit, crop, headers/footers, watermark, background,
bookmarks, links, attachments, Bates numbering) · Organize Pages (insert,
delete, rotate, extract, replace, split, page labels) · Combine Files ·
Compress (file-size reduction, an optimizer, scanned-document optimization) ·
Export (to office formats, RTF, HTML/XML — the export path embeds an OCR
engine) · Create PDF (from arbitrary formats, including an HTML-to-PDF
converter) · Scan & OCR (perspective correction, recognized-text correction
passes, batch OCR) · Protect (passwords, policy management, hidden-info
removal) · Redact (mark/apply/search-pattern redaction with sanitize) ·
Certificates (sign/certify/validate/timestamp) · Send for Comments · Send &
Track and Collect Signatures · Index (embedded and collection indexes) ·
Prepare Form · Print Production (object touchup, color conversion, flatten,
PDF/X export, printer marks) · PDF Standards (PDF/A, PDF/X, PDF/E save-as
plus standards preflight) · Rich Media (audio/video/3D annotations with a
JavaScript-accessible 3D engine) · Action Wizard (guided multi-step
workflows, custom tools) · Compare Files (side-by-side with suggestion
merging) · Stamp palettes · Measure · Accessibility tools · Create Custom
Tool · cloud link sharing · mobile hand-off.

### 2.9 Cloud services & connectors
- Identity and licensing services (OAuth delegation, entitlement endpoints,
  trial and in-app purchase surfaces).
- Storage connectors: Dropbox, Google Drive, OneDrive (personal and
  business), Box, SharePoint — a shared account-manager framework.
- Vendor cloud document services (files, collections, link sharing with
  permissions), hosted e-signature (send, in-app signing, status webhooks),
  cloud comment synchronization, in-product messaging and update services.
- **DocuPro charter note:** this entire surface is permanently out of scope
  in this product by frozen decision (docs/DECISIONS.md § 9.1, R3/R4
  amendments); it is catalogued here only to define the baseline's boundary.

### 2.10 OS & office integrations
- Browser integration (extensions with a native messaging host, legacy
  ActiveX embedding), web-capture to PDF.
- Shell integration (thumbnail and preview handlers, property handlers).
- Office add-ins (document-creation from word processor, spreadsheet,
  presentation, and CAD hosts), print-to-PDF pipeline, telemetry client.
- Charter note: not shipped in DocuPro; listed for baseline completeness.

### 2.11 Internationalization
- 26 UI language packs; locale display-name mapping.
- Hunspell spell-check dictionaries with hyphenation; CJK character-map
  resources; embedded brand typography (including a proprietary UI font
  family for form filler appearance).

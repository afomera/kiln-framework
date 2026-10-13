// Kiln's rich text editor: Lexxy (vendored, see crates/kiln/vendor/README.md), configured
// for Kiln. `{{ field(form.body) }}` loads this for `RichText` fields. Lexxy defines its
// elements a tick after it loads, so configuration here applies to every editor.
import { configure } from "lexxy"

configure({
  global: {
    // Kiln's own names for attachments (stored HTML never says "action-text").
    attachmentTagName: "kiln-attachment",
    attachmentContentTypeNamespace: "kiln",
  },
  default: {
    // Off until Kiln has file storage; the server strips attachments regardless.
    attachments: false,
    // Lexxy offers h2-h4; posts often want a top-level heading too.
    headings: ["h1", "h2", "h3", "h4"],
  },
})

// The editor's own controls (its code-language picker) are form elements, so they'd be
// submitted with the form. Leave them out; the server ignores them either way.
document.addEventListener("formdata", (event) => {
  for (const name of [...event.formData.keys()]) {
    if (name.startsWith("lexxy-")) event.formData.delete(name)
  }
})

// Highlights code blocks in rich text shown on a page ({{ post.body | rich_text }} adds
// this when the text has one), with the Prism build inside Lexxy. Runs again after Turbo
// visits, renders and morphs, which replace the page's content.
import { highlightCode } from "lexxy"

const highlight = () => highlightCode()

highlight()
for (const event of ["turbo:load", "turbo:render", "turbo:morph", "turbo:frame-render"]) {
  document.addEventListener(event, highlight)
}

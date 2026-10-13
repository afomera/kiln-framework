// Live reload in development (see crates/kiln/src/dev_reload.rs). Loaded once; the stream
// stays open across Turbo visits.
import * as Turbo from "@hotwired/turbo"

const source = new EventSource("/kiln/dev/reload")
let boot = null

// A new boot id means the server restarted (Rust changed): reload everything.
source.addEventListener("hello", (event) => {
  if (boot && boot !== event.data) location.reload()
  boot = event.data
})

// A stylesheet changed: swap it in place. The new link loads before the old one goes, so
// nothing flashes unstyled.
source.addEventListener("css", (event) => {
  const { name, url } = JSON.parse(event.data)
  const stem = "/assets/" + name.replace(/\.css$/, "")
  for (const link of document.querySelectorAll('link[rel="stylesheet"]')) {
    const path = new URL(link.href, location.href).pathname
    if (path === `/assets/${name}` || (path.startsWith(stem + "-") && path.endsWith(".css"))) {
      const next = link.cloneNode()
      next.href = url
      next.addEventListener("load", () => link.remove(), { once: true })
      link.after(next)
    }
  }
})

// A template (or another asset) changed: refresh through Turbo, keeping the scroll position.
source.addEventListener("html", () => {
  const x = scrollX, y = scrollY
  document.addEventListener("turbo:load", () => scrollTo(x, y), { once: true })
  Turbo.visit(location.href, { action: "replace" })
})

source.addEventListener("reload", () => location.reload())

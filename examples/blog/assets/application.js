// The blog's JavaScript, loaded as an ES module through the import map.
// Turbo is already loaded by `{{ javascript_importmap() }}`; import it here to use its API:
//   import * as Turbo from "@hotwired/turbo"

// Theme switcher: light, dark, or follow the system. The choice lives in localStorage; the
// layout's inline <head> script applies it before first paint, and this keeps the buttons
// in sync (Turbo replaces <body> on each visit but keeps <html> and its data-theme).
const KEY = "theme"

function saved() {
  try { return localStorage.getItem(KEY) || "system" } catch { return "system" }
}

function apply(theme) {
  const root = document.documentElement
  if (theme === "light" || theme === "dark") root.dataset.theme = theme
  else delete root.dataset.theme
  for (const button of document.querySelectorAll("[data-theme-choice]")) {
    button.setAttribute("aria-pressed", String(button.dataset.themeChoice === theme))
  }
}

document.addEventListener("click", (event) => {
  const button = event.target.closest("[data-theme-choice]")
  if (!button) return
  const theme = button.dataset.themeChoice
  try {
    if (theme === "system") localStorage.removeItem(KEY)
    else localStorage.setItem(KEY, theme)
  } catch {}
  apply(theme)
})

document.addEventListener("turbo:load", () => apply(saved()))
apply(saved())

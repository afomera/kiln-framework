// Confirmations for forms and buttons: `form_start(.., confirm="Delete this post?")` or
// `button_to("Delete", path, method="delete", confirm="..")` set `data-turbo-confirm`, and
// Turbo asks before submitting. Kiln answers with a styled <dialog> instead of the
// browser's confirm(). Three ways to make it yours:
//
//   1. CSS: style `.kiln-confirm` and friends. Kiln's defaults live in `@layer kiln`, so
//      any of your own CSS wins without specificity fights.
//   2. Markup: put a <template id="kiln-confirm"> in your layout. Kiln clones it and fills
//      in [data-confirm-message] and the button labels; buttons with value="confirm" and
//      value="cancel" answer.
//   3. JS: `import { setConfirm } from "kiln/confirm"` and pass
//      `async (message, element, submitter) => boolean`.
//
// Per form or button: `data-confirm-button="Delete"` labels the confirm button and
// `data-confirm-tone="danger"` marks it destructive (Cancel gets focus first).
import * as Turbo from "@hotwired/turbo"

const STYLE = `@layer kiln {
  .kiln-confirm {
    --kiln-confirm-bg: Canvas; --kiln-confirm-ink: CanvasText; --kiln-confirm-muted: color-mix(in oklab, CanvasText 60%, Canvas);
    --kiln-confirm-line: color-mix(in oklab, CanvasText 14%, Canvas); --kiln-confirm-accent: oklch(52% 0.15 42); --kiln-confirm-danger: oklch(55% 0.19 27);
    border: 1px solid var(--kiln-confirm-line); border-radius: 14px; padding: 0; width: min(26rem, calc(100vw - 2rem));
    background: var(--kiln-confirm-bg); color: var(--kiln-confirm-ink); font: inherit;
    box-shadow: 0 24px 60px oklch(20% 0.02 60 / 0.28), 0 2px 8px oklch(20% 0.02 60 / 0.12);
  }
  .kiln-confirm[open] { animation: kiln-confirm-in 140ms ease-out; }
  .kiln-confirm::backdrop { background: oklch(20% 0.02 60 / 0.4); backdrop-filter: blur(2px); }
  .kiln-confirm__body { padding: 20px 22px 6px; }
  .kiln-confirm__message { margin: 0; font-size: 1rem; line-height: 1.5; font-weight: 550; }
  .kiln-confirm__actions { display: flex; justify-content: flex-end; gap: 8px; padding: 16px 22px 20px; }
  .kiln-confirm__actions button { font: inherit; font-size: .95rem; padding: .45rem .95rem; border-radius: 8px; cursor: pointer; border: 1px solid var(--kiln-confirm-line); background: transparent; color: inherit; }
  .kiln-confirm__actions button:focus-visible { outline: 2px solid var(--kiln-confirm-accent); outline-offset: 2px; }
  .kiln-confirm__confirm { background: var(--kiln-confirm-accent) !important; border-color: transparent !important; color: white !important; }
  .kiln-confirm--danger .kiln-confirm__confirm { background: var(--kiln-confirm-danger) !important; }
  @keyframes kiln-confirm-in { from { opacity: 0; transform: translateY(6px) scale(.98); } }
  @media (prefers-reduced-motion: reduce) { .kiln-confirm[open] { animation: none; } }
}`

const DEFAULT_MARKUP = `<div class="kiln-confirm__body"><p class="kiln-confirm__message" data-confirm-message></p></div>
<div class="kiln-confirm__actions"><button type="button" value="cancel" class="kiln-confirm__cancel" data-confirm-cancel>Cancel</button><button type="button" value="confirm" class="kiln-confirm__confirm" data-confirm-confirm>Confirm</button></div>`

function attribute(name, ...elements) {
  for (const element of elements) {
    const value = element?.getAttribute?.(name)
    if (value) return value
  }
  return null
}

function ensureStyle() {
  if (document.getElementById("kiln-confirm-style")) return
  const style = document.createElement("style")
  style.id = "kiln-confirm-style"
  style.textContent = STYLE
  document.head.prepend(style)
}

/** Kiln's dialog: resolves true when confirmed. */
export function kilnConfirm(message, element, submitter) {
  ensureStyle()
  const dialog = document.createElement("dialog")
  const template = document.getElementById("kiln-confirm")
  if (template instanceof HTMLTemplateElement) {
    dialog.append(template.content.cloneNode(true))
  } else {
    dialog.innerHTML = DEFAULT_MARKUP
  }
  dialog.classList.add("kiln-confirm")
  const tone = attribute("data-confirm-tone", submitter, element)
  if (tone) dialog.classList.add(`kiln-confirm--${tone}`)
  dialog.setAttribute("aria-modal", "true")

  const messageElement = dialog.querySelector("[data-confirm-message]")
  if (messageElement) {
    messageElement.textContent = message
    messageElement.id ||= `kiln-confirm-${Date.now()}`
    dialog.setAttribute("aria-labelledby", messageElement.id)
  }
  const confirmButton = dialog.querySelector('[value="confirm"]')
  const cancelButton = dialog.querySelector('[value="cancel"]')
  const confirmLabel = attribute("data-confirm-button", submitter, element)
  if (confirmLabel && confirmButton) confirmButton.textContent = confirmLabel
  const cancelLabel = attribute("data-confirm-cancel", submitter, element)
  if (cancelLabel && cancelButton) cancelButton.textContent = cancelLabel

  document.body.append(dialog)
  return new Promise((resolve) => {
    let answered = false
    const answer = (value) => {
      if (answered) return
      answered = true
      dialog.close()
      dialog.remove()
      resolve(value)
    }
    dialog.addEventListener("click", (event) => {
      const button = event.target.closest("button[value]")
      if (button) answer(button.value === "confirm")
      else if (event.target === dialog) answer(false) // a click on the backdrop
    })
    dialog.addEventListener("cancel", (event) => {
      event.preventDefault()
      answer(false)
    })
    dialog.showModal()
    // Destructive actions start on Cancel; others on the confirm button.
    ;(tone === "danger" ? cancelButton : confirmButton)?.focus()
  })
}

/** Replaces Kiln's dialog with your own `(message, element, submitter) => Promise<boolean>`. */
export function setConfirm(confirm) {
  Turbo.config.forms.confirm = confirm
}

setConfirm(kilnConfirm)

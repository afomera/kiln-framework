// Kiln's Stimulus setup (`kiln/stimulus`): starts the application and registers every
// controller in the page's import map, the way stimulus-rails' `eagerLoadControllersFrom`
// does. `assets/controllers/hello_controller.js` is `data-controller="hello"`,
// `controllers/admin/users_controller.js` is `admin--users`, `bulk_edit_controller.js` is
// `bulk-edit`. To register one by hand (say, from a pinned package):
//   import { application } from "kiln/stimulus"
//   application.register("sortable", Sortable)
import { Application } from "@hotwired/stimulus"

export const application = Application.start()
window.Stimulus = application

const importmap = document.querySelector('script[type="importmap"]')
const imports = importmap ? JSON.parse(importmap.textContent).imports || {} : {}

for (const name of Object.keys(imports)) {
  const match = name.match(/^controllers\/(.+)_controller$/)
  if (!match) continue
  const identifier = match[1].replace(/\//g, "--").replace(/_/g, "-")
  import(name)
    .then((module) => application.register(identifier, module.default))
    .catch((error) => console.error(`Kiln: couldn't load the "${identifier}" controller (${name})`, error))
}

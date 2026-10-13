// Direct uploads: files go straight to storage (S3, R2, GCS or Kiln's own endpoint for
// local disk), and forms submit only a signed token for each.
//
// - `DirectUpload` is the same API as Active Storage's, so Lexxy (which imports it as
//   "@rails/activestorage") uploads its attachments this way.
// - Forms: a file input with `data-direct-upload-url` (Kiln's `field()` adds it) uploads its
//   file when the form is submitted, shows a progress bar, then the form submits the token
//   instead of the bytes. Without JavaScript the same form posts the file as multipart.

function csrfToken() {
  return document.querySelector('meta[name="csrf-token"]')?.content
}

function request(method, url, { body, headers = {}, json = false, onXHR } = {}) {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest()
    xhr.open(method, url)
    if (json) xhr.responseType = "json"
    for (const [name, value] of Object.entries(headers)) xhr.setRequestHeader(name, value)
    onXHR?.(xhr)
    xhr.addEventListener("load", () => {
      if (xhr.status >= 200 && xhr.status < 300) resolve(xhr.response)
      else reject(new Error(xhr.response?.error || `Upload failed (${xhr.status})`))
    })
    xhr.addEventListener("error", () => reject(new Error("Upload failed: the network request didn't complete")))
    xhr.send(body)
  })
}

export class DirectUpload {
  constructor(file, url, delegate) {
    this.file = file
    this.url = url
    this.delegate = delegate
  }

  // `callback(error, blob)`, as Active Storage's does.
  create(callback) {
    this.#run().then((blob) => callback(null, blob), (error) => callback(error))
  }

  async #run() {
    const blob = await request("POST", this.url, {
      json: true,
      headers: {
        "Content-Type": "application/json",
        Accept: "application/json",
        ...(csrfToken() ? { "X-CSRF-Token": csrfToken() } : {}),
      },
      body: JSON.stringify({
        blob: { filename: this.file.name, content_type: this.file.type, byte_size: this.file.size },
      }),
      onXHR: (xhr) => this.delegate?.directUploadWillCreateBlobWithXHR?.(xhr),
    })
    const { url, headers } = blob.direct_upload
    await request("PUT", url, {
      headers,
      body: this.file.slice(),
      onXHR: (xhr) => this.delegate?.directUploadWillStoreFileWithXHR?.(xhr),
    })
    return blob
  }
}

// Forms ---------------------------------------------------------------------------------

function upload(input, file) {
  const progress = document.createElement("progress")
  progress.className = "kiln-upload-progress"
  progress.max = 100
  progress.value = 0
  input.after(progress)
  const upload = new DirectUpload(file, input.dataset.directUploadUrl, {
    directUploadWillStoreFileWithXHR(xhr) {
      xhr.upload.addEventListener("progress", (event) => {
        if (event.lengthComputable) progress.value = (event.loaded / event.total) * 100
      })
    },
  })
  return new Promise((resolve, reject) => {
    upload.create((error, blob) => {
      progress.remove()
      error ? reject(error) : resolve(blob)
    })
  })
}

function showError(input, message) {
  input.closest(".field")?.classList.add("field-with-errors")
  const note = document.createElement("p")
  note.className = "error kiln-upload-error"
  note.textContent = message
  input.after(note)
}

// Capturing, so this runs before Turbo sees the submission.
document.addEventListener(
  "submit",
  async (event) => {
    const form = event.target
    if (!(form instanceof HTMLFormElement) || form.dataset.kilnUploaded === "true") return
    const inputs = [...form.querySelectorAll("input[type=file][data-direct-upload-url]")].filter(
      (input) => !input.disabled && input.files?.length,
    )
    if (inputs.length === 0) return
    event.preventDefault()
    event.stopImmediatePropagation()
    const submitter = event.submitter
    if (submitter) submitter.disabled = true
    form.querySelectorAll(".kiln-upload-error").forEach((note) => note.remove())
    try {
      for (const input of inputs) {
        const blob = await upload(input, input.files[0])
        const token = document.createElement("input")
        token.type = "hidden"
        token.name = input.name
        token.value = blob.upload_token
        input.after(token)
        // The bytes are stored; don't send them again.
        input.disabled = true
      }
      form.dataset.kilnUploaded = "true"
      if (submitter) submitter.disabled = false
      form.requestSubmit(submitter)
    } catch (error) {
      if (submitter) submitter.disabled = false
      showError(inputs[0], error.message)
    }
  },
  true,
)

// After Turbo renders the response (a 422 re-render, say), the form starts fresh.
document.addEventListener("turbo:submit-end", (event) => {
  delete event.target.dataset?.kilnUploaded
})

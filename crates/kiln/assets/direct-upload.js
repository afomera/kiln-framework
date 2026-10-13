// Kiln's stand-in for Active Storage's `DirectUpload`, which Lexxy imports (as
// "@rails/activestorage") to upload attachments: one multipart POST of the file to
// `/kiln/uploads`, which stores it and answers with the blob Lexxy expects.
export class DirectUpload {
  constructor(file, url, delegate) {
    this.file = file
    this.url = url
    this.delegate = delegate
  }

  create(callback) {
    const xhr = new XMLHttpRequest()
    xhr.open("POST", this.url)
    xhr.responseType = "json"
    xhr.setRequestHeader("Accept", "application/json")
    const token = document.querySelector('meta[name="csrf-token"]')?.content
    if (token) xhr.setRequestHeader("X-CSRF-Token", token)
    // Lexxy's hooks: credentials, and upload progress for its progress bar.
    this.delegate?.directUploadWillCreateBlobWithXHR?.(xhr)
    this.delegate?.directUploadWillStoreFileWithXHR?.(xhr)
    xhr.addEventListener("load", () => {
      if (xhr.status >= 200 && xhr.status < 300 && xhr.response) {
        callback(null, xhr.response)
      } else {
        callback(new Error(xhr.response?.error || `Upload failed (${xhr.status})`))
      }
    })
    xhr.addEventListener("error", () => callback(new Error("Upload failed")))
    const body = new FormData()
    body.append("file", this.file)
    xhr.send(body)
  }
}

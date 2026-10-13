# Vendored JavaScript

Served by kiln at fixed, fingerprinted URLs so apps need no Node toolchain or CDN.

| File | Source | License |
| --- | --- | --- |
| `turbo-8.0.12.js` | `@hotwired/turbo@8.0.12`, `dist/turbo.es2017-esm.js` | MIT (`turbo-MIT-LICENSE`, from the hotwired/turbo repository) |
| `lexxy/` | The `lexxy` gem (Basecamp's rich text editor, on Lexical): `lexxy.min.js` and its three stylesheets, unmodified | MIT (`lexxy/MIT-LICENSE`, Copyright Jorge Manrubia), plus the libraries it bundles in `lexxy/THIRD-PARTY-NOTICES.md` |

To upgrade Turbo, replace the file, update the version in `src/assets.rs`, and run the blog's tests.

To upgrade Lexxy, run `make vendor-lexxy VERSION=x.y.z` (or `scripts/vendor-lexxy.sh x.y.z`): it verifies the gem
against rubygems.org's checksum, copies the files, and writes `VERSION` and `SHA256SUMS`. Kiln reads the version from
`lexxy/VERSION` and adds the license header when serving the file, so no Rust changes are needed. Then check
`THIRD-PARTY-NOTICES.md` against the bundled versions the script prints, and run `cargo test`: tests fail if a release
drops something kiln relies on (the configuration it sets, the CSS classes its skin styles).

Kiln's own editor code isn't vendored: `assets/rich-text.js` (configuration) and `assets/rich-text.css` (the skin).

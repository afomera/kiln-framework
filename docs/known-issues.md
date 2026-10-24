# Known issues

- **rustc incremental-compilation ICE.** On rustc 1.98.1 and 1.99.1, adding a field to a record while `src/factories.rs` has `async fn` trait impls crashes incremental compilation (`encountered incremental compilation error with evaluate_obligation`, `rustc_middle/src/verify_ich.rs:82`). In the blog, adding a field to `Post` does it every time. The real error (e.g. E0063, missing field) still prints first, and the next build after fixing it succeeds. Workarounds: `cargo clean -p <app>`, or `CARGO_INCREMENTAL=0`, which avoids it. To be reported upstream with a reproduction.
- zsh treats `?` as a glob, so quote optional fields: `'notes:text?'`.
- Factories live in `src/factories.rs` (not `tests/`) because of Rust's orphan rule.

# Developing Kiln

## Make tasks

```sh
make install            # builds the CLI and links it as `kiln-dev` in ~/.local/bin
make                    # rebuilds the CLI; the installed link picks it up
make test
make lint               # cargo fmt --check and clippy -D warnings
make uninstall
```

This is for working on Kiln itself; apps use `cargo install kiln-framework-cli`. The checkout's CLI is linked as `kiln-dev` so it doesn't shadow an installed `kiln`, and apps it creates with `kiln-dev new` depend on this checkout's `kiln` crate by path. `make install BIN_DIR=~/bin BIN_NAME=kiln` installs elsewhere or under another name.

## The example blog

```sh
cd examples/blog
kiln-dev db reset       # create storage/development.sqlite3, migrate, seed
kiln-dev dev            # http://127.0.0.1:3000 (sign in as ada@example.com)
kiln-dev check
kiln-dev test
``` `make vendor-lexxy VERSION=..` updates the vendored rich text editor, and `make release VERSION=..` bumps, checks, commits and tags, then asks before publishing to crates.io.

## Crates

Kiln is published on crates.io as `kiln-framework`, `kiln-framework-macros` and `kiln-framework-cli`, since `kiln` is taken; apps still `use kiln::prelude::*` and the command is `kiln`.

- `crates/kiln`: the framework
- `crates/kiln-macros`: `#[derive(Record)]`, `#[action]` and the other macros
- `crates/kiln-cli`: the `kiln` command, its generators and templates
- `examples/blog`: the example app, and most of the integration tests

## What the tests prove

- `examples/blog/tests/ui/`: code that must not compile, with the compiler output recorded. Using an unpreloaded relation, mixing `Id<User>` and `Id<Post>`, filtering with another table's column, route captures without a `Param`, wrong path-helper argument types, missing templates or layouts, and part mistakes.
- `tests/posts.rs`: sign-in, CRUD, 422 re-rendering with kept input, unique-slug checks, unknown form fields (400), missing CSRF tokens (403), policies (403), and transactional deletes.
- `tests/http.rs`: preloading is one query per relation, layouts, optional relations, error reports for browsers and agents.
- `tests/api.rs`: JSON lists without leaking emails, JSON 404s, field errors as JSON, unknown JSON fields (400), and API routes ignoring the session cookie.
- `crates/kiln/src/migrate.rs`: diff output, including the rebuild path for NOT NULL columns.
- The rest of `examples/blog/tests/` covers each M2 piece and what came after it: jobs and batches, mail, notifications, uploads, cache, sessions, joins and queries, row locks, API tokens and scopes, webhooks, catch-all routes, split databases, assets and the dev tools.

## Known issues

See [known-issues.md](known-issues.md).

<h1>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/kiln-wordmark-dark.svg">
    <img src="assets/brand/kiln-wordmark.svg" alt="Kiln" height="56">
  </picture>
</h1>

A full-stack Rust web framework where the compiler is the reviewer. Working name.

Spec: [Kiln Framework Spec](https://claude.ai/artifact/PNHemnZU3NnUq51gfW65uv)

## Quick start

```sh
make install            # builds the CLI and links it as `kiln-dev` in ~/.local/bin
cd examples/blog
kiln-dev db reset       # create db/development.sqlite3, migrate, seed
kiln-dev dev            # http://127.0.0.1:3000 (sign in as ada@example.com)
kiln-dev check          # compile + conventions + schema + template routes
kiln-dev test
```

`make` rebuilds the CLI; the installed link picks it up. `make install BIN_DIR=~/bin BIN_NAME=kiln` installs elsewhere or under another name; `make test`, `make lint` and `make uninstall` do what they say.

New app: `kiln-dev new journal` creates a standalone app with sign-in, a layout, a `User` record, its first migration, tests and `AGENTS.md`, linked to this repo's `kiln` crate.

Agents working in an app start with its `AGENTS.md` ([the template](crates/kiln-cli/templates/AGENTS.md)).

## Status: M1 (core)

| Piece | Where |
| --- | --- |
| Records: the struct is the schema; typed columns, `Id<T>`, queries, `insert`/`update`/`delete` | `crates/kiln/src/query.rs`, `crates/kiln-macros/src/record.rs` |
| `Loaded<T, (rels..)>` preloading; N+1 is a compile error | `crates/kiln/src/rel.rs` |
| Schema diffs: `kiln db diff <name>` writes reviewed SQL from the structs, including SQLite table rebuilds; `db migrate`, `db status`, drift detection | `crates/kiln/src/migrate.rs` |
| Transactions: `ctx.db.transaction(\|tx\| async move { .. })` | `crates/kiln/src/db.rs` |
| `#[action(PATCH "/posts/{slug}")]`, typed `path(..)` helpers, link-time route registry | `crates/kiln-macros/src/action.rs` |
| API pipeline: `#[action(GET "/api/posts", pipeline = Api)]`, `Json<T>`, JSON/form bodies through `Form<T>`, problem+json errors, no session cookie | `crates/kiln/src/app.rs`, `error_page.rs` |
| Sessions (signed cookies), flash, CSRF, `_method` override, `Current<User>` | `crates/kiln/src/session.rs`, `app.rs` |
| Forms: `#[derive(Form)]` + `#[validate]` + `#[form(textarea, label, hint, placeholder)]`, `Form<T>` rejects unknown fields, `or_render` (422), async `CheckWith`; template helpers `form_start`/`fields`/`field`/`form_end` render inputs with `required`/`maxlength` from the rules | `crates/kiln/src/form.rs`, `view.rs` |
| Time: `Timestamp::days_from_now(7)`, `hours_ago(2)`, `+ kiln::time::SignedDuration`; `{{ ts \| datetime }}` | `crates/kiln/src/types.rs` |
| Policies with proof types: `Allows<Update, Post>`, `authorize` → `Authorized<Update, Post>` | `crates/kiln/src/policy.rs` |
| Views, layouts, parts, strict templates, error pages (HTML + Markdown for agents) | `crates/kiln/src/view.rs`, `error_page.rs` |
| Test harness: `#[kiln::test]`, `TestApp` with migrations, factories, `as_user`, `post_form`, `assert_flash` | `crates/kiln/src/testing.rs` |
| `kiln` CLI: `new`, generators (`resource`, `record`, `action`, `op`, `part`), `db`, `routes`, `check`, `test`, `dev --port` | `crates/kiln-cli` |

### `kiln g resource`

```sh
kiln g resource Bookmark title:string url:string 'notes:text?' 'rating:integer?' pinned:bool owner:ref:User
```

writes the record, migration, form, policy, three ops, seven actions, four views, four templates, a factory and eight tests, updates the module files, and runs `kiln db diff`. The blog's `Bookmark` resource is that command's unedited output; it passes `kiln check` and its tests.

### `kiln check`

| Code | Rule |
| --- | --- |
| K0001 | The app compiles (`cargo check --all-targets`); the first compiler error is printed in full |
| K0101 | Records, forms, views, parts and actions live in their directories; a record's file is named after it |
| K0102 | One `#[action]` per file |
| K0103 | (warn) The action function is named after its file |
| K0202 | The record structs match `db/snapshot.json` (no undiffed schema change) |
| K0301 | A function that writes a policy-guarded record (`Post::insert/update/delete`) takes the matching `Authorized<_, Post>` proof |
| K0203 | (warn) Migrations not yet applied to the development database |
| K0302 | (warn) A data-changing action has no test that calls its path helper |
| K0402 | Every `path("..")` in a template names a real route |
| K0403 | Every `include`/`extends` names a real template |
| K0501 | No raw `sqlx` outside `src/records/` |
| K0601 | No environment reads outside `src/main.rs` |

### What the tests prove

- `examples/blog/tests/ui/`: code that must not compile, with the compiler output recorded. Using an unpreloaded relation, mixing `Id<User>` and `Id<Post>`, filtering with another table's column, route captures without a `Param`, wrong path-helper argument types, missing templates or layouts, and part mistakes.
- `tests/posts.rs`: sign-in, CRUD, 422 re-rendering with kept input, unique-slug checks, unknown form fields (400), missing CSRF tokens (403), policies (403), and transactional deletes.
- `tests/http.rs`: preloading is one query per relation, layouts, optional relations, error reports for browsers and agents.
- `tests/api.rs`: JSON lists without leaking emails, JSON 404s, field errors as JSON, unknown JSON fields (400), and API routes ignoring the session cookie.
- `crates/kiln/src/migrate.rs`: diff output, including the rebuild path for NOT NULL columns.

## Known issues

- **rustc incremental-compilation ICE.** On rustc 1.98.1, adding a field to a record while `src/factories.rs` has `async fn` trait impls sometimes crashes incremental compilation. The real error (e.g. E0063, missing field) still prints first. Workaround: `cargo clean -p <app>`. To be reported upstream with a reproduction.
- zsh treats `?` as a glob, so quote optional fields: `'notes:text?'`.
- Factories live in `src/factories.rs` (not `tests/`) because of Rust's orphan rule.
- SQLite only. Postgres is planned as its own step after M1.

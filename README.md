<h1>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/kiln-wordmark-dark.svg">
    <img src="assets/brand/kiln-wordmark.svg" alt="Kiln" height="56">
  </picture>
</h1>

A full-stack Rust web framework where the compiler is the reviewer. Published on crates.io as `kiln-framework` (with `kiln-framework-macros` and `kiln-framework-cli`), since `kiln` is taken; apps still `use kiln::prelude::*` and the command is `kiln`.

Spec: [Kiln Framework Spec](https://claude.ai/artifact/PNHemnZU3NnUq51gfW65uv)

## Quick start

```sh
make install            # builds the CLI and links it as `kiln-dev` in ~/.local/bin
cd examples/blog
kiln-dev db reset       # create storage/development.sqlite3, migrate, seed
kiln-dev dev            # http://127.0.0.1:3000 (sign in as ada@example.com)
kiln-dev check          # compile + conventions + schema + template routes
kiln-dev test
```

`make` rebuilds the CLI; the installed link picks it up. `make install BIN_DIR=~/bin BIN_NAME=kiln` installs elsewhere or under another name; `make test`, `make lint` and `make uninstall` do what they say.

New app: `kiln-dev new journal` creates a standalone app with sign-in, a layout, a `User` record, its first migration, tests and `AGENTS.md`, linked to this repo's `kiln` crate.

Agents working in an app start with its `AGENTS.md` ([the template](crates/kiln-cli/templates/AGENTS.md)).

## Status: M3 (agent tooling)

M0–M1 built the core, M2 the full stack, M3 the tools agents use; work since M3 fills out the query layer, APIs and assets. Published as 0.0.1.

### Core (M1)

| Piece | Where |
| --- | --- |
| Records: the struct is the schema; typed columns, `Id<T>`, queries, `insert`/`update`/`delete` | `crates/kiln/src/query.rs`, `crates/kiln-macros/src/record.rs` |
| `Loaded<T, (rels..)>` preloading; N+1 is a compile error | `crates/kiln/src/rel.rs` |
| Schema diffs: `kiln db diff <name>` writes reviewed SQL from the structs, including SQLite table rebuilds and Postgres ALTERs; `db migrate`, `db status`, drift detection, `db undo`, `db squash` (one file per past year) | `crates/kiln/src/migrate.rs` |
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

### Full stack (M2 and since)

| Piece | Where |
| --- | --- |
| Postgres behind a `Dialect` interface (`kiln new --database postgres`); SQLite stays the default. Kiln's own tables can live in a separate database | `crates/kiln/src/dialect.rs`, `db.rs` |
| Queries: typed joins (filtering on an unjoined table is a compile error), `has_many(.., through = ..)`, subqueries, aggregates, row locks, `increment`, raw `db.sql` | `crates/kiln/src/query.rs`, `join.rs`, `sql.rs` |
| Record constraints, cascades, `one_of` and `has_one` | `crates/kiln-macros/src/record.rs` |
| Jobs, schedules and batches, enqueued on a transaction so they run only after commit; `kiln jobs`, `schedules`, `batches` | `crates/kiln/src/jobs.rs`, `schedule.rs`, `batch.rs` |
| Mail with previews, notifications and notification settings | `crates/kiln/src/mail.rs`, `notify.rs` |
| Auth with password settings; API tokens whose scopes deny by default (`#[action(.., pipeline = Api, scope = "write")]`); rate limits | `crates/kiln/src/auth.rs`, `rate_limit.rs` |
| Turbo, realtime streams, live reload; Stimulus by default and import-map pins (`kiln importmap pin`) | `crates/kiln/src/turbo.rs`, `realtime.rs`, `dev_reload.rs`, `assets.rs` |
| Rich text (Lexxy) with attachments, file uploads, direct uploads, image variants | `crates/kiln/src/rich_text.rs`, `files.rs`, `storage.rs` |
| Markdown and syntax highlighting: `\| markdown`, `\| highlight`, a `Markdown` field type | `crates/kiln/src/markdown.rs` |
| Catch-all route captures: `#[action(GET "/{owner}/{repo}/tree/{*path}")]` | `crates/kiln/src/routing.rs` |
| Cache; `#[derive(Config)]` with encrypted secrets (`kiln secrets`) | `crates/kiln/src/cache.rs`, `config.rs` |
| Signed outbound webhooks and an SSRF-guarded HTTP client | `crates/kiln/src/webhooks.rs`, `http.rs` |
| The `/kiln` command center and a welcome page for `/` in development | `crates/kiln/src/devtools.rs`, `welcome.rs` |
| `kiln dev`: a terminal UI with a routes pane, process groups, crash restarts, `--inline` and `--json` | `crates/kiln-cli/src/dev/` |
| Deploys: `kiln build` makes a single-binary `dist/`, `kiln build --docker` a production image; `make release` | `crates/kiln-cli`, `scripts/release.sh` |

### Agent tooling (M3)

| Piece | Where |
| --- | --- |
| `kiln introspect [--json]`: routes, records, views, forms, policies, ops, jobs, mail, config and templates, with file:line | `crates/kiln-cli/src/introspect.rs` |
| `kiln explain <code>`: the long form of every check and error-page code | `crates/kiln-cli/src/explain.rs` |
| `kiln mcp`: an MCP server with introspect, request (as any user), read-only query, mail, jobs, check, explain and test tools | `crates/kiln-cli/src/mcp.rs` |
| App-written lint rules (`#[lint]` in `src/lints/`) that `kiln check` runs | `crates/kiln/src/lint.rs` |

### `kiln g resource`

```sh
kiln g resource Bookmark title:string url:string 'notes:text?' 'rating:integer?' pinned:bool owner:ref:User
```

writes the record, migration, form, policy, three ops, seven actions, four views, four templates, a factory and eight tests, updates the module files, and runs `kiln db diff`. The blog's `Bookmark` resource is that command's unedited output; it passes `kiln check` and its tests.

### `kiln check`

| Code | Rule |
| --- | --- |
| K0001 | The app compiles (`cargo check --all-targets`); the first compiler error is printed in full |
| K0002 | The app binary can report its schema status |
| K0003 | The app's own rules (`src/lints/`) can run |
| K0101 | Records, forms, views, parts and actions live in their directories; a record's file is named after it |
| K0102 | One `#[action]` per file |
| K0103 | (warn) The action function is named after its file |
| K0110 | No two routes match the same requests |
| K0202 | The record structs match `db/snapshot.json` (no undiffed schema change) |
| K0301 | A function that writes a policy-guarded record (`Post::insert/update/delete`) takes the matching `Authorized<_, Post>` proof |
| K0203 | (warn) Migrations not yet applied to the development database |
| K0204 | `db/snapshot.json` was written with a migration that's since been deleted (`kiln db undo` restores it) |
| K0302 | (warn) A data-changing action has no test that calls its path helper |
| K0304 | No `policy::as_system` in an action |
| K0401 | A template, its layout and its includes read only what the view provides |
| K0402 | Every `path("..")` in a template names a real route |
| K0403 | Every `include`/`extends` names a real template |
| K0404 | Every `data-controller` names a real Stimulus controller |
| K0405 | Every import-map pin loads |
| K0412 | A template, its layout and its includes read only the relations the view loaded |
| K0501 | No raw `sqlx` outside `src/records/` |
| K0601 | No environment reads outside `src/main.rs` |
| K0701 | Jobs, mail and notifications inside a transaction are enqueued on it, not on the pool |

Error pages carry `K1xxx` codes (404s, CSRF, missing scopes, rate limits, database errors, ...); `kiln explain` covers those too.

### What the tests prove

- `examples/blog/tests/ui/`: code that must not compile, with the compiler output recorded. Using an unpreloaded relation, mixing `Id<User>` and `Id<Post>`, filtering with another table's column, route captures without a `Param`, wrong path-helper argument types, missing templates or layouts, and part mistakes.
- `tests/posts.rs`: sign-in, CRUD, 422 re-rendering with kept input, unique-slug checks, unknown form fields (400), missing CSRF tokens (403), policies (403), and transactional deletes.
- `tests/http.rs`: preloading is one query per relation, layouts, optional relations, error reports for browsers and agents.
- `tests/api.rs`: JSON lists without leaking emails, JSON 404s, field errors as JSON, unknown JSON fields (400), and API routes ignoring the session cookie.
- `crates/kiln/src/migrate.rs`: diff output, including the rebuild path for NOT NULL columns.
- The rest of `examples/blog/tests/` covers each M2 piece and what came after it: jobs and batches, mail, notifications, uploads, cache, sessions, joins and queries, row locks, API tokens and scopes, webhooks, catch-all routes, split databases, assets and the dev tools.

## Known issues

- **rustc incremental-compilation ICE.** On rustc 1.98.1 and 1.99.1, adding a field to a record while `src/factories.rs` has `async fn` trait impls crashes incremental compilation (`encountered incremental compilation error with evaluate_obligation`, `rustc_middle/src/verify_ich.rs:82`). In the blog, adding a field to `Post` does it every time. The real error (e.g. E0063, missing field) still prints first, and the next build after fixing it succeeds. Workarounds: `cargo clean -p <app>`, or `CARGO_INCREMENTAL=0`, which avoids it. To be reported upstream with a reproduction.
- zsh treats `?` as a glob, so quote optional fields: `'notes:text?'`.
- Factories live in `src/factories.rs` (not `tests/`) because of Rust's orphan rule.

## License

Kiln is © 2026 [Andrea Fomera](https://afomera.dev), dual-licensed under either of [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), at your option. Vendored libraries keep their own licenses: Turbo (MIT) and Lexxy with what it bundles (MIT; DOMPurify under Apache 2.0), in `crates/kiln/vendor/`.

# Features

What Kiln has today, and where each piece lives. M0–M1 built the core, M2 the full stack, M3 the tools agents use ([agents.md](agents.md)); work since M3 fills out the query layer, APIs and assets.

## Core (M1)

| Piece | Where |
| --- | --- |
| Records: the struct is the schema; typed columns, `Id<T>`, queries, `insert`/`update`/`delete` | `crates/kiln/src/query.rs`, `crates/kiln-macros/src/record.rs` |
| `Loaded<T, (rels..)>` preloading; N+1 is a compile error | `crates/kiln/src/rel.rs` |
| Schema diffs: `kiln db diff <name>` writes reviewed SQL from the structs, including SQLite table rebuilds and Postgres ALTERs; `db migrate`, `db status`, drift detection, `db undo`, `db squash` (one file per past year) | `crates/kiln/src/migrate.rs` |
| Transactions: `ctx.db.transaction(\|tx\| async move { .. })` | `crates/kiln/src/db.rs` |
| `#[action(PATCH "/posts/{slug}")]`, typed `path(..)` helpers, link-time route registry | `crates/kiln-macros/src/action.rs` |
| API pipeline: `#[action(GET "/api/posts", pipeline = Api)]`, `Json<T>`, JSON/form bodies through `Form<T>`, problem+json errors, no session cookie | `crates/kiln/src/app.rs`, `error_page.rs` |
| Sessions (signed cookies), flash, CSRF, `_method` override, `Current<User>`; `app.layout_data(..)` for per-request layout values (`{{ layout_data.unread }}`) | `crates/kiln/src/session.rs`, `app.rs` |
| Forms: `#[derive(Form)]` + `#[validate]` + `#[form(textarea, label, hint, placeholder)]`, `Form<T>` rejects unknown fields, `or_render` (422), async `CheckWith`; template helpers `form_start`/`fields`/`field`/`form_end` render inputs with `required`/`maxlength` from the rules; `form_start`/`button_to` take `id`, `data={..}`, `attrs={..}`, and `button_to` a `{% call %}` block for icon buttons | `crates/kiln/src/form.rs`, `view.rs` |
| Time: `Timestamp::days_from_now(7)`, `hours_ago(2)`, `+ kiln::time::SignedDuration`; `{{ ts \| datetime }}`, `\| time_ago`, `\| time_ago_tag`; numbers: `\| number`, `\| compact_number`, `\| filesize`, `\| duration` (also `kiln::format::*`) | `crates/kiln/src/types.rs`, `format.rs` |
| Policies with proof types: `Allows<Update, Post>`, `authorize` → `Authorized<Update, Post>` | `crates/kiln/src/policy.rs` |
| Views, layouts, parts, strict templates, error pages (HTML + Markdown for agents) | `crates/kiln/src/view.rs`, `error_page.rs` |
| Test harness: `#[kiln::test]`, `TestApp` with migrations, factories, `as_user`, `post_form`, `assert_flash` | `crates/kiln/src/testing.rs` |
| `kiln` CLI: `new`, generators (`resource`, `record`, `action`, `op`, `part`), `db`, `routes`, `check`, `test`, `dev --port` | `crates/kiln-cli` |

## Full stack (M2 and since)

| Piece | Where |
| --- | --- |
| Postgres behind a `Dialect` interface (`kiln new --database postgres`); SQLite stays the default. Kiln's own tables can live in a separate database | `crates/kiln/src/dialect.rs`, `db.rs` |
| Queries: typed joins (filtering on an unjoined table is a compile error), `has_many(.., through = ..)`, subqueries, aggregates, row locks, `increment`, raw `db.sql` | `crates/kiln/src/query.rs`, `join.rs`, `sql.rs` |
| Record constraints, cascades, `one_of` and `has_one` | `crates/kiln-macros/src/record.rs` |
| Jobs, schedules and batches, enqueued on a transaction so they run only after commit; `kiln jobs`, `schedules`, `batches` | `crates/kiln/src/jobs.rs`, `schedule.rs`, `batch.rs` |
| Mail with previews, notifications and notification settings | `crates/kiln/src/mail.rs`, `notify.rs` |
| Auth with password settings; API tokens whose scopes deny by default (`#[action(.., pipeline = Api, scope = "write")]`); rate limits | `crates/kiln/src/auth.rs`, `rate_limit.rs` |
| Turbo, realtime streams, live reload; Stimulus by default and import-map pins (`kiln importmap pin`) | `crates/kiln/src/turbo.rs`, `realtime.rs`, `dev_reload.rs`, `assets.rs` |
| Fingerprinted assets, cached forever; stylesheets' `url(..)` and `@import` references rewritten to fingerprinted URLs | `crates/kiln/src/assets.rs` |
| Rich text (Lexxy) with attachments, file uploads, direct uploads, image variants | `crates/kiln/src/rich_text.rs`, `files.rs`, `storage.rs` |
| Markdown and syntax highlighting: `\| markdown`, `\| highlight`, a `Markdown` field type | `crates/kiln/src/markdown.rs` |
| Catch-all route captures: `#[action(GET "/{owner}/{repo}/tree/{*path}")]` | `crates/kiln/src/routing.rs` |
| Cache; `#[derive(Config)]` with encrypted secrets (`kiln secrets`) | `crates/kiln/src/cache.rs`, `config.rs` |
| Signed outbound webhooks and an SSRF-guarded HTTP client | `crates/kiln/src/webhooks.rs`, `http.rs` |
| The `/kiln` command center and a welcome page for `/` in development | `crates/kiln/src/devtools.rs`, `welcome.rs` |
| `kiln dev`: a terminal UI with a routes pane, process groups, crash restarts, `--inline` and `--json` | `crates/kiln-cli/src/dev/` |
| Deploys: `kiln build` makes a single-binary `dist/`, `kiln build --docker` a production image; `make release` | `crates/kiln-cli`, `scripts/release.sh` |

## Agent tooling (M3)

See [agents.md](agents.md).

## `kiln g resource`

```sh
kiln g resource Bookmark title:string url:string 'notes:text?' 'rating:integer?' pinned:bool owner:ref:User
```

writes the record, migration, form, policy, three ops, seven actions, four views, four templates, a factory and eight tests, updates the module files, and runs `kiln db diff`. The blog's `Bookmark` resource is that command's unedited output; it passes `kiln check` and its tests.

zsh treats `?` as a glob, so quote optional fields: `'notes:text?'`.

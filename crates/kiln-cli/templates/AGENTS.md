# Working in this app

This is a Kiln app. Conventions are enforced by the compiler and by `kiln check`. Use the
generators and copy the shape of existing code instead of inventing structure.

Commands below say `kiln`. If it's installed under another name (e.g. `kiln-dev`), use that.
Run them from the app directory.

## Commands

| Task | Command |
| --- | --- |
| CRUD resource | `kiln g resource Event name:string description:text starts_at:timestamp 'venue:string?' owner:ref:User` |
| Record only | `kiln g record Tag name:string` |
| One more action | `kiln g action posts publish POST /posts/{slug}/publish` |
| Domain operation | `kiln g op publish_post` |
| Presentation methods | `kiln g part Post` |
| Background job | `kiln g job send_digest` |
| Email | `kiln g mail welcome_email` (with a preview at `/kiln/mail/previews`) |
| Notification (inbox + email + live) | `kiln g notification comment_posted [--to User]` (adds the `/notifications` inbox and `/settings/notifications` the first time) |
| Passwords, sign-up, resets, sign-in list | `kiln g auth` (once; replaces the demo sign-in) |
| Schema change | edit the record struct → `kiln db diff <snake_case_name>` → review the SQL → `kiln db migrate` |
| Redo a migration | `kiln db undo` (deletes the newest generated migration and restores the snapshot), then `kiln db diff` again. Never delete migration files by hand. |
| Deploy | `kiln build --docker` builds the production image from `Dockerfile` (`docker run -p 8080:8080 -e KILN_SECRET_KEY=… -v app-storage:/app/storage app`; migrations run on boot, `GET /up` is the health check). `kiln build` makes `dist/<app>`: one binary with templates, assets, migrations and secrets compiled in (`.files(kiln::embedded_files!())` in `src/main.rs`; debug builds read them from disk), run with `KILN_ENV=production` where it may create `storage/`. In production SQLite is `storage/production.sqlite3` (next to uploads: keep `storage/` on a volume) and the server binds `0.0.0.0` (`HOST`, `PORT`). `kiln g dockerfile` adds the files to an older app. |
| Fewer migration files | `kiln db squash` (one file per past year; databases that ran the originals skip it) |
| Verify | `kiln check` (must end `0 errors`), then `kiln test` |
| Understand a finding or an error page's code | `kiln explain K0301` (why the rule exists, how to fix it); `kiln explain` lists every code, the app's own included |
| A team convention `kiln check` should enforce | `kiln g lint no_unwrap_in_actions` → `src/lints/` + tests; codes are `A0001`.. (never `K`) |
| Orient | `kiln introspect` before changing an unfamiliar app: every route (args, pipeline, return type), record (columns, relations), view (template, fields), form, policy, op signature, job, mail and config, with `file:line`. `--json` for the full detail; narrow with a section (`kiln introspect records`) or `--find Event` |
| See it work | `kiln mcp` (in `.mcp.json`): `request` sends a request through the app as any user (`as: "User:1"` or an email) and returns the page as text, every SQL statement, the jobs enqueued and the mail sent (`perform_jobs: true` runs the queued jobs first, so mail sent later shows too; `files` uploads); `query` runs read-only SQL on the dev database; also `introspect`, `mail`, `jobs`, `check`, `explain`, `test`. Non-GET requests need `kiln mcp --write`. Without MCP, the app binary does the same: `cargo run -- request POST /posts --as 1 --form title=Hi --perform-jobs` (`request --help` for every option), `cargo run -- db query "SELECT .."` |
| Routes | `kiln routes` |
| Run | `kiln dev` (port 3000; `--port 4000` for another): runs `Procfile.dev`, rebuilds and restarts on src/ changes, restarts crashed processes. In a terminal it's a TUI (the `routes` entry lists the app's routes, refreshed on each build; `kiln routes` too): `1`-`9`/Tab select, `j`/`k` `g`/`G` scroll, `r` restart (on build: rebuild), `s` stop/start, `/` search (`n`/`N`), `f` filter, `m` all-output stream, `c` clear, `o` open /kiln, `K` check, `T` test, `q` quit. Piped, in CI or with `--inline` it prints `web \| ...` lines; `--json` prints NDJSON events (start, output, exit, restarting, failed, build, ready). Stopping it (q, Ctrl-C, SIGTERM) stops every process it started and frees the port. |
| Jobs | `kiln jobs` (counts, queues, failures), `kiln jobs retry [id]`, `cancel <id>`, `run <id>`, `pause <queue>`, `resume <queue>`, `discard-failed` |
| Schedules, batches | `kiln schedules [pause\|resume\|run <job>]`, `kiln batches [cancel <id>]` |
| Inspect the running app | `/kiln` in development: routes, records, jobs, schedules, batches, mail, broadcasts, sign-ins, each searchable, with controls (retry, cancel, pause, run now) |
| Secrets | `kiln secrets set SMTP_URL smtp://..`, `kiln secrets show` (key in `config/master.key`, never committed) |
| Fresh database | `kiln db reset` (migrate + seed); sign in at /session/new as ada@example.com |
| Database | SQLite (`storage/development.sqlite3`, beside uploaded files) or Postgres (`kiln new app --database postgres`: `postgres://localhost/<app>_development`, tests in `<app>_test`); `DATABASE_URL` / `TEST_DATABASE_URL` override. `db/snapshot.json` records which. Kiln's queue, cache and broadcast log can each live in their own database (`QUEUE_DATABASE_URL`, `CACHE_DATABASE_URL`, `STREAMS_DATABASE_URL`, same backend); then jobs and broadcasts sent inside a transaction are handed over when it commits (a rollback still sends nothing). |

Field types: `string`, `text`, `rich_text` (the editor; `?` means it may be left empty), `integer`, `float`, `bool`, `timestamp`, `file`, `image` (uploads; `image` accepts only images), `ref:<Record>`. A
trailing `?` makes a field optional; quote it in zsh (`'notes:text?'`). A trailing `:internal`
(`'sent_at:timestamp?:internal'`, `sends:integer:internal`) is a field the app sets, not the form:
it's left off the form and its tests, and new records start with `None`, `false` or `0`. Fields
named `email` / `*_email` are validated as addresses and get `name1@example.com` factory values.

`kiln g action issues send POST /issues/{id}/send`, when an `Issue` record exists, writes an
action that loads the issue and authorizes the signed-in user (`Update`; `Read` for GET, which
also gets a view and template), plus tests for the owner and for signed-out visitors.

The first required ref to a record people sign in as (`owner:ref:User`, or
`seller:ref:Merchant` once `Merchant` is registered in `src/app.rs`) is the **owner**: it's
set from the signed-in record, and the generated actions take `Current<Merchant>`.
Owned resources are **private by default**: the list shows only the signed-in user's
records (via a generated `Record::owned_by(&user)` query), the show page requires the
`Read` policy, and only the owner may edit or delete. Pass `--public` when everyone should
be able to list and view them (only the owner still edits and deletes). Resources without
an owner are public.

Generated code is a starting point: change it freely, then keep `kiln check` and the
generated tests passing.

## Where things go

| Concept | Location | Shape |
| --- | --- | --- |
| Table + entity | `src/records/{record}.rs` (file name must match) | `#[derive(Record)]`; re-exported from `src/records/mod.rs` |
| Named query ("scope") | the record's file | `impl Post { pub fn published() -> Query<Post> { Post::query().filter(status.eq(..)).order(published_at.desc()) } }` |
| HTTP endpoint | `src/actions/{resource}/{verb}.rs`, one per file | `#[action(METHOD "/path/{param}")] async fn {verb}` |
| JSON endpoint | `src/actions/api/{resource}/{verb}.rs` | `#[action(GET "/api/..", pipeline = Api)]` returning `Json<T>` |
| User input | `src/forms/{name}_form.rs` | `#[derive(Form)]` |
| Permission rule | `src/policies/{record}.rs` | `impl Allows<Update, Post> for User` |
| Domain logic | `src/ops/{verb}_{noun}.rs` | `pub async fn`, takes `Authorized<_, _>` proofs |
| App settings | `src/app.rs` | `pub fn configure(app: App) -> Result<App>`; used by the server and by `#[kiln::test]` |
| Config and secrets | `src/config.rs` | `#[derive(Config)] pub struct Config { .. }`, loaded in `configure` with `app.config(Config::load()?)`, read with `ctx.config::<Config>()` |
| Background job | `src/jobs/{verb}_{noun}.rs` | `#[derive(Job, Serialize, Deserialize)]` + `impl Perform` |
| Email | `src/mail/{name}.rs` + `templates/mail/{name}.html.jinja` (+ `.txt.jinja`) | `#[derive(Mail, Serialize)] #[mail(template = "mail/{name}", subject = "..")]` with a `to: String` field; wrapped in `layouts/mail.html.jinja` |
| Recurring jobs | `src/schedule.rs` | `pub fn schedule(s: Schedule) -> Schedule { s.cron("0 3 * * *", Job {}) }`, registered with `app.schedule(schedule::schedule)` |
| Notification | `src/notifications/{what_happened}.rs` | `#[derive(Notification, Serialize, Deserialize)] #[notification(description = "..", optional(mail))]` + `impl Notify` |
| Dev processes | `Procfile.dev` | `name: command`; `$APP` is the app binary |
| What templates see of a signed-in record | `src/views/current_user.rs` | `#[derive(Serialize)] struct CurrentUser` + `fn of(&User) -> Self` |
| Page | `src/views/{resource}/{verb}.rs` + `templates/{resource}/{verb}.html.jinja` | `#[derive(View)]` |
| JSON shape | `src/views/api/` | `#[derive(Serialize)]` struct; don't serialize records directly |
| Partial (a fragment for Turbo Streams, broadcasts, includes) | `src/views/{resource}/{name}.rs` + `templates/{resource}/_{name}.html.jinja` | `#[derive(View)] #[view(template = "comments/_comment", layout = none)]` |
| CSS, JS, images | `assets/` (JS entry point: `assets/application.js`) | linked with `{{ asset("app.css") }}`; JS modules load through the import map |
| Files served as-is | `public/` (`robots.txt`, `favicon.svg`) | served at the site root when no route matches |
| Presentation methods | `src/parts/{record}.rs` | `#[part] impl Record` + `#[record(.., parts)]` |
| Test data | `src/factories.rs` | `impl Factory for Record`; defaults are placeholders, override per test with `app.create_with::<T>(\|n\| n.field = ..)` |
| Tests | `tests/{resource}.rs` | `#[kiln::test] async fn ..(app: TestApp) -> Result` |
| The app's own `kiln check` rules | `src/lints/{what_it_enforces}.rs` | `#[lint(code = "A0001")] pub fn rule(file: &File, report: &mut Report)` with `use kiln::lint::*`; the doc comment is its `kiln explain` text; test it with `kiln::lint::check(rule, path, source)` |
| Migrations | `db/migrations/*.sql` | generated by `kiln db diff`; never hand-write the schema |

## Rules that bite

- **SQL is written once, for every backend:** queries built with `Record::query()` and filters
  work on SQLite and Postgres alike. Migrations are generated for the app's backend (the file's
  `-- kiln: backend` line) and won't run on another; regenerate rather than hand-port them. In raw
  SQL use `?` placeholders and double-quoted identifiers, and avoid backend-only functions
  (`MAX(a, b)`, `IFNULL`, `ILIKE`): `CASE WHEN`, `COALESCE`, `LOWER(..) LIKE LOWER(?)` work everywhere.
  Booleans are stored as 0/1 integers.

- **Relations:** `#[record(belongs_to(author: User), has_many(comments: Comment), has_one(profile: Profile))]`;
  `has_many`/`has_one` expect `{record}_id` on the other table (`comments.post_id`), which may
  be `Option<Id<..>>` (with `one_of`). `has_one` gives an `Option`.
- **Relations exist only when preloaded:** `.preload(post::author)` before `post.author()`,
  and the view's field type lists them: `Loaded<Post, (post::author,)>`. Templates read
  `post.author.name`.
- **Constraints live on the record,** so `kiln db diff` writes them: `#[index]` /
  `#[index(unique)]` on a field; `#[record(unique(event_id, user_id))]` or `index(a, b)` for
  several columns, with `where = "deleted_at IS NULL"` for a partial index;
  `#[on_delete(cascade)]` (or `set_null`) on a `belongs_to` key so rows go with their parent
  instead of blocking its delete. No polymorphic `(type, id)` pairs: give each possible parent
  its own nullable foreign key and `#[record(one_of(work_item_id, commit_id))]` (exactly one
  is set), or give both kinds one shared parent (`namespaces` with `person_id`/`org_id` under `one_of`, and
  `has_one(namespace: Namespace)` on each).
- **Links are typed:** `posts::show::path(&post.slug)` in Rust,
  `{{ path("posts::show", slug=post.slug) }}` in templates.
- **Permissions are policies:** `impl Allows<A, Post> for User` in `src/policies/post.rs`,
  where `A` is `Read`, `Create` (checked against `NewPost`), `Update` or `Delete` from
  `kiln::policy` (or your own unit struct, e.g. `Publish`). `user.authorize::<Update, _>(post)?`
  gives `Authorized<Update, Post>` or a 403; ops take the proof, and `kiln check` (K0301)
  flags writes without one. For showing UI: `user.can::<Update, _>(&post)`. Other users get
  403, not 404, on records they may not touch.
- **Signed-in user, in Rust:** an action takes `user: Current<User>` (or
  `Option<Current<User>>` for pages that work signed out). Ops never look the user up; they
  get it, or an `Authorized<..>` proof, as an argument.
- **Signed-in user, in templates:** every template, including layouts, can read
  `current_user` (none when signed out): `{% if current_user %}{{ current_user.name }}{% endif %}`.
  It's the allowlisted `CurrentUser` shape from `src/views/current_user.rs`, never the whole
  record; to show another field, add it there. It's loaded only for pages whose templates
  use it, and reuses the user if the action took `Current<User>`. Don't put `User` records
  in view structs just to show the signed-in user.
- **Who signs in** is registered in `src/app.rs`:
  `app.signs_in::<User, _>(sessions::new::path(), CurrentUser::of)`. Separate account types
  (a marketplace's `Merchant`) get their own registration, sign-in page, `current_merchant`
  value and session; one person in several roles (a course's student and instructor) is one
  `User` plus records such as `Enrollment`, checked in policies.
- **Sign-in redirects:** a signed-out browser asking for a page (GET with
  `Accept: text/html`) whose action takes `Current<T>` is sent to `T`'s sign-in page; every
  other request gets 401. `#[kiln::test]` applies `src/app.rs`, so tests see the same
  behavior: `app.sign_in_as(&user)` signs a client in, `get_html(..)` acts as a browser,
  `get(..)` as an API client.
- **Forms:** `form.validate().or_render(|form| New { form })?` re-renders with errors (422).
  Use `form.validate_with(&ctx).await` when the form implements `CheckWith` (database checks).
  Submitted fields that aren't on the form are rejected with 400 (K1005), so an owner id
  can't be forged; set such values in `into_new`, not the form.
  Field attributes:
  - `#[validate(length(min = 1, max = 200))]`, `range(min = 1, max = 5)`, `email`, `required`
  - `#[form(textarea)]`, `#[form(label = "Starts at")]`, `#[form(hint = "..")]`, `#[form(placeholder = "..")]`

  In templates, never write raw `<form>` tags. Use `{{ form_start(path("posts::update", slug=post.slug), method="patch") }}`, then `{{ fields(form) }}` (or `{{ field(form.title, label="..") }}`), then `{{ form_end() }}`. These add the CSRF token, the method override, labels, errors and the `required`/`maxlength` attributes.
- **Time:**
  - `Timestamp` values are UTC.
  - Build them with `Timestamp::now()`, `Timestamp::days_from_now(7)`, `hours_ago(2)`, or `ts + kiln::time::SignedDuration::from_mins(30)`.
  - Compare in queries with `event::starts_at.gt(Timestamp::now())`.
  - Display with `{{ event.starts_at | datetime }}` (`Oct 12, 2026 09:30 UTC`; the default format ends in `UTC`) or `{{ event.starts_at | datetime("%b %-d") }}`.
  - Form time inputs are read as UTC.
- **API routes** (`pipeline = Api`) take JSON or form bodies through the same `Form<T>`, return
  JSON problem reports on errors, and never use the session cookie. API auth (tokens) isn't
  available yet.
- **Turbo is on.** Links and forms navigate without full reloads (Turbo Drive), and kiln's
  303 redirects and 422 re-renders are what Turbo expects, so normal actions need nothing
  special. For in-place updates, take `turbo: Turbo` and return
  `turbo.stream_or_redirect(TurboStream::new().append("comments", Item { comment }), Redirect::to(..))`:
  Turbo requests get the stream, plain forms the redirect. Stream actions: `append`,
  `prepend`, `replace`, `update`, `before`, `after`, `remove`, `refresh`. Give list items
  stable ids (`id="comment_{{ comment.id }}"`) so appends don't duplicate. Frame requests
  (`<turbo-frame id="new_comment">`) render pages without the layout.
- **Realtime:** in an op, `tx.broadcast(&post::stream_name(post.id), TurboStream::new().append(..))`
  inside the same `ctx.db.transaction` as the write (a rolled-back write never
  broadcasts); in the page, `{{ turbo_stream_from("post:" ~ post.id) }}` (it renders a signed
  `<turbo-stream-source>` tag; assert on `turbo-stream-source` in page tests). Keep the stream
  name in one helper (`post::stream_name(post_id: Id<Post>)`; don't name the parameter `id`,
  which is the record module's column constant) so Rust and templates agree. Test the page with
  `page.assert_streams_from("post:1")`. Broadcasts go to
  everyone, so their views can't use `current_user`. Reconnecting pages get what they
  missed; older gaps refresh the page.
- **Testing Turbo and realtime:** `client.turbo_post_form(..)` submits like Turbo;
  `app.assert_broadcast("post:1", "text").await?`; `client.read_stream(url, None, |t| ..)`
  reads a page's stream.
- **Jobs:** enqueue inside the write's transaction (`Job { post_id }.enqueue(&tx).await?`)
  so a rolled-back write never runs its job; `enqueue_in(&db, SignedDuration::from_mins(5))`
  delays it. Store ids, not records, and reload in `perform`; jobs retry with backoff and
  may run twice, so make them safe to repeat. The web process runs jobs too; for a
  separate worker use `web: WORKERS=0 $APP serve` and `worker: $APP work` in `Procfile.dev`.
- **Job options:** `#[job(queue = "imports", priority = 10, timeout = "30s", attempts = 5, unique_for = "10m")]`.
  Higher priority runs first; an attempt past `timeout` is stopped and retried. Cap a queue
  across all processes with `app.queue_limit("imports", 2)` in `src/app.rs`. Running jobs can be
  cancelled (`kiln jobs cancel <id>`, `/kiln/jobs`): they stop at their next `.await`.
  On SIGTERM workers finish within `JOB_SHUTDOWN_GRACE` (25s in production) and put
  unfinished jobs back.
- **Jobs act as the system.** A job that changes a policy-guarded record calls an op with
  `kiln::policy::as_system::<Update, _>(record)` (never in actions: K0304). `#[job(unique_for = "10m")]`
  collapses duplicate enqueues.
- **Schedules:** `s.cron("0 3 * * *", Job {})` (UTC), `s.cron_in("0 9 * * mon", "America/New_York", ..)`,
  `s.every(SignedDuration::from_mins(15), ..)`; one schedule per job type. Missed runs run once, not
  once per slot. Test with `app.travel(SignedDuration::from_hours(25))`, then `app.run_schedules().await?`
  (enqueues what's due) and `app.perform_due_jobs().await?`.
- **Batches:** `Batch::new("Import").jobs(rows.map(..)).on_success(J).on_failure(J).on_complete(J).on_cancel(J).enqueue(&tx)`
  returns a `BatchId`; hooks are jobs and fire once. Inside a batch's job or hook, `BatchId::current()`;
  jobs fan out with `batch.add(&ctx.db, job)`, hooks read `batch.status(&ctx.db)`. A `BatchId` can be a
  record field (`pub import_batch: Option<BatchId>`); show progress with `ctx.db.batch(id).await?`, a
  `BatchStatus { status, total, pending, succeeded, failed, cancelled, .. }`. An empty batch finishes
  (and succeeds) at once. `.on_progress(J)` runs `J` after jobs finish (at most one waiting), with
  `status` already counting the job that just ended: broadcast a progress bar from it.
- **`/kiln` in production** is off unless `app.kiln_tools_for::<User>(|user| user.admin)` in `src/app.rs`;
  then only those users see it (others get a 404), and every control used is logged.
- **Test helpers** (`kiln::testing`), all you need without reading the source:
  `TestApp`: `create::<T>()`, `create_with::<T>(|n| ..)`, `sign_in_as(&user)` / `client()` (a
  `TestClient`), `ctx()`, `perform_enqueued_jobs()`, `perform_due_jobs()`, `travel(..)`,
  `run_schedules()`, `enqueued::<J>()` / `assert_enqueued::<J>(n)`, `notified::<N>()` /
  `assert_notified::<N>(&to, n)`, `outbox()` / `assert_mailed(to, text)`,
  `render_mail_previews()`, `broadcasts(stream)` / `assert_broadcast(stream, text)`.
  `TestClient`: `get`, `get_html`, `post_form`, `patch_form`, `put_form`, `delete`,
  `turbo_post_form`, `post_multipart(path, fields, &[("cover", Upload::png("c.png"))])`,
  `patch_multipart`, `post_json`, `direct_upload(Upload)`, `request(Method, path, accept)`.
  `TestResponse`: `assert_status`, `assert_contains`, `assert_not_contains`,
  `assert_redirect`, `assert_flash`, `assert_streams_from`, `json()`, `.text`, `.flash`,
  `.error_report` (a failing page's Markdown report: print it when a status is wrong).
- **Testing jobs:** `app.assert_enqueued::<SendDigest>(1).await?`, `app.enqueued::<J>()`, then
  `app.perform_enqueued_jobs().await?` runs them all now, even ones scheduled for later (an error if
  one fails for good); `app.perform_due_jobs()` runs only those due.
- **Time in tests:** `app.travel(SignedDuration::from_hours(24 * 91))`, `app.travel_to(ts)`, `app.travel_back()`
  move `Timestamp::now()` for the test, its requests and the jobs it performs (not threads it spawns).
  Call ops directly with `app.ctx()`: `ops::publish_post(app.ctx(), proof).await?`.
- **Sign-ins are database rows** (`kiln_sessions`); the cookie holds only a token.
  `session.sign_in(&ctx.db, &user).await?` / `session.sign_out::<User>(&ctx.db).await?`;
  `session.sign_ins(&ctx.db, &*user)` lists devices, `session.revoke(&ctx.db, &*user, id)`
  signs one out, `kiln::auth::sign_out_everywhere(&ctx.db, &user)` after a password change.
- **Passwords:** `kiln::auth::hash_password` / `check_password(password, Some(digest))`
  (pass `None` when the email has no account, so timing doesn't leak). Mark the digest
  `#[hidden]` on the record so templates and JSON never see it; use `#[form(password)]`
  inputs, which are never re-rendered. Emailed links use
  `ctx.tokens().make("purpose", user.id, bound_to, ttl)` and `tokens.read::<User>(..)` +
  `tokens.matches(token, bound_to)`; binding to the password digest makes a token single-use.
  Factories and seeds give users the password `password`.
- **Mail:** `Welcome { to, .. }.deliver_later(&tx).await?` inside the write's transaction;
  it renders now and a job delivers it. `mail.deliver_now(&ctx).await?` sends immediately (no job; for
  jobs that are themselves the delivery). Links in mail must be absolute:
  `{{ url(path("posts::show", slug=post.slug)) }}` (base: `APP_URL`). `MAIL_URL` picks the
  transport (`smtp://user:pass@host:587`, `smtps://..`); in development and tests mail
  goes to the outbox: browse it at `/kiln/mail`. `MAIL_FROM` sets the sender.
- **Confirmations:** `{{ button_to("Delete", path("posts::destroy", slug=post.slug), method="delete",
  confirm="Delete this post?", confirm_button="Delete", tone="danger") }}`, or `confirm=` on
  `form_start(..)`. Kiln shows its own dialog; restyle `.kiln-confirm` (its defaults are in
  `@layer kiln`), supply `<template id="kiln-confirm">` in the layout, or replace it with
  `import { setConfirm } from "kiln/confirm"`.
- **Live reload (development):** pages update as you save. CSS swaps in place, template edits
  refresh the page (scroll kept), JS edits reload, and Rust edits reload once `kiln dev` restarts.
- **Files:** `pub cover: Option<Attachment>` on the record and the form, with
  `#[validate(file(max_size = "5MB", accept = "image/*"))]`, and `form_start(.., multipart=true)` (the
  generator does all three for `file`/`image` fields). With JavaScript, file fields upload straight to storage on submit (direct uploads: `POST /kiln/direct_uploads`, then a `PUT` to the store or, for local disk, to Kiln; S3/R2 buckets need CORS allowing `PUT` from the app's origin) and the form sends the blob's `upload_token` as the field's value; without it they post as multipart and are stored as the request arrives. Test either way: `client.post_multipart(..)`, or `let blob = client.direct_upload(Upload::png("c.png")).await;` then submit `("cover", blob["upload_token"].as_str().unwrap())` in `post_form`. From the CLI or MCP: `request POST /posts --form title=Hi --file cover=./c.png`. Uploads are typed by their bytes: a file claiming to be a PNG that isn't one fails `accept = "image/*"`. A form
  re-rendered with errors, and an edit form, keep the current file (a signed hidden field) unless a new
  one is chosen. Show with `{{ post.cover.url }}`, `.filename`, `.content_type`, `.byte_size`,
  `.is_image`; resized: `{{ post.cover | variant(width=400) }}`, `variant(width=200, height=200, crop=true)`
  (made on first request, kept in storage; never enlarged). In Rust: `ctx.attach("report.pdf", "application/pdf", bytes).await?` stores a file a job
  made; `ctx.storage()?.read(&attachment)` reads one. Links (`/kiln/files/..`) work for anyone who has
  them and can't be guessed; HTML and SVG always download. `STORAGE_URL` picks the store (`file://storage`
  by default; `s3://bucket` with `AWS_*` for S3/R2/MinIO; `gs://bucket`), `MAX_UPLOAD_SIZE` caps uploads
  (25MB). Files no record holds are removed after two days. Tests: `client.direct_upload(Upload::png("a.png"))` returns the blob (`upload_token` is what a form submits), `client.post_multipart(path, &[..],
  &[("cover", Upload::png("cover.png"))])`, `patch_multipart`; each test has its own in-memory store.
- **Cache:** `ctx.cache().fetch("key", SignedDuration::from_mins(5), || async { .. }).await?` computes on a
  miss (values are `Serialize + Deserialize`); `get::<T>`, `set(key, &value, ttl)`, `delete`, `delete_prefix("post:12:")`,
  `increment(key, 1, ttl)` (atomic counters, e.g. rate limits), `clear`. It lives in the app's database (`kiln_cache`), so
  there's nothing to run. Put a version or `updated_at` in keys whose contents change (`format!("post:{}:{}", post.id,
  post.updated_at)`) instead of remembering to delete. `/kiln/cache` lists and clears entries.
- **Pending migrations (development):** a database error caused by unapplied migrations lists them with a
  "Run migrations" button. `app.stop_on_pending_migrations()` in `src/app.rs` shows that page for every
  request until they're applied.
- **Rich text attachments:** images and files dropped or pasted into the editor upload directly (`/kiln/direct_uploads`)
  and are stored in the HTML as `<kiln-attachment>` elements with signed `/kiln/files/` links;
  `| rich_text` shows them as figures (dropping any link Kiln didn't sign), and the files are kept while
  a rich text column links them.
- **Rich text:** a `RichText` field (records and forms) renders the editor with `{{ field(form.body) }}`
  and is stored as sanitized HTML in a TEXT column; show it with `{{ post.body | rich_text }}`
  (never `| safe`). `#[validate(length(min = 1))]` counts words, not markup. `post.body.to_plain_text()`
  in Rust or `{{ post.body | plain_text }}` in templates for excerpts. In mail, `| rich_text` leaves out
  the page stylesheet and scripts; the mail layout styles it.
- **Mail previews:** add `preview` to `#[mail(..)]` and `impl Preview for X { async fn preview(ctx: &Ctx) -> Result<Self> { .. } }`;
  test them all with `app.render_mail_previews().await?`.
- **Notifications:** `CommentPosted { comment_id }.notify(&tx, &author).await?`. In `deliver`,
  `via.inbox("title").link(path)`, `via.mail(Mail { .. })?`, `via.broadcast(stream, turbo)`.
  Channels people turned off at `/settings/notifications` are dropped by `via` itself; don't
  re-check them. Inbox: `db.inbox(&user, 20)`, `db.unread_notifications(&user)`,
  `db.read_notification(&user, id)` (marks it read, returns it to follow `url`),
  `db.mark_all_notifications_read(&user)`. The first `kiln g notification` also writes the `/notifications`
  inbox page; link it from the layout. Test with `perform_enqueued_jobs` then `db.inbox`.
- **Testing mail:** `app.perform_enqueued_jobs().await?`, then
  `app.assert_mailed("ada@example.com", "Welcome").await?` (returns the message), or
  `app.outbox()`; `mail.message()?` renders without sending.
- **Config:** each field reads its env var (`smtp_url` → `SMTP_URL`), then the encrypted
  secrets, then `#[config(default = "..")]`, then (outside production) `dev_default`.
  `Option<T>` fields are optional; wrap credentials in `Secret<T>` (prints `[redacted]`;
  `.expose()` reads it). Missing values fail at boot, all listed at once.
- **Templates are strict:** an undefined value is an error, not an empty string. Output is
  HTML-escaped, including `/` inside computed strings (`{{ a ~ "/5" }}` renders `&#x2f;`);
  put literal text outside the braces: `{{ book.rating }}/5`.
- **Inside a record's own file**, column and relation names are constants (`title`,
  `owner`), so a binding or parameter with one of those names won't compile there; pick
  another name (the generated `owned_by` uses `who`).
- **Queries:** filters `eq`, `ne`, `lt`, `lte`, `gt`, `gte`, `like`, `is_null`, `is_not_null`, combined with
  `.and(..)` / `.or(..)`; then `.order(..)`, `.limit(n)`, and `.all`, `.one`, `.one_or_404`, `.count`, `.exists`,
  `.delete_all` (each takes `&ctx.db`). A request that clashes with a record's state (already sent) is
  `Err(Error::Conflict("Already sent.".into()))`: a 409. For a browser, turn it into a flash:
  `Redirect::to(back).notice_or_alert(ops::send(&ctx, x).await, "Sent.")`.
- **Errors are reports** with stable codes (`K1003`, `K0202`, ...). Fetch any failing URL
  without `Accept: text/html` to get the report as Markdown. `kiln check` prints the first
  compiler error in full; fix that one first.

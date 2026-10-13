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
| Notification (inbox + email + live) | `kiln g notification comment_posted [--to User]` (adds `/settings/notifications` the first time) |
| Passwords, sign-up, resets, sign-in list | `kiln g auth` (once; replaces the demo sign-in) |
| Schema change | edit the record struct → `kiln db diff <snake_case_name>` → review the SQL → `kiln db migrate` |
| Verify | `kiln check` (must end `0 errors`), then `kiln test` |
| Routes | `kiln routes` |
| Run | `kiln dev` (port 3000; `--port 4000` for another): runs `Procfile.dev`, rebuilds and restarts on src/ changes |
| Jobs | `kiln jobs` (counts, queues, failures), `kiln jobs retry [id]`, `cancel <id>`, `run <id>`, `pause <queue>`, `resume <queue>`, `discard-failed` |
| Schedules, batches | `kiln schedules [pause\|resume\|run <job>]`, `kiln batches [cancel <id>]` |
| Inspect the running app | `/kiln` in development: routes, records, jobs, schedules, batches, mail, broadcasts, sign-ins, each searchable, with controls (retry, cancel, pause, run now) |
| Secrets | `kiln secrets set SMTP_URL smtp://..`, `kiln secrets show` (key in `config/master.key`, never committed) |
| Fresh database | `kiln db reset` (migrate + seed); sign in at /session/new as ada@example.com |

Field types: `string`, `text`, `rich_text` (the editor; `?` means it may be left empty), `integer`, `float`, `bool`, `timestamp`, `ref:<Record>`. A
trailing `?` makes a field optional; quote it in zsh (`'notes:text?'`).

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
| Migrations | `db/migrations/*.sql` | generated by `kiln db diff`; never hand-write the schema |

## Rules that bite

- **Relations exist only when preloaded:** `.preload(post::author)` before `post.author()`,
  and the view's field type lists them: `Loaded<Post, (post::author,)>`. Templates read
  `post.author.name`.
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
  - Display with `{{ event.starts_at | datetime }}` or `{{ event.starts_at | datetime("%b %-d") }}`.
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
  broadcasts); in the page, `{{ turbo_stream_from("post:" ~ post.id) }}`. Keep the stream
  name in one helper (`post::stream_name`) so Rust and templates agree. Broadcasts go to
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
  once per slot. Test with `app.run_schedules_at(Timestamp::now() + SignedDuration::from_hours(25))`.
- **Batches:** `Batch::new("Import").jobs(rows.map(..)).on_success(J).on_failure(J).on_complete(J).on_cancel(J).enqueue(&tx)`
  returns a `BatchId`; hooks are jobs and fire once. Inside a batch's job or hook, `BatchId::current()`;
  jobs fan out with `batch.add(&ctx.db, job)`, hooks read `batch.status(&ctx.db)`.
- **`/kiln` in production** is off unless `app.kiln_tools_for::<User>(|user| user.admin)` in `src/app.rs`;
  then only those users see it (others get a 404), and every control used is logged.
- **Testing jobs:** `app.assert_enqueued::<SendDigest>(1).await?`, `app.enqueued::<J>()`, then
  `app.perform_enqueued_jobs().await?` runs them all now (an error if one fails for good).
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
  it renders now and a job delivers it. Links in mail must be absolute:
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
- **Rich text:** a `RichText` field (records and forms) renders the editor with `{{ field(form.body) }}`
  and is stored as sanitized HTML in a TEXT column; show it with `{{ post.body | rich_text }}`
  (never `| safe`). `#[validate(length(min = 1))]` counts words, not markup. `post.body.to_plain_text()`
  for excerpts. Attachments are off until file storage exists.
- **Mail previews:** add `preview` to `#[mail(..)]` and `impl Preview for X { async fn preview(ctx: &Ctx) -> Result<Self> { .. } }`;
  test them all with `app.render_mail_previews().await?`.
- **Notifications:** `CommentPosted { comment_id }.notify(&tx, &author).await?`. In `deliver`,
  `via.inbox("title").link(path)`, `via.mail(Mail { .. })?`, `via.broadcast(stream, turbo)`.
  Channels people turned off at `/settings/notifications` are dropped by `via` itself; don't
  re-check them. Inbox: `db.inbox(&user, 20)`, `db.unread_notifications(&user)`,
  `db.mark_notification_read(&user, id)`. Test with `perform_enqueued_jobs` then `db.inbox`.
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
- **Errors are reports** with stable codes (`K1003`, `K0202`, ...). Fetch any failing URL
  without `Accept: text/html` to get the report as Markdown. `kiln check` prints the first
  compiler error in full; fix that one first.

//! `kiln explain <code>`: the long form of a `kiln check` finding or an error page, like
//! `rustc --explain`. `K` codes are Kiln's and written here; any other code is one of the
//! app's own rules (`#[lint]` in src/lints/), explained by its doc comment.

use crate::app::App;

/// `(code, title, explanation)`. Every code Kiln emits has an entry (a test checks).
pub const CODES: &[(&str, &str, &str)] = &[
    (
        "K0001",
        "The app doesn't compile",
        "`kiln check` starts with `cargo check --all-targets`. If that fails, the first compiler
error is printed in full and the rest as one line each: later errors are often caused by
the first, so fix it and re-run before reading the others.

The convention checks still run on code that doesn't compile, but the schema, template
and app-rule checks need the app binary, so they're skipped until it builds.",
    ),
    (
        "K0002",
        "The app binary couldn't report its schema status",
        "`kiln check` runs the app's `db status --json` to compare the record structs with
`db/snapshot.json` and the development database. That command failed.

Run it yourself to see why: `cargo run -- db status`. A bad `DATABASE_URL`, an unreachable
Postgres server or a panic in `src/app.rs` are the usual causes.",
    ),
    (
        "K0003",
        "The app's own rules couldn't run",
        "The app has `src/lints/`, so `kiln check` runs its rules through the app binary
(`cargo run -- lint`), and that failed. Run it yourself to see the error.

A rule that panics is reported against the rule, not this code; this one means the binary
itself didn't start.",
    ),
    (
        "K0101",
        "A file is in the wrong directory",
        "Each kind of item has one home, so people and agents always know where to look and
generated code always fits:

  records        src/records/<record>.rs   (one per file, named after the struct)
  forms          src/forms/
  views          src/views/<resource>/<verb>.rs
  parts          src/parts/
  actions        src/actions/<resource>/<verb>.rs
  app's rules    src/lints/

A record must be in its own file because `#[derive(Record)]` generates its columns and
relations as items of the enclosing module (`post::title`, `post::author`): two records in
one module would collide.

Move the item to the directory named in the finding and update its `mod` and `use` lines.",
    ),
    (
        "K0102",
        "More than one action in a file",
        "Each `#[action]` gets its own file, `src/actions/<resource>/<verb>.rs`. The file's module
is where the action's typed path helper lives, so `posts::show::path(&slug)` names exactly
one route, and the route table, `kiln routes` and error pages can point at one file.

Split the file: one action per file, each registered in `src/actions/<resource>/mod.rs`.
`kiln g action <resource> <verb> <METHOD> <path>` writes one in the right shape.",
    ),
    (
        "K0103",
        "An action isn't named after its file",
        "`src/actions/posts/publish.rs` holds `pub async fn publish(..)`. The route's name
(`posts::publish`, used by `path(\"posts::publish\")` in templates and by `kiln routes`)
comes from the module, and a function with another name makes the two disagree.

Rename the function to match the file, or the file to match the function.",
    ),
    (
        "K0110",
        "Two routes match the same requests",
        "Two actions declare the same method and a path that matches the same URLs: identical, or
the same apart from a parameter's name (`/posts/{slug}` and `/posts/{id}`). The router
can't choose between them and refuses to start.

Change one path (`/posts/{id}/preview`), or merge the two actions. Static segments beat
parameters, so `/posts/new` next to `/posts/{slug}` is fine.",
    ),
    (
        "K0202",
        "The record structs changed since the last migration",
        "Record structs are the schema. `db/snapshot.json` records what the migrations have built
so far, and the structs no longer match it: a field was added, removed, renamed or
changed type.

  kiln db diff <name>     writes db/migrations/<timestamp>_<name>.sql and updates the snapshot
  kiln db migrate         applies it

Read the generated SQL before migrating: a rename looks like a drop and an add, so edit
it into a rename if that's what you meant.",
    ),
    (
        "K0203",
        "A migration isn't applied to the development database",
        "The migration file exists but the development database hasn't run it, so pages that use
the new columns will fail with \"no such column\". Run `kiln db migrate`.

In development the server can also offer a Run migrations button on its error page, and
`app.stop_on_pending_migrations()` in src/app.rs makes every request show that page until
they're applied.",
    ),
    (
        "K0204",
        "The snapshot was written with a migration that's since been deleted",
        "`db/snapshot.json` remembers the newest migration it was written with, and that file is
gone. The snapshot now describes a schema no migration builds, so the next `db diff` would
be wrong.

If you deleted the migration to redo it, `kiln db undo` takes back the newest generated
migration and restores the snapshot properly. Otherwise restore the file from version
control.",
    ),
    (
        "K0301",
        "A write to a policy-guarded record without proof of authorization",
        "When a record has a policy (`impl Allows<Update, Post> for User` in src/policies/), every
function that writes it must take an `Authorized<_, Post>` argument: the proof, produced
only by checking the policy, that the signed-in user may do this.

  // in the action
  let post = user.authorize::<Update, _>(post)?;        // 403 if the policy says no
  ops::update_post(&ctx, post, input).await?;

  // in src/ops/update_post.rs
  pub async fn update_post(ctx: &Ctx, post: Authorized<Update, Post>, input: Valid<PostForm>) -> Result<Post>

This makes \"forgot to check permissions\" a missing argument instead of a security bug.
Code that runs without a viewer (jobs, seeds) uses `policy::as_system`.",
    ),
    (
        "K0302",
        "A data-changing action has no test",
        "Every POST, PATCH, PUT or DELETE action needs a test that calls its path helper
(`posts::update::path(&slug)`), so the write, its validation and its authorization are
exercised. Include the forbidden case: another user gets a 403 and nothing changes.

`kiln g resource` and `kiln g action` write these tests; copy their shape from tests/.",
    ),
    (
        "K0304",
        "`as_system` in an action",
        "`policy::as_system` produces authorization proof without a viewer, for jobs, seeds and
other code no person is driving. In an action there is a viewer, and acting as the system
skips their policy: anyone who can reach the URL can do the write.

Authorize the signed-in record instead: `user.authorize::<Update, _>(record)?`. If the
action really has no viewer (a webhook), verify the caller another way and move the write
into a job or op that documents why.",
    ),
    (
        "K0401",
        "A template reads something its view doesn't provide",
        "A page's template sees exactly its view struct's fields (plus `flash`, `current_user`
and the template functions). A record in a field carries its columns, its parts
(src/parts/) and the relations its `Loaded<..>` type names. This template reads a
variable the view has no field for, or an attribute the record has no column or part
for, so the page would fail when it renders (templates are strict about undefined
values).

Fix the spelling, add the field to the view struct and set it in the action, or add a
part for a computed value:

  #[part] impl Post { pub fn word_count(&self) -> usize { .. } }

`kiln introspect` lists each view's fields and each record's columns and parts.",
    ),
    (
        "K0402",
        "A template links to a route that doesn't exist",
        "`path(\"posts::show\", slug=post.slug)` in a template names routes as
`<resource>::<verb>`, the module path of the action under src/actions/. This one matches
no route, so the link would fail when the page renders.

`kiln routes` lists every route's name. Check the spelling, or add the action.",
    ),
    (
        "K0403",
        "A template includes or extends a template that doesn't exist",
        "`{% include \"comments/_comment.html.jinja\" %}` and `{% extends .. %}` name templates
relative to templates/. This one isn't there, so the page would fail when it renders.

Check the path, including the `.html.jinja` extension, or create the template.",
    ),
    (
        "K0404",
        "A template uses a Stimulus controller that doesn't exist",
        "Kiln registers every file in assets/controllers/ as a Stimulus controller, named after
its path: `hello_controller.js` is `data-controller=\"hello\"`, `admin/users_controller.js`
is `admin--users`, `bulk_edit_controller.js` is `bulk-edit`. This `data-controller` names
none of them, so the element would silently do nothing.

Fix the name, or create the controller with `kiln g controller <name>`. A controller
registered by hand in the app's JavaScript (`application.register(\"sortable\", Sortable)`
with `import { application } from \"kiln/stimulus\"`) counts too.",
    ),
    (
        "K0405",
        "An import-map pin won't load",
        "config/importmap.toml maps module names to a file under assets/ or a URL:

    \"@pierre/diffs\" = \"vendor/npm/@pierre/diffs@1.5.2.js\"
    \"canvas-confetti\" = \"https://cdn.jsdelivr.net/npm/canvas-confetti@1.9.3/+esm\"

This entry isn't valid TOML, isn't a string, or points at a file that isn't there, so every
page's `{{ javascript_importmap() }}` would fail to render. Re-run
`kiln importmap pin <package>` to download the package again, fix the path, or remove the
line (`kiln importmap unpin <package>`).",
    ),
    (
        "K0412",
        "A template reads a relation the view didn't load",
        "Relations are loaded only when the view's type says so, which is what makes N+1 queries
impossible: `Loaded<Post, (post::author,)>` has `post.author`; plain `Post` has no
relations at all. The template reads a relation the field's type doesn't name, so the
page would fail when it renders.

Add it to the field's type and load it in the action:

  pub post: Loaded<Post, (post::author, post::comments)>,
  // nested: post::comments<(comment::author,)>

Loops are followed one level, so `{% for c in post.comments %}{{ c.author.name }}` needs
`post::comments<(comment::author,)>`.",
    ),
    (
        "K0501",
        "Raw `sqlx` outside src/records/",
        "Queries go through the record API (`Post::query()`, `Post::update(id)`, `Loaded<..>`),
which is typed, portable between SQLite and Postgres, and lets Kiln catch N+1 queries at
compile time.

When SQL is the right tool, put it in the record's own file (src/records/post.rs) as a
method, so every query on a table is in one place, and call that method elsewhere.",
    ),
    (
        "K0601",
        "An environment variable read outside src/main.rs",
        "App settings come from a `#[derive(Config)]` struct (src/config.rs): each field is read
from its environment variable, then `config/secrets.enc`, then its default, and the app
fails at boot with a clear message if a required one is missing. `std::env::var` in app
code skips all of that and fails later, in a request.

Add a field to the Config struct and read it with `ctx.config::<Config>()`.",
    ),
    (
        "K0701",
        "A job, mail or notification inside a transaction goes on the pool",
        "Inside `db.transaction(|tx| ..)`, enqueue on the transaction: `.enqueue(&tx)`,
`.deliver_later(&tx)`, `.notify(&tx, ..)`. Kiln holds those until the transaction commits
and drops them if it rolls back. Enqueued on the pool (`&ctx.db`), the job is stored at
once: a worker can run it before the write it depends on is visible, or after the write
was rolled back, and it then fails or acts on data that doesn't exist.",
    ),
    (
        "K1001",
        "No route matches the request",
        "No action declares this method and path. Declare one with
`#[action(GET \"/path\")]` in src/actions/<resource>/<verb>.rs (or `kiln g action`), and
link to it with its generated `path(..)` helper so the two can't drift.

In development the error page lists every route. Visitors see a plain 404 in production.",
    ),
    ("K1002", "Not found", "An action returned `Error::NotFound`. Visitors see a plain Not Found page in production."),
    (
        "K1003",
        "Record not found",
        "A query that needs a row (`one_or_404`, `find`) found none. Check the path parameter and
any filters in the action's query; an owner scope that excludes the row also lands here,
which is usually what you want (a 404 reveals less than a 403).",
    ),
    (
        "K1004",
        "Method not allowed",
        "The path exists, but not for this method: a form posting to a GET route, or a link to
a PATCH route. The `Allow` header lists the methods it accepts.

Browsers only send GET and POST; Kiln forms add `_method` for PATCH and DELETE, so use the
form helpers rather than a hand-written `<form method=\"patch\">`.",
    ),
    (
        "K1005",
        "Bad request",
        "The request couldn't be read: a path parameter that doesn't parse into the type the
action declares (`id: Param<i64>` given `abc`), a malformed body, or an action returning
`Error::BadRequest(..)`. Build links with the generated `path(..)` helpers so they always
match the route.",
    ),
    (
        "K1006",
        "Invalid CSRF token",
        "Every non-GET browser request must carry the session's CSRF token, so another site can't
submit forms as your users. Put `{{ csrf_field() }}` inside every
`<form method=\"post\">`, or send the token in an `X-CSRF-Token` header (Turbo does this
from the layout's meta tag).

The token rotates on sign-in and sign-out, so a form opened before either needs a reload.",
    ),
    (
        "K1007",
        "Sign-in required",
        "The action takes `Current<User>` (or another signed-in record) and nobody is signed in.
Browsers are redirected to the sign-in page registered with `signs_in::<..>(..)` in
src/app.rs; API clients get a 401.

If the page should work signed out, take `Option<Current<User>>` instead.",
    ),
    (
        "K1008",
        "Not allowed",
        "`authorize::<Action, _>(record)` asked the policy, and `impl Allows<Action, Record>` in
src/policies/ returned false. That's the policy working; if it's wrong, change the policy
and add a test for both the allowed and the refused case.",
    ),
    (
        "K1009",
        "Rendered in place of the normal response",
        "Not a failure: an action returned a page in place of its usual response, most often a
form re-rendered with its errors (status 422) through `.or_render(..)` or
`Error::unprocessable(&view)`. No error page is shown.",
    ),
    (
        "K1010",
        "Invalid input",
        "A `Valid<Form>` was required and the input failed validation, and nothing re-rendered the
form. For browsers, re-render it with its errors:

  let input = form.validate().or_render(|form| New { form })?;

JSON clients get a 422 with the errors by field.",
    ),
    (
        "K1011",
        "Conflict",
        "An action returned `Error::Conflict(..)`: the request is well-formed but clashes with the
record's current state (an issue already sent, a seat already taken). It responds 409 with
the message. For browsers, a redirect back with a flash message is often kinder.",
    ),
    (
        "K1101",
        "Template error",
        "A template failed to render: an undefined variable or attribute, a filter given the
wrong type, or a syntax error. The development error page shows the template, the line
and the expression that failed.

Templates only see the view struct's fields (and parts on records). `kiln introspect`
lists each view's fields; a relation must be in the view's `Loaded<..>` type to be read.",
    ),
    (
        "K1201",
        "Database error",
        "The database refused a query. The message comes from SQLite or Postgres. Common causes:

  no such table / column     the database is behind the record structs: `kiln db migrate`
                             (or `kiln db diff <name>` first if no migration exists)
  UNIQUE constraint failed   a unique index rejected the write; validate first
  FOREIGN KEY failed         the parent row doesn't exist",
    ),
    (
        "K1202",
        "Data integrity error",
        "A relation points at a row that no longer exists, found while loading `Loaded<..>`.
Check deletes on the parent table: delete or reassign children first, or make the
relation optional.",
    ),
    (
        "K1203",
        "Pending migrations",
        "Migrations exist that the database hasn't applied. Run `kiln db migrate`, or press Run
migrations on the development error page.

By default the server starts anyway and only requests that touch the new schema fail;
`app.stop_on_pending_migrations()` in src/app.rs makes every request show this page
instead.",
    ),
    (
        "K1301",
        "I/O error",
        "Reading or writing a file failed: storage/ not writable, a missing template directory
under `KILN_ROOT`, or a full disk. The message names the operation.",
    ),
    (
        "K1401",
        "Config error",
        "A `#[derive(Config)]` field couldn't be read: a required value is missing or doesn't
parse. Values come from environment variables, then `config/secrets.enc`
(`kiln secrets set KEY value`), then the field's `default` (and `dev_default` outside
production).",
    ),
    (
        "K1501",
        "Job failed",
        "A background job returned an error or panicked. It's retried with backoff up to its
`attempts`; after the last it stays in `kiln_jobs` as failed. `kiln jobs` lists failures
with their errors, `kiln jobs retry [id]` queues them again, and /kiln/jobs does both in
the browser.

Jobs may run more than once, so make `perform` safe to repeat.",
    ),
];

pub fn run(app: Result<App, String>, code: Option<&str>) -> Result<bool, String> {
    print!("{}", text(app.as_ref().ok(), code)?);
    Ok(true)
}

/// What `kiln explain [code]` prints. `app` is needed only for the app's own codes.
pub fn text(app: Option<&App>, code: Option<&str>) -> Result<String, String> {
    use std::fmt::Write;
    let mut out = String::new();
    let Some(code) = code else {
        out.push_str("Kiln's codes (`kiln explain <code>` for the long form):\n\n");
        for (code, title, _) in CODES {
            let _ = writeln!(out, "  {code}  {title}");
        }
        if let Some(app) = app
            && let Some(rules) = app_rules(app)?
            && !rules.is_empty()
        {
            out.push_str("\nThe app's own rules (src/lints/):\n\n");
            for rule in rules {
                let _ = writeln!(out, "  {}  {}", rule["code"].as_str().unwrap_or_default(), first_line(&rule));
            }
        }
        return Ok(out);
    };
    let code = code.trim().to_ascii_uppercase();
    if let Some((code, title, body)) = CODES.iter().find(|(c, _, _)| *c == code) {
        return Ok(format!("{code}: {title}\n\n{body}\n"));
    }
    if code.starts_with('K') {
        return Err(format!("{code} isn't one of Kiln's codes; `kiln explain` lists them"));
    }
    let app = app.ok_or_else(|| format!("{code} isn't one of Kiln's codes; app codes need an app (run inside one)"))?;
    let rules = app_rules(app)?.ok_or("couldn't list the app's rules (`lint --list` in the app binary failed)")?;
    let Some(rule) = rules.iter().find(|r| r["code"] == code.as_str()) else {
        return Err(format!("no rule in src/lints/ has code {code}; `kiln explain` lists them"));
    };
    let doc = rule["doc"].as_str().filter(|d| !d.is_empty()).unwrap_or("(this rule has no doc comment)");
    Ok(format!(
        "{code}: the app's rule `{}` ({}, {}:{})\n\n{doc}\n",
        rule["name"].as_str().unwrap_or_default(),
        rule["level"].as_str().unwrap_or("error"),
        rule["file"].as_str().unwrap_or_default(),
        rule["line"],
    ))
}

/// The app's `#[lint]` rules, or `None` if the binary couldn't list them. Skips the build
/// when there's no src/lints/.
fn app_rules(app: &App) -> Result<Option<Vec<serde_json::Value>>, String> {
    if !app.path("src/lints").is_dir() {
        return Ok(Some(vec![]));
    }
    Ok(app
        .binary_output(&["lint", "--list"])?
        .and_then(|out| serde_json::from_str::<Vec<serde_json::Value>>(&out).ok()))
}

fn first_line(rule: &serde_json::Value) -> String {
    let doc = rule["doc"].as_str().unwrap_or_default();
    let sentence = doc.split(". ").next().unwrap_or(doc).replace('\n', " ");
    if sentence.is_empty() { rule["name"].as_str().unwrap_or_default().to_owned() } else { sentence }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, path::Path};

    use super::*;

    /// Every `"Kdddd"` in Kiln's source has an explanation, and every explanation is used.
    #[test]
    fn every_code_is_explained() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut used = BTreeSet::new();
        for dir in ["kiln/src", "kiln-macros/src", "kiln-cli/src"] {
            let mut stack = vec![root.join(dir)];
            while let Some(dir) = stack.pop() {
                for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else if path.extension().is_some_and(|e| e == "rs") && !path.ends_with("explain.rs") {
                        let text = std::fs::read_to_string(&path).unwrap();
                        for part in text.split("\"K").skip(1) {
                            if part.len() > 5 && part[..4].chars().all(|c| c.is_ascii_digit()) && &part[4..5] == "\"" {
                                used.insert(format!("K{}", &part[..4]));
                            }
                        }
                    }
                }
            }
        }
        if used.is_empty() {
            return; // Not in a checkout (a published crate): nothing to compare.
        }
        let explained: BTreeSet<String> = CODES.iter().map(|(c, _, _)| c.to_string()).collect();
        let missing: Vec<_> = used.difference(&explained).collect();
        let unused: Vec<_> = explained.difference(&used).collect();
        assert!(missing.is_empty(), "codes without an explanation: {missing:?}");
        assert!(unused.is_empty(), "explanations for codes nothing emits: {unused:?}");
        let sorted: Vec<_> = CODES.iter().map(|(c, _, _)| *c).collect();
        assert!(sorted.windows(2).all(|w| w[0] < w[1]), "keep CODES sorted");
    }
}

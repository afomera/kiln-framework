<h1>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/afomera/kiln-framework/main/assets/brand/kiln-wordmark-dark.svg">
    <img src="https://raw.githubusercontent.com/afomera/kiln-framework/main/assets/brand/kiln-wordmark.svg" alt="Kiln" height="56">
  </picture>
</h1>

A full-stack Rust web framework where the compiler is the reviewer.

Kiln gives you what a batteries-included framework does — records and migrations, forms, sessions, mail, jobs, file uploads, Turbo and Stimulus — and leans on Rust's type system to catch mistakes before they ship. A missing preload, a post updated without permission, or a link to a route that doesn't exist won't compile, or fails `kiln check`, so it never reaches production.

> Kiln is early (0.0.x). Things will change, and it isn't ready for production apps yet.

## A taste

The struct is the schema. `kiln db diff` turns changes to it into a migration you review.

```rust
#[derive(Record, Debug, Clone)]
#[record(table = "posts", belongs_to(author: User), has_many(comments: Comment))]
pub struct Post {
    pub id: Id<Post>,
    #[index(unique)]
    pub slug: String,
    pub title: String,
    pub body: RichText,
    pub status: PostStatus,
    pub author_id: Id<User>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
```

An action is a function with a route. Its arguments say what it needs, and the types carry proof: `ops::update_post` only accepts a post the user was authorized to update.

```rust
#[action(PATCH "/posts/{slug}")]
pub async fn update(ctx: Ctx, user: Current<User>, slug: Param<String>, form: Form<PostForm>) -> Result<Redirect> {
    let post = Post::find_by(post::slug.eq(&*slug)).one_or_404(&ctx.db).await?;
    let post = user.authorize::<Update, _>(post)?;
    let input = form.validate().or_render(|form| Edit { form, post: (*post).clone() })?;
    let post = ops::update_post(&ctx, post, input).await?;
    Ok(Redirect::to(posts::show::path(&post.slug)).notice("Post updated."))
}
```

## Getting started

Kiln needs Rust 1.99 or newer. Install the `kiln` command from crates.io:

```sh
cargo install kiln-framework-cli
```

Then make an app:

```sh
kiln new journal        # sign-in, a layout, a User record, its first migration, tests and AGENTS.md
cd journal
kiln db reset           # create the database, migrate, seed
kiln dev                # http://127.0.0.1:3000
kiln check              # compile + conventions + schema + template routes
kiln test
```

### Using Postgres

New apps use SQLite (`storage/development.sqlite3`) by default. For Postgres, pass `--database postgres`:

```sh
kiln new journal --database postgres
```

The app connects to `postgres://localhost/journal_development`, with tests in `journal_test`. `kiln new` migrates and seeds right away, so start a Postgres server first; Kiln creates the databases if they don't exist. To use another server, set `DATABASE_URL` and `TEST_DATABASE_URL`:

```sh
DATABASE_URL=postgres://user:pass@db.internal/journal_development kiln new journal --database postgres
```

`kiln db reset` won't drop a database on another machine unless you pass `--allow-remote`.

Generate a whole resource — record, migration, form, policy, actions, views, templates and tests — in one go:

```sh
kiln g resource Bookmark title:string url:string 'notes:text?' pinned:bool owner:ref:User
```

For a fuller app, [`examples/blog`](https://github.com/afomera/kiln-framework/tree/main/examples/blog) uses most of the framework.

## What's in the box

- **Records and queries**: typed columns and IDs, joins, preloading, transactions, row locks; SQLite by default, Postgres too
- **Migrations from your structs**, with drift detection, undo and squash
- **Actions, forms and policies**: typed routes and path helpers, validation that re-renders with your input, CSRF, sessions and flash
- **Views and templates** that are checked against what the view provides
- **Jobs, schedules and batches**, enqueued only after the transaction commits
- **Mail and notifications**, with previews
- **Auth and API tokens**, scoped and deny-by-default, with rate limits
- **Hotwire**: Turbo, realtime streams, Stimulus and import maps; live reload in development
- **Files and rich text**: uploads, direct uploads, image variants, the Lexxy editor, Markdown
- **Deploys**: `kiln build` makes a single binary, or a Docker image
- **Agent tooling**: every app ships an `AGENTS.md`, and `kiln introspect`, `kiln explain` and `kiln mcp` let coding agents see inside it

## Docs

- [Features](https://github.com/afomera/kiln-framework/blob/main/docs/features.md): the full list, and where each piece lives
- [`kiln check`](https://github.com/afomera/kiln-framework/blob/main/docs/checks.md): every check code
- [Working with agents](https://github.com/afomera/kiln-framework/blob/main/docs/agents.md)
- [Developing Kiln](https://github.com/afomera/kiln-framework/blob/main/docs/development.md)
- [Known issues](https://github.com/afomera/kiln-framework/blob/main/docs/known-issues.md)

The design is written up in the [Kiln Framework Spec](https://claude.ai/artifact/PNHemnZU3NnUq51gfW65uv).

Kiln is published on crates.io as `kiln-framework` (with `kiln-framework-macros` and `kiln-framework-cli`), since `kiln` is taken; apps still `use kiln::prelude::*` and the command is `kiln`.

## License

Kiln is © 2026 [Andrea Fomera](https://afomera.dev), dual-licensed under either of [MIT](https://github.com/afomera/kiln-framework/blob/main/LICENSE-MIT) or [Apache 2.0](https://github.com/afomera/kiln-framework/blob/main/LICENSE-APACHE), at your option. Vendored libraries keep their own licenses: Turbo (MIT) and Lexxy with what it bundles (MIT; DOMPurify under Apache 2.0), in `crates/kiln/vendor/`.

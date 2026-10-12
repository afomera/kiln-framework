# Kiln

A full-stack Rust web framework where the compiler is the reviewer. Working name.

Spec: [Kiln Framework Spec (draft 0.1)](https://claude.ai/artifact/PNHemnZU3NnUq51gfW65uv)

## Status: M0 spike

M0's exit criterion was "the blog example compiles, and N+1 is a compile error." Both hold.

| Piece | Where | State |
| --- | --- | --- |
| `#[derive(Record)]`: struct is the schema, typed columns, `New{Name}`, `CREATE TABLE` from the struct | `crates/kiln-macros/src/record.rs` | done |
| Typed queries: `Expr<T>`, `Id<T>`, `find`, `find_by`, `one_or_404`, `count`, `insert`, `update` | `crates/kiln/src/query.rs` | done |
| `Loaded<T, (rels..)>`, `preload`, nested preload, named accessors, optional `belongs_to` | `crates/kiln/src/rel.rs` | done |
| `#[action(GET "/posts/{slug}")]`, link-time route registry, typed `path(..)` helpers | `crates/kiln-macros/src/action.rs` | done |
| `#[derive(View)]` + minijinja templates, hot-reloaded in debug, strict undefined | `crates/kiln/src/view.rs` | done |
| Layouts: `layouts/app` by default, `layout = "x"` / `layout = none`, page `{% set %}` values reach the layout | `crates/kiln/src/view.rs` | done |
| Parts: `#[part] impl Post { .. }` in `src/parts/`, exposed to templates as values | `crates/kiln-macros/src/part.rs` | done |
| Error pages: HTML with "Copy as Markdown" for browsers, Markdown for agents, plain in release | `crates/kiln/src/error_page.rs` | done |
| Request logging: one structured line per request, with the action and error code | `crates/kiln/src/app.rs` | done |
| `#[derive(DbEnum)]` with a CHECK constraint | `crates/kiln-macros/src/db_enum.rs` | done |
| In-process test client | `crates/kiln/src/testing.rs` | done |

```sh
cargo run -p blog         # http://127.0.0.1:3000, in-memory SQLite with seed data
cargo test                # HTTP, query-count and compile-fail tests
```

### What the tests prove

`examples/blog/tests/ui/` holds code that must not compile, with the exact compiler output recorded:

- `n_plus_one.rs`: calling `post.comments()` on `Loaded<Post, (post::author,)>` fails with "relation `comments<_>` was not preloaded".
- `wrong_id.rs`: `Post::find(user.id)` fails, because `Id<User>` is not `Id<Post>`.
- `wrong_table_filter.rs`: a `Comment` column can't filter a `Post` query.
- `missing_param.rs`: a `{slug}` capture without a `slug: Param<_>` argument.
- `path_arg_type.rs`: `show::path("ada")` for a route whose `{id}` is `Param<i64>`.
- `missing_template.rs`, `missing_layout.rs`: a view pointing at a template or layout file that doesn't exist.
- `part_without_flag.rs`, `flag_without_part.rs`: a `#[part]` block and the record's `parts` opt-in must come together.
- `part_takes_args.rs`, `part_shadows_column.rs`: part methods take only `&self` and can't reuse a column name.

`tests/http.rs` checks that preloading posts → authors → comments → comment authors costs exactly 4 queries, whatever the row count, and that an optional relation with no keys skips its query.

### Error pages

In debug builds, any error renders a report with the status, a stable code (`K1001` no route, `K1003` record not found, `K1101` template error, ...), the request and action, a source excerpt, and fixes. Browsers get HTML with a **Copy as Markdown** button (or press `C`). Anything that doesn't send `Accept: text/html` gets the same report as Markdown:

```sh
curl localhost:3000/nowhere          # Markdown report listing every route
```

Release builds show only the status line.

## Findings to fold back into the spec

1. **Relation markers are lowercase: `post::author`, not `post::Author`.** PascalCase markers collide with the target type whenever a relation is named after its record (`user: User`), which is the most common case. Lowercase markers sit next to the column constants (`post::slug`) and never collide with record names.
2. **One record per file is now enforced by the compiler.** Columns and relations are generated as items of the record's module, so `post::slug` only works when `Post` lives in `records/post.rs`. The cost: inside a record file, `let title = ..` is read as a pattern against the `title` constant. The compiler explains this clearly, but it is a footgun. It's a candidate for a `kiln check` lint, or for moving scopes out of record files.
3. **Accessors need a trait import.** Rust doesn't allow inherent methods on `Loaded` from the app crate, so `post.author()` comes from a generated `PostRelations` trait that `records/mod.rs` re-exports. Generators should maintain that file.
4. **Templates see data, not methods.** `Loaded` serializes as the record's fields plus one key per relation, so templates use `post.author.name`. Template URLs use `{{ path("posts::show", slug=post.slug) }}`, which is checked at render time. Compile-time checking of templates is still `kiln check` work (K0401/K0412).
5. **Writes are simpler than the spec draft.** `Post::insert(new).run(&db)` and `Post::update(id).set(..).run(&db)` return the stored record. There's no separate `.returning().one()`.
6. **Compile-error quality is good, with noise.** The N+1 error names the missing relation and the fix, but rustc appends a list of `HasRel` impls that `#[diagnostic::on_unimplemented]` can't suppress.

## Not in M0

SQLite only (Postgres comes with M1's schema diffing). No transactions, pipelines, forms, policies, sessions or jobs. Tables are created from structs at boot, not through reviewed migrations. Template checks (undefined values, unknown routes) happen at render time; `kiln check` moves them to build time later.

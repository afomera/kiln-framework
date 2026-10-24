# Working with agents

Kiln is built so a coding agent can work in an app the way a person does: the compiler and `kiln check` catch its mistakes, and the tools below tell it what the app contains.

Agents working in an app start with its `AGENTS.md`. `kiln new` writes one from [the template](../crates/kiln-cli/templates/AGENTS.md); the blog has its own at [`examples/blog/AGENTS.md`](../examples/blog/AGENTS.md).

## Tools (M3)

| Piece | Where |
| --- | --- |
| `kiln introspect [--json]`: routes, records, views, forms, policies, ops, jobs, mail, config and templates, with file:line | `crates/kiln-cli/src/introspect.rs` |
| `kiln explain <code>`: the long form of every check and error-page code | `crates/kiln-cli/src/explain.rs` |
| `kiln mcp`: an MCP server with introspect, request (as any user), read-only query, mail, jobs, check, explain and test tools | `crates/kiln-cli/src/mcp.rs` |
| App-written lint rules (`#[lint]` in `src/lints/`) that `kiln check` runs | `crates/kiln/src/lint.rs` |
| Error pages in Markdown for agents, alongside the HTML ones for browsers | `crates/kiln/src/error_page.rs` |

`kiln dev --json` streams the dev server's output as JSON, and the check codes are listed in [checks.md](checks.md).

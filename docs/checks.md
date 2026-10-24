# `kiln check`

`kiln check` compiles the app and then checks what the compiler can't: conventions, the schema, and that templates name real routes, templates and controllers. `kiln explain <code>` prints the long form of any code.

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
| K0203 | (warn) Migrations not yet applied to the development database |
| K0204 | `db/snapshot.json` was written with a migration that's since been deleted (`kiln db undo` restores it) |
| K0301 | A function that writes a policy-guarded record (`Post::insert/update/delete`) takes the matching `Authorized<_, Post>` proof |
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

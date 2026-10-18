//! `kiln introspect`: the whole app in one answer, for agents and for people new to it.
//!
//! Two halves:
//! - The app binary's `introspect` (what the registries know once it's compiled): routes with
//!   their arguments and pipelines, records with columns and relations, jobs, schedules,
//!   notifications and mail previews.
//! - What only the source says: views and their templates, forms and their validations,
//!   policies, ops, mail, parts, config, the templates on disk, Stimulus controllers and
//!   import-map pins.
//!
//! `--json` prints one JSON object; without it, a summary to read. A section name
//! (`kiln introspect routes`) or `--find Post` narrows either.

use std::{fs, path::Path};

use quote::ToTokens;
use serde_json::{Value, json};
use syn::{Attribute, Fields, Item, spanned::Spanned};

use crate::app::App;

pub fn run(app: &App, section: Option<&str>, find: Option<&str>, json: bool) -> Result<bool, String> {
    let report = narrow(report(app)?, section, find)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report).expect("introspection serializes"));
    } else {
        print!("{}", summary(&report));
    }
    Ok(true)
}

/// Just one section, and/or the items that mention `find` (case-insensitive). The app's
/// name and backend stay.
pub fn narrow(mut report: Value, section: Option<&str>, find: Option<&str>) -> Result<Value, String> {
    let Some(map) = report.as_object_mut() else { return Ok(report) };
    if let Some(section) = section {
        if !map.get(section).is_some_and(Value::is_array) {
            let sections: Vec<&str> = map.iter().filter(|(_, v)| v.is_array()).map(|(k, _)| k.as_str()).collect();
            return Err(format!("no section `{section}`; there are: {}", sections.join(", ")));
        }
        map.retain(|key, value| key == section || !value.is_array());
    }
    if let Some(find) = find.map(str::to_lowercase) {
        for value in map.values_mut() {
            if let Value::Array(items) = value {
                items.retain(|item| item.to_string().to_lowercase().contains(&find));
            }
        }
        // Sections with nothing left say nothing.
        map.retain(|_, value| value.as_array().is_none_or(|items| !items.is_empty()));
    }
    Ok(report)
}

/// The binary's half and the source's half, merged.
pub fn report(app: &App) -> Result<Value, String> {
    let Some(out) = app.binary_output(&["introspect"])? else {
        return Err("the app didn't build or `introspect` failed; run `kiln check` to see why".into());
    };
    let mut report: Value =
        serde_json::from_str(&out).map_err(|e| format!("the app's introspection isn't JSON ({e})"))?;
    let source = scan(app);
    for (key, value) in source.as_object().into_iter().flatten() {
        report[key] = value.clone();
    }
    Ok(report)
}

/// What the app's source declares, by kind.
pub fn scan(app: &App) -> Value {
    let mut views = vec![];
    let mut forms = vec![];
    let mut mail = vec![];
    let mut config = vec![];
    let mut policies = vec![];
    let mut ops = vec![];
    let mut parts = vec![];
    for path in rust_files(&app.path("src")) {
        let file = path.strip_prefix(&app.root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        let Ok(text) = fs::read_to_string(&path) else { continue };
        let Ok(parsed) = syn::parse_file(&text) else { continue };
        for item in &parsed.items {
            match item {
                Item::Struct(s) => {
                    let derives = derives(&s.attrs);
                    let entry = || {
                        json!({
                            "name": s.ident.to_string(),
                            "file": file,
                            "line": s.span().start().line,
                            "doc": doc(&s.attrs),
                            "fields": fields(&s.fields),
                        })
                    };
                    if derives.iter().any(|d| d == "View") {
                        let mut v = entry();
                        v["template"] = json!(attr_value(&s.attrs, "view", "template"));
                        v["layout"] = json!(view_layout(app, &s.attrs));
                        views.push(v);
                    }
                    if derives.iter().any(|d| d == "Form") {
                        forms.push(entry());
                    }
                    if derives.iter().any(|d| d == "Mail") {
                        let mut m = entry();
                        m["template"] = json!(attr_value(&s.attrs, "mail", "template"));
                        m["subject"] = json!(attr_value(&s.attrs, "mail", "subject"));
                        let mail_layout = "layouts/mail.html.jinja";
                        m["layout"] = json!(app.path("templates").join(mail_layout).exists().then_some(mail_layout));
                        mail.push(m);
                    }
                    if derives.iter().any(|d| d == "Config") {
                        config.push(entry());
                    }
                }
                Item::Impl(i) => {
                    let target = type_text(&i.self_ty.to_token_stream().to_string());
                    if let Some((_, trait_path, _)) = &i.trait_
                        && let Some(last) = trait_path.segments.last()
                        && last.ident == "Allows"
                    {
                        let (action, resource) = match &last.arguments {
                            syn::PathArguments::AngleBracketed(args) => {
                                let mut types = args.args.iter().map(|a| type_text(&a.to_token_stream().to_string()));
                                (types.next().unwrap_or_default(), types.next().unwrap_or_default())
                            }
                            _ => Default::default(),
                        };
                        policies.push(json!({
                            "actor": target,
                            "action": action,
                            "resource": resource,
                            "file": file,
                            "line": i.span().start().line,
                            "doc": doc(&i.attrs),
                        }));
                    } else if i.trait_.is_none() && i.attrs.iter().any(|a| a.path().is_ident("part")) {
                        for inner in &i.items {
                            if let syn::ImplItem::Fn(f) = inner
                                && matches!(f.vis, syn::Visibility::Public(_))
                            {
                                parts.push(json!({
                                    "record": target,
                                    "name": f.sig.ident.to_string(),
                                    "returns": returns(&f.sig.output),
                                    "file": file,
                                    "line": f.span().start().line,
                                    "doc": doc(&f.attrs),
                                }));
                            }
                        }
                    }
                }
                Item::Fn(f) if file.starts_with("src/ops/") && matches!(f.vis, syn::Visibility::Public(_)) => {
                    ops.push(json!({
                        "name": f.sig.ident.to_string(),
                        "signature": signature(&f.sig),
                        "file": file,
                        "line": f.span().start().line,
                        "doc": doc(&f.attrs),
                    }));
                }
                _ => {}
            }
        }
    }
    let templates: Vec<String> = files_under(&app.path("templates"))
        .into_iter()
        .filter_map(|p| p.strip_prefix(app.path("templates")).ok().map(|p| p.to_string_lossy().replace('\\', "/")))
        .collect();
    let controllers: Vec<Value> = crate::importmap::controllers(&app.root)
        .into_iter()
        .map(|(identifier, file)| json!({ "identifier": identifier, "file": file }))
        .collect();
    let pins: Vec<Value> = fs::read_to_string(app.path(crate::importmap::PINS))
        .ok()
        .and_then(|text| crate::importmap::pins(&text).ok())
        .into_iter()
        .flatten()
        .map(|(name, to)| json!({ "name": name, "to": to }))
        .collect();
    json!({
        "views": views,
        "forms": forms,
        "policies": policies,
        "ops": ops,
        "parts": parts,
        "mail": mail,
        "config": config,
        "templates": templates,
        "stimulus_controllers": controllers,
        "importmap_pins": pins,
    })
}

/// The human-readable version: one line per thing, grouped.
fn summary(report: &Value) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let list = |key: &str| report[key].as_array().cloned().unwrap_or_default();
    let s = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    let _ = writeln!(out, "{} ({} backend)", s(&report["app"]), s(&report["backend"]));

    let routes = list("routes");
    let width = routes.iter().map(|r| s(&r["path"]).len()).max().unwrap_or(0) + 2;
    // A narrowed report (`--find`, a section) leaves out what has nothing to show.
    if report.get("routes").is_some() {
        let _ = writeln!(out, "\nroutes ({})", routes.len());
    }
    for r in &routes {
        let mut extras = vec![];
        if let Some(user) = r["signed_in"].as_str() {
            let optional = r["sign_in_required"] == Value::Bool(false);
            extras.push(if optional { format!("signed in or out: {user}") } else { format!("signed in: {user}") });
        }
        if r["accepts_api_tokens"].as_bool() == Some(true) {
            extras.push(match r["scope"].as_str() {
                Some("any") => "any api token".to_owned(),
                Some(scope) => format!("api tokens with {scope}"),
                None => "api tokens".to_owned(),
            });
        }
        if r["rate_limited"].as_bool() == Some(true) {
            extras.push("rate limited".to_owned());
        }
        if let Some(form) = r["form"].as_str() {
            extras.push(format!("form: {form}"));
        }
        let extras = if extras.is_empty() { String::new() } else { format!("  [{}]", extras.join(", ")) };
        let _ = writeln!(
            out,
            "  {:<7}{:<width$}{} -> {}{extras}",
            s(&r["method"]),
            s(&r["path"]),
            s(&r["action"]),
            s(&r["returns"])
        );
    }

    let records = list("records");
    if report.get("records").is_some() {
        let _ = writeln!(out, "\nrecords ({})", records.len());
    }
    for r in &records {
        let columns: Vec<String> = r["columns"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| format!("{}: {}{}", s(&c["name"]), s(&c["type"]), if c["nullable"] == true { "?" } else { "" }))
            .collect();
        let _ = writeln!(out, "  {} ({})  {}", s(&r["name"]), s(&r["table"]), columns.join(", "));
        for rel in r["relations"].as_array().into_iter().flatten() {
            let through = rel["through"].as_str().map(|t| format!(" (through {t})")).unwrap_or_default();
            let _ = writeln!(out, "    {} {} -> {}{through}", s(&rel["kind"]), s(&rel["name"]), s(&rel["target"]));
        }
        for index in r["indexes"].as_array().into_iter().flatten() {
            let columns: Vec<String> = index["columns"].as_array().into_iter().flatten().map(s).collect();
            let kind = if index["unique"] == true { "unique" } else { "index" };
            let condition = index["where"].as_str().map(|w| format!(" where {w}")).unwrap_or_default();
            let _ = writeln!(out, "    {kind} ({}){condition}", columns.join(", "));
        }
        for group in r["one_of"].as_array().into_iter().flatten() {
            let columns: Vec<String> = group.as_array().into_iter().flatten().map(s).collect();
            let _ = writeln!(out, "    one_of ({})", columns.join(", "));
        }
    }

    let simple = |out: &mut String, title: &str, items: &[Value], line: &dyn Fn(&Value) -> String| {
        if items.is_empty() {
            return;
        }
        let _ = writeln!(out, "\n{title} ({})", items.len());
        for item in items {
            let _ = writeln!(out, "  {}", line(item));
        }
    };
    let at = |v: &Value| format!("{}:{}", s(&v["file"]), v["line"]);
    let field_names =
        |v: &Value| v["fields"].as_array().into_iter().flatten().map(|f| s(&f["name"])).collect::<Vec<_>>().join(", ");
    simple(&mut out, "views", &list("views"), &|v| {
        format!("{} -> templates/{}.html.jinja  ({})  {}", s(&v["name"]), s(&v["template"]), field_names(v), at(v))
    });
    simple(&mut out, "forms", &list("forms"), &|v| format!("{} ({})  {}", s(&v["name"]), field_names(v), at(v)));
    simple(&mut out, "policies", &list("policies"), &|v| {
        format!("{} may {} {}  {}", s(&v["actor"]), s(&v["action"]), s(&v["resource"]), at(v))
    });
    simple(&mut out, "ops", &list("ops"), &|v| format!("{}  {}", s(&v["signature"]), at(v)));
    simple(&mut out, "parts", &list("parts"), &|v| {
        format!("{}.{} -> {}  {}", s(&v["record"]), s(&v["name"]), s(&v["returns"]), at(v))
    });
    simple(&mut out, "mail", &list("mail"), &|v| format!("{} \"{}\"  {}", s(&v["name"]), s(&v["subject"]), at(v)));
    simple(&mut out, "jobs", &list("jobs"), &|v| {
        format!("{} (queue {}, {} attempts)", s(&v["name"]), s(&v["queue"]), v["attempts"])
    });
    simple(&mut out, "schedules", &list("schedules"), &|v| format!("{} {}", s(&v["job"]), s(&v["when"])));
    simple(&mut out, "notifications", &list("notifications"), &|v| {
        format!("{}: {}", s(&v["name"]), s(&v["description"]))
    });
    simple(&mut out, "config", &list("config"), &|v| format!("{} ({})", s(&v["name"]), field_names(v)));
    simple(&mut out, "stimulus controllers", &list("stimulus_controllers"), &|v| {
        format!("data-controller=\"{}\"  {}", s(&v["identifier"]), s(&v["file"]))
    });
    simple(&mut out, "import map pins", &list("importmap_pins"), &|v| format!("{} -> {}", s(&v["name"]), s(&v["to"])));
    if report.get("templates").is_some() {
        let _ = writeln!(out, "\n{} templates.", list("templates").len());
    }
    out.push_str("\n`kiln introspect --json` has every detail; narrow it with a section (`kiln introspect routes`) or `--find Post`.\n");
    out
}

fn derives(attrs: &[Attribute]) -> Vec<String> {
    let mut out = vec![];
    for attr in attrs.iter().filter(|a| a.path().is_ident("derive")) {
        let _ = attr.parse_nested_meta(|meta| {
            if let Some(last) = meta.path.segments.last() {
                out.push(last.ident.to_string());
            }
            Ok(())
        });
    }
    out
}

/// `#[name(key = "value")]`'s value.
fn attr_value(attrs: &[Attribute], name: &str, key: &str) -> Option<String> {
    let mut found = None;
    for attr in attrs.iter().filter(|a| a.path().is_ident(name)) {
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident(key) {
                let lit: syn::LitStr = meta.value()?.parse()?;
                found = Some(lit.value());
            } else if meta.input.peek(syn::Token![=]) {
                let _: syn::Expr = meta.value()?.parse()?;
            }
            Ok(())
        });
    }
    found
}

/// The layout file a view renders in, as `#[derive(View)]` picks it: `layout = "repo"` →
/// `layouts/repo.html.jinja`, `layout = none` → none, unset → `layouts/app.html.jinja` when
/// the app has one.
fn view_layout(app: &App, attrs: &[Attribute]) -> Option<String> {
    let mut layout: Option<Option<String>> = None;
    for attr in attrs.iter().filter(|a| a.path().is_ident("view")) {
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("layout") {
                match meta.value()?.parse::<syn::Expr>()? {
                    syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(name), .. }) => {
                        layout = Some(Some(format!("layouts/{}.html.jinja", name.value())));
                    }
                    _ => layout = Some(None),
                }
            } else if meta.input.peek(syn::Token![=]) {
                let _: syn::Expr = meta.value()?.parse()?;
            }
            Ok(())
        });
    }
    let default = "layouts/app.html.jinja";
    layout.unwrap_or_else(|| app.path("templates").join(default).exists().then(|| default.to_owned()))
}

/// The `///` comment, joined into one paragraph-ish string.
fn doc(attrs: &[Attribute]) -> Option<String> {
    let lines: Vec<String> = attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .filter_map(|a| match &a.meta {
            syn::Meta::NameValue(nv) => match &nv.value {
                syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(s), .. }) => Some(s.value().trim().to_owned()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join(" ").trim().to_owned())
}

fn fields(fields: &Fields) -> Vec<Value> {
    fields
        .iter()
        .filter_map(|f| {
            let name = f.ident.as_ref()?.to_string();
            let attributes: Vec<String> = f
                .attrs
                .iter()
                .filter(|a| !a.path().is_ident("doc"))
                .map(|a| type_text(&a.to_token_stream().to_string()))
                .collect();
            Some(json!({
                "name": name,
                "type": type_text(&f.ty.to_token_stream().to_string()),
                "attributes": attributes,
                "doc": doc(&f.attrs),
            }))
        })
        .collect()
}

/// `async fn name(a: A, b: B) -> R`, on one line however it was formatted.
fn signature(sig: &syn::Signature) -> String {
    let inputs: Vec<String> = sig.inputs.iter().map(|i| type_text(&i.to_token_stream().to_string())).collect();
    let asyncness = if sig.asyncness.is_some() { "async " } else { "" };
    let output = match &sig.output {
        syn::ReturnType::Default => String::new(),
        syn::ReturnType::Type(..) => format!(" -> {}", returns(&sig.output)),
    };
    format!("{asyncness}fn {}({}){output}", sig.ident, inputs.join(", "))
}

fn returns(output: &syn::ReturnType) -> String {
    match output {
        syn::ReturnType::Default => "()".into(),
        syn::ReturnType::Type(_, ty) => type_text(&ty.to_token_stream().to_string()),
    }
}

/// `quote`'s spaced-out tokens as code reads: `Current < User >` → `Current<User>`.
fn type_text(tokens: &str) -> String {
    let mut out = String::new();
    for part in tokens.split_whitespace() {
        let glue = out.ends_with(['<', '(', '[', '&', '#', '!'])
            || out.ends_with("::")
            || part.starts_with(['<', '>', ')', ']', ',', ':', '.', '!', '?'])
            // A call or index glues on; a tuple after `,`, `->` or `=` doesn't.
            || (part.starts_with(['(', '[']) && !out.ends_with([',', '>', '=']));
        if !out.is_empty() && !glue {
            out.push(' ');
        }
        out.push_str(part);
    }
    out
}

fn rust_files(dir: &Path) -> Vec<std::path::PathBuf> {
    files_under(dir).into_iter().filter(|p| p.extension().is_some_and(|e| e == "rs")).collect()
}

fn files_under(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    let Ok(entries) = fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_types_as_written() {
        assert_eq!(type_text("Current < User >"), "Current<User>");
        assert_eq!(type_text("Result < Option < (User , Post) > >"), "Result<Option<(User, Post)>>");
        assert_eq!(type_text("& mut Vec < u8 >"), "&mut Vec<u8>");
        assert_eq!(type_text("Valid < PostForm >"), "Valid<PostForm>");
        assert_eq!(type_text("Loaded < Comment , (comment :: author ,) >"), "Loaded<Comment, (comment::author,)>");
        assert_eq!(type_text("Result < (A , B) >"), "Result<(A, B)>");
    }

    #[test]
    fn scans_views_forms_policies_and_ops() {
        let dir = std::env::temp_dir().join(format!("kiln-introspect-{}", std::process::id()));
        let write = |path: &str, text: &str| {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write("Cargo.toml", "[package]\nname = \"demo\"\n");
        write(
            "src/views/posts/show.rs",
            "/// One post.\n#[derive(View)]\n#[view(template = \"posts/show\")]\npub struct Show { pub post: Post, pub can_edit: bool }\n\n\
             #[derive(View)]\n#[view(template = \"posts/show\", layout = \"admin\")]\npub struct Admin { pub post: Post }\n\n\
             #[derive(View)]\n#[view(template = \"posts/show\", layout = none)]\npub struct Bare { pub post: Post }\n",
        );
        write("templates/layouts/app.html.jinja", "");
        write(
            "src/forms/post_form.rs",
            "#[derive(Form, Debug)]\npub struct PostForm {\n    #[validate(length(min = 3))]\n    pub title: String,\n}\n",
        );
        write(
            "src/policies/post.rs",
            "/// Authors edit.\nimpl Allows<Update, Post> for User { fn allows(&self, p: &Post) -> bool { true } }\n",
        );
        write(
            "src/ops/create_post.rs",
            "/// Makes one.\npub async fn create_post(ctx: &Ctx, input: Valid<PostForm>) -> Result<Post> { todo!() }\nfn helper() {}\n",
        );
        write("templates/posts/show.html.jinja", "");
        write("assets/controllers/admin/users_controller.js", "");
        write("config/importmap.toml", "\"@pierre/diffs\" = \"vendor/npm/@pierre/diffs@1.5.2.js\"\n");
        fs::create_dir_all(dir.join("src/actions")).unwrap();
        let app = App { root: dir.clone(), package: "demo".into() };

        let out = scan(&app);
        assert_eq!(out["views"][0]["name"], "Show");
        assert_eq!(out["views"][0]["template"], "posts/show");
        assert_eq!(out["views"][0]["doc"], "One post.");
        assert_eq!(out["views"][0]["fields"][1]["type"], "bool");
        let layouts: Vec<&Value> = out["views"].as_array().unwrap().iter().map(|v| &v["layout"]).collect();
        assert_eq!(layouts, [&json!("layouts/app.html.jinja"), &json!("layouts/admin.html.jinja"), &Value::Null]);
        assert_eq!(out["forms"][0]["fields"][0]["attributes"][0], "#[validate(length(min = 3))]");
        assert_eq!(out["policies"][0]["action"], "Update");
        assert_eq!(out["policies"][0]["resource"], "Post");
        assert_eq!(out["policies"][0]["actor"], "User");
        assert_eq!(out["ops"].as_array().unwrap().len(), 1);
        assert_eq!(
            out["ops"][0]["signature"],
            "async fn create_post(ctx: &Ctx, input: Valid<PostForm>) -> Result<Post>"
        );
        assert_eq!(out["templates"][1], "posts/show.html.jinja");
        assert_eq!(out["stimulus_controllers"][0]["identifier"], "admin--users");
        assert_eq!(out["stimulus_controllers"][0]["file"], "assets/controllers/admin/users_controller.js");
        assert_eq!(out["importmap_pins"][0]["name"], "@pierre/diffs");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn reports_narrow_to_a_section_or_a_word() {
        let report = json!({
            "app": "gather",
            "routes": [{ "path": "/events" }, { "path": "/posts" }],
            "records": [{ "name": "Event" }, { "name": "User" }],
        });
        let routes = narrow(report.clone(), Some("routes"), None).unwrap();
        assert_eq!(routes, json!({ "app": "gather", "routes": [{ "path": "/events" }, { "path": "/posts" }] }));
        let events = narrow(report.clone(), None, Some("EVENT")).unwrap();
        assert_eq!(
            events,
            json!({ "app": "gather", "routes": [{ "path": "/events" }], "records": [{ "name": "Event" }] })
        );
        assert!(narrow(report, Some("nope"), None).unwrap_err().contains("records, routes"));
    }
}

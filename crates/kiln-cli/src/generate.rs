//! Generators. Each writes code in the same shape a person would: the blog example's posts
//! CRUD is the reference. Generated code compiles, passes `kiln check`, and ships with tests.

use clap::Subcommand;

use crate::{
    app::App,
    edit::Plan,
    names::{human, is_snake, pascal, plural, snake},
};

#[derive(Subcommand)]
pub enum Generate {
    /// A CRUD resource: record, migration, form, policy, ops, actions, views, templates,
    /// factory and tests. Fields: `title:string body:text published_at:timestamp?
    /// owner:ref:User`. Types: string, text, integer, float, bool, timestamp, ref:<Record>;
    /// a trailing `?` makes a field optional. The first required ref to a record people sign
    /// in as (registered with `signs_in::<..>` in src/app.rs, e.g. `owner:ref:User` or
    /// `seller:ref:Merchant`) is the owner: set from the signed-in record. Owned resources
    /// are private: only the owner can list, see, edit or delete them. Pass `--public` to
    /// let everyone list and see them.
    Resource {
        name: String,
        fields: Vec<String>,
        /// For owned resources: anyone may list and view; only the owner may change.
        #[arg(long)]
        public: bool,
        /// Skip `db diff create_<plural>`.
        #[arg(long)]
        no_migration: bool,
        #[arg(long)]
        force: bool,
    },
    /// A record (and its migration), with a factory.
    Record {
        name: String,
        fields: Vec<String>,
        #[arg(long)]
        no_migration: bool,
        #[arg(long)]
        force: bool,
    },
    /// One action: `kiln g action posts publish POST /posts/{slug}/publish`.
    Action {
        resource: String,
        name: String,
        method: String,
        path: String,
        #[arg(long)]
        force: bool,
    },
    /// An operation in src/ops: `kiln g op publish_post`.
    Op {
        name: String,
        #[arg(long)]
        force: bool,
    },
    /// A `#[part]` block for a record, and the record's `parts` opt-in: `kiln g part Post`.
    Part {
        record: String,
        #[arg(long)]
        force: bool,
    },
}

impl Generate {
    pub fn run(self, app: &App) -> Result<(), String> {
        let (plan, force, migration) = match self {
            Generate::Resource { name, fields, public, no_migration, force } => {
                let r = Resource::new(app, &name, &fields, force, public)?;
                let mut plan = Plan::default();
                r.record(&mut plan);
                r.factory(&mut plan);
                r.crud(&mut plan);
                (plan, force, (!no_migration).then(|| format!("create_{}", r.plural)))
            }
            Generate::Record { name, fields, no_migration, force } => {
                let r = Resource::new(app, &name, &fields, force, true)?;
                let mut plan = Plan::default();
                r.record(&mut plan);
                r.factory(&mut plan);
                (plan, force, (!no_migration).then(|| format!("create_{}", r.plural)))
            }
            Generate::Action { resource, name, method, path, force } => {
                (action(&resource, &name, &method, &path)?, force, None)
            }
            Generate::Op { name, force } => (op(&name)?, force, None),
            Generate::Part { record, force } => (part(app, &record)?, force, None),
        };
        for line in plan.apply(&app.root, force)? {
            println!("{line}");
        }
        // Generated code is formatted like the rest of the app (rustfmt.toml applies).
        let rust_files: Vec<_> = plan.written().filter(|p| p.ends_with(".rs")).map(|p| app.path(p)).collect();
        if !rust_files.is_empty() {
            let _ = std::process::Command::new("rustfmt").args(["--edition", "2024"]).args(&rust_files).status();
        }
        if let Some(name) = migration {
            println!("\nrunning `kiln db diff {name}`");
            if !app.run_binary(&["db", "diff", &name])? {
                return Err(
                    "the app didn't compile or the diff failed; fix the errors above, then run `kiln db diff`".into()
                );
            }
        }
        Ok(())
    }
}

/// Replaces `@@key@@` placeholders. (Templates use `{{ }}`, so generators don't.) Values
/// may contain placeholders themselves, so it repeats until nothing changes.
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    loop {
        let before = out.clone();
        for (key, value) in values {
            out = out.replace(&format!("@@{key}@@"), value);
        }
        if out == before {
            break;
        }
    }
    debug_assert!(!out.contains("@@"), "unfilled placeholder in:\n{out}");
    out
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "static",
    "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while", "yield", "box", "try",
];

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    String,
    Text,
    Integer,
    Float,
    Bool,
    Timestamp,
    Ref,
}

#[derive(Clone)]
struct Field {
    /// For refs, the relation name (`owner`); the column is `owner_id`.
    name: String,
    kind: Kind,
    optional: bool,
    target: Option<String>,
    /// For refs: the related record's `name` or `title` column, shown instead of the id.
    target_label: Option<String>,
}

impl Field {
    fn parse(spec: &str) -> Result<Self, String> {
        let parts: Vec<&str> = spec.split(':').collect();
        let (name, ty, target) = match parts.as_slice() {
            [name, ty] => (*name, *ty, None),
            [name, "ref", target] => (*name, "ref", Some(*target)),
            _ => return Err(format!("`{spec}`: fields are `name:type` or `name:ref:Record`")),
        };
        let (ty, mut optional) = match ty.strip_suffix('?') {
            Some(ty) => (ty, true),
            None => (ty, false),
        };
        let target = target.map(|t| match t.strip_suffix('?') {
            Some(t) => {
                optional = true;
                pascal(t)
            }
            None => pascal(t),
        });
        let name = name.strip_suffix("_id").filter(|_| ty == "ref").unwrap_or(name).to_owned();
        if !is_snake(&name) {
            return Err(format!("`{name}`: field names are snake_case"));
        }
        if ["id", "created_at", "updated_at"].contains(&name.as_str()) {
            return Err(format!("`{name}` is added automatically"));
        }
        if RUST_KEYWORDS.contains(&name.as_str()) {
            return Err(format!("`{name}` is a Rust keyword; pick another name"));
        }
        let kind = match ty {
            "string" => Kind::String,
            "text" => Kind::Text,
            "integer" | "int" => Kind::Integer,
            "float" => Kind::Float,
            "bool" | "boolean" => Kind::Bool,
            "timestamp" | "datetime" => Kind::Timestamp,
            "ref" => Kind::Ref,
            other => {
                return Err(format!(
                    "`{spec}`: unknown type `{other}`; use string, text, integer, float, bool, timestamp or ref:<Record>"
                ));
            }
        };
        if kind == Kind::Bool && optional {
            return Err(format!("`{spec}`: bool fields can't be optional; an unchecked box is false"));
        }
        Ok(Self { name, kind, optional, target, target_label: None })
    }

    fn column(&self) -> String {
        if self.kind == Kind::Ref { format!("{}_id", self.name) } else { self.name.clone() }
    }

    fn base_type(&self) -> String {
        match self.kind {
            Kind::String | Kind::Text => "String".into(),
            Kind::Integer => "i64".into(),
            Kind::Float => "f64".into(),
            Kind::Bool => "bool".into(),
            Kind::Timestamp => "Timestamp".into(),
            Kind::Ref => format!("Id<{}>", self.target.as_deref().unwrap_or("?")),
        }
    }

    fn rust_type(&self) -> String {
        if self.optional { format!("Option<{}>", self.base_type()) } else { self.base_type() }
    }

    fn is_text(&self) -> bool {
        matches!(self.kind, Kind::String | Kind::Text)
    }

    fn label(&self) -> String {
        human(&self.name)
    }

    /// A value a test submits in a form.
    fn sample(&self, prefix: &str) -> String {
        match self.kind {
            Kind::String | Kind::Text => format!("{prefix} {}", self.name.replace('_', " ")),
            // Small values, so adding a `range(..)` rule later doesn't break the tests.
            Kind::Integer => "3".into(),
            Kind::Float => "2.5".into(),
            Kind::Bool => "true".into(),
            Kind::Timestamp => "2026-01-01T12:00".into(),
            Kind::Ref => String::new(),
        }
    }
}

struct Resource {
    krate: String,
    pascal: String,
    snake: String,
    plural: String,
    fields: Vec<Field>,
    /// The `ref:<Record>` field set from the signed-in record (a type registered with
    /// `signs_in::<..>` in src/app.rs, usually `User`).
    owner: Option<String>,
    /// The record type that signs in to use this resource, e.g. `User` or `Merchant`.
    actor: String,
    actor_snake: String,
    /// Owned and not `--public`: only the owner lists or sees records.
    private: bool,
}

impl Resource {
    fn new(app: &App, name: &str, specs: &[String], force: bool, public: bool) -> Result<Self, String> {
        let pascal = pascal(name);
        let snake = snake(&pascal);
        if !is_snake(&snake) {
            return Err(format!("`{name}` isn't a valid record name; use PascalCase like `BlogPost`"));
        }
        let mut fields: Vec<Field> = specs.iter().map(|s| Field::parse(s)).collect::<Result<_, _>>()?;
        for f in fields.iter_mut() {
            let Some(target) = &f.target else { continue };
            let source =
                std::fs::read_to_string(app.path(&format!("src/records/{}.rs", snake_of(target)))).unwrap_or_default();
            f.target_label = ["name", "title"]
                .into_iter()
                .find(|column| source.contains(&format!("pub {column}: String,")))
                .map(str::to_owned);
        }
        for (i, f) in fields.iter().enumerate() {
            if fields[..i].iter().any(|g| g.column() == f.column()) {
                return Err(format!("field `{}` is listed twice", f.name));
            }
            if let Some(target) = &f.target {
                let file = app.path(&format!("src/records/{}.rs", snake_of(target)));
                if target != &pascal && !file.exists() {
                    return Err(format!(
                        "`{}:ref:{target}`: there's no `{target}` record ({} is missing)",
                        f.name,
                        file.display()
                    ));
                }
            }
        }
        if !force && app.path(&format!("src/records/{snake}.rs")).exists() {
            return Err(format!("src/records/{snake}.rs already exists (pass --force to regenerate)"));
        }
        // Record types people sign in as, from `signs_in::<T, _>(..)` in src/app.rs.
        let app_rs = std::fs::read_to_string(app.path("src/app.rs")).unwrap_or_default();
        let mut signs_in: Vec<String> = app_rs
            .split("signs_in::<")
            .skip(1)
            .filter_map(|rest| rest.split([',', '>']).next().map(|t| t.trim().to_owned()))
            .filter(|t| !t.is_empty())
            .collect();
        if signs_in.is_empty() {
            signs_in.push("User".to_owned());
        }
        let owner_field = fields
            .iter()
            .find(|f| f.kind == Kind::Ref && !f.optional && f.target.as_ref().is_some_and(|t| signs_in.contains(t)));
        let owner = owner_field.map(|f| f.name.clone());
        let actor = owner_field.and_then(|f| f.target.clone()).unwrap_or_else(|| signs_in[0].clone());
        if !app.path(&format!("src/records/{}.rs", snake_of(&actor))).exists() {
            return Err(format!(
                "resources need a record people sign in as; `{actor}` has no src/records/{}.rs. Generate it (`kiln g record {actor} name:string email:string`) and register it with `signs_in::<{actor}, _>(..)` in src/app.rs",
                snake_of(&actor)
            ));
        }
        let private = owner.is_some() && !public;
        let actor_snake = snake_of(&actor);
        Ok(Self {
            krate: app.crate_name(),
            plural: plural(&snake),
            pascal,
            snake,
            fields,
            owner,
            actor,
            actor_snake,
            private,
        })
    }

    fn values(&self) -> Vec<(&str, &str)> {
        vec![
            ("Name", &self.pascal),
            ("name", &self.snake),
            ("names", &self.plural),
            ("a", article(&self.snake)),
            ("Actor", &self.actor),
            ("actor", &self.actor_snake),
        ]
    }

    /// Relations shown by name on the show page.
    fn labeled_refs(&self) -> Vec<&Field> {
        // On private records the owner is always the viewer, so it isn't shown.
        self.fields
            .iter()
            .filter(|f| f.target_label.is_some() && !(self.private && Some(&f.name) == self.owner.as_ref()))
            .collect()
    }

    /// `(@@name@@::owner, @@name@@::venue)`
    fn labeled_rel_tuple(&self) -> String {
        let rels: Vec<String> = self.labeled_refs().iter().map(|f| format!("@@name@@::{}", f.name)).collect();
        if rels.len() == 1 { format!("({},)", rels[0]) } else { format!("({})", rels.join(", ")) }
    }

    fn display_field(&self) -> Option<&Field> {
        self.fields.iter().find(|f| f.is_text() && !f.optional)
    }

    fn owner_field(&self) -> Option<&Field> {
        self.owner.as_ref().and_then(|o| self.fields.iter().find(|f| &f.name == o))
    }

    /// Fields the form edits: everything except the owner.
    fn form_fields(&self) -> Vec<&Field> {
        self.fields.iter().filter(|f| Some(&f.name) != self.owner.as_ref()).collect()
    }

    fn record(&self, plan: &mut Plan) {
        let mut targets: Vec<&str> =
            self.fields.iter().filter_map(|f| f.target.as_deref()).filter(|t| *t != self.pascal).collect();
        targets.sort();
        targets.dedup();
        let imports = match targets.as_slice() {
            [] => String::new(),
            [one] => format!("\nuse super::{one};\n"),
            many => format!("\nuse super::{{{}}};\n", many.join(", ")),
        };
        let rels: Vec<String> = self
            .fields
            .iter()
            .filter(|f| f.kind == Kind::Ref)
            .map(|f| format!("{}: {}", f.name, f.target.as_deref().unwrap_or("?")))
            .collect();
        let attr = if rels.is_empty() {
            format!("#[record(table = \"{}\")]", self.plural)
        } else {
            format!("#[record(table = \"{}\", belongs_to({}))]", self.plural, rels.join(", "))
        };
        let columns: String =
            self.fields.iter().map(|f| format!("    pub {}: {},\n", f.column(), f.rust_type())).collect();
        // Private resources are always listed through their owner. (`who`, not `user`: in
        // this file, column and relation names are constants.)
        let scope = match (self.private, self.owner_field()) {
            (true, Some(owner)) => format!(
                "\nimpl @@Name@@ {{\n    /// The {} `who` owns, newest first.\n    pub fn owned_by(who: &@@Actor@@) -> Query<@@Name@@> {{\n        @@Name@@::query().filter({}.eq(who.id)).order(created_at.desc())\n    }}\n}}\n",
                human(&self.plural).to_lowercase(),
                owner.column()
            ),
            _ => String::new(),
        };
        let scope = fill(&scope, &self.values());
        plan.file(
            format!("src/records/{}.rs", self.snake),
            fill(
                "use kiln::prelude::*;\n@@imports@@\n#[derive(Record, Debug, Clone)]\n@@attr@@\npub struct @@Name@@ {\n    pub id: Id<@@Name@@>,\n@@columns@@    pub created_at: Timestamp,\n    pub updated_at: Timestamp,\n}\n@@scope@@",
                &[&self.values()[..], &[("imports", &imports), ("attr", &attr), ("columns", &columns), ("scope", &scope)]]
                    .concat(),
            ),
        );
        plan.module("src/records/mod.rs", format!("pub mod {};", self.snake));
        // Sorted the way rustfmt sorts them, so regenerating finds the existing line.
        let mut names = vec![self.pascal.clone(), format!("New{}", self.pascal)];
        if !rels.is_empty() {
            names.push(format!("{}Relations", self.pascal));
        }
        names.sort();
        plan.module("src/records/mod.rs", format!("pub use {}::{{{}}};", self.snake, names.join(", ")));
    }

    fn factory(&self, plan: &mut Plan) {
        let mut setup = String::new();
        let mut values = String::new();
        for f in &self.fields {
            let value = match (f.kind, f.optional) {
                (_, true) => "None".to_owned(),
                (Kind::String, _) => format!("format!(\"{} {{n}}\")", human(&f.name)),
                (Kind::Text, _) => format!("\"{} text.\".into()", human(&f.name)),
                (Kind::Integer, _) => "n as i64".into(),
                (Kind::Float, _) => "1.5".into(),
                (Kind::Bool, _) => "false".into(),
                (Kind::Timestamp, _) => "Timestamp::now()".into(),
                (Kind::Ref, _) => {
                    let target = f.target.as_deref().unwrap_or("?");
                    setup.push_str(&format!("        let {} = app.create::<{target}>().await?;\n", f.name));
                    format!("{}.id", f.name)
                }
            };
            values.push_str(&format!("            {}: {value},\n", f.column()));
        }
        let app_arg = if setup.is_empty() { "_app" } else { "app" };
        let uses_n = values.contains("{n}") || values.contains("n as i64");
        let n_arg = if uses_n { "n" } else { "_n" };
        plan.append(
            "src/factories.rs",
            "//! Default records for tests: `app.create::<Post>()`. In the library (not `tests/`) so\n//! factories can implement kiln's `Factory` trait for the app's records.\n\nuse kiln::{\n    prelude::*,\n    testing::{Factory, TestApp},\n};\n\nuse crate::records::*;\n",
            fill(
                "impl Factory for @@Name@@ {\n    async fn build(@@app@@: &TestApp, @@n@@: u64) -> Result<New@@Name@@> {\n@@setup@@        Ok(New@@Name@@ {\n@@values@@        })\n    }\n}\n",
                &[&self.values()[..], &[("app", app_arg), ("n", n_arg), ("setup", &setup), ("values", &values)]].concat(),
            ),
        );
        plan.module("src/lib.rs", "pub mod factories;");
    }

    fn crud(&self, plan: &mut Plan) {
        self.form(plan);
        self.policy(plan);
        self.ops(plan);
        self.actions(plan);
        self.views(plan);
        self.templates(plan);
        self.tests(plan);
        for module in ["actions", "forms", "ops", "policies", "views"] {
            plan.module("src/lib.rs", format!("pub mod {module};"));
        }
    }

    fn form(&self, plan: &mut Plan) {
        let fields = self.form_fields();
        let mut body = String::new();
        for f in &fields {
            match (f.kind, f.optional) {
                (Kind::String, false) => body.push_str("    #[validate(length(min = 1, max = 200))]\n"),
                (Kind::String, true) => body.push_str("    #[validate(length(max = 200))]\n"),
                (Kind::Text, false) => body.push_str("    #[validate(length(min = 1))]\n    #[form(textarea)]\n"),
                (Kind::Text, true) => body.push_str("    #[form(textarea)]\n"),
                _ => {}
            }
            body.push_str(&format!("    pub {}: {},\n", f.column(), f.rust_type()));
        }
        let from_record: String = fields
            .iter()
            .map(|f| {
                let clone = if f.is_text() { ".clone()" } else { "" };
                format!("            {}: {}.{}{clone},\n", f.column(), self.snake, f.column())
            })
            .collect();
        let mut new_values: String =
            fields.iter().map(|f| format!("            {c}: self.{c},\n", c = f.column())).collect();
        let (owner_param, owner_doc) = match self.owner_field() {
            Some(owner) => {
                new_values.push_str(&format!("            {}: {}.id,\n", owner.column(), owner.name));
                (format!(", {}: &@@Actor@@", owner.name), format!(" owned by `{}`", owner.name))
            }
            None => (String::new(), String::new()),
        };
        let sets: String =
            fields.iter().map(|f| format!(".set({}::{c}, self.{c})", self.snake, c = f.column())).collect();
        let apply_body = if fields.is_empty() { "update".to_owned() } else { format!("update{sets}") };
        let user_import = if self.owner.is_some() { ", @@Actor@@" } else { "" };
        let module_import = if fields.is_empty() { "" } else { ", @@name@@" };
        let template = format!(
            "use kiln::prelude::*;\n\nuse crate::records::{{@@Name@@, New@@Name@@{user_import}{module_import}}};\n\n#[derive(Form, Debug)]\npub struct @@Name@@Form {{\n@@body@@}}\n\nimpl @@Name@@Form {{\n    /// Prefills the edit form.\n    pub fn from_record(@@name@@: &@@Name@@) -> Self {{\n        Self {{\n@@from_record@@        }}\n    }}\n\n    /// The insert for a new @@human@@@@owner_doc@@.\n    pub fn into_new(self@@owner_param@@) -> New@@Name@@ {{\n        New@@Name@@ {{\n@@new_values@@        }}\n    }}\n\n    /// The update for an existing @@human@@.\n    pub fn apply(self, update: kiln::Update<@@Name@@>) -> kiln::Update<@@Name@@> {{\n        @@apply@@\n    }}\n}}\n"
        );
        let human = human(&self.snake).to_lowercase();
        plan.file(
            format!("src/forms/{}_form.rs", self.snake),
            fill(
                &template,
                &[
                    &self.values()[..],
                    &[
                        ("body", &body),
                        ("from_record", &from_record),
                        ("new_values", &new_values),
                        ("owner_param", &owner_param),
                        ("owner_doc", &owner_doc),
                        ("apply", &apply_body),
                        ("human", &human),
                    ],
                ]
                .concat(),
            ),
        );
        plan.module("src/forms/mod.rs", format!("mod {}_form;", self.snake));
        plan.module("src/forms/mod.rs", format!("pub use {}_form::{}Form;", self.snake, self.pascal));
    }

    fn policy(&self, plan: &mut Plan) {
        let human = human(&self.snake).to_lowercase();
        let (create, edit) = match self.owner_field() {
            Some(owner) => (
                (
                    format!("Users create {} they own.", plural(&human)),
                    format!("{}.{} == self.id", self.snake, owner.column()),
                ),
                (
                    format!("Only the {} edits {} {human}.", owner.name.replace('_', " "), article(&human)),
                    format!("{}.{} == self.id", self.snake, owner.column()),
                ),
            ),
            None => (
                (format!("Any signed-in user creates {}.", plural(&human)), "true".to_owned()),
                (format!("Any signed-in user edits {}.", plural(&human)), "true".to_owned()),
            ),
        };
        let param = if self.owner.is_some() { self.snake.clone() } else { format!("_{}", self.snake) };
        let delete_doc = edit.0.replace("edits", "deletes");
        let (read_import, read_impl) = if self.private {
            (
                "Read, ",
                format!(
                    "/// {} are private: only the owner sees them.\nimpl Allows<Read, @@Name@@> for @@Actor@@ {{\n    fn allows(&self, @@param@@: &@@Name@@) -> bool {{\n        @@edit_rule@@\n    }}\n}}\n\n",
                    crate::names::human(&self.plural)
                ),
            )
        } else {
            ("", String::new())
        };
        plan.file(
            format!("src/policies/{}.rs", self.snake),
            fill(
                &format!("use kiln::policy::{{Allows, Create, Delete, {read_import}Update}};\n\nuse crate::records::{{@@Name@@, New@@Name@@, @@Actor@@}};\n\n{read_impl}/// @@create_doc@@\nimpl Allows<Create, New@@Name@@> for @@Actor@@ {{\n    fn allows(&self, @@param@@: &New@@Name@@) -> bool {{\n        @@create_rule@@\n    }}\n}}\n\n/// @@edit_doc@@\nimpl Allows<Update, @@Name@@> for @@Actor@@ {{\n    fn allows(&self, @@param@@: &@@Name@@) -> bool {{\n        @@edit_rule@@\n    }}\n}}\n\n/// @@delete_doc@@\nimpl Allows<Delete, @@Name@@> for @@Actor@@ {{\n    fn allows(&self, @@param@@: &@@Name@@) -> bool {{\n        @@edit_rule@@\n    }}\n}}\n"),
                &[
                    &self.values()[..],
                    &[
                        ("create_doc", &create.0),
                        ("create_rule", &create.1),
                        ("edit_doc", &edit.0),
                        ("edit_rule", &edit.1),
                        ("delete_doc", &delete_doc),
                        ("param", &param),
                    ],
                ]
                .concat(),
            ),
        );
        plan.module("src/policies/mod.rs", format!("mod {};", self.snake));
    }

    fn ops(&self, plan: &mut Plan) {
        let v = self.values();
        plan.file(
            format!("src/ops/create_{}.rs", self.snake),
            fill(
                "use kiln::{policy::Create, prelude::*};\n\nuse crate::records::{@@Name@@, New@@Name@@};\n\npub async fn create_@@name@@(ctx: &Ctx, new: Authorized<Create, New@@Name@@>) -> Result<@@Name@@> {\n    @@Name@@::insert(new.into_inner()).run(&ctx.db).await\n}\n",
                &v,
            ),
        );
        plan.file(
            format!("src/ops/update_{}.rs", self.snake),
            fill(
                "use kiln::{policy::Update, prelude::*};\n\nuse crate::{forms::@@Name@@Form, records::@@Name@@};\n\npub async fn update_@@name@@(ctx: &Ctx, @@name@@: Authorized<Update, @@Name@@>, input: Valid<@@Name@@Form>) -> Result<@@Name@@> {\n    input.into_inner().apply(@@Name@@::update(@@name@@.id)).run(&ctx.db).await\n}\n",
                &v,
            ),
        );
        plan.file(
            format!("src/ops/delete_{}.rs", self.snake),
            fill(
                "use kiln::{policy::Delete, prelude::*};\n\nuse crate::records::@@Name@@;\n\n/// Records that reference this one must be deleted first; do it here, in one transaction.\npub async fn delete_@@name@@(ctx: &Ctx, @@name@@: Authorized<Delete, @@Name@@>) -> Result {\n    @@Name@@::delete(@@name@@.id).run(&ctx.db).await\n}\n",
                &v,
            ),
        );
        for verb in ["create", "delete", "update"] {
            plan.module("src/ops/mod.rs", format!("mod {verb}_{};", self.snake));
            plan.module("src/ops/mod.rs", format!("pub use {verb}_{name}::{verb}_{name};", name = self.snake));
        }
    }

    fn actions(&self, plan: &mut Plan) {
        let v = self.values();
        let human = human(&self.snake);
        let order = "order(@@name@@::created_at.desc())";
        let preloads: String = self.labeled_refs().iter().map(|f| format!(".preload(@@name@@::{})", f.name)).collect();
        let into_new = match self.owner_field() {
            Some(_) => "into_new(&@@actor@@)",
            None => "into_new()",
        };
        let (index, show) = if self.private {
            (
                "use kiln::prelude::*;\n\nuse crate::{records::*, views::@@names@@::Index};\n\n/// The signed-in @@actor@@'s @@names@@ only.\n#[action(GET \"/@@names@@\")]\npub async fn index(ctx: Ctx, @@actor@@: Current<@@Actor@@>) -> Result<Index> {\n    let @@names@@ = @@Name@@::owned_by(&@@actor@@).all(&ctx.db).await?;\n    Ok(Index { @@names@@ })\n}\n".to_owned(),
                format!("use kiln::{{\n    policy::{{Read, Update}},\n    prelude::*,\n}};\n\nuse crate::{{records::*, views::@@names@@::Show}};\n\n#[action(GET \"/@@names@@/{{id}}\")]\npub async fn show(ctx: Ctx, @@actor@@: Current<@@Actor@@>, id: Param<Id<@@Name@@>>) -> Result<Show> {{\n    let @@name@@ = @@actor@@.authorize::<Read, _>(@@Name@@::find(*id){preloads}.one_or_404(&ctx.db).await?)?;\n    let can_edit = @@actor@@.can::<Update, _>(&*@@name@@);\n    Ok(Show {{ @@name@@: @@name@@.into_inner(), can_edit }})\n}}\n"),
            )
        } else {
            (
                format!(
                    "use kiln::prelude::*;\n\nuse crate::{{records::*, views::@@names@@::Index}};\n\n#[action(GET \"/@@names@@\")]\npub async fn index(ctx: Ctx) -> Result<Index> {{\n    let @@names@@ = @@Name@@::query().{order}.all(&ctx.db).await?;\n    Ok(Index {{ @@names@@ }})\n}}\n"
                ),
                format!(
                    "use kiln::{{policy::Update, prelude::*}};\n\nuse crate::{{records::*, views::@@names@@::Show}};\n\n#[action(GET \"/@@names@@/{{id}}\")]\npub async fn show(ctx: Ctx, @@actor@@: Option<Current<@@Actor@@>>, id: Param<Id<@@Name@@>>) -> Result<Show> {{\n    let @@name@@ = @@Name@@::find(*id){preloads}.one_or_404(&ctx.db).await?;\n    let can_edit = @@actor@@.is_some_and(|u| u.can::<Update, _>(&@@name@@));\n    Ok(Show {{ @@name@@, can_edit }})\n}}\n"
                ),
            )
        };
        let files = [
            ("index", index),
            ("show", show),
            (
                "new",
                "use kiln::prelude::*;\n\nuse crate::{records::@@Actor@@, views::@@names@@::New};\n\n#[action(GET \"/@@names@@/new\")]\npub async fn new(_user: Current<@@Actor@@>) -> New {\n    New { form: FormState::new() }\n}\n".to_owned(),
            ),
            (
                "create",
                format!("use kiln::{{policy::Create, prelude::*}};\n\nuse crate::{{actions::@@names@@, forms::@@Name@@Form, ops, records::@@Actor@@, views::@@names@@::New}};\n\n#[action(POST \"/@@names@@\")]\npub async fn create(ctx: Ctx, @@actor@@: Current<@@Actor@@>, form: Form<@@Name@@Form>) -> Result<Redirect> {{\n    let input = form.validate().or_render(|form| New {{ form }})?;\n    let new = @@actor@@.authorize::<Create, _>(input.into_inner().{into_new})?;\n    let @@name@@ = ops::create_@@name@@(&ctx, new).await?;\n    Ok(Redirect::to(@@names@@::show::path(@@name@@.id)).notice(\"{human} created.\"))\n}}\n"),
            ),
            (
                "edit",
                "use kiln::{policy::Update, prelude::*};\n\nuse crate::{forms::@@Name@@Form, records::*, views::@@names@@::Edit};\n\n#[action(GET \"/@@names@@/{id}/edit\")]\npub async fn edit(ctx: Ctx, @@actor@@: Current<@@Actor@@>, id: Param<Id<@@Name@@>>) -> Result<Edit> {\n    let @@name@@ = @@actor@@.authorize::<Update, _>(@@Name@@::find(*id).one_or_404(&ctx.db).await?)?;\n    Ok(Edit { form: FormState::from_input(&@@Name@@Form::from_record(&@@name@@)), @@name@@: @@name@@.into_inner() })\n}\n".to_owned(),
            ),
            (
                "update",
                format!("use kiln::{{policy::Update, prelude::*}};\n\nuse crate::{{actions::@@names@@, forms::@@Name@@Form, ops, records::*, views::@@names@@::Edit}};\n\n#[action(PATCH \"/@@names@@/{{id}}\")]\npub async fn update(ctx: Ctx, @@actor@@: Current<@@Actor@@>, id: Param<Id<@@Name@@>>, form: Form<@@Name@@Form>) -> Result<Redirect> {{\n    let @@name@@ = @@actor@@.authorize::<Update, _>(@@Name@@::find(*id).one_or_404(&ctx.db).await?)?;\n    let input = form.validate().or_render(|form| Edit {{ form, @@name@@: (*@@name@@).clone() }})?;\n    let @@name@@ = ops::update_@@name@@(&ctx, @@name@@, input).await?;\n    Ok(Redirect::to(@@names@@::show::path(@@name@@.id)).notice(\"{human} updated.\"))\n}}\n"),
            ),
            (
                "destroy",
                format!("use kiln::{{policy::Delete, prelude::*}};\n\nuse crate::{{actions::@@names@@, ops, records::*}};\n\n#[action(DELETE \"/@@names@@/{{id}}\")]\npub async fn destroy(ctx: Ctx, @@actor@@: Current<@@Actor@@>, id: Param<Id<@@Name@@>>) -> Result<Redirect> {{\n    let @@name@@ = @@actor@@.authorize::<Delete, _>(@@Name@@::find(*id).one_or_404(&ctx.db).await?)?;\n    ops::delete_@@name@@(&ctx, @@name@@).await?;\n    Ok(Redirect::to(@@names@@::index::path()).notice(\"{human} deleted.\"))\n}}\n"),
            ),
        ];
        for (verb, template) in files {
            plan.file(format!("src/actions/{}/{verb}.rs", self.plural), fill(&template, &v));
            plan.module(format!("src/actions/{}/mod.rs", self.plural), format!("pub mod {verb};"));
        }
        plan.module("src/actions/mod.rs", format!("pub mod {};", self.plural));
    }

    fn views(&self, plan: &mut Plan) {
        let v = self.values();
        let show_view = if self.labeled_refs().is_empty() {
            "use kiln::prelude::*;\n\nuse crate::records::@@Name@@;\n\n#[derive(View)]\n#[view(template = \"@@names@@/show\")]\npub struct Show {\n    pub @@name@@: @@Name@@,\n    pub can_edit: bool,\n}\n".to_owned()
        } else {
            format!(
                "use kiln::prelude::*;\n\nuse crate::records::{{@@Name@@, @@name@@}};\n\n#[derive(View)]\n#[view(template = \"@@names@@/show\")]\npub struct Show {{\n    pub @@name@@: Loaded<@@Name@@, {}>,\n    pub can_edit: bool,\n}}\n",
                self.labeled_rel_tuple()
            )
        };
        let views = [
            (
                "index",
                "Index",
                "use kiln::prelude::*;\n\nuse crate::records::@@Name@@;\n\n#[derive(View)]\n#[view(template = \"@@names@@/index\")]\npub struct Index {\n    pub @@names@@: Vec<@@Name@@>,\n}\n",
            ),
            ("show", "Show", show_view.as_str()),
            (
                "new",
                "New",
                "use kiln::prelude::*;\n\nuse crate::forms::@@Name@@Form;\n\n#[derive(View)]\n#[view(template = \"@@names@@/new\")]\npub struct New {\n    pub form: FormState<@@Name@@Form>,\n}\n",
            ),
            (
                "edit",
                "Edit",
                "use kiln::prelude::*;\n\nuse crate::{forms::@@Name@@Form, records::@@Name@@};\n\n#[derive(View)]\n#[view(template = \"@@names@@/edit\")]\npub struct Edit {\n    pub form: FormState<@@Name@@Form>,\n    pub @@name@@: @@Name@@,\n}\n",
            ),
        ];
        for (file, ty, template) in views {
            plan.file(format!("src/views/{}/{file}.rs", self.plural), fill(template, &v));
            plan.module(format!("src/views/{}/mod.rs", self.plural), format!("mod {file};"));
            plan.module(format!("src/views/{}/mod.rs", self.plural), format!("pub use {file}::{ty};"));
        }
        plan.module("src/views/mod.rs", format!("pub mod {};", self.plural));
    }

    fn templates(&self, plan: &mut Plan) {
        let human_one = human(&self.snake);
        let human_many = human(&self.plural);
        let lower_one = human_one.to_lowercase();
        let display = self.display_field().map_or("id".to_owned(), Field::column);
        let title_expr = if display == "id" {
            format!("\"{human_one} #\" ~ {}.id", self.snake)
        } else {
            format!("{}.{display}", self.snake)
        };
        let mut v = self.values();
        let index = format!(
            "{{% set title = \"{human_many}\" %}}\n<h1>{human_many}</h1>\n{{% if current_@@actor@@ %}}\n<p><a href=\"{{{{ path(\"@@names@@::new\") }}}}\">New {lower_one}</a></p>\n{{% endif %}}\n{{% if @@names@@ %}}\n<ul>\n  {{% for @@name@@ in @@names@@ %}}\n  <li><a href=\"{{{{ path(\"@@names@@::show\", id=@@name@@.id) }}}}\">{{{{ {item} }}}}</a></li>\n  {{% endfor %}}\n</ul>\n{{% else %}}\n<p>No {lower_many} yet.</p>\n{{% endif %}}\n",
            item = title_expr,
            lower_many = human_many.to_lowercase(),
        );
        let rows: String = self
            .fields
            .iter()
            .filter(|f| f.column() != display)
            // On private records the viewer is the owner, so there's no owner row.
            .filter(|f| !(self.private && Some(&f.name) == self.owner.as_ref()))
            .map(|f| {
                if let Some(label) = &f.target_label {
                    let value = if f.optional {
                        format!("{{{{ @@name@@.{rel}.{label} if @@name@@.{rel} else \"—\" }}}}", rel = f.name)
                    } else {
                        format!("{{{{ @@name@@.{}.{label} }}}}", f.name)
                    };
                    return format!("  <dt>{}</dt><dd>{value}</dd>\n", f.label());
                }
                let value = match (f.kind, f.optional) {
                    (Kind::Timestamp, _) => format!("{{{{ @@name@@.{} | datetime }}}}", f.column()),
                    (Kind::Bool, _) => format!("{{{{ \"Yes\" if @@name@@.{} else \"No\" }}}}", f.column()),
                    (_, true) => {
                        format!("{{{{ @@name@@.{} if @@name@@.{} is not none else \"—\" }}}}", f.column(), f.column())
                    }
                    _ => format!("{{{{ @@name@@.{} }}}}", f.column()),
                };
                format!("  <dt>{}</dt><dd>{value}</dd>\n", f.label())
            })
            .collect();
        let show = format!(
            "{{% set title = {title_expr} %}}\n<h1>{{{{ title }}}}</h1>\n<dl>\n{rows}</dl>\n{{% if can_edit %}}\n<p>\n  <a href=\"{{{{ path(\"@@names@@::edit\", id=@@name@@.id) }}}}\">Edit</a>\n  {{{{ form_start(path(\"@@names@@::destroy\", id=@@name@@.id), method=\"delete\", class=\"inline\") }}}}<button>Delete</button>{{{{ form_end() }}}}\n</p>\n{{% endif %}}\n<p><a href=\"{{{{ path(\"@@names@@::index\") }}}}\">All {lower_many}</a></p>\n",
            lower_many = human_many.to_lowercase(),
        );
        let new = format!(
            "{{% set title = \"New {lower_one}\" %}}\n<h1>New {lower_one}</h1>\n{{{{ form_start(path(\"@@names@@::create\")) }}}}\n  {{{{ fields(form) }}}}\n  <button>Create {lower_one}</button>\n{{{{ form_end() }}}}\n<p><a href=\"{{{{ path(\"@@names@@::index\") }}}}\">Cancel</a></p>\n"
        );
        let edit = format!(
            "{{% set title = \"Edit {lower_one}\" %}}\n<h1>Edit {lower_one}</h1>\n{{{{ form_start(path(\"@@names@@::update\", id=@@name@@.id), method=\"patch\") }}}}\n  {{{{ fields(form) }}}}\n  <button>Save {lower_one}</button>\n{{{{ form_end() }}}}\n<p><a href=\"{{{{ path(\"@@names@@::show\", id=@@name@@.id) }}}}\">Cancel</a></p>\n"
        );
        v.push(("display", &display));
        for (file, contents) in [("index", index), ("show", show), ("new", new), ("edit", edit)] {
            plan.file(format!("templates/{}/{file}.html.jinja", self.plural), fill(&contents, &v));
        }
    }

    fn tests(&self, plan: &mut Plan) {
        let v = self.values();
        let human_one = human(&self.snake);
        let fields = self.form_fields();
        let mut setup = String::new();
        let pairs = |prefix: &str, blank: Option<&str>| -> String {
            fields
                .iter()
                .map(|f| {
                    let value = if Some(f.column().as_str()) == blank {
                        "\"\"".to_owned()
                    } else if f.kind == Kind::Ref {
                        format!("&{}_id", f.name)
                    } else {
                        format!("\"{}\"", f.sample(prefix))
                    };
                    format!("(\"{}\", {value})", f.column())
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        for f in fields.iter().filter(|f| f.kind == Kind::Ref) {
            setup.push_str(&format!(
                "    let {}_id = app.create::<{}>().await?.id.to_string();\n",
                f.name,
                f.target.as_deref().unwrap_or("?")
            ));
        }
        let create_pairs = pairs("Example", None);
        let update_pairs = pairs("Updated", None);
        let (owner_setup, owner_check) = match self.owner_field() {
            Some(owner) => (
                format!("    let owner = @@Actor@@::find(@@name@@.{}).one_or_404(&app.db).await?;\n", owner.column()),
                format!(
                    "    let @@name@@ = @@Name@@::query().one_or_404(&app.db).await?;\n    assert_eq!(@@name@@.{}, user.id);\n",
                    owner.column()
                ),
            ),
            None => (
                "    let owner = app.create::<@@Actor@@>().await?;\n".to_owned(),
                "    assert_eq!(@@Name@@::query().count(&app.db).await?, 1);\n".to_owned(),
            ),
        };
        let changed = self.display_field().map(|f| {
            format!(
                "    let @@name@@ = @@Name@@::find(@@name@@.id).one_or_404(&app.db).await?;\n    assert_eq!(@@name@@.{}, \"{}\");\n",
                f.column(),
                f.sample("Updated")
            )
        });
        let mut tests = format!(
            "//! Generated by `kiln g resource`. Extend these as the {lower} gains behavior.\n\nuse @@krate@@::{{actions::@@names@@, records::*}};\nuse kiln::{{prelude::*, testing::TestApp}};\n\n#[kiln::test]\nasync fn index_lists_@@names@@(app: TestApp) -> Result {{\n    let @@name@@ = app.create::<@@Name@@>().await?;\n    app.get(@@names@@::index::path()).await.assert_status(200).assert_contains(&@@names@@::show::path(@@name@@.id));\n    Ok(())\n}}\n\n#[kiln::test]\nasync fn show_renders_@@a@@_@@name@@(app: TestApp) -> Result {{\n    let @@name@@ = app.create::<@@Name@@>().await?;\n    app.get(@@names@@::show::path(@@name@@.id)).await.assert_status(200);\n    Ok(())\n}}\n\n#[kiln::test]\nasync fn signed_out_users_cannot_create_@@names@@(app: TestApp) -> Result {{\n{setup}    app.client().post_form(@@names@@::create::path(), &[{create_pairs}]).await.assert_status(401);\n    assert_eq!(@@Name@@::query().count(&app.db).await?, 0);\n    Ok(())\n}}\n\n#[kiln::test]\nasync fn signed_in_users_create_@@names@@(app: TestApp) -> Result {{\n    let user = app.create::<@@Actor@@>().await?;\n{setup}    app.sign_in_as(&user)\n        .post_form(@@names@@::create::path(), &[{create_pairs}])\n        .await\n        .assert_status(303)\n        .assert_flash(\"{human_one} created.\");\n{owner_check}    Ok(())\n}}\n",
            lower = human_one.to_lowercase(),
        );
        if let Some(required) = fields.iter().find(|f| f.is_text() && !f.optional) {
            let invalid_pairs = pairs("Example", Some(&required.column()));
            tests.push_str(&format!(
                "\n#[kiln::test]\nasync fn invalid_input_rerenders_the_form(app: TestApp) -> Result {{\n    let user = app.create::<@@Actor@@>().await?;\n{setup}    app.sign_in_as(&user)\n        .post_form(@@names@@::create::path(), &[{invalid_pairs}])\n        .await\n        .assert_status(422)\n        .assert_contains(\"{label} can&#x27;t be blank\");\n    assert_eq!(@@Name@@::query().count(&app.db).await?, 0);\n    Ok(())\n}}\n",
                label = required.label(),
            ));
        }
        tests.push_str(&format!(
            "\n#[kiln::test]\nasync fn owner_updates_@@a@@_@@name@@(app: TestApp) -> Result {{\n    let @@name@@ = app.create::<@@Name@@>().await?;\n{owner_setup}{setup}    app.sign_in_as(&owner)\n        .patch_form(@@names@@::update::path(@@name@@.id), &[{update_pairs}])\n        .await\n        .assert_redirect(&@@names@@::show::path(@@name@@.id))\n        .assert_flash(\"{human_one} updated.\");\n{changed}    Ok(())\n}}\n\n#[kiln::test]\nasync fn owner_deletes_@@a@@_@@name@@(app: TestApp) -> Result {{\n    let @@name@@ = app.create::<@@Name@@>().await?;\n{owner_setup}    app.sign_in_as(&owner)\n        .delete(@@names@@::destroy::path(@@name@@.id))\n        .await\n        .assert_redirect(&@@names@@::index::path())\n        .assert_flash(\"{human_one} deleted.\");\n    assert_eq!(@@Name@@::query().count(&app.db).await?, 0);\n    Ok(())\n}}\n",
            changed = changed.unwrap_or_default(),
        ));
        if let (true, Some(owner)) = (self.private, self.owner_field()) {
            // Swap the public listing tests for ones proving records stay private.
            let start = tests.find("#[kiln::test]\nasync fn index_lists_@@names@@").expect("index test");
            let end = tests.find("#[kiln::test]\nasync fn signed_out_users_cannot_create").expect("create test");
            let private_tests = format!(
                "#[kiln::test]\nasync fn index_lists_only_your_@@names@@(app: TestApp) -> Result {{\n    let mine = app.create::<@@Name@@>().await?;\n    let theirs = app.create::<@@Name@@>().await?;\n    let owner = @@Actor@@::find(mine.{col}).one_or_404(&app.db).await?;\n    app.sign_in_as(&owner)\n        .get(@@names@@::index::path())\n        .await\n        .assert_status(200)\n        .assert_contains(&@@names@@::show::path(mine.id))\n        .assert_not_contains(&@@names@@::show::path(theirs.id));\n    Ok(())\n}}\n\n#[kiln::test]\nasync fn signed_out_users_cannot_list_@@names@@(app: TestApp) -> Result {{\n    app.get(@@names@@::index::path()).await.assert_status(401);\n    Ok(())\n}}\n\n#[kiln::test]\nasync fn owner_sees_@@a@@_@@name@@(app: TestApp) -> Result {{\n    let @@name@@ = app.create::<@@Name@@>().await?;\n    let owner = @@Actor@@::find(@@name@@.{col}).one_or_404(&app.db).await?;\n    app.sign_in_as(&owner).get(@@names@@::show::path(@@name@@.id)).await.assert_status(200);\n    Ok(())\n}}\n\n",
                col = owner.column()
            );
            tests.replace_range(start..end, &private_tests);
        }
        if self.owner.is_some() {
            let (name, see) = if self.private {
                (
                    "other_users_cannot_see_or_change",
                    "    stranger.get(@@names@@::show::path(@@name@@.id)).await.assert_status(403);\n",
                )
            } else {
                ("other_users_cannot_change", "")
            };
            tests.push_str(&format!(
                "\n#[kiln::test]\nasync fn {name}_@@a@@_@@name@@(app: TestApp) -> Result {{\n    let @@name@@ = app.create::<@@Name@@>().await?;\n    let stranger = app.sign_in_as(&app.create::<@@Actor@@>().await?);\n{setup}{see}    stranger.get(@@names@@::edit::path(@@name@@.id)).await.assert_status(403);\n    stranger.patch_form(@@names@@::update::path(@@name@@.id), &[{update_pairs}]).await.assert_status(403);\n    stranger.delete(@@names@@::destroy::path(@@name@@.id)).await.assert_status(403);\n    assert_eq!(@@Name@@::query().count(&app.db).await?, 1);\n    Ok(())\n}}\n"
            ));
        }
        let mut values = v.clone();
        values.push(("krate", &self.krate));
        plan.file(format!("tests/{}.rs", self.plural), fill(&tests, &values));
    }
}

/// "a" or "an" before a snake_case word.
fn article(word: &str) -> &'static str {
    let starts = |p: &str| word.starts_with(p);
    let vowel = matches!(word.chars().next(), Some('a' | 'e' | 'i' | 'o'))
        || (starts("u") && !["uni", "use", "usu", "uti", "ur", "uk"].iter().any(|p| starts(p)));
    if vowel { "an" } else { "a" }
}

fn snake_of(pascal_name: &str) -> String {
    snake(pascal_name)
}

/// `kiln g action posts publish POST /posts/{slug}/publish`
fn action(resource: &str, name: &str, method: &str, path: &str) -> Result<Plan, String> {
    if !is_snake(resource) || !is_snake(name) {
        return Err(
            "resource and action names are snake_case, e.g. `kiln g action posts publish POST /posts/{slug}/publish`"
                .into(),
        );
    }
    let method = method.to_ascii_uppercase();
    if !["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&method.as_str()) {
        return Err(format!("`{method}` isn't an HTTP method"));
    }
    if !path.starts_with('/') {
        return Err("paths start with `/`".into());
    }
    let captures: Vec<&str> = path.split('/').filter_map(|s| s.strip_prefix('{')?.strip_suffix('}')).collect();
    let mut args = vec!["_ctx: Ctx".to_owned()];
    args.extend(captures.iter().map(|c| format!("{c}: Param<String>")));
    let ret = if method == "GET" { "Result<String>" } else { "Result<Redirect>" };
    let mut plan = Plan::default();
    plan.file(
        format!("src/actions/{resource}/{name}.rs"),
        format!(
            "use kiln::prelude::*;\n\n#[action({method} \"{path}\")]\npub async fn {name}({}) -> {ret} {{\n    todo!(\"implement {resource}::{name}\")\n}}\n",
            args.join(", ")
        ),
    );
    plan.module(format!("src/actions/{resource}/mod.rs"), format!("pub mod {name};"));
    plan.module("src/actions/mod.rs", format!("pub mod {resource};"));
    Ok(plan)
}

/// `kiln g op publish_post`
fn op(name: &str) -> Result<Plan, String> {
    if !is_snake(name) || !name.contains('_') {
        return Err("operations are named `verb_noun`, e.g. `publish_post`".into());
    }
    let mut plan = Plan::default();
    plan.file(
        format!("src/ops/{name}.rs"),
        format!(
            "use kiln::prelude::*;\n\n/// Take the records it changes as `Authorized<_, _>` proofs, and use `ctx.db.transaction`\n/// when it writes more than once.\npub async fn {name}(ctx: &Ctx) -> Result {{\n    let _ = ctx;\n    todo!(\"implement {name}\")\n}}\n"
        ),
    );
    plan.module("src/ops/mod.rs", format!("mod {name};"));
    plan.module("src/ops/mod.rs", format!("pub use {name}::{name};"));
    plan.module("src/lib.rs", "pub mod ops;");
    Ok(plan)
}

/// `kiln g part Post`: a part block, plus `parts` in the record's `#[record(..)]`.
fn part(app: &App, record: &str) -> Result<Plan, String> {
    let pascal = pascal(record);
    let snake = snake(&pascal);
    let record_path = app.path(&format!("src/records/{snake}.rs"));
    let source = std::fs::read_to_string(&record_path)
        .map_err(|_| format!("no `{pascal}` record ({} is missing)", record_path.display()))?;
    let mut plan = Plan::default();
    if !source.contains(", parts") && !source.contains("(parts") {
        let updated = source.replacen("#[record(", "#[record(parts, ", 1);
        if updated == source {
            return Err(format!("couldn't find `#[record(..)]` in {}", record_path.display()));
        }
        plan.replace(format!("src/records/{snake}.rs"), updated);
    }
    plan.file(
        format!("src/parts/{snake}.rs"),
        format!(
            "use kiln::prelude::*;\n\nuse crate::records::{pascal};\n\n/// Presentation logic for {lower}s. Templates read each method as a value: `{{{{ {snake}.method_name }}}}`.\n#[part]\nimpl {pascal} {{\n    pub fn label(&self) -> String {{\n        format!(\"{pascal} #{{}}\", self.id)\n    }}\n}}\n",
            lower = human(&snake).to_lowercase(),
        ),
    );
    plan.module("src/parts/mod.rs", format!("mod {snake};"));
    plan.module("src/lib.rs", "pub mod parts;");
    Ok(plan)
}

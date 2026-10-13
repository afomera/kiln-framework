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
    /// owner:ref:User`. Types: string, text, rich_text, integer, float, bool, timestamp,
    /// ref:<Record>; a trailing `?` makes a field optional, and `:internal`
    /// (`sent_at:timestamp?:internal`) keeps a field the app sets off the form. The first required ref to a record people sign
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
    /// A background job in src/jobs, with a test: `kiln g job send_digest`.
    Job {
        name: String,
        #[arg(long)]
        force: bool,
    },
    /// An email in src/mail, its HTML and text templates, and a test: `kiln g mail welcome_email`.
    Mail {
        name: String,
        #[arg(long)]
        force: bool,
    },
    /// A notification (inbox, mail, broadcast) in src/notifications, with a test:
    /// `kiln g notification comment_posted`. The recipient is the record people sign in as
    /// (`--to Merchant` for another).
    Notification {
        name: String,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        force: bool,
    },
    /// A `/settings/notifications` page where people turn notification channels on and off.
    /// `kiln g notification` adds it the first time.
    NotificationSettings {
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        force: bool,
    },
    /// Password authentication for `User`: sign up, sign in and out, password resets,
    /// email confirmation, and a page listing where you're signed in. Adds
    /// `password_digest` and `confirmed_at` to the User record (with a migration) and
    /// replaces the starter's demo sign-in.
    Auth {
        #[arg(long)]
        no_migration: bool,
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
                (action(app, &resource, &name, &method, &path)?, force, None)
            }
            Generate::Op { name, force } => (op(app, &name)?, force, None),
            Generate::Part { record, force } => (part(app, &record)?, force, None),
            Generate::Job { name, force } => (job(app, &name)?, force, None),
            Generate::Mail { name, force } => (mail(app, &name)?, force, None),
            Generate::Notification { name, to, force } => (notification(app, &name, to.as_deref())?, force, None),
            Generate::NotificationSettings { to, force } => {
                let to = recipient(app, to.as_deref())?;
                let mut plan = Plan::default();
                notification_settings(app, &to, &mut plan);
                (plan, force, None)
            }
            Generate::Auth { no_migration, force } => {
                (auth(app, force)?, force, (!no_migration).then(|| "add_passwords_to_users".to_owned()))
            }
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
    /// Formatted text in the rich text editor (`kiln::RichText`).
    RichText,
    Integer,
    Float,
    Bool,
    Timestamp,
    Ref,
    /// An uploaded file (`kiln::Attachment`).
    File,
    /// An uploaded image: a file limited to `image/*`.
    Image,
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
    /// `rich_text?`: the editor may be left empty (no `length` rule).
    may_be_empty: bool,
    /// `sent_at:timestamp?:internal`: set by the app, not the form (starts empty).
    internal: bool,
}

impl Field {
    fn parse(spec: &str) -> Result<Self, String> {
        let mut parts: Vec<&str> = spec.split(':').collect();
        let internal = parts.len() > 2 && parts.last() == Some(&"internal");
        if internal {
            parts.pop();
        }
        let (name, ty, target) = match parts.as_slice() {
            [name, ty] => (*name, *ty, None),
            [name, "ref", target] => (*name, "ref", Some(*target)),
            _ => {
                return Err(format!(
                    "`{spec}`: fields are `name:type`, `name:ref:Record`, or either with `:internal` (set by the app, not the form)"
                ));
            }
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
            "rich_text" | "richtext" => Kind::RichText,
            "integer" | "int" => Kind::Integer,
            "float" => Kind::Float,
            "bool" | "boolean" => Kind::Bool,
            "timestamp" | "datetime" => Kind::Timestamp,
            "ref" => Kind::Ref,
            "file" | "attachment" => Kind::File,
            "image" => Kind::Image,
            other => {
                return Err(format!(
                    "`{spec}`: unknown type `{other}`; use string, text, rich_text, integer, float, bool, timestamp, file, image or ref:<Record>"
                ));
            }
        };
        if kind == Kind::Bool && optional {
            return Err(format!("`{spec}`: bool fields can't be optional; an unchecked box is false"));
        }
        // An empty editor is an empty RichText, so `?` means "may be left empty", not Option.
        let rich_optional = kind == Kind::RichText && optional;
        if internal && !optional && !matches!(kind, Kind::Bool | Kind::Integer) {
            return Err(format!(
                "`{spec}`: an internal field needs a starting value; make it optional (`{name}:{ty}?:internal`), or a bool or integer (false / 0)"
            ));
        }
        Ok(Self {
            name,
            kind,
            optional: optional && !rich_optional,
            target,
            target_label: None,
            may_be_empty: rich_optional,
            internal,
        })
    }

    fn column(&self) -> String {
        if self.kind == Kind::Ref { format!("{}_id", self.name) } else { self.name.clone() }
    }

    fn base_type(&self) -> String {
        match self.kind {
            Kind::String | Kind::Text => "String".into(),
            Kind::RichText => "RichText".into(),
            Kind::Integer => "i64".into(),
            Kind::Float => "f64".into(),
            Kind::Bool => "bool".into(),
            Kind::Timestamp => "Timestamp".into(),
            Kind::Ref => format!("Id<{}>", self.target.as_deref().unwrap_or("?")),
            Kind::File | Kind::Image => "Attachment".into(),
        }
    }

    fn rust_type(&self) -> String {
        if self.optional { format!("Option<{}>", self.base_type()) } else { self.base_type() }
    }

    /// `email`, `contact_email`: an address (validated, and sampled as one).
    fn is_email(&self) -> bool {
        self.kind == Kind::String && (self.name == "email" || self.name.ends_with("_email"))
    }

    /// What a new record starts with when the form doesn't set the field.
    fn starting_value(&self) -> &'static str {
        match self.kind {
            _ if self.optional => "None",
            Kind::Bool => "false",
            _ => "0",
        }
    }

    fn is_file(&self) -> bool {
        matches!(self.kind, Kind::File | Kind::Image)
    }

    /// The upload a test submits for a file field.
    fn sample_upload(&self) -> String {
        match self.kind {
            Kind::Image => format!("Upload::png(\"{}.png\")", self.name),
            _ => format!("Upload::text(\"{}.txt\", \"{} contents\")", self.name, human(&self.name)),
        }
    }

    fn is_text(&self) -> bool {
        matches!(self.kind, Kind::String | Kind::Text)
    }

    fn label(&self) -> String {
        human(&self.name)
    }

    /// A value a test submits in a form.
    fn sample(&self, prefix: &str) -> String {
        if self.is_email() {
            return format!("{}@example.com", prefix.to_lowercase());
        }
        match self.kind {
            Kind::String | Kind::Text | Kind::RichText => format!("{prefix} {}", self.name.replace('_', " ")),
            // Small values, so adding a `range(..)` rule later doesn't break the tests.
            Kind::Integer => "3".into(),
            Kind::Float => "2.5".into(),
            Kind::Bool => "true".into(),
            Kind::Timestamp => "2026-01-01T12:00".into(),
            Kind::Ref | Kind::File | Kind::Image => String::new(),
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
        let mut signs_in = signs_in_records(app);
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

    /// Fields the form edits: everything except the owner and internal fields.
    fn form_fields(&self) -> Vec<&Field> {
        self.fields.iter().filter(|f| Some(&f.name) != self.owner.as_ref() && !f.internal).collect()
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
                (Kind::String, _) if f.is_email() => format!("format!(\"{}{{n}}@example.com\")", self.snake),
                (Kind::String, _) => format!("format!(\"{} {{n}}\")", human(&f.name)),
                (Kind::Text, _) => format!("\"{} text.\".into()", human(&f.name)),
                (Kind::RichText, _) => format!("\"<p>{} text.</p>\".into()", human(&f.name)),
                (Kind::Integer, _) => "n as i64".into(),
                (Kind::Float, _) => "1.5".into(),
                (Kind::Bool, _) => "false".into(),
                (Kind::Timestamp, _) => "Timestamp::now()".into(),
                (Kind::File | Kind::Image, _) => {
                    let (file, kind) = if f.kind == Kind::Image {
                        (format!("{}.png", f.name), "image/png")
                    } else {
                        (format!("{}.txt", f.name), "text/plain")
                    };
                    let bytes = if f.kind == Kind::Image {
                        format!("kiln::testing::Upload::png(\"{file}\").bytes")
                    } else {
                        "b\"Contents.\".to_vec()".to_owned()
                    };
                    setup.push_str(&format!(
                        "        let {} = app.ctx().attach(\"{file}\", \"{kind}\", {bytes}).await?;\n",
                        f.name
                    ));
                    f.name.clone()
                }
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
                (Kind::String, false) if f.is_email() => body.push_str("    #[validate(email)]\n"),
                (Kind::String, true) if f.is_email() => body.push_str("    #[validate(email)]\n"),
                (Kind::String, false) => body.push_str("    #[validate(length(min = 1, max = 200))]\n"),
                (Kind::String, true) => body.push_str("    #[validate(length(max = 200))]\n"),
                (Kind::Text, false) => body.push_str("    #[validate(length(min = 1))]\n    #[form(textarea)]\n"),
                (Kind::Text, true) => body.push_str("    #[form(textarea)]\n"),
                (Kind::Image, _) => body.push_str("    #[validate(file(max_size = \"10MB\", accept = \"image/*\"))]\n"),
                (Kind::File, _) => body.push_str("    #[validate(file(max_size = \"25MB\"))]\n"),
                (Kind::RichText, _) if !f.may_be_empty => {
                    body.push_str("    /// The rich text editor; `length` counts words people see, not markup.\n    #[validate(length(min = 1))]\n")
                }
                _ => {}
            }
            body.push_str(&format!("    pub {}: {},\n", f.column(), f.rust_type()));
        }
        let from_record: String = fields
            .iter()
            .map(|f| {
                let clone = if f.is_text() || f.is_file() || f.kind == Kind::RichText { ".clone()" } else { "" };
                format!("            {}: {}.{}{clone},\n", f.column(), self.snake, f.column())
            })
            .collect();
        let mut new_values: String =
            fields.iter().map(|f| format!("            {c}: self.{c},\n", c = f.column())).collect();
        for f in self.fields.iter().filter(|f| f.internal) {
            new_values.push_str(&format!("            {}: {},\n", f.column(), f.starting_value()));
        }
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
                    (Kind::RichText, _) => format!("{{{{ @@name@@.{} | rich_text }}}}", f.column()),
                    (Kind::Image, _) => format!(
                        "{{% if @@name@@.{c} %}}<img src=\"{{{{ @@name@@.{c} | variant(width=640) }}}}\" alt=\"\">{{% else %}}—{{% endif %}}",
                        c = f.column()
                    ),
                    (Kind::File, _) => format!(
                        "{{% if @@name@@.{c} %}}<a href=\"{{{{ @@name@@.{c}.url }}}}\">{{{{ @@name@@.{c}.filename }}}}</a>{{% else %}}—{{% endif %}}",
                        c = f.column()
                    ),
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
            "{{% set title = {title_expr} %}}\n<h1>{{{{ title }}}}</h1>\n<dl>\n{rows}</dl>\n{{% if can_edit %}}\n<p>\n  <a href=\"{{{{ path(\"@@names@@::edit\", id=@@name@@.id) }}}}\">Edit</a>\n  {{{{ button_to(\"Delete\", path(\"@@names@@::destroy\", id=@@name@@.id), method=\"delete\", class=\"inline\", confirm=\"Delete this {lower_one}? This can't be undone.\", confirm_button=\"Delete\", tone=\"danger\") }}}}\n</p>\n{{% endif %}}\n<p><a href=\"{{{{ path(\"@@names@@::index\") }}}}\">All {lower_many}</a></p>\n",
            lower_many = human_many.to_lowercase(),
        );
        let multipart = if self.form_fields().iter().any(|f| f.is_file()) { ", multipart=true" } else { "" };
        let new = format!(
            "{{% set title = \"New {lower_one}\" %}}\n<h1>New {lower_one}</h1>\n{{{{ form_start(path(\"@@names@@::create\"){multipart}) }}}}\n  {{{{ fields(form) }}}}\n  <button>Create {lower_one}</button>\n{{{{ form_end() }}}}\n<p><a href=\"{{{{ path(\"@@names@@::index\") }}}}\">Cancel</a></p>\n"
        );
        let edit = format!(
            "{{% set title = \"Edit {lower_one}\" %}}\n<h1>Edit {lower_one}</h1>\n{{{{ form_start(path(\"@@names@@::update\", id=@@name@@.id), method=\"patch\"{multipart}) }}}}\n  {{{{ fields(form) }}}}\n  <button>Save {lower_one}</button>\n{{{{ form_end() }}}}\n<p><a href=\"{{{{ path(\"@@names@@::show\", id=@@name@@.id) }}}}\">Cancel</a></p>\n"
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
        // With file fields, forms go up as multipart with an upload per file.
        let uploads: Vec<String> = fields
            .iter()
            .filter(|f| f.is_file())
            .map(|f| format!("(\"{}\", {})", f.column(), f.sample_upload()))
            .collect();
        let with_files = |pairs: String| {
            if uploads.is_empty() { pairs } else { format!("{pairs}], &[{}", uploads.join(", ")) }
        };
        let create_pairs = with_files(pairs("Example", None));
        let update_pairs = with_files(pairs("Updated", None));
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
            let invalid_pairs = with_files(pairs("Example", Some(&required.column())));
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
        if !uploads.is_empty() {
            tests = tests
                .replace("post_form(", "post_multipart(")
                .replace("patch_form(", "patch_multipart(")
                .replace("testing::TestApp}", "testing::{TestApp, Upload}}");
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
fn action(app: &App, resource: &str, name: &str, method: &str, path: &str) -> Result<Plan, String> {
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
    // `posts` with an existing `Post` record and an `{id}` in the path: load, authorize, test.
    if captures.first() == Some(&"id") {
        let record = std::fs::read_dir(app.path("src/records"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".rs").map(str::to_owned))
            .find(|r| r != "mod" && plural(r) == resource);
        if let Some(record) = record {
            return record_action(app, resource, &record, name, &method, path, &captures[1..]);
        }
    }
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

/// `kiln g action issues send POST /issues/{id}/send` when `Issue` exists: the action loads
/// the record, authorizes the signed-in user (`Read` for GET, `Update` otherwise), and has a
/// view (GET) or a redirect, plus tests.
fn record_action(
    app: &App,
    resource: &str,
    record: &str,
    name: &str,
    method: &str,
    path: &str,
    other_captures: &[&str],
) -> Result<Plan, String> {
    let record_pascal = pascal(record);
    let mut signs_in = signs_in_records(app);
    if signs_in.is_empty() {
        signs_in.push("User".to_owned());
    }
    let actor = signs_in[0].clone();
    let actor_snake = snake_of(&actor);
    let get = method == "GET";
    let permission = if get { "Read" } else { "Update" };
    let has_policy = app.path(&format!("src/policies/{record}.rs")).exists();
    let has_show = app.path(&format!("src/actions/{resource}/show.rs")).exists();
    let record_source = std::fs::read_to_string(app.path(&format!("src/records/{record}.rs"))).unwrap_or_default();
    // `belongs_to(owner: User)` on the record: the owner is who may act on it.
    let owner = record_source.split("belongs_to(").skip(1).find_map(|rest| {
        let (rel, ty) = rest.split_once(')')?.0.split_once(':')?;
        (ty.trim() == actor).then(|| rel.trim().to_owned())
    });
    let human_name = human(name);
    let pascal_name = pascal(name);

    let mut args = vec![
        "ctx: Ctx".to_owned(),
        format!("{actor_snake}: Current<{actor}>"),
        format!("id: Param<Id<{record_pascal}>>"),
    ];
    args.extend(other_captures.iter().map(|c| format!("{c}: Param<String>")));
    let load = format!("{record_pascal}::find(*id).one_or_404(&ctx.db).await?");
    let (imports, load_line) = if has_policy {
        (
            format!("use kiln::{{policy::{permission}, prelude::*}};"),
            format!("    let {record} = {actor_snake}.authorize::<{permission}, _>({load})?;\n"),
        )
    } else {
        (
            "use kiln::prelude::*;".to_owned(),
            format!(
                "    // No policy for {record_pascal} yet: add src/policies/{record}.rs and authorize here.\n    let _ = &{actor_snake};\n    let {record} = {load};\n"
            ),
        )
    };
    let unwrap = if has_policy { ".into_inner()" } else { "" };
    let mut plan = Plan::default();
    let body = if get {
        format!(
            "{imports}\n\nuse crate::{{records::*, views::{resource}::{pascal_name}}};\n\n#[action(GET \"{path}\")]\npub async fn {name}({}) -> Result<{pascal_name}> {{\n{load_line}    Ok({pascal_name} {{ {record}: {record}{unwrap} }})\n}}\n",
            args.join(", ")
        )
    } else {
        let back = if has_show {
            format!("{resource}::show::path({record}.id)")
        } else if app.path(&format!("src/actions/{resource}/index.rs")).exists() {
            format!("{resource}::index::path()")
        } else {
            "\"/\"".to_owned()
        };
        let actions = if back.starts_with('"') { String::new() } else { format!("actions::{resource}, ") };
        // Without a show page the record is only authorized, not used yet.
        let keep = if has_show { String::new() } else { format!("    let _ = {record};\n") };
        format!(
            "{imports}\n\nuse crate::{{{actions}records::*}};\n\n/// {human_name}. Do the work in an op that takes the `Authorized` proof\n/// (`kiln g op {name}_{record}`), so the rule stays in one place.\n#[action({method} \"{path}\")]\npub async fn {name}({}) -> Result<Redirect> {{\n{load_line}    // ops::{name}_{record}(&ctx, {record}).await?;\n{keep}    Ok(Redirect::to({back}).notice(\"Done.\"))\n}}\n",
            args.join(", ")
        )
    };
    plan.file(format!("src/actions/{resource}/{name}.rs"), body);
    plan.module(format!("src/actions/{resource}/mod.rs"), format!("pub mod {name};"));
    plan.module("src/actions/mod.rs", format!("pub mod {resource};"));
    if get {
        plan.file(
            format!("src/views/{resource}/{name}.rs"),
            format!(
                "use kiln::prelude::*;\n\nuse crate::records::{record_pascal};\n\n#[derive(View)]\n#[view(template = \"{resource}/{name}\")]\npub struct {pascal_name} {{\n    pub {record}: {record_pascal},\n}}\n"
            ),
        );
        plan.module(format!("src/views/{resource}/mod.rs"), format!("mod {name};"));
        plan.module(format!("src/views/{resource}/mod.rs"), format!("pub use {name}::{pascal_name};"));
        plan.module("src/views/mod.rs", format!("pub mod {resource};"));
        plan.file(
            format!("templates/{resource}/{name}.html.jinja"),
            format!("{{% set title = \"{human_name}\" %}}\n<h1>{human_name}</h1>\n<p>{record_pascal} #{{{{ {record}.id }}}}</p>\n"),
        );
    }

    // Tests: signed out is refused; the owner (when the record has one) gets through.
    let extra: String = other_captures.iter().map(|c| format!(", \"{c}\"")).collect();
    let call = format!("{resource}::{name}::path({record}.id{extra})");
    let request = match method {
        "GET" => format!("get_html({call}).await.assert_status(200)"),
        "DELETE" => format!("delete({call}).await.assert_status(303)"),
        "PATCH" => format!("patch_form({call}, &[]).await.assert_status(303)"),
        "PUT" => format!("put_form({call}, &[]).await.assert_status(303)"),
        _ => format!("post_form({call}, &[]).await.assert_status(303)"),
    };
    let signed_out = match method {
        "GET" => format!("get({call}).await"),
        "DELETE" => format!("delete({call}).await"),
        "PATCH" => format!("patch_form({call}, &[]).await"),
        "PUT" => format!("put_form({call}, &[]).await"),
        _ => format!("post_form({call}, &[]).await"),
    };
    let happy = match &owner {
        Some(rel) => format!(
            "\n#[kiln::test]\nasync fn the_owner_can_{name}_a_{record}(app: TestApp) -> Result {{\n    let {record} = app.create::<{record_pascal}>().await?;\n    let owner = {actor}::find({record}.{rel}_id).one_or_404(&app.db).await?;\n    app.sign_in_as(&owner).{request};\n    Ok(())\n}}\n"
        ),
        None => String::new(),
    };
    plan.file(
        format!("tests/{resource}_{name}.rs"),
        format!(
            "use {krate}::{{actions::{resource}, records::*}};\nuse kiln::{{prelude::*, testing::TestApp}};\n{happy}\n#[kiln::test]\nasync fn signed_out_people_cannot_{name}_a_{record}(app: TestApp) -> Result {{\n    let {record} = app.create::<{record_pascal}>().await?;\n    app.client().{signed_out}.assert_status(401);\n    Ok(())\n}}\n",
            krate = app.crate_name()
        ),
    );
    Ok(plan)
}

/// Ops and jobs are named for what they do: `publish_post`, not `post_publish` or `publish`.
fn check_verb_noun(app: &App, name: &str, what: &str, example: &str) -> Result<(), String> {
    if !is_snake(name) || !name.contains('_') {
        return Err(format!("{what} are named `verb_noun`, e.g. `{example}` (got `{name}`)"));
    }
    let (first, rest) = name.split_once('_').expect("checked above");
    let records: Vec<String> = std::fs::read_dir(app.root.join("src/records"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".rs").map(str::to_owned))
        .filter(|n| n != "mod")
        .collect();
    let noun = |w: &str| records.iter().any(|r| r == w || format!("{r}s") == w);
    if noun(first) && !noun(rest) {
        return Err(format!("{what} are named `verb_noun`, so the verb comes first: `{rest}_{first}`, not `{name}`"));
    }
    Ok(())
}

/// `kiln g op publish_post`
fn op(app: &App, name: &str) -> Result<Plan, String> {
    check_verb_noun(app, name, "operations", "publish_post")?;
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

/// `kiln g job send_digest`
fn job(app: &App, name: &str) -> Result<Plan, String> {
    check_verb_noun(app, name, "jobs", "send_digest")?;
    let pascal = pascal(name);
    let mut plan = Plan::default();
    plan.file(
        format!("src/jobs/{name}.rs"),
        fill(
            "use kiln::prelude::*;\nuse serde::{Deserialize, Serialize};\n\n/// Enqueue it with `@@Name@@ { .. }.enqueue(&ctx.db).await?` (inside the write's\n/// transaction, so it runs only if the write commits). Fields are stored as JSON: pass ids,\n/// not records, and load fresh data in `perform`. It may run more than once, so make it\n/// safe to repeat.\n#[derive(Job, Serialize, Deserialize, Debug)]\n#[job(queue = \"default\", attempts = 5)]\npub struct @@Name@@ {}\n\nimpl Perform for @@Name@@ {\n    async fn perform(self, ctx: &Ctx) -> Result {\n        let _ = ctx;\n        todo!(\"implement @@Name@@\")\n    }\n}\n",
            &[("Name", &pascal)],
        ),
    );
    plan.module("src/jobs/mod.rs", format!("mod {name};"));
    plan.module("src/jobs/mod.rs", format!("pub use {name}::{pascal};"));
    plan.module("src/lib.rs", "pub mod jobs;");
    plan.file(
        format!("tests/{name}.rs"),
        fill(
            "use @@krate@@::jobs::@@Name@@;\nuse kiln::{prelude::*, testing::TestApp};\n\n#[kiln::test]\nasync fn @@name@@_runs(app: TestApp) -> Result {\n    @@Name@@ {}.enqueue(&app.db).await?;\n    app.assert_enqueued::<@@Name@@>(1).await?;\n    assert_eq!(app.perform_enqueued_jobs().await?, 1);\n    app.assert_enqueued::<@@Name@@>(0).await?;\n    Ok(())\n}\n",
            &[("Name", &pascal), ("name", name), ("krate", &app.crate_name())],
        ),
    );
    Ok(plan)
}

/// Record types people sign in as, from `signs_in::<T, _>(..)` in src/app.rs.
fn signs_in_records(app: &App) -> Vec<String> {
    let app_rs = std::fs::read_to_string(app.path("src/app.rs")).unwrap_or_default();
    app_rs
        .split("signs_in::<")
        .skip(1)
        .filter_map(|rest| rest.split([',', '>']).next().map(|t| t.trim().to_owned()))
        .filter(|t| !t.is_empty())
        .collect()
}

/// `kiln g notification comment_posted [--to User]`
fn notification(app: &App, name: &str, to: Option<&str>) -> Result<Plan, String> {
    if !is_snake(name) || !name.contains('_') {
        return Err("notifications are named for what happened, e.g. `comment_posted`".into());
    }
    let to = recipient(app, to)?;
    let to_snake = snake(&to);
    let pascal_name = pascal(name);
    let human_name = human(name);
    let values = [
        ("Name", pascal_name.as_str()),
        ("name", name),
        ("To", to.as_str()),
        ("to", to_snake.as_str()),
        ("human", human_name.as_str()),
        ("krate", &app.crate_name()),
    ];
    let mut plan = Plan::default();
    plan.file(
        format!("src/notifications/{name}.rs"),
        fill(
            "use kiln::prelude::*;\nuse serde::{Deserialize, Serialize};\n\nuse crate::records::@@To@@;\n\n/// Send it with `@@Name@@ { .. }.notify(&tx, &@@to@@).await?` inside the write's transaction.\n/// Fields are stored as JSON until delivery: pass ids, and load fresh data in `deliver`.\n/// People can turn its email off at /settings/notifications (`optional(..)` lists the\n/// channels they may turn off; `optional()` for notices nobody may mute).\n#[derive(Notification, Serialize, Deserialize, Debug)]\n#[notification(description = \"@@human@@\", optional(mail))]\npub struct @@Name@@ {}\n\nimpl Notify for @@Name@@ {\n    type To = @@To@@;\n\n    /// What `to` gets. `via.inbox(..)` for the in-app inbox, `via.mail(SomeMail { .. })?` for\n    /// email, `via.broadcast(stream, turbo)` for open pages. Check `to`'s preferences here.\n    async fn deliver(&self, _ctx: &Ctx, _to: &@@To@@, via: &mut Via) -> Result {\n        via.inbox(\"@@human@@\");\n        // via.mail(SomeMail { to: to.email.clone(), .. })?;   // skipped if they turned email off\n        Ok(())\n    }\n}\n",
            &values,
        ),
    );
    plan.module("src/notifications/mod.rs", format!("mod {name};"));
    plan.module("src/notifications/mod.rs", format!("pub use {name}::{pascal_name};"));
    plan.module("src/lib.rs", "pub mod notifications;");
    if !app.path("src/actions/notifications/index.rs").exists() {
        notification_inbox(app, &to, &mut plan);
    }
    if !app.path("src/actions/notification_settings/index.rs").exists() {
        notification_settings(app, &to, &mut plan);
    }
    plan.file(
        format!("tests/notification_{name}.rs"),
        fill(
            "use @@krate@@::{notifications::@@Name@@, records::*};\nuse kiln::{prelude::*, testing::TestApp};\n\n#[kiln::test]\nasync fn @@name@@_reaches_the_inbox(app: TestApp) -> Result {\n    let @@to@@ = app.create::<@@To@@>().await?;\n    @@Name@@ {}.notify(&app.db, &@@to@@).await?;\n    app.perform_enqueued_jobs().await?;\n    let inbox = app.db.inbox(&@@to@@, 10).await?;\n    assert_eq!(inbox.len(), 1);\n    assert_eq!(inbox[0].kind, \"@@Name@@\");\n    assert_eq!(app.db.unread_notifications(&@@to@@).await?, 1);\n    Ok(())\n}\n",
            &values,
        ),
    );
    Ok(plan)
}

/// The record notifications go to: `--to`, else the first `signs_in::<..>` record.
fn recipient(app: &App, to: Option<&str>) -> Result<String, String> {
    let to = match to {
        Some(to) => pascal(to),
        None => signs_in_records(app).first().cloned().unwrap_or_else(|| "User".to_owned()),
    };
    let to_snake = snake(&to);
    if !app.path(&format!("src/records/{to_snake}.rs")).exists() {
        return Err(format!("no `{to}` record (src/records/{to_snake}.rs) to notify"));
    }
    Ok(to)
}

/// `/notifications`: the signed-in recipient's inbox, newest first, with read tracking.
fn notification_inbox(app: &App, to: &str, plan: &mut Plan) {
    let to_snake = snake(to);
    let values = [("To", to), ("to", to_snake.as_str()), ("krate", &app.crate_name())];
    let files: [(&str, &str); 6] = [
        (
            "src/actions/notifications/index.rs",
            "use kiln::prelude::*;\n\nuse crate::{records::@@To@@, views::notifications::Index};\n\n/// The signed-in @@to@@'s inbox. Link to it from the layout; `db.unread_notifications`\n/// gives a count for a badge.\n#[action(GET \"/notifications\")]\npub async fn index(ctx: Ctx, @@to@@: Current<@@To@@>) -> Result<Index> {\n    let notifications = ctx.db.inbox(&*@@to@@, 50).await?;\n    let unread = ctx.db.unread_notifications(&*@@to@@).await?;\n    Ok(Index { notifications, unread })\n}\n",
        ),
        (
            "src/actions/notifications/read.rs",
            "use kiln::prelude::*;\n\nuse crate::{actions::notifications, records::@@To@@};\n\n/// Marks one notification read and follows its link (someone else's id just goes to the inbox).\n#[action(PATCH \"/notifications/{id}/read\")]\npub async fn read(ctx: Ctx, @@to@@: Current<@@To@@>, id: Param<i64>) -> Result<Redirect> {\n    let link = ctx.db.read_notification(&*@@to@@, *id).await?.and_then(|n| n.url);\n    Ok(Redirect::to(link.unwrap_or_else(notifications::index::path)))\n}\n",
        ),
        (
            "src/actions/notifications/read_all.rs",
            "use kiln::prelude::*;\n\nuse crate::{actions::notifications, records::@@To@@};\n\n#[action(PATCH \"/notifications/read\")]\npub async fn read_all(ctx: Ctx, @@to@@: Current<@@To@@>) -> Result<Redirect> {\n    let count = ctx.db.mark_all_notifications_read(&*@@to@@).await?;\n    Ok(Redirect::to(notifications::index::path()).notice(format!(\"Marked {count} read.\")))\n}\n",
        ),
        (
            "src/views/notifications/index.rs",
            "use kiln::{notify::InboxItem, prelude::*};\n\n#[derive(View)]\n#[view(template = \"notifications/index\")]\npub struct Index {\n    pub notifications: Vec<InboxItem>,\n    pub unread: i64,\n}\n",
        ),
        (
            "templates/notifications/index.html.jinja",
            "{% set title = \"Notifications\" %}\n<h1>Notifications{% if unread %} <small>({{ unread }} unread)</small>{% endif %}</h1>\n{% if unread %}\n{{ form_start(path(\"notifications::read_all\"), method=\"patch\") }}<button>Mark all read</button>{{ form_end() }}\n{% endif %}\n<ul class=\"notifications\">\n  {% for n in notifications %}\n  <li id=\"notification_{{ n.id }}\"{% if not n.read %} class=\"unread\"{% endif %}>\n    {{ form_start(path(\"notifications::read\", id=n.id), method=\"patch\", class=\"inline\") }}<button class=\"link\">{{ n.title }}</button>{{ form_end() }}\n    {% if n.body %}<br><small>{{ n.body }}</small>{% endif %}\n    <br><small>{{ n.created_at | datetime }}</small>\n  </li>\n  {% else %}\n  <li>Nothing yet.</li>\n  {% endfor %}\n</ul>\n",
        ),
        (
            "tests/notifications_inbox.rs",
            "use @@krate@@::{actions::notifications, records::*};\nuse kiln::{prelude::*, testing::TestApp};\n\n#[kiln::test]\nasync fn the_inbox_lists_and_marks_notifications_read(app: TestApp) -> Result {\n    let @@to@@ = app.create::<@@To@@>().await?;\n    let client = app.sign_in_as(&@@to@@);\n    client.get_html(notifications::index::path()).await.assert_status(200).assert_contains(\"Nothing yet.\");\n    client.patch_form(notifications::read_all::path(), &[]).await.assert_flash(\"Marked 0 read.\");\n    Ok(())\n}\n\n#[kiln::test]\nasync fn nobody_reads_someone_elses_notifications(app: TestApp) -> Result {\n    let @@to@@ = app.create::<@@To@@>().await?;\n    // An id that isn't theirs marks nothing and just lands on their inbox.\n    app.sign_in_as(&@@to@@).patch_form(notifications::read::path(999_999), &[]).await.assert_redirect(&notifications::index::path());\n    Ok(())\n}\n\n#[kiln::test]\nasync fn signed_out_people_have_no_inbox(app: TestApp) -> Result {\n    app.client().get(notifications::index::path()).await.assert_status(401);\n    Ok(())\n}\n",
        ),
    ];
    for (path, template) in files {
        plan.file(path, fill(template, &values));
    }
    for module in ["index", "read", "read_all"] {
        plan.module("src/actions/notifications/mod.rs", format!("pub mod {module};"));
    }
    plan.module("src/actions/mod.rs", "pub mod notifications;");
    plan.module("src/views/notifications/mod.rs", "mod index;");
    plan.module("src/views/notifications/mod.rs", "pub use index::Index;");
    plan.module("src/views/mod.rs", "pub mod notifications;");
}

/// `/settings/notifications`: every switch `db.notification_prefs` lists, with on/off.
fn notification_settings(app: &App, to: &str, plan: &mut Plan) {
    let to_snake = snake(to);
    let values = [("To", to), ("to", to_snake.as_str()), ("krate", &app.crate_name())];
    plan.file(
        "src/actions/notification_settings/index.rs",
        fill(
            "use kiln::prelude::*;\n\nuse crate::{records::@@To@@, views::notification_settings::Index};\n\n/// What the signed-in @@to@@ gets, and where.\n#[action(GET \"/settings/notifications\")]\npub async fn index(ctx: Ctx, @@to@@: Current<@@To@@>) -> Result<Index> {\n    Ok(Index { prefs: ctx.db.notification_prefs(&*@@to@@).await? })\n}\n",
            &values,
        ),
    );
    plan.file(
        "src/actions/notification_settings/update.rs",
        fill(
            "use kiln::{notify::Channel, prelude::*};\n\nuse crate::{actions::notification_settings, records::@@To@@};\n\n/// Turns one channel on or off: `kind` is a notification type, or `*` for all of them.\n#[action(PATCH \"/settings/notifications/{kind}/{channel}/{state}\")]\npub async fn update(\n    ctx: Ctx,\n    @@to@@: Current<@@To@@>,\n    kind: Param<String>,\n    channel: Param<String>,\n    state: Param<String>,\n) -> Result<Redirect> {\n    let channel: Channel = channel.parse()?;\n    let on = state.as_str() == \"on\";\n    ctx.db.set_notification_pref(&*@@to@@, &kind, channel, on).await?;\n    Ok(Redirect::to(notification_settings::index::path()).notice(if on { \"Turned on.\" } else { \"Turned off.\" }))\n}\n",
            &values,
        ),
    );
    plan.module("src/actions/notification_settings/mod.rs", "pub mod index;");
    plan.module("src/actions/notification_settings/mod.rs", "pub mod update;");
    plan.module("src/actions/mod.rs", "pub mod notification_settings;");
    plan.file(
        "src/views/notification_settings/index.rs",
        "use kiln::{notify::NotificationPref, prelude::*};\n\n#[derive(View)]\n#[view(template = \"notification_settings/index\")]\npub struct Index {\n    pub prefs: Vec<NotificationPref>,\n}\n".to_owned(),
    );
    plan.module("src/views/notification_settings/mod.rs", "mod index;");
    plan.module("src/views/notification_settings/mod.rs", "pub use index::Index;");
    plan.module("src/views/mod.rs", "pub mod notification_settings;");
    plan.file(
        "templates/notification_settings/index.html.jinja",
        "{% set title = \"Notifications\" %}\n<h1>Notification settings</h1>\n<table>\n  <thead><tr><th>Notification</th><th>Channel</th><th></th></tr></thead>\n  <tbody>\n    {% for pref in prefs %}\n    <tr id=\"pref_{{ pref.kind }}_{{ pref.channel }}\">\n      <td>{% if pref.kind == \"*\" %}<strong>{{ pref.description }}</strong>{% else %}{{ pref.description }}{% endif %}</td>\n      <td>{{ pref.channel }}</td>\n      <td>\n        {% if pref.enabled %}\n        {{ form_start(path(\"notification_settings::update\", kind=pref.kind, channel=pref.channel, state=\"off\"), method=\"patch\", class=\"inline\") }}<button>Turn off</button>{{ form_end() }}\n        {% else %}\n        <em>Off</em> {{ form_start(path(\"notification_settings::update\", kind=pref.kind, channel=pref.channel, state=\"on\"), method=\"patch\", class=\"inline\") }}<button>Turn on</button>{{ form_end() }}\n        {% endif %}\n      </td>\n    </tr>\n    {% else %}\n    <tr><td colspan=\"3\">No notifications can be turned off yet.</td></tr>\n    {% endfor %}\n  </tbody>\n</table>\n".to_owned(),
    );
    plan.file(
        "tests/notification_settings.rs",
        fill(
            "use @@krate@@::{actions::notification_settings, records::*};\nuse kiln::{notify::Channel, prelude::*, testing::TestApp};\n\n#[kiln::test]\nasync fn people_turn_every_email_off_and_on(app: TestApp) -> Result {\n    let @@to@@ = app.create::<@@To@@>().await?;\n    let client = app.sign_in_as(&@@to@@);\n    client.get_html(notification_settings::index::path()).await.assert_status(200);\n    client.patch_form(notification_settings::update::path(\"*\", \"mail\", \"off\"), &[]).await.assert_flash(\"Turned off.\");\n    let mail = |prefs: &[kiln::notify::NotificationPref]| prefs.iter().find(|p| p.kind == \"*\" && p.channel == Channel::Mail).map(|p| p.enabled);\n    assert_eq!(mail(&app.db.notification_prefs(&@@to@@).await?), Some(false));\n    client.patch_form(notification_settings::update::path(\"*\", \"mail\", \"on\"), &[]).await.assert_flash(\"Turned on.\");\n    assert_eq!(mail(&app.db.notification_prefs(&@@to@@).await?), Some(true));\n    Ok(())\n}\n\n#[kiln::test]\nasync fn signed_out_people_cannot_change_settings(app: TestApp) -> Result {\n    app.client().patch_form(notification_settings::update::path(\"*\", \"mail\", \"off\"), &[]).await.assert_status(401);\n    Ok(())\n}\n",
            &values,
        ),
    );
}

/// `kiln g mail welcome_email`
fn mail(app: &App, name: &str) -> Result<Plan, String> {
    if !is_snake(name) {
        return Err("mail is named in snake_case, e.g. `welcome_email`".into());
    }
    let pascal = pascal(name);
    let values = [("Name", pascal.as_str()), ("name", name), ("krate", &app.crate_name()), ("human", &human(name))];
    let mut plan = Plan::default();
    plan.file(
        format!("src/mail/{name}.rs"),
        fill(
            "use kiln::prelude::*;\nuse serde::Serialize;\n\n/// Send it with `@@Name@@ { .. }.deliver_later(&tx).await?` inside the write's transaction.\n/// Every field is available to the templates and the subject.\n#[derive(Mail, Serialize, Debug)]\n#[mail(template = \"mail/@@name@@\", subject = \"@@human@@ for {{ name }}\", preview)]\npub struct @@Name@@ {\n    pub to: String,\n    pub name: String,\n}\n\n/// Sample data for /kiln/mail/previews (may read `ctx.db`).\nimpl Preview for @@Name@@ {\n    async fn preview(_ctx: &Ctx) -> Result<Self> {\n        Ok(Self { to: \"ada@example.com\".into(), name: \"Ada\".into() })\n    }\n}\n",
            &values,
        ),
    );
    plan.file(
        format!("templates/mail/{name}.html.jinja"),
        "<p>Hi {{ name }},</p>\n<p>Write the message here. Link with absolute URLs: <a href=\"{{ url(\"/\") }}\">{{ url(\"/\") }}</a></p>\n".to_owned(),
    );
    plan.file(
        format!("templates/mail/{name}.txt.jinja"),
        "Hi {{ name }},\n\nWrite the message here.\n\n{{ url(\"/\") }}\n".to_owned(),
    );
    if !app.path("templates/layouts/mail.html.jinja").exists() {
        plan.file(
            "templates/layouts/mail.html.jinja",
            "<!doctype html>\n<html>\n  <body style=\"font:15px/1.5 system-ui,sans-serif;color:#1c1917\">\n    {{ content }}\n  </body>\n</html>\n".to_owned(),
        );
    }
    plan.module("src/mail/mod.rs", format!("mod {name};"));
    plan.module("src/mail/mod.rs", format!("pub use {name}::{pascal};"));
    plan.module("src/lib.rs", "pub mod mail;");
    plan.file(
        format!("tests/mail_{name}.rs"),
        fill(
            "use @@krate@@::mail::@@Name@@;\nuse kiln::{prelude::*, testing::TestApp};\n\n#[kiln::test]\nasync fn @@name@@_is_delivered(app: TestApp) -> Result {\n    @@Name@@ { to: \"ada@example.com\".into(), name: \"Ada\".into() }.deliver_later(&app.db).await?;\n    app.perform_enqueued_jobs().await?;\n    let mail = app.assert_mailed(\"ada@example.com\", \"Hi Ada\").await?;\n    assert!(mail.text.is_some());\n    assert!(app.render_mail_preview::<@@Name@@>().await?.contains(\"Hi Ada\"));\n    Ok(())\n}\n",
            &values,
        ),
    );
    Ok(plan)
}

macro_rules! auth_files {
    ($($path:literal),* $(,)?) => {
        &[$(($path, include_str!(concat!("../templates/auth/", $path)))),*]
    };
}

const AUTH_FILES: &[(&str, &str)] = auth_files![
    "src/actions/confirmations/show.rs",
    "src/actions/password_settings/edit.rs",
    "src/actions/password_settings/update.rs",
    "src/actions/passwords/create.rs",
    "src/actions/passwords/edit.rs",
    "src/actions/passwords/new.rs",
    "src/actions/passwords/update.rs",
    "src/actions/registrations/create.rs",
    "src/actions/registrations/new.rs",
    "src/actions/sessions/create.rs",
    "src/actions/sessions/destroy.rs",
    "src/actions/sessions/new.rs",
    "src/actions/sign_ins/destroy.rs",
    "src/actions/sign_ins/destroy_others.rs",
    "src/actions/sign_ins/index.rs",
    "src/forms/change_password_form.rs",
    "src/forms/new_password_form.rs",
    "src/forms/password_reset_request_form.rs",
    "src/forms/sign_in_form.rs",
    "src/forms/sign_up_form.rs",
    "src/mail/email_confirmation.rs",
    "src/mail/password_changed.rs",
    "src/mail/password_reset.rs",
    "src/ops/change_password.rs",
    "src/ops/confirm_email.rs",
    "src/ops/request_password_reset.rs",
    "src/ops/reset_password.rs",
    "src/ops/sign_up.rs",
    "src/views/password_settings/edit.rs",
    "src/views/passwords/edit.rs",
    "src/views/passwords/new.rs",
    "src/views/registrations/new.rs",
    "src/views/sessions/new.rs",
    "src/views/sign_ins/index.rs",
    "templates/layouts/mail.html.jinja",
    "templates/mail/email_confirmation.html.jinja",
    "templates/mail/email_confirmation.txt.jinja",
    "templates/mail/password_changed.html.jinja",
    "templates/mail/password_changed.txt.jinja",
    "templates/mail/password_reset.html.jinja",
    "templates/mail/password_reset.txt.jinja",
    "templates/password_settings/edit.html.jinja",
    "templates/passwords/edit.html.jinja",
    "templates/passwords/new.html.jinja",
    "templates/registrations/new.html.jinja",
    "templates/sessions/new.html.jinja",
    "templates/sign_ins/index.html.jinja",
    "tests/auth.rs",
    "tests/sessions.rs",
];

/// `kiln g auth`
fn auth(app: &App, force: bool) -> Result<Plan, String> {
    let read = |path: &str| std::fs::read_to_string(app.path(path)).ok();
    let user = read("src/records/user.rs")
        .ok_or("`kiln g auth` adds passwords to the `User` record, but src/records/user.rs is missing")?;
    if user.contains("password_digest") && !force {
        return Err("User already has a `password_digest`; auth looks set up (pass --force to regenerate)".into());
    }
    let mut plan = Plan::default();
    if !user.contains("password_digest") {
        let anchor = "    pub email: String,\n";
        let fields = "    /// Argon2 hash from `kiln::auth::hash_password`. `#[hidden]`: never serialized.\n    #[hidden]\n    pub password_digest: String,\n    pub confirmed_at: Option<Timestamp>,\n";
        if !user.contains(anchor) {
            return Err("couldn't find `pub email: String,` in src/records/user.rs; auth needs an email field".into());
        }
        plan.replace("src/records/user.rs", user.replacen(anchor, &format!("{anchor}{fields}"), 1));
        for (path, value) in [
            (
                "src/factories.rs",
                "password_digest: kiln::auth::hash_password(\"password\")?, confirmed_at: Some(Timestamp::now())",
            ),
            (
                "src/seed.rs",
                "password_digest: kiln::auth::hash_password(\"password\")?, confirmed_at: Some(Timestamp::now())",
            ),
        ] {
            if let Some(source) = read(path) {
                plan.replace(path, add_to_struct_literals(&source, "NewUser {", value));
            }
        }
    }
    let krate = app.crate_name();
    for (path, contents) in AUTH_FILES {
        if *path == "templates/layouts/mail.html.jinja" && app.path(path).exists() {
            continue;
        }
        let contents = fill(contents, &[("krate", &krate)]);
        if app.path(path).exists() { plan.replace(*path, contents) } else { plan.file(*path, contents) }
    }
    // Only once: regenerating (`--force`) must not add the links again.
    if let Some(layout) = read("templates/layouts/app.html.jinja").filter(|l| !l.contains("registrations::new")) {
        let updated = layout
            .replacen(
                "<span>{{ current_user.name }}</span>",
                "<a href=\"{{ path(\"sign_ins::index\") }}\">{{ current_user.name }}</a>",
                1,
            )
            .replacen(
                "<a href=\"{{ path(\"sessions::new\") }}\">Sign in</a>",
                "<a href=\"{{ path(\"sessions::new\") }}\">Sign in</a>\n      <a href=\"{{ path(\"registrations::new\") }}\">Sign up</a>",
                1,
            );
        if updated != layout {
            plan.replace("templates/layouts/app.html.jinja", updated);
        }
    }
    for (module, items) in [
        (
            "forms",
            &[
                "sign_in_form:SignInForm",
                "sign_up_form:SignUpForm",
                "password_reset_request_form:PasswordResetRequestForm",
                "new_password_form:NewPasswordForm",
                "change_password_form:ChangePasswordForm",
            ][..],
        ),
        (
            "mail",
            &[
                "email_confirmation:EmailConfirmation",
                "password_reset:PasswordReset",
                "password_changed:PasswordChanged",
            ][..],
        ),
        (
            "ops",
            &[
                "sign_up:sign_up",
                "confirm_email:confirm_email",
                "request_password_reset:request_password_reset",
                "reset_password:reset_password",
                "reset_password:user_for_reset",
                "change_password:change_password",
                "change_password:PasswordChange",
            ][..],
        ),
    ] {
        for item in items {
            let (file, name) = item.split_once(':').expect("file:name");
            plan.module(format!("src/{module}/mod.rs"), format!("mod {file};"));
            plan.module(format!("src/{module}/mod.rs"), format!("pub use {file}::{name};"));
        }
    }
    plan.module("src/lib.rs", "pub mod mail;");
    plan.module("src/lib.rs", "pub mod ops;");
    for (resource, verbs) in [
        ("sessions", &["new", "create", "destroy"][..]),
        ("registrations", &["new", "create"][..]),
        ("confirmations", &["show"][..]),
        ("passwords", &["new", "create", "edit", "update"][..]),
        ("sign_ins", &["index", "destroy", "destroy_others"][..]),
        ("password_settings", &["edit", "update"][..]),
    ] {
        plan.module("src/actions/mod.rs", format!("pub mod {resource};"));
        for verb in verbs {
            plan.module(format!("src/actions/{resource}/mod.rs"), format!("pub mod {verb};"));
        }
    }
    for (resource, views) in [
        ("sessions", &["new:New"][..]),
        ("registrations", &["new:New"][..]),
        ("passwords", &["new:New", "edit:Edit"][..]),
        ("sign_ins", &["index:Index"][..]),
        ("password_settings", &["edit:Edit"][..]),
    ] {
        plan.module("src/views/mod.rs", format!("pub mod {resource};"));
        for view in views {
            let (file, name) = view.split_once(':').expect("file:name");
            plan.module(format!("src/views/{resource}/mod.rs"), format!("mod {file};"));
            plan.module(format!("src/views/{resource}/mod.rs"), format!("pub use {file}::{name};"));
        }
    }
    // Argon2 is slow unoptimized; a standalone app can speed it up in dev builds.
    if let Some(manifest) = read("Cargo.toml")
        && manifest.contains("[workspace]")
        && !manifest.contains("[profile.dev.package.argon2]")
    {
        plan.replace(
            "Cargo.toml",
            format!(
                "{}\n# Password hashing is slow without optimizations.\n[profile.dev.package.argon2]\nopt-level = 3\n",
                manifest.trim_end()
            ),
        );
    }
    Ok(plan)
}

/// Adds `fields` to every `Name { .. }` struct literal starting with `open` in `source`.
fn add_to_struct_literals(source: &str, open: &str, fields: &str) -> String {
    let mut out = String::new();
    let mut rest = source;
    while let Some(start) = rest.find(open) {
        let body_start = start + open.len();
        let Some(close) = closing_brace(&rest[body_start..]) else { break };
        let body = rest[body_start..body_start + close].trim_end();
        let separator = if body.trim().is_empty() || body.ends_with(',') { " " } else { ", " };
        out.push_str(&rest[..body_start]);
        out.push_str(body);
        out.push_str(separator);
        out.push_str(fields);
        out.push(' ');
        rest = &rest[body_start + close..];
    }
    out.push_str(rest);
    out
}

/// The offset of the `}` closing a block that's already open, skipping string literals
/// (`format!("User {n}")`) and nested braces.
fn closing_brace(text: &str) -> Option<usize> {
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for (i, c) in text.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string && depth == 0 => return Some(i),
            '}' if !in_string => depth -= 1,
            _ => {}
        }
    }
    None
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

//! Forms: typed input, validation and re-rendering with errors.
//!
//! ```ignore
//! #[action(POST "/posts")]
//! pub async fn create(ctx: Ctx, user: Current<User>, form: Form<PostForm>) -> Result<Redirect> {
//!     let input = form.validate().or_render(|form| New { form })?;   // 422 + errors on failure
//!     ...
//! }
//! ```

use std::{collections::BTreeMap, fmt::Display, future::Future, marker::PhantomData, ops::Deref};

use axum::{
    body::Bytes,
    extract::{FromRequest, Request},
};
use serde::{Serialize, Serializer, ser::SerializeMap};

use crate::{Ctx, Error, Id, Result, Timestamp, View};

/// A struct that can be read from a submitted form. Derive it with `#[derive(Form)]`.
pub trait FormInput: Sized + Send + 'static {
    const FIELDS: &'static [&'static str];
    /// How each field renders, in declaration order. Used by `{{ fields(form) }}`.
    const SPECS: &'static [FieldSpec];

    /// Parses and validates. Field-level messages go into the returned `Errors`.
    fn from_fields(fields: &Fields) -> std::result::Result<Self, Errors>;

    /// The form's values as strings, to prefill an edit form.
    fn to_fields(&self) -> Fields;
}

/// Validation that needs the app, e.g. uniqueness checks against the database.
/// Called by [`Form::validate_with`] after the field rules pass.
pub trait CheckWith: FormInput + Sync {
    fn check(&self, ctx: &Ctx, errors: &mut Errors) -> impl Future<Output = Result<()>> + Send;
}

/// Raw submitted values by field name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fields(BTreeMap<String, String>);

impl Fields {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.0.insert(name.into(), value.into());
    }

    /// Parses one field, recording a message in `errors` when it doesn't parse.
    pub fn parse<T: FormField>(&self, name: &str, errors: &mut Errors) -> Option<T> {
        match T::from_field(self.get(name)) {
            Ok(value) => Some(value),
            Err(message) => {
                errors.add(name, message);
                None
            }
        }
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for Fields {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self(iter.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
    }
}

/// Validation messages by field. Messages not tied to a field go under `_base`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Errors(BTreeMap<String, Vec<String>>);

impl Errors {
    pub fn add(&mut self, field: &str, message: impl Into<String>) {
        self.0.entry(field.to_owned()).or_default().push(message.into());
    }

    /// A message about the form as a whole.
    pub fn base(&mut self, message: impl Into<String>) {
        self.add("_base", message);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, field: &str) -> &[String] {
        self.0.get(field).map_or(&[], Vec::as_slice)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_slice()))
    }
}

impl Display for Errors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let parts: Vec<String> =
            self.iter().flat_map(|(field, msgs)| msgs.iter().map(move |m| format!("{field} {m}"))).collect();
        f.write_str(&parts.join("; "))
    }
}

/// How a form field renders, derived from its Rust type and `#[validate]` rules so the
/// HTML attributes (`required`, `maxlength`, `type="email"`) never drift from the rules.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct FieldSpec {
    pub name: &'static str,
    pub label: &'static str,
    /// `text`, `textarea`, `email`, `password`, `number`, `decimal`, `checkbox`,
    /// `datetime-local` or `select`.
    pub input: &'static str,
    pub required: bool,
    pub min_length: Option<usize>,
    pub max_length: Option<usize>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// Choices for `select` inputs.
    pub options: &'static [&'static str],
    pub hint: Option<&'static str>,
    pub placeholder: Option<&'static str>,
    /// Browser autofill hint, e.g. `current-password`. Password inputs default to
    /// `new-password` and email inputs to `email`.
    pub autocomplete: Option<&'static str>,
}

/// A Rust type a single form field parses into.
pub trait FormField: Sized {
    /// The HTML input it renders as.
    const INPUT: &'static str = "text";
    /// Whether the input must be filled in. `Option` fields and checkboxes aren't.
    const REQUIRED: bool = true;
    /// Choices, for enums.
    const OPTIONS: &'static [&'static str] = &[];

    /// `raw` is `None` when the field wasn't submitted at all.
    fn from_field(raw: Option<&str>) -> std::result::Result<Self, String>;
    fn to_field(&self) -> String;
}

impl FormField for String {
    fn from_field(raw: Option<&str>) -> std::result::Result<Self, String> {
        raw.map(|s| s.trim().to_owned()).ok_or_else(|| "is missing".to_owned())
    }

    fn to_field(&self) -> String {
        self.clone()
    }
}

/// Checkboxes: unchecked boxes aren't submitted, so a missing field is `false`.
impl FormField for bool {
    const INPUT: &'static str = "checkbox";
    const REQUIRED: bool = false;

    fn from_field(raw: Option<&str>) -> std::result::Result<Self, String> {
        match raw.map(str::trim) {
            None | Some("" | "0" | "false" | "off" | "no") => Ok(false),
            Some("1" | "true" | "on" | "yes") => Ok(true),
            Some(_) => Err("must be true or false".to_owned()),
        }
    }

    fn to_field(&self) -> String {
        if *self { "true".into() } else { String::new() }
    }
}

macro_rules! number_field {
    ($($t:ty => $input:literal, $msg:literal),*) => {$(
        impl FormField for $t {
            const INPUT: &'static str = $input;

            fn from_field(raw: Option<&str>) -> std::result::Result<Self, String> {
                let raw = raw.map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| "can't be blank".to_owned())?;
                raw.parse().map_err(|_| $msg.to_owned())
            }

            fn to_field(&self) -> String {
                self.to_string()
            }
        }
    )*};
}

number_field!(
    i64 => "number", "must be a whole number",
    i32 => "number", "must be a whole number",
    f64 => "decimal", "must be a number"
);

impl<T> FormField for Id<T> {
    const INPUT: &'static str = "number";

    fn from_field(raw: Option<&str>) -> std::result::Result<Self, String> {
        i64::from_field(raw).map(Id::new).map_err(|_| "must be an id".to_owned())
    }

    fn to_field(&self) -> String {
        self.to_string()
    }
}

/// Accepts RFC 3339 (`2026-10-12T14:30:00Z`) or an `<input type="datetime-local">` value,
/// read as UTC.
impl FormField for Timestamp {
    const INPUT: &'static str = "datetime-local";

    fn from_field(raw: Option<&str>) -> std::result::Result<Self, String> {
        let raw = raw.map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| "can't be blank".to_owned())?;
        if let Ok(ts) = raw.parse::<jiff::Timestamp>() {
            return Ok(Timestamp::from_jiff(ts));
        }
        raw.parse::<jiff::civil::DateTime>()
            .ok()
            .and_then(|dt| dt.to_zoned(jiff::tz::TimeZone::UTC).ok())
            .map(|z| Timestamp::from_jiff(z.timestamp()))
            .ok_or_else(|| "must be a date and time".to_owned())
    }

    /// The `datetime-local` format, so edit forms prefill.
    fn to_field(&self) -> String {
        self.as_jiff().strftime("%Y-%m-%dT%H:%M").to_string()
    }
}

/// Optional fields: blank or missing is `None`.
impl<T: FormField> FormField for Option<T> {
    const INPUT: &'static str = T::INPUT;
    const REQUIRED: bool = false;
    const OPTIONS: &'static [&'static str] = T::OPTIONS;

    fn from_field(raw: Option<&str>) -> std::result::Result<Self, String> {
        match raw.map(str::trim) {
            None | Some("") => Ok(None),
            Some(s) => T::from_field(Some(s)).map(Some),
        }
    }

    fn to_field(&self) -> String {
        self.as_ref().map(T::to_field).unwrap_or_default()
    }
}

/// Field rules used by `#[validate(..)]`.
pub mod rules {
    use super::Errors;

    /// What a rule can inspect. `None` (an empty optional field) skips the rule.
    pub trait RuleTarget {
        fn text(&self) -> Option<&str> {
            None
        }
        fn number(&self) -> Option<f64> {
            None
        }
    }

    impl RuleTarget for String {
        fn text(&self) -> Option<&str> {
            Some(self)
        }
    }

    impl RuleTarget for Option<String> {
        fn text(&self) -> Option<&str> {
            self.as_deref()
        }
    }

    macro_rules! numeric_target {
        ($($t:ty),*) => {$(
            impl RuleTarget for $t {
                fn number(&self) -> Option<f64> {
                    Some(*self as f64)
                }
            }

            impl RuleTarget for Option<$t> {
                fn number(&self) -> Option<f64> {
                    self.map(|v| v as f64)
                }
            }
        )*};
    }

    numeric_target!(i64, i32, f64);

    pub fn length(value: &impl RuleTarget, min: Option<usize>, max: Option<usize>, field: &str, errors: &mut Errors) {
        let Some(text) = value.text() else { return };
        let len = text.chars().count();
        if let Some(min) = min
            && len < min
        {
            errors.add(
                field,
                if min == 1 {
                    "can't be blank".to_owned()
                } else {
                    format!("is too short (minimum is {min} characters)")
                },
            );
        }
        if let Some(max) = max
            && len > max
        {
            errors.add(field, format!("is too long (maximum is {max} characters)"));
        }
    }

    pub fn range(value: &impl RuleTarget, min: Option<f64>, max: Option<f64>, field: &str, errors: &mut Errors) {
        let Some(n) = value.number() else { return };
        if let Some(min) = min
            && n < min
        {
            errors.add(field, format!("must be at least {min}"));
        }
        if let Some(max) = max
            && n > max
        {
            errors.add(field, format!("must be at most {max}"));
        }
    }

    pub fn email(value: &impl RuleTarget, field: &str, errors: &mut Errors) {
        let Some(text) = value.text() else { return };
        let valid = text.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
        });
        if !valid {
            errors.add(field, "must be an email address");
        }
    }

    /// For `Option` fields that must be filled in.
    pub fn required<T>(value: &Option<T>, field: &str, errors: &mut Errors) {
        if value.is_none() {
            errors.add(field, "can't be blank");
        }
    }
}

/// A submitted form, not yet validated. Extract it as the last action argument.
///
/// Fields not declared on `T` are rejected with 400, except framework fields that start
/// with `_` (`_csrf`, `_method`).
pub struct Form<T> {
    fields: Fields,
    _input: PhantomData<fn() -> T>,
}

impl<T: FormInput> Form<T> {
    /// A form from raw values, e.g. in tests.
    pub fn from_fields(fields: Fields) -> Self {
        Self { fields, _input: PhantomData }
    }

    pub fn fields(&self) -> &Fields {
        &self.fields
    }

    /// Runs the field rules.
    pub fn validate(self) -> std::result::Result<Valid<T>, Rejected<T>> {
        match T::from_fields(&self.fields) {
            Ok(value) => Ok(Valid(value)),
            Err(errors) => Err(Rejected::Invalid(FormState { fields: self.fields, errors, _input: PhantomData })),
        }
    }

    /// Runs the field rules, then `T`'s [`CheckWith`] checks (e.g. uniqueness).
    pub async fn validate_with(self, ctx: &Ctx) -> std::result::Result<Valid<T>, Rejected<T>>
    where
        T: CheckWith,
    {
        let fields = self.fields.clone();
        let valid = self.validate()?;
        let mut errors = Errors::default();
        valid.check(ctx, &mut errors).await.map_err(Rejected::Failed)?;
        if errors.is_empty() {
            Ok(valid)
        } else {
            Err(Rejected::Invalid(FormState { fields, errors, _input: PhantomData }))
        }
    }
}

impl<T: FormInput, S: Send + Sync> FromRequest<S> for Form<T> {
    type Rejection = Error;

    async fn from_request(request: Request, state: &S) -> std::result::Result<Self, Self::Rejection> {
        let json = request
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("application/json"));
        let bytes = Bytes::from_request(request, state).await.map_err(|e| Error::BadRequest(e.to_string()))?;
        let pairs: Vec<(String, String)> = if json {
            json_pairs(&bytes)?
        } else {
            serde_urlencoded::from_bytes(&bytes).map_err(|e| Error::BadRequest(format!("unreadable form body: {e}")))?
        };
        let mut fields = Fields::default();
        for (name, value) in pairs {
            if name.starts_with('_') {
                continue;
            }
            if !T::FIELDS.contains(&name.as_str()) {
                return Err(Error::BadRequest(format!(
                    "unexpected form field `{name}`; `{}` accepts: {}",
                    short_name::<T>(),
                    T::FIELDS.join(", ")
                )));
            }
            fields.set(name, value);
        }
        Ok(Self::from_fields(fields))
    }
}

/// A flat JSON object as form fields, so JSON and HTML forms share parsing and rules.
/// `null` means "not sent"; nested values are rejected.
fn json_pairs(bytes: &[u8]) -> Result<Vec<(String, String)>> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| Error::BadRequest(format!("unreadable JSON body: {e}")))?;
    let serde_json::Value::Object(object) = value else {
        return Err(Error::BadRequest("the JSON body must be an object of field values".into()));
    };
    let mut pairs = Vec::new();
    for (name, value) in object {
        let text = match value {
            serde_json::Value::Null => continue,
            serde_json::Value::String(s) => s,
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                return Err(Error::BadRequest(format!("field `{name}` must be a string, number or boolean")));
            }
        };
        pairs.push((name, text));
    }
    Ok(pairs)
}

fn short_name<T>() -> &'static str {
    let name = std::any::type_name::<T>();
    name.rsplit("::").next().unwrap_or(name)
}

/// Input that passed validation. Derefs to `T`.
#[derive(Debug, Clone)]
pub struct Valid<T>(T);

impl<T> Valid<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> Deref for Valid<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

/// Why validation didn't produce a value.
pub enum Rejected<T> {
    /// The input has errors; re-render the form with [`OrRender::or_render`].
    Invalid(FormState<T>),
    /// A check itself failed, e.g. the database was unreachable.
    Failed(Error),
}

/// Without `or_render`, invalid input is a 422 error report listing the messages.
impl<T> From<Rejected<T>> for Error {
    fn from(rejected: Rejected<T>) -> Self {
        match rejected {
            Rejected::Invalid(state) => Error::Invalid(state.errors),
            Rejected::Failed(e) => e,
        }
    }
}

/// Turns invalid input into a re-rendered page (422) with the user's values and messages.
pub trait OrRender<T> {
    fn or_render<V: View>(self, view: impl FnOnce(FormState<T>) -> V) -> Result<Valid<T>>;
}

impl<T> OrRender<T> for std::result::Result<Valid<T>, Rejected<T>> {
    fn or_render<V: View>(self, view: impl FnOnce(FormState<T>) -> V) -> Result<Valid<T>> {
        match self {
            Ok(valid) => Ok(valid),
            Err(Rejected::Invalid(state)) => Err(Error::Rendered {
                status: axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                page: crate::view::PendingRender::of(&view(state)),
            }),
            Err(Rejected::Failed(e)) => Err(e),
        }
    }
}

/// What a form template sees: each field's value and messages.
///
/// ```jinja
/// <input name="title" value="{{ form.title.value }}">
/// {% for message in form.title.errors %}<p class="error">{{ message }}</p>{% endfor %}
/// ```
///
/// Messages for the whole form are under `form._errors`.
pub struct FormState<T> {
    fields: Fields,
    errors: Errors,
    _input: PhantomData<fn() -> T>,
}

impl<T: FormInput> FormState<T> {
    /// A blank form.
    pub fn new() -> Self {
        Self { fields: Fields::default(), errors: Errors::default(), _input: PhantomData }
    }

    /// A form prefilled from a value, e.g. for an edit page.
    pub fn from_input(input: &T) -> Self {
        Self { fields: input.to_fields(), errors: Errors::default(), _input: PhantomData }
    }

    /// Adds a message, e.g. one found after validation: `state.with_error("email", "is taken")`.
    pub fn with_error(mut self, field: &str, message: impl Into<String>) -> Self {
        self.errors.add(field, message);
        self
    }

    pub fn errors(&self) -> &Errors {
        &self.errors
    }

    pub fn fields(&self) -> &Fields {
        &self.fields
    }
}

impl<T: FormInput> Default for FormState<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> std::fmt::Debug for FormState<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FormState").field("fields", &self.fields).field("errors", &self.errors).finish()
    }
}

impl<T: FormInput> Serialize for FormState<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Field<'a> {
            value: &'a str,
            errors: &'a [String],
            #[serde(flatten)]
            spec: &'a FieldSpec,
        }
        let mut map = serializer.serialize_map(Some(T::SPECS.len() + 2))?;
        for spec in T::SPECS {
            // Passwords are never sent back to the browser, even when the form re-renders.
            let value = if spec.input == "password" { "" } else { self.fields.get(spec.name).unwrap_or("") };
            let field = Field { value, errors: self.errors.get(spec.name), spec };
            map.serialize_entry(spec.name, &field)?;
        }
        // Field names in order, for `{{ fields(form) }}`.
        map.serialize_entry("_fields", T::FIELDS)?;
        map.serialize_entry("_errors", self.errors.get("_base"))?;
        map.end()
    }
}

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

/// Raw submitted values by field name. A name can hold several values (`tag=a&tag=b`), for
/// `Vec<T>` fields; single-value fields read the last one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fields(BTreeMap<String, Vec<String>>);

impl Fields {
    /// The field's value (the last one, when it was submitted more than once).
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).and_then(|values| values.last()).map(String::as_str)
    }

    /// Every value submitted under `name`, in order; empty when it wasn't submitted.
    pub fn get_all(&self, name: &str) -> &[String] {
        self.0.get(name).map_or(&[], Vec::as_slice)
    }

    /// Replaces the field's values with one value.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.0.insert(name.into(), vec![value.into()]);
    }

    /// Adds a value after the field's others, as a repeated key in a form body does.
    pub fn append(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.0.entry(name.into()).or_default().push(value.into());
    }

    /// Replaces the field's values.
    pub fn set_all(&mut self, name: impl Into<String>, values: Vec<String>) {
        self.0.insert(name.into(), values);
    }

    /// Parses one field, recording a message in `errors` when it doesn't parse.
    pub fn parse<T: FormField>(&self, name: &str, errors: &mut Errors) -> Option<T> {
        match T::from_values(self.0.get(name).map(Vec::as_slice)) {
            Ok(value) => Some(value),
            Err(message) => {
                errors.add(name, message);
                None
            }
        }
    }
}

/// Pairs collect like a form body: a repeated name keeps every value.
impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for Fields {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut fields = Self::default();
        for (k, v) in iter {
            fields.append(k, v);
        }
        fields
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
    /// `datetime-local`, `select`, or `checkboxes` (a `Vec<T>` field).
    pub input: &'static str,
    /// A `Vec<T>` field: several values under one name.
    pub multiple: bool,
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
    /// File inputs: the types the picker offers (`image/*`), from `#[validate(file(accept = ..))]`.
    pub accept: Option<&'static str>,
}

/// A Rust type a single form field parses into.
pub trait FormField: Sized {
    /// The HTML input it renders as.
    const INPUT: &'static str = "text";
    /// Whether the input must be filled in. `Option` fields and checkboxes aren't.
    const REQUIRED: bool = true;
    /// Choices, for enums.
    const OPTIONS: &'static [&'static str] = &[];
    /// Whether the field takes several values (`Vec<T>`).
    const MULTIPLE: bool = false;

    /// `raw` is `None` when the field wasn't submitted at all.
    fn from_field(raw: Option<&str>) -> std::result::Result<Self, String>;
    fn to_field(&self) -> String;

    /// Parses every value submitted under the field's name (`None` when it wasn't
    /// submitted). Single-value fields read the last one.
    fn from_values(raw: Option<&[String]>) -> std::result::Result<Self, String> {
        Self::from_field(raw.and_then(|values| values.last()).map(String::as_str))
    }

    /// The values that prefill the form.
    fn to_values(&self) -> Vec<String> {
        vec![self.to_field()]
    }
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

/// Several values under one name: checkbox groups and `<select multiple>` (`tag=a&tag=b`, or
/// `tag[]=a&tag[]=b`, or a JSON array). Blank values are skipped, so a hidden `""` input can
/// stand for "none chosen"; nothing submitted is an empty list.
impl<T: FormField> FormField for Vec<T> {
    const INPUT: &'static str = "checkboxes";
    const REQUIRED: bool = false;
    const OPTIONS: &'static [&'static str] = T::OPTIONS;
    const MULTIPLE: bool = true;

    fn from_field(raw: Option<&str>) -> std::result::Result<Self, String> {
        match raw {
            None => Self::from_values(None),
            Some(raw) => Self::from_values(Some(&[raw.to_owned()])),
        }
    }

    /// Comma-separated, for display; forms prefill from [`FormField::to_values`].
    fn to_field(&self) -> String {
        self.to_values().join(",")
    }

    fn from_values(raw: Option<&[String]>) -> std::result::Result<Self, String> {
        let values = raw.unwrap_or_default().iter().map(|v| v.trim()).filter(|v| !v.is_empty());
        values.map(|v| T::from_field(Some(v)).map_err(|message| item_message(v, &message))).collect()
    }

    fn to_values(&self) -> Vec<String> {
        self.iter().map(T::to_field).collect()
    }
}

/// `must be a whole number` about `x` → `"x" isn't a whole number`: a message about one item.
fn item_message(value: &str, message: &str) -> String {
    let value: String = value.chars().take(40).collect();
    match message.strip_prefix("must be ") {
        Some(rest) => format!("has \"{value}\", which isn't {rest}"),
        None => format!("has \"{value}\", which {message}"),
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
        fn attachment(&self) -> Option<&crate::storage::Attachment> {
            None
        }
        /// A list field's items: `length` counts them, other rules check each one.
        fn items(&self) -> Option<Vec<&dyn RuleTarget>> {
            None
        }
    }

    impl<T: RuleTarget> RuleTarget for Vec<T> {
        fn items(&self) -> Option<Vec<&dyn RuleTarget>> {
            Some(self.iter().map(|item| item as &dyn RuleTarget).collect())
        }
    }

    /// Runs a rule on each item of a list field, reporting each message once.
    /// `false` when `value` isn't a list. Messages read as being about one of the items:
    /// `must be at least 1` → `has a value that isn't at least 1`.
    fn each(
        value: &(impl RuleTarget + ?Sized),
        field: &str,
        errors: &mut Errors,
        rule: impl Fn(&dyn RuleTarget, &mut Errors),
    ) -> bool {
        let Some(items) = value.items() else { return false };
        let mut found = Errors::default();
        for item in items {
            rule(item, &mut found);
        }
        for message in found.get(field) {
            let message = match (message.strip_prefix("must be "), message.strip_prefix("is ")) {
                (Some(rest), _) => format!("has a value that isn't {rest}"),
                (_, Some(rest)) => format!("has a value that is {rest}"),
                _ if message == "can't be blank" => "has a blank value".to_owned(),
                _ => format!("has a value that {message}"),
            };
            if !errors.get(field).contains(&message) {
                errors.add(field, message);
            }
        }
        true
    }

    /// Ids and plain-value items have nothing for the rules to inspect.
    impl<T> RuleTarget for crate::Id<T> {}
    impl RuleTarget for bool {}
    impl RuleTarget for crate::Timestamp {}

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

    /// Characters of text; on a list field (`Vec<T>`), the number of values chosen.
    pub fn length(
        value: &(impl RuleTarget + ?Sized),
        min: Option<usize>,
        max: Option<usize>,
        field: &str,
        errors: &mut Errors,
    ) {
        if let Some(items) = value.items() {
            let count = items.len();
            if let Some(min) = min
                && count < min
            {
                errors.add(
                    field,
                    if min == 1 {
                        "needs at least one choice".to_owned()
                    } else {
                        format!("needs at least {min} choices")
                    },
                );
            }
            if let Some(max) = max
                && count > max
            {
                errors.add(field, format!("has too many choices (maximum is {max})"));
            }
            return;
        }
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

    /// On a list field, each value.
    pub fn range(
        value: &(impl RuleTarget + ?Sized),
        min: Option<f64>,
        max: Option<f64>,
        field: &str,
        errors: &mut Errors,
    ) {
        if each(value, field, errors, |item, errors| range(item, min, max, field, errors)) {
            return;
        }
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

    /// On a list field, each value.
    pub fn email(value: &(impl RuleTarget + ?Sized), field: &str, errors: &mut Errors) {
        if each(value, field, errors, |item, errors| email(item, field, errors)) {
            return;
        }
        let Some(text) = value.text() else { return };
        let valid = text.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
        });
        if !valid {
            errors.add(field, "must be an email address");
        }
    }

    /// `#[validate(file(max_size = "5MB", accept = "image/*"))]`: `accept` is a comma-separated
    /// list of types (`image/png`), wildcards (`image/*`) or extensions (`.pdf`).
    pub fn file(
        value: &(impl RuleTarget + ?Sized),
        max_size: Option<u64>,
        accept: Option<&str>,
        field: &str,
        errors: &mut Errors,
    ) {
        let Some(file) = value.attachment() else { return };
        if let Some(max) = max_size
            && file.byte_size() as u64 > max
        {
            errors.add(field, format!("is too big (maximum is {})", human_size(max)));
        }
        if let Some(accept) = accept {
            let ext = file.filename().rsplit_once('.').map(|(_, e)| format!(".{}", e.to_ascii_lowercase()));
            let ok = accept.split(',').map(str::trim).any(|pattern| {
                if let Some(major) = pattern.strip_suffix("/*") {
                    file.content_type().split('/').next() == Some(major)
                } else if pattern.starts_with('.') {
                    ext.as_deref() == Some(&pattern.to_ascii_lowercase())
                } else {
                    file.content_type() == pattern
                }
            });
            if !ok {
                errors.add(field, format!("must be {}", accept.replace(',', " or")));
            }
        }
    }

    fn human_size(bytes: u64) -> String {
        match bytes {
            b if b >= 1024 * 1024 * 1024 && b % (1024 * 1024 * 1024) == 0 => format!("{}GB", b / (1024 * 1024 * 1024)),
            b if b >= 1024 * 1024 => format!("{:.0}MB", b as f64 / (1024.0 * 1024.0)),
            b if b >= 1024 => format!("{:.0}KB", b as f64 / 1024.0),
            b => format!("{b} bytes"),
        }
    }

    /// For `Option` fields that must be filled in, and list fields that need a choice.
    pub fn required(value: &impl Presence, field: &str, errors: &mut Errors) {
        if !value.is_present() {
            errors.add(field, value.missing_message());
        }
    }

    /// A value `#[validate(required)]` applies to: `Option<T>` or `Vec<T>`.
    pub trait Presence {
        fn is_present(&self) -> bool;
        fn missing_message(&self) -> &'static str;
    }

    impl<T> Presence for Option<T> {
        fn is_present(&self) -> bool {
            self.is_some()
        }
        fn missing_message(&self) -> &'static str {
            "can't be blank"
        }
    }

    impl<T> Presence for Vec<T> {
        fn is_present(&self) -> bool {
            !self.is_empty()
        }
        fn missing_message(&self) -> &'static str {
            "needs at least one choice"
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

impl<T: FormInput> FromRequest<Ctx> for Form<T> {
    type Rejection = Error;

    async fn from_request(request: Request, ctx: &Ctx) -> std::result::Result<Self, Self::Rejection> {
        let content_type = request
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        if content_type.starts_with("multipart/form-data") {
            let mut fields = Fields::default();
            read_multipart::<T>(request, ctx, &content_type, &mut fields).await?;
            return Ok(Self::from_fields(fields));
        }
        let bytes = Bytes::from_request(request, ctx).await.map_err(|e| Error::BadRequest(e.to_string()))?;
        let pairs: Vec<(String, String)> = if content_type.starts_with("application/json") {
            json_pairs::<T>(&bytes)?
        } else {
            serde_urlencoded::from_bytes(&bytes).map_err(|e| Error::BadRequest(format!("unreadable form body: {e}")))?
        };
        let mut fields = decode_pairs::<T>(pairs)?;
        for spec in T::SPECS.iter().filter(|s| s.input == "file") {
            if let Some(value) = fields.get(spec.name) {
                let token = file_token(ctx, value).await?;
                fields.set(spec.name, token);
            }
        }
        Ok(Self::from_fields(fields))
    }
}

/// Decoded `name=value` pairs (a urlencoded body or query string) as `T`'s fields:
/// framework names (`_csrf`, `_method`, `lexxy-*`) are dropped, a name `T` doesn't declare
/// is a 400 naming the fields it does, a repeated name keeps every value (for `Vec<T>`
/// fields; single-value fields read the last), and `tags[]` is read as `tags` for a list
/// field. Run `T::from_fields` on the result to parse and validate.
pub(crate) fn decode_pairs<T: FormInput>(pairs: impl IntoIterator<Item = (String, String)>) -> Result<Fields> {
    let mut fields = Fields::default();
    for (name, value) in pairs {
        let name = match name.strip_suffix("[]") {
            Some(base) if is_multiple::<T>(base) => base.to_owned(),
            _ => name,
        };
        if accepts::<T>(&name)? {
            fields.append(name, value);
        }
    }
    Ok(fields)
}

fn is_multiple<T: FormInput>(name: &str) -> bool {
    T::SPECS.iter().any(|s| s.name == name && s.multiple)
}

/// Whether `name` is data for `T`: `_csrf`, `_method` (Kiln's own) and `lexxy-*` (controls
/// inside the rich text editor, like its code-language picker) are skipped; anything else
/// undeclared is refused.
fn accepts<T: FormInput>(name: &str) -> Result<bool> {
    if name.starts_with('_') || name.starts_with("lexxy-") {
        return Ok(false);
    }
    if !T::FIELDS.contains(&name) {
        return Err(Error::BadRequest(format!(
            "unexpected form field `{name}`; `{}` accepts: {}",
            short_name::<T>(),
            T::FIELDS.join(", ")
        )));
    }
    Ok(true)
}

fn is_file<T: FormInput>(name: &str) -> bool {
    T::SPECS.iter().any(|s| s.name == name && s.input == "file")
}

/// A file field's text value: empty, or the signed token of a file the form already had
/// (kept across a re-render or an edit). Anything else is refused, so a form can't claim
/// someone else's file.
async fn file_token(ctx: &Ctx, token: &str) -> Result<String> {
    if token.trim().is_empty() {
        return Ok(String::new());
    }
    match crate::storage::from_form_token(&ctx.db, ctx.signing_key(), token.trim()).await? {
        Some(attachment) => {
            crate::storage::confirm_stored(ctx, &attachment).await?;
            Ok(crate::form::FormField::to_field(&attachment))
        }
        None => Err(Error::BadRequest("a file field held something other than a file this form uploaded".into())),
    }
}

/// Reads a multipart body: text fields as usual, files stored as they arrive. A file input
/// left empty keeps the field's current file (its hidden token), if any.
async fn read_multipart<T: FormInput>(
    request: Request,
    ctx: &Ctx,
    content_type: &str,
    fields: &mut Fields,
) -> Result<()> {
    let boundary =
        multer::parse_boundary(content_type).map_err(|e| Error::BadRequest(format!("unreadable upload: {e}")))?;
    let limit = crate::storage::max_upload_size() + 2 * 1024 * 1024;
    let bytes = axum::body::to_bytes(request.into_body(), limit)
        .await
        .map_err(|_| Error::BadRequest("the upload is larger than MAX_UPLOAD_SIZE".into()))?;
    let stream = futures_util::stream::once(async move { Ok::<_, std::io::Error>(bytes) });
    let mut multipart = multer::Multipart::new(stream, boundary);
    let storage = ctx.storage()?;
    let mut uploaded = std::collections::HashSet::new();
    while let Some(field) =
        multipart.next_field().await.map_err(|e| Error::BadRequest(format!("unreadable upload: {e}")))?
    {
        let name = field.name().unwrap_or_default();
        let name = match name.strip_suffix("[]") {
            Some(base) if is_multiple::<T>(base) => base.to_owned(),
            _ => name.to_owned(),
        };
        if !accepts::<T>(&name)? {
            continue;
        }
        if let Some(filename) = field.file_name().map(str::to_owned) {
            let declared = field.content_type().map(ToString::to_string).unwrap_or_default();
            let data = field.bytes().await.map_err(|e| Error::BadRequest(format!("unreadable upload: {e}")))?;
            if filename.is_empty() && data.is_empty() {
                continue; // no file chosen
            }
            if !is_file::<T>(&name) {
                return Err(Error::BadRequest(format!("`{name}` isn't a file field")));
            }
            let attachment = crate::storage::store(&ctx.db, &storage, &filename, &declared, data).await?;
            fields.set(&name, crate::form::FormField::to_field(&attachment));
            uploaded.insert(name);
        } else {
            let text = field.text().await.map_err(|e| Error::BadRequest(format!("unreadable upload: {e}")))?;
            if is_file::<T>(&name) {
                if !uploaded.contains(&name) {
                    fields.set(&name, file_token(ctx, &text).await?);
                }
            } else {
                fields.append(name, text);
            }
        }
    }
    Ok(())
}

/// A flat JSON object as form fields, so JSON and HTML forms share parsing and rules.
/// `null` means "not sent"; an array of values fills a list (`Vec<T>`) field; other nested
/// values are rejected.
fn json_pairs<T: FormInput>(bytes: &[u8]) -> Result<Vec<(String, String)>> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| Error::BadRequest(format!("unreadable JSON body: {e}")))?;
    let serde_json::Value::Object(object) = value else {
        return Err(Error::BadRequest("the JSON body must be an object of field values".into()));
    };
    let mut pairs = Vec::new();
    let scalar = |name: &str, value: serde_json::Value| match value {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Bool(b) => Ok(b.to_string()),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        _ => Err(Error::BadRequest(format!("field `{name}` must be a string, number or boolean"))),
    };
    for (name, value) in object {
        match value {
            serde_json::Value::Null => {}
            serde_json::Value::Array(items) if is_multiple::<T>(&name) => {
                for item in items {
                    let text = scalar(&name, item).map_err(|_| {
                        Error::BadRequest(format!("field `{name}` must be an array of strings, numbers or booleans"))
                    })?;
                    pairs.push((name.clone(), text));
                }
            }
            value => {
                let text = scalar(&name, value)?;
                pairs.push((name, text));
            }
        }
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
            /// Every value, for list (`Vec<T>`) fields.
            values: &'a [String],
            errors: &'a [String],
            /// File inputs: the file the field holds now.
            file: Option<crate::storage::Attachment>,
            #[serde(flatten)]
            spec: &'a FieldSpec,
        }
        let key = crate::session::current_scope().and_then(|scope| scope.key);
        let mut map = serializer.serialize_map(Some(T::SPECS.len() + 2))?;
        for spec in T::SPECS {
            let raw = self.fields.get(spec.name).unwrap_or("");
            // Passwords are never sent back to the browser, even when the form re-renders.
            // Files go back as a signed token, so the form keeps them without re-uploading.
            let (value, file) = match spec.input {
                "password" => (String::new(), None),
                "file" => {
                    let file = <crate::storage::Attachment as FormField>::from_field(Some(raw)).ok();
                    let token = file.as_ref().zip(key.as_ref()).map(|(f, k)| f.form_token(k)).unwrap_or_default();
                    (token, file)
                }
                _ => (raw.to_owned(), None),
            };
            let values = if spec.input == "password" { &[] } else { self.fields.get_all(spec.name) };
            let field = Field { value: &value, values, errors: self.errors.get(spec.name), file, spec };
            map.serialize_entry(spec.name, &field)?;
        }
        // Field names in order, for `{{ fields(form) }}`.
        map.serialize_entry("_fields", T::FIELDS)?;
        map.serialize_entry("_errors", self.errors.get("_base"))?;
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DbEnum, Form as DeriveForm};

    #[derive(DbEnum, Debug, Clone, Copy, PartialEq)]
    enum Topic {
        Rust,
        WebDev,
    }

    #[derive(DeriveForm, Debug)]
    struct ReviewForm {
        title: String,
        #[validate(length(max = 3), email)]
        reviewers: Vec<String>,
        #[validate(range(min = 1))]
        ids: Vec<i64>,
        #[validate(required)]
        topics: Vec<Topic>,
    }

    fn pairs(raw: &[(&str, &str)]) -> Vec<(String, String)> {
        raw.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn repeated_keys_fill_list_fields() {
        let fields = decode_pairs::<ReviewForm>(pairs(&[
            ("title", "Fix"),
            ("reviewers", "a@x.io"),
            ("reviewers[]", "b@x.io"),
            ("ids", "3"),
            ("ids", ""),
            ("ids", "4"),
            ("topics", "web_dev"),
            ("_csrf", "t"),
        ]))
        .unwrap();
        let form = ReviewForm::from_fields(&fields).unwrap();
        assert_eq!(form.title, "Fix");
        assert_eq!(form.reviewers, ["a@x.io", "b@x.io"]);
        assert_eq!(form.ids, [3, 4]);
        assert_eq!(form.topics, [Topic::WebDev]);
        // And back, to prefill an edit form.
        assert_eq!(form.to_fields().get_all("reviewers"), ["a@x.io", "b@x.io"]);
        assert_eq!(form.to_fields().get_all("topics"), ["web_dev"]);
    }

    #[test]
    fn unknown_fields_are_still_refused() {
        let err = decode_pairs::<ReviewForm>(pairs(&[("admin", "1")])).unwrap_err();
        assert!(err.to_string().contains("unexpected form field `admin`"), "{err}");
        // `[]` is only an alias on list fields.
        let err = decode_pairs::<ReviewForm>(pairs(&[("title[]", "x")])).unwrap_err();
        assert!(err.to_string().contains("`title[]`"), "{err}");
    }

    #[test]
    fn list_rules_count_and_check_each_value() {
        let fields = decode_pairs::<ReviewForm>(pairs(&[("title", "Fix"), ("ids", "0"), ("ids", "x")])).unwrap();
        let errors = ReviewForm::from_fields(&fields).unwrap_err();
        assert_eq!(errors.get("ids"), ["has \"x\", which isn't a whole number"]);
        assert_eq!(errors.get("topics"), ["needs at least one choice"]);

        let fields = decode_pairs::<ReviewForm>(pairs(&[
            ("title", "Fix"),
            ("reviewers", "a@x.io"),
            ("reviewers", "nope"),
            ("reviewers", "c@x.io"),
            ("reviewers", "d@x.io"),
            ("ids", "0"),
            ("ids", "-2"),
            ("topics", "rust"),
        ]))
        .unwrap();
        let errors = ReviewForm::from_fields(&fields).unwrap_err();
        assert_eq!(
            errors.get("reviewers"),
            ["has too many choices (maximum is 3)", "has a value that isn't an email address"]
        );
        assert_eq!(errors.get("ids"), ["has a value that isn't at least 1"]);
        assert!(errors.get("topics").is_empty());
    }

    #[test]
    fn json_arrays_fill_list_fields_only() {
        let body = br#"{"title": "Fix", "reviewers": ["a@x.io", "b@x.io"], "ids": [1, 2], "topics": ["rust"]}"#;
        let fields = decode_pairs::<ReviewForm>(json_pairs::<ReviewForm>(body).unwrap()).unwrap();
        let form = ReviewForm::from_fields(&fields).unwrap();
        assert_eq!((form.reviewers.len(), form.ids, form.topics), (2, vec![1, 2], vec![Topic::Rust]));
        assert!(json_pairs::<ReviewForm>(br#"{"title": ["a"]}"#).is_err());
        assert!(json_pairs::<ReviewForm>(br#"{"ids": [[1]]}"#).is_err());
    }

    #[test]
    fn list_fields_render_as_checkboxes_or_a_multiple_select() {
        let dir = std::env::temp_dir().join(format!("kiln-form-list-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("f.html.jinja"),
            r#"{{ field(form.reviewers, options=people) }}|{{ field(form.topics) }}|{{ field(form.ids, as="select", options=[[1, "One"], {"value": 2, "label": "Two"}]) }}"#,
        )
        .unwrap();
        let root: &'static str = Box::leak(dir.display().to_string().into_boxed_str());
        let fields = decode_pairs::<ReviewForm>(pairs(&[("reviewers", "b@x.io"), ("ids", "2"), ("topics", "rust")]));
        let state = FormState::<ReviewForm> { fields: fields.unwrap(), errors: Errors::default(), _input: PhantomData };
        let context = minijinja::context! { form => state, people => vec![("a@x.io", "Ada"), ("b@x.io", "Bob")] };
        let html = crate::view::render(root, "f.html.jinja", None, &context).unwrap();
        let [reviewers, topics, ids]: [&str; 3] = html.split('|').collect::<Vec<_>>().try_into().unwrap();
        assert!(reviewers.contains("<legend>Reviewers</legend>"), "{reviewers}");
        assert!(reviewers.contains(r#"name="reviewers" value="a@x.io"> Ada</label>"#), "{reviewers}");
        assert!(reviewers.contains(r#"name="reviewers" value="b@x.io" checked> Bob</label>"#), "{reviewers}");
        assert!(topics.contains(r#"value="rust" checked> Rust</label>"#), "{topics}");
        assert!(topics.contains(r#"value="web_dev"> Web dev</label>"#), "{topics}");
        assert!(ids.contains("<select id=\"ids\" name=\"ids\" multiple>"), "{ids}");
        assert!(ids.contains(r#"<option value="1">One</option>"#), "{ids}");
        assert!(ids.contains(r#"<option value="2" selected>Two</option>"#), "{ids}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! File storage: uploads, attachments, and where their bytes live.
//!
//! A record field `pub cover: Option<Attachment>` holds a stored file. A form with an
//! `Attachment` field takes uploads (`form_start(.., multipart=true)`): files are stored as
//! the request arrives, and the field reads as the stored `Attachment`. Templates show
//! `{{ post.cover.url }}`, `.filename`, `.content_type`, `.byte_size` and `.is_image`.
//!
//! Where the bytes go is `STORAGE_URL`:
//! - `file://storage` (the default): `storage/` in the app directory;
//! - `s3://bucket/prefix`: S3, or R2 / MinIO with `AWS_ENDPOINT`, using `AWS_ACCESS_KEY_ID`,
//!   `AWS_SECRET_ACCESS_KEY` and `AWS_REGION` (environment or `kiln secrets`);
//! - `gs://bucket/prefix`: Google Cloud Storage, using `GOOGLE_SERVICE_ACCOUNT`;
//! - `memory://`: gone when the process exits. Tests get a fresh one each.
//!
//! Files are served at `/kiln/files/<signed id>/<filename>`: anyone with the link can
//! download it, and links can't be guessed. Cloud files redirect to a short-lived signed
//! URL; local files are streamed. Types a browser could run (HTML, SVG) always download.

use std::{
    path::PathBuf,
    sync::{Arc, OnceLock, RwLock},
    time::Duration,
};

use axum::{
    body::{Body, Bytes},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use object_store::{ObjectStore, ObjectStoreExt, PutPayload, path::Path as StorePath, signer::Signer};
use serde::{Deserialize, Serialize, Serializer, ser::SerializeStruct};

use crate::{Ctx, Db, Error, Result, SecretKey, Timestamp, Value, config::Config};

/// Where files are kept. One per app; read it with `ctx.storage()`.
#[derive(Clone)]
pub struct Storage {
    store: Arc<dyn ObjectStore>,
    /// Cloud stores hand out signed URLs, so downloads skip the app.
    signer: Option<Arc<dyn Signer>>,
    describe: String,
    /// The directory of a `file://` store.
    dir: Option<PathBuf>,
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Storage({})", self.describe)
    }
}

impl Storage {
    /// A store from a `STORAGE_URL`; relative `file://` paths are under `root`.
    pub fn from_url(url: &str, root: &std::path::Path) -> Result<Self> {
        let bad = |e: object_store::Error| Error::Config(format!("STORAGE_URL `{url}`: {e}"));
        let (scheme, rest) = url.split_once("://").unwrap_or(("file", url));
        match scheme {
            "file" => {
                let dir = if rest.starts_with('/') { PathBuf::from(rest) } else { root.join(rest) };
                std::fs::create_dir_all(&dir)?;
                let store = object_store::local::LocalFileSystem::new_with_prefix(&dir).map_err(bad)?;
                Ok(Self {
                    store: Arc::new(store),
                    signer: None,
                    describe: format!("file://{}", dir.display()),
                    dir: Some(dir),
                })
            }
            "memory" => Ok(Self::memory()),
            "s3" => {
                let mut builder = object_store::aws::AmazonS3Builder::from_env().with_url(url);
                for (key, name) in [
                    ("aws_access_key_id", "AWS_ACCESS_KEY_ID"),
                    ("aws_secret_access_key", "AWS_SECRET_ACCESS_KEY"),
                    ("aws_session_token", "AWS_SESSION_TOKEN"),
                    ("aws_region", "AWS_REGION"),
                    ("aws_endpoint", "AWS_ENDPOINT"),
                ] {
                    if let Some(value) = crate::config::value(name) {
                        builder = builder.with_config(key.parse().map_err(bad)?, value);
                    }
                }
                let store = Arc::new(builder.build().map_err(bad)?);
                Ok(Self { store: store.clone(), signer: Some(store), describe: url.to_owned(), dir: None })
            }
            "gs" => {
                let mut builder = object_store::gcp::GoogleCloudStorageBuilder::from_env().with_url(url);
                if let Some(account) = crate::config::value("GOOGLE_SERVICE_ACCOUNT") {
                    builder = builder.with_config("google_service_account".parse().map_err(bad)?, account);
                }
                let store = Arc::new(builder.build().map_err(bad)?);
                Ok(Self { store: store.clone(), signer: Some(store), describe: url.to_owned(), dir: None })
            }
            other => Err(Error::Config(format!(
                "STORAGE_URL `{url}`: `{other}://` isn't a store Kiln knows; use file://, s3://, gs:// or memory://"
            ))),
        }
    }

    /// An in-memory store (tests).
    pub fn memory() -> Self {
        Self {
            store: Arc::new(object_store::memory::InMemory::new()),
            signer: None,
            describe: "memory://".into(),
            dir: None,
        }
    }

    async fn put(&self, key: &str, bytes: Bytes) -> Result<()> {
        self.store.put(&StorePath::from(key), PutPayload::from(bytes)).await.map_err(storage_error)?;
        Ok(())
    }

    /// A stored file's bytes.
    pub async fn read(&self, attachment: &Attachment) -> Result<Bytes> {
        self.get(&attachment.key).await
    }

    async fn get(&self, key: &str) -> Result<Bytes> {
        let result = self.store.get(&StorePath::from(key)).await.map_err(storage_error)?;
        result.bytes().await.map_err(storage_error)
    }

    async fn delete(&self, key: &str) -> Result<()> {
        match self.store.delete(&StorePath::from(key)).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(storage_error(e)),
        }
    }

    /// The directory a `file://` store keeps files in; `None` for cloud and memory stores.
    pub fn local_dir(&self) -> Option<&std::path::Path> {
        self.dir.as_deref()
    }

    /// Deletes everything under `prefix` (an image's variants).
    async fn delete_prefix(&self, prefix: &str) -> Result<()> {
        use futures_util::StreamExt;
        let prefix = StorePath::from(prefix);
        let mut listing = self.store.list(Some(&prefix));
        let mut paths = vec![];
        while let Some(meta) = listing.next().await {
            paths.push(meta.map_err(storage_error)?.location);
        }
        for path in paths {
            match self.store.delete(&path).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(e) => return Err(storage_error(e)),
            }
        }
        Ok(())
    }

    /// A presigned `PUT` for a direct upload, when the store can take one.
    async fn signed_upload_url(&self, key: &str) -> Option<String> {
        let signer = self.signer.as_ref()?;
        let url = signer.signed_url(axum::http::Method::PUT, &StorePath::from(key), PUT_TTL).await.ok()?;
        Some(url.to_string())
    }

    async fn signed_url(&self, key: &str) -> Option<String> {
        let signer = self.signer.as_ref()?;
        let url = signer
            .signed_url(axum::http::Method::GET, &StorePath::from(key), Duration::from_secs(300))
            .await
            .map_err(|e| tracing::warn!(target: "kiln::storage", error = %e, "signing a download URL failed"))
            .ok()?;
        Some(url.to_string())
    }
}

fn storage_error(e: object_store::Error) -> Error {
    match e {
        object_store::Error::NotFound { .. } => Error::NotFound,
        e => Error::Io(std::io::Error::other(e.to_string())),
    }
}

#[derive(crate::Config)]
struct StorageSettings {
    #[config(default = "file://storage")]
    storage_url: String,
    /// The largest upload accepted, e.g. `25MB`.
    #[config(default = "25MB")]
    max_upload_size: String,
}

fn settings() -> Result<&'static StorageSettings> {
    static SETTINGS: OnceLock<StorageSettings> = OnceLock::new();
    if let Some(settings) = SETTINGS.get() {
        return Ok(settings);
    }
    let loaded = StorageSettings::load()?;
    Ok(SETTINGS.get_or_init(|| loaded))
}

/// The app's store from `STORAGE_URL`, unless the app provided one (`app.storage(..)`).
pub(crate) fn default_storage() -> Result<Storage> {
    static DEFAULT: OnceLock<Storage> = OnceLock::new();
    if let Some(storage) = DEFAULT.get() {
        return Ok(storage.clone());
    }
    let root = crate::config::root().unwrap_or_else(|| PathBuf::from("."));
    let storage = Storage::from_url(&settings()?.storage_url, &root)?;
    tracing::info!(target: "kiln::storage", store = ?storage, "files");
    Ok(DEFAULT.get_or_init(|| storage).clone())
}

/// The largest upload, in bytes (`MAX_UPLOAD_SIZE`, default 25MB).
pub(crate) fn max_upload_size() -> usize {
    settings().ok().and_then(|s| parse_size(&s.max_upload_size)).unwrap_or(25 * 1024 * 1024) as usize
}

/// `5MB`, `512KB`, `1GB`, `2000` (bytes).
pub fn parse_size(text: &str) -> Option<u64> {
    let text = text.trim().to_ascii_uppercase();
    let (number, unit) = match text.find(|c: char| !c.is_ascii_digit() && c != '.') {
        Some(at) => text.split_at(at),
        None => (text.as_str(), "B"),
    };
    let number: f64 = number.parse().ok()?;
    let scale: f64 = match unit.trim() {
        "B" | "" => 1.0,
        "KB" | "K" => 1024.0,
        "MB" | "M" => 1024.0 * 1024.0,
        "GB" | "G" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((number * scale) as u64)
}

impl Ctx {
    /// Where the app's files are kept.
    pub fn storage(&self) -> Result<Storage> {
        match self.try_get::<Storage>() {
            Some(storage) => Ok(storage.clone()),
            None => default_storage(),
        }
    }

    /// Stores `bytes` as a new file, e.g. one a job generated: `post.cover = Some(ctx.attach(..).await?)`.
    pub async fn attach(&self, filename: &str, content_type: &str, bytes: impl Into<Bytes>) -> Result<Attachment> {
        store(&self.db, &self.storage()?, filename, content_type, bytes.into()).await
    }
}

/// Stores a file and records its blob.
pub(crate) async fn store(
    db: &Db,
    storage: &Storage,
    filename: &str,
    declared: &str,
    bytes: Bytes,
) -> Result<Attachment> {
    let filename = clean_filename(filename);
    let content_type = real_type(&bytes, declared).to_owned();
    let key = new_key();
    let checksum = {
        use base64::{Engine, engine::general_purpose::STANDARD};
        use sha2::{Digest, Sha256};
        STANDARD.encode(Sha256::digest(&bytes))
    };
    let byte_size = bytes.len() as i64;
    storage.put(&key, bytes).await?;
    insert_blob(db, key, filename, content_type, byte_size, checksum).await
}

async fn insert_blob(
    db: &Db,
    key: String,
    filename: String,
    content_type: String,
    byte_size: i64,
    checksum: String,
) -> Result<Attachment> {
    let rows = db
        .fetch_all(
            "INSERT INTO \"kiln_blobs\" (\"key\", \"filename\", \"content_type\", \"byte_size\", \"checksum\", \"created_at\") \
             VALUES (?, ?, ?, ?, ?, ?) RETURNING \"id\"",
            vec![
                Value::Text(key.clone()),
                Value::Text(filename.clone()),
                Value::Text(content_type.clone()),
                Value::Integer(byte_size),
                Value::Text(checksum),
                Value::Integer(Timestamp::now().as_jiff().as_microsecond()),
            ],
        )
        .await?;
    let id = rows.first().map(|r| sqlx::Row::get::<i64, _>(r, 0)).unwrap_or_default();
    Ok(Attachment { blob_id: id, key, filename, content_type, byte_size })
}

/// Before a form keeps a file: it must really be in the store (a direct upload the browser
/// never finished isn't) and within `MAX_UPLOAD_SIZE`.
pub(crate) async fn confirm_stored(ctx: &Ctx, attachment: &Attachment) -> Result<()> {
    let storage = ctx.storage()?;
    let meta = match storage.store.head(&StorePath::from(attachment.key.as_str())).await {
        Ok(meta) => meta,
        Err(object_store::Error::NotFound { .. }) => {
            return Err(Error::BadRequest(format!(
                "`{}` didn't finish uploading; choose it again",
                attachment.filename
            )));
        }
        Err(e) => return Err(storage_error(e)),
    };
    if meta.size as usize > max_upload_size() {
        return Err(Error::BadRequest(format!("`{}` is larger than MAX_UPLOAD_SIZE", attachment.filename)));
    }
    Ok(())
}

/// `ab/cd/abcd…`: 26 random characters, fanned out so no directory gets huge.
fn new_key() -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    let key: String = (0..26).map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char).collect();
    format!("{}/{}/{key}", &key[..2], &key[2..4])
}

/// The last path segment, without control characters or quotes; `file` if nothing's left.
fn clean_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let clean: String = base.chars().filter(|c| !c.is_control() && !matches!(c, '"' | '<' | '>')).take(200).collect();
    let clean = clean.trim().to_owned();
    if clean.is_empty() || clean == "." || clean == ".." { "file".into() } else { clean }
}

/// The real type of common binary formats, whatever the browser claimed.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    let starts = |magic: &[u8]| bytes.starts_with(magic);
    if starts(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if starts(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && starts(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if starts(b"%PDF-") {
        Some("application/pdf")
    } else {
        None
    }
}

/// The type to keep for an upload: what the bytes are, when Kiln can tell, else what the
/// browser said. A claim Kiln could check but the bytes don't back up (a "PNG" that isn't
/// one) is dropped, so `accept = "image/*"` rejects it and nothing tries to resize it.
fn real_type<'a>(bytes: &[u8], declared: &'a str) -> &'a str {
    match sniff(bytes) {
        // The magic bytes alone are easy to fake: an image must have a readable header too.
        Some(image) if image.starts_with("image/") && !has_dimensions(bytes) => "application/octet-stream",
        Some(real) => real,
        None if declared.is_empty() || SNIFFED.contains(&declared) => "application/octet-stream",
        None => declared,
    }
}

/// Whether an image's header says how big it is (it's read, the pixels aren't).
fn has_dimensions(bytes: &[u8]) -> bool {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        .is_some()
}

/// The types [`sniff`] recognizes.
const SNIFFED: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp", "application/pdf"];

/// Types shown in the browser; everything else downloads. HTML and SVG never render
/// inline, since an uploaded page could run script on the app's origin.
fn inline(content_type: &str) -> bool {
    matches!(
        content_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/avif" | "application/pdf" | "text/plain"
    ) || content_type.starts_with("video/")
        || content_type.starts_with("audio/")
}

// ---------------------------------------------------------------------------------------
// Attachments

/// A stored file, kept on a record: `pub cover: Option<Attachment>`. The column holds the
/// file's details as JSON, so pages show it without another query.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
#[serde(rename = "Attachment")]
pub struct AttachmentData {
    blob: i64,
    key: String,
    filename: String,
    content_type: String,
    byte_size: i64,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Attachment {
    blob_id: i64,
    key: String,
    filename: String,
    content_type: String,
    byte_size: i64,
}

impl Attachment {
    pub fn filename(&self) -> &str {
        &self.filename
    }

    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    pub fn byte_size(&self) -> i64 {
        self.byte_size
    }

    pub fn is_image(&self) -> bool {
        self.content_type.starts_with("image/") && self.content_type != "image/svg+xml"
    }

    /// `/kiln/files/<signed id>/<filename>`.
    pub fn url(&self) -> String {
        match signing_key() {
            Some(key) => self.url_signed(&key),
            None => String::new(),
        }
    }

    fn url_signed(&self, key: &SecretKey) -> String {
        let token = key.sign_str(&format!("file:{}", self.blob_id));
        let name = percent_encoding::utf8_percent_encode(&self.filename, FILENAME);
        format!("{FILES_PATH}{token}/{name}")
    }

    fn to_json(&self) -> String {
        serde_json::to_string(&AttachmentData {
            blob: self.blob_id,
            key: self.key.clone(),
            filename: self.filename.clone(),
            content_type: self.content_type.clone(),
            byte_size: self.byte_size,
        })
        .expect("attachments serialize")
    }

    fn from_json(text: &str) -> Option<Self> {
        let d: AttachmentData = serde_json::from_str(text).ok()?;
        Some(Self {
            blob_id: d.blob,
            key: d.key,
            filename: d.filename,
            content_type: d.content_type,
            byte_size: d.byte_size,
        })
    }

    /// A resized copy of an image, made the first time it's requested and kept:
    /// `cover.variant(Variant::fit(400, 400))` (a URL). Non-images serve the original.
    pub fn variant(&self, variant: Variant) -> String {
        match signing_key() {
            Some(key) => variant_url(&key, self.blob_id, &self.filename, variant),
            None => String::new(),
        }
    }

    /// The token a form carries to keep this file across a re-render or an edit.
    pub(crate) fn form_token(&self, key: &SecretKey) -> String {
        key.sign_str(&format!("upload:{}", self.blob_id))
    }
}

/// Templates see `url`, `filename`, `content_type`, `byte_size` and `is_image`.
impl Serialize for Attachment {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("Attachment", 5)?;
        s.serialize_field("url", &self.url())?;
        s.serialize_field("filename", &self.filename)?;
        s.serialize_field("content_type", &self.content_type)?;
        s.serialize_field("byte_size", &self.byte_size)?;
        s.serialize_field("is_image", &self.is_image())?;
        s.end()
    }
}

impl crate::types::ToValue for Attachment {
    fn to_value(&self) -> Value {
        Value::Text(self.to_json())
    }
}

impl crate::types::FromColumn for Attachment {
    fn from_column(row: &crate::Row, name: &str) -> std::result::Result<Self, sqlx::Error> {
        let text: String = sqlx::Row::try_get(row, name)?;
        Self::from_json(&text).ok_or_else(|| sqlx::Error::ColumnDecode {
            index: name.to_owned(),
            source: format!("`{name}` isn't an attachment").into(),
        })
    }
}

impl crate::types::SqlType for Attachment {
    const SQL: &'static str = "TEXT";
    const ATTACHMENT: bool = true;
}

/// The form side: an `<input type="file">`. The submitted value is checked and stored by
/// the form extractor before parsing, so only a real upload (or a signed token for a file
/// the form already had) ever reaches this.
impl crate::form::FormField for Attachment {
    const INPUT: &'static str = "file";

    fn from_field(raw: Option<&str>) -> std::result::Result<Self, String> {
        let raw = raw.filter(|s| !s.is_empty()).ok_or_else(|| "can't be blank".to_owned())?;
        Self::from_json(raw).ok_or_else(|| "isn't a file".to_owned())
    }

    fn to_field(&self) -> String {
        self.to_json()
    }
}

impl crate::form::rules::RuleTarget for Attachment {
    fn attachment(&self) -> Option<&Attachment> {
        Some(self)
    }
}

impl crate::form::rules::RuleTarget for Option<Attachment> {
    fn attachment(&self) -> Option<&Attachment> {
        self.as_ref()
    }
}

// ---------------------------------------------------------------------------------------
// Variants

/// A size to show an image at. Sizes are in pixels; images are never enlarged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Variant {
    pub width: u32,
    pub height: u32,
    /// Fill the box exactly, cropping the overflow (`true`), or fit inside it (`false`).
    pub crop: bool,
}

impl Variant {
    /// Fits inside `width` × `height`, keeping the aspect ratio. `0` leaves a side free.
    pub fn fit(width: u32, height: u32) -> Self {
        Self { width, height, crop: false }
    }

    /// Exactly `width` × `height`, cropped from the center.
    pub fn fill(width: u32, height: u32) -> Self {
        Self { width, height, crop: true }
    }

    fn spec(&self) -> String {
        format!("{}x{}{}", self.width, self.height, if self.crop { "c" } else { "" })
    }

    fn parse(spec: &str) -> Option<Self> {
        let (crop, spec) = match spec.strip_suffix('c') {
            Some(rest) => (true, rest),
            None => (false, spec),
        };
        let (w, h) = spec.split_once('x')?;
        let variant = Self { width: w.parse().ok()?, height: h.parse().ok()?, crop };
        (variant.width <= MAX_SIDE && variant.height <= MAX_SIDE && (variant.width > 0 || variant.height > 0))
            .then_some(variant)
    }
}

/// The largest variant side, so links can't ask for huge images.
const MAX_SIDE: u32 = 4096;

pub(crate) const VARIANTS_PATH: &str = "/kiln/variants/";

fn variant_url(key: &SecretKey, blob: i64, filename: &str, variant: Variant) -> String {
    let token = key.sign_str(&format!("variant:{blob}:{}", variant.spec()));
    let name = percent_encoding::utf8_percent_encode(filename, FILENAME);
    format!("{VARIANTS_PATH}{token}/{name}")
}

/// The `variant` template filter's side of [`Attachment::variant`], from the file's `url`.
pub(crate) fn variant_for_url(url: &str, variant: Variant) -> Option<String> {
    let key = signing_key()?;
    let (token, name) = url.strip_prefix(FILES_PATH)?.split_once('/')?;
    let blob: i64 = key.verify_str(token)?.strip_prefix("file:")?.parse().ok()?;
    let token = key.sign_str(&format!("variant:{blob}:{}", variant.spec()));
    Some(format!("{VARIANTS_PATH}{token}/{name}"))
}

/// `GET /kiln/variants/<token>/<filename>`.
pub(crate) async fn serve_variant(ctx: &Ctx, key: &SecretKey, path: &str) -> Option<Response> {
    let token = path.strip_prefix(VARIANTS_PATH)?.split('/').next().unwrap_or_default();
    let parsed = key.verify_str(token).and_then(|v| {
        let (blob, spec) = v.strip_prefix("variant:")?.split_once(':')?;
        Some((blob.parse::<i64>().ok()?, Variant::parse(spec)?))
    });
    let Some((id, variant)) = parsed else { return Some(StatusCode::NOT_FOUND.into_response()) };
    Some(match make_variant(ctx, key, id, variant).await {
        Ok(response) => response,
        Err(Error::NotFound) => StatusCode::NOT_FOUND.into_response(),
        // The bytes aren't an image Kiln can read (uploaded straight to the store, so never
        // checked): the original, like any other file Kiln doesn't resize.
        Err(Error::BadRequest(reason)) => {
            tracing::warn!(target: "kiln::storage", blob = id, "{reason}; serving the original");
            match blob(&ctx.db, id).await {
                Ok(Some(original)) => redirect(&original.url_signed(key)),
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }
        Err(e) => {
            tracing::error!(target: "kiln::storage", error = %e, blob = id, "making an image variant failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "making this image variant failed; the log has the error")
                .into_response()
        }
    })
}

async fn make_variant(ctx: &Ctx, key: &SecretKey, id: i64, variant: Variant) -> Result<Response> {
    let original = blob(&ctx.db, id).await?.ok_or(Error::NotFound)?;
    let storage = ctx.storage()?;
    let format = match original.content_type.as_str() {
        "image/png" | "image/gif" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/webp" => image::ImageFormat::WebP,
        // Not something Kiln resizes: the original instead.
        _ => return Ok(redirect(&original.url_signed(key))),
    };
    let cached = format!("variants/{}/{}", original.key, variant.spec());
    let bytes = match storage.get(&cached).await {
        Ok(bytes) => bytes,
        Err(Error::NotFound) => {
            let source = storage.get(&original.key).await?;
            let resized = tokio::task::spawn_blocking(move || resize(&source, variant, format))
                .await
                .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))??;
            let resized = Bytes::from(resized);
            storage.put(&cached, resized.clone()).await?;
            resized
        }
        Err(e) => return Err(e),
    };
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(format.to_mime_type()));
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=31536000, immutable"));
    Ok(response)
}

fn redirect(to: &str) -> Response {
    let mut response = StatusCode::FOUND.into_response();
    if let Ok(location) = HeaderValue::from_str(to) {
        response.headers_mut().insert(header::LOCATION, location);
    }
    response
}

/// Decodes, resizes (never enlarging) and re-encodes an image.
fn resize(source: &[u8], variant: Variant, format: image::ImageFormat) -> Result<Vec<u8>> {
    use image::imageops::FilterType;
    let bad = |e: image::ImageError| Error::BadRequest(format!("that image can't be resized: {e}"));
    let img = image::load_from_memory(source).map_err(bad)?;
    let side = |want: u32, have: u32| if want == 0 { have } else { want.min(have) };
    let (w, h) = (side(variant.width, img.width()), side(variant.height, img.height()));
    let out = if variant.crop && variant.width > 0 && variant.height > 0 {
        img.resize_to_fill(w, h, FilterType::Lanczos3)
    } else {
        img.resize(w, h, FilterType::Lanczos3)
    };
    let mut bytes = Vec::new();
    match format {
        image::ImageFormat::Jpeg => {
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 82);
            out.to_rgb8().write_with_encoder(encoder).map_err(bad)?;
        }
        format => out.write_to(&mut std::io::Cursor::new(&mut bytes), format).map_err(bad)?,
    }
    Ok(bytes)
}

// ---------------------------------------------------------------------------------------
// Signing

/// Signs file links outside requests (mail rendered in jobs). Set when an app is built.
static FALLBACK_KEY: RwLock<Option<SecretKey>> = RwLock::new(None);

pub(crate) fn set_signing_key(key: &SecretKey) {
    *FALLBACK_KEY.write().unwrap_or_else(|e| e.into_inner()) = Some(key.clone());
}

fn signing_key() -> Option<SecretKey> {
    crate::session::current_scope()
        .and_then(|scope| scope.key)
        .or_else(|| FALLBACK_KEY.read().unwrap_or_else(|e| e.into_inner()).clone())
}

/// `url` if it's a `/kiln/files/` link this app signed (stored rich text names files this
/// way), else `None`.
pub(crate) fn verified_file_link(url: &str) -> Option<String> {
    let token = url.strip_prefix(FILES_PATH)?.split('/').next()?;
    let key = signing_key()?;
    key.verify_str(token)?.strip_prefix("file:")?.parse::<i64>().ok()?;
    Some(url.to_owned())
}

/// The blob id behind a signed `/kiln/files/` link.
fn file_link_blob(key: &SecretKey, url: &str) -> Option<i64> {
    let token = url.strip_prefix(FILES_PATH)?.split('/').next()?;
    key.verify_str(token)?.strip_prefix("file:")?.parse().ok()
}

/// A form's file token back to the blob it names.
pub(crate) async fn from_form_token(db: &Db, key: &SecretKey, token: &str) -> Result<Option<Attachment>> {
    let Some(id) = key.verify_str(token).and_then(|v| v.strip_prefix("upload:")?.parse::<i64>().ok()) else {
        return Ok(None);
    };
    blob(db, id).await
}

async fn blob(db: &Db, id: i64) -> Result<Option<Attachment>> {
    let rows = db
        .fetch_all(
            "SELECT \"id\", \"key\", \"filename\", \"content_type\", \"byte_size\" FROM \"kiln_blobs\" WHERE \"id\" = ?",
            vec![Value::Integer(id)],
        )
        .await?;
    Ok(rows.first().map(|r| {
        use sqlx::Row;
        Attachment { blob_id: r.get(0), key: r.get(1), filename: r.get(2), content_type: r.get(3), byte_size: r.get(4) }
    }))
}

// ---------------------------------------------------------------------------------------
// Serving

pub(crate) const FILES_PATH: &str = "/kiln/files/";

/// Direct uploads: `POST` the file's details here, then `PUT` the bytes where it says.
pub(crate) const DIRECT_UPLOADS_PATH: &str = "/kiln/direct_uploads";

/// How long a direct upload's `PUT` URL works.
const PUT_TTL: Duration = Duration::from_secs(600);

fn json_response(status: StatusCode, value: serde_json::Value) -> Response {
    let mut response = (status, value.to_string()).into_response();
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

/// `POST /kiln/direct_uploads` with `{"blob": {"filename", "content_type", "byte_size"}}` (Active
/// Storage's shape, which Lexxy sends): records the blob and answers where to `PUT` the
/// bytes. Cloud stores get a presigned URL, so the file never passes through the app; local
/// disk gets Kiln's own `PUT /kiln/direct_uploads/<token>`.
pub(crate) async fn create_direct_upload(ctx: &Ctx, csrf_ok: bool, body: &[u8]) -> Response {
    if !csrf_ok {
        return json_response(
            StatusCode::FORBIDDEN,
            serde_json::json!({ "error": "missing or stale CSRF token; reload the page" }),
        );
    }
    let request: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let blob = request.get("blob").unwrap_or(&request);
    let filename = clean_filename(blob["filename"].as_str().unwrap_or("file"));
    let declared = blob["content_type"].as_str().filter(|t| !t.is_empty()).unwrap_or("application/octet-stream");
    let Some(byte_size) = blob["byte_size"].as_u64() else {
        return json_response(StatusCode::BAD_REQUEST, serde_json::json!({ "error": "send the file's byte_size" }));
    };
    if byte_size as usize > max_upload_size() {
        return json_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            serde_json::json!({ "error": format!("`{filename}` is larger than MAX_UPLOAD_SIZE ({} bytes)", max_upload_size()) }),
        );
    }
    let checksum = blob["checksum"].as_str().unwrap_or_default().to_owned();
    let result: Result<(Attachment, String)> = async {
        let storage = ctx.storage()?;
        let file = insert_blob(&ctx.db, new_key(), filename, declared.to_owned(), byte_size as i64, checksum).await?;
        let put = match storage.signed_upload_url(&file.key).await {
            Some(url) => url,
            None => {
                let until = (Timestamp::now() + jiff::SignedDuration::try_from(PUT_TTL).unwrap_or_default())
                    .as_jiff()
                    .as_second();
                format!(
                    "{DIRECT_UPLOADS_PATH}/{}",
                    ctx.signing_key().sign_str(&format!("put:{}:{until}", file.blob_id))
                )
            }
        };
        Ok((file, put))
    }
    .await;
    match result {
        Ok((file, put)) => {
            let key = ctx.signing_key();
            json_response(
                StatusCode::OK,
                serde_json::json!({
                    "signed_id": key.sign_str(&format!("file:{}", file.blob_id)),
                    "attachable_sgid": key.sign_str(&format!("file:{}", file.blob_id)),
                    // What a form field submits to keep the file.
                    "upload_token": file.form_token(key),
                    "filename": file.filename,
                    "content_type": file.content_type,
                    "byte_size": file.byte_size,
                    "previewable": false,
                    "url": file.url_signed(key),
                    "direct_upload": { "url": put, "headers": { "Content-Type": file.content_type } },
                }),
            )
        }
        Err(e) => {
            tracing::error!(target: "kiln::storage", error = %e, "starting a direct upload failed");
            json_response(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e.to_string() }))
        }
    }
}

/// `PUT /kiln/direct_uploads/<token>`: the bytes of a direct upload, for stores that can't
/// take them from the browser themselves (local disk, memory).
pub(crate) async fn receive_direct_upload(ctx: &Ctx, path: &str, body: axum::body::Body) -> Response {
    let token = path.strip_prefix(DIRECT_UPLOADS_PATH).unwrap_or_default().trim_start_matches('/');
    let parsed = ctx.signing_key().verify_str(token).and_then(|v| {
        let (id, until) = v.strip_prefix("put:")?.split_once(':')?;
        Some((id.parse::<i64>().ok()?, until.parse::<i64>().ok()?))
    });
    let Some((id, _)) = parsed.filter(|(_, until)| *until >= Timestamp::now().as_jiff().as_second()) else {
        return json_response(
            StatusCode::FORBIDDEN,
            serde_json::json!({ "error": "that upload link is invalid or has expired" }),
        );
    };
    let bytes = match axum::body::to_bytes(body, max_upload_size()).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return json_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                serde_json::json!({ "error": "larger than MAX_UPLOAD_SIZE" }),
            );
        }
    };
    let result: Result<()> = async {
        let file = blob(&ctx.db, id).await?.ok_or(Error::NotFound)?;
        if bytes.len() as i64 != file.byte_size {
            return Err(Error::BadRequest(format!("expected {} bytes, got {}", file.byte_size, bytes.len())));
        }
        // The bytes are here, so the real type can be read from them.
        let real = real_type(&bytes, &file.content_type);
        if real != file.content_type {
            ctx.db
                .fetch_all(
                    "UPDATE \"kiln_blobs\" SET \"content_type\" = ? WHERE \"id\" = ? RETURNING \"id\"",
                    vec![Value::Text(real.to_owned()), Value::Integer(id)],
                )
                .await?;
        }
        ctx.storage()?.put(&file.key, bytes).await
    }
    .await;
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => json_response(StatusCode::BAD_REQUEST, serde_json::json!({ "error": e.to_string() })),
    }
}

/// Filenames in links and headers keep `.`, `-`, `_` and `~` readable.
const FILENAME: &percent_encoding::AsciiSet =
    &percent_encoding::NON_ALPHANUMERIC.remove(b'.').remove(b'-').remove(b'_').remove(b'~');

/// `GET /kiln/files/<token>/<filename>`.
pub(crate) async fn serve(ctx: &Ctx, key: &SecretKey, path: &str) -> Option<Response> {
    let rest = path.strip_prefix(FILES_PATH)?;
    let token = rest.split('/').next().unwrap_or_default();
    let Some(id) = key.verify_str(token).and_then(|v| v.strip_prefix("file:")?.parse::<i64>().ok()) else {
        return Some(StatusCode::NOT_FOUND.into_response());
    };
    Some(match download(ctx, id).await {
        Ok(response) => response,
        Err(Error::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(target: "kiln::storage", error = %e, blob = id, "serving a file failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    })
}

async fn download(ctx: &Ctx, id: i64) -> Result<Response> {
    let attachment = blob(&ctx.db, id).await?.ok_or(Error::NotFound)?;
    let storage = ctx.storage()?;
    if let Some(url) = storage.signed_url(&attachment.key).await {
        let mut response = StatusCode::FOUND.into_response();
        response
            .headers_mut()
            .insert(header::LOCATION, HeaderValue::from_str(&url).map_err(|e| Error::BadRequest(e.to_string()))?);
        response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=240"));
        return Ok(response);
    }
    let bytes = storage.get(&attachment.key).await?;
    let disposition = if inline(&attachment.content_type) { "inline" } else { "attachment" };
    let encoded = percent_encoding::utf8_percent_encode(&attachment.filename, FILENAME);
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    let content_type =
        if disposition == "inline" { attachment.content_type.as_str() } else { "application/octet-stream" };
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    if let Ok(value) = HeaderValue::from_str(&format!("{disposition}; filename*=UTF-8''{encoded}")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; sandbox"));
    // Blobs never change; the link is the file.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=31536000, immutable"));
    Ok(response)
}

// ---------------------------------------------------------------------------------------
// The blob table and clean-up

impl Db {
    pub(crate) async fn with_blob_table(self) -> Result<Self> {
        self.execute(
            "CREATE TABLE IF NOT EXISTS \"kiln_blobs\" (\"id\" INTEGER PRIMARY KEY AUTOINCREMENT, \"key\" TEXT NOT NULL, \
             \"filename\" TEXT NOT NULL, \"content_type\" TEXT NOT NULL, \"byte_size\" INTEGER NOT NULL, \
             \"checksum\" TEXT NOT NULL, \"created_at\" INTEGER NOT NULL)",
        )
        .await?;
        Ok(self)
    }
}

/// Deletes files no record holds that are older than `older_than` (uploads from forms that
/// were never saved, files a record replaced). Returns how many went.
pub async fn purge_unattached(ctx: &Ctx, older_than: jiff::SignedDuration) -> Result<usize> {
    use std::collections::HashSet;
    let mut held: HashSet<i64> = HashSet::new();
    for record in crate::migrate::records() {
        let schema = (record.schema)();
        for column in schema.columns.iter().filter(|c| c.rich_text) {
            let rows = ctx
                .db
                .fetch_all(
                    &format!(
                        "SELECT \"{c}\" FROM \"{t}\" WHERE \"{c}\" LIKE '%kiln-attachment%'",
                        c = column.name,
                        t = schema.table
                    ),
                    vec![],
                )
                .await?;
            for text in rows.iter().filter_map(|r| sqlx::Row::try_get::<String, _>(r, 0).ok()) {
                held.extend(text.split("url=\"").skip(1).filter_map(|rest| {
                    let url = rest.split('"').next()?;
                    file_link_blob(ctx.signing_key(), url)
                }));
            }
        }
        for column in schema.columns.iter().filter(|c| c.attachment) {
            let rows = ctx
                .db
                .fetch_all(
                    &format!(
                        "SELECT \"{}\" FROM \"{}\" WHERE \"{}\" IS NOT NULL",
                        column.name, schema.table, column.name
                    ),
                    vec![],
                )
                .await?;
            held.extend(
                rows.iter()
                    .filter_map(|r| sqlx::Row::try_get::<String, _>(r, 0).ok())
                    .filter_map(|text| Attachment::from_json(&text))
                    .map(|a| a.blob_id),
            );
        }
    }
    let cutoff = (Timestamp::now() - older_than).as_jiff().as_microsecond();
    let rows = ctx
        .db
        .fetch_all("SELECT \"id\", \"key\" FROM \"kiln_blobs\" WHERE \"created_at\" < ?", vec![Value::Integer(cutoff)])
        .await?;
    let storage = ctx.storage()?;
    let mut purged = 0;
    for row in &rows {
        use sqlx::Row;
        let (id, key): (i64, String) = (row.get(0), row.get(1));
        if held.contains(&id) {
            continue;
        }
        storage.delete(&key).await?;
        storage.delete_prefix(&format!("variants/{key}")).await?;
        ctx.db
            .fetch_all("DELETE FROM \"kiln_blobs\" WHERE \"id\" = ? RETURNING \"id\"", vec![Value::Integer(id)])
            .await?;
        purged += 1;
    }
    if purged > 0 {
        tracing::info!(target: "kiln::storage", purged, "removed unattached files");
    }
    Ok(purged)
}

/// Every stored file's key the database knows (`kiln_blobs`), for `db reset`. Empty when
/// the table doesn't exist yet.
pub(crate) async fn blob_keys(db: &Db) -> Vec<String> {
    let Ok(rows) = db.fetch_all("SELECT \"key\" FROM \"kiln_blobs\"", vec![]).await else { return vec![] };
    rows.iter().filter_map(|r| sqlx::Row::try_get::<String, _>(r, 0).ok()).filter(|k| is_blob_key(k)).collect()
}

/// `ab/cd/abcd…` as [`new_key`] makes them, so nothing else in the store is ever touched.
fn is_blob_key(key: &str) -> bool {
    let parts: Vec<&str> = key.split('/').collect();
    let plain = |s: &str| s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    matches!(parts.as_slice(), [a, b, name]
        if name.len() == 26 && plain(name) && name.starts_with(&format!("{a}{b}")) && a.len() == 2 && b.len() == 2)
}

/// Deletes the files (and their image variants) under `keys`, then the fan-out directories
/// they leave empty. Returns how many files were there.
pub(crate) async fn remove_blobs(storage: &Storage, keys: &[String]) -> Result<usize> {
    let mut removed = 0;
    for key in keys.iter().filter(|k| is_blob_key(k)) {
        if storage.store.head(&StorePath::from(key.as_str())).await.is_ok() {
            removed += 1;
        }
        storage.delete(key).await?;
        storage.delete_prefix(&format!("variants/{key}")).await?;
        if let Some(dir) = storage.local_dir() {
            // Only directories that are now empty go; `remove_dir` refuses any other.
            let (fan, _) = key.rsplit_once('/').unwrap_or_default();
            let (top, _) = fan.split_once('/').unwrap_or_default();
            for empty in
                [format!("variants/{key}"), format!("variants/{fan}"), format!("variants/{top}"), "variants".into()]
                    .into_iter()
                    .chain([fan.to_owned(), top.to_owned()])
            {
                let _ = std::fs::remove_dir(dir.join(empty));
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("5MB"), Some(5 * 1024 * 1024));
        assert_eq!(parse_size("512 kb"), Some(512 * 1024));
        assert_eq!(parse_size("2000"), Some(2000));
        assert_eq!(parse_size("1.5G"), Some(1610612736));
        assert_eq!(parse_size("lots"), None);
    }

    #[test]
    fn filenames_are_cleaned() {
        assert_eq!(clean_filename("C:\\Users\\ada\\photo.png"), "photo.png");
        assert_eq!(clean_filename("../../etc/passwd"), "passwd");
        assert_eq!(clean_filename("a\"<b>.txt"), "ab.txt");
        assert_eq!(clean_filename(".."), "file");
    }

    #[test]
    fn content_is_sniffed_and_scripts_download() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff(b"<html>"), None);
        assert_eq!(real_type(b"fake", "image/png"), "application/octet-stream");
        assert_eq!(real_type(b"\x89PNG\r\n\x1a\nrest", "image/png"), "application/octet-stream");
        assert_eq!(real_type(&crate::testing::Upload::png("a.png").bytes, "text/plain"), "image/png");
        assert_eq!(real_type(b"hello", "text/plain"), "text/plain");
        assert_eq!(real_type(b"hello", ""), "application/octet-stream");
        assert!(inline("image/png"));
        assert!(!inline("image/svg+xml"));
        assert!(!inline("text/html"));
    }
}

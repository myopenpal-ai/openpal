use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// A `crate::app_db`-backed store's query/statement failed. Callers that
    /// need to distinguish "no such row" translate
    /// `rusqlite::Error::QueryReturnedNoRows` into `Error::NotFound`
    /// themselves before this conversion ever runs — see `crate::store::get`
    /// for the pattern, mirroring today's `io::ErrorKind::NotFound`
    /// translation.
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    /// The engine failed to start, crashed, or returned an unexpected response.
    /// The message must be safe to show to a non-technical user.
    #[error("{0}")]
    Engine(String),

    /// A download failed for a reason other than cancellation.
    /// The message must be safe to show to a non-technical user.
    #[error("{0}")]
    Download(String),

    /// An update check, download, or install step failed.
    /// The message must be safe to show to a non-technical user.
    #[error("{0}")]
    Update(String),

    /// Sign-in is required for this feature.
    /// The message must be safe to show to a non-technical user.
    #[error("{0}")]
    AuthRequired(String),

    /// Signed in, but the account's plan doesn't include this feature.
    /// The message must be safe to show to a non-technical user.
    #[error("{0}")]
    NotEntitled(String),

    /// A cloud-routed request failed talking to the account/cloud-model
    /// backend (kept distinct from `Http`/`Engine` so cloud failures read
    /// differently from local-engine failures in logs).
    /// The message must be safe to show to a non-technical user.
    #[error("{0}")]
    Cloud(String),

    #[error("not found: {0}")]
    NotFound(String),

    /// A skill import (`crate::skills::import`/`import_zip`) failed for a
    /// reason other than "the picker was closed" — kept distinct from
    /// [`Error::NotFound`] because that variant's `Display` prepends
    /// "not found: ", which reads as nonsense stapled onto a sentence that
    /// isn't about anything being missing (e.g. "not found: that .zip file
    /// couldn't be extracted"). The message must be safe to show to a
    /// non-technical user.
    #[error("{0}")]
    Import(String),

    /// A data export (`crate::export::write_export_zip`) failed for a
    /// reason other than "the destination picker was closed" — kept
    /// distinct from [`Error::Import`] (scoped to skill imports) even
    /// though the shape mirrors it, so a log line naming the variant
    /// already says which direction failed. The message must be safe to
    /// show to a non-technical user.
    #[error("{0}")]
    Export(String),

    /// A data import (`crate::import::read_import_zip`, reading back a zip
    /// [`Error::Export`]/`crate::export::write_export_zip` produced) failed
    /// for a reason other than "the source picker was closed" — kept
    /// distinct from [`Error::Import`] (scoped to skill imports, unrelated
    /// data) even though the name pairing with [`Error::Export`] would
    /// otherwise suggest reusing it. The message must be safe to show to a
    /// non-technical user.
    #[error("{0}")]
    DataImport(String),

    /// The operation was cancelled by the user. Not an error to surface loudly.
    #[error("cancelled")]
    Cancelled,

    /// The input itself was rejected before any work was done (e.g.
    /// oversized content). Kept distinct from [`Error::NotFound`] (whose
    /// `Display` prepends "not found: ", nonsensical here) and
    /// [`Error::Import`] (scoped to skill imports specifically). The
    /// message must be safe to show to a non-technical user.
    #[error("{0}")]
    Rejected(String),

    /// Device pairing/rotation (`crate::sync::pairing`) failed — a bad MAC,
    /// a failed AEAD open, an invalid signature, a mismatched request, or a
    /// transport error talking to the relay. Deliberately generic in its
    /// `Display` text at call sites (never "MAC verification failed" shown
    /// to a user) — see `crate::sync::pairing`'s own doc comment for why a
    /// cryptographic failure here must never be distinguished from "closed
    /// the dialog" or "timed out" in anything shown to a non-technical
    /// user. The message must be safe to show to a non-technical user.
    #[error("{0}")]
    Pairing(String),
}

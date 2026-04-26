//! Documents plugin: tracks PDF changes between two parallel directory
//! trees (current vs last), generates a diff PDF marking which page rows
//! changed, then exposes the diff as a signed-URL download. Events are
//! derived from filenames in the diff directory (the timestamp is encoded
//! in the filename).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use chrono::DateTime;
use rocket::fs::{FileServer, NamedFile, Options};
use rocket::http::Status;
use rocket::{get, routes, Build, Rocket, Route, State};
use rsa::pkcs1v15::{Signature, SigningKey, VerifyingKey};
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding};
use rsa::sha2::Sha256;
use rsa::signature::{Keypair, RandomizedSigner, SignatureEncoding, Verifier};
use rsa::RsaPrivateKey;
use serde::{Deserialize, Serialize};
use tokio::fs::read_dir;

use timeline_plugin_sdk::auth::AuthedClient;
use timeline_plugin_sdk::{
    APIError, APIResult, CompressedEvent, Context, Manifest, Plugin, Style, TimeRange, Timing,
};

mod files;
mod pdf;

use crate::files::FileManager;
use crate::pdf::get_pdfium;

#[derive(Debug, Clone, Deserialize)]
pub struct Location {
    pub current_path: PathBuf,
    pub last_path: PathBuf,
    pub diff_path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DocumentsConfig {
    pub locations: Vec<Location>,
    /// Optional path to libpdfium.so / pdfium dir. Defaults to ./pdfium
    /// in CWD.
    #[serde(default)]
    pub pdfium_path: Option<PathBuf>,
    /// Optional path to a directory containing pdfjs (`build/pdf.mjs`,
    /// `build/pdf.worker.mjs`). If set, files are served at
    /// `/api/plugin/timeline_plugin_documents/js/...` for the client.
    #[serde(default)]
    pub pdfjs_path: Option<PathBuf>,
    /// Where the RSA signing key lives. Auto-generated as PKCS#8 PEM on
    /// first run. Defaults to `<plugin_root>/signing_key.pem`.
    #[serde(default)]
    pub signing_key_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedDocument {
    pub path: String,
    pub signature: String,
}

pub struct DocumentsPlugin {
    ctx: Context,
    config: DocumentsConfig,
    file_managers: Arc<Vec<FileManager>>,
    signing_key: SigningKey<Sha256>,
    verifying_key: VerifyingKey<Sha256>,
}

impl Plugin for DocumentsPlugin {
    async fn new(ctx: Context) -> anyhow::Result<Self> {
        let config: DocumentsConfig = ctx
            .extra
            .clone()
            .try_into()
            .map_err(|e| anyhow::anyhow!("plugin config: {}", e))?;

        let key_path = config
            .signing_key_path
            .clone()
            .unwrap_or_else(|| ctx.config.plugin_root().join("signing_key.pem"));
        let private_key = load_or_generate_key(&key_path).await?;
        let signing_key: SigningKey<Sha256> = SigningKey::new(private_key);
        let verifying_key = signing_key.verifying_key();

        let pdfium = Arc::new(get_pdfium(config.pdfium_path.as_deref()));
        let file_managers: Vec<FileManager> = config
            .locations
            .iter()
            .map(|v| {
                FileManager::new(
                    pdfium.clone(),
                    v.current_path.clone(),
                    v.last_path.clone(),
                    v.diff_path.clone(),
                )
            })
            .collect();

        Ok(Self {
            ctx,
            config,
            file_managers: Arc::new(file_managers),
            signing_key,
            verifying_key,
        })
    }

    fn manifest(&self) -> Manifest {
        Manifest {
            name: self.ctx.config.name.clone(),
            display_name: self
                .ctx
                .config
                .display_name
                .clone()
                .unwrap_or_else(|| "Documents".into()),
            style: Style::Acc2,
            icon: None,
            web_entry: Some("timeline_plugin_documents_client.js".into()),
        }
    }

    async fn events(&self, range: TimeRange) -> APIResult<Vec<CompressedEvent>> {
        let mut out = Vec::new();
        for fm in self.file_managers.iter() {
            let mut entries = match read_dir(&fm.diff_path).await {
                Ok(e) => e,
                Err(e) => {
                    self.ctx
                        .errors
                        .report(format!("read_dir {}: {}", fm.diff_path.display(), e));
                    continue;
                }
            };
            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| APIError::Custom(format!("dir entry: {}", e)))?
            {
                let path = entry.path();
                let Some((title, time)) = parse_diff_filename(&path) else {
                    continue;
                };
                if !range.includes(&time) {
                    continue;
                }
                let path_str = path.to_string_lossy().into_owned();
                let signature = sign_string(&self.signing_key, &path_str);
                out.push(CompressedEvent {
                    title,
                    time: Timing::Instant(time),
                    data: serde_json::to_value(SignedDocument {
                        path: path_str,
                        signature,
                    })?,
                });
            }
        }
        Ok(out)
    }

    async fn request_loop(&self) -> Option<Duration> {
        for fm in self.file_managers.iter() {
            match fm.update().await {
                Ok(map) => {
                    for (path, result) in map {
                        if let Err(e) = result {
                            self.ctx
                                .errors
                                .report(format!("update {}: {}", path.display(), e));
                        }
                    }
                }
                Err(e) => {
                    self.ctx
                        .errors
                        .report(format!("filemanager init: {}", e));
                }
            }
        }
        Some(Duration::from_secs(60))
    }

    fn routes(&self) -> Vec<Route> {
        routes![get_file]
    }

    fn rocket_attach(&self, rocket: Rocket<Build>) -> Rocket<Build> {
        let mut rocket = rocket.manage(VerifyingKeyState(self.verifying_key.clone()));
        if let Some(pdfjs) = &self.config.pdfjs_path {
            rocket = rocket.mount(
                "/js",
                FileServer::new(pdfjs, Options::Index | Options::DotFiles).rank(11),
            );
        }
        rocket
    }
}

// ---- routes ----

struct VerifyingKeyState(VerifyingKey<Sha256>);

#[get("/file/<file>/<signature>")]
async fn get_file(
    _auth: AuthedClient,
    file: &str,
    signature: &str,
    verifying_key: &State<VerifyingKeyState>,
) -> Result<NamedFile, Status> {
    if !verify_string(&verifying_key.inner().0, file, signature) {
        return Err(Status::Unauthorized);
    }
    NamedFile::open(file).await.map_err(|_| Status::NotFound)
}

// ---- helpers ----

fn parse_diff_filename(path: &Path) -> Option<(String, DateTime<chrono::Utc>)> {
    // `<title>.diff.<unix_seconds>.pdf`
    let name = path.file_name()?.to_str()?;
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() < 4 {
        return None;
    }
    let ts: i64 = parts[parts.len() - 2].parse().ok()?;
    let when = DateTime::from_timestamp(ts, 0)?;
    let title = parts[..parts.len() - 3].join(".");
    Some((title, when))
}

fn sign_string(signing_key: &SigningKey<Sha256>, string: &str) -> String {
    let mut rng = rand::thread_rng();
    let signature = signing_key.sign_with_rng(&mut rng, string.as_bytes());
    base64::prelude::BASE64_STANDARD.encode(signature.to_vec())
}

fn verify_string(verifying_key: &VerifyingKey<Sha256>, string: &str, signature: &str) -> bool {
    let Ok(bytes) = base64::prelude::BASE64_STANDARD.decode(signature) else {
        return false;
    };
    let Ok(sig) = Signature::try_from(bytes.as_slice()) else {
        return false;
    };
    verifying_key.verify(string.as_bytes(), &sig).is_ok()
}

async fn load_or_generate_key(path: &Path) -> anyhow::Result<RsaPrivateKey> {
    use rand::SeedableRng;
    if let Ok(content) = tokio::fs::read_to_string(path).await {
        let key = RsaPrivateKey::from_pkcs8_pem(&content)
            .map_err(|e| anyhow::anyhow!("invalid signing key at {:?}: {}", path, e))?;
        return Ok(key);
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    let key = tokio::task::spawn_blocking(|| {
        let mut rng = rand::rngs::StdRng::from_entropy();
        RsaPrivateKey::new(&mut rng, 2048)
    })
    .await
    .map_err(|e| anyhow::anyhow!("rsa keygen task: {}", e))?
    .map_err(|e| anyhow::anyhow!("rsa keygen: {}", e))?;
    let pem = key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| anyhow::anyhow!("encode signing key: {}", e))?;
    tokio::fs::write(path, pem.as_bytes()).await?;
    tracing::info!(path = %path.display(), "generated new documents signing key");
    Ok(key)
}

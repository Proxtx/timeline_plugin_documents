use crate::types::available_plugins::AvailablePlugins;
use base64::Engine;
use files::FileManager;
use pdf::get_pdfium;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use rocket::get;
use rocket::http::Status;
use rocket::routes;
use rocket::Build;
use rocket::Rocket;
use rocket::State;
use rsa::pkcs1v15::Signature;
use rsa::signature::RandomizedSigner;
use rsa::signature::SignatureEncoding;
use rsa::signature::Verifier;
use rsa::{
    pkcs1v15::{SigningKey, VerifyingKey},
    pkcs8::DecodePrivateKey,
    sha2::Sha256,
    signature::Keypair,
    RsaPrivateKey,
};
use serde::{Deserialize, Serialize};
use server_api::{
    external::{
        futures::{future::join_all, FutureExt, StreamExt},
        tokio::{
            fs::{read_dir, File},
            io::AsyncReadExt,
        },
        toml,
        types::{
            self,
            api::{APIError, CompressedEvent},
            external::{
                chrono::{DateTime, Duration, Utc},
                serde_json,
            },
        },
    },
    plugin::PluginData,
};
use std::str::FromStr;
use std::{fmt::format, path::PathBuf, sync::Arc};

mod files;
mod pdf;

#[derive(Serialize, Deserialize)]
struct Location {
    current_path: PathBuf,
    last_path: PathBuf,
    diff_path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct ConfigData {
    locations: Vec<Location>,
}

pub struct Plugin {
    plugin_data: PluginData,
    file_managers: Arc<Vec<FileManager>>,
    signing_key: SigningKey<Sha256>,
    verifying_key: VerifyingKey<Sha256>,
}

impl server_api::plugin::PluginTrait for Plugin {
    fn get_type() -> AvailablePlugins
    where
        Self: Sized,
    {
        AvailablePlugins::timeline_plugin_documents
    }

    async fn new(data: server_api::plugin::PluginData) -> Self
    where
        Self: Sized,
    {
        let config: ConfigData = toml::Value::try_into(
            data.config
                .clone()
                .expect("Failed to init documents plugin! No config was provided!"),
        )
        .unwrap_or_else(|e| {
            panic!(
                "Unable to init documents plugin! Provided config does not fit the requirements: {}",
                e
            )
        });
        let mut key = String::new();
        File::open("../plugins/timeline_plugin_documents/server/key")
            .await
            .expect("Documents Plugin: Unable to open encryption key.")
            .read_to_string(&mut key)
            .await
            .expect("Documents Plugin: Unable to read key file to string");

        let signing_key = SigningKey::new(
            RsaPrivateKey::from_pkcs8_pem(&key)
                .expect("Documents Plugin: Unable to parse encryption key!"),
        );
        let verifying_key = signing_key.verifying_key();

        let pdfium = Arc::new(get_pdfium());

        let file_managers = config
            .locations
            .into_iter()
            .map(|v| FileManager::new(pdfium.clone(), v.current_path, v.last_path, v.diff_path))
            .collect();

        Plugin {
            plugin_data: data,
            file_managers: Arc::new(file_managers),
            signing_key,
            verifying_key,
        }
    }

    fn request_loop<'a>(
        &'a self,
    ) -> std::pin::Pin<
        Box<
            dyn server_api::external::futures::Future<
                    Output = Option<types::external::chrono::Duration>,
                > + Send
                + 'a,
        >,
    > {
        async move {
            for mngr in self.file_managers.iter() {
                let res = mngr.update().await;
                match res {
                    Ok(v) => {
                        v.iter().for_each(|v| {
                            if let Err(e) = v.1 {
                                self.plugin_data.report_error_string(format!(
                                    "Was unable to update document: {}. FileManagerError: {}",
                                    v.0.display(),
                                    e
                                ));
                            }
                        });
                    }
                    Err(e) => {
                        self.plugin_data
                            .report_error_string(format!("Unable initialize Document Scan: {}", e));
                    }
                }
            }
            Some(Duration::minutes(1))
        }
        .boxed()
    }

    fn get_compressed_events(
        &self,
        query_range: &types::timing::TimeRange,
    ) -> std::pin::Pin<
        Box<
            dyn server_api::external::futures::Future<
                    Output = types::api::APIResult<Vec<types::api::CompressedEvent>>,
                > + Send,
        >,
    > {
        let file_managers = self.file_managers.clone();
        let range = query_range.clone();
        let singing_key = self.signing_key.clone();

        async move {
            Ok(join_all(
                join_all(
                    file_managers
                        .iter()
                        .map(|v| read_dir(&v.diff_path))
                        .collect::<Vec<_>>(),
                )
                .await
                .into_iter()
                .map(|v| async move {
                    match v {
                        Ok(mut v) => {
                            let mut res = Vec::new();
                            while let Some(v) = match v.next_entry().await {
                                Ok(v) => v,
                                Err(e) => {
                                    return Err(APIError::Custom(format!(
                                        "IO Error unable to read file entry: {}",
                                        e
                                    )))
                                }
                            } {
                                res.push(v.path());
                            }
                            Ok(res)
                        }
                        Err(e) => Err(APIError::Custom(format!(
                            "Unable to read diff documents. IO Error: {}",
                            e
                        ))),
                    }
                })
                .collect::<Vec<_>>(),
            )
            .await
            .into_iter()
            .collect::<Result<Vec<_>, APIError>>()?
            .into_iter()
            .flatten()
            .map(|f| {
                f.file_name()
                    .and_then(|v| v.to_str())
                    .ok_or(APIError::Custom(
                        "Unable to parse filename: Can't read filename".to_string(),
                    ))
                    .and_then(|v| {
                        v.split('.')
                            .rev()
                            .nth(1)
                            .ok_or(APIError::Custom(
                                "Unable to parse filename. Filename is too short".to_string(),
                            ))
                            .and_then(|v| {
                                v.parse::<i64>()
                                    .map_err(|v| {
                                        APIError::Custom(format!("Unable to parse filename. Not a valid number inside filename: {}", v))
                                    })
                                    .and_then(|t| {
                                        DateTime::from_timestamp(t, 0)
                                            .ok_or(APIError::Custom("Unable to parse filename. Unable to parse timestamp.".to_string()))
                                            .map(|t| (f.clone(), v.to_string(), t))
                                    })
                            })
                    })
            })
            .collect::<Result<Vec<_>, APIError>>()?
            .into_iter()
            .filter_map(|v| {
                range.includes(&(v.2)).then(||{ 
                    let path = v.0.to_str().unwrap_or("").to_string(); 
                    CompressedEvent {
                    title: v.1,
                    time: types::timing::Timing::Instant(v.2),
                    data: serde_json::to_value(SignedDocument {
                        signature: sign_string(&singing_key, &path),
                        path,
                    }).unwrap(),
                }})
            })
            .collect())
        }
        .boxed()
    }

    fn get_routes() -> Vec<rocket::Route> {
        routes![get_file]
    }

    fn rocket_build_access(&self, rocket: Rocket<Build>) -> Rocket<Build> {
        rocket
            .manage(VerifyingKeyWrapper(self.verifying_key.clone()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SignedDocument {
    path: String,
    signature: String,
}

fn sign_string(signing_key: &SigningKey<Sha256>, string: &str) -> String {
    let mut rng = rand::thread_rng();
    let signature = signing_key.sign_with_rng(&mut rng, string.as_bytes());
    base64::prelude::BASE64_STANDARD.encode(signature.to_vec())
}

fn verify_string(verifying_key: &VerifyingKey<Sha256>, string: &str, signature: &str) -> bool {
    let bytes = match base64::prelude::BASE64_STANDARD.decode(signature) {
        Ok(v) => v,
        Err(_e) => return false,
    };
    let bytes_slice: &[u8] = &bytes;
    verifying_key
        .verify(
            string.as_bytes(),
            &match Signature::try_from(bytes_slice) {
                Ok(v) => v,
                Err(_e) => return false,
            },
        )
        .is_ok()
}

#[get("/file/<file>/<signature>")]
async fn get_file(
    file: &str,
    signature: &str,
    verifying_key: &State<VerifyingKeyWrapper>,
) -> (Status, Option<Result<File, std::io::Error>>) {
    if !verify_string(&verifying_key.inner().0, file, signature) {
        return (Status::Unauthorized, None);
    }
    match PathBuf::from_str(file) {
        Ok(v) => (Status::Ok, (Some(File::open(v).await))),
        Err(_) => (Status::BadRequest, None),
    }
}

struct VerifyingKeyWrapper(pub VerifyingKey<Sha256>);

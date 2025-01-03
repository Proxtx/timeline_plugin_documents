use std::{fmt::format, path::PathBuf, sync::Arc};

use crate::types::available_plugins::AvailablePlugins;
use files::FileManager;
use pdf::get_pdfium;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use serde::{Deserialize, Serialize};
use server_api::{
    external::{
        futures::{future::join_all, FutureExt, StreamExt},
        tokio::fs::read_dir,
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

        let pdfium = Arc::new(get_pdfium());

        let file_managers = config
            .locations
            .into_iter()
            .map(|v| FileManager::new(pdfium.clone(), v.current_path, v.last_path, v.diff_path))
            .collect();

        Plugin {
            plugin_data: data,
            file_managers: Arc::new(file_managers),
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
                range.includes(&(v.2)).then(|| CompressedEvent {
                    title: v.1,
                    time: types::timing::Timing::Instant(v.2),
                    data: serde_json::Value::Null,
                })
            })
            .collect())
        }
        .boxed()
    }
}

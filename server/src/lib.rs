use std::{path::PathBuf, sync::Arc};

use crate::types::available_plugins::AvailablePlugins;
use files::FileManager;
use pdf::get_pdfium;
use serde::{Deserialize, Serialize};
use server_api::{
    external::{toml, types},
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
        todo!()
    }
}

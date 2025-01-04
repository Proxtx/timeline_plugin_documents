use {
    client_api::{
        api,
        external::{
            leptos::{view, IntoView, View},
            types::external::serde::Deserialize,
            web_sys::{wasm_bindgen::prelude::wasm_bindgen, HtmlDivElement},
        },
        plugin::{PluginData, PluginEventData, PluginTrait},
        result::EventResult,
        style::Style,
    },
    leptos::create_node_ref,
    std::{path::PathBuf, rc::Rc},
};

#[wasm_bindgen(module = "/../server/js/pdfGen.js")]
extern "C" {
    type PDFGenerator;
    #[wasm_bindgen(constructor)]
    fn new() -> PDFGenerator;

    #[wasm_bindgen(method)]
    fn generate_pdfs(
        this: &PDFGenerator,
        path: String,
        container: &HtmlDivElement,
        importUrl: String,
        workerSrc: String,
    );
}

#[derive(Clone, Debug, Deserialize)]
pub struct SignedMedia {
    path: String,
    signature: String,
}

pub struct Plugin {
    pdf_generator: Rc<PDFGenerator>,
}

impl PluginTrait for Plugin {
    async fn new(_data: PluginData) -> Self
    where
        Self: Sized,
    {
        Plugin {
            pdf_generator: Rc::new(PDFGenerator::new()),
        }
    }

    fn get_component(&self, data: PluginEventData) -> EventResult<Box<dyn FnOnce() -> View>> {
        let media = data.get_data::<SignedMedia>()?;
        let path = PathBuf::from(media.path);
        let path_string = path.as_os_str().to_str().unwrap().to_string();
        let path_encoded = api::encode_url_component(&path_string);
        let signature_encoded = api::encode_url_component(&media.signature);
        let pdf_generator = self.pdf_generator.clone();
        let url = api::relative_url("/api/plugin/timeline_plugin_documents/file/")
            .unwrap()
            .join(&format!("{}/{}", &path_encoded, &signature_encoded))
            .unwrap()
            .as_str()
            .to_string();
        Ok(Box::new(move || {
            let container_ref = create_node_ref();
            container_ref.on_load(move |elem| {
                pdf_generator.generate_pdfs(
                    url.clone(),
                    &elem,
                    api::relative_url(
                        "/api/plugin/timeline_plugin_documents/js/pdfjs/build/pdf.mjs",
                    )
                    .unwrap()
                    .to_string(),
                    api::relative_url(
                        "/api/plugin/timeline_plugin_documents/js/pdfjs/build/pdf.worker.mjs",
                    )
                    .unwrap()
                    .to_string(),
                );
            });
            view! { <div ref=container_ref></div> }.into_view()
        }))
    }

    fn get_style(&self) -> Style {
        Style::Acc2
    }
}

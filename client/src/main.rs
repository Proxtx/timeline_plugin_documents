use leptos::prelude::*;
use serde::Deserialize;
use wasm_bindgen::prelude::*;

use timeline_plugin_client_sdk::{plugin_entry, PluginContext};

#[wasm_bindgen(module = "/pdfGen.js")]
extern "C" {
    type PDFGenerator;
    #[wasm_bindgen(constructor)]
    fn new() -> PDFGenerator;
    #[wasm_bindgen(method)]
    fn generate_pdfs(
        this: &PDFGenerator,
        path: String,
        container: &web_sys::HtmlDivElement,
        importUrl: String,
        workerSrc: String,
    );
}

#[derive(Debug, Clone, Deserialize)]
struct SignedDocument {
    path: String,
    signature: String,
}

fn main() {
    console_error_panic_hook::set_once();
}

fn render(ctx: PluginContext) -> impl IntoView {
    let Ok(doc) = serde_json::from_value::<SignedDocument>(ctx.event.data.clone()) else {
        return view! { <div>Malformed document event</div> }.into_any();
    };
    let api = ctx.api_base.trim_end_matches('/').to_string();
    let path_encoded = encode_uri(&doc.path);
    let sig_encoded = encode_uri(&doc.signature);
    let file_url = format!("{}/file/{}/{}", api, path_encoded, sig_encoded);
    let import_url = format!("{}/js/pdfjs/build/pdf.mjs", api);
    let worker_src = format!("{}/js/pdfjs/build/pdf.worker.mjs", api);

    let container_ref: NodeRef<leptos::html::Div> = NodeRef::new();
    Effect::new(move |_| {
        let Some(container) = container_ref.get() else {
            return;
        };
        let div: web_sys::HtmlDivElement = container.into();
        let gen = PDFGenerator::new();
        gen.generate_pdfs(
            file_url.clone(),
            &div,
            import_url.clone(),
            worker_src.clone(),
        );
    });
    view! { <div node_ref=container_ref></div> }.into_any()
}

fn encode_uri(s: &str) -> String {
    let Some(win) = web_sys::window() else {
        return s.to_string();
    };
    let Some(encoder) = win.get("encodeURIComponent") else {
        return s.to_string();
    };
    let Ok(func) = encoder.dyn_into::<js_sys::Function>() else {
        return s.to_string();
    };
    func.call1(&JsValue::null(), &JsValue::from_str(s))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| s.to_string())
}

plugin_entry!(render);

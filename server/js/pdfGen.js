import { getDocument,  GlobalWorkerOptions } from "/api/plugin/timeline_plugin_documents/js/pdfjs/build/pdf.mjs";

GlobalWorkerOptions.workerSrc = "/api/plugin/timeline_plugin_documents/js/pdfjs/build/pdf.worker.mjs"

export async function generate_pdfs(path, container) {
  let pdf = await getDocument(path).promise;
  for (let pageNum = 1; pageNum <= pdf.numPages; pageNum++) {
      pdf.getPage(pageNum).then(page => {
          const canvas = document.createElement('canvas');
          container.appendChild(canvas);
          const context = canvas.getContext('2d');
          const viewport = page.getViewport({ scale: 1 });
          canvas.style.width = "100%";
          canvas.width = viewport.width;
          canvas.height = viewport.height;
          page.render({ canvasContext: context, viewport });
    });
  }
}
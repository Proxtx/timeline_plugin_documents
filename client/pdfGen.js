export class PDFGenerator {
  constructor() {
  }
  
  async generate_pdfs(path, container, importUrl, workerSrc,) {
    if (!this.pdfium) {
      this.pdfium = await import(importUrl);
      this.pdfium.GlobalWorkerOptions.workerSrc = workerSrc;
    }
    let getDocument = (await this.pdfium).getDocument;
    let pdf = await getDocument(path).promise;
    for (let pageNum = 1; pageNum <= pdf.numPages; pageNum++) {
        pdf.getPage(pageNum).then(page => {
            const canvas = document.createElement('canvas');
            container.appendChild(canvas);
            const context = canvas.getContext('2d');
            const viewport = page.getViewport({ scale: 2 });
            canvas.style.width = "100%";
            canvas.width = viewport.width;
            canvas.height = viewport.height;
            page.render({ canvasContext: context, viewport });
      });
    }
  }
}

use image::RgbImage;
use pdfium_render::prelude::*;
use rayon::prelude::*;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::{path::Path, thread};

#[derive(Debug)]
enum Comparison {
    Identical,
    Different(Vec<usize>),
}

#[derive(Debug)]
enum Similiarity {
    Different,
    Similar(usize),
}

impl Similiarity {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Similiarity::Different, Similiarity::Different) => std::cmp::Ordering::Equal,
            (Similiarity::Different, Similiarity::Similar(_)) => std::cmp::Ordering::Greater,
            (Similiarity::Similar(_), Similiarity::Different) => std::cmp::Ordering::Less,
            (Similiarity::Similar(s), Similiarity::Similar(o)) => s.cmp(o),
        }
    }
}

#[derive(Debug)]
enum PageSimilarity {
    Different,
    Similar(usize, usize),
}

#[derive(Debug)]
enum PDFComparisonError {
    UnableToLoadPDF(PdfiumError),
    UnableToRenderPDF(PdfiumError),
}

struct PDFComparison {
    pdfium: Arc<Pdfium>,
}

impl PDFComparison {
    pub fn new() -> Self {
        let pdfium = Pdfium::new(
            Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(
                "../plugins/timeline_plugin_documents/pdfium",
            ))
            .or_else(|_| {
                Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("../pdfium"))
            })
            .unwrap(),
        );

        PDFComparison {
            pdfium: Arc::new(pdfium),
        }
    }

    pub fn compare_pdfs(&self, a: &Path, b: &Path) -> Result<Vec<Comparison>, PDFComparisonError> {
        let pdf_a = self.pdfium.load_pdf_from_file(a, None);
        let pdf_b = self.pdfium.load_pdf_from_file(b, None);
        let (pdf_a, pdf_b) = match (pdf_a, pdf_b) {
            (Ok(pdf_a), Ok(pdf_b)) => (Arc::new(pdf_a), Arc::new(pdf_b)),
            (Err(e), _) | (_, Err(e)) => return Err(PDFComparisonError::UnableToLoadPDF(e)),
        };

        let images = thread::scope(|s| {
            //let handle_a = s.spawn(|| self.extract_images(pdf_a));
            //let handle_b = s.spawn(|| self.extract_images(pdf_b));
            //(handle_a.join().unwrap(), handle_b.join().unwrap())
            (self.extract_images(pdf_a), self.extract_images(pdf_b))
        });

        let (images_a, images_b) = match images {
            (Ok(images_a), Ok(images_b)) => (images_a, images_b),
            (Err(e), _) | (_, Err(e)) => return Err(e),
        };

        let page_similarities = PDFComparison::find_min_similarity_for_images(&images_a, &images_b);

        println!("{:?}", page_similarities);

        Ok(Vec::new())
    }

    fn find_min_similarity_for_images(
        img_a: &Vec<RgbImage>,
        img_b: &Vec<RgbImage>,
    ) -> Vec<PageSimilarity> {
        img_a
            .par_iter()
            .map(|a| PDFComparison::find_min_similarity(a, img_b))
            .collect()
    }

    fn find_min_similarity(img_a: &RgbImage, img_b: &Vec<RgbImage>) -> PageSimilarity {
        match img_b
            .par_iter()
            .enumerate()
            .map(|(i, v)| (i, PDFComparison::compare_images(img_a, v)))
            .min_by(|a, b| a.1.cmp(&b.1))
        {
            Some((i, sim)) => match sim {
                Similiarity::Similar(sim) => PageSimilarity::Similar(i, sim),
                Similiarity::Different => PageSimilarity::Different,
            },
            None => PageSimilarity::Different,
        }
    }

    fn compare_images(img_a: &RgbImage, img_b: &RgbImage) -> Similiarity {
        let similarity = AtomicUsize::new(0);
        if img_a.dimensions() != img_b.dimensions() {
            return Similiarity::Different;
        }
        (0..img_a.dimensions().0).into_par_iter().for_each(|x| {
            (0..img_a.dimensions().1).into_par_iter().for_each(|y| {
                if img_a.get_pixel(x, y) != img_b.get_pixel(x, y) {
                    similarity.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            })
        });
        Similiarity::Similar(similarity.into_inner())
    }

    fn extract_images(&self, pdf: Arc<PdfDocument>) -> Result<Vec<RgbImage>, PDFComparisonError> {
        let render_config = PdfRenderConfig::new()
            .set_target_width(500)
            .set_maximum_height(10000)
            .rotate_if_landscape(PdfPageRenderRotation::Degrees90, true);

        (0..pdf.pages().len())
            .map(|p_index| {
                match pdf
                    .pages()
                    .get(p_index)
                    .unwrap()
                    .render_with_config(&render_config)
                {
                    Ok(bitmap) => Ok(bitmap.as_image().into_rgb8()),
                    Err(e) => Err(PDFComparisonError::UnableToRenderPDF(e)),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod pdf_comparison {
    use std::path::PathBuf;

    use super::PDFComparison;

    #[test]
    fn init_pdfium() {
        PDFComparison::new();
    }

    #[test]
    fn comparison() {
        let strct = PDFComparison::new();
        let cmp = strct
            .compare_pdfs(
                &PathBuf::from("./new_dev.pdf"),
                &PathBuf::from("./old_dev.pdf"),
            )
            .unwrap();
        println!("{:?}", cmp);
    }
}

use {
    image::{RgbImage, Rgba, RgbaImage},
    pdfium_render::prelude::*,
    rayon::prelude::*,
    std::{
        error::Error,
        path::Path,
        sync::{atomic::AtomicUsize, Arc},
    },
};

#[derive(Debug)]
pub enum Comparison {
    Identical,
    Different(DifferenceSegments),
}

impl Comparison {
    pub fn from_similarity(sim: &PageSimilarity, img_a: &RgbImage, img_b: &[RgbImage]) -> Self {
        match sim {
            PageSimilarity::Different => Comparison::Different(DifferenceSegments {
                segments: vec![(0., 1.)],
            }),
            PageSimilarity::Similar(index, sim) => {
                if *sim == 0 {
                    Comparison::Identical
                } else {
                    let img_b = img_b.get(*index).unwrap();
                    let num_rows = img_a.rows().len();
                    let mut difference_builder = DifferenceSegementsBuilder::build();
                    img_a
                        .rows()
                        .zip(img_b.rows())
                        .enumerate()
                        .for_each(|(index, (r_a, r_b))| {
                            let mut equal = true;
                            for (p_a, p_b) in r_a.zip(r_b) {
                                if p_a != p_b {
                                    equal = false;
                                    break;
                                }
                            }
                            difference_builder.step(index as f64 / (num_rows - 1) as f64, !equal);
                        });
                    Comparison::Different(difference_builder.finish())
                }
            }
        }
    }
}

struct DifferenceSegementsBuilder {
    segments: DifferenceSegments,
    current_segment: Option<(f64, f64)>,
}

impl DifferenceSegementsBuilder {
    pub fn build() -> Self {
        DifferenceSegementsBuilder {
            segments: DifferenceSegments {
                segments: Vec::new(),
            },
            current_segment: None,
        }
    }

    pub fn step(&mut self, position: f64, hit: bool) {
        match &self.current_segment {
            Some(v) => {
                if hit {
                    self.current_segment = Some((v.0, position));
                } else {
                    self.segments.segments.push(*v);
                    self.current_segment = None;
                }
            }
            None => {
                if hit {
                    self.current_segment = Some((position, position))
                }
            }
        }
    }

    pub fn finish(mut self) -> DifferenceSegments {
        match self.current_segment {
            Some(v) => {
                self.segments.segments.push(v);
                self.segments
            }
            None => self.segments,
        }
    }
}

#[derive(Debug)]
pub struct DifferenceSegments {
    pub segments: Vec<(f64, f64)>,
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
pub enum PageSimilarity {
    Different,
    Similar(usize, usize),
}

#[derive(Debug)]
pub enum PDFComparisonError {
    UnableToLoadPDF(PdfiumError),
    UnableToRenderPDF(PdfiumError),
    PdfiumError(PdfiumError),
}

impl Error for PDFComparisonError {}

impl std::fmt::Display for PDFComparisonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnableToLoadPDF(e) => write!(f, "Was unable to load pdf: {}", e),
            Self::UnableToRenderPDF(e) => write!(f, "Was unable to render a pdf. Error: {}", e),
            Self::PdfiumError(e) => write!(f, "Unkown or unexpected pdfium error: {}", e),
        }
    }
}

impl From<PdfiumError> for PDFComparisonError {
    fn from(value: PdfiumError) -> Self {
        Self::PdfiumError(value)
    }
}

pub fn get_pdfium() -> Pdfium {
    Pdfium::new(
        Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(
            "../plugins/timeline_plugin_documents/pdfium",
        ))
        .or_else(|_| {
            Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("../pdfium"))
        })
        .expect("Was unable to load pdfium."),
    )
}

pub struct PDFComparison {
    pdfium: Arc<Pdfium>,
}

impl PDFComparison {
    pub fn new(pdfium: Arc<Pdfium>) -> Self {
        PDFComparison { pdfium }
    }

    pub fn compare_pdfs(&self, a: &Path, b: &Path) -> Result<Vec<Comparison>, PDFComparisonError> {
        let pdf_a = self.pdfium.load_pdf_from_file(a, None);
        let pdf_b = self.pdfium.load_pdf_from_file(b, None);
        let (pdf_a, pdf_b) = match (pdf_a, pdf_b) {
            (Ok(pdf_a), Ok(pdf_b)) => (Arc::new(pdf_a), Arc::new(pdf_b)),
            (Ok(pdf_a), Err(_e)) => {
                return Ok((0..pdf_a.pages().len())
                    .map(|_| {
                        Comparison::Different(DifferenceSegments {
                            segments: vec![(0., 1.)],
                        })
                    })
                    .collect())
            }
            (Err(e), _) => return Err(PDFComparisonError::UnableToLoadPDF(e)),
        };

        let images = (self.extract_images(pdf_a), self.extract_images(pdf_b));

        let (images_a, images_b) = match images {
            (Ok(images_a), Ok(images_b)) => (images_a, images_b),
            (Err(e), _) | (_, Err(e)) => return Err(e),
        };

        let page_similarities = PDFComparison::find_min_similarity_for_images(&images_a, &images_b);

        Ok(page_similarities
            .par_iter()
            .enumerate()
            .map(|(index, sim)| {
                Comparison::from_similarity(sim, images_a.get(index).unwrap(), &images_b)
            })
            .collect())
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
            .map(
                |p_index| match pdf.pages().get(p_index)?.render_with_config(&render_config) {
                    Ok(bitmap) => Ok(bitmap.as_image().into_rgb8()),
                    Err(e) => Err(PDFComparisonError::UnableToRenderPDF(e)),
                },
            )
            .collect()
    }
}

#[derive(Debug)]
pub enum PDFEditorError {
    UnableToLoadPDF(PdfiumError),
    UnableToSavePDF(PdfiumError),
    UnableToModifyPDF(PdfiumError),
    PdfiumError(PdfiumError),
}

impl Error for PDFEditorError {}

impl std::fmt::Display for PDFEditorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnableToLoadPDF(e) => write!(f, "Was unable to load a pdf. Error: {}", e),
            Self::PdfiumError(e) => write!(f, "Unkown or unexpected pdfium error: {}", e),
            Self::UnableToSavePDF(e) => write!(f, "Was unable to save the pdf: {}", e),
            Self::UnableToModifyPDF(e) => write!(
                f,
                "Was unable to create pdf object or modify the pdf. Error: {}",
                e
            ),
        }
    }
}

impl From<PdfiumError> for PDFEditorError {
    fn from(value: PdfiumError) -> Self {
        PDFEditorError::PdfiumError(value)
    }
}

pub struct PDFEditor {
    pdfium: Arc<Pdfium>,
}

impl PDFEditor {
    pub fn new(pdfium: Arc<Pdfium>) -> Self {
        PDFEditor { pdfium }
    }

    pub fn mark_differences(
        &self,
        in_path: &Path,
        differences: &[Comparison],
        out_path: &Path,
    ) -> Result<(), PDFEditorError> {
        let mut pdf = match self.pdfium.load_pdf_from_file(in_path, None) {
            Ok(v) => v,
            Err(e) => return Err(PDFEditorError::UnableToLoadPDF(e)),
        };

        let mut page_shift: i16 = 0;

        differences
            .iter()
            .enumerate()
            .try_for_each(|(index, difference)| match difference {
                Comparison::Identical => {
                    let _ = pdf
                        .pages_mut()
                        .get((index as i16 + page_shift) as u16)?
                        .delete();
                    page_shift -= 1;
                    Ok::<(), PDFEditorError>(())
                }
                Comparison::Different(seg) => {
                    let mut p = pdf.pages_mut().get((index as i16 + page_shift) as u16)?;
                    self.mark_page_differences(&pdf, &mut p, seg)?;
                    Ok(())
                }
            })?;

        if let Err(e) = pdf.save_to_file(out_path) {
            return Err(PDFEditorError::UnableToSavePDF(e));
        }

        Ok(())
    }

    fn mark_page_differences<'a>(
        &self,
        doc: &PdfDocument<'a>,
        page: &mut PdfPage<'a>,
        segments: &DifferenceSegments,
    ) -> Result<(), PDFEditorError> {
        let image_width = page.width().value as u32 * 5;
        let image_height = page.height().value as u32 * 5;

        let mut buffer = RgbaImage::new(image_width, image_height);

        segments.segments.iter().for_each(|(start, end)| {
            (((image_height as f64 * *start).floor() as u32)
                ..(image_height as f64 * *end).floor() as u32)
                .for_each(|row| {
                    (0..10.min(image_width)).for_each(|column| {
                        buffer.put_pixel(column, row, Rgba([255, 0, 0, 255]));
                    });
                });
        });

        let object = match PdfPageImageObject::new_with_height(doc, &buffer.into(), page.height()) {
            Ok(v) => v,
            Err(e) => return Err(PDFEditorError::UnableToModifyPDF(e)),
        };

        if let Err(e) = page.objects_mut().add_image_object(object) {
            return Err(PDFEditorError::UnableToModifyPDF(e));
        }
        Ok(())
    }
}

#[cfg(test)]
mod pdf_comparison {
    use std::{path::PathBuf, sync::Arc};

    use crate::pdf::get_pdfium;

    use super::{PDFComparison, PDFEditor};

    #[test]
    fn init_pdfium() {
        let pdfium = Arc::new(get_pdfium());
        PDFComparison::new(pdfium);
    }

    #[test]
    fn comparison() {
        let pdfium = Arc::new(get_pdfium());
        let strct = PDFComparison::new(pdfium);
        let cmp = strct
            .compare_pdfs(
                &PathBuf::from("./new_dev.pdf"),
                &PathBuf::from("./old_dev.pdf"),
            )
            .unwrap();
        println!("{:?}", cmp);
    }

    #[test]
    fn annotation() {
        let pdfium = Arc::new(get_pdfium());
        let strct = PDFComparison::new(pdfium.clone());
        let cmp = strct
            .compare_pdfs(
                &PathBuf::from("./new_dev.pdf"),
                &PathBuf::from("./old_dev.pdf"),
            )
            .unwrap();
        println!("Got comparison. Now loading pdfium.");
        let edtr = PDFEditor::new(pdfium.clone());
        println!("Ok good");
        edtr.mark_differences(
            &PathBuf::from("./new_dev.pdf"),
            &cmp,
            &PathBuf::from("./newest_dev.pdf"),
        )
        .unwrap();
    }
}

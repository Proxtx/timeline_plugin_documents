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

/// Build per-row difference segments between two equally-sized page renders.
fn row_diff(img_a: &RgbImage, img_b: &RgbImage) -> DifferenceSegments {
    let num_rows = img_a.rows().len();
    if num_rows <= 1 {
        return DifferenceSegments {
            segments: vec![(0., 1.)],
        };
    }
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
    difference_builder.finish()
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

pub fn get_pdfium(library_path: Option<&std::path::Path>) -> Pdfium {
    if let Some(p) = library_path {
        if let Ok(b) = Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(p)) {
            return Pdfium::new(b);
        }
    }
    Pdfium::new(
        Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path("./pdfium"))
            .or_else(|_| Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(".")))
            .or_else(|_| Pdfium::bind_to_system_library())
            .expect("Was unable to load pdfium. Set [config].pdfium_path or place libpdfium.so in CWD."),
    )
}

pub struct PDFComparison {
    pdfium: Arc<Pdfium>,
    render_config: PdfRenderConfig,
}

impl PDFComparison {
    pub fn new(pdfium: Arc<Pdfium>) -> Self {
        let render_config = PdfRenderConfig::new()
            .set_target_width(500)
            .set_maximum_height(10000)
            .rotate_if_landscape(PdfPageRenderRotation::Degrees90, true);
        PDFComparison {
            pdfium,
            render_config,
        }
    }

    pub fn compare_pdfs(&self, a: &Path, b: &Path) -> Result<Vec<Comparison>, PDFComparisonError> {
        let pdf_a = self.pdfium.load_pdf_from_file(a, None);
        let pdf_b = self.pdfium.load_pdf_from_file(b, None);
        let (pdf_a, pdf_b) = match (pdf_a, pdf_b) {
            (Ok(pdf_a), Ok(pdf_b)) => (pdf_a, pdf_b),
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

        let n_b = pdf_b.pages().len();
        // Render pages on demand, one at a time, comparing page A[i] against
        // pdf_b. Peak memory is ~two page bitmaps regardless of document length
        // — rendering every page up-front (the previous approach) OOM-killed the
        // process on memory-constrained hosts. Slower (up to O(n*m) renders) but
        // safe; the same-index fast path keeps the common "unchanged page" case
        // at ~2 renders/page.
        (0..pdf_a.pages().len())
            .map(|i| {
                let img_a = self.render_page(&pdf_a, i)?;
                self.compare_page(&img_a, &pdf_b, n_b, i)
            })
            .collect()
    }

    /// Classify page `img_a` against pdf_b, rendering B pages on demand.
    fn compare_page(
        &self,
        img_a: &RgbImage,
        pdf_b: &PdfDocument,
        n_b: u16,
        same_index: u16,
    ) -> Result<Comparison, PDFComparisonError> {
        // Fast path: an identical page at the same index needs no full scan.
        if same_index < n_b {
            let img_b = self.render_page(pdf_b, same_index)?;
            if let Similiarity::Similar(0) = PDFComparison::compare_images(img_a, &img_b) {
                return Ok(Comparison::Identical);
            }
        }
        // Otherwise find the most-similar B page (handles inserted/moved pages).
        let mut best: Option<(u16, usize)> = None;
        for j in 0..n_b {
            let img_b = self.render_page(pdf_b, j)?;
            if let Similiarity::Similar(c) = PDFComparison::compare_images(img_a, &img_b) {
                if best.map_or(true, |(_, bc)| c < bc) {
                    best = Some((j, c));
                }
            }
        }
        match best {
            None => Ok(Comparison::Different(DifferenceSegments {
                segments: vec![(0., 1.)],
            })),
            Some((_, 0)) => Ok(Comparison::Identical),
            Some((j, _)) => {
                let img_b = self.render_page(pdf_b, j)?;
                Ok(Comparison::Different(row_diff(img_a, &img_b)))
            }
        }
    }

    fn compare_images(img_a: &RgbImage, img_b: &RgbImage) -> Similiarity {
        if img_a.dimensions() != img_b.dimensions() {
            return Similiarity::Different;
        }
        let similarity = AtomicUsize::new(0);
        (0..img_a.dimensions().0).into_par_iter().for_each(|x| {
            (0..img_a.dimensions().1).into_par_iter().for_each(|y| {
                if img_a.get_pixel(x, y) != img_b.get_pixel(x, y) {
                    similarity.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            })
        });
        Similiarity::Similar(similarity.into_inner())
    }

    fn render_page(&self, pdf: &PdfDocument, index: u16) -> Result<RgbImage, PDFComparisonError> {
        match pdf.pages().get(index)?.render_with_config(&self.render_config) {
            Ok(bitmap) => Ok(bitmap.as_image().into_rgb8()),
            Err(e) => Err(PDFComparisonError::UnableToRenderPDF(e)),
        }
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


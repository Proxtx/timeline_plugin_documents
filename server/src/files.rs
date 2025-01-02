use std::{
    collections::HashMap,
    fs::{FileType, Metadata},
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use server_api::external::{
    futures::{future::BoxFuture, FutureExt},
    tokio::fs::{metadata, read_dir, File},
};

use crate::pdf::{
    get_pdfium, Comparison, PDFComparison, PDFComparisonError, PDFEditor, PDFEditorError,
};

#[derive(Debug)]
enum FileManagerError {
    Io(io::Error),
    PDFComparisonError(PDFComparisonError),
    PDFEditorError(PDFEditorError),
}

impl From<io::Error> for FileManagerError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<PDFComparisonError> for FileManagerError {
    fn from(value: PDFComparisonError) -> Self {
        Self::PDFComparisonError(value)
    }
}

impl From<PDFEditorError> for FileManagerError {
    fn from(value: PDFEditorError) -> Self {
        Self::PDFEditorError(value)
    }
}

enum FileTypeEnum {
    Dir,
    File,
}

impl From<FileType> for FileTypeEnum {
    fn from(value: FileType) -> Self {
        if value.is_dir() {
            Self::Dir
        } else {
            Self::File
        }
    }
}

impl From<&Metadata> for FileTypeEnum {
    fn from(value: &Metadata) -> Self {
        if value.is_dir() {
            Self::Dir
        } else {
            Self::File
        }
    }
}

struct FileManager {
    input_path: PathBuf,
    last_path: PathBuf,
    diff_path: PathBuf,
    pdf_comparison: PDFComparison,
    pdf_editor: PDFEditor,
}

impl FileManager {
    pub fn new(input_path: PathBuf, last_path: PathBuf, diff_path: PathBuf) -> Self {
        let pdfium = Arc::new(get_pdfium());

        FileManager {
            diff_path,
            input_path,
            last_path,
            pdf_comparison: PDFComparison::new(pdfium.clone()),
            pdf_editor: PDFEditor::new(pdfium),
        }
    }

    async fn update(&self) -> Result<(), FileManagerError> {
        let updated_files =
            FileManager::find_updated_files(self.input_path.clone(), self.last_path.clone())
                .await?;

        Ok(())
    }

    fn generate_updated_pdfs(
        &self,
        tasks: HashMap<&Path, Vec<Comparison>>,
    ) -> Result<OutPdfs, FileManagerError> {
        tasks
            .iter()
            .map(|(path, comparisons)| {
                self.pdf_editor
                    .mark_differences(&path, &comparisons, out_path)
            })
            .collect()
    }

    fn generate_comparisons<'a>(
        &self,
        files: &'a [(PathBuf, PathBuf)],
    ) -> Result<HashMap<&'a Path, Vec<Comparison>>, FileManagerError> {
        files
            .iter()
            .filter_map(|(current_path, last_path)| {
                match self.pdf_comparison.compare_pdfs(current_path, last_path) {
                    Ok(res) => {
                        if res
                            .iter()
                            .find(|v| match v {
                                Comparison::Different(_) => true,
                                Comparison::Identical => false,
                            })
                            .is_none()
                        {
                            return None;
                        }
                        Some(Ok((current_path.as_path(), res)))
                    }
                    Err(e) => Some(Err(FileManagerError::PDFComparisonError(e))),
                }
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|v| v.into_iter().collect())
    }

    fn find_updated_files(
        current_path: PathBuf,
        last_path: PathBuf,
    ) -> BoxFuture<'static, Result<Vec<(PathBuf, PathBuf)>, FileManagerError>> {
        async move {
            let mut entires = read_dir(current_path).await?;
            let mut result = Vec::new();
            while let Some(entry) = entires.next_entry().await? {
                let file_type: FileTypeEnum = entry.file_type().await?.into();
                let file_name = entry.file_name();
                let last_path_file_path = last_path.join(file_name);
                let last_path_metadata = metadata(&last_path_file_path)
                    .await
                    .map(|v| (FileTypeEnum::from(&v), v));
                match (file_type, last_path_metadata) {
                    (FileTypeEnum::File, Ok((FileTypeEnum::File, last_meta))) => {
                        let current_meta = metadata(entry.path()).await?;
                        if current_meta.modified()? > last_meta.modified()? {
                            result.push((entry.path(), last_path_file_path));
                        }
                    }
                    (FileTypeEnum::File, Err(e)) => {
                        if let io::ErrorKind::NotFound = e.kind() {
                            result.push((entry.path(), last_path_file_path));
                        } else {
                            return Err(FileManagerError::Io(e));
                        }
                    }
                    (FileTypeEnum::File, Ok((FileTypeEnum::Dir, _last_meta))) => {
                        //wtf
                        result.push((entry.path(), last_path_file_path));
                    }
                    (FileTypeEnum::Dir, Err(e)) => {
                        if let io::ErrorKind::NotFound = e.kind() {
                            result.append(
                                &mut FileManager::find_updated_files(
                                    entry.path(),
                                    last_path_file_path,
                                )
                                .await?,
                            )
                        } else {
                            return Err(FileManagerError::Io(e));
                        }
                    }
                    (FileTypeEnum::Dir, _) => {
                        result.append(
                            &mut FileManager::find_updated_files(entry.path(), last_path_file_path)
                                .await?,
                        );
                    }
                }
            }

            Ok(result)
        }
        .boxed()
    }
}

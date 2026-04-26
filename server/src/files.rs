use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{FileType, Metadata};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use pdfium_render::prelude::Pdfium;
use tokio::fs::{copy, create_dir_all, metadata, read_dir};

use crate::pdf::{Comparison, PDFComparison, PDFComparisonError, PDFEditor, PDFEditorError};

#[derive(Debug, thiserror::Error)]
pub enum FileManagerError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("pdf compare: {0}")]
    Compare(#[from] PDFComparisonError),
    #[error("pdf edit: {0}")]
    Edit(#[from] PDFEditorError),
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

pub struct FileManager {
    pub current_path: PathBuf,
    pub last_path: PathBuf,
    pub diff_path: PathBuf,
    pdf_comparison: PDFComparison,
    pdf_editor: PDFEditor,
}

impl FileManager {
    pub fn new(
        pdfium: Arc<Pdfium>,
        current_path: PathBuf,
        last_path: PathBuf,
        diff_path: PathBuf,
    ) -> Self {
        FileManager {
            diff_path,
            current_path,
            last_path,
            pdf_comparison: PDFComparison::new(pdfium.clone()),
            pdf_editor: PDFEditor::new(pdfium),
        }
    }

    pub async fn update(
        &self,
    ) -> Result<HashMap<PathBuf, Result<PathBuf, FileManagerError>>, FileManagerError> {
        let updated_files = Box::pin(FileManager::find_updated_files(
            self.current_path.clone(),
            self.last_path.clone(),
        ))
        .await?
        .into_iter()
        .collect::<HashMap<_, _>>();
        let comparisons = self.generate_comparisons(&updated_files);
        let updated_pdfs = self.generate_updated_pdfs(comparisons);
        let post_update_status = self.update_changed_pdfs(updated_pdfs, &updated_files).await;
        Ok(post_update_status
            .into_iter()
            .map(|(p, r)| (p.to_path_buf(), r))
            .collect())
    }

    async fn update_changed_pdfs<'a>(
        &self,
        updated_pdfs: HashMap<&'a Path, Result<PathBuf, FileManagerError>>,
        associations: &'a HashMap<PathBuf, PathBuf>,
    ) -> HashMap<&'a Path, Result<PathBuf, FileManagerError>> {
        let mut res = HashMap::new();
        for (path, result) in updated_pdfs.into_iter() {
            let entry = match result {
                Ok(diff_path) => {
                    let target_path = associations.get(path).unwrap();
                    let parent_res = match target_path.parent() {
                        Some(parent) => Some(create_dir_all(parent).await),
                        None => None,
                    };
                    match (parent_res, copy(path, target_path).await) {
                        (Some(Err(e)), _) => (path, Err(FileManagerError::Io(e))),
                        (_, Ok(_)) => (path, Ok(diff_path)),
                        (_, Err(e)) => (path, Err(FileManagerError::Io(e))),
                    }
                }
                Err(e) => (path, Err(e)),
            };
            res.insert(entry.0, entry.1);
        }
        res
    }

    fn generate_updated_pdfs<'a>(
        &self,
        tasks: HashMap<&'a Path, Result<Vec<Comparison>, FileManagerError>>,
    ) -> HashMap<&'a Path, Result<PathBuf, FileManagerError>> {
        tasks
            .into_iter()
            .map(|(path, comparisons)| {
                let res = comparisons.and_then(|comparisons| {
                    let filename = path
                        .file_name()
                        .and_then(|v| v.to_str())
                        .unwrap_or("unknown_filename");
                    let outpath = self.diff_path.join(format!(
                        "{}.diff.{}.pdf",
                        filename,
                        Utc::now().timestamp()
                    ));
                    self.pdf_editor
                        .mark_differences(path, &comparisons, &outpath)?;
                    Ok(outpath)
                });
                (path, res)
            })
            .collect()
    }

    fn generate_comparisons<'a>(
        &self,
        files: &'a HashMap<PathBuf, PathBuf>,
    ) -> HashMap<&'a Path, Result<Vec<Comparison>, FileManagerError>> {
        files
            .iter()
            .filter_map(|(current_path, last_path)| {
                match self.pdf_comparison.compare_pdfs(current_path, last_path) {
                    Ok(res) => {
                        res.iter().find(|v| matches!(v, Comparison::Different(_)))?;
                        Some((current_path.as_path(), Ok(res)))
                    }
                    Err(e) => Some((current_path.as_path(), Err(FileManagerError::Compare(e)))),
                }
            })
            .collect()
    }

    fn find_updated_files(
        current_path: PathBuf,
        last_path: PathBuf,
    ) -> futures::future::BoxFuture<'static, Result<Vec<(PathBuf, PathBuf)>, FileManagerError>>
    {
        Box::pin(async move {
            let mut entries = read_dir(&current_path).await?;
            let mut result = Vec::new();
            while let Some(entry) = entries.next_entry().await? {
                let file_type: FileTypeEnum = entry.file_type().await?.into();
                let file_name = entry.file_name();
                let last_path_file_path = last_path.join(&file_name);
                let last_path_metadata = metadata(&last_path_file_path)
                    .await
                    .map(|v| (FileTypeEnum::from(&v), v));
                match (file_type, last_path_metadata) {
                    (FileTypeEnum::File, Ok((FileTypeEnum::File, last_meta))) => {
                        let current_meta = metadata(entry.path()).await?;
                        if current_meta.modified()? > last_meta.modified()?
                            && entry.path().extension() == Some(OsStr::new("pdf"))
                        {
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
        })
    }
}

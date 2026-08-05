//! Model panel pane — dimensions, features, metadata, pending files.

use crate::python::ModelMetadata;
use std::path::PathBuf;

pub struct ModelPanel {
    pub metadata: Option<ModelMetadata>,
    pub iteration: u32,
    pub pending_files: Vec<PathBuf>,
}

impl ModelPanel {
    pub fn new() -> Self {
        Self { metadata: None, iteration: 0, pending_files: Vec::new() }
    }

    pub fn update(&mut self, metadata: &ModelMetadata, _stl_path: Option<&std::path::Path>, iteration: u32) {
        self.metadata = Some(metadata.clone());
        self.iteration = iteration;
    }

    pub fn clear(&mut self) {
        self.metadata = None;
        self.iteration = 0;
    }
}

use std::env;
use std::path::PathBuf;
use once_cell::sync::Lazy;
use crate::utils::instance::INSTANCE_ID;

/// Folder shared with the guests (9p), where they write their test results. One per instance: it is
/// cleared after each experiment, which would otherwise delete the results of the other instances.
pub static SHARED_DIR_PATH: Lazy<PathBuf> = Lazy::new(|| env::current_dir().unwrap().join("experimentation").join("shared_with_vm").join(&*INSTANCE_ID));

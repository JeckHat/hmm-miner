use std::{fs, path::PathBuf};

pub fn read_filenames(dir: &PathBuf) -> Vec<String> {
    let mut out = Vec::new();

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                if let Some(name) = p.file_name().and_then(|x| x.to_str()) {
                    out.push(format!("{}/{}", dir.to_string_lossy(), name));
                }
            }
        }
    }

    out
}

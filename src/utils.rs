use std::{
    env,
    path::{Path, PathBuf},
};

pub fn relative_to_home(directory: &Path) -> String {
    let Ok(home) = env::var("HOME") else {
        return directory.display().to_string();
    };
    let home_path = PathBuf::from(home);
    if let Ok(stripped) = directory.strip_prefix(&home_path) {
        if stripped.components().next().is_none() {
            return "~".to_string();
        }
        return format!("~/{}", stripped.display());
    }
    directory.display().to_string()
}

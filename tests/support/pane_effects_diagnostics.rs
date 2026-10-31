use std::{io::Write, path::Path};

pub fn emit(state: &Path) {
    if let Ok(record) = std::fs::read(state.join("pane-effects-retries.log")) {
        let _ = std::io::stderr().lock().write_all(&record);
    }
}

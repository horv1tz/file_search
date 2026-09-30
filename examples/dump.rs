use std::path::Path;

use file_search::extract::{Limits, UNIT_SEP, extract_file};

fn main() {
    for arg in std::env::args().skip(1) {
        let path = Path::new(&arg);
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let e = extract_file(path, size, &Limits::default());
        println!("=== {arg}");
        println!("status: {:?}  truncated: {}  units: {:?}", e.status, e.truncated, e.units);
        println!("meta: {:?}", e.meta);
        println!("{}", e.text.replace(UNIT_SEP, "\n---- UNIT ----\n"));
    }
}

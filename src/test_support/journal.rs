//! Read and query diagnostic events from an isolated test session directory.
use std::{fs, path::Path};

use serde_json::Value;

pub(crate) fn log(root: &Path) -> Vec<Value> {
    let paths: Vec<_> = fs::read_dir(root.join("robbie/sessions"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(paths.len(), 1);
    fs::read_to_string(&paths[0])
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub(crate) fn events<'a>(records: &'a [Value], event: &str) -> Vec<&'a Value> {
    records
        .iter()
        .filter(|record| record["event"] == event)
        .collect()
}

//! The extraction child, run through the real binary as the server runs it.

use std::io::Write;
use std::process::{Command, Stdio};

fn run_child(filename: &str, input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fastmail"))
        .args(["__extract-text", filename, &input.len().to_string()])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn the_child_returns_the_document_text_on_stdout() {
    let out = run_child("notes.txt", b"  quarterly figures attached  ");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "quarterly figures attached"
    );
}

#[test]
fn the_child_reads_a_document_larger_than_a_pipe_buffer() {
    let body = "line of text\n".repeat(200_000);
    let out = run_child("big.txt", body.as_bytes());
    assert!(out.status.success());
    assert_eq!(out.stdout.len(), body.trim().len());
}

#[test]
fn an_unparseable_document_is_empty_not_a_failure() {
    let out = run_child("broken.pdf", b"%PDF-1.7 not really");
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
}

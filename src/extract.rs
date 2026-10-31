//! Work done on attachment bytes that a sender chose: document text extraction
//! and image resizing.
//!
//! Both are CPU-bound, so neither runs on an async worker, where it would stall
//! every other request and keep-alive sharing that thread. Each has its own
//! small pool, so a slow document never queues an image behind it.
//!
//! Extraction goes further and runs in a child process when [`isolate`] has
//! been called, as the binary does. kreuzberg parses PDFs with pdfium, which is
//! C++, behind a process-global lock; in-process, a hang holds that lock
//! forever, an abort takes the server down, and a decompression bomb in an
//! Office or EPUB container allocates in the server's address space. A child
//! can be killed on a deadline and given its own memory limit. It does not
//! sandbox against code execution: it runs as the same user, with the
//! environment cleared so it at least holds no credentials.

use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;

/// The hidden first argument that turns the binary into an extraction child.
/// Checked before argument parsing, config loading or logging setup, so the
/// child reads nothing it was not handed on stdin.
pub const CHILD_ARG: &str = "__extract-text";

/// Concurrent extractions and resizes. PDFs serialise on pdfium's lock anyway,
/// and each child can use a lot of memory on a large document.
static EXTRACTORS: Semaphore = Semaphore::const_new(2);
static RESIZERS: Semaphore = Semaphore::const_new(2);

/// Wall-clock limit on one extraction, after which the child is killed. Long,
/// because a large PDF is a legitimate thing to read; the point is that a hung
/// one ends.
const TIMEOUT: Duration = Duration::from_secs(300);

/// Most text read back from one child: kreuzberg's own output limit
/// (`SecurityLimits::max_content_size`), so this only bounds a child that
/// misbehaves.
const MAX_TEXT_BYTES: usize = 100 * 1024 * 1024;

/// The child's data-segment limit: a fixed base plus a multiple of the
/// document. Large documents get room in proportion to their size, while a
/// small file that inflates far past it, as a decompression bomb does, hits
/// the limit and dies alone. Enforced on Linux, where the server runs; macOS
/// does not enforce it.
#[cfg(unix)]
const CHILD_MEMORY_BASE: u64 = 1024 * 1024 * 1024;
#[cfg(unix)]
const CHILD_MEMORY_PER_INPUT_BYTE: u64 = 16;

struct Child {
    exe: PathBuf,
    /// TMPDIR for the child. kreuzberg unpacks its bundled pdfium library into
    /// the temp directory and trusts a file already there, so a shared `/tmp`
    /// would let another local user plant one.
    tmp: PathBuf,
}

static CHILD: OnceLock<Child> = OnceLock::new();

/// Run extraction in child processes of this executable from now on.
///
/// Only the `fastmail` binary can serve as the child, so it opts in from
/// `main`. A library caller, or a test binary, extracts in-process on a
/// blocking thread instead.
pub fn isolate() {
    let Ok(exe) = std::env::current_exe() else {
        tracing::warn!("Cannot locate own executable; extracting text in-process");
        return;
    };
    let tmp = private_tmp().unwrap_or_else(std::env::temp_dir);
    let _ = CHILD.set(Child { exe, tmp });
}

/// A temp directory only this user can write to, under the user's cache
/// directory rather than the shared system one.
fn private_tmp() -> Option<PathBuf> {
    let dir = dirs::cache_dir()?.join("fastmail-cli").join("tmp");
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&dir).ok()?;
    Some(dir)
}

/// Extract text from a document attachment.
///
/// `Ok(None)` when there is nothing to extract: an image, an unsupported
/// format, or a document with no text. An error means the extraction itself
/// failed: it crashed, ran out of memory or time.
pub async fn extract_text(bytes: &[u8], filename: &str) -> anyhow::Result<Option<String>> {
    if crate::util::is_image_extension(filename) {
        return Ok(None);
    }
    let _permit = EXTRACTORS.acquire().await?;
    let text = match CHILD.get() {
        Some(child) => extract_in_child(child, bytes, filename).await?,
        None => {
            let (bytes, filename) = (bytes.to_vec(), filename.to_string());
            let handle = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || {
                handle.block_on(extract_in_process(&bytes, &filename))
            })
            .await?
        }
    };
    Ok(Some(text).filter(|t| !t.is_empty()))
}

/// Resize an image to fit `max_bytes`, off the async runtime.
pub async fn resize_image(
    data: std::sync::Arc<Vec<u8>>,
    content_type: String,
    max_bytes: usize,
) -> Result<(Vec<u8>, String), String> {
    // Moved into the blocking task, so the slot stays taken until the work
    // ends even when the caller stops waiting for it.
    let permit = RESIZERS.acquire().await.map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        crate::util::resize_image(&data, &content_type, max_bytes)
    })
    .await
    .map_err(|e| format!("Image worker failed: {e}"))?
}

async fn extract_in_child(child: &Child, bytes: &[u8], filename: &str) -> anyhow::Result<String> {
    let mut process = tokio::process::Command::new(&child.exe)
        .arg(CHILD_ARG)
        .arg(filename)
        .arg(bytes.len().to_string())
        .env_clear()
        .env("TMPDIR", &child.tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // A dropped request drops this future, and with it the child.
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = process.stdin.take().expect("stdin is piped");
    let stdout = process.stdout.take().expect("stdout is piped");

    let run = async {
        // Written and read together: a document bigger than the pipe buffer
        // would otherwise deadlock once the child starts writing back.
        let write = async {
            // A child that exits early closes the pipe; its exit status is
            // what reports why, so the write error is not interesting.
            let _ = stdin.write_all(bytes).await;
            drop(stdin);
        };
        let read = async {
            let mut out = Vec::new();
            stdout
                .take(MAX_TEXT_BYTES as u64 + 1)
                .read_to_end(&mut out)
                .await
                .map(|_| out)
        };
        let ((), out) = tokio::join!(write, read);
        let mut out = out?;
        if out.len() > MAX_TEXT_BYTES {
            // Still writing, and nothing reads the rest.
            process.start_kill()?;
            out.truncate(MAX_TEXT_BYTES);
        }
        let status = process.wait().await?;
        anyhow::Ok((status, out))
    };

    let (status, out) = match tokio::time::timeout(TIMEOUT, run).await {
        Ok(result) => result?,
        Err(_) => {
            // Dropping `run` released the borrow; reap so no zombie is left.
            let _ = process.kill().await;
            anyhow::bail!("timed out after {}s", TIMEOUT.as_secs());
        }
    };
    if !status.success() && out.len() < MAX_TEXT_BYTES {
        anyhow::bail!("extractor exited with {status}");
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

async fn extract_in_process(bytes: &[u8], filename: &str) -> String {
    let config = kreuzberg::ExtractionConfig::default();
    let mime_type = crate::util::mime_from_filename(filename);
    match kreuzberg::extract_bytes(bytes, &mime_type, &config).await {
        Ok(result) => result.content.trim().to_string(),
        Err(e) => {
            tracing::debug!("kreuzberg extraction failed for {filename}: {e}");
            String::new()
        }
    }
}

/// The child's side: document on stdin, text on stdout. `input_len` is the
/// document's size, which sets the memory limit before any of it is read.
pub async fn run_child(filename: &Path, input_len: u64) -> ExitCode {
    #[cfg(unix)]
    {
        use rustix::process::{Resource, Rlimit, setrlimit};
        let bytes = CHILD_MEMORY_PER_INPUT_BYTE
            .saturating_mul(input_len)
            .saturating_add(CHILD_MEMORY_BASE);
        let limit = Rlimit {
            current: Some(bytes),
            maximum: Some(bytes),
        };
        if setrlimit(Resource::Data, limit).is_err() && cfg!(target_os = "linux") {
            return ExitCode::FAILURE;
        }
    }

    let mut bytes = Vec::new();
    if tokio::io::stdin().read_to_end(&mut bytes).await.is_err() {
        return ExitCode::FAILURE;
    }
    let text = extract_in_process(&bytes, &filename.to_string_lossy()).await;
    let mut stdout = tokio::io::stdout();
    match stdout.write_all(text.as_bytes()).await {
        Ok(()) if stdout.flush().await.is_ok() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn plain_text_extracts_in_process() {
        let text = extract_text(b"hello from an attachment", "notes.txt")
            .await
            .unwrap();
        assert_eq!(text.as_deref(), Some("hello from an attachment"));
    }

    #[tokio::test]
    async fn images_are_not_documents() {
        assert_eq!(extract_text(b"\x89PNG", "photo.png").await.unwrap(), None);
    }
}

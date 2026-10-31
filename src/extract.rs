//! Work done on attachment bytes that a sender chose: document text extraction
//! and image resizing.
//!
//! Both are CPU-bound and can need a lot of memory, so neither runs on an async
//! worker, where it would stall every request and keep-alive sharing that
//! thread. Each has its own small pool, so a slow document never queues an
//! image behind it.
//!
//! When [`isolate`] has been called, as the binary does, both run in a child
//! process. kreuzberg parses PDFs with pdfium, which is C++, behind a
//! process-global lock: in-process, a hang holds that lock forever and an abort
//! takes the server down. Decompression is the other hazard. An Office file, an
//! archive or a PNG can inflate to many times its size, and in a child that
//! memory belongs to a process the kernel kills first, not to the server.
//!
//! The child is not a sandbox against code execution. It runs as the same
//! user; its environment is cleared, so it holds no credentials of its own.

use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;

/// The hidden first argument that turns the binary into a worker child.
/// `main` checks for it before parsing arguments, loading config or setting up
/// logging, so the child reads nothing it was not handed on stdin.
pub const CHILD_ARG: &str = "__attachment-worker";

static EXTRACTORS: Semaphore = Semaphore::const_new(2);
static RESIZERS: Semaphore = Semaphore::const_new(2);

/// Wall-clock limit on one job, after which the child is killed. Long, because
/// a large PDF is a legitimate thing to read; the point is that a hung one
/// ends. Time spent waiting for a free slot is not counted.
const TIMEOUT: Duration = Duration::from_secs(300);

/// Most output read back from one child. kreuzberg's own output limit
/// (`SecurityLimits::max_content_size`) is the same, so this only bounds a
/// child that misbehaves.
const MAX_OUTPUT_BYTES: usize = 100 * 1024 * 1024;

struct Worker {
    exe: PathBuf,
    /// TMPDIR for the child. kreuzberg unpacks its bundled pdfium library into
    /// the temp directory and trusts a file already there, so a shared `/tmp`
    /// would let another local user plant one.
    tmp: PathBuf,
}

static WORKER: OnceLock<Worker> = OnceLock::new();

/// Run attachment work in child processes of this executable from now on.
///
/// Only the `fastmail` binary can serve as the child, so it opts in from
/// `main`. A library caller, or a test binary, does the work in-process on a
/// blocking thread instead.
pub fn isolate() {
    let Ok(exe) = std::env::current_exe() else {
        tracing::warn!("Cannot locate own executable; processing attachments in-process");
        return;
    };
    let tmp = private_tmp().unwrap_or_else(std::env::temp_dir);
    let _ = WORKER.set(Worker { exe, tmp });
}

/// A temp directory only this user can use, under the user's cache directory
/// rather than the shared system one.
fn private_tmp() -> Option<PathBuf> {
    let dir = dirs::cache_dir()?.join("fastmail-cli").join("tmp");
    std::fs::create_dir_all(&dir).ok()?;
    // Fails unless we own it, which is the other half of the check.
    #[cfg(unix)]
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).ok()?;
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
    let mime = crate::util::mime_from_filename(filename);
    let permit = EXTRACTORS.acquire().await?;
    let text = match WORKER.get() {
        Some(worker) => {
            let out = run_worker(worker, &["text", &mime], bytes).await?;
            String::from_utf8_lossy(&out).into_owned()
        }
        None => {
            let bytes = bytes.to_vec();
            let handle = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                handle.block_on(extract_in_process(&bytes, &mime))
            })
            .await?
        }
    };
    Ok(Some(text).filter(|t| !t.is_empty()))
}

/// Resize an image to fit `max_bytes`. Returns the bytes and their MIME type.
pub async fn resize_image(
    data: Arc<Vec<u8>>,
    content_type: String,
    max_bytes: usize,
) -> Result<(Vec<u8>, String), String> {
    // Nothing to decode, so nothing worth a child.
    if data.len() <= max_bytes {
        return Ok((data.to_vec(), content_type));
    }
    let permit = RESIZERS.acquire().await.map_err(|e| e.to_string())?;
    match WORKER.get() {
        Some(worker) => {
            let max = max_bytes.to_string();
            let out = run_worker(worker, &["image", &content_type, &max], &data)
                .await
                .map_err(|e| format!("Failed to resize image: {e}"))?;
            Ok((out, "image/jpeg".to_string()))
        }
        // The permit moves into the task, so the slot stays taken until the
        // work ends even when the caller stops waiting for it.
        None => tokio::task::spawn_blocking(move || {
            let _permit = permit;
            crate::util::resize_image(&data, &content_type, max_bytes)
        })
        .await
        .map_err(|e| format!("Image worker failed: {e}"))?,
    }
}

async fn run_worker(worker: &Worker, args: &[&str], input: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut process = tokio::process::Command::new(&worker.exe)
        .arg(CHILD_ARG)
        .args(args)
        .env_clear()
        .env("TMPDIR", &worker.tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // A dropped request drops this future, and with it the child.
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = process.stdin.take().expect("stdin is piped");
    let stdout = process.stdout.take().expect("stdout is piped");

    let run = async {
        // Written and read together: input bigger than the pipe buffer would
        // otherwise deadlock once the child starts writing back.
        let write = async {
            // A child that exits early closes the pipe; its exit status is
            // what reports why, so the write error is not interesting.
            let _ = stdin.write_all(input).await;
            drop(stdin);
        };
        let read = async {
            let mut out = Vec::new();
            stdout
                .take(MAX_OUTPUT_BYTES as u64 + 1)
                .read_to_end(&mut out)
                .await
                .map(|_| out)
        };
        let ((), out) = tokio::join!(write, read);
        let out = out?;
        if out.len() > MAX_OUTPUT_BYTES {
            process.start_kill()?;
            process.wait().await?;
            anyhow::bail!("output exceeded {} MB", MAX_OUTPUT_BYTES / (1024 * 1024));
        }
        let status = process.wait().await?;
        anyhow::ensure!(status.success(), "worker exited with {status}");
        Ok(out)
    };

    match tokio::time::timeout(TIMEOUT, run).await {
        Ok(result) => result,
        Err(_) => {
            // Dropping `run` released the borrow; reap so no zombie is left.
            let _ = process.kill().await;
            anyhow::bail!("timed out after {}s", TIMEOUT.as_secs())
        }
    }
}

async fn extract_in_process(bytes: &[u8], mime: &str) -> String {
    let config = kreuzberg::ExtractionConfig::default();
    match kreuzberg::extract_bytes(bytes, mime, &config).await {
        Ok(result) => result.content.trim().to_string(),
        Err(e) => {
            tracing::debug!("kreuzberg extraction of {mime} failed: {e}");
            String::new()
        }
    }
}

/// The child's side: input on stdin, result on stdout, failure as the exit
/// status. `args` are what followed [`CHILD_ARG`].
pub fn run_child(args: &[String]) -> ExitCode {
    #[cfg(target_os = "linux")]
    contain_memory();

    let mut input = Vec::new();
    if std::io::Read::read_to_end(&mut std::io::stdin(), &mut input).is_err() {
        return ExitCode::FAILURE;
    }
    let output = match args {
        [job, mime] if job == "text" => {
            // One sequential job: a single-threaded runtime, rather than a
            // worker thread per core.
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return ExitCode::FAILURE;
            };
            runtime
                .block_on(extract_in_process(&input, mime))
                .into_bytes()
        }
        [job, mime, max] if job == "image" => {
            let Ok(max) = max.parse() else {
                return ExitCode::FAILURE;
            };
            match crate::util::resize_image(&input, mime, max) {
                Ok((bytes, _)) => bytes,
                Err(_) => return ExitCode::FAILURE,
            }
        }
        _ => return ExitCode::FAILURE,
    };
    let mut stdout = std::io::stdout().lock();
    match std::io::Write::write_all(&mut stdout, &output)
        .and_then(|()| std::io::Write::flush(&mut stdout))
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

/// Make this process the one the kernel kills when memory runs out, and keep it
/// within what the machine or container has.
///
/// The OOM score is what protects the server: whatever the child asks for, it
/// is chosen before its parent, by the kernel and by a cgroup limit alike. The
/// data limit stops one child taking everything else on the host with it. It is
/// the whole of the available memory rather than some multiple of the input,
/// because legitimate formats differ too much in how far they inflate: a large
/// spreadsheet expands far more than a large PDF.
#[cfg(target_os = "linux")]
fn contain_memory() {
    let _ = std::fs::write("/proc/self/oom_score_adj", "1000");
    if let Some(limit) = memory_limit() {
        use rustix::process::{Resource, Rlimit, setrlimit};
        let _ = setrlimit(
            Resource::Data,
            Rlimit {
                current: Some(limit),
                maximum: Some(limit),
            },
        );
    }
}

/// The cgroup's memory limit (v2, then v1), or the machine's total memory.
#[cfg(target_os = "linux")]
fn memory_limit() -> Option<u64> {
    let read = |path| std::fs::read_to_string(path).ok();
    // v2 says "max" when unlimited; v1 says a number near u64::MAX.
    let cgroup = read("/sys/fs/cgroup/memory.max")
        .or_else(|| read("/sys/fs/cgroup/memory/memory.limit_in_bytes"))
        .and_then(|s| s.trim().parse::<u64>().ok());
    let total = read("/proc/meminfo").and_then(|info| {
        let line = info.lines().find(|l| l.starts_with("MemTotal:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    });
    match (cgroup, total) {
        (Some(c), Some(t)) => Some(c.min(t)),
        (c, t) => c.or(t),
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

    #[cfg(target_os = "linux")]
    #[test]
    fn a_memory_limit_is_found() {
        assert!(memory_limit().is_some_and(|l| l > 0));
    }
}

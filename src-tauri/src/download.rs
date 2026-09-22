//! In-app model downloader (Phase 5).
//!
//! Weights are not in the installer. They are fetched here, into
//! `app_data_dir/models/`, which `paths::init` already registers as a model
//! search root — so a file that lands here is visible to `asr::find_model`
//! and `tts::models_dirs` with no further wiring.
//!
//! Three properties this module exists to guarantee:
//!
//! 1. **Pinned.** Every URL in [`CATALOG`] points at an immutable commit
//!    SHA, never a branch. `main` moves: the previous release of
//!    `scripts/download-models.sh` fetched
//!    `…/resolve/main/en_wav2vec2-base-960h/model.onnx`, and that path is a
//!    404 today because the upstream repo reorganized its directories.
//!    A branch ref is a promise the upstream never made.
//! 2. **Verified.** Every file has a hard-coded sha256 that is checked
//!    before the file is installed. A download that does not match is
//!    deleted, not kept — there is no "warn and continue" path, because a
//!    silently wrong model produces silently wrong transcripts.
//! 3. **Resumable.** Bytes go to `<name>.part` and are only renamed onto
//!    the real name after the hash matches, so a crash or a cancel can
//!    never leave a truncated file that looks installed. A restart resumes
//!    with a `Range` request and re-hashes the existing prefix.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;

use crate::error::AppError;

/// One downloadable file.
pub struct ModelSpec {
    /// Stable file id, used in progress events.
    pub id: &'static str,
    /// Which user-facing group this file belongs to (`asr` / `tts`).
    pub group: &'static str,
    /// Name the file is installed under, inside the models dir.
    pub file_name: &'static str,
    /// Immutable, commit-pinned source URL.
    pub url: &'static str,
    /// Lowercase hex sha256 of the exact bytes at `url`.
    pub sha256: &'static str,
    /// Size in bytes, so the UI can show a total before the first response.
    pub bytes: u64,
}

/// A user-facing download group: what the onboarding screen lists as a row.
#[derive(Clone, Debug, serde::Serialize)]
pub struct GroupInfo {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub bytes: u64,
    /// Every file in the group is present on disk.
    pub installed: bool,
}

// Upstream revisions. Pinned, and re-pinning one means re-deriving its
// hashes below — the two always move together.
//
// darjusul/wav2vec2-ONNX-collection @ 10fe51e6 (2025-03-24)
// rhasspy/piper-voices            @ c10ece1a (2026-09-17)
//
// `concat!` only takes literals, so each base is a macro rather than a
// `const &str`: that keeps the URL joined at compile time while still
// writing the repo path once.
macro_rules! w2v {
    ($file:literal) => {
        concat!(
            "https://huggingface.co/darjusul/wav2vec2-ONNX-collection/resolve/",
            "10fe51e66cf604261f729d7f66499ee56e43b62d",
            "/wav2vec2_onnx_models/en_wav2vec2-asr-base-960h/",
            $file
        )
    };
}
macro_rules! piper {
    ($file:literal) => {
        concat!(
            "https://huggingface.co/rhasspy/piper-voices/resolve/",
            "c10ece1aade47bb51c153c893d14e5bf8e5b7117",
            "/en/en_US/libritts_r/medium/",
            $file
        )
    };
}

/// Every file the app can fetch for itself.
///
/// The hashes are the sha256 of the bytes actually served at these URLs,
/// verified by downloading them; the two large ones additionally match the
/// LFS oids Hugging Face reports for those blobs, which is the same digest
/// computed independently by the host.
pub const CATALOG: &[ModelSpec] = &[
    ModelSpec {
        id: "asr-model",
        group: "asr",
        file_name: "wav2vec2.onnx",
        url: w2v!("model.onnx"),
        sha256: "200bb76524fca2316ee3713338e2f517ca0a032bbc5423b5441d50ddb8761a88",
        bytes: 377_859_675,
    },
    ModelSpec {
        id: "asr-vocab",
        group: "asr",
        file_name: "vocab.json",
        url: w2v!("vocab.json"),
        sha256: "b6c1048b94eb345c58a7c857ed4c198f2b26f43c22b06661409d6720eee35069",
        bytes: 357,
    },
    ModelSpec {
        id: "tts-voice",
        group: "tts",
        file_name: "en_US-libritts_r-medium.onnx",
        url: piper!("en_US-libritts_r-medium.onnx"),
        sha256: "10bb85e071d616fcf4071f369f1799d0491492ab3c5d552ec19fb548fac13195",
        bytes: 78_580_914,
    },
    ModelSpec {
        id: "tts-config",
        group: "tts",
        file_name: "en_US-libritts_r-medium.onnx.json",
        url: piper!("en_US-libritts_r-medium.onnx.json"),
        sha256: "b471dc60d2d8335e819c393d196d6fbf792817f40051257b269878505bc9afb3",
        bytes: 20_123,
    },
];

/// The groups, in the order onboarding shows them.
const GROUPS: &[(&str, &str, &str)] = &[
    (
        "asr",
        "Speech recognition",
        "Listens to you and writes down what you said. Required to be scored.",
    ),
    (
        "tts",
        "Voice",
        "Reads prompts and answers out loud. Required for Listen.",
    ),
];

/// Describe every group, marking the ones already fully present in `dir`.
pub fn groups_in(dir: &Path) -> Vec<GroupInfo> {
    GROUPS
        .iter()
        .map(|(id, label, detail)| {
            let files: Vec<&ModelSpec> = CATALOG.iter().filter(|s| s.group == *id).collect();
            GroupInfo {
                id: (*id).to_string(),
                label: (*label).to_string(),
                detail: (*detail).to_string(),
                bytes: files.iter().map(|s| s.bytes).sum(),
                installed: files.iter().all(|s| dir.join(s.file_name).is_file()),
            }
        })
        .collect()
}

/// Expand group ids to the files they contain.
///
/// An empty `which` means "everything". An unknown id is an error rather
/// than a silent no-op: a typo in a group name should not look like a
/// successful download of nothing.
pub fn select(which: &[String]) -> Result<Vec<&'static ModelSpec>, AppError> {
    if which.is_empty() {
        return Ok(CATALOG.iter().collect());
    }
    for id in which {
        if !GROUPS.iter().any(|(g, _, _)| g == id) {
            return Err(AppError::BadInput(format!("unknown model group {id:?}")));
        }
    }
    Ok(CATALOG
        .iter()
        .filter(|s| which.iter().any(|w| w == s.group))
        .collect())
}

// ─── Progress ────────────────────────────────────────────────────────────────

/// What the frontend sees while a download runs.
///
/// One flat tagged enum rather than several channels: the UI renders a
/// single ordered log, and interleaving separate streams would make
/// "which file is this about" the frontend's problem.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DownloadEvent {
    #[serde(rename_all = "camelCase")]
    Started {
        id: String,
        file: String,
        total: u64,
        index: usize,
        count: usize,
    },
    #[serde(rename_all = "camelCase")]
    Progress {
        id: String,
        received: u64,
        total: u64,
    },
    #[serde(rename_all = "camelCase")]
    Verifying { id: String },
    #[serde(rename_all = "camelCase")]
    Installed { id: String, file: String },
    #[serde(rename_all = "camelCase")]
    Done { installed: Vec<String> },
    #[serde(rename_all = "camelCase")]
    Failed { id: String, message: String },
    #[serde(rename_all = "camelCase")]
    Cancelled { id: String },
}

// ─── Pause / resume / cancel ────────────────────────────────────────────────

const RUNNING: u8 = 0;
const PAUSED: u8 = 1;
const CANCELLED: u8 = 2;

/// Shared run-state for the single in-flight download job.
///
/// `busy` makes the job singleton: two concurrent `download_models` calls
/// writing the same `.part` file would interleave bytes and fail the hash,
/// so the second caller is refused instead.
#[derive(Debug, Default)]
pub struct Control {
    state: AtomicU8,
    busy: AtomicBool,
    wake: Notify,
}

/// RAII guard for the singleton job slot: releasing on drop means an early
/// `?` return cannot wedge the downloader in a permanently busy state.
pub struct BusyGuard<'a>(&'a Control);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::SeqCst);
    }
}

impl Control {
    /// Claim the job slot, or `None` if a download is already running.
    pub fn begin(&self) -> Option<BusyGuard<'_>> {
        self.busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| {
                self.state.store(RUNNING, Ordering::SeqCst);
                BusyGuard(self)
            })
    }

    pub fn pause(&self) {
        let _ = self
            .state
            .compare_exchange(RUNNING, PAUSED, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        let _ = self
            .state
            .compare_exchange(PAUSED, RUNNING, Ordering::SeqCst, Ordering::SeqCst);
        self.wake.notify_waiters();
    }

    /// Cancel the running job. Also wakes a paused one so it can observe
    /// the cancel instead of sleeping forever.
    pub fn cancel(&self) {
        self.state.store(CANCELLED, Ordering::SeqCst);
        self.wake.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::SeqCst) == CANCELLED
    }

    /// Block while paused; return `Err` once cancelled.
    ///
    /// Called between chunks, so pause takes effect within one chunk rather
    /// than at the next file boundary.
    pub async fn checkpoint(&self) -> Result<(), AppError> {
        loop {
            match self.state.load(Ordering::SeqCst) {
                CANCELLED => return Err(AppError::BadInput("download cancelled".into())),
                PAUSED => {
                    // Register before re-reading the state: a `resume`
                    // between the load above and this await would otherwise
                    // notify nobody and leave the job parked.
                    let waiter = self.wake.notified();
                    if self.state.load(Ordering::SeqCst) != PAUSED {
                        continue;
                    }
                    waiter.await;
                }
                _ => return Ok(()),
            }
        }
    }
}

// ─── Hashing ─────────────────────────────────────────────────────────────────

fn hex(digest: &[u8]) -> String {
    use std::fmt::Write as _;
    digest.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// sha256 of a whole file, read in 1 MiB blocks (blocking; call from
/// `spawn_blocking` or a context where a few hundred ms of disk read is
/// acceptable).
pub fn sha256_file(path: &Path) -> Result<String, AppError> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)
        .map_err(|e| AppError::BadInput(format!("cannot read {}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| AppError::BadInput(format!("read failed: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

// ─── The download itself ─────────────────────────────────────────────────────

/// How often progress is emitted. Fast enough to look live, slow enough
/// that a 380 MB file does not push ~100k messages through the IPC channel.
const PROGRESS_EVERY: Duration = Duration::from_millis(200);

/// Fetch `specs` into `dir`, reporting through `emit`.
///
/// Returns the file names actually installed. Files already present with a
/// matching hash are skipped (and reported as installed), so re-running
/// after a partial failure costs one hash pass instead of another download.
pub async fn run(
    specs: &[&'static ModelSpec],
    dir: &Path,
    ctl: &Control,
    emit: &(dyn Fn(DownloadEvent) + Send + Sync),
) -> Result<Vec<String>, AppError> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| AppError::BadInput(format!("cannot create {}: {e}", dir.display())))?;

    let client = reqwest::Client::builder()
        // No overall request timeout: a 380 MB download on a slow line is
        // legitimately long. `read_timeout` is the one that matters — it
        // fires when the socket goes quiet, which is what "stalled" means.
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60))
        .user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .https_only(true)
        .build()
        .map_err(|e| AppError::Network(format!("cannot create http client: {e}")))?;

    let count = specs.len();
    let mut installed = Vec::new();
    for (index, spec) in specs.iter().enumerate() {
        ctl.checkpoint().await?;
        emit(DownloadEvent::Started {
            id: spec.id.to_string(),
            file: spec.file_name.to_string(),
            total: spec.bytes,
            index,
            count,
        });

        match one(&client, spec, dir, ctl, emit).await {
            Ok(()) => {
                emit(DownloadEvent::Installed {
                    id: spec.id.to_string(),
                    file: spec.file_name.to_string(),
                });
                installed.push(spec.file_name.to_string());
            }
            Err(e) if ctl.is_cancelled() => {
                emit(DownloadEvent::Cancelled {
                    id: spec.id.to_string(),
                });
                return Err(e);
            }
            Err(e) => {
                emit(DownloadEvent::Failed {
                    id: spec.id.to_string(),
                    message: e.to_string(),
                });
                return Err(e);
            }
        }
    }

    emit(DownloadEvent::Done {
        installed: installed.clone(),
    });
    Ok(installed)
}

async fn one(
    client: &reqwest::Client,
    spec: &ModelSpec,
    dir: &Path,
    ctl: &Control,
    emit: &(dyn Fn(DownloadEvent) + Send + Sync),
) -> Result<(), AppError> {
    let dest = dir.join(spec.file_name);
    let part = dir.join(format!("{}.part", spec.file_name));

    // Already installed? Verify rather than trust the file name — a
    // truncated or tampered file must not be mistaken for a good one.
    if dest.is_file() {
        emit(DownloadEvent::Verifying {
            id: spec.id.to_string(),
        });
        let dest_owned = dest.clone();
        let found = tokio::task::spawn_blocking(move || sha256_file(&dest_owned))
            .await
            .map_err(|e| AppError::BadInput(format!("hash task failed: {e}")))??;
        if found == spec.sha256 {
            return Ok(());
        }
        // Wrong bytes under the right name: replace it.
        let _ = tokio::fs::remove_file(&dest).await;
    }

    // Resume: re-hash whatever the last attempt left behind.
    let mut hasher = Sha256::new();
    let mut have: u64 = 0;
    if let Ok(meta) = tokio::fs::metadata(&part).await {
        if meta.is_file() && meta.len() < spec.bytes {
            have = meta.len();
            let part_owned = part.clone();
            let prefix = tokio::task::spawn_blocking(move || -> Result<Sha256, AppError> {
                use std::io::Read;
                let mut f = std::fs::File::open(&part_owned)
                    .map_err(|e| AppError::BadInput(format!("cannot reopen part file: {e}")))?;
                let mut h = Sha256::new();
                let mut buf = vec![0u8; 1 << 20];
                loop {
                    let n = f
                        .read(&mut buf)
                        .map_err(|e| AppError::BadInput(format!("read failed: {e}")))?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                }
                Ok(h)
            })
            .await
            .map_err(|e| AppError::BadInput(format!("hash task failed: {e}")))??;
            hasher = prefix;
        } else {
            // A `.part` at or beyond the expected size is not a prefix of
            // anything useful; start over.
            let _ = tokio::fs::remove_file(&part).await;
        }
    }

    let mut req = client.get(spec.url);
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = req
        .send()
        .await
        .map_err(|e| AppError::Network(format!("{}: {e}", spec.file_name)))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(AppError::Network(format!(
            "{}: server returned {status}",
            spec.file_name
        )));
    }
    // A server free to ignore `Range` answers 200 with the whole body. Then
    // the bytes on disk are not a prefix of what is arriving, so the only
    // correct move is to discard them.
    let resuming = have > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT;
    if have > 0 && !resuming {
        hasher = Sha256::new();
        have = 0;
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(resuming)
        .truncate(!resuming)
        .open(&part)
        .await
        .map_err(|e| AppError::BadInput(format!("cannot open {}: {e}", part.display())))?;

    let mut received = have;
    let mut last = Instant::now();
    let mut resp = resp;
    loop {
        if let Err(e) = ctl.checkpoint().await {
            // Cancel is destructive on purpose: a cancelled download should
            // not silently occupy disk until someone finds the `.part`.
            let _ = file.shutdown().await;
            drop(file);
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e);
        }
        let chunk = resp
            .chunk()
            .await
            .map_err(|e| AppError::Network(format!("{}: {e}", spec.file_name)))?;
        let Some(chunk) = chunk else { break };
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| AppError::BadInput(format!("write failed: {e}")))?;
        received += chunk.len() as u64;
        if last.elapsed() >= PROGRESS_EVERY {
            last = Instant::now();
            emit(DownloadEvent::Progress {
                id: spec.id.to_string(),
                received,
                total: spec.bytes,
            });
        }
    }
    file.flush()
        .await
        .map_err(|e| AppError::BadInput(format!("flush failed: {e}")))?;
    file.shutdown()
        .await
        .map_err(|e| AppError::BadInput(format!("close failed: {e}")))?;
    drop(file);

    emit(DownloadEvent::Progress {
        id: spec.id.to_string(),
        received,
        total: spec.bytes,
    });
    emit(DownloadEvent::Verifying {
        id: spec.id.to_string(),
    });

    let found = hex(&hasher.finalize());
    if found != spec.sha256 {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(AppError::BadInput(format!(
            "{}: checksum mismatch (expected {}, got {found}); the file was discarded",
            spec.file_name, spec.sha256
        )));
    }

    tokio::fs::rename(&part, &dest)
        .await
        .map_err(|e| AppError::BadInput(format!("cannot install {}: {e}", dest.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "olp-dl-test-{}-{}-{name}",
            std::process::id(),
            crate::db::now_unix()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn catalog_urls_are_pinned_to_immutable_commits() {
        for spec in CATALOG {
            let rest = spec
                .url
                .split("/resolve/")
                .nth(1)
                .unwrap_or_else(|| panic!("{}: url has no /resolve/ segment", spec.id));
            let rev = rest.split('/').next().unwrap_or("");
            assert_eq!(
                rev.len(),
                40,
                "{}: revision {rev:?} is not a 40-char commit sha",
                spec.id
            );
            assert!(
                rev.chars().all(|c| c.is_ascii_hexdigit()),
                "{}: revision {rev:?} is not hex — a branch name would move under us",
                spec.id
            );
            assert!(
                spec.url.starts_with("https://"),
                "{}: url is not https",
                spec.id
            );
        }
    }

    #[test]
    fn catalog_hashes_are_well_formed_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for spec in CATALOG {
            assert_eq!(spec.sha256.len(), 64, "{}: hash is not 64 chars", spec.id);
            assert!(
                spec.sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{}: hash must be lowercase hex",
                spec.id
            );
            assert!(seen.insert(spec.id), "duplicate spec id {}", spec.id);
            assert!(spec.bytes > 0, "{}: size must be known", spec.id);
        }
    }

    #[test]
    fn select_expands_groups_and_rejects_unknown_ids() {
        let all = select(&[]).unwrap();
        assert_eq!(all.len(), CATALOG.len());

        let asr = select(&["asr".to_string()]).unwrap();
        assert_eq!(asr.len(), 2);
        assert!(asr.iter().all(|s| s.group == "asr"));

        let both = select(&["asr".to_string(), "tts".to_string()]).unwrap();
        assert_eq!(both.len(), CATALOG.len());

        assert!(select(&["nope".to_string()]).is_err());
        // One bad id poisons the whole request rather than silently
        // downloading the good half.
        assert!(select(&["asr".to_string(), "nope".to_string()]).is_err());
    }

    #[test]
    fn groups_report_installed_only_when_every_file_is_present() {
        let dir = temp_dir("groups");
        let before = groups_in(&dir);
        assert!(before.iter().all(|g| !g.installed));

        std::fs::write(dir.join("en_US-libritts_r-medium.onnx"), b"x").unwrap();
        let partial = groups_in(&dir);
        let tts = partial.iter().find(|g| g.id == "tts").unwrap();
        assert!(!tts.installed, "one of two files is not installed");

        std::fs::write(dir.join("en_US-libritts_r-medium.onnx.json"), b"x").unwrap();
        let full = groups_in(&dir);
        let tts = full.iter().find(|g| g.id == "tts").unwrap();
        assert!(tts.installed);
        assert!(!full.iter().find(|g| g.id == "asr").unwrap().installed);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn group_byte_totals_match_the_catalog() {
        let dir = temp_dir("bytes");
        for g in groups_in(&dir) {
            let want: u64 = CATALOG
                .iter()
                .filter(|s| s.group == g.id)
                .map(|s| s.bytes)
                .sum();
            assert_eq!(g.bytes, want, "group {} total", g.id);
            assert!(want > 0);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sha256_file_matches_a_known_vector() {
        let dir = temp_dir("sha");
        let p = dir.join("abc.txt");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let empty = dir.join("empty");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(
            sha256_file(&empty).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sha256_file_spans_multiple_read_blocks() {
        // 3 MiB crosses the 1 MiB read buffer, so a buggy loop that only
        // hashed the first block would be caught here.
        let dir = temp_dir("big");
        let p = dir.join("big.bin");
        let data = vec![0xABu8; 3 << 20];
        std::fs::write(&p, &data).unwrap();
        let mut h = Sha256::new();
        h.update(&data);
        assert_eq!(sha256_file(&p).unwrap(), hex(&h.finalize()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn busy_slot_admits_one_job_at_a_time() {
        let ctl = Control::default();
        let first = ctl.begin().expect("first caller gets the slot");
        assert!(ctl.begin().is_none(), "second caller is refused");
        drop(first);
        assert!(ctl.begin().is_some(), "slot is released on drop");
    }

    #[tokio::test]
    async fn checkpoint_passes_while_running_and_fails_once_cancelled() {
        let ctl = Control::default();
        let _g = ctl.begin().unwrap();
        ctl.checkpoint().await.unwrap();
        ctl.cancel();
        assert!(ctl.is_cancelled());
        assert!(ctl.checkpoint().await.is_err());
    }

    #[tokio::test]
    async fn pause_blocks_until_resume() {
        let ctl = std::sync::Arc::new(Control::default());
        let _g = ctl.begin().unwrap();
        ctl.pause();

        let c = ctl.clone();
        let task = tokio::spawn(async move { c.checkpoint().await });
        // Still parked after a beat.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!task.is_finished(), "checkpoint should block while paused");

        ctl.resume();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("resume must wake the waiter")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn cancel_wakes_a_paused_job() {
        // Without the notify in `cancel`, a paused download would sleep
        // forever and the UI's Cancel button would do nothing.
        let ctl = std::sync::Arc::new(Control::default());
        let _g = ctl.begin().unwrap();
        ctl.pause();
        let c = ctl.clone();
        let task = tokio::spawn(async move { c.checkpoint().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        ctl.cancel();
        let res = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("cancel must wake the waiter")
            .unwrap();
        assert!(res.is_err(), "a cancelled checkpoint reports an error");
    }

    // ─── Network tests ──────────────────────────────────────────────────
    //
    // `#[ignore]`d, so CI (and anyone offline) skips them: they are the
    // only tests here that talk to Hugging Face. Run them deliberately
    // after re-pinning a revision or touching the transfer loop:
    //
    //   cargo test --manifest-path src-tauri/Cargo.toml network_ -- --ignored --nocapture
    //
    // They fetch only the two small files (~20 KB total), never the
    // 380 MB model.

    fn small_specs() -> Vec<&'static ModelSpec> {
        CATALOG
            .iter()
            .filter(|s| s.id == "asr-vocab" || s.id == "tts-config")
            .collect()
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn network_downloads_and_verifies_the_small_files() {
        let dir = temp_dir("net-fresh");
        let ctl = Control::default();
        let _g = ctl.begin().unwrap();
        let seen = std::sync::Mutex::new(Vec::new());
        let emit = |ev: DownloadEvent| seen.lock().unwrap().push(ev);

        let specs = small_specs();
        let installed = run(&specs, &dir, &ctl, &emit).await.expect("download");
        assert_eq!(installed.len(), 2);

        for spec in &specs {
            let path = dir.join(spec.file_name);
            assert!(path.is_file(), "{} was not installed", spec.file_name);
            assert_eq!(
                sha256_file(&path).unwrap(),
                spec.sha256,
                "{} hash mismatch — the pinned revision may have moved",
                spec.file_name
            );
            assert!(
                !dir.join(format!("{}.part", spec.file_name)).exists(),
                "{}: .part file survived a successful install",
                spec.file_name
            );
        }

        let events = seen.lock().unwrap();
        assert!(matches!(events.last(), Some(DownloadEvent::Done { .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn network_resumes_from_a_partial_file() {
        // The resume path is the one that can silently corrupt a file: the
        // existing prefix has to be fed into the hasher before the new
        // bytes, or the final digest is wrong even though every byte on
        // disk is correct. Prove it end to end against a real server.
        let spec = CATALOG.iter().find(|s| s.id == "tts-config").unwrap();
        let dir = temp_dir("net-resume");
        let ctl = Control::default();
        let _g = ctl.begin().unwrap();
        let emit = |_: DownloadEvent| {};

        // First: a complete download, to get authentic bytes to truncate.
        run(&[spec], &dir, &ctl, &emit)
            .await
            .expect("seed download");
        let full = std::fs::read(dir.join(spec.file_name)).unwrap();
        assert_eq!(full.len() as u64, spec.bytes);

        // Now stage a half-finished attempt and re-run.
        let half = full.len() / 2;
        std::fs::remove_file(dir.join(spec.file_name)).unwrap();
        std::fs::write(dir.join(format!("{}.part", spec.file_name)), &full[..half]).unwrap();

        run(&[spec], &dir, &ctl, &emit)
            .await
            .expect("resumed download");

        let got = std::fs::read(dir.join(spec.file_name)).unwrap();
        assert_eq!(got, full, "resumed file differs from a fresh download");
        assert_eq!(sha256_file(&dir.join(spec.file_name)).unwrap(), spec.sha256);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    #[ignore = "network"]
    async fn network_rejects_a_file_whose_hash_does_not_match() {
        // Point a spec at a real URL with the wrong digest: the download
        // must fail and leave nothing behind.
        static BOGUS: ModelSpec = ModelSpec {
            id: "bogus",
            group: "asr",
            file_name: "bogus.json",
            url: w2v!("vocab.json"),
            sha256: "0000000000000000000000000000000000000000000000000000000000000000",
            bytes: 357,
        };
        let dir = temp_dir("net-badhash");
        let ctl = Control::default();
        let _g = ctl.begin().unwrap();
        let emit = |_: DownloadEvent| {};

        let err = run(&[&BOGUS], &dir, &ctl, &emit)
            .await
            .expect_err("a mismatched checksum must fail the download");
        assert!(
            err.to_string().contains("checksum mismatch"),
            "unexpected error: {err}"
        );
        assert!(!dir.join("bogus.json").exists(), "bad file was installed");
        assert!(
            !dir.join("bogus.json.part").exists(),
            "bad .part was kept — it would be resumed into the same bad file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn events_serialize_with_a_discriminating_tag() {
        let v = serde_json::to_value(DownloadEvent::Progress {
            id: "asr-model".into(),
            received: 5,
            total: 10,
        })
        .unwrap();
        assert_eq!(v["kind"], "progress");
        assert_eq!(v["received"], 5);
        let v = serde_json::to_value(DownloadEvent::Done {
            installed: vec!["vocab.json".into()],
        })
        .unwrap();
        assert_eq!(v["kind"], "done");
        assert_eq!(v["installed"][0], "vocab.json");
    }
}

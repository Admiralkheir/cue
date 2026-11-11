//! Finds newer Cue releases on GitHub and installs them over the running copy.
//!
//! The release workflow publishes one archive per platform plus `SHA256SUMS.txt`. An
//! update downloads this platform's archive, checks it against that file, unpacks it with
//! the system's own tools (`ditto` on macOS, `tar` elsewhere) next to the installed app,
//! and swaps it in. The running copy keeps going until it restarts:
//!
//! - macOS replaces the whole `Cue.app` bundle.
//! - Windows can't overwrite a running `cue.exe`, but it can rename it: the old one moves
//!   aside and is deleted on the next start.
//! - Linux renames the new binary over the old one.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::providers::USER_AGENT;

/// The version of this build.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Passed to the new version on Windows, so it waits for the old one to exit instead of
/// handing over to it as a second launch would.
pub const RELAUNCHED_ARG: &str = "--relaunched";

const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/Admiralkheir/cue/releases/latest";
const CHECKSUMS: &str = "SHA256SUMS.txt";
/// Release archives are tens of megabytes; anything far bigger isn't one.
const MAX_DOWNLOAD: u64 = 1024 * 1024 * 1024;
/// Everything an update puts next to the app starts with this, so the next start can find
/// and delete what's left.
const LEFTOVER_PREFIX: &str = ".cue-update";

/// The end of the archive name for this platform, best match first. Matches the names
/// `package.yml` gives the release downloads.
const ASSET_SUFFIXES: &[&str] = if cfg!(target_os = "macos") {
    if cfg!(target_arch = "aarch64") {
        &["-macos-universal.zip", "-macos-aarch64.zip"]
    } else {
        &["-macos-universal.zip", "-macos-x86_64.zip"]
    }
} else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
    &["-windows-x64.zip"]
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    &["-linux-x86_64.tar.gz"]
} else {
    &[]
};

/// The executable's name inside the Windows and Linux archives.
const ARCHIVE_EXE: &str = if cfg!(windows) { "cue.exe" } else { "cue" };

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("network error: {0}")]
    Network(String),
    #[error("GitHub answered {status}: {message}")]
    Http { status: u16, message: String },
    #[error("unexpected answer from GitHub: {0}")]
    Protocol(String),
    #[error("Cue {0} is out, but it has no download for this platform")]
    NoAsset(String),
    #[error("the release is missing {CHECKSUMS}")]
    NoChecksums,
    #[error("{CHECKSUMS} doesn't list {0}")]
    NoChecksum(String),
    #[error("the download was damaged (its checksum doesn't match); try again")]
    ChecksumMismatch,
    #[error("{0}")]
    CantInstall(String),
    #[error("{what}: {source}")]
    Io { what: String, source: io::Error },
}

fn io_error(what: impl Into<String>) -> impl FnOnce(io::Error) -> UpdateError {
    let what = what.into();
    move |source| UpdateError::Io { what, source }
}

/// A newer release that has a download for this platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Without the tag's `v`, e.g. `0.2.0`.
    pub version: String,
    /// The release's page on GitHub, with its notes.
    pub notes_url: String,
    asset: Asset,
    checksums: Asset,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// The parts of GitHub's release object Cue reads.
#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .build()
        .into()
}

fn network(err: impl ToString) -> UpdateError {
    UpdateError::Network(err.to_string())
}

/// GitHub's `{"message": …}`, or the start of the body.
fn github_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|body| body["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| body.chars().take(200).collect())
}

/// Asks GitHub for the latest published release. `None` when this build is that release
/// or newer, or when nothing has been published yet.
pub fn check() -> Result<Option<Release>, UpdateError> {
    let current = Version::parse(CURRENT_VERSION).expect("the crate version is semver");
    let response = agent()
        .get(LATEST_RELEASE_URL)
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28")
        .call()
        .map_err(network)?;
    let status = response.status().as_u16();
    let body = response.into_body().read_to_string().map_err(network)?;
    match status {
        200 => {}
        // The repository has no published release yet.
        404 => return Ok(None),
        _ => {
            return Err(UpdateError::Http {
                status,
                message: github_message(&body),
            });
        }
    }
    let release: GithubRelease =
        serde_json::from_str(&body).map_err(|err| UpdateError::Protocol(err.to_string()))?;
    newer_release(release, &current, ASSET_SUFFIXES)
}

fn newer_release(
    release: GithubRelease,
    current: &Version,
    suffixes: &[&str],
) -> Result<Option<Release>, UpdateError> {
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let tag = release.tag_name.trim();
    let version = Version::parse(tag.strip_prefix('v').unwrap_or(tag))
        .map_err(|err| UpdateError::Protocol(format!("release tag {tag:?}: {err}")))?;
    if version <= *current {
        return Ok(None);
    }
    let asset = suffixes
        .iter()
        .find_map(|suffix| {
            release
                .assets
                .iter()
                .find(|asset| asset.name.ends_with(suffix))
        })
        .ok_or_else(|| UpdateError::NoAsset(version.to_string()))?;
    let checksums = release
        .assets
        .iter()
        .find(|asset| asset.name == CHECKSUMS)
        .ok_or(UpdateError::NoChecksums)?;
    Ok(Some(Release {
        version: version.to_string(),
        notes_url: release.html_url,
        asset: asset.clone(),
        checksums: checksums.clone(),
    }))
}

/// The SHA-256 that a `sha256sum` listing gives for `name`, in lowercase hex.
fn checksum_for(listing: &str, name: &str) -> Option<String> {
    listing.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        // `sha256sum` marks files it read in binary mode with a `*`.
        let file = file.trim_start().trim_start_matches('*');
        let valid = hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit());
        (valid && file == name).then(|| hash.to_ascii_lowercase())
    })
}

/// Where the running copy is installed.
#[derive(Debug, Clone)]
enum Location {
    /// macOS: the `Cue.app` bundle.
    Bundle(PathBuf),
    /// Windows and Linux: the executable, with the files it ships with beside it.
    Executable(PathBuf),
}

impl Location {
    fn path(&self) -> &Path {
        match self {
            Location::Bundle(path) | Location::Executable(path) => path,
        }
    }

    /// The folder the update unpacks into, so the swap is a rename on the same disk.
    fn dir(&self) -> Result<&Path, UpdateError> {
        self.path().parent().ok_or_else(|| {
            UpdateError::CantInstall(format!("{} has no parent folder", self.path().display()))
        })
    }
}

fn locate() -> Result<Location, UpdateError> {
    if cfg!(debug_assertions) {
        return Err(UpdateError::CantInstall(
            "Development builds don't update themselves.".into(),
        ));
    }
    let exe = std::env::current_exe().map_err(io_error("couldn't find Cue's executable"))?;
    if cfg!(target_os = "macos") {
        let exe = fs::canonicalize(&exe).map_err(io_error("couldn't find Cue's executable"))?;
        let in_bundle = exe
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "MacOS");
        let bundle = exe
            .ancestors()
            .nth(3)
            .filter(|bundle| in_bundle && bundle.extension().is_some_and(|ext| ext == "app"))
            .ok_or_else(|| UpdateError::CantInstall("Cue isn't running from Cue.app.".into()))?;
        // macOS runs a downloaded app that was never moved from a hidden, read-only copy.
        if bundle.to_string_lossy().contains("/AppTranslocation/") {
            return Err(UpdateError::CantInstall(
                "Move Cue.app to your Applications folder and reopen it; then it can update \
                 itself."
                    .into(),
            ));
        }
        return Ok(Location::Bundle(bundle.to_owned()));
    }
    Ok(Location::Executable(exe))
}

/// Why this copy can't install updates itself, if it can't. It can still check for them.
pub fn install_blocker() -> Option<String> {
    locate().err().map(|err| err.to_string())
}

/// An installed update, waiting for Cue to restart.
#[derive(Debug)]
pub struct Installed {
    location: Location,
}

/// Downloads `release`, checks it and puts it in place of the running copy. Blocks for as
/// long as the download takes.
pub fn install(release: &Release) -> Result<Installed, UpdateError> {
    let location = locate()?;
    let dir = location.dir()?;
    let staging = dir.join(crate::unique_id(LEFTOVER_PREFIX));
    fs::create_dir(&staging).map_err(io_error(format!(
        "couldn't write to {} (Cue needs to be in a folder you can change to update itself)",
        dir.display()
    )))?;
    let installed = install_from(release, &location, &staging);
    // Whatever is left is cleaned up on the next start if this fails.
    let _ = fs::remove_dir_all(&staging);
    installed.map(|()| Installed { location })
}

fn install_from(release: &Release, location: &Location, staging: &Path) -> Result<(), UpdateError> {
    let agent = agent();
    let listing = get(&agent, &release.checksums.browser_download_url)?
        .read_to_string()
        .map_err(network)?;
    let expected = checksum_for(&listing, &release.asset.name)
        .ok_or_else(|| UpdateError::NoChecksum(release.asset.name.clone()))?;

    // A fixed name: the asset's name comes from GitHub and never becomes a path.
    let extension = if release.asset.name.ends_with(".tar.gz") {
        "tar.gz"
    } else {
        "zip"
    };
    let archive = staging.join(format!("download.{extension}"));
    let actual = download(&agent, &release.asset.browser_download_url, &archive)?;
    if actual != expected {
        return Err(UpdateError::ChecksumMismatch);
    }

    let unpacked = staging.join("unpacked");
    fs::create_dir(&unpacked).map_err(io_error("couldn't make a folder for the update"))?;
    unpack(&archive, &unpacked)?;
    match location {
        Location::Bundle(bundle) => swap_bundle(bundle, &unpacked),
        Location::Executable(exe) => swap_executable(exe, &unpacked),
    }
}

fn get(agent: &ureq::Agent, url: &str) -> Result<ureq::Body, UpdateError> {
    let response = agent.get(url).call().map_err(network)?;
    let status = response.status().as_u16();
    let mut body = response.into_body();
    if status != 200 {
        let text = body.read_to_string().unwrap_or_default();
        return Err(UpdateError::Http {
            status,
            message: github_message(&text),
        });
    }
    Ok(body)
}

/// Saves `url` to `path` and returns its SHA-256 in lowercase hex.
fn download(agent: &ureq::Agent, url: &str, path: &Path) -> Result<String, UpdateError> {
    let mut reader = get(agent, url)?.into_reader();
    let mut file = File::create(path).map_err(io_error("couldn't save the download"))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    let mut total = 0;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(network(err)),
        };
        total += read as u64;
        if total > MAX_DOWNLOAD {
            return Err(UpdateError::Protocol(
                "the download is far too large".into(),
            ));
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .map_err(io_error("couldn't save the download"))?;
    }
    file.sync_all()
        .map_err(io_error("couldn't save the download"))?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn unpack(archive: &Path, into: &Path) -> Result<(), UpdateError> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("/usr/bin/ditto");
        command.args(["-x", "-k"]).arg(archive).arg(into);
        command
    } else if cfg!(windows) {
        // bsdtar, which reads zip files, ships with Windows 10 (1803) and later.
        let root =
            std::env::var_os("SystemRoot").map_or_else(|| "C:\\Windows".into(), PathBuf::from);
        let mut command = Command::new(root.join("System32").join("tar.exe"));
        command.arg("-xf").arg(archive).arg("-C").arg(into);
        command
    } else {
        let mut command = Command::new("tar");
        command.arg("-xzf").arg(archive).arg("-C").arg(into);
        command
    };
    hide_console(&mut command);
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(io_error("couldn't run the unpacker"))?;
    if !output.status.success() {
        return Err(UpdateError::CantInstall(format!(
            "couldn't unpack the update: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

/// Cue is a GUI app on Windows; without this each helper flashes a console window.
fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// A name next to `path` for moving the old copy aside.
fn aside(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(Default::default, |name| name.to_string_lossy());
    path.with_file_name(format!("{}-old-{name}", crate::unique_id(LEFTOVER_PREFIX)))
}

fn swap_bundle(bundle: &Path, unpacked: &Path) -> Result<(), UpdateError> {
    // The zip holds Cue.app at its root, even if the installed copy was renamed.
    let new = unpacked.join("Cue.app");
    if !new.join("Contents").join("MacOS").join("cue").is_file() {
        return Err(UpdateError::CantInstall(
            "the download doesn't contain Cue.app".into(),
        ));
    }
    let old = aside(bundle);
    fs::rename(bundle, &old).map_err(io_error(format!(
        "couldn't move {} aside",
        bundle.display()
    )))?;
    if let Err(source) = fs::rename(&new, bundle) {
        let _ = fs::rename(&old, bundle);
        return Err(io_error("couldn't put the new Cue.app in place")(source));
    }
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

fn swap_executable(exe: &Path, unpacked: &Path) -> Result<(), UpdateError> {
    // The archive holds a single folder (Cue on Windows, cue on Linux) with the app inside.
    let folders = fs::read_dir(unpacked)
        .map_err(io_error("couldn't read the update"))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    let [folder] = folders.as_slice() else {
        return Err(UpdateError::CantInstall(
            "the download isn't laid out as expected".into(),
        ));
    };
    let new_exe = folder.join(ARCHIVE_EXE);
    if !new_exe.is_file() {
        return Err(UpdateError::CantInstall(format!(
            "the download doesn't contain {ARCHIVE_EXE}"
        )));
    }
    let dir = exe
        .parent()
        .ok_or_else(|| UpdateError::CantInstall("Cue's executable has no folder".into()))?;

    // The files shipped beside it (licenses) aren't in use, so they're simply replaced.
    for entry in fs::read_dir(folder)
        .map_err(io_error("couldn't read the update"))?
        .flatten()
    {
        let path = entry.path();
        if path == new_exe || !path.is_file() {
            continue;
        }
        let target = dir.join(entry.file_name());
        fs::rename(&path, &target)
            .map_err(io_error(format!("couldn't replace {}", target.display())))?;
    }

    if cfg!(windows) {
        let old = aside(exe);
        fs::rename(exe, &old)
            .map_err(io_error(format!("couldn't move {} aside", exe.display())))?;
        if let Err(source) = fs::rename(&new_exe, exe) {
            let _ = fs::rename(&old, exe);
            return Err(io_error("couldn't put the new version in place")(source));
        }
    } else {
        // The running process keeps the old file open; the name now points to the new one.
        fs::rename(&new_exe, exe).map_err(io_error("couldn't put the new version in place"))?;
    }
    Ok(())
}

impl Installed {
    /// Starts the new version once this process has exited; quit right after this.
    pub fn relaunch(&self) -> Result<(), UpdateError> {
        let target = self.location.path();
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new(target);
            command.arg(RELAUNCHED_ARG);
            command
        };
        #[cfg(not(windows))]
        let mut command = {
            let launch = if cfg!(target_os = "macos") {
                "exec /usr/bin/open \"$2\""
            } else {
                "exec \"$2\""
            };
            let mut command = Command::new("/bin/sh");
            command
                .arg("-c")
                .arg(format!(
                    "while kill -0 \"$1\" 2>/dev/null; do sleep 0.2; done; {launch}"
                ))
                .arg("cue-relaunch")
                .arg(std::process::id().to_string())
                .arg(target);
            command
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(drop)
            .map_err(io_error("couldn't start the new version"))
    }
}

/// Deletes what an earlier update left next to the app: the old copy it moved aside and
/// any download that was cut short. Returns how many it removed.
pub fn clean_up() -> usize {
    let Ok(location) = locate() else {
        return 0;
    };
    let Ok(dir) = location.dir() else {
        return 0;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(LEFTOVER_PREFIX)
        })
        .filter(|entry| {
            let path = entry.path();
            if path.is_dir() {
                fs::remove_dir_all(&path).is_ok()
            } else {
                fs::remove_file(&path).is_ok()
            }
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn github(tag: &str, assets: &[&str]) -> GithubRelease {
        serde_json::from_value(json!({
            "tag_name": tag,
            "html_url": format!("https://github.com/Admiralkheir/cue/releases/tag/{tag}"),
            "draft": false,
            "prerelease": false,
            "assets": assets.iter().map(|name| json!({
                "name": name,
                "browser_download_url": format!("https://example.com/{name}"),
                "size": 1,
            })).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    const SUFFIXES: &[&str] = &["-macos-universal.zip", "-macos-aarch64.zip"];

    fn newer(tag: &str, assets: &[&str]) -> Result<Option<Release>, UpdateError> {
        newer_release(github(tag, assets), &Version::new(0, 1, 0), SUFFIXES)
    }

    #[test]
    fn offers_a_newer_release_with_its_download() {
        let release = newer(
            "v0.2.0",
            &[
                "Cue-0.2.0-macos-aarch64.zip",
                "Cue-0.2.0-macos-universal.zip",
                "Cue-0.2.0-windows-x64.zip",
                CHECKSUMS,
            ],
        )
        .unwrap()
        .unwrap();
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.asset.name, "Cue-0.2.0-macos-universal.zip");
        assert_eq!(release.checksums.name, CHECKSUMS);
        assert!(release.notes_url.ends_with("/v0.2.0"));
    }

    #[test]
    fn ignores_the_same_or_older_versions() {
        let assets = ["Cue-macos-universal.zip", CHECKSUMS];
        assert_eq!(newer("v0.1.0", &assets).unwrap(), None);
        assert_eq!(newer("0.0.9", &assets).unwrap(), None);
        assert!(newer("0.1.1", &assets).unwrap().is_some());
    }

    #[test]
    fn ignores_drafts_and_prereleases() {
        let mut release = github("v9.0.0", &["Cue-macos-universal.zip", CHECKSUMS]);
        release.prerelease = true;
        assert_eq!(
            newer_release(release, &Version::new(0, 1, 0), SUFFIXES).unwrap(),
            None
        );
    }

    #[test]
    fn needs_a_download_for_this_platform_and_checksums() {
        assert!(matches!(
            newer("v0.2.0", &["Cue-0.2.0-windows-x64.zip", CHECKSUMS]),
            Err(UpdateError::NoAsset(version)) if version == "0.2.0"
        ));
        assert!(matches!(
            newer("v0.2.0", &["Cue-0.2.0-macos-universal.zip"]),
            Err(UpdateError::NoChecksums)
        ));
        assert!(matches!(
            newer("latest", &[]),
            Err(UpdateError::Protocol(_))
        ));
    }

    #[test]
    fn reads_sha256sum_listings() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let listing = format!(
            "{a}  Cue-0.2.0-macos-universal.zip\n{b} *Cue-0.2.0-windows-x64.zip\nnot a line\n"
        );
        assert_eq!(
            checksum_for(&listing, "Cue-0.2.0-macos-universal.zip"),
            Some(a)
        );
        assert_eq!(
            checksum_for(&listing, "Cue-0.2.0-windows-x64.zip"),
            Some("b".repeat(64))
        );
        assert_eq!(checksum_for(&listing, "Cue-0.2.0-windows-x64"), None);
        assert_eq!(checksum_for("abc  file.zip", "file.zip"), None);
    }

    #[test]
    fn this_platform_has_a_download_name() {
        assert_eq!(
            ASSET_SUFFIXES.is_empty(),
            !(cfg!(target_os = "macos")
                || cfg!(all(target_os = "windows", target_arch = "x86_64"))
                || cfg!(all(target_os = "linux", target_arch = "x86_64")))
        );
    }

    #[test]
    fn this_version_parses() {
        Version::parse(CURRENT_VERSION).unwrap();
    }

    #[test]
    fn swaps_the_executable_and_the_files_beside_it() {
        let root = tempfile::tempdir().unwrap();
        let installed = root.path().join("install");
        let unpacked = root.path().join("unpacked");
        let folder = unpacked.join("Cue");
        fs::create_dir_all(&installed).unwrap();
        fs::create_dir_all(&folder).unwrap();
        let exe = installed.join(ARCHIVE_EXE);
        fs::write(&exe, "old").unwrap();
        fs::write(installed.join("LICENSE"), "old license").unwrap();
        fs::write(folder.join(ARCHIVE_EXE), "new").unwrap();
        fs::write(folder.join("LICENSE"), "new license").unwrap();

        swap_executable(&exe, &unpacked).unwrap();

        assert_eq!(fs::read_to_string(&exe).unwrap(), "new");
        assert_eq!(
            fs::read_to_string(installed.join("LICENSE")).unwrap(),
            "new license"
        );
        let leftovers = fs::read_dir(&installed)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(LEFTOVER_PREFIX)
            })
            .count();
        // Windows keeps the old executable aside until the next start.
        assert_eq!(leftovers, usize::from(cfg!(windows)));
    }

    #[test]
    fn refuses_an_archive_without_the_app() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("unpacked/Cue")).unwrap();
        let exe = root.path().join(ARCHIVE_EXE);
        fs::write(&exe, "old").unwrap();
        assert!(matches!(
            swap_executable(&exe, &root.path().join("unpacked")),
            Err(UpdateError::CantInstall(_))
        ));
        assert_eq!(fs::read_to_string(&exe).unwrap(), "old");
    }
}

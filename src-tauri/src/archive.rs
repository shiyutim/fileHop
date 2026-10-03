//! Prepare folder transfers in a private, automatically cleaned temporary directory.
use crate::protocol::{Result, CANCELLED};
use std::{
    ffi::OsStr,
    fs::{self, File, Metadata, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use tokio::sync::watch;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

const MAX_SIZE: u64 = 10 * 1024 * 1024 * 1024 * 1024;
const BUFFER_SIZE: usize = 64 * 1024;

#[derive(Debug)]
pub(crate) struct PreparedArchive {
    path: PathBuf,
    size: u64,
    // The directory is private (0700 on Unix) and survives until sending finishes.
    _temporary: TempDir,
}

impl PreparedArchive {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn size(&self) -> u64 {
        self.size
    }
}

/// This performs blocking filesystem and compression work; call using spawn_blocking.
pub(crate) fn compress_directory(
    path: &Path,
    cancelled: &watch::Receiver<bool>,
) -> Result<PreparedArchive> {
    check_cancelled(cancelled)?;
    let temporary = tempfile::Builder::new()
        .prefix("filehop-archive-")
        .tempdir()
        .map_err(|e| format!("无法创建临时压缩文件：{e}"))?;
    compress_into(path, cancelled, temporary, MAX_SIZE)
}

fn compress_into(
    path: &Path,
    cancelled: &watch::Receiver<bool>,
    temporary: TempDir,
    max_size: u64,
) -> Result<PreparedArchive> {
    check_cancelled(cancelled)?;
    let root_metadata = safe_metadata(path)?;
    if !root_metadata.is_dir() {
        return Err("请选择需要压缩的文件夹".into());
    }
    let root_name = portable_component(path.file_name().ok_or("无法压缩没有名称的文件夹")?)?;
    let root = fs::canonicalize(path).map_err(|e| format!("无法打开文件夹：{e}"))?;
    let temporary_path =
        fs::canonicalize(temporary.path()).map_err(|e| format!("无法打开临时文件夹：{e}"))?;
    let archive_path = temporary.path().join("archive.zip");
    let file = File::create(&archive_path).map_err(|e| format!("无法创建压缩文件：{e}"))?;
    let writer = ArchiveFile {
        file,
        cancelled: cancelled.clone(),
        position: 0,
        max_size,
    };
    let mut archive = ZipWriter::new(writer);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6));
    let root_entry = format!("{root_name}/");
    archive
        .add_directory(&root_entry, directory_options(&root_metadata))
        .map_err(|e| compression_error(e, cancelled))?;
    let entries = fs::read_dir(&root).map_err(|e| format!("无法读取文件夹内容：{e}"))?;
    // Keep only active directory iterators and one bounded file buffer in memory.
    // ZipWriter retains the central-directory metadata required by the ZIP format.
    let mut directories = vec![(entries, root_entry)];
    let mut buffer = [0; BUFFER_SIZE];
    let mut raw_size = 0_u64;

    while let Some((entries, prefix)) = directories.last_mut() {
        check_cancelled(cancelled)?;
        let Some(entry) = entries.next() else {
            directories.pop();
            continue;
        };
        let entry = entry.map_err(|e| format!("无法读取文件夹内容：{e}"))?;
        let entry_path = entry.path();
        // If the selected folder contains the OS temp directory, never archive
        // this preparation's own output as it grows.
        if entry_path == temporary_path {
            continue;
        }
        let metadata = safe_metadata(&entry_path)?;
        let name = portable_component(&entry.file_name())?;
        let mut member = format!("{prefix}{name}");
        if metadata.is_dir() {
            member.push('/');
        }
        if member.len() > u16::MAX as usize {
            return Err(format!("文件夹内的路径过长，无法压缩：{member}"));
        }
        if metadata.is_dir() {
            archive
                .add_directory(&member, directory_options(&metadata))
                .map_err(|e| compression_error(e, cancelled))?;
            let children = fs::read_dir(&entry_path)
                .map_err(|e| format!("无法读取文件夹 {}：{e}", entry_path.display()))?;
            directories.push((children, member));
            continue;
        }

        if metadata.len() > max_size.saturating_sub(raw_size) {
            return Err("文件夹内容超过单次传输大小限制（10 TiB）".into());
        }
        let mut input = open_regular_file(&entry_path)?;
        let before = input
            .metadata()
            .map_err(|e| format!("无法读取文件属性：{e}"))?;
        // Reserve ZIP64 for large files, allowing for DEFLATE overhead on
        // incompressible input near 4 GiB. Tiny members use usual ZIP headers.
        archive
            .start_file(
                &member,
                file_options(options, &before)
                    .large_file(before.len() >= u32::MAX as u64 - 16 * 1024 * 1024),
            )
            .map_err(|e| compression_error(e, cancelled))?;
        let mut file_size = 0_u64;
        loop {
            check_cancelled(cancelled)?;
            let count = input
                .read(&mut buffer)
                .map_err(|e| format!("无法读取文件 {}：{e}", entry_path.display()))?;
            if count == 0 {
                break;
            }
            file_size += count as u64;
            if file_size > before.len() {
                return Err(format!(
                    "压缩时文件发生变化，请重试：{}",
                    entry_path.display()
                ));
            }
            raw_size = raw_size
                .checked_add(count as u64)
                .filter(|&size| size <= max_size)
                .ok_or("文件夹内容超过单次传输大小限制（10 TiB）")?;
            archive
                .write_all(&buffer[..count])
                .map_err(|e| compression_error(e, cancelled))?;
        }
        let after = input
            .metadata()
            .map_err(|e| format!("无法读取文件属性：{e}"))?;
        if file_size != before.len()
            || before.len() != after.len()
            || before.modified().ok() != after.modified().ok()
        {
            return Err(format!(
                "压缩时文件发生变化，请重试：{}",
                entry_path.display()
            ));
        }
    }

    check_cancelled(cancelled)?;
    let mut output = archive
        .finish()
        .map_err(|e| compression_error(e, cancelled))?;
    output
        .flush()
        .map_err(|e| compression_error(e, cancelled))?;
    let size = output
        .file
        .metadata()
        .map_err(|e| format!("无法读取压缩文件大小：{e}"))?
        .len();
    drop(output);
    check_cancelled(cancelled)?;
    Ok(PreparedArchive {
        path: archive_path,
        size,
        _temporary: temporary,
    })
}

fn check_cancelled(cancelled: &watch::Receiver<bool>) -> Result<()> {
    if *cancelled.borrow() {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}

fn compression_error(error: impl std::fmt::Display, cancelled: &watch::Receiver<bool>) -> String {
    if *cancelled.borrow() {
        CANCELLED.into()
    } else {
        format!("压缩文件夹失败：{error}")
    }
}

fn safe_metadata(path: &Path) -> Result<Metadata> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("无法读取文件属性 {}：{e}", path.display()))?;
    if is_link(&metadata) {
        return Err(format!("文件夹包含链接，无法自动压缩：{}", path.display()));
    }
    if !metadata.is_dir() && !metadata.is_file() {
        return Err(format!("文件夹包含不支持的特殊文件：{}", path.display()));
    }
    Ok(metadata)
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Includes junctions and other directory reparse points, not only symlinks.
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

fn open_regular_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Do not follow a last-moment symlink replacement or block on a FIFO.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(path)
        .map_err(|e| format!("无法打开文件 {}：{e}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("无法读取文件属性：{e}"))?;
    if !metadata.is_file() || is_link(&metadata) {
        return Err(format!(
            "文件夹包含链接或特殊文件，无法自动压缩：{}",
            path.display()
        ));
    }
    Ok(file)
}

fn portable_component(name: &OsStr) -> Result<String> {
    let name = name
        .to_str()
        .ok_or("文件夹包含非 UTF-8 名称，无法跨平台压缩")?;
    let stem = name.split('.').next().unwrap_or_default().to_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        });
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.ends_with(['.', ' '])
        || reserved
        || name.chars().any(|c| {
            c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
    {
        return Err(format!("文件名不适合跨平台压缩，请先重命名：{name}"));
    }
    Ok(name.to_owned())
}

fn directory_options(metadata: &Metadata) -> SimpleFileOptions {
    file_options(
        SimpleFileOptions::default().unix_permissions(0o755),
        metadata,
    )
}

fn file_options(options: SimpleFileOptions, metadata: &Metadata) -> SimpleFileOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        options.unix_permissions(metadata.mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        options
    }
}

/// Enforce the archive cap and cancellation during data and central-directory writes.
struct ArchiveFile {
    file: File,
    cancelled: watch::Receiver<bool>,
    position: u64,
    max_size: u64,
}

impl ArchiveFile {
    fn check_cancelled(&self) -> io::Result<()> {
        // Interrupted would make write_all retry indefinitely after cancellation.
        if *self.cancelled.borrow() {
            Err(io::Error::other(CANCELLED))
        } else {
            Ok(())
        }
    }
}

impl Write for ArchiveFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.check_cancelled()?;
        if buffer.len() as u64 > self.max_size.saturating_sub(self.position) {
            return Err(io::Error::other("压缩文件超过单次传输大小限制（10 TiB）"));
        }
        let count = self.file.write(buffer)?;
        self.position += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.check_cancelled()?;
        self.file.flush()
    }
}

impl Seek for ArchiveFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.check_cancelled()?;
        self.position = self.file.seek(position)?;
        Ok(self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::ZipArchive;

    fn active() -> (watch::Sender<bool>, watch::Receiver<bool>) {
        watch::channel(false)
    }

    #[test]
    fn preserves_unicode_contents_root_and_empty_directories() {
        let source = tempfile::tempdir().unwrap();
        let root = source.path().join("资料");
        fs::create_dir_all(root.join("子目录")).unwrap();
        fs::create_dir(root.join("空文件夹")).unwrap();
        fs::write(root.join("子目录/你好.txt"), "你好，世界\n").unwrap();
        fs::write(root.join("empty.txt"), []).unwrap();
        let (_tx, rx) = active();
        let prepared = compress_directory(&root, &rx).unwrap();
        let archive_path = prepared.path().to_path_buf();
        let temporary = archive_path.parent().unwrap().to_path_buf();
        assert_eq!(prepared.size(), fs::metadata(&archive_path).unwrap().len());
        let mut archive = ZipArchive::new(File::open(&archive_path).unwrap()).unwrap();
        assert_eq!(archive.len(), 5);
        assert!(archive.by_name("资料/").unwrap().is_dir());
        assert!(archive.by_name("资料/空文件夹/").unwrap().is_dir());
        assert_eq!(archive.by_name("资料/empty.txt").unwrap().size(), 0);
        let mut text = String::new();
        let mut entry = archive.by_name("资料/子目录/你好.txt").unwrap();
        assert_eq!(entry.compression(), CompressionMethod::Deflated);
        entry.read_to_string(&mut text).unwrap();
        assert_eq!(text, "你好，世界\n");
        drop(entry);
        drop(archive);
        drop(prepared);
        assert!(!temporary.exists());
        assert!(root.join("子目录/你好.txt").exists());
    }

    #[test]
    fn empty_folder_produces_a_zip_with_its_root() {
        let source = tempfile::tempdir().unwrap();
        let root = source.path().join("empty");
        fs::create_dir(&root).unwrap();
        let (_tx, rx) = active();
        let prepared = compress_directory(&root, &rx).unwrap();
        let mut archive = ZipArchive::new(File::open(prepared.path()).unwrap()).unwrap();
        assert_eq!(archive.len(), 1);
        assert!(archive.by_name("empty/").unwrap().is_dir());
    }

    #[test]
    fn excludes_its_own_staging_directory_when_created_inside_source() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("file.txt"), b"content").unwrap();
        let temporary = tempfile::tempdir_in(source.path()).unwrap();
        let (_tx, rx) = active();
        let prepared = compress_into(source.path(), &rx, temporary, MAX_SIZE).unwrap();
        let archive = ZipArchive::new(File::open(prepared.path()).unwrap()).unwrap();
        assert_eq!(archive.len(), 2);
        assert!(archive.file_names().any(|name| name.ends_with("/file.txt")));
        assert!(!archive
            .file_names()
            .any(|name| name.ends_with("archive.zip")));
    }

    #[test]
    fn zip64_supports_more_than_65535_members() {
        let temporary = tempfile::tempdir().unwrap();
        let (_tx, rx) = active();
        let output = ArchiveFile {
            file: File::create(temporary.path().join("archive.zip")).unwrap(),
            cancelled: rx,
            position: 0,
            max_size: MAX_SIZE,
        };
        let mut writer = ZipWriter::new(output);
        let options = SimpleFileOptions::default().unix_permissions(0o755);
        for index in 0..65_536 {
            writer
                .add_directory(format!("root/{index}/"), options)
                .unwrap();
        }
        let output = writer.finish().unwrap();
        drop(output);
        let mut archive =
            ZipArchive::new(File::open(temporary.path().join("archive.zip")).unwrap()).unwrap();
        assert_eq!(archive.len(), 65_536);
        assert!(archive.by_name("root/65535/").unwrap().is_dir());
    }

    #[test]
    fn cancellation_removes_owned_temporary_directory() {
        let source = tempfile::tempdir().unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let temporary_path = temporary.path().to_path_buf();
        let (tx, rx) = active();
        tx.send(true).unwrap();
        assert_eq!(
            compress_into(source.path(), &rx, temporary, MAX_SIZE).unwrap_err(),
            CANCELLED
        );
        assert!(!temporary_path.exists());
        assert_eq!(
            compress_directory(source.path(), &rx).unwrap_err(),
            CANCELLED
        );
    }

    #[test]
    fn cancellation_interrupts_archive_writes() {
        let temporary = tempfile::tempdir().unwrap();
        let (tx, rx) = active();
        let mut file = ArchiveFile {
            file: File::create(temporary.path().join("archive.zip")).unwrap(),
            cancelled: rx,
            position: 0,
            max_size: MAX_SIZE,
        };
        file.write_all(b"before cancellation").unwrap();
        tx.send(true).unwrap();
        assert_eq!(
            file.write_all(b"after cancellation")
                .unwrap_err()
                .to_string(),
            CANCELLED
        );
        assert_eq!(file.flush().unwrap_err().to_string(), CANCELLED);
    }

    #[test]
    fn failures_remove_partial_archive() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("large.txt"), [b'a'; 256]).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let temporary_path = temporary.path().to_path_buf();
        let (_tx, rx) = active();
        let error = compress_into(source.path(), &rx, temporary, 128).unwrap_err();
        assert!(error.contains("大小限制"), "{error}");
        assert!(!temporary_path.exists());
    }

    #[test]
    fn rejects_unsafe_or_nonportable_names() {
        for name in [
            "..",
            ".",
            "a\\b",
            "a:b",
            "a/b",
            "CON.txt",
            "LPT1",
            "COM¹.txt",
            "a.",
            "a ",
            "a\nb",
        ] {
            assert!(portable_component(OsStr::new(name)).is_err(), "{name}");
        }
        assert_eq!(
            portable_component(OsStr::new("中文 🌍.txt")).unwrap(),
            "中文 🌍.txt"
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_file_and_directory_links_without_copying_their_targets() {
        use std::os::unix::fs::symlink;
        let source = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "outside selection").unwrap();
        let (_tx, rx) = active();
        for target in [
            outside.path().join("secret.txt"),
            outside.path().to_path_buf(),
        ] {
            let link = source.path().join("link");
            symlink(target, &link).unwrap();
            let temporary = tempfile::tempdir().unwrap();
            let temporary_path = temporary.path().to_path_buf();
            let error = compress_into(source.path(), &rx, temporary, MAX_SIZE).unwrap_err();
            assert!(error.contains("链接"), "{error}");
            assert!(!temporary_path.exists());
            fs::remove_file(link).unwrap();
        }
        symlink(outside.path(), source.path().join("root-link")).unwrap();
        assert!(compress_directory(&source.path().join("root-link"), &rx)
            .unwrap_err()
            .contains("链接"));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_special_files() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let source = tempfile::tempdir().unwrap();
        let fifo = CString::new(source.path().join("fifo").as_os_str().as_bytes()).unwrap();
        // A FIFO must be rejected without opening it (which would block).
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let (_tx, rx) = active();
        assert!(compress_directory(source.path(), &rx)
            .unwrap_err()
            .contains("特殊文件"));
    }
}

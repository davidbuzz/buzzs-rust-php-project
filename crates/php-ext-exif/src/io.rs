//! Stream plumbing: opening paths through the stream layer, adapting an
//! open [`Stream`] to `Read + Seek` for the `php-exif` parsers, and
//! working out the [`StreamOrigin`] exif reports in its `FILE` section.

use std::io::{self, Read, Seek, SeekFrom};

use php_exif::StreamOrigin;
use php_streams::stream::Stream;

/// `SEEK_SET` / `SEEK_CUR` / `SEEK_END`.
const SEEK_SET: i32 = 0;
const SEEK_CUR: i32 = 1;
const SEEK_END: i32 = 2;

/// A borrowed PHP stream seen as `Read + Seek`.
///
/// Reads go through `php_stream_read()` (filters, buffering), seeks through
/// `php_stream_seek()`; a failed seek becomes an `io::Error` so the parsers
/// see exactly what the C code would.
pub(crate) struct StreamIo<'a>(pub(crate) &'a mut Stream);

impl Read for StreamIo<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0
            .read(buf)
            .map_err(|_| io::Error::other("php_stream_read() failed"))
    }
}

impl Seek for StreamIo<'_> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        // The parsers' bookkeeping re-seeks to where the stream already is;
        // the C code never seeks then, so pipes must not warn. (php-streams
        // reports -1 for `popen()` pipes where C's
        // `php_stream_fopen_from_pipe()` keeps position 0.)
        if let SeekFrom::Start(n) = pos {
            let cur = u64::try_from(self.0.tell()).unwrap_or(0);
            if n == cur {
                return Ok(cur);
            }
        }
        let (off, whence) = match pos {
            SeekFrom::Start(n) => (
                i64::try_from(n).map_err(|_| io::Error::other("seek offset out of range"))?,
                SEEK_SET,
            ),
            SeekFrom::Current(n) => (n, SEEK_CUR),
            SeekFrom::End(n) => (n, SEEK_END),
        };
        if self.0.seek(off, whence) != 0 {
            return Err(io::Error::other("php_stream_seek() failed"));
        }
        u64::try_from(self.0.tell()).map_err(|_| io::Error::other("negative stream position"))
    }

    fn stream_position(&mut self) -> io::Result<u64> {
        Ok(u64::try_from(self.0.tell()).unwrap_or(0))
    }
}

/// What `exif_read_from_impl()` learns about a stream: plain files are
/// `stat()`ed through their `orig_path`, anything else is sized by seeking.
pub(crate) fn origin_of(stream: &Stream) -> StreamOrigin {
    if stream.ops_label() != "STDIO" {
        return StreamOrigin::Other;
    }
    let Some(path) = stream.orig_path.as_ref() else {
        return StreamOrigin::UnstatableFile;
    };
    match stat_path(path) {
        Some(meta) => StreamOrigin::File {
            path: path.clone(),
            mtime: mtime_of(&meta),
            size: meta.len(),
            regular: meta.file_type().is_file(),
        },
        None => StreamOrigin::UnstatableFile,
    }
}

/// `VCWD_STAT()` of a byte path.
fn stat_path(path: &[u8]) -> Option<std::fs::Metadata> {
    use std::os::unix::ffi::OsStrExt;
    std::fs::metadata(std::ffi::OsStr::from_bytes(path)).ok()
}

/// `st_mtime`.
fn mtime_of(meta: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    meta.mtime()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_streams_are_other() {
        let mut s = php_streams::memory::memory_create(php_streams::mode::TempMode::Default);
        assert_eq!(s.write(b"abcdef").ok(), Some(6));
        assert_eq!(origin_of(&s), StreamOrigin::Other);
        let mut io = StreamIo(&mut s);
        assert_eq!(io.seek(SeekFrom::Start(2)).ok(), Some(2));
        let mut buf = [0u8; 3];
        assert_eq!(io.read(&mut buf).ok(), Some(3));
        assert_eq!(&buf, b"cde");
        assert_eq!(io.seek(SeekFrom::End(-1)).ok(), Some(5));
        assert!(io.seek(SeekFrom::Current(-10)).is_err());
    }
}

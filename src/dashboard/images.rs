//! Images the person attaches to a Channels message.
//!
//! A message names each image on a line of its own at its end, `Image: ` and
//! the file's absolute path, so an agent opens it with its ordinary file
//! tools. The file is named by the SHA-256 of its bytes and its real type.

use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

pub(crate) const MAX_BYTES: usize = 10 * 1024 * 1024;
pub(crate) const MAX_PER_MESSAGE: usize = 4;
const KEEP: Duration = Duration::from_secs(30 * 24 * 60 * 60);
pub(crate) const DIRECTORY: &str = "channel-images";
const LINE_PREFIX: &str = "Image: ";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl Kind {
    const ALL: [Self; 4] = [Self::Png, Self::Jpeg, Self::Gif, Self::Webp];

    /// The type the bytes themselves start as, whatever the upload called it.
    fn of_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            Some(Self::Jpeg)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else {
            None
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Webp => "webp",
        }
    }

    pub(crate) fn content_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }
}

/// A stored image's file name: 64 lowercase hex digits, a dot and its type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Name {
    name: String,
    kind: Kind,
}

impl Name {
    pub(crate) fn parse(name: &str) -> Option<Self> {
        let (digest, extension) = name.split_once('.')?;
        let kind = Kind::ALL
            .into_iter()
            .find(|kind| kind.extension() == extension)?;
        (digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then(|| Self {
            name: name.to_owned(),
            kind,
        })
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.name
    }

    pub(crate) fn kind(&self) -> Kind {
        self.kind
    }
}

#[derive(Debug)]
pub(crate) enum SaveError {
    NotAnImage,
    Io(io::Error),
}

fn directory(state: &Path) -> PathBuf {
    state.join(DIRECTORY)
}

/// The line a message carries for this image.
pub(crate) fn line(state: &Path, name: &Name) -> String {
    format!(
        "{LINE_PREFIX}{}",
        directory(state).join(name.as_str()).display()
    )
}

/// Stores the bytes under their digest. The same image stored again keeps one
/// file and counts as new for pruning.
pub(crate) fn save(state: &Path, bytes: &[u8], now: SystemTime) -> Result<Name, SaveError> {
    let kind = Kind::of_bytes(bytes).ok_or(SaveError::NotAnImage)?;
    let name = Name {
        name: format!(
            "{}.{}",
            crate::lower_hex(&Sha256::digest(bytes)),
            kind.extension()
        ),
        kind,
    };
    let directory = directory(state);
    fs::create_dir_all(&directory).map_err(SaveError::Io)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(SaveError::Io)?;
    let target = directory.join(name.as_str());
    if let Ok(known) = OpenOptions::new().write(true).open(&target)
        && known
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() == bytes.len() as u64)
    {
        known.set_modified(now).map_err(SaveError::Io)?;
        return Ok(name);
    }
    let temporary = directory.join(format!(".{}.{}.tmp", name.as_str(), std::process::id()));
    let written = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.set_modified(now)?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temporary, &target));
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(SaveError::Io(error));
    }
    Ok(name)
}

/// The stored file and its length. A symlink planted under the name is refused.
pub(crate) fn open(state: &Path, name: &Name) -> Option<(File, u64)> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory(state).join(name.as_str()))
        .ok()?;
    let metadata = file.metadata().ok()?;
    metadata.is_file().then_some((file, metadata.len()))
}

/// Deletes images last stored more than 30 days ago, and upload leftovers.
pub(crate) fn prune(state: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(directory(state)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let ours = name
            .to_str()
            .is_some_and(|name| Name::parse(name).is_some() || name.ends_with(".tmp"));
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| now.duration_since(modified).is_ok_and(|age| age > KEEP));
        if ours && old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// A message body without the image lines at its end, and the images those
/// lines name, first to last.
pub(crate) fn split(body: &str) -> (&str, Vec<Name>) {
    let mut text = body.trim_end();
    let mut images = Vec::new();
    while let Some(start) = text.rfind('\n').map(|at| at + 1).or(Some(0)) {
        let Some(name) = image_of_line(&text[start..]) else {
            break;
        };
        images.push(name);
        text = text[..start].trim_end();
        if start == 0 {
            break;
        }
    }
    images.reverse();
    (text, images)
}

fn image_of_line(line: &str) -> Option<Name> {
    let path = Path::new(line.strip_prefix(LINE_PREFIX)?);
    if !path.is_absolute() || path.parent()?.file_name()?.to_str()? != DIRECTORY {
        return None;
    }
    Name::parse(path.file_name()?.to_str()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn state(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "namedrop-images-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn the_type_comes_from_the_bytes() {
        for (bytes, kind) in [
            (PNG, Some(Kind::Png)),
            (b"\xff\xd8\xff\xe0\0\x10JFIF".as_slice(), Some(Kind::Jpeg)),
            (b"GIF89a\x01\0\x01\0", Some(Kind::Gif)),
            (b"GIF87a\x01\0\x01\0", Some(Kind::Gif)),
            (b"RIFF\x24\0\0\0WEBPVP8 ", Some(Kind::Webp)),
            (b"RIFF\x24\0\0\0WAVEfmt ", None),
            (b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>", None),
            (b"%PDF-1.7", None),
            (b"", None),
        ] {
            assert_eq!(Kind::of_bytes(bytes), kind, "{bytes:?}");
        }
    }

    #[test]
    fn an_image_is_stored_private_under_its_digest_and_read_back() {
        let state = state("save");
        let now = SystemTime::now();
        let name = save(&state, PNG, now).unwrap();
        assert_eq!(
            name.as_str(),
            format!("{}.png", crate::lower_hex(&Sha256::digest(PNG)))
        );
        let path = state.join("channel-images").join(name.as_str());
        assert_eq!(fs::read(&path).unwrap(), PNG);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let (_, length) = open(&state, &name).unwrap();
        assert_eq!(length, PNG.len() as u64);
        assert_eq!(save(&state, PNG, now).unwrap(), name);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        assert!(matches!(
            save(&state, b"not an image", now),
            Err(SaveError::NotAnImage)
        ));
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn a_symlink_under_an_image_name_is_not_served() {
        let state = state("symlink");
        let name = save(&state, PNG, SystemTime::now()).unwrap();
        let path = state.join("channel-images").join(name.as_str());
        let secret = state.join("secret");
        fs::write(&secret, "secret").unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&secret, &path).unwrap();
        assert!(open(&state, &name).is_none());
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn only_images_older_than_thirty_days_are_pruned_and_a_new_upload_keeps_one() {
        let state = state("prune");
        let now = SystemTime::now();
        let day = Duration::from_secs(24 * 60 * 60);
        let old = save(&state, PNG, now - 31 * day).unwrap();
        let recent = save(&state, b"GIF89a recent", now - 29 * day).unwrap();
        let other = state.join("channel-images").join("notes.txt");
        fs::write(&other, "kept").unwrap();
        File::open(&other)
            .unwrap()
            .set_modified(now - 90 * day)
            .unwrap();
        prune(&state, now);
        assert!(open(&state, &old).is_none());
        assert!(open(&state, &recent).is_some());
        assert!(other.exists());

        let again = save(&state, PNG, now - 31 * day).unwrap();
        save(&state, PNG, now).unwrap();
        prune(&state, now);
        assert!(open(&state, &again).is_some());
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn image_lines_at_the_end_of_a_body_are_split_off() {
        let a = format!("{}.png", "a".repeat(64));
        let b = format!("{}.webp", "b".repeat(64));
        let line = |name: &str| format!("Image: /state/channel-images/{name}");
        let body = format!("@lead look at this\n\n{}\n{}\n", line(&a), line(&b));
        let (text, images) = split(&body);
        assert_eq!(text, "@lead look at this");
        assert_eq!(
            images.iter().map(Name::as_str).collect::<Vec<_>>(),
            [a.as_str(), b.as_str()]
        );
        assert_eq!(images[1].kind(), Kind::Webp);

        let only = line(&a);
        assert_eq!(split(&only), ("", vec![Name::parse(&a).unwrap()]));

        for unchanged in [
            "plain text".to_owned(),
            format!("{}\nthen more text", line(&a)),
            format!("text\nImage: channel-images/{a}"),
            format!("text\nImage: /state/other/{a}"),
            format!("text\nImage: /state/channel-images/{}.png", "A".repeat(64)),
            format!("text\nImage: /state/channel-images/{}.svg", "a".repeat(64)),
            format!("text\nImage: /state/channel-images/../{a}"),
            format!("text\n image: /state/channel-images/{a}"),
        ] {
            assert_eq!(
                split(&unchanged),
                (unchanged.as_str(), vec![]),
                "{unchanged}"
            );
        }
    }

    #[test]
    fn the_line_for_an_image_names_its_absolute_path_and_splits_back() {
        let state = Path::new("/home/someone/.local/state/namedrop");
        let name = Name::parse(&format!("{}.jpg", "0".repeat(64))).unwrap();
        let line = line(state, &name);
        assert_eq!(
            line,
            format!(
                "Image: /home/someone/.local/state/namedrop/channel-images/{}.jpg",
                "0".repeat(64)
            )
        );
        assert_eq!(split(&format!("hi\n\n{line}")), ("hi", vec![name]));
    }
}

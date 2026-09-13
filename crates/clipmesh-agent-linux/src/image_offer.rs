//! Produce a bounded `image/png` offer for one native image file.
//!
//! The original file URI remains the caller's source of truth. Errors from
//! this helper are soft failures, so a caller can keep that URI when a decode
//! is unavailable or unsafe. Static PNGs use a small signature and IHDR check
//! and pass through unchanged. Other supported formats go through the local
//! ImageMagick executable, with its decoder selected explicitly.

use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const MAGICK: &str = "/usr/bin/magick";
const MAX_INPUT_BYTES: u64 = 100 * 1024 * 1024;
const MAX_OUTPUT_BYTES: u64 = 100 * 1024 * 1024;
const MAX_MEMORY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PIXELS: u64 = 64_000_000;
const MAX_DIMENSION: u32 = 32_768;
const CONVERSION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImageFormat {
    Gif,
    Heic,
    Jpeg,
    Png,
    Tiff,
    Webp,
}

impl ImageFormat {
    fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match extension.as_str() {
            "gif" => Self::Gif,
            "heic" | "heif" => Self::Heic,
            "jpg" | "jpeg" => Self::Jpeg,
            "png" => Self::Png,
            "tif" | "tiff" => Self::Tiff,
            "webp" => Self::Webp,
            _ => return None,
        })
    }

    fn decoder(self) -> &'static str {
        match self {
            Self::Gif => "GIF",
            Self::Heic => "HEIC",
            Self::Jpeg => "JPEG",
            Self::Png => "PNG",
            Self::Tiff => "TIFF",
            Self::Webp => "WEBP",
        }
    }
}

/// Return one PNG clipboard offer for a supported image path.
///
/// Unknown extensions return `Ok(None)`. A recognized extension with invalid
/// data, a resource-limit failure, or a missing decoder returns an `io::Error`.
pub fn png_offer(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let Some(format) = ImageFormat::from_path(path) else {
        return Ok(None);
    };
    let bytes = read_bounded(path)?;
    if format == ImageFormat::Png {
        validate_png_header(&bytes)?;
        return Ok(Some(bytes));
    }
    convert_with_magick(format, &bytes).map(Some)
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid_data("image path is not a regular file"));
    }
    if metadata.len() > MAX_INPUT_BYTES {
        return Err(invalid_data("image input exceeds 100 MiB"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_INPUT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(invalid_data("image input exceeds 100 MiB"));
    }
    Ok(bytes)
}

fn validate_png_header(bytes: &[u8]) -> io::Result<()> {
    const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < 33 || &bytes[..8] != SIGNATURE {
        return Err(invalid_data("invalid PNG signature"));
    }
    let ihdr_len = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
    if ihdr_len != 13 || &bytes[12..16] != b"IHDR" {
        return Err(invalid_data("PNG has no valid IHDR"));
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(invalid_data("image dimensions exceed bounds"));
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PIXELS || pixels.checked_mul(4).unwrap_or(u64::MAX) > MAX_MEMORY_BYTES {
        return Err(invalid_data("image pixels exceed bounds"));
    }
    Ok(())
}

fn convert_with_magick(format: ImageFormat, bytes: &[u8]) -> io::Result<Vec<u8>> {
    let directory = tempfile::Builder::new()
        .prefix("clipmesh-image-offer-")
        .tempdir()?;
    let input_path = directory.path().join("input.image");
    let output_path = directory.path().join("output.png");
    let mut input = OpenOptions::new();
    input.write(true).create_new(true).mode(0o600);
    let mut input_file = input.open(&input_path)?;
    input_file.write_all(bytes)?;
    input_file.sync_all()?;
    drop(input_file);

    let input_arg = format!("{}:{}[0]", format.decoder(), input_path.display());
    let output_arg = format!("PNG:{}", output_path.display());
    let mut command = Command::new(MAGICK);
    command
        .args([
            "-limit", "memory", "2GiB", "-limit", "map", "2GiB", "-limit", "disk", "2GiB",
            "-limit", "area", "64MP", "-limit", "thread", "2", "-limit", "width", "32768",
            "-limit", "height", "32768",
        ])
        .arg(input_arg)
        .arg("-auto-orient")
        .arg("-strip")
        .arg(output_arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    install_limits(&mut command);
    let mut child = command.spawn()?;
    wait_bounded(&mut child)?;

    let metadata = fs::symlink_metadata(&output_path)
        .map_err(|_| invalid_data("ImageMagick produced no PNG"))?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_OUTPUT_BYTES {
        return Err(invalid_data("PNG output exceeds 100 MiB"));
    }
    let mut output_options = OpenOptions::new();
    output_options.read(true).custom_flags(libc::O_NOFOLLOW);
    let output_file = output_options.open(&output_path)?;
    let mut output = Vec::new();
    output_file
        .take(MAX_OUTPUT_BYTES + 1)
        .read_to_end(&mut output)?;
    if output.len() as u64 > MAX_OUTPUT_BYTES {
        return Err(invalid_data("PNG output exceeds 100 MiB"));
    }
    validate_png_header(&output)?;
    Ok(output)
}

fn install_limits(command: &mut Command) {
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            // ImageMagick's pixel-cache limits cap image memory and scratch
            // disk. These kernel limits cap the child address space, CPU
            // time, and file size even if a delegate ignores an ImageMagick
            // setting.
            set_limit(libc::RLIMIT_AS, 2 * 1024 * 1024 * 1024)?;
            set_limit(libc::RLIMIT_CPU, CONVERSION_TIMEOUT.as_secs() + 1)?;
            set_limit(libc::RLIMIT_FSIZE, MAX_OUTPUT_BYTES)?;
            Ok(())
        });
    }
}

fn set_limit(resource: libc::__rlimit_resource_t, value: u64) -> io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: value,
        rlim_max: value,
    };
    if unsafe { libc::setrlimit(resource, &limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn wait_bounded(child: &mut Child) -> io::Result<()> {
    let deadline = Instant::now() + CONVERSION_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err(invalid_data("ImageMagick conversion failed")),
            Ok(None) if Instant::now() >= deadline => {
                unsafe {
                    let _ = libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "ImageMagick conversion timed out",
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                unsafe {
                    let _ = libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
    }
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE_BY_ONE_PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x04\x00\x00\x00\xb5\x1c\x0c\x02\x00\x00\x00\x0bIDATx\xda\x63\x64\xf8\x0f\x00\x01\x05\x01\x01\x27\x18\xe3\x66\x00\x00\x00\x00IEND\xaeB`\x82";

    #[test]
    fn static_png_is_bounded_and_passes_through() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.png");
        fs::write(&path, ONE_BY_ONE_PNG).unwrap();
        assert_eq!(png_offer(&path).unwrap().as_deref(), Some(ONE_BY_ONE_PNG));
    }

    #[test]
    fn unsupported_file_returns_none() {
        let path = Path::new("/definitely/not/a/clipmesh-image.txt");
        assert_eq!(png_offer(path).unwrap(), None);
    }

    #[test]
    fn malformed_and_oversized_images_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let malformed = directory.path().join("bad.png");
        fs::write(&malformed, b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(png_offer(&malformed).is_err());

        let oversized = directory.path().join("large.jpg");
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&oversized)
            .unwrap();
        file.set_len(MAX_INPUT_BYTES + 1).unwrap();
        assert!(png_offer(&oversized).is_err());
    }

    #[test]
    fn synthetic_formats_convert_when_magick_is_available() {
        if !Path::new(MAGICK).is_file() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        for extension in ["gif", "jpg", "png", "tiff", "webp", "heic"] {
            let path = directory.path().join(format!("generated.{extension}"));
            let status = Command::new(MAGICK)
                .args(["-size", "2x3", "xc:tomato"])
                .arg(format!(
                    "{}:{}",
                    extension.to_ascii_uppercase(),
                    path.display()
                ))
                .status()
                .unwrap();
            assert!(status.success(), "could not create {extension} fixture");
            let offered = png_offer(&path).unwrap().expect("PNG offer");
            validate_png_header(&offered).unwrap();
            assert_eq!(
                (
                    u32::from_be_bytes(offered[16..20].try_into().unwrap()),
                    u32::from_be_bytes(offered[20..24].try_into().unwrap())
                ),
                (2, 3)
            );
        }
    }

    #[test]
    fn twenty_four_megapixel_jpeg_and_heic_convert_when_magick_is_available() {
        if !Path::new(MAGICK).is_file() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        for extension in ["jpg", "heic"] {
            let path = directory.path().join(format!("large.{extension}"));
            let status = Command::new(MAGICK)
                .args(["-size", "6000x4000", "xc:tomato", "-quality", "85"])
                .arg(format!(
                    "{}:{}",
                    extension.to_ascii_uppercase(),
                    path.display()
                ))
                .status()
                .unwrap();
            assert!(status.success(), "could not create {extension} fixture");
            let offered = png_offer(&path).unwrap().expect("PNG offer");
            validate_png_header(&offered).unwrap();
            assert_eq!(
                (
                    u32::from_be_bytes(offered[16..20].try_into().unwrap()),
                    u32::from_be_bytes(offered[20..24].try_into().unwrap())
                ),
                (6000, 4000)
            );
        }
    }
}

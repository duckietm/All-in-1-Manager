//! One asset in any of the three formats Habbo and Nitro hotels serve (.hab, .nitro, .swf), told apart by its
//! content, and turned into the .hab Octane loads. The entry point every part of the installer uses for assets.

use anyhow::{bail, Result};

use crate::{hab, nitro};

/// What kind of asset a bundle holds: a .swf is converted differently per kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetKind {
    Furniture,
    Clothes,
    Pets,
    Effects,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Hab,
    Nitro,
    Swf,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Hab => "hab",
            Format::Nitro => "nitro",
            Format::Swf => "swf",
        }
    }

    pub fn from_extension(extension: &str) -> Option<Format> {
        match extension.to_ascii_lowercase().as_str() {
            "hab" => Some(Format::Hab),
            "nitro" => Some(Format::Nitro),
            "swf" => Some(Format::Swf),
            _ => None,
        }
    }
}

pub fn is_swf(data: &[u8]) -> bool {
    data.len() > 3 && matches!(data[0], b'F' | b'C' | b'Z') && data[1] == b'W' && data[2] == b'S'
}

/// The format of a bundle by its content, or None for anything else (an error page, a damaged file).
pub fn detect(data: &[u8]) -> Option<Format> {
    if hab::is_hab(data) {
        Some(Format::Hab)
    } else if is_swf(data) {
        Some(Format::Swf)
    } else if data.len() > 2 && !data.starts_with(b"<") && nitro::read(data).is_ok() {
        Some(Format::Nitro)
    } else {
        None
    }
}

/// The bundle as a .hab named `name`: a .hab as it is, a .nitro repacked with the same files.
pub fn to_hab(name: &str, kind: AssetKind, data: &[u8]) -> Result<Vec<u8>> {
    match detect(data) {
        Some(Format::Hab) => Ok(data.to_vec()),
        Some(Format::Nitro) => Ok(hab::write(name, &nitro::read(data)?)),
        Some(Format::Swf) => bail!("Converting a {kind:?} .swf to .hab is not built yet."),
        None => bail!("{name} is not a .hab, .nitro or .swf bundle."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_format_is_told_apart_and_becomes_a_hab() {
        let files = vec![("chair.json".to_string(), b"{}".to_vec()), ("chair.png".to_string(), vec![0x89, b'P', b'N', b'G', 0, 0, 0, 0, 0])];
        let nitro_bundle = nitro::write(&files);
        let hab_bundle = hab::write("chair", &files);
        assert_eq!(detect(&nitro_bundle), Some(Format::Nitro));
        assert_eq!(detect(&hab_bundle), Some(Format::Hab));
        assert_eq!(detect(b"CWS\x0a...."), Some(Format::Swf));
        assert_eq!(detect(b"<html>404</html>"), None);
        assert_eq!(hab::read(&to_hab("chair", AssetKind::Furniture, &nitro_bundle).unwrap()).unwrap(), files);
        assert_eq!(to_hab("chair", AssetKind::Furniture, &hab_bundle).unwrap(), hab_bundle);
        assert!(to_hab("x", AssetKind::Furniture, b"<html>").is_err());
    }
}

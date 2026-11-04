//! Habbo's asset bundles, shared by every part of the installer that handles assets: the .hab bundle Octane loads,
//! Nitro's .nitro, Flash's .swf (converted to .hab), and the Habbo Classic app's .asar. No network and no server here:
//! bytes in, bytes out. (All-in-1 Converter: NitroCompiler, SWF_*_Compiler)

pub mod bundle;
pub mod hab;
pub mod nitro;
pub mod swf;
pub mod swf_pets;
pub mod swf_effects;

pub use bundle::{detect, to_hab, AssetKind, Format};

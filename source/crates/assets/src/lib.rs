//! Habbo's asset bundles, shared by every part of the installer that handles assets: the .hab bundle Octane loads,
//! Nitro's .nitro, Flash's .swf (furniture, clothes, pets and effects converted to .hab: the swf_* modules), and the
//! Habbo Classic app's .asar. No network and no server here: bytes in, bytes out. (All-in-1 Converter: NitroCompiler,
//! SWF_*_Compiler, Tools/NativeSwfExtractor.cs)

pub mod bundle;
pub mod hab;
pub mod nitro;
pub mod swf;
pub mod swf_pets;
pub mod swf_effects;
pub mod swf_clothes;
pub mod swf_furniture;
pub mod workshop;

pub use bundle::{detect, to_hab, AssetKind, Format};

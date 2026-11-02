mod build;
mod image;

pub use build::{BuildError, Keymap, Platform, Project, build};
pub use image::{GenerateError, generate, replace_app_region};

impl Keymap {
    fn expected_sha256(self) -> &'static str {
        match self {
            Self::Default => "913daf19ee8ac0a4e86f0d17b2d99a773797b1489bc77cfaae32dcad4b59aaff",
            Self::Via => "4a12b64b99c25d877def3ff59432e25217b66f9191661a8d7b890ce9f9f652e0",
        }
    }
}

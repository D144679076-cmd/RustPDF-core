//! Embedded sRGB ICC profile for PDF/A output intents.

/// Returns the sRGB IEC61966-2-1 ICC profile as a static byte slice.
///
/// This profile is embedded at compile time from `assets/sRGB_IEC61966-2-1.icc`
/// and is required by PDF/A to declare the document's colour space.
pub fn srgb_icc_profile() -> &'static [u8] {
    include_bytes!("../../assets/sRGB_IEC61966-2-1.icc")
}

//! Image XObject writing (ISO 32000-1 §8.9).

use crate::error::Result;
use crate::parser::objects::{PdfDict, PdfObject};
use crate::writer::document::PdfWriter;
use crate::writer::streams::make_flate_stream;

/// Color space for a raw pixel image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageColorSpace {
    /// Single-channel grayscale.
    DeviceGray,
    /// Three-channel RGB (interleaved R G B bytes).
    DeviceRGB,
    /// Four-channel CMYK.
    DeviceCMYK,
}

impl ImageColorSpace {
    fn pdf_name(self) -> &'static str {
        match self {
            ImageColorSpace::DeviceGray => "DeviceGray",
            ImageColorSpace::DeviceRGB => "DeviceRGB",
            ImageColorSpace::DeviceCMYK => "DeviceCMYK",
        }
    }
}

/// Uncompressed image ready to be written into a PDF.
pub struct ImageData {
    /// Raw pixel bytes (row-major, top-to-bottom).
    pub pixels: Vec<u8>,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Color model.
    pub color_space: ImageColorSpace,
    /// Bits per color component (typically 8).
    pub bits_per_component: u8,
}

/// Write a raw pixel image as a FlateDecode-compressed Image XObject.
///
/// Returns the object ID of the image XObject stream.
pub fn write_image_xobject(image: &ImageData, writer: &mut PdfWriter) -> Result<u32> {
    let mut extras = PdfDict::new();
    extras.insert("Type".to_owned(), PdfObject::Name("XObject".to_owned()));
    extras.insert("Subtype".to_owned(), PdfObject::Name("Image".to_owned()));
    extras.insert("Width".to_owned(), PdfObject::Integer(image.width as i64));
    extras.insert("Height".to_owned(), PdfObject::Integer(image.height as i64));
    extras.insert(
        "ColorSpace".to_owned(),
        PdfObject::Name(image.color_space.pdf_name().to_owned()),
    );
    extras.insert(
        "BitsPerComponent".to_owned(),
        PdfObject::Integer(image.bits_per_component as i64),
    );

    let stream = make_flate_stream(&image.pixels, extras)?;
    Ok(writer.add_object(PdfObject::Stream(Box::new(stream))))
}

/// Write a pre-encoded JPEG image as a DCTDecode Image XObject (pass-through).
///
/// The JPEG bytes are embedded as-is without re-encoding.
///
/// Returns the object ID of the image XObject stream.
pub fn write_jpeg_xobject(
    jpeg_data: &[u8],
    width: u32,
    height: u32,
    writer: &mut PdfWriter,
) -> Result<u32> {
    let mut extras = PdfDict::new();
    extras.insert("Type".to_owned(), PdfObject::Name("XObject".to_owned()));
    extras.insert("Subtype".to_owned(), PdfObject::Name("Image".to_owned()));
    extras.insert("Width".to_owned(), PdfObject::Integer(width as i64));
    extras.insert("Height".to_owned(), PdfObject::Integer(height as i64));
    // DCTDecode images are always 3-component RGB (or 1-component gray)
    // Caller is responsible for providing correct dimensions.
    extras.insert(
        "ColorSpace".to_owned(),
        PdfObject::Name("DeviceRGB".to_owned()),
    );
    extras.insert("BitsPerComponent".to_owned(), PdfObject::Integer(8));
    // DCTDecode is the JPEG filter — bytes pass through unchanged.
    extras.insert("Filter".to_owned(), PdfObject::Name("DCTDecode".to_owned()));
    extras.insert(
        "Length".to_owned(),
        PdfObject::Integer(jpeg_data.len() as i64),
    );

    // Use make_raw_stream so the JPEG bytes are not double-compressed.
    // We already set /Filter above, so skip make_flate_stream.
    let stream = crate::parser::objects::PdfStream {
        dict: extras,
        raw_data: jpeg_data.to_vec(),
    };
    Ok(writer.add_object(PdfObject::Stream(Box::new(stream))))
}

// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn gray_image() -> ImageData {
        ImageData {
            pixels: vec![128u8; 4 * 4], // 4×4 gray image
            width: 4,
            height: 4,
            color_space: ImageColorSpace::DeviceGray,
            bits_per_component: 8,
        }
    }

    #[test]
    fn image_xobject_has_required_keys() {
        let mut writer = PdfWriter::new();
        let id = write_image_xobject(&gray_image(), &mut writer).unwrap();
        let obj = writer.get_object(id).unwrap();
        if let PdfObject::Stream(s) = obj {
            assert_eq!(
                s.dict.get("Subtype"),
                Some(&PdfObject::Name("Image".to_owned()))
            );
            assert_eq!(s.dict.get("Width"), Some(&PdfObject::Integer(4)));
            assert_eq!(s.dict.get("Height"), Some(&PdfObject::Integer(4)));
            assert_eq!(
                s.dict.get("ColorSpace"),
                Some(&PdfObject::Name("DeviceGray".to_owned()))
            );
            assert!(s.dict.contains_key("Filter")); // FlateDecode
                                                    // Length matches actual compressed bytes
            if let Some(PdfObject::Integer(n)) = s.dict.get("Length") {
                assert_eq!(*n as usize, s.raw_data.len());
            } else {
                panic!("missing /Length");
            }
        } else {
            panic!("expected stream");
        }
    }

    #[test]
    fn jpeg_xobject_has_dct_filter() {
        let fake_jpeg = b"\xFF\xD8\xFF\xE0dummy jpeg bytes\xFF\xD9".to_vec();
        let mut writer = PdfWriter::new();
        let id = write_jpeg_xobject(&fake_jpeg, 10, 10, &mut writer).unwrap();
        let obj = writer.get_object(id).unwrap();
        if let PdfObject::Stream(s) = obj {
            assert_eq!(
                s.dict.get("Filter"),
                Some(&PdfObject::Name("DCTDecode".to_owned()))
            );
        } else {
            panic!("expected stream");
        }
    }

    #[test]
    fn rgb_image_color_space() {
        let image = ImageData {
            pixels: vec![255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0], // 2×2 red
            width: 2,
            height: 2,
            color_space: ImageColorSpace::DeviceRGB,
            bits_per_component: 8,
        };
        let mut writer = PdfWriter::new();
        let id = write_image_xobject(&image, &mut writer).unwrap();
        let obj = writer.get_object(id).unwrap();
        if let PdfObject::Stream(s) = obj {
            assert_eq!(
                s.dict.get("ColorSpace"),
                Some(&PdfObject::Name("DeviceRGB".to_owned()))
            );
        } else {
            panic!("expected stream");
        }
    }
}

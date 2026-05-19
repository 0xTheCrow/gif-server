pub mod db;
pub mod files;

use image::{imageops::FilterType, DynamicImage, ImageFormat};
use std::io::Cursor;
use crate::error::AppError;

pub struct GifInfo {
    pub frame_count: i32,
    pub duration_ms: i32,
    pub width:       u32,
    pub height:      u32,
}

const MAX_DIMENSION: u32 = 4096;
/// Caps to bound decode/resize cost regardless of how small the encoded
/// file is (GIF "decompression bomb" defence).
const MAX_FRAMES: i32 = 1000;
const MAX_TOTAL_PIXELS: u64 = 250_000_000;

pub fn parse_gif_info(data: &[u8]) -> Result<GifInfo, AppError> {
    let mut opts = gif::DecodeOptions::new();
    opts.set_color_output(gif::ColorOutput::RGBA);
    let mut reader = opts
        .read_info(Cursor::new(data))
        .map_err(|e| AppError::BadRequest(format!("invalid GIF: {}", e)))?;

    let width  = reader.width()  as u32;
    let height = reader.height() as u32;

    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(AppError::BadRequest(format!(
            "GIF dimensions {}x{} exceed maximum {}x{}",
            width, height, MAX_DIMENSION, MAX_DIMENSION
        )));
    }

    let mut frame_count = 0i32;
    let mut duration_ms = 0i32;

    while let Ok(Some(frame)) = reader.read_next_frame() {
        frame_count += 1;
        // Bail as soon as the cap is exceeded so a many-frames bomb can't
        // make us decode the whole stream just to count it.
        if frame_count > MAX_FRAMES {
            return Err(AppError::BadRequest(format!(
                "GIF has too many frames (maximum {})",
                MAX_FRAMES
            )));
        }
        duration_ms += frame.delay as i32 * 10; // delay in 1/100s → ms
    }

    if frame_count == 0 {
        return Err(AppError::BadRequest("GIF has no frames".into()));
    }

    let total_pixels = width as u64 * height as u64 * frame_count as u64;
    if total_pixels > MAX_TOTAL_PIXELS {
        return Err(AppError::BadRequest(format!(
            "GIF is too large to process ({} total pixels exceeds maximum {})",
            total_pixels, MAX_TOTAL_PIXELS
        )));
    }

    Ok(GifInfo { frame_count, duration_ms, width, height })
}

/// Resize a GIF to max_width, preserving aspect ratio.
/// Correctly composites partial frames using each frame's offset and disposal method.
pub fn resize_gif(data: &[u8], max_width: u32) -> Result<Vec<u8>, AppError> {
    let mut opts = gif::DecodeOptions::new();
    opts.set_color_output(gif::ColorOutput::RGBA);
    let mut reader = opts
        .read_info(Cursor::new(data))
        .map_err(|e| AppError::Internal(format!("gif decode: {}", e)))?;

    let src_width  = reader.width()  as u32;
    let src_height = reader.height() as u32;

    let (new_width, new_height) = if src_width <= max_width {
        (src_width, src_height)
    } else {
        let ratio = max_width as f32 / src_width as f32;
        (max_width, (src_height as f32 * ratio) as u32)
    };

    let canvas_len = (src_width * src_height * 4) as usize;
    let mut canvas: Vec<u8> = vec![0u8; canvas_len];
    let mut prev_canvas: Vec<u8> = vec![0u8; canvas_len];

    let mut frames: Vec<gif::Frame<'static>> = Vec::new();

    while let Ok(Some(frame)) = reader.read_next_frame() {
        let left = frame.left as u32;
        let top  = frame.top  as u32;
        let fw   = frame.width  as u32;
        let fh   = frame.height as u32;

        // Save canvas before blitting if we'll need to restore it after.
        if frame.dispose == gif::DisposalMethod::Previous {
            prev_canvas.copy_from_slice(&canvas);
        }

        // Blit this frame onto the canvas at its offset, skipping transparent pixels.
        for y in 0..fh {
            for x in 0..fw {
                let dst_x = left + x;
                let dst_y = top  + y;
                if dst_x >= src_width || dst_y >= src_height {
                    continue;
                }
                let src_i = ((y * fw + x) * 4) as usize;
                let dst_i = ((dst_y * src_width + dst_x) * 4) as usize;
                if frame.buffer[src_i + 3] > 0 {
                    canvas[dst_i..dst_i + 4].copy_from_slice(&frame.buffer[src_i..src_i + 4]);
                }
            }
        }

        // Resize the fully composited canvas for this output frame.
        let img = DynamicImage::ImageRgba8(
            image::RgbaImage::from_raw(src_width, src_height, canvas.clone())
                .ok_or_else(|| AppError::Internal("canvas buffer mismatch".into()))?,
        );
        let resized = img.resize_exact(new_width, new_height, FilterType::Lanczos3);
        let mut raw = resized.to_rgba8().into_raw();

        let mut out_frame = gif::Frame::from_rgba_speed(new_width as u16, new_height as u16, &mut raw, 10);
        out_frame.delay   = frame.delay;
        out_frame.dispose = gif::DisposalMethod::Keep;
        frames.push(out_frame);

        // Apply disposal to prepare the canvas for the next frame.
        match frame.dispose {
            gif::DisposalMethod::Background => {
                for y in 0..fh {
                    for x in 0..fw {
                        let dst_x = left + x;
                        let dst_y = top  + y;
                        if dst_x < src_width && dst_y < src_height {
                            let dst_i = ((dst_y * src_width + dst_x) * 4) as usize;
                            canvas[dst_i..dst_i + 4].copy_from_slice(&[0, 0, 0, 0]);
                        }
                    }
                }
            }
            gif::DisposalMethod::Previous => {
                canvas.copy_from_slice(&prev_canvas);
            }
            _ => {}
        }
    }

    let mut output = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut output, new_width as u16, new_height as u16, &[])
            .map_err(|e| AppError::Internal(format!("gif encode: {}", e)))?;
        encoder.set_repeat(gif::Repeat::Infinite)
            .map_err(|e| AppError::Internal(format!("gif encode: {}", e)))?;
        for frame in frames {
            encoder.write_frame(&frame)
                .map_err(|e| AppError::Internal(format!("gif encode: {}", e)))?;
        }
    }

    Ok(output)
}

/// Extract the first frame as a PNG thumbnail.
pub fn extract_thumbnail(data: &[u8], max_width: u32) -> Result<(Vec<u8>, u32, u32), AppError> {
    let mut opts = gif::DecodeOptions::new();
    opts.set_color_output(gif::ColorOutput::RGBA);
    let mut reader = opts
        .read_info(Cursor::new(data))
        .map_err(|e| AppError::Internal(format!("gif decode: {}", e)))?;

    let src_width  = reader.width()  as u32;
    let src_height = reader.height() as u32;

    let frame = reader
        .read_next_frame()
        .map_err(|e| AppError::Internal(format!("gif decode: {}", e)))?
        .ok_or_else(|| AppError::BadRequest("GIF has no frames".into()))?;

    let rgba = DynamicImage::ImageRgba8(
        image::RgbaImage::from_raw(
            frame.width as u32,
            frame.height as u32,
            frame.buffer.to_vec(),
        )
        .ok_or_else(|| AppError::Internal("frame buffer mismatch".into()))?,
    );

    let (new_width, new_height) = if src_width <= max_width {
        (src_width, src_height)
    } else {
        let ratio = max_width as f32 / src_width as f32;
        (max_width, (src_height as f32 * ratio) as u32)
    };

    let resized = rgba.resize_exact(new_width, new_height, FilterType::Lanczos3);

    let mut png_bytes = Vec::new();
    resized
        .write_to(&mut Cursor::new(&mut png_bytes), ImageFormat::Png)
        .map_err(|e| AppError::Internal(format!("png encode: {}", e)))?;

    Ok((png_bytes, new_width, new_height))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid animated GIF in memory.
    fn make_gif(width: u16, height: u16, frames: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let pixels = vec![128u8; width as usize * height as usize * 4];
        let mut enc = gif::Encoder::new(&mut out, width, height, &[]).unwrap();
        enc.set_repeat(gif::Repeat::Infinite).unwrap();
        for _ in 0..frames {
            let mut frame = gif::Frame::from_rgba_speed(width, height, &mut pixels.clone(), 1);
            frame.delay = 10;
            enc.write_frame(&frame).unwrap();
        }
        drop(enc);
        out
    }

    #[test]
    fn parse_gif_info_extracts_metadata() {
        let data = make_gif(100, 80, 3);
        let info = parse_gif_info(&data).unwrap();
        assert_eq!(info.width, 100);
        assert_eq!(info.height, 80);
        assert_eq!(info.frame_count, 3);
        assert_eq!(info.duration_ms, 300); // 3 frames × 100ms
    }

    #[test]
    fn parse_gif_info_rejects_oversized() {
        // Build a GIF header claiming a huge size without actually allocating it.
        // Manually construct a GIF89a header with large dimensions.
        let data = vec![
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, // GIF89a
            0xFF, 0xFF, // width: 65535
            0xFF, 0xFF, // height: 65535
            0x00,       // packed field (no global color table)
            0x00,       // background color index
            0x00,       // pixel aspect ratio
            0x3B,       // trailer
        ];
        let result = parse_gif_info(&data);
        assert!(matches!(result, Err(AppError::BadRequest(_))));
    }

    #[test]
    fn parse_gif_info_rejects_too_many_frames() {
        // Tiny frames so the bomb is cheap to build but still over the cap.
        let data = make_gif(2, 2, (MAX_FRAMES as usize) + 1);
        let result = parse_gif_info(&data);
        assert!(matches!(result, Err(AppError::BadRequest(_))));
    }

    #[test]
    fn parse_gif_info_accepts_frame_count_at_cap() {
        let data = make_gif(2, 2, MAX_FRAMES as usize);
        assert!(parse_gif_info(&data).is_ok());
    }

    #[test]
    fn parse_gif_info_rejects_invalid_data() {
        let result = parse_gif_info(b"not a gif");
        assert!(matches!(result, Err(AppError::BadRequest(_))));
    }

    #[test]
    fn resize_gif_reduces_width() {
        let data = make_gif(400, 300, 2);
        let resized = resize_gif(&data, 220).unwrap();
        let info = parse_gif_info(&resized).unwrap();
        assert!(info.width <= 220);
    }

    #[test]
    fn resize_gif_does_not_upscale() {
        let data = make_gif(100, 80, 1);
        let resized = resize_gif(&data, 220).unwrap();
        let info = parse_gif_info(&resized).unwrap();
        assert_eq!(info.width, 100); // smaller than max, unchanged
    }

    #[test]
    fn extract_thumbnail_produces_png() {
        let data = make_gif(300, 200, 5);
        let (png, w, h) = extract_thumbnail(&data, 220).unwrap();
        // PNG magic bytes: 0x89 0x50 0x4E 0x47
        assert_eq!(&png[0..4], &[0x89, 0x50, 0x4E, 0x47]);
        assert!(w <= 220);
        assert!(h > 0);
    }
}

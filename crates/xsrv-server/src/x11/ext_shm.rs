// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! The MIT-SHM extension, version 1.2, for images in shared memory (ShmPutImage: ZPixmap of depth
//! 8, 24 and 32, and one-bit images, which cairo sends for a mask without alpha). A segment is
//! a System V segment (ShmAttach) or a file descriptor that the client passes (ShmAttachFd).
//! Shared pixmaps, ShmGetImage, ShmCreatePixmap and ShmCreateSegment are not offered.
use wire::BYTES_PER_PIXEL;

use super::codec::Writer;
use super::dispatch::{Reply, Request};
use super::error::XError;
use super::event::Event;
use super::extensions::{event_code, first_error, MIT_SHM};
use super::resources::{pixel_bytes, Surface, PIXEL_MASK};
use super::shm::Segment;
use super::state::{ClientId, State};
use super::zpixmap;

const MAX_SEGMENTS: usize = 256;
/// One client may not use all the segments of the server.
const MAX_SEGMENTS_PER_CLIENT: usize = 32;
const MAX_SEGMENT_BYTES: usize = 256 << 20;
const FORMAT_XY_PIXMAP: u8 = 1;
const FORMAT_Z_PIXMAP: u8 = 2;
const BITMAP_DEPTH: u8 = 1;
const BITMAP_SCANLINE_PAD: usize = 32;

/// How the pixels of an image row lie in a segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    /// Whole bytes for each pixel: ZPixmap of depth 8, 24 or 32.
    Bytes { depth: u8, pixel_len: usize },
    /// One bit for each pixel, the lowest bit of a byte first. With one plane, XYPixmap and
    /// ZPixmap are the same, and the bits are the pixel values, as in the core PutImage.
    Bits,
}

impl Layout {
    fn of(depth: u8, format: u8) -> Option<Self> {
        match (format, depth) {
            (FORMAT_Z_PIXMAP | FORMAT_XY_PIXMAP, BITMAP_DEPTH) => Some(Self::Bits),
            (FORMAT_Z_PIXMAP, _) => {
                let pixel_len = zpixmap::bytes_per_pixel(depth)?;
                Some(Self::Bytes { depth, pixel_len })
            }
            _ => None,
        }
    }

    /// Bytes of one image row, padded to 32 bits.
    fn stride(self, width: usize) -> Option<usize> {
        match self {
            Self::Bytes { depth, .. } => zpixmap::stride(depth, width),
            Self::Bits => Some(width.div_ceil(BITMAP_SCANLINE_PAD) * (BITMAP_SCANLINE_PAD / 8)),
        }
    }

    /// The bytes of a row that hold `width` pixels from the column `x`: the first one and how many.
    fn span(self, x: usize, width: usize) -> (usize, usize) {
        match self {
            Self::Bytes { pixel_len, .. } => (x * pixel_len, width * pixel_len),
            Self::Bits => (x / 8, (x % 8 + width).div_ceil(8)),
        }
    }

    /// True when a row in the segment is laid out as the surface wants it.
    fn is_surface_layout(self) -> bool {
        matches!(self, Self::Bytes { pixel_len, .. } if pixel_len == BYTES_PER_PIXEL)
    }

    /// The bytes of `span(x, width)` as surface pixels.
    fn unpack(self, line: &[u8], x: usize, width: usize) -> Vec<u8> {
        match self {
            Self::Bytes { depth, .. } => zpixmap::unpack_row(depth, line, width),
            Self::Bits => (x % 8..x % 8 + width)
                .flat_map(|bit| {
                    let set = line[bit / 8] >> (bit % 8) & 1 == 1;
                    pixel_bytes(if set { PIXEL_MASK } else { 0 })
                })
                .collect(),
        }
    }
}
const COMPLETION_EVENT: u8 = 0;
const PUT_IMAGE: u8 = 3;
const ATTACH_FD: u8 = 6;
const CREATE_SEGMENT: u8 = 7;

pub struct ShmSegment {
    owner: ClientId,
    segment: Segment,
}

impl State {
    pub(super) fn shm_request(&mut self, req: &Request) -> Result<Option<Reply>, XError> {
        match req.detail {
            0 => self.shm_query_version(req).map(Some),
            1 => self.shm_attach(req).map(|()| None),
            2 => self.shm_detach(req).map(|()| None),
            3 => self.shm_put_image(req).map(|()| None),
            ATTACH_FD => self.shm_attach_fd(req).map(|()| None),
            4 | 5 | CREATE_SEGMENT => {
                self.log_once("shm unsupported".into(), || {
                    "xsrv-web: MIT-SHM GetImage, CreatePixmap and CreateSegment are not supported"
                        .into()
                });
                Err(XError::Implementation)
            }
            _ => Err(XError::Request),
        }
    }

    fn shm_query_version(&self, req: &Request) -> Result<Reply, XError> {
        req.reader().end()?;
        let mut w = Writer::new(req.endian);
        w.u16(1);
        w.u16(2);
        // The user and group ids that own the shared segments: not used by clients here.
        w.u16(0);
        w.u16(0);
        w.u8(FORMAT_Z_PIXMAP);
        // The detail byte is "shared pixmaps": false.
        Ok(Reply::new(0, w))
    }

    fn shm_error(&self, seg: u32) -> XError {
        XError::Extension(first_error(MIT_SHM), seg)
    }

    fn shm_attach(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let seg = r.u32()?;
        let shm_id = r.u32()?;
        let _read_only = r.u8()?;
        r.skip(3)?;
        r.end()?;
        self.check_new_segment(req.client, seg)?;
        let uid = self.client_uid(req.client).ok_or(XError::Access)?;
        let segment = i32::try_from(shm_id)
            .ok()
            .and_then(|id| Segment::attach(id, uid).ok())
            .ok_or(XError::Access)?;
        self.add_segment(req.client, seg, segment)
    }

    /// ShmAttachFd: the memory is the file descriptor that came with the request. No permission
    /// check is needed: the client had the descriptor, so it could read the memory itself.
    fn shm_attach_fd(&mut self, req: &Request) -> Result<(), XError> {
        // Taken before parsing, so that a malformed request still uses up its own descriptor and
        // the next AttachFd does not get it. The errors are those of Xorg: no descriptor or an
        // empty file is BadMatch, a file that cannot be used is BadAccess.
        let fd = self.take_client_fd(req.client);
        let mut r = req.reader();
        let seg = r.u32()?;
        let read_only = r.u8()?;
        r.skip(3)?;
        r.end()?;
        if read_only > 1 {
            return Err(XError::Value(read_only.into()));
        }
        self.check_new_segment(req.client, seg)?;
        let segment = Segment::from_fd(fd.ok_or(XError::Match)?).map_err(|_| XError::Access)?;
        if segment.len() == 0 {
            return Err(XError::Match);
        }
        self.add_segment(req.client, seg, segment)
    }

    /// The id must be free and the client's, and the segment limits must allow one more.
    fn check_new_segment(&self, client: ClientId, seg: u32) -> Result<(), XError> {
        if !self.owns_id(client, seg) || self.id_in_use(seg) {
            return Err(XError::IdChoice(seg));
        }
        self.check_resource_quota(client)?;
        let held = self
            .shm_segments
            .values()
            .filter(|s| s.owner == client)
            .count();
        if self.shm_segments.len() >= MAX_SEGMENTS || held >= MAX_SEGMENTS_PER_CLIENT {
            return Err(XError::Alloc);
        }
        Ok(())
    }

    fn add_segment(&mut self, client: ClientId, seg: u32, segment: Segment) -> Result<(), XError> {
        if segment.len() > MAX_SEGMENT_BYTES {
            return Err(XError::Access);
        }
        self.shm_segments.insert(
            seg,
            ShmSegment {
                owner: client,
                segment,
            },
        );
        self.owners.insert(seg, client);
        Ok(())
    }

    fn shm_detach(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let seg = r.u32()?;
        r.end()?;
        self.shm_segments.remove(&seg).ok_or(self.shm_error(seg))?;
        self.owners.remove(&seg);
        Ok(())
    }

    fn shm_put_image(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let drawable = r.u32()?;
        let gc = r.u32()?;
        let (total_width, total_height) = (usize::from(r.u16()?), usize::from(r.u16()?));
        let (src_x, src_y) = (usize::from(r.u16()?), usize::from(r.u16()?));
        let (src_width, src_height) = (usize::from(r.u16()?), usize::from(r.u16()?));
        let (dst_x, dst_y) = (i32::from(r.i16()?), i32::from(r.i16()?));
        let depth = r.u8()?;
        let format = r.u8()?;
        let send_event = r.u8()? != 0;
        r.skip(1)?;
        let seg = r.u32()?;
        let offset = r.u32()? as usize;
        r.end()?;
        let layout = Layout::of(depth, format).ok_or(XError::Match)?;
        if src_x + src_width > total_width || src_y + src_height > total_height {
            return Err(XError::Value(0));
        }
        let stride = layout.stride(total_width).ok_or(XError::Value(0))?;
        let needed = stride
            .checked_mul(total_height)
            .ok_or(XError::Value(offset as u32))?;
        let segment = &self
            .shm_segments
            .get(&seg)
            .ok_or(self.shm_error(seg))?
            .segment;
        if offset
            .checked_add(needed)
            .is_none_or(|end| end > segment.len())
        {
            return Err(XError::Value(offset as u32));
        }
        if self.surface(drawable)?.depth() != depth {
            return Err(XError::Match);
        }
        // Only the part that lands inside the drawable is read. A small window must not make the
        // server read a whole segment, one byte at a time.
        let bounds = self.surface(drawable)?.bounds();
        let left = (bounds.x0 - dst_x).clamp(0, src_width as i32) as usize;
        let top = (bounds.y0 - dst_y).clamp(0, src_height as i32) as usize;
        let right = (bounds.x1 - dst_x).clamp(0, src_width as i32) as usize;
        let bottom = (bounds.y1 - dst_y).clamp(0, src_height as i32) as usize;
        if left < right && top < bottom {
            let (src_x, src_y) = (src_x + left, src_y + top);
            let (width, height) = (right - left, bottom - top);
            let (first_byte, row_bytes) = layout.span(src_x, width);
            let first = offset + src_y * stride + first_byte;
            let out_row = width * BYTES_PER_PIXEL;
            let mut block = vec![0; out_row * height];
            // Rows of 4-byte pixels are laid out as the surface wants them, and the surface
            // masks each pixel to its depth when it writes.
            let same_layout = layout.is_surface_layout();
            for rows in row_groups(height, stride, row_bytes) {
                let start = first + rows.start * stride;
                let target = &mut block[rows.start * out_row..rows.end * out_row];
                // Rows with nothing between them go from the segment to their place at once.
                if same_layout && (rows.len() == 1 || stride == row_bytes) {
                    segment
                        .read_into(start, target)
                        .ok_or(XError::Value(offset as u32))?;
                    continue;
                }
                let span = (rows.len() - 1) * stride + row_bytes;
                let bytes = segment
                    .read(start, span)
                    .ok_or(XError::Value(offset as u32))?;
                for (line, out) in bytes.chunks(stride).zip(target.chunks_exact_mut(out_row)) {
                    let line = &line[..row_bytes];
                    if same_layout {
                        out.copy_from_slice(line);
                    } else {
                        out.copy_from_slice(&layout.unpack(line, src_x, width));
                    }
                }
            }
            let origin = (dst_x + left as i32, dst_y + top as i32);
            let size = (width as i32, height as i32);
            self.draw(drawable, gc, |surface: &mut Surface, gc| {
                surface.write_rect(origin, size, &block, gc.function, gc.plane_mask)
            })?;
        }
        if send_event {
            let event = Event::ShmCompletion {
                code: event_code(MIT_SHM, COMPLETION_EVENT).unwrap_or(0),
                drawable,
                minor: PUT_IMAGE.into(),
                major: MIT_SHM,
                seg,
                offset: offset as u32,
            };
            self.send_event_to(req.client, &event);
        }
        Ok(())
    }
}

/// The rows of an image, in groups that are each read with one call (one `pread` for a file
/// segment). Rows are grouped only when they use at least half of the stride, so that the bytes
/// between them are not much more than the image; a group holds at most `MAX_READ_BYTES`.
fn row_groups(
    height: usize,
    stride: usize,
    row_bytes: usize,
) -> impl Iterator<Item = std::ops::Range<usize>> {
    const MAX_READ_BYTES: usize = 1 << 20;
    let per_group = if row_bytes * 2 >= stride {
        (MAX_READ_BYTES / stride.max(1)).max(1)
    } else {
        1
    };
    (0..height)
        .step_by(per_group)
        .map(move |start| start..(start + per_group).min(height))
}

impl State {
    pub(super) fn forget_shm_segments(&mut self, client: ClientId) {
        let gone: Vec<u32> = self
            .shm_segments
            .iter()
            .filter(|(_, segment)| segment.owner == client)
            .map(|(seg, _)| *seg)
            .collect();
        for seg in gone {
            self.shm_segments.remove(&seg);
            self.owners.remove(&seg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_that_fill_the_stride_are_read_in_groups_of_at_most_one_mib() {
        // A full-HD row of 4-byte pixels: 136 rows fit in 1 MiB.
        let groups: Vec<_> = row_groups(1080, 7680, 7680).collect();
        assert_eq!(groups.len(), 8);
        assert_eq!(groups[0], 0..136);
        assert_eq!(groups[7], 952..1080);
    }

    #[test]
    fn a_narrow_column_of_a_wide_image_is_read_row_by_row() {
        assert_eq!(
            row_groups(3, 7680, 4).collect::<Vec<_>>(),
            [0..1, 1..2, 2..3]
        );
    }

    #[test]
    fn a_stride_wider_than_one_mib_still_makes_progress() {
        assert_eq!(
            row_groups(2, 2 << 20, 2 << 20).collect::<Vec<_>>(),
            [0..1, 1..2]
        );
    }

    #[test]
    fn a_one_bit_row_is_read_from_the_bit_of_its_first_column() {
        let layout = Layout::of(1, FORMAT_Z_PIXMAP).expect("one-bit images are taken");
        assert_eq!(layout, Layout::of(1, FORMAT_XY_PIXMAP).unwrap());
        assert_eq!(layout.stride(33), Some(8), "rows are padded to 32 bits");
        // Three pixels from column 6: the last two bits of byte 0 and the first bit of byte 1.
        assert_eq!(layout.span(6, 3), (0, 2));
        let (on, off) = (pixel_bytes(PIXEL_MASK), pixel_bytes(0));
        assert_eq!(
            layout.unpack(&[0b0100_0000, 0b0000_0001], 6, 3),
            [on, off, on].concat()
        );
        assert_eq!(layout.span(16, 8), (2, 1), "a row that starts on a byte");
    }

    #[test]
    fn other_depths_and_formats_are_refused() {
        assert_eq!(Layout::of(24, FORMAT_XY_PIXMAP), None);
        assert_eq!(Layout::of(16, FORMAT_Z_PIXMAP), None);
        assert!(Layout::of(24, FORMAT_Z_PIXMAP).unwrap().is_surface_layout());
        assert!(!Layout::of(8, FORMAT_Z_PIXMAP).unwrap().is_surface_layout());
    }
}

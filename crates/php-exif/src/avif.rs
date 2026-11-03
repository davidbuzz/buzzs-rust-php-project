//! Port of libavifinfo's streamed API (`ext/standard/libavifinfo`), as used
//! by `php_is_image_avif()` and `php_handle_avif()`.
//!
//! The PHP adapter reads at most 64 bytes at a time; a short read or a failed
//! seek poisons the stream so every later read fails.

use std::io::{Read, Seek};

use crate::stream::PhpStream;

/// Parse status (`AvifInfoInternalStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum St {
    Found,
    NotFound,
    Truncated,
    Aborted,
    Invalid,
}

/// Image features (`AvifInfoFeatures`, the fields PHP uses).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Features {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bits per channel.
    pub bit_depth: u32,
    /// Number of channels.
    pub num_channels: u32,
    has_gainmap: bool,
}

const MAX_SIZE: u32 = u32::MAX;
const MAX_NUM_BOXES: u32 = 4096;
const MAX_VALUE: u32 = 255;
const MAX_TILES: usize = 16;
const MAX_PROPS: usize = 32;
const MAX_FEATURES: usize = 8;
const MAX_READ: usize = 64;

struct Stream<'s, R: Read + Seek> {
    inner: Option<&'s mut PhpStream<R>>,
    buf: [u8; MAX_READ],
    num_read_bytes: u64,
}

impl<R: Read + Seek> Stream<'_, R> {
    /// Reads `n` (1..=64) bytes into `self.buf[..n]`.
    fn read(&mut self, n: u32) -> St {
        let n = n as usize;
        let Some(s) = self.inner.as_mut() else {
            return St::Truncated;
        };
        if n > MAX_READ || s.read(&mut self.buf[..n]) != n {
            self.inner = None;
            return St::Truncated;
        }
        self.num_read_bytes += n as u64;
        St::Found
    }

    fn skip(&mut self, n: u32) -> St {
        if n > 0 {
            if let Some(s) = self.inner.as_mut() {
                if !s.seek_cur(i128::from(n)) {
                    self.inner = None;
                }
            }
            self.num_read_bytes += u64::from(n);
        }
        St::Found
    }

    fn be(&self, off: usize, n: usize) -> u32 {
        self.buf[off..off + n]
            .iter()
            .fold(0u32, |v, &b| (v << 8) | u32::from(b))
    }
}

#[derive(Debug, Clone, Copy, Default)]
#[allow(clippy::struct_field_names)]
struct Tile {
    tile_item_id: u8,
    parent_item_id: u8,
    dimg_idx: u8,
}

#[derive(Debug, Clone, Copy, Default)]
struct Prop {
    property_index: u8,
    item_id: u8,
}

#[derive(Debug, Clone, Copy, Default)]
struct DimProp {
    property_index: u8,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Copy, Default)]
struct ChanProp {
    property_index: u8,
    bit_depth: u8,
    num_channels: u8,
}

#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools)]
struct Internal {
    has_primary_item: bool,
    has_alpha: bool,
    gainmap_property_index: u8,
    primary_item_id: u8,
    primary: Features,
    data_was_skipped: bool,
    tone_mapped_item_id: u8,
    iinf_parsed: bool,
    iref_parsed: bool,
    tiles: Vec<Tile>,
    props: Vec<Prop>,
    dim_props: Vec<DimProp>,
    chan_props: Vec<ChanProp>,
}

macro_rules! check {
    ($cond:expr, $st:expr) => {
        if !($cond) {
            return $st;
        }
    };
}
macro_rules! found {
    ($e:expr) => {{
        let s = $e;
        if s != St::Found {
            return s;
        }
    }};
}
macro_rules! not_found {
    ($e:expr) => {{
        let s = $e;
        if s != St::NotFound {
            return s;
        }
    }};
}

#[allow(clippy::cast_possible_truncation)]
fn get_item_features(f: &mut Internal, target: u32, tile_depth: u32) -> St {
    for pi in 0..f.props.len() {
        if u32::from(f.props[pi].item_id) != target {
            continue;
        }
        let property_index = f.props[pi].property_index;
        if target == u32::from(f.primary_item_id) && (f.primary.width == 0 || f.primary.height == 0)
        {
            if let Some(d) = f
                .dim_props
                .iter()
                .find(|d| d.property_index == property_index)
            {
                f.primary.width = d.width;
                f.primary.height = d.height;
                if f.primary.bit_depth != 0 && f.primary.num_channels != 0 {
                    return St::Found;
                }
            }
        }
        if f.primary.bit_depth == 0 || f.primary.num_channels == 0 {
            if let Some(c) = f
                .chan_props
                .iter()
                .find(|c| c.property_index == property_index)
            {
                f.primary.bit_depth = u32::from(c.bit_depth);
                f.primary.num_channels = u32::from(c.num_channels);
                if f.primary.width != 0 && f.primary.height != 0 {
                    return St::Found;
                }
            }
        }
    }
    if tile_depth < 3 {
        for t in 0..f.tiles.len() {
            if u32::from(f.tiles[t].parent_item_id) != target {
                continue;
            }
            let id = u32::from(f.tiles[t].tile_item_id);
            not_found!(get_item_features(f, id, tile_depth + 1));
        }
    }
    St::NotFound
}

fn get_primary_item_features(f: &mut Internal) -> St {
    check!(f.has_primary_item, St::NotFound);
    check!(
        !f.dim_props.is_empty() && !f.chan_props.is_empty(),
        St::NotFound
    );
    if f.tone_mapped_item_id != 0
        && f.tiles
            .iter()
            .any(|t| t.parent_item_id == f.tone_mapped_item_id && t.dimg_idx == 1)
    {
        f.primary.has_gainmap = true;
    }
    if !f.primary.has_gainmap
        && f.gainmap_property_index > 0
        && f.props
            .iter()
            .any(|p| p.property_index == f.gainmap_property_index)
    {
        f.primary.has_gainmap = true;
    }
    if !f.primary.has_gainmap && (!f.iinf_parsed || (f.tone_mapped_item_id != 0 && !f.iref_parsed))
    {
        return St::NotFound;
    }
    let id = u32::from(f.primary_item_id);
    found!(get_item_features(f, id, 0));
    if f.has_alpha {
        f.primary.num_channels += 1;
    }
    St::Found
}

#[derive(Debug, Clone, Copy, Default)]
struct BoxHdr {
    size: u32,
    ty: [u8; 4],
    version: u32,
    flags: u32,
    content_size: u32,
}

fn parse_box<R: Read + Seek>(
    nesting: i32,
    s: &mut Stream<'_, R>,
    remaining: u32,
    num_boxes: &mut u32,
    b: &mut BoxHdr,
) -> St {
    let mut hdr = 8u32;
    check!(hdr <= remaining, St::Invalid);
    found!(s.read(8));
    b.size = s.be(0, 4);
    b.ty.copy_from_slice(&s.buf[4..8]);
    if b.size == 1 {
        hdr += 8;
        check!(hdr <= remaining, St::Invalid);
        found!(s.read(8));
        check!(s.be(0, 4) == 0, St::Aborted);
        b.size = s.be(4, 4);
    } else if b.size == 0 {
        check!(nesting == 0, St::Invalid);
        b.size = remaining;
    }
    check!(b.size >= hdr, St::Invalid);
    check!(b.size <= remaining, St::Invalid);
    let t = &b.ty;
    let full = [
        b"meta", b"pitm", b"ipma", b"ispe", b"pixi", b"iref", b"auxC", b"iinf", b"infe",
    ]
    .contains(&t);
    if full {
        hdr += 4;
    }
    check!(b.size >= hdr, St::Invalid);
    b.content_size = b.size - hdr;
    if nesting != 0 || &b.ty != b"ftyp" {
        *num_boxes += 1;
        check!(*num_boxes < MAX_NUM_BOXES, St::Aborted);
    }
    b.version = 0;
    b.flags = 0;
    if full {
        found!(s.read(4));
        b.version = s.be(0, 1);
        b.flags = s.be(1, 3);
        let v = b.version;
        let parsable = match &b.ty {
            b"meta" | b"ispe" | b"pixi" | b"auxC" => v == 0,
            b"pitm" | b"ipma" | b"iref" | b"iinf" => v <= 1,
            b"infe" => (2..=3).contains(&v),
            _ => false,
        };
        if !parsable {
            b.ty = *b"skip";
        }
    }
    St::Found
}

#[allow(clippy::cast_possible_truncation, clippy::too_many_lines)]
fn parse_ipco<R: Read + Seek>(
    nesting: i32,
    s: &mut Stream<'_, R>,
    mut remaining: u32,
    num_boxes: &mut u32,
    f: &mut Internal,
) -> St {
    let mut box_index: u32 = 1;
    loop {
        let mut b = BoxHdr::default();
        found!(parse_box(nesting, s, remaining, num_boxes, &mut b));
        match &b.ty {
            b"ispe" => {
                check!(b.content_size >= 8, St::Invalid);
                found!(s.read(8));
                let width = s.be(0, 4);
                let height = s.be(4, 4);
                check!(width != 0 && height != 0, St::Invalid);
                if f.dim_props.len() < MAX_FEATURES && box_index <= MAX_VALUE {
                    f.dim_props.push(DimProp {
                        property_index: box_index as u8,
                        width,
                        height,
                    });
                } else {
                    f.data_was_skipped = true;
                }
                found!(s.skip(b.content_size - 8));
            }
            b"pixi" => {
                check!(b.content_size >= 1, St::Invalid);
                found!(s.read(1));
                let num_channels = s.be(0, 1);
                check!(num_channels >= 1, St::Invalid);
                check!(b.content_size > num_channels, St::Invalid);
                found!(s.read(1));
                let bit_depth = s.be(0, 1);
                check!(bit_depth >= 1, St::Invalid);
                for i in 1..num_channels {
                    found!(s.read(1));
                    check!(s.be(0, 1) == bit_depth, St::Invalid);
                    check!(i <= 32, St::Aborted);
                }
                if f.chan_props.len() < MAX_FEATURES
                    && box_index <= MAX_VALUE
                    && bit_depth <= MAX_VALUE
                    && num_channels <= MAX_VALUE
                {
                    f.chan_props.push(ChanProp {
                        property_index: box_index as u8,
                        bit_depth: bit_depth as u8,
                        num_channels: num_channels as u8,
                    });
                } else {
                    f.data_was_skipped = true;
                }
                found!(s.skip(b.content_size - (1 + num_channels)));
            }
            b"av1C" => {
                check!(b.content_size >= 3, St::Invalid);
                found!(s.read(3));
                let high = s.buf[2] & 0x40 != 0;
                let twelve = s.buf[2] & 0x20 != 0;
                let mono = s.buf[2] & 0x10 != 0;
                if twelve {
                    check!(high, St::Invalid);
                }
                if f.chan_props.len() < MAX_FEATURES && box_index <= MAX_VALUE {
                    f.chan_props.push(ChanProp {
                        property_index: box_index as u8,
                        bit_depth: if high {
                            if twelve {
                                12
                            } else {
                                10
                            }
                        } else {
                            8
                        },
                        num_channels: if mono { 1 } else { 3 },
                    });
                } else {
                    f.data_was_skipped = true;
                }
                found!(s.skip(b.content_size - 3));
            }
            b"auxC" => {
                const ALPHA: &[u8; 44] = b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha\0";
                const GAINMAP: &[u8; 29] = b"urn:com:photo:aux:hdrgainmap\0";
                let mut num_read = 0;
                if b.content_size >= 29 {
                    found!(s.read(29));
                    num_read = 29;
                    if s.buf[..29] == GAINMAP[..] {
                        if box_index <= MAX_VALUE {
                            f.gainmap_property_index = box_index as u8;
                        } else {
                            f.data_was_skipped = true;
                        }
                    } else if b.content_size >= 44 && s.buf[..29] == ALPHA[..29] {
                        found!(s.read(15));
                        num_read = 44;
                        if s.buf[..15] == ALPHA[29..] {
                            f.has_alpha = true;
                        }
                    }
                }
                found!(s.skip(b.content_size - num_read));
            }
            _ => found!(s.skip(b.content_size)),
        }
        box_index += 1;
        remaining -= b.size;
        if remaining == 0 {
            break;
        }
    }
    St::NotFound
}

#[allow(clippy::cast_possible_truncation)]
fn parse_iprp<R: Read + Seek>(
    nesting: i32,
    s: &mut Stream<'_, R>,
    mut remaining: u32,
    num_boxes: &mut u32,
    f: &mut Internal,
) -> St {
    loop {
        let mut b = BoxHdr::default();
        found!(parse_box(nesting, s, remaining, num_boxes, &mut b));
        if &b.ty == b"ipco" {
            not_found!(parse_ipco(nesting + 1, s, b.content_size, num_boxes, f));
        } else if &b.ty == b"ipma" {
            let mut num_read: u32 = 4;
            check!(b.content_size >= num_read, St::Invalid);
            found!(s.read(4));
            let entry_count = s.be(0, 4);
            let id_bytes: u32 = if b.version < 1 { 2 } else { 4 };
            let index_bytes: u32 = if b.flags & 1 != 0 { 2 } else { 1 };
            let essential_mask: u32 = if b.flags & 1 != 0 { 0x8000 } else { 0x80 };
            for entry in 0..entry_count {
                if entry as usize >= MAX_PROPS || f.props.len() >= MAX_PROPS {
                    f.data_was_skipped = true;
                    break;
                }
                num_read += id_bytes + 1;
                check!(b.content_size >= num_read, St::Invalid);
                found!(s.read(id_bytes + 1));
                let item_id = s.be(0, id_bytes as usize);
                let assoc_count = s.be(id_bytes as usize, 1);
                let mut property = 0;
                while property < assoc_count {
                    if property as usize >= MAX_PROPS || f.props.len() >= MAX_PROPS {
                        f.data_was_skipped = true;
                        break;
                    }
                    num_read += index_bytes;
                    check!(b.content_size >= num_read, St::Invalid);
                    found!(s.read(index_bytes));
                    let value = s.be(0, index_bytes as usize);
                    let property_index = value & !essential_mask;
                    if property_index <= MAX_VALUE && item_id <= MAX_VALUE {
                        f.props.push(Prop {
                            property_index: property_index as u8,
                            item_id: item_id as u8,
                        });
                    } else {
                        f.data_was_skipped = true;
                    }
                    property += 1;
                }
                if property < assoc_count {
                    break;
                }
            }
            not_found!(get_primary_item_features(f));
            found!(s.skip(b.content_size - num_read));
        } else {
            found!(s.skip(b.content_size));
        }
        remaining -= b.size;
        if remaining == 0 {
            break;
        }
    }
    St::NotFound
}

#[allow(clippy::cast_possible_truncation)]
fn parse_iref<R: Read + Seek>(
    nesting: i32,
    s: &mut Stream<'_, R>,
    mut remaining: u32,
    num_boxes: &mut u32,
    f: &mut Internal,
) -> St {
    f.iref_parsed = true;
    while remaining > 0 {
        let mut b = BoxHdr::default();
        found!(parse_box(nesting, s, remaining, num_boxes, &mut b));
        if &b.ty == b"dimg" {
            let id_bytes: u32 = if b.version == 0 { 2 } else { 4 };
            let mut num_read = id_bytes + 2;
            check!(b.content_size >= num_read, St::Invalid);
            found!(s.read(id_bytes + 2));
            let from = s.be(0, id_bytes as usize);
            let count = s.be(id_bytes as usize, 2);
            for i in 0..count {
                if i as usize >= MAX_TILES {
                    f.data_was_skipped = true;
                    break;
                }
                num_read += id_bytes;
                check!(b.content_size >= num_read, St::Invalid);
                found!(s.read(id_bytes));
                let to = s.be(0, id_bytes as usize);
                if from <= MAX_VALUE && to <= MAX_VALUE && f.tiles.len() < MAX_TILES {
                    f.tiles.push(Tile {
                        tile_item_id: to as u8,
                        parent_item_id: from as u8,
                        dimg_idx: i as u8,
                    });
                } else {
                    f.data_was_skipped = true;
                }
            }
            not_found!(get_primary_item_features(f));
            found!(s.skip(b.content_size - num_read));
        } else {
            found!(s.skip(b.content_size));
        }
        remaining -= b.size;
    }
    St::NotFound
}

#[allow(clippy::cast_possible_truncation)]
fn parse_iinf<R: Read + Seek>(
    nesting: i32,
    s: &mut Stream<'_, R>,
    mut remaining: u32,
    version: u32,
    num_boxes: &mut u32,
    f: &mut Internal,
) -> St {
    f.iinf_parsed = true;
    let count_bytes: u32 = if version == 0 { 2 } else { 4 };
    check!(count_bytes <= remaining, St::Invalid);
    found!(s.read(count_bytes));
    remaining -= count_bytes;
    let entry_count = s.be(0, count_bytes as usize);
    let mut i: u32 = 0;
    while i < entry_count {
        let mut b = BoxHdr::default();
        found!(parse_box(nesting, s, remaining, num_boxes, &mut b));
        if &b.ty == b"infe" {
            let id_bytes: u32 = if b.version == 2 { 2 } else { 4 };
            check!(id_bytes + 2 + 4 <= b.content_size, St::Invalid);
            found!(s.read(id_bytes));
            let item_id = s.be(0, id_bytes as usize);
            found!(s.skip(2));
            found!(s.read(4));
            if &s.buf[..4] == b"tmap" {
                if item_id <= MAX_VALUE {
                    f.tone_mapped_item_id = item_id as u8;
                } else {
                    f.data_was_skipped = true;
                }
            }
            found!(s.skip(b.content_size - (id_bytes + 2 + 4)));
        } else {
            found!(s.skip(b.content_size));
        }
        remaining -= b.size;
        if remaining == 0 {
            break;
        }
        i += 1;
    }
    St::NotFound
}

#[allow(clippy::cast_possible_truncation)]
fn parse_meta<R: Read + Seek>(
    nesting: i32,
    s: &mut Stream<'_, R>,
    mut remaining: u32,
    num_boxes: &mut u32,
    f: &mut Internal,
) -> St {
    loop {
        let mut b = BoxHdr::default();
        found!(parse_box(nesting, s, remaining, num_boxes, &mut b));
        match &b.ty {
            b"pitm" => {
                let id_bytes: u32 = if b.version == 0 { 2 } else { 4 };
                check!(id_bytes <= remaining, St::Invalid);
                found!(s.read(id_bytes));
                let id = s.be(0, id_bytes as usize);
                check!(id <= MAX_VALUE, St::Aborted);
                f.has_primary_item = true;
                f.primary_item_id = id as u8;
                found!(s.skip(b.content_size.wrapping_sub(id_bytes)));
            }
            b"iprp" => not_found!(parse_iprp(nesting + 1, s, b.content_size, num_boxes, f)),
            b"iref" => not_found!(parse_iref(nesting + 1, s, b.content_size, num_boxes, f)),
            b"iinf" => not_found!(parse_iinf(
                nesting + 1,
                s,
                b.content_size,
                b.version,
                num_boxes,
                f
            )),
            _ => found!(s.skip(b.content_size)),
        }
        remaining -= b.size;
        if remaining == 0 {
            break;
        }
    }
    if f.data_was_skipped {
        St::Aborted
    } else {
        St::Invalid
    }
}

fn parse_ftyp<R: Read + Seek>(s: &mut Stream<'_, R>) -> St {
    let mut b = BoxHdr::default();
    let mut num_boxes = 0;
    found!(parse_box(0, s, MAX_SIZE, &mut num_boxes, &mut b));
    check!(&b.ty == b"ftyp", St::Invalid);
    check!(b.content_size >= 8, St::Invalid);
    let mut i: u32 = 0;
    while i + 4 <= b.content_size {
        found!(s.read(4));
        if i != 4 {
            if &s.buf[..4] == b"avif" || &s.buf[..4] == b"avis" {
                found!(s.skip(b.content_size - (i + 4)));
                return St::Found;
            }
            check!(i <= 32 * 4, St::Aborted);
        }
        i += 4;
    }
    St::Invalid
}

fn parse_file<R: Read + Seek>(s: &mut Stream<'_, R>, num_boxes: &mut u32, f: &mut Internal) -> St {
    loop {
        let mut b = BoxHdr::default();
        found!(parse_box(0, s, MAX_SIZE, num_boxes, &mut b));
        if &b.ty == b"meta" {
            return parse_meta(1, s, b.content_size, num_boxes, f);
        }
        found!(s.skip(b.content_size));
    }
}

/// `php_is_image_avif()`: `AvifInfoIdentifyStream() == kAvifInfoOk`.
pub(crate) fn is_avif<R: Read + Seek>(stream: &mut PhpStream<R>) -> bool {
    let mut s = Stream {
        inner: Some(stream),
        buf: [0; MAX_READ],
        num_read_bytes: 0,
    };
    parse_ftyp(&mut s) == St::Found
}

/// `AvifInfoGetFeaturesStream()`; `Some` on `kAvifInfoOk`.
pub(crate) fn get_features<R: Read + Seek>(stream: &mut PhpStream<R>) -> Option<Features> {
    let mut s = Stream {
        inner: Some(stream),
        buf: [0; MAX_READ],
        num_read_bytes: 0,
    };
    let mut num_boxes = 0;
    let mut f = Internal::default();
    if parse_file(&mut s, &mut num_boxes, &mut f) == St::Found {
        Some(f.primary)
    } else {
        None
    }
}

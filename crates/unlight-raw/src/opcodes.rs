//! DNG opcode list parsing. LibRaw exposes these lists as raw bytes and only applies
//! them through the Adobe DNG SDK, so phone DNGs need them handled here.

use serde::{Deserialize, Serialize};

const OPCODE_WARP_RECTILINEAR: u32 = 1;
const OPCODE_FIX_VIGNETTE_RADIAL: u32 = 3;
const OPCODE_GAIN_MAP: u32 = 9;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Opcodes {
    pub list1: Vec<Opcode>,
    pub list2: Vec<Opcode>,
    pub list3: Vec<Opcode>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Opcode {
    GainMap(GainMap),
    WarpRectilinear(WarpRectilinear),
    FixVignetteRadial(FixVignetteRadial),
    Unsupported { id: u32, optional: bool },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GainMap {
    pub top: u32,
    pub left: u32,
    pub bottom: u32,
    pub right: u32,
    pub plane: u32,
    pub planes: u32,
    pub row_pitch: u32,
    pub col_pitch: u32,
    pub points_v: u32,
    pub points_h: u32,
    pub spacing_v: f64,
    pub spacing_h: f64,
    pub origin_v: f64,
    pub origin_h: f64,
    pub map_planes: u32,
    pub gains: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WarpPlane {
    pub radial: [f64; 4],
    pub tangential: [f64; 2],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WarpRectilinear {
    pub planes: Vec<WarpPlane>,
    pub center_x: f64,
    pub center_y: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FixVignetteRadial {
    pub k: [f64; 5],
    pub center_x: f64,
    pub center_y: f64,
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.bytes.split_at_checked(count)?;
        self.bytes = tail;
        Some(head)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }
}

/// Parses an opcode list. Returns `None` if the list is truncated or malformed.
pub fn parse_opcode_list(bytes: &[u8]) -> Option<Vec<Opcode>> {
    let mut reader = Reader { bytes };
    let count = reader.u32()?;
    let mut opcodes = Vec::new();
    for _ in 0..count {
        let id = reader.u32()?;
        let _dng_version = reader.u32()?;
        let flags = reader.u32()?;
        let size = reader.u32()? as usize;
        let body = reader.take(size)?;
        let mut body = Reader { bytes: body };
        opcodes.push(match id {
            OPCODE_GAIN_MAP => Opcode::GainMap(parse_gain_map(&mut body)?),
            OPCODE_WARP_RECTILINEAR => Opcode::WarpRectilinear(parse_warp(&mut body)?),
            OPCODE_FIX_VIGNETTE_RADIAL => Opcode::FixVignetteRadial(parse_vignette(&mut body)?),
            _ => Opcode::Unsupported { id, optional: flags & 1 != 0 },
        });
    }
    Some(opcodes)
}

fn parse_gain_map(body: &mut Reader) -> Option<GainMap> {
    let top = body.u32()?;
    let left = body.u32()?;
    let bottom = body.u32()?;
    let right = body.u32()?;
    let plane = body.u32()?;
    let planes = body.u32()?;
    let row_pitch = body.u32()?;
    let col_pitch = body.u32()?;
    let points_v = body.u32()?;
    let points_h = body.u32()?;
    let spacing_v = body.f64()?;
    let spacing_h = body.f64()?;
    let origin_v = body.f64()?;
    let origin_h = body.f64()?;
    let map_planes = body.u32()?;
    let count = (points_v as usize)
        .checked_mul(points_h as usize)?
        .checked_mul(map_planes as usize)?;
    let gains = (0..count).map(|_| body.f32()).collect::<Option<Vec<_>>>()?;
    Some(GainMap {
        top,
        left,
        bottom,
        right,
        plane,
        planes,
        row_pitch,
        col_pitch,
        points_v,
        points_h,
        spacing_v,
        spacing_h,
        origin_v,
        origin_h,
        map_planes,
        gains,
    })
}

fn parse_warp(body: &mut Reader) -> Option<WarpRectilinear> {
    let plane_count = body.u32()?;
    let mut planes = Vec::new();
    for _ in 0..plane_count {
        planes.push(WarpPlane {
            radial: [body.f64()?, body.f64()?, body.f64()?, body.f64()?],
            tangential: [body.f64()?, body.f64()?],
        });
    }
    Some(WarpRectilinear { planes, center_x: body.f64()?, center_y: body.f64()? })
}

fn parse_vignette(body: &mut Reader) -> Option<FixVignetteRadial> {
    Some(FixVignetteRadial {
        k: [body.f64()?, body.f64()?, body.f64()?, body.f64()?, body.f64()?],
        center_x: body.f64()?,
        center_y: body.f64()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opcode(id: u32, flags: u32, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(id.to_be_bytes());
        out.extend(0x0103_0000u32.to_be_bytes());
        out.extend(flags.to_be_bytes());
        out.extend((body.len() as u32).to_be_bytes());
        out.extend(body);
        out
    }

    fn list(ops: &[Vec<u8>]) -> Vec<u8> {
        let mut out = (ops.len() as u32).to_be_bytes().to_vec();
        ops.iter().for_each(|op| out.extend(op));
        out
    }

    fn gain_map_body(points_v: u32, points_h: u32) -> Vec<u8> {
        let mut body = Vec::new();
        for value in [0u32, 1, 100, 200, 0, 1, 2, 2, points_v, points_h] {
            body.extend(value.to_be_bytes());
        }
        for value in [0.5f64, 0.25, 0.0, 0.125] {
            body.extend(value.to_be_bytes());
        }
        body.extend(1u32.to_be_bytes());
        for index in 0..points_v * points_h {
            body.extend((1.0f32 + index as f32).to_be_bytes());
        }
        body
    }

    #[test]
    fn parses_gain_map() {
        let bytes = list(&[opcode(9, 1, &gain_map_body(2, 3))]);
        let ops = parse_opcode_list(&bytes).unwrap();
        let [Opcode::GainMap(map)] = ops.as_slice() else { panic!("expected one gain map") };
        assert_eq!((map.top, map.left, map.bottom, map.right), (0, 1, 100, 200));
        assert_eq!((map.row_pitch, map.col_pitch), (2, 2));
        assert_eq!((map.points_v, map.points_h, map.map_planes), (2, 3, 1));
        assert_eq!((map.spacing_v, map.spacing_h, map.origin_h), (0.5, 0.25, 0.125));
        assert_eq!(map.gains, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn parses_warp_and_vignette() {
        let mut warp = 1u32.to_be_bytes().to_vec();
        for value in [1.0f64, -0.1, 0.05, 0.0, 0.001, -0.002, 0.5, 0.5] {
            warp.extend(value.to_be_bytes());
        }
        let mut vignette = Vec::new();
        for value in [0.1f64, 0.2, 0.3, 0.4, 0.5, 0.25, 0.75] {
            vignette.extend(value.to_be_bytes());
        }
        let bytes = list(&[opcode(1, 0, &warp), opcode(3, 0, &vignette)]);
        let ops = parse_opcode_list(&bytes).unwrap();
        assert_eq!(
            ops[0],
            Opcode::WarpRectilinear(WarpRectilinear {
                planes: vec![WarpPlane { radial: [1.0, -0.1, 0.05, 0.0], tangential: [0.001, -0.002] }],
                center_x: 0.5,
                center_y: 0.5,
            })
        );
        assert_eq!(
            ops[1],
            Opcode::FixVignetteRadial(FixVignetteRadial {
                k: [0.1, 0.2, 0.3, 0.4, 0.5],
                center_x: 0.25,
                center_y: 0.75,
            })
        );
    }

    #[test]
    fn keeps_unknown_opcodes_and_their_optional_flag() {
        let bytes = list(&[opcode(7, 1, &[0; 12]), opcode(8, 0, &[])]);
        let ops = parse_opcode_list(&bytes).unwrap();
        assert_eq!(ops[0], Opcode::Unsupported { id: 7, optional: true });
        assert_eq!(ops[1], Opcode::Unsupported { id: 8, optional: false });
    }

    #[test]
    fn rejects_truncated_lists() {
        let bytes = list(&[opcode(9, 1, &gain_map_body(4, 4))]);
        assert_eq!(parse_opcode_list(&bytes[..bytes.len() - 3]), None);
        let mut body = gain_map_body(2, 2);
        body.truncate(body.len() - 1);
        assert_eq!(parse_opcode_list(&list(&[opcode(9, 1, &body)])), None);
    }

    #[test]
    fn rejects_gain_map_with_overflowing_dimensions() {
        let mut body = gain_map_body(1, 1);
        body[32..40].copy_from_slice(&[0xff; 8]);
        body[72..76].copy_from_slice(&[0xff; 4]);
        assert_eq!(parse_opcode_list(&list(&[opcode(9, 1, &body)])), None);
    }
}

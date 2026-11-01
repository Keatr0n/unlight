use std::{env, path::Path};
use unlight_raw::{Opcode, Preview, load, load_preview};

fn summarize(list: &[Opcode]) -> String {
    let names: Vec<String> = list
        .iter()
        .map(|op| match op {
            Opcode::GainMap(m) => format!(
                "GainMap(plane {} {}x{} rect {},{},{},{} pitch {}x{})",
                m.plane, m.points_h, m.points_v, m.top, m.left, m.bottom, m.right, m.row_pitch, m.col_pitch
            ),
            Opcode::WarpRectilinear(w) => format!("WarpRectilinear({} planes)", w.planes.len()),
            Opcode::FixVignetteRadial(_) => "FixVignetteRadial".to_string(),
            Opcode::Unsupported { id, optional } => format!("Unsupported({id}, optional {optional})"),
        })
        .collect();
    names.join(", ")
}

fn main() {
    for path in env::args().skip(1) {
        let path = Path::new(&path);
        let name = path.file_name().unwrap().to_string_lossy();
        match load(path) {
            Ok(raw) => {
                let black: Vec<f32> = raw.black[0].iter().take(2).chain(raw.black[1].iter().take(2)).copied().collect();
                let min = raw.samples.iter().min().unwrap();
                let max = raw.samples.iter().max().unwrap();
                println!(
                    "{name}: {} {}x{} ({:.1} MP) period {} black {black:?} white {} data {min}..{max} as-shot {:?} baseline {} dng {} flip {} iso {}",
                    raw.metadata.camera(),
                    raw.width,
                    raw.height,
                    (raw.width * raw.height) as f32 / 1e6,
                    raw.cfa.period,
                    raw.white,
                    raw.color.as_shot_multipliers,
                    raw.color.baseline_exposure,
                    raw.color.dng.as_ref().map_or(0, |d| d.illuminants.len()),
                    raw.metadata.orientation,
                    raw.metadata.iso,
                );
                for (index, list) in [&raw.opcodes.list1, &raw.opcodes.list2, &raw.opcodes.list3].iter().enumerate() {
                    if !list.is_empty() {
                        println!("  OpcodeList{}: {}", index + 1, summarize(list));
                    }
                }
            }
            Err(error) => println!("{name}: ERROR {error}"),
        }
        match load_preview(path) {
            Ok(Preview::Jpeg(bytes)) => println!("  preview: JPEG {} KB", bytes.len() / 1024),
            Ok(Preview::Rgb8 { width, height, .. }) => println!("  preview: RGB {width}x{height}"),
            Err(error) => println!("  preview: ERROR {error}"),
        }
    }
}

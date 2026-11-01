pub type Vec3 = [f64; 3];
pub type Mat3 = [[f64; 3]; 3];

pub const IDENTITY: Mat3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

pub const WHITE_D50: Vec3 = [0.964_22, 1.0, 0.825_21];
pub const WHITE_D65: Vec3 = [0.950_47, 1.0, 1.088_83];

pub const XYZ_D50_TO_PROPHOTO: Mat3 = [
    [1.345_943_3, -0.255_607_5, -0.051_111_8],
    [-0.544_598_9, 1.508_167_3, 0.020_535_1],
    [0.0, 0.0, 1.211_812_8],
];

pub const XYZ_D65_TO_SRGB: Mat3 = [
    [3.240_454_2, -1.537_138_5, -0.498_531_4],
    [-0.969_266_0, 1.876_010_8, 0.041_556_0],
    [0.055_643_4, -0.204_025_9, 1.057_225_2],
];

const BRADFORD: Mat3 = [
    [0.895_1, 0.266_4, -0.161_4],
    [-0.750_2, 1.713_5, 0.036_7],
    [0.038_9, -0.068_5, 1.029_6],
];

pub fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    std::array::from_fn(|row| {
        std::array::from_fn(|col| (0..3).map(|k| a[row][k] * b[k][col]).sum())
    })
}

pub fn apply(m: &Mat3, v: &Vec3) -> Vec3 {
    std::array::from_fn(|row| (0..3).map(|k| m[row][k] * v[k]).sum())
}

pub fn diagonal(v: &Vec3) -> Mat3 {
    [[v[0], 0.0, 0.0], [0.0, v[1], 0.0], [0.0, 0.0, v[2]]]
}

pub fn scale(m: &Mat3, factor: f64) -> Mat3 {
    m.map(|row| row.map(|value| value * factor))
}

pub fn lerp(a: &Mat3, b: &Mat3, t: f64) -> Mat3 {
    std::array::from_fn(|row| {
        std::array::from_fn(|col| a[row][col] + (b[row][col] - a[row][col]) * t)
    })
}

pub fn inverse(m: &Mat3) -> Option<Mat3> {
    let cofactor = |r0: usize, c0: usize, r1: usize, c1: usize| {
        m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]
    };
    let adjugate = [
        [cofactor(1, 1, 2, 2), -cofactor(0, 1, 2, 2), cofactor(0, 1, 1, 2)],
        [-cofactor(1, 0, 2, 2), cofactor(0, 0, 2, 2), -cofactor(0, 0, 1, 2)],
        [cofactor(1, 0, 2, 1), -cofactor(0, 0, 2, 1), cofactor(0, 0, 1, 1)],
    ];
    let determinant = (0..3).map(|col| m[0][col] * adjugate[col][0]).sum::<f64>();
    (determinant.abs() > 1e-12).then(|| scale(&adjugate, 1.0 / determinant))
}

pub fn xyz_to_xy(xyz: &Vec3) -> [f64; 2] {
    let sum = xyz[0] + xyz[1] + xyz[2];
    [xyz[0] / sum, xyz[1] / sum]
}

pub fn xy_to_xyz(xy: &[f64; 2]) -> Vec3 {
    [xy[0] / xy[1], 1.0, (1.0 - xy[0] - xy[1]) / xy[1]]
}

/// Bradford adaptation from one white to another.
pub fn bradford(from: &Vec3, to: &Vec3) -> Mat3 {
    let cone_from = apply(&BRADFORD, from);
    let cone_to = apply(&BRADFORD, to);
    let gains = std::array::from_fn(|i| cone_to[i] / cone_from[i]);
    let inverse_bradford = inverse(&BRADFORD).expect("Bradford matrix is invertible");
    mul(&inverse_bradford, &mul(&diagonal(&gains), &BRADFORD))
}

pub fn prophoto_to_srgb() -> Mat3 {
    let prophoto_to_xyz = inverse(&XYZ_D50_TO_PROPHOTO).expect("ProPhoto matrix is invertible");
    let to_d65 = bradford(&WHITE_D50, &WHITE_D65);
    mul(&XYZ_D65_TO_SRGB, &mul(&to_d65, &prophoto_to_xyz))
}

/// McCamy's approximation, accurate over the range white balance uses.
pub fn cct_from_xy(xy: &[f64; 2]) -> f64 {
    let n = (xy[0] - 0.332_0) / (0.185_8 - xy[1]);
    449.0 * n.powi(3) + 3525.0 * n.powi(2) + 6823.3 * n + 5520.33
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(a: &Vec3, b: &Vec3, tolerance: f64) {
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() < tolerance, "{a:?} != {b:?}");
        }
    }

    #[test]
    fn inverse_round_trips() {
        let m = [[2.0, 1.0, 0.5], [0.0, 3.0, 1.0], [1.0, 0.0, 4.0]];
        let product = mul(&m, &inverse(&m).unwrap());
        for row in 0..3 {
            assert_close(&product[row], &IDENTITY[row], 1e-12);
        }
    }

    #[test]
    fn singular_matrix_has_no_inverse() {
        assert_eq!(inverse(&[[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [0.0, 0.0, 1.0]]), None);
    }

    #[test]
    fn bradford_maps_source_white_to_target_white() {
        let adapted = apply(&bradford(&WHITE_D65, &WHITE_D50), &WHITE_D65);
        assert_close(&adapted, &WHITE_D50, 1e-9);
    }

    #[test]
    fn prophoto_white_maps_to_srgb_white() {
        let white = apply(&prophoto_to_srgb(), &[1.0, 1.0, 1.0]);
        assert_close(&white, &[1.0, 1.0, 1.0], 2e-3);
    }

    #[test]
    fn cct_recovers_daylight_whites() {
        let d65 = cct_from_xy(&xyz_to_xy(&WHITE_D65));
        let d50 = cct_from_xy(&xyz_to_xy(&WHITE_D50));
        assert!((d65 - 6504.0).abs() < 30.0, "D65 gave {d65}");
        assert!((d50 - 5003.0).abs() < 30.0, "D50 gave {d50}");
    }
}

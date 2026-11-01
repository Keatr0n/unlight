//! The little linear algebra the geometry needs. Everything is small and dense.

use unlight_color::matrix::{Mat3, Vec3, apply};

pub(crate) fn transpose(m: &Mat3) -> Mat3 {
    std::array::from_fn(|row| std::array::from_fn(|column| m[column][row]))
}

pub(crate) fn dot(a: &Vec3, b: &Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: &Vec3, b: &Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

pub(crate) fn normalized(v: &Vec3) -> Vec3 {
    let length = dot(v, v).sqrt().max(1e-300);
    v.map(|value| value / length)
}

/// Rotation by `axis`'s length in radians around its direction (Rodrigues' formula).
pub(crate) fn rotation_from_vector(axis: &Vec3) -> Mat3 {
    let angle = dot(axis, axis).sqrt();
    if angle < 1e-12 {
        return [
            [1.0, -axis[2], axis[1]],
            [axis[2], 1.0, -axis[0]],
            [-axis[1], axis[0], 1.0],
        ];
    }
    let [x, y, z] = axis.map(|value| value / angle);
    let (sin, cos) = angle.sin_cos();
    let rest = 1.0 - cos;
    [
        [cos + x * x * rest, x * y * rest - z * sin, x * z * rest + y * sin],
        [y * x * rest + z * sin, cos + y * y * rest, y * z * rest - x * sin],
        [z * x * rest - y * sin, z * y * rest + x * sin, cos + z * z * rest],
    ]
}

/// Rotation about the y axis, which is yaw in the panorama's world.
pub(crate) fn yaw_rotation(angle: f64) -> Mat3 {
    rotation_from_vector(&[0.0, angle, 0.0])
}

/// Eigenvalues and eigenvectors of a symmetric matrix by Jacobi rotations, largest value first.
/// Each returned vector is a column of the eigenvector matrix, stored as a row of the result.
pub(crate) fn symmetric_eigen<const N: usize>(mut matrix: [[f64; N]; N]) -> ([f64; N], [[f64; N]; N]) {
    let mut vectors: [[f64; N]; N] = std::array::from_fn(|row| std::array::from_fn(|column| f64::from(u8::from(row == column))));
    for _ in 0..64 {
        let off: f64 = (0..N).flat_map(|p| (p + 1..N).map(move |q| (p, q))).map(|(p, q)| matrix[p][q].powi(2)).sum();
        if off < 1e-24 {
            break;
        }
        for p in 0..N {
            for q in p + 1..N {
                if matrix[p][q].abs() < 1e-300 {
                    continue;
                }
                let theta = (matrix[q][q] - matrix[p][p]) / (2.0 * matrix[p][q]);
                let tangent = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let tangent = if theta == 0.0 { 1.0 } else { tangent };
                let cosine = 1.0 / (tangent * tangent + 1.0).sqrt();
                let sine = tangent * cosine;
                for row in &mut matrix {
                    let (a, b) = (row[p], row[q]);
                    row[p] = cosine * a - sine * b;
                    row[q] = sine * a + cosine * b;
                }
                let (upper, lower) = matrix.split_at_mut(q);
                for (first, second) in upper[p].iter_mut().zip(&mut lower[0]) {
                    let (a, b) = (*first, *second);
                    *first = cosine * a - sine * b;
                    *second = sine * a + cosine * b;
                }
                for row in &mut vectors {
                    let (a, b) = (row[p], row[q]);
                    row[p] = cosine * a - sine * b;
                    row[q] = sine * a + cosine * b;
                }
            }
        }
    }
    let mut order: [usize; N] = std::array::from_fn(|index| index);
    order.sort_by(|&a, &b| matrix[b][b].total_cmp(&matrix[a][a]));
    let values = order.map(|index| matrix[index][index]);
    let rows = order.map(|index| std::array::from_fn(|row| vectors[row][index]));
    (values, rows)
}

/// The rotation that best carries each `from` onto its `to` (Horn 1987).
pub(crate) fn best_rotation(pairs: impl Iterator<Item = (Vec3, Vec3)>) -> Mat3 {
    let mut s = [[0.0f64; 3]; 3];
    for (from, to) in pairs {
        for row in 0..3 {
            for column in 0..3 {
                s[row][column] += from[row] * to[column];
            }
        }
    }
    let [[xx, xy, xz], [yx, yy, yz], [zx, zy, zz]] = s;
    let n = [
        [xx + yy + zz, yz - zy, zx - xz, xy - yx],
        [yz - zy, xx - yy - zz, xy + yx, zx + xz],
        [zx - xz, xy + yx, -xx + yy - zz, yz + zy],
        [xy - yx, zx + xz, yz + zy, -xx - yy + zz],
    ];
    let (_, vectors) = symmetric_eigen(n);
    let [w, x, y, z] = vectors[0];
    [
        [w * w + x * x - y * y - z * z, 2.0 * (x * y - w * z), 2.0 * (x * z + w * y)],
        [2.0 * (x * y + w * z), w * w - x * x + y * y - z * z, 2.0 * (y * z - w * x)],
        [2.0 * (x * z - w * y), 2.0 * (y * z + w * x), w * w - x * x - y * y + z * z],
    ]
}

/// Solves a positive definite system in place by Cholesky. `None` if it isn't one.
pub(crate) fn solve_spd(matrix: &mut [f64], vector: &mut [f64]) -> Option<()> {
    let size = vector.len();
    for column in 0..size {
        let mut diagonal = matrix[column * size + column];
        for k in 0..column {
            diagonal -= matrix[column * size + k].powi(2);
        }
        if diagonal <= 0.0 {
            return None;
        }
        let diagonal = diagonal.sqrt();
        matrix[column * size + column] = diagonal;
        for row in column + 1..size {
            let mut value = matrix[row * size + column];
            for k in 0..column {
                value -= matrix[row * size + k] * matrix[column * size + k];
            }
            matrix[row * size + column] = value / diagonal;
        }
    }
    for row in 0..size {
        let mut value = vector[row];
        for k in 0..row {
            value -= matrix[row * size + k] * vector[k];
        }
        vector[row] = value / matrix[row * size + row];
    }
    for row in (0..size).rev() {
        let mut value = vector[row];
        for k in row + 1..size {
            value -= matrix[k * size + row] * vector[k];
        }
        vector[row] = value / matrix[row * size + row];
    }
    Some(())
}

/// Gaussian elimination with partial pivoting. `None` if the matrix is singular.
pub(crate) fn solve_general<const N: usize>(mut matrix: [[f64; N]; N], mut vector: [f64; N]) -> Option<[f64; N]> {
    for column in 0..N {
        let pivot = (column..N).max_by(|&a, &b| matrix[a][column].abs().total_cmp(&matrix[b][column].abs()))?;
        if matrix[pivot][column].abs() < 1e-12 {
            return None;
        }
        matrix.swap(column, pivot);
        vector.swap(column, pivot);
        for row in column + 1..N {
            let factor = matrix[row][column] / matrix[column][column];
            let (above, below) = matrix.split_at_mut(row);
            for (value, pivot) in below[0].iter_mut().zip(&above[column]).skip(column) {
                *value -= factor * pivot;
            }
            vector[row] -= factor * vector[column];
        }
    }
    for row in (0..N).rev() {
        let known: f64 = (row + 1..N).map(|k| matrix[row][k] * vector[k]).sum();
        vector[row] = (vector[row] - known) / matrix[row][row];
    }
    Some(vector)
}

/// Angle between two rotations in radians.
#[cfg(test)]
pub(crate) fn rotation_distance(a: &Mat3, b: &Mat3) -> f64 {
    let relative = unlight_color::matrix::mul(a, &transpose(b));
    let trace = relative[0][0] + relative[1][1] + relative[2][2];
    ((trace - 1.0) * 0.5).clamp(-1.0, 1.0).acos()
}

#[cfg(test)]
pub(crate) fn is_rotation(m: &Mat3, tolerance: f64) -> bool {
    let product = unlight_color::matrix::mul(m, &transpose(m));
    let identity = unlight_color::matrix::IDENTITY;
    (0..3).all(|row| (0..3).all(|column| (product[row][column] - identity[row][column]).abs() < tolerance))
}

/// Where a homography sends a point.
pub(crate) fn apply_homography(m: &Mat3, point: [f64; 2]) -> [f64; 2] {
    let [x, y, w] = apply(m, &[point[0], point[1], 1.0]);
    [x / w, y / w]
}

#[cfg(test)]
mod tests {
    use super::*;
    use unlight_color::matrix::mul;

    #[test]
    fn rotations_compose_and_stay_rotations() {
        let a = rotation_from_vector(&[0.3, -0.2, 0.5]);
        let b = rotation_from_vector(&[-0.1, 0.4, 0.2]);
        assert!(is_rotation(&mul(&a, &b), 1e-12));
        assert!((rotation_distance(&a, &a)).abs() < 1e-6);
    }

    #[test]
    fn a_tiny_rotation_is_the_first_order_one() {
        let small = rotation_from_vector(&[1e-13, 0.0, 0.0]);
        assert!(is_rotation(&small, 1e-12));
    }

    #[test]
    fn eigenvectors_recover_a_known_system() {
        let basis = rotation_from_vector(&[0.4, 0.2, -0.7]);
        let values = [9.0, 4.0, 1.0];
        let mut matrix = [[0.0; 3]; 3];
        for row in 0..3 {
            for column in 0..3 {
                matrix[row][column] = (0..3).map(|k| basis[row][k] * values[k] * basis[column][k]).sum();
            }
        }
        let (found, vectors) = symmetric_eigen(matrix);
        for (expected, got) in values.iter().zip(&found) {
            assert!((expected - got).abs() < 1e-9, "{found:?}");
        }
        let first: [f64; 3] = std::array::from_fn(|row| basis[row][0]);
        assert!(dot(&first, &vectors[0]).abs() > 1.0 - 1e-9);
    }

    #[test]
    fn horn_recovers_a_rotation_from_rays() {
        let truth = rotation_from_vector(&[0.2, -0.5, 0.1]);
        let rays = [[0.0, 0.0, 1.0], [0.4, 0.1, 1.0], [-0.3, 0.5, 1.0], [0.2, -0.6, 1.0]];
        let found = best_rotation(rays.iter().map(|ray| (normalized(ray), apply(&truth, &normalized(ray)))));
        assert!(rotation_distance(&found, &truth) < 1e-6);
    }

    #[test]
    fn cholesky_solves_a_small_system() {
        let mut matrix = [4.0, 1.0, 1.0, 3.0];
        let mut vector = [1.0, 2.0];
        solve_spd(&mut matrix, &mut vector).unwrap();
        assert!((vector[0] - 1.0 / 11.0).abs() < 1e-12);
        assert!((vector[1] - 7.0 / 11.0).abs() < 1e-12);
    }

    #[test]
    fn a_singular_system_is_refused() {
        assert!(solve_general([[1.0, 2.0], [2.0, 4.0]], [1.0, 2.0]).is_none());
        assert!(solve_spd(&mut [1.0, 1.0, 1.0, 1.0], &mut [1.0, 1.0]).is_none());
    }
}

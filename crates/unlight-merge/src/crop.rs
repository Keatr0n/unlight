//! Auto Crop: the largest rectangle with no empty pixels in it.

/// Finds the largest rectangle of `true` cells, as `[left, top, width, height]`. Treats each row
/// as the floor of a histogram of how far `true` runs upward, which takes one pass (Bradski's
/// largest rectangle under a histogram).
pub fn largest_rectangle(mask: &[bool], width: usize, height: usize) -> Option<[usize; 4]> {
    let mut heights = vec![0usize; width + 1];
    let mut best = (0usize, [0usize; 4]);
    let mut stack: Vec<usize> = Vec::with_capacity(width + 1);
    for row in 0..height {
        for column in 0..width {
            heights[column] = if mask[row * width + column] { heights[column] + 1 } else { 0 };
        }
        stack.clear();
        for column in 0..=width {
            while let Some(&top) = stack.last() {
                if heights[top] <= heights[column] {
                    break;
                }
                stack.pop();
                let tall = heights[top];
                let left = stack.last().map_or(0, |value| value + 1);
                let wide = column - left;
                if tall * wide > best.0 {
                    best = (tall * wide, [left, row + 1 - tall, wide, tall]);
                }
            }
            stack.push(column);
        }
    }
    (best.0 > 0).then_some(best.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brute_force(mask: &[bool], width: usize, height: usize) -> usize {
        let mut best = 0;
        for top in 0..height {
            for left in 0..width {
                for bottom in top..height {
                    for right in left..width {
                        let clean = (top..=bottom).all(|y| (left..=right).all(|x| mask[y * width + x]));
                        if clean {
                            best = best.max((bottom - top + 1) * (right - left + 1));
                        }
                    }
                }
            }
        }
        best
    }

    #[test]
    fn matches_brute_force_on_random_masks() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..200 {
            let (width, height) = (9, 7);
            let mask: Vec<bool> = (0..width * height)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    state % 100 < 75
                })
                .collect();
            let found = largest_rectangle(&mask, width, height);
            let area = found.map_or(0, |[_, _, w, h]| w * h);
            assert_eq!(area, brute_force(&mask, width, height));
            if let Some([left, top, w, h]) = found {
                for y in top..top + h {
                    for x in left..left + w {
                        assert!(mask[y * width + x], "{found:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn an_empty_mask_has_no_rectangle() {
        assert_eq!(largest_rectangle(&[false; 12], 4, 3), None);
    }

    #[test]
    fn a_full_mask_is_its_own_rectangle() {
        assert_eq!(largest_rectangle(&[true; 12], 4, 3), Some([0, 0, 4, 3]));
    }
}

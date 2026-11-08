//! int8 向量量化与余弦相似度（纯函数，无依赖）。
//!
//! 量化：`round(x * 127 / max|x|)`。余弦对「逐向量统一缩放」不变，因此用**每向量最大
//! 绝对值**归一化不会损失方向信息——这是 int8 暴力检索能用的前提。
//!
//! 字节序：`Vec<u8>` 存 `i8` 的二进制补码（SQLite BLOB 只有无符号字节），比较时
//! `as i8` 还原。字节循环写成标量形式，交给编译器自动向量化（不引入 SIMD crate）。

/// 把 f32 向量量化成 int8（以 `u8` 字节保存）。全零向量返回全零。
pub fn quantize_i8(vector: &[f32]) -> Vec<u8> {
    let max = vector
        .iter()
        .filter(|v| v.is_finite())
        .fold(0.0f32, |acc, v| acc.max(v.abs()));
    if max <= f32::EPSILON {
        return vec![0u8; vector.len()];
    }
    let scale = 127.0 / max;
    vector
        .iter()
        .map(|value| {
            let scaled = if value.is_finite() {
                value * scale
            } else {
                0.0
            };
            #[allow(clippy::cast_possible_truncation)]
            let clamped = scaled.round().clamp(-127.0, 127.0) as i8;
            clamped as u8
        })
        .collect()
}

/// 反量化（取证/调试用；检索路径不需要）。
pub fn dequantize_i8(bytes: &[u8]) -> Vec<f32> {
    bytes.iter().map(|&b| f32::from(b as i8)).collect()
}

/// int8 余弦相似度，取值范围 `[-1, 1]`；任一向量为零向量时返回 0。
///
/// 长度不一致时按较短的算（调用方应先用 `dim` 过滤脏数据）。
pub fn cosine_i8(a: &[u8], b: &[u8]) -> f32 {
    let len = a.len().min(b.len());
    let mut dot: i32 = 0;
    let mut norm_a: i32 = 0;
    let mut norm_b: i32 = 0;
    for i in 0..len {
        let x = i32::from(a[i] as i8);
        let y = i32::from(b[i] as i8);
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0 || norm_b == 0 {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let denom = (norm_a as f32).sqrt() * (norm_b as f32).sqrt();
    #[allow(clippy::cast_precision_loss)]
    let value = dot as f32 / denom;
    value.clamp(-1.0, 1.0)
}

/// f32 余弦（参考实现，用于量化误差对比与测试）。
pub fn cosine_f32(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for i in 0..len {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    if norm_a <= f32::EPSILON || norm_b <= f32::EPSILON {
        return 0.0;
    }
    (dot / (norm_a.sqrt() * norm_b.sqrt())).clamp(-1.0, 1.0)
}

/// L2 归一化（原地）。
pub fn normalize_l2(vector: &mut [f32]) {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 1e-12 {
        for value in vector.iter_mut() {
            *value /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantize_zero_vector_stays_zero() {
        assert_eq!(quantize_i8(&[0.0, 0.0, 0.0]), vec![0, 0, 0]);
        assert!(cosine_i8(&[0, 0], &[1, 1]).abs() < 1e-6);
    }

    #[test]
    fn quantize_scales_by_max_abs() {
        let quantized = quantize_i8(&[0.5, -1.0, 0.0]);
        let restored = dequantize_i8(&quantized);
        assert!((restored[1] + 127.0).abs() < 1e-6, "最大值应映射到 -127");
        assert!((restored[0] - 63.5).abs() < 1.0);
        assert!((restored[2]).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_identical_direction_is_one() {
        let a = quantize_i8(&[1.0, 2.0, 3.0]);
        let b = quantize_i8(&[2.0, 4.0, 6.0]);
        assert!((cosine_i8(&a, &b) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn cosine_of_orthogonal_is_zero() {
        let a = quantize_i8(&[1.0, 0.0]);
        let b = quantize_i8(&[0.0, 1.0]);
        assert!(cosine_i8(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_opposite_is_minus_one() {
        let a = quantize_i8(&[1.0, 2.0]);
        let b = quantize_i8(&[-1.0, -2.0]);
        assert!((cosine_i8(&a, &b) + 1.0).abs() < 1e-4);
    }

    #[test]
    fn length_mismatch_uses_shorter_prefix() {
        let a = quantize_i8(&[1.0, 0.0]);
        let b = quantize_i8(&[1.0, 0.0, 5.0, 5.0]);
        assert!((cosine_i8(&a, &b) - 1.0).abs() < 1e-4);
    }

    /// 确定性 LCG：不引入 rand 依赖，测试可复现。
    struct Lcg(u64);

    impl Lcg {
        fn next_f32(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695);
            let bits = (self.0 >> 33) as u32;
            (bits as f32 / u32::MAX as f32) * 2.0 - 1.0
        }

        fn vector(&mut self, dim: usize) -> Vec<f32> {
            (0..dim).map(|_| self.next_f32()).collect()
        }
    }

    fn spearman(xs: &[f32], ys: &[f32]) -> f32 {
        let n = xs.len();
        let rank = |values: &[f32]| -> Vec<f32> {
            let mut idx: Vec<usize> = (0..values.len()).collect();
            idx.sort_by(|a, b| values[*a].partial_cmp(&values[*b]).unwrap());
            let mut ranks = vec![0.0f32; values.len()];
            for (position, &i) in idx.iter().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                {
                    ranks[i] = position as f32 + 1.0;
                }
            }
            ranks
        };
        let rx = rank(xs);
        let ry = rank(ys);
        let d2: f32 = rx
            .iter()
            .zip(ry.iter())
            .map(|(a, b)| (a - b) * (a - b))
            .sum();
        #[allow(clippy::cast_precision_loss)]
        let n_f = n as f32;
        1.0 - (6.0 * d2) / (n_f * (n_f * n_f - 1.0))
    }

    #[test]
    fn int8_ranking_matches_f32_ranking() {
        let mut rng = Lcg(0x5EED);
        let dim = 64;
        let pairs = 1000;
        let mut f32_scores = Vec::with_capacity(pairs);
        let mut i8_scores = Vec::with_capacity(pairs);

        // 一条固定 query，1000 条随机候选（更接近检索场景）
        let query = rng.vector(dim);
        let query_i8 = quantize_i8(&query);
        for _ in 0..pairs {
            let candidate = rng.vector(dim);
            let candidate_i8 = quantize_i8(&candidate);
            f32_scores.push(cosine_f32(&query, &candidate));
            i8_scores.push(cosine_i8(&query_i8, &candidate_i8));
        }

        let rho = spearman(&f32_scores, &i8_scores);
        assert!(rho > 0.99, "量化后排序应保持一致，Spearman={rho}");
    }

    #[test]
    fn normalize_makes_unit_norm() {
        let mut v = vec![3.0, 4.0];
        normalize_l2(&mut v);
        assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-6);
    }
}

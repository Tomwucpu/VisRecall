//! 暴力 int8 向量索引（`BruteForceInt8`）的集成测试。

use std::sync::Arc;

use visrecall_domain::providers::VectorIndex;
use visrecall_domain::vector_math::{cosine_f32, quantize_i8};
use visrecall_domain::{AppError, ItemKind, NewItem};
use visrecall_library::{BruteForceInt8, Library};

const MODEL_ID: &str = "bge-small-zh-v1.5-int8";
const DIM: u32 = 8;

fn fixture() -> (tempfile::TempDir, Arc<Library>, BruteForceInt8) {
    let dir = tempfile::tempdir().unwrap();
    let library = Arc::new(Library::open(dir.path().to_path_buf()).unwrap());
    library.init_embedding_space(MODEL_ID, DIM).unwrap();
    let index = BruteForceInt8::new(Arc::clone(&library));
    (dir, library, index)
}

fn insert(library: &Library, hash: &str) -> i64 {
    let item = library
        .insert_item(NewItem {
            kind: ItemKind::Image,
            captured_at: 1,
            source_app: None,
            source_title: None,
            source_url: None,
            width: None,
            height: None,
            char_count: None,
            original_path: format!("originals/images/{hash}.png"),
            content_hash: hash.into(),
        })
        .unwrap();
    library.item_rowid(&item.id).unwrap().unwrap()
}

/// 确定性 LCG（不引入 rand 依赖）。
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

#[test]
fn add_then_search_ranks_by_cosine() {
    let (_dir, library, index) = fixture();
    let rowids: Vec<i64> = (0..3).map(|i| insert(&library, &format!("h{i}"))).collect();

    let vectors = [
        vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        vec![0.0f32, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        vec![0.7f32, 0.7, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    ];
    for (rowid, vector) in rowids.iter().zip(vectors.iter()) {
        index.add(*rowid, &quantize_i8(vector)).unwrap();
    }

    let query = vec![1.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let hits = index.search(&query, 3, None).unwrap();
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].0, rowids[0], "与 query 完全同向的应排第一");
    assert_eq!(hits[1].0, rowids[2], "部分同向的排第二");
    assert_eq!(hits[2].0, rowids[1], "正交的排最后");
    assert!(hits[0].1 > hits[1].1 && hits[1].1 > hits[2].1);
}

#[test]
fn search_respects_top_k_and_candidates() {
    let (_dir, library, index) = fixture();
    let mut rng = Lcg(7);
    let mut rowids = Vec::new();
    for i in 0..10 {
        let rowid = insert(&library, &format!("h{i}"));
        index
            .add(rowid, &quantize_i8(&rng.vector(DIM as usize)))
            .unwrap();
        rowids.push(rowid);
    }

    let query = rng.vector(DIM as usize);
    let top3 = index.search(&query, 3, None).unwrap();
    assert_eq!(top3.len(), 3);

    let subset = [rowids[2], rowids[5]];
    let filtered = index.search(&query, 3, Some(&subset)).unwrap();
    assert!(filtered.len() <= 2);
    for (rowid, _) in &filtered {
        assert!(subset.contains(rowid), "候选集之外的行不应出现：{rowid}");
    }

    assert!(index.search(&query, 3, Some(&[])).unwrap().is_empty());
    assert!(index.search(&query, 0, None).unwrap().is_empty());
}

/// 排名（1 起，同分取平均名次）。
fn ranks(values: &[f32]) -> Vec<f32> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|a, b| values[*a].partial_cmp(&values[*b]).unwrap());
    let mut out = vec![0.0f32; values.len()];
    for (position, &idx) in order.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        {
            out[idx] = position as f32 + 1.0;
        }
    }
    out
}

fn spearman(a: &[f32], b: &[f32]) -> f32 {
    let ra = ranks(a);
    let rb = ranks(b);
    let d2: f32 = ra
        .iter()
        .zip(rb.iter())
        .map(|(x, y)| (x - y) * (x - y))
        .sum();
    #[allow(clippy::cast_precision_loss)]
    let n = a.len() as f32;
    1.0 - (6.0 * d2) / (n * (n * n - 1.0))
}

#[test]
fn ranking_matches_brute_force_f32_reference() {
    let (_dir, library, index) = fixture();
    let mut rng = Lcg(11);
    let dim = 32usize;
    library.force_embedding_space(MODEL_ID, dim as u32).unwrap();
    let mut reference: Vec<(i64, Vec<f32>)> = Vec::new();
    for i in 0..100 {
        let rowid = insert(&library, &format!("h{i}"));
        let vector = rng.vector(dim);
        index.add(rowid, &quantize_i8(&vector)).unwrap();
        reference.push((rowid, vector));
    }

    let query = rng.vector(dim);
    let f32_scores: Vec<f32> = reference
        .iter()
        .map(|(_, vector)| cosine_f32(&query, vector))
        .collect();

    let hits = index.search(&query, 100, None).unwrap();
    // 按 reference 顺序取回 i8 分数（未出现 = 0，正常不会发生，因为 100 条全返回）
    let i8_scores: Vec<f32> = reference
        .iter()
        .map(|(rowid, _)| {
            hits.iter()
                .find(|(id, _)| id == rowid)
                .map(|(_, score)| *score)
                .unwrap_or(0.0)
        })
        .collect();

    // 门禁口径（design §9.3）：量化后排序不劣化，Spearman 一致性 > 0.99
    let rho = spearman(&f32_scores, &i8_scores);
    assert!(rho > 0.99, "Spearman={rho}");

    // Top-10 集合一致，且第一名相同（近邻同分的相邻位次允许互换）
    let mut expected_top: Vec<i64> = {
        let mut scored: Vec<(i64, f32)> = reference
            .iter()
            .map(|(rowid, vector)| (*rowid, cosine_f32(&query, vector)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
        scored.into_iter().take(10).map(|(id, _)| id).collect()
    };
    let mut actual_top: Vec<i64> = hits.into_iter().take(10).map(|(id, _)| id).collect();
    assert_eq!(actual_top[0], expected_top[0], "Top-1 必须一致");
    expected_top.sort_unstable();
    actual_top.sort_unstable();
    assert_eq!(actual_top, expected_top, "Top-10 集合必须一致");
}

#[test]
fn rows_with_mismatched_dim_are_skipped() {
    let (_dir, library, index) = fixture();
    let good = insert(&library, "good");
    let bad = insert(&library, "bad");
    index.add(good, &quantize_i8(&[1.0; 8])).unwrap();
    // 绕过校验直接塞一条维度不对的脏数据
    library.vector_upsert(bad, &[1, 2, 3], 3, MODEL_ID).unwrap();

    let hits = index.search(&[1.0; 8], 10, None).unwrap();
    assert_eq!(hits.len(), 1, "维度不匹配的行必须被跳过");
    assert_eq!(hits[0].0, good);
}

#[test]
fn add_rejects_mismatched_model_and_dim() {
    let (_dir, library, index) = fixture();
    let rowid = insert(&library, "h0");

    let err = index
        .add_with_model(rowid, &quantize_i8(&[1.0; 8]), "bge-base-zh-v1.5-int8")
        .expect_err("模型不一致必须报错");
    assert!(matches!(err, AppError::InvalidInput(_)), "err={err:?}");

    let err = index
        .add_with_model(rowid, &[1, 2, 3], MODEL_ID)
        .expect_err("维度不一致必须报错");
    assert!(matches!(err, AppError::InvalidInput(_)), "err={err:?}");

    assert!(matches!(
        index.add(rowid, &[]),
        Err(AppError::InvalidInput(_))
    ));
}

#[test]
fn add_before_pinning_space_fails() {
    let dir = tempfile::tempdir().unwrap();
    let library = Arc::new(Library::open(dir.path().to_path_buf()).unwrap());
    let index = BruteForceInt8::new(Arc::clone(&library));
    let rowid = insert(&library, "h0");

    assert!(matches!(
        index.add(rowid, &[1, 2]),
        Err(AppError::InvalidInput(_))
    ));
}

#[test]
fn remove_drops_the_vector() {
    let (_dir, library, index) = fixture();
    let rowid = insert(&library, "h0");
    index.add(rowid, &quantize_i8(&[1.0; 8])).unwrap();
    assert_eq!(index.search(&[1.0; 8], 5, None).unwrap().len(), 1);

    index.remove(rowid).unwrap();
    assert!(index.search(&[1.0; 8], 5, None).unwrap().is_empty());
    assert_eq!(library.vector_count().unwrap(), 0);
}

#[test]
fn search_is_deterministic_for_ties() {
    let (_dir, library, index) = fixture();
    let first = insert(&library, "h0");
    let second = insert(&library, "h1");
    // 两条完全相同的向量 → 同分，顺序必须稳定（rowid 升序）
    index.add(first, &quantize_i8(&[1.0; 8])).unwrap();
    index.add(second, &quantize_i8(&[1.0; 8])).unwrap();

    let a = index.search(&[1.0; 8], 5, None).unwrap();
    let b = index.search(&[1.0; 8], 5, None).unwrap();
    assert_eq!(a, b);
    assert_eq!(a[0].0, first);
    assert_eq!(a[1].0, second);
}

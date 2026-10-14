//! A 3D map of the index: every file is a point, and files with similar meaning sit together.
//!
//! Each file's vector (the mean of its passages) is squeezed from 256 dimensions into 3: PCA first,
//! then t-SNE, which keeps neighbours together. That takes a while for thousands of files, so the
//! layout is kept (on disk too) and changed as little as possible:
//!
//! - after a restart, the saved layout is there right away;
//! - new files are placed next to their most similar files, and every other point stays put;
//! - only once much has changed is everything laid out again, starting from the current layout
//!   and turned to match it, so the overall picture stays where it was.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Instant;

use nalgebra::{DMatrix, Matrix3};
use parking_lot::Mutex;
use serde_json::json;

use crate::config::EMBED_DIM;
use crate::governor::set_background_priority;
use crate::store::{Store, dot};

const MAX_POINTS: usize = 6000; // t-SNE gets slow beyond this: bigger indexes show a sample
const REFRESH_S: f64 = 10.0; // while files are being indexed, the map catches up at most this often
const TSNE_MIN: usize = 150; // below this, PCA alone keeps neighbours together better
const REDO_SHARE: f64 = 0.3; // lay everything out again once this share is new or only roughly placed
const NEIGHBOURS: usize = 8; // a new file goes where its most similar files are
const MAGIC: &[u8; 8] = b"QEMAP001";

/// One unit vector per file: the mean of its chunk vectors (ids sorted).
pub fn per_file(file_ids: &[i64], vectors: &[f32]) -> (Vec<i64>, Vec<f32>) {
    let mut order: Vec<usize> = (0..file_ids.len()).collect();
    order.sort_by_key(|&i| file_ids[i]);
    let (mut ids, mut means): (Vec<i64>, Vec<f32>) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < order.len() {
        let id = file_ids[order[i]];
        let mut sum = vec![0f32; EMBED_DIM];
        let mut n = 0;
        while i < order.len() && file_ids[order[i]] == id {
            let row = &vectors[order[i] * EMBED_DIM..(order[i] + 1) * EMBED_DIM];
            sum.iter_mut().zip(row).for_each(|(s, v)| *s += v);
            n += 1;
            i += 1;
        }
        sum.iter_mut().for_each(|s| *s /= n as f32);
        let norm = dot(&sum, &sum).sqrt().max(1e-9);
        sum.iter_mut().for_each(|s| *s /= norm);
        ids.push(id);
        means.extend(sum);
    }
    (ids, means)
}

/// The rows' principal components, at most `k` of them: the data turned so its widest spread
/// comes first (what PCA does), as rows of k values.
fn pca(vectors: &[f32], n: usize, k: usize) -> Vec<f64> {
    let d = EMBED_DIM;
    let mut mean = vec![0f64; d];
    for r in 0..n {
        for j in 0..d {
            mean[j] += vectors[r * d + j] as f64;
        }
    }
    mean.iter_mut().for_each(|m| *m /= n as f64);
    let x = DMatrix::<f64>::from_fn(n, d, |r, j| vectors[r * d + j] as f64 - mean[j]);
    let cov = x.transpose() * &x;
    let eig = cov.symmetric_eigen();
    let mut order: Vec<usize> = (0..d).collect();
    order.sort_by(|&a, &b| eig.eigenvalues[b].total_cmp(&eig.eigenvalues[a]));
    let mut out = vec![0f64; n * k];
    for (c, &e) in order.iter().take(k).enumerate() {
        let v = eig.eigenvectors.column(e);
        // The sign PCA picks (the largest value positive), so the same data gives the same layout.
        let flip = if v.iter().fold(0f64, |m, &x| if x.abs() > m.abs() { x } else { m }) < 0.0 { -1.0 } else { 1.0 };
        for r in 0..n {
            out[r * k + c] = flip * x.row(r).iter().zip(v.iter()).map(|(a, b)| a * b).sum::<f64>();
        }
    }
    out
}

/// Coordinates of roughly [-1, 1]^3, where nearby points have similar vectors. `init`: where the
/// points are now, so that a new layout keeps the picture (it is also turned to match it).
pub fn to_3d(vectors: &[f32], init: Option<&[f32]>) -> Vec<f32> {
    let n = vectors.len() / EMBED_DIM;
    if n == 0 {
        return Vec::new();
    }
    let k = 1.max(50.min(n.saturating_sub(1)).min(EMBED_DIM));
    let reduced = pca(vectors, n, k);
    let mut coords = vec![0f64; n * 3];
    if n >= TSNE_MIN {
        let rows: Vec<&[f64]> = reduced.chunks(k).collect();
        let start: Vec<f64> = match init {
            // t-SNE starts best from a tiny layout, as from its own PCA one.
            Some(init) => {
                let first: Vec<f64> = (0..n).map(|i| init[i * 3] as f64).collect();
                let std = std_dev(&first);
                let means: Vec<f64> = (0..3).map(|c| (0..n).map(|i| init[i * 3 + c] as f64).sum::<f64>() / n as f64).collect();
                (0..n * 3).map(|i| (init[i] as f64 - means[i % 3]) / if std > 0.0 { std } else { 1.0 } * 1e-4).collect()
            }
            None => {
                let first: Vec<f64> = (0..n).map(|i| reduced[i * k]).collect();
                let std = std_dev(&first);
                (0..n * 3)
                    .map(|i| {
                        let (r, c) = (i / 3, i % 3);
                        if c < k { reduced[r * k + c] / if std > 0.0 { std } else { 1.0 } * 1e-4 } else { 0.0 }
                    })
                    .collect()
            }
        };
        let perplexity = 30f64.min((n as f64 - 1.0) / 3.0);
        let learning_rate = (n as f64 / 12.0 / 4.0).max(50.0);
        // Stay gentle: this runs next to whatever else you do.
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).build();
        let run = || {
            let mut t: bhtsne::tSNE<f64, &[f64]> = bhtsne::tSNE::new(&rows);
            t.embedding_dim(3)
                .perplexity(perplexity)
                .learning_rate(learning_rate)
                .epochs(1000)
                .initial_embedding(start.clone())
                .barnes_hut(0.5, |a, b| a.iter().zip(b.iter()).map(|(x, y)| (x - y) * (x - y)).sum::<f64>().sqrt());
            t.embedding()
        };
        coords = match pool {
            Ok(p) => p.install(run),
            Err(_) => run(),
        };
    } else {
        for r in 0..n {
            for c in 0..3.min(k) {
                coords[r * 3 + c] = reduced[r * k + c];
            }
        }
    }
    let fitted = fit(&coords);
    match init {
        Some(target) => align(&fitted, target),
        None => fitted,
    }
}

fn std_dev(v: &[f64]) -> f64 {
    let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
    (v.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / v.len().max(1) as f64).sqrt()
}

/// numpy's quantile (linear interpolation).
fn quantile(mut values: Vec<f64>, q: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let pos = q * (values.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    values[lo] + (values[hi] - values[lo]) * (pos - lo as f64)
}

fn fit(coords: &[f64]) -> Vec<f32> {
    let n = coords.len() / 3;
    let means: Vec<f64> = (0..3).map(|c| (0..n).map(|i| coords[i * 3 + c]).sum::<f64>() / n.max(1) as f64).collect();
    let centred: Vec<f64> = coords.iter().enumerate().map(|(i, v)| v - means[i % 3]).collect();
    let scale = quantile(centred.iter().map(|v| v.abs()).collect(), 0.98);
    let scale = if scale > 0.0 { scale } else { 1.0 };
    centred.iter().map(|v| (v / scale).clamp(-1.15, 1.15) as f32).collect()
}

/// Turn, mirror and scale `coords` to lie as close as possible to `target` (the same points).
pub fn align(coords: &[f32], target: &[f32]) -> Vec<f32> {
    let n = coords.len() / 3;
    if n < 4 {
        return coords.to_vec();
    }
    let mean = |v: &[f32]| -> [f64; 3] {
        let mut m = [0f64; 3];
        for i in 0..n {
            for c in 0..3 {
                m[c] += v[i * 3 + c] as f64;
            }
        }
        m.map(|x| x / n as f64)
    };
    let (ma, mb) = (mean(coords), mean(target));
    let a = |i: usize, c: usize| coords[i * 3 + c] as f64 - ma[c];
    let b = |i: usize, c: usize| target[i * 3 + c] as f64 - mb[c];
    let mut m = Matrix3::<f64>::zeros();
    let mut sq = 0.0;
    for i in 0..n {
        for r in 0..3 {
            sq += a(i, r) * a(i, r);
            for c in 0..3 {
                m[(r, c)] += a(i, r) * b(i, c);
            }
        }
    }
    let svd = m.svd(true, true);
    let (Some(u), Some(vt)) = (svd.u, svd.v_t) else { return coords.to_vec() };
    let rot = u * vt;
    let scale = svd.singular_values.sum() / if sq > 0.0 { sq } else { 1.0 };
    let mut out = vec![0f32; n * 3];
    for i in 0..n {
        for c in 0..3 {
            let v: f64 = (0..3).map(|r| a(i, r) * rot[(r, c)]).sum();
            out[i * 3 + c] = (v * scale + mb[c]).clamp(-1.15, 1.15) as f32;
        }
    }
    out
}

/// The same n files every time, as far as the index allows: the ones whose ids hash lowest. A new
/// file only joins (and pushes one out) if its hash is lower, so the map changes little.
pub fn sample(ids: &[i64], n: usize) -> Vec<i64> {
    if ids.len() <= n {
        return ids.to_vec();
    }
    let mut hashed: Vec<(u64, i64)> = ids.iter().map(|&i| ((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15), i)).collect();
    hashed.select_nth_unstable(n - 1);
    let mut out: Vec<i64> = hashed[..n].iter().map(|&(_, i)| i).collect();
    out.sort_unstable();
    out
}

/// A small spread around a spot, the same for a file every time.
fn spread(id: i64) -> [f32; 3] {
    let mut state = (id as u64) ^ 0x5DEE_CE66_D1CE_4E5B;
    let mut next = || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    };
    [0, 1, 2].map(|_| {
        let (u1, u2) = (next().max(1e-12), next());
        ((-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos() * 0.015) as f32
    })
}

/// Where new files go: among their most similar files on the map, with a little spread.
fn place(new: &[f32], vectors: &[f32], coords: &[f32], new_ids: &[i64]) -> Vec<f32> {
    let m = vectors.len() / EMBED_DIM;
    let k = NEIGHBOURS.min(m);
    let mut out = Vec::with_capacity(new_ids.len() * 3);
    for (r, id) in new_ids.iter().enumerate() {
        let q = &new[r * EMBED_DIM..(r + 1) * EMBED_DIM];
        let mut sims: Vec<(f32, usize)> = (0..m).map(|j| (dot(q, &vectors[j * EMBED_DIM..(j + 1) * EMBED_DIM]), j)).collect();
        if k < sims.len() {
            sims.select_nth_unstable_by(k - 1, |a, b| b.0.total_cmp(&a.0));
            sims.truncate(k);
        }
        let top = sims.iter().map(|s| s.0).fold(f32::MIN, f32::max);
        let weights: Vec<f64> = sims.iter().map(|s| (((s.0 - top) / 0.02) as f64).exp()).collect(); // mostly the very closest ones
        let total: f64 = weights.iter().sum();
        let s = spread(*id);
        for c in 0..3 {
            let pos: f64 = sims.iter().zip(&weights).map(|((_, j), w)| w * coords[j * 3 + c] as f64).sum::<f64>() / total;
            out.push((pos as f32 + s[c]).clamp(-1.15, 1.15));
        }
    }
    out
}

#[derive(Clone, Default)]
struct Layout {
    ids: Vec<i64>,           // sorted
    coords: Vec<f32>,        // (n, 3)
    vectors: Vec<f32>,       // (n, dim), unit length
    stamps: Vec<(f64, f64)>, // mtime and size when placed, to notice changed files
    rough: Vec<bool>,        // placed next to similar files rather than laid out
}

impl Layout {
    fn len(&self) -> usize {
        self.ids.len()
    }

    fn keep(&self, keep: &[bool]) -> Layout {
        let mut out = Layout::default();
        for i in 0..self.len() {
            if keep[i] {
                out.ids.push(self.ids[i]);
                out.coords.extend_from_slice(&self.coords[i * 3..i * 3 + 3]);
                out.vectors.extend_from_slice(&self.vectors[i * EMBED_DIM..(i + 1) * EMBED_DIM]);
                out.stamps.push(self.stamps[i]);
                out.rough.push(self.rough[i]);
            }
        }
        out
    }
}

struct Shown {
    layout: Option<Arc<Layout>>,
    body: String, // the map as JSON, minus its status, ready to send
    points: Vec<serde_json::Value>,
    total: usize,
    rev: u64,
    version: Option<u64>,
    computed_at: Option<Instant>,
    running: bool,
    loaded: bool,
}

pub struct FileMap {
    store: Arc<Store>,
    path: PathBuf,
    state: Mutex<Shown>,
    me: Weak<FileMap>,
}

impl FileMap {
    pub fn new(store: Arc<Store>) -> Arc<FileMap> {
        let path = store.map_path();
        Self::at(store, path)
    }

    pub fn at(store: Arc<Store>, path: PathBuf) -> Arc<FileMap> {
        // Counts layouts shown; starts from the clock, so the page never takes a map from before a
        // restart for the current one.
        let rev = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
            % 1_000_000_000) as u64;
        Arc::new_cyclic(|me| FileMap {
            store,
            path,
            state: Mutex::new(Shown {
                layout: None,
                body: String::new(),
                points: Vec::new(),
                total: 0,
                rev,
                version: None,
                computed_at: None,
                running: false,
                loaded: false,
            }),
            me: me.clone(),
        })
    }

    /// The map as JSON, and a tag that changes whenever the reply does.
    pub fn reply(&self) -> (String, String) {
        let status = self.status();
        let s = self.state.lock();
        if s.layout.is_none() {
            return (json!({"status": status}).to_string(), format!("\"none-{status}\""));
        }
        (format!("{{\"status\":\"{status}\",{}", &s.body[1..]), format!("\"{}-{status}\"", s.rev))
    }

    /// Cosine similarity of every dot on the map to `vector`.
    pub fn related(&self, vector: &[f32]) -> serde_json::Value {
        let Some(layout) = self.state.lock().layout.clone() else { return json!({"ids": [], "sims": []}) };
        let sims: Vec<f64> = (0..layout.len())
            .map(|i| ((dot(&layout.vectors[i * EMBED_DIM..(i + 1) * EMBED_DIM], vector) as f64) * 1000.0).round() / 1000.0)
            .collect();
        json!({"ids": layout.ids, "sims": sims})
    }

    /// Every dot's value in one of the dimensions.
    pub fn dimension(&self, index: usize) -> serde_json::Value {
        let Some(layout) = self.state.lock().layout.clone() else { return json!({"ids": [], "values": []}) };
        let values: Vec<f64> =
            (0..layout.len()).map(|i| (layout.vectors[i * EMBED_DIM + index] as f64 * 10000.0).round() / 10000.0).collect();
        json!({"ids": layout.ids, "values": values})
    }

    /// "ready", "updating" (the index changed since; catching up) or "computing" (no map yet).
    fn status(&self) -> &'static str {
        self.load_saved();
        let version = self.store.version();
        let mut s = self.state.lock();
        let fresh = s.layout.is_some() && s.version == Some(version);
        let due = s.computed_at.is_none_or(|t| t.elapsed().as_secs_f64() > REFRESH_S);
        if !fresh && due && !s.running {
            s.running = true;
            if let Some(me) = self.me.upgrade() {
                std::thread::Builder::new().name("map".into()).spawn(move || me.compute(version)).ok();
            }
        }
        if s.layout.is_none() {
            "computing"
        } else if fresh {
            "ready"
        } else {
            "updating"
        }
    }

    // ---- keeping the layout ----------------------------------------------------------------

    fn load_saved(&self) {
        let mut s = self.state.lock();
        if s.loaded {
            return;
        }
        s.loaded = true;
        match read_layout(&self.path) {
            Ok(Some(layout)) => {
                let total = self.store.map_file_count() as usize;
                self.set(&mut s, layout, total, true);
            }
            Ok(None) => {}
            Err(e) => log::warn!("Could not read the saved map; it will be laid out again: {e}"),
        }
    }

    /// Make `layout` the one shown.
    fn set(&self, s: &mut Shown, layout: Layout, total: usize, changed: bool) {
        let files = self.store.files_by_ids(&layout.ids);
        let keep: Vec<bool> = layout.ids.iter().map(|i| files.contains_key(i)).collect();
        let all = keep.iter().all(|&k| k);
        let layout = if all { layout } else { layout.keep(&keep) }; // files gone meanwhile
        if changed || !all {
            s.rev += 1;
            let r4 = |v: f32| (v as f64 * 10000.0).round() / 10000.0;
            s.points = layout
                .ids
                .iter()
                .enumerate()
                .map(|(i, id)| {
                    let f = &files[id];
                    json!({"id": id, "x": r4(layout.coords[i * 3]), "y": r4(layout.coords[i * 3 + 1]),
                           "z": r4(layout.coords[i * 3 + 2]), "k": f.kind, "p": f.path})
                })
                .collect();
        }
        s.total = total;
        s.body = json!({"points": s.points, "total": total, "rev": s.rev}).to_string();
        s.layout = Some(Arc::new(layout));
    }

    fn compute(&self, version: u64) {
        set_background_priority(true);
        let started = Instant::now();
        let current = self.state.lock().layout.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.next_layout(current.as_deref())));
        match result {
            Ok((layout, total, how)) => {
                let changed = layout.is_some();
                let mut s = self.state.lock();
                let next = layout.clone().or_else(|| s.layout.as_deref().cloned()).unwrap_or_default();
                self.set(&mut s, next, total, changed);
                s.version = Some(version);
                drop(s);
                if let Some(l) = layout {
                    if let Err(e) = write_layout(&self.path, &l) {
                        log::warn!("Could not save the map: {e}");
                    }
                    log::info!("Map of {} files {how} in {:.1}s", l.len(), started.elapsed().as_secs_f64());
                }
            }
            Err(_) => log::error!("Could not compute the map"),
        }
        let mut s = self.state.lock();
        s.computed_at = Some(Instant::now());
        s.running = false;
    }

    /// The next layout (None: nothing on the map changed), how many files there are, and how it
    /// came about.
    fn next_layout(&self, old: Option<&Layout>) -> (Option<Layout>, usize, String) {
        let files = self.store.map_files(); // every file with vectors, by id
        let total = files.len();
        let all_ids: Vec<i64> = files.iter().map(|f| f.0).collect();
        let stamp_of: HashMap<i64, (f64, f64)> = files.iter().map(|f| (f.0, (f.1, f.2))).collect();
        let mut shown = sample(&all_ids, MAX_POINTS);
        let old_at: HashMap<i64, usize> = old.map(|o| o.ids.iter().enumerate().map(|(i, id)| (*id, i)).collect()).unwrap_or_default();
        let same_of = |id: i64| -> bool {
            match (old, old_at.get(&id)) {
                (Some(o), Some(&i)) => o.stamps[i] == stamp_of[&id],
                _ => false,
            }
        };
        if let Some(o) = old {
            if shown.len() == o.len() && shown.iter().all(|&id| same_of(id)) {
                return (None, total, "unchanged".into()); // nothing on the map changed
            }
        }
        let mut new_ids: Vec<i64> = shown.iter().copied().filter(|&id| !same_of(id)).collect();
        let (vec_ids, vecs) = {
            let (ids, v) = self.store.vectors_of(&new_ids);
            per_file(&ids, &v)
        };
        let has: HashSet<i64> = vec_ids.iter().copied().collect();
        if new_ids.iter().any(|id| !has.contains(id)) {
            // (a file may lose its vectors in between)
            let drop: HashSet<i64> = new_ids.iter().copied().filter(|id| !has.contains(id)).collect();
            shown.retain(|id| !drop.contains(id));
            new_ids.retain(|id| !drop.contains(id));
        }
        let at_new: HashMap<i64, usize> = vec_ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let n = shown.len();
        let mut layout = Layout {
            ids: shown.clone(),
            coords: vec![0f32; n * 3],
            vectors: vec![0f32; n * EMBED_DIM],
            stamps: shown.iter().map(|id| stamp_of[id]).collect(),
            rough: vec![false; n],
        };
        let same: Vec<bool> = shown.iter().map(|&id| same_of(id)).collect();
        for (i, &id) in shown.iter().enumerate() {
            if same[i] {
                let (o, j) = (old.unwrap(), old_at[&id]);
                layout.vectors[i * EMBED_DIM..(i + 1) * EMBED_DIM].copy_from_slice(&o.vectors[j * EMBED_DIM..(j + 1) * EMBED_DIM]);
                layout.coords[i * 3..i * 3 + 3].copy_from_slice(&o.coords[j * 3..j * 3 + 3]);
                layout.rough[i] = o.rough[j];
            } else {
                let j = at_new[&id];
                layout.vectors[i * EMBED_DIM..(i + 1) * EMBED_DIM].copy_from_slice(&vecs[j * EMBED_DIM..(j + 1) * EMBED_DIM]);
            }
        }
        let n_same = same.iter().filter(|&&s| s).count();
        if !new_ids.is_empty() && n_same >= NEIGHBOURS {
            let (mut sv, mut sc, mut nv, mut ni) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for i in 0..n {
                if same[i] {
                    sv.extend_from_slice(&layout.vectors[i * EMBED_DIM..(i + 1) * EMBED_DIM]);
                    sc.extend_from_slice(&layout.coords[i * 3..i * 3 + 3]);
                } else {
                    nv.extend_from_slice(&layout.vectors[i * EMBED_DIM..(i + 1) * EMBED_DIM]);
                    ni.push(layout.ids[i]);
                }
            }
            let placed = place(&nv, &sv, &sc, &ni);
            let mut k = 0;
            for i in 0..n {
                if !same[i] {
                    layout.coords[i * 3..i * 3 + 3].copy_from_slice(&placed[k * 3..k * 3 + 3]);
                    layout.rough[i] = true;
                    k += 1;
                }
            }
        }
        let rough = layout.rough.iter().filter(|&&r| r).count();
        let how = if n_same < NEIGHBOURS || n < TSNE_MIN || rough as f64 > REDO_SHARE * n as f64 {
            // Lay it all out again, starting from where everything is now.
            let init = (n_same >= NEIGHBOURS).then(|| layout.coords.clone());
            layout.coords = to_3d(&layout.vectors, init.as_deref());
            layout.rough = vec![false; n];
            if init.is_none() { "laid out".to_string() } else { "laid out again".to_string() }
        } else {
            format!("updated ({} placed)", new_ids.len())
        };
        (Some(layout), total, how)
    }
}

fn write_layout(path: &std::path::Path, l: &Layout) -> std::io::Result<()> {
    let tmp = path.with_extension("bin.tmp");
    {
        let mut w = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        w.write_all(MAGIC)?;
        w.write_all(&(l.len() as u64).to_le_bytes())?;
        w.write_all(&(EMBED_DIM as u64).to_le_bytes())?;
        for id in &l.ids {
            w.write_all(&id.to_le_bytes())?;
        }
        for c in &l.coords {
            w.write_all(&c.to_le_bytes())?;
        }
        for v in &l.vectors {
            w.write_all(&half::f16::from_f32(*v).to_le_bytes())?;
        }
        for (m, s) in &l.stamps {
            w.write_all(&m.to_le_bytes())?;
            w.write_all(&s.to_le_bytes())?;
        }
        for r in &l.rough {
            w.write_all(&[*r as u8])?;
        }
        w.flush()?;
    }
    std::fs::rename(&tmp, path)
}

fn read_layout(path: &std::path::Path) -> std::io::Result<Option<Layout>> {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut r = data.as_slice();
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic)?;
    let mut u = [0u8; 8];
    r.read_exact(&mut u)?;
    let n = u64::from_le_bytes(u) as usize;
    r.read_exact(&mut u)?;
    if &magic != MAGIC || u64::from_le_bytes(u) as usize != EMBED_DIM || n > 10_000_000 {
        return Ok(None);
    }
    let mut take = |len: usize| -> std::io::Result<Vec<u8>> {
        let mut b = vec![0u8; len];
        r.read_exact(&mut b)?;
        Ok(b)
    };
    let ids = take(n * 8)?.chunks_exact(8).map(|c| i64::from_le_bytes(c.try_into().unwrap())).collect();
    let coords = take(n * 12)?.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
    let vectors = take(n * EMBED_DIM * 2)?.chunks_exact(2).map(|c| half::f16::from_le_bytes([c[0], c[1]]).to_f32()).collect();
    let stamps = take(n * 16)?
        .chunks_exact(16)
        .map(|c| (f64::from_le_bytes(c[..8].try_into().unwrap()), f64::from_le_bytes(c[8..].try_into().unwrap())))
        .collect();
    let rough = take(n)?.into_iter().map(|b| b != 0).collect();
    Ok(Some(Layout { ids, coords, vectors, stamps, rough }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn means_samples_and_alignment() {
        let mut v = vec![0f32; 3 * EMBED_DIM];
        v[0] = 1.0;
        v[EMBED_DIM + 1] = 1.0;
        v[2 * EMBED_DIM] = 1.0;
        let (ids, means) = per_file(&[7, 3, 7], &v);
        assert_eq!(ids, vec![3, 7]);
        assert!((means[EMBED_DIM] - 1.0).abs() < 1e-6); // file 7: both rows say dimension 0
        let s = sample(&(0..100).collect::<Vec<_>>(), 10);
        assert_eq!(s.len(), 10);
        assert_eq!(s, sample(&(0..100).collect::<Vec<_>>(), 10));
        let pts: Vec<f32> = vec![0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.5, 0.5, 0.5];
        let mirrored: Vec<f32> = pts.iter().enumerate().map(|(i, x)| if i % 3 == 0 { -x } else { *x }).collect();
        let back = align(&mirrored, &pts);
        assert!(back.iter().zip(&pts).all(|(a, b)| (a - b).abs() < 1e-4), "{back:?}");
    }

    #[test]
    fn small_maps_use_pca() {
        let mut v = vec![0f32; 20 * EMBED_DIM];
        for i in 0..20 {
            v[i * EMBED_DIM + i % 5] = 1.0;
            v[i * EMBED_DIM + 10] = i as f32 / 20.0;
        }
        let c = to_3d(&v, None);
        assert_eq!(c.len(), 60);
        assert!(c.iter().all(|x| x.abs() <= 1.15));
    }
}

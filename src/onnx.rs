//! EmbeddingGemma 2 through ONNX Runtime: on any graphics card Windows can drive (DirectML) or on
//! the processor. The model's published files and tokenizer, with texts and pictures prepared as
//! its own processor prepares them.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use image::RgbImage;
use ort::session::{OutputSelector, RunOptions, Session};
use ort::session::builder::{GraphOptimizationLevel, SessionBuilder};
use ort::value::Tensor;
use tokenizers::{Tokenizer, TruncationParams};

pub const REPO: &str = "onnx-community/embeddinggemma-2-ONNX";
pub const REVISION: &str = "daa72c51243991dfcaf9f9137d2c573d8f7790c0"; // these exact files
pub const EMBED_DIM: usize = 256;

const BOS: i64 = 2;
const EOS: i64 = 1;
const PAD: i64 = 0;
const BOI: i64 = 255_999;
const IMAGE: i64 = 258_880;
const EOI: i64 = 258_882;
pub const PATCH: usize = 16;
pub const POOL: usize = 3;
// A picture becomes at most this many tokens for the model. Its own processor allows 280; half that
// halves the vision encoder's time (it attends over every patch) and finds the same photos (measured
// on captioned photos: the right one in the top five as often, first a little less often).
pub const SOFT_TOKENS: usize = 140;
pub const MAX_PATCHES: usize = SOFT_TOKENS * POOL * POOL;
#[cfg(test)]
const REFERENCE_PATCHES: usize = 280 * POOL * POOL; // (the model's own processor, for the tests)
const MAX_TOKENS: usize = 2048; // passages are far shorter; this only stops a runaway one
pub const QUERY_PROMPT: &str = "task: search result | query: ";
const FEATURES: usize = 512;
/// On the graphics card, ONNX Runtime keeps DirectML's prepared work only for the first input
/// shape each step of a session sees: any other shape is prepared anew on every call, which costs
/// more than the call itself. So texts run in two shapes (rows, tokens), padded up to them: batches
/// of passages through a session made for exactly BATCH, and searches, pictures and single
/// passages through one that saw SINGLE first. Anything larger runs as it is, padded to steps of
/// STEP tokens. (Padding is masked out.)
const BATCH: (usize, usize) = (8, 512);
// Pictures' text part, 8 at a time: BOS BOI, the picture's tokens, EOI EOS, and masked picture
// tokens up to SOFT_TOKENS, so every batch has the same shape (the graphics card keeps it compiled).
const PICTURES: (usize, usize) = (8, SOFT_TOKENS + 20);
const SINGLE: (usize, usize) = (1, 320);
const STEP: usize = 64;

/// What a graphics card (half precision) or the processor (8-bit) needs.
pub fn files(gpu: bool) -> Vec<String> {
    let flavour = if gpu { "fp16" } else { "quantized" };
    let mut out = vec!["config.json".to_string(), "tokenizer.json".to_string()];
    for part in ["model", "vision_encoder"] {
        for ext in ["", "_data"] {
            out.push(format!("onnx/{part}_{flavour}.onnx{ext}"));
        }
    }
    out
}

/// The model's folder in the Hugging Face cache (shared with other apps: read only here).
pub fn snapshot() -> PathBuf {
    let home = std::env::var_os("HF_HOME").map(PathBuf::from).unwrap_or_else(|| {
        let profile = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
        profile.join(".cache").join("huggingface")
    });
    let hub = std::env::var_os("HF_HUB_CACHE").map(PathBuf::from).unwrap_or_else(|| home.join("hub"));
    hub.join(format!("models--{}", REPO.replace('/', "--"))).join("snapshots").join(REVISION)
}

pub fn cached(gpu: bool) -> bool {
    let folder = snapshot();
    files(gpu).iter().all(|f| folder.join(f).is_file())
}

/// ONNX Runtime's library, next to the app (vendor\onnxruntime while developing).
pub fn runtime_dir() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let mut dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    for _ in 0..4 {
        if dir.join("onnxruntime.dll").is_file() {
            return dir;
        }
        let vendored = dir.join("vendor").join("onnxruntime");
        if vendored.join("onnxruntime.dll").is_file() {
            return vendored;
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => break,
        }
    }
    PathBuf::from("vendor").join("onnxruntime")
}

static INIT: std::sync::Once = std::sync::Once::new();

fn init_runtime() -> Result<()> {
    let dir = runtime_dir();
    let dll = dir.join("onnxruntime.dll");
    if !dll.is_file() {
        bail!("ONNX Runtime not found at {}", dll.display());
    }
    let mut result = Ok(());
    INIT.call_once(|| {
        // onnxruntime.dll looks for DirectML.dll on the PATH (it's next to it).
        // SAFETY: done once, before the model's threads start.
        unsafe { std::env::set_var("PATH", format!("{};{}", dir.display(), std::env::var("PATH").unwrap_or_default())) };
        match ort::init_from(&dll) {
            Ok(env) => {
                env.with_name("quiekel-embed").commit();
            }
            Err(e) => result = Err(anyhow!("ONNX Runtime didn't start: {e}")),
        }
    });
    result
}

// A Reshape attribute, as it's stored: name "allowzero", value 1. DirectML refuses Reshape with
// allowzero=1, and here it changes nothing (no target size is zero), so the copy DirectML gets
// says 0, the default. Same length: nothing else moves.
const ALLOWZERO: &[u8] = b"\x0a\x09allowzero\x18\x01";

/// The graph as DirectML takes it: a copy next to the original that uses the same weights file.
fn for_directml(path: &Path) -> Result<PathBuf> {
    let graph = std::fs::read(path)?;
    if !contains(&graph, ALLOWZERO) {
        return Ok(path.to_path_buf());
    }
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let fixed = path.with_file_name(format!("{stem}_directml.onnx"));
    if !fixed.exists() {
        let mut zero = ALLOWZERO.to_vec();
        *zero.last_mut().unwrap() = 0;
        let out = replace_all(&graph, ALLOWZERO, &zero);
        let tmp = fixed.with_extension("tmp");
        std::fs::write(&tmp, out)?;
        std::fs::rename(&tmp, &fixed)?;
    }
    Ok(fixed)
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn replace_all(hay: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(hay.len());
    let mut i = 0;
    while i < hay.len() {
        if hay[i..].starts_with(from) {
            out.extend_from_slice(to);
            i += from.len();
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out
}

// ---- pictures, as the model's own processor prepares them ---------------------------------

/// The largest size with the same shape that fits the patch budget, in steps of 48 pixels:
/// (height, width).
pub fn target_size(height: u32, width: u32) -> (u32, u32) {
    target_size_for(height, width, MAX_PATCHES)
}

fn target_size_for(height: u32, width: u32, max_patches: usize) -> (u32, u32) {
    let factor = ((max_patches * PATCH * PATCH) as f64 / (height as f64 * width as f64)).sqrt();
    let side = (POOL * PATCH) as f64;
    let mut h = (factor * height as f64 / side).floor() * side;
    let mut w = (factor * width as f64 / side).floor() * side;
    let longest = (max_patches / (POOL * POOL)) as f64 * side;
    if h == 0.0 {
        h = side;
        w = ((width as f64 / height as f64).floor() * side).min(longest);
    } else if w == 0.0 {
        w = side;
        h = ((height as f64 / width as f64).floor() * side).min(longest);
    }
    (h as u32, w as u32)
}

/// Pixels of each patch (padded to MAX_PATCHES), each patch's (column, row) (-1 for padding),
/// and the number of soft tokens the picture becomes.
pub fn patches(image: &RgbImage) -> (Vec<f32>, Vec<i64>, usize) {
    let (h, w) = target_size(image.height(), image.width());
    let resized;
    let img = if (h, w) != (image.height(), image.width()) {
        resized = crate::pictures::resize_bicubic(image, w, h);
        &resized
    } else {
        image
    };
    let (rows, cols) = (h as usize / PATCH, w as usize / PATCH);
    let per = PATCH * PATCH * 3;
    let mut pixels = vec![0f32; MAX_PATCHES * per];
    let mut places = vec![-1i64; MAX_PATCHES * 2];
    let raw = img.as_raw();
    let stride = w as usize * 3;
    for r in 0..rows {
        for c in 0..cols {
            let n = r * cols + c;
            let out = &mut pixels[n * per..(n + 1) * per];
            for py in 0..PATCH {
                let row = (r * PATCH + py) * stride;
                for px in 0..PATCH {
                    let at = row + (c * PATCH + px) * 3;
                    let o = (py * PATCH + px) * 3;
                    out[o] = raw[at] as f32 / 255.0;
                    out[o + 1] = raw[at + 1] as f32 / 255.0;
                    out[o + 2] = raw[at + 2] as f32 / 255.0;
                }
            }
            places[n * 2] = c as i64;
            places[n * 2 + 1] = r as i64;
        }
    }
    (pixels, places, rows * cols / (POOL * POOL))
}

fn picture_ids(soft_tokens: usize) -> Vec<i64> {
    let mut ids = vec![BOS, BOI];
    ids.extend(std::iter::repeat_n(IMAGE, soft_tokens));
    ids.extend([EOI, EOS]);
    ids
}

// ---- the model ------------------------------------------------------------------------------

/// Where and how the sessions run.
struct Engine {
    gpu: bool,
    device: i32,
    threads: usize,
    folder: PathBuf,
}

pub struct OnnxModel {
    pub gpu: bool,
    engine: Engine,
    tokenizer: Tokenizer,
    text: Session,
    /// On the graphics card: batches of passages, in exactly the BATCH shape (made when first needed).
    batches: Option<Session>,
    no_batches: bool, // it couldn't be made: batches run as they are
    vision: Option<Session>,
    /// On the graphics card: pictures' text part in exactly the PICTURES shape (made when first needed).
    pictures: Option<Session>,
    no_pictures: bool,
}

impl OnnxModel {
    pub fn new(gpu: bool, device: i32, threads: usize) -> Result<Self> {
        init_runtime()?;
        let folder = snapshot();
        if !cached(gpu) {
            crate::download::model(gpu)?;
        }
        crate::download::as_real_files(&folder, &files(gpu));
        let mut tokenizer = Tokenizer::from_file(folder.join("tokenizer.json")).map_err(|e| anyhow!("tokenizer: {e}"))?;
        tokenizer
            .with_truncation(Some(TruncationParams { max_length: MAX_TOKENS, ..Default::default() }))
            .map_err(|e| anyhow!("tokenizer: {e}"))?;
        let engine = Engine { gpu, device, threads, folder };
        let text = engine.session("model", None)?;
        let mut model = OnnxModel {
            gpu, engine, tokenizer, text, batches: None, no_batches: false, vision: None, pictures: None, no_pictures: false,
        };
        if gpu {
            model.run_text(&[vec![BOS, EOS]], (vec![], 0))?; // its first shape: SINGLE
        }
        Ok(model)
    }

    fn vision(&mut self) -> Result<&mut Session> {
        if self.vision.is_none() {
            self.vision = Some(self.engine.session("vision_encoder", None)?);
        }
        Ok(self.vision.as_mut().unwrap())
    }

    /// The session for pictures' text part, made the first time several pictures come.
    fn picture_batches(&mut self) -> Option<&mut Session> {
        if self.pictures.is_none() && !self.no_pictures {
            match self.engine.session("model", Some((PICTURES.0, PICTURES.1, PICTURES.0 * SOFT_TOKENS))) {
                Ok(s) => self.pictures = Some(s),
                Err(e) => {
                    log::warn!("Pictures run one at a time: {e:#}");
                    self.no_pictures = true;
                }
            }
        }
        self.pictures.as_mut()
    }

    /// The session for batches, made the first time one comes (None if it can't be).
    fn batches(&mut self) -> Option<&mut Session> {
        if self.batches.is_none() && !self.no_batches {
            match self.engine.session("model", Some((BATCH.0, BATCH.1, 0))) {
                Ok(s) => self.batches = Some(s),
                Err(e) => {
                    log::warn!("Batches run as they are: {e:#}");
                    self.no_batches = true;
                }
            }
        }
        self.batches.as_mut()
    }
}

impl Engine {
    /// A part of the model; `shape` (rows, tokens, picture tokens) fixes a text session to exactly
    /// that shape.
    fn session(&self, part: &str, shape: Option<(usize, usize, usize)>) -> Result<Session> {
        let flavour = if self.gpu { "fp16" } else { "quantized" };
        let mut path = self.folder.join("onnx").join(format!("{part}_{flavour}.onnx"));
        if self.gpu {
            path = for_directml(&path)?;
        }
        let mut builder = ok(Session::builder()?.with_optimization_level(GraphOptimizationLevel::All))?;
        builder = ok(builder.with_intra_threads(self.threads))?;
        builder = ok(builder.with_inter_threads(1))?;
        // Threads that sleep when there's nothing to do (by default they spin, keeping cores busy
        // between batches).
        builder = ok(builder.with_config_entry("session.intra_op.allow_spinning", "0"))?;
        builder = ok(builder.with_config_entry("session.inter_op.allow_spinning", "0"))?;
        if self.gpu {
            builder = ok(builder.with_memory_pattern(false))?;
            builder = ok(builder.with_parallel_execution(false))?;
            if let Some((rows, tokens, picture_tokens)) = shape {
                // With every size known DirectML would run the model as one piece, a little faster,
                // but that keeps a second copy of the weights in the PC's memory for good.
                builder = ok(builder.with_config_entry("ep.dml.disable_graph_fusion", "1"))?;
                let sizes = [("batch_size", rows), ("sequence_length", tokens), ("num_image_tokens", picture_tokens)];
                for (name, size) in sizes.into_iter().chain([("num_video_tokens", 0), ("num_audio_tokens", 0)]) {
                    builder = ok(builder.with_dimension_override(name, size as i64))?;
                }
            }
            builder = ok(builder.with_execution_providers([ort::ep::DirectML::default()
                .with_device_id(self.device)
                .build()
                .error_on_failure()]))?;
        } else {
            // No memory pool: it keeps the largest batch's memory for good, for no gain in speed.
            builder = ok(builder.with_execution_providers([ort::ep::CPU::default().with_arena_allocator(false).build()]))?;
        }
        builder.commit_from_file(&path).with_context(|| format!("loading {}", path.display()))
    }
}

impl OnnxModel {
    /// Token ids of texts (with the prompt in front).
    pub fn tokens(&self, texts: &[String], prompt: &str) -> Result<Vec<Vec<i64>>> {
        let inputs: Vec<String> = texts.iter().map(|t| format!("{prompt}{t}")).collect();
        let encoded = self.tokenizer.encode_batch(inputs, true).map_err(|e| anyhow!("tokenizer: {e}"))?;
        Ok(encoded.iter().map(|e| e.get_ids().iter().map(|&i| i as i64).collect()).collect())
    }

    /// Unit vectors of texts, cut to EMBED_DIM.
    pub fn embed_texts(&mut self, texts: &[String], prompt: &str) -> Result<Vec<Vec<f32>>> {
        let ids = self.tokens(texts, prompt)?;
        self.run_text(&ids, (vec![], 0))
    }

    /// Unit vectors of pictures (one at a time: their encoder attends over all of a picture's patches,
    /// which keeps the graphics card busy; batches of pictures measured no faster).
    pub fn embed_images(&mut self, images: &[RgbImage]) -> Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(images.len());
        if self.gpu && images.len() > 1 && self.picture_batches().is_some() {
            for chunk in images.chunks(PICTURES.0) {
                let features = chunk.iter().map(|image| self.picture_features(image)).collect::<Result<Vec<_>>>()?;
                out.extend(self.run_pictures(&features)?);
            }
            return Ok(out);
        }
        for image in images {
            let (data, soft) = self.picture_features(image)?;
            out.extend(self.run_text(&[picture_ids(soft)], (data, soft))?);
        }
        Ok(out)
    }

    /// The vision encoder's features of one picture: (its rows of FEATURES, how many rows).
    fn picture_features(&mut self, image: &RgbImage) -> Result<(Vec<f32>, usize)> {
        let (pixels, places, soft) = patches(image);
        let vision = self.vision()?;
        let pv = Tensor::from_array(([1usize, MAX_PATCHES, PATCH * PATCH * 3], pixels))?;
        let pp = Tensor::from_array(([1usize, MAX_PATCHES, 2usize], places))?;
        let outputs = vision.run(ort::inputs!["pixel_values" => pv, "pixel_position_ids" => pp])?;
        let (shape, data) = extract_f32(&outputs["image_features"])?;
        let rows = shape.first().copied().unwrap_or(0) as usize;
        anyhow::ensure!(rows == soft && rows <= SOFT_TOKENS, "the vision encoder gave {rows} rows for {soft} tokens");
        Ok((data, rows))
    }

    /// Up to PICTURES.0 pictures' text part in one call, in exactly the PICTURES shape: each row's
    /// picture tokens are filled up to SOFT_TOKENS with masked ones (their features zero), which
    /// nothing attends to, so the vectors are those of each picture alone.
    fn run_pictures(&mut self, pictures: &[(Vec<f32>, usize)]) -> Result<Vec<Vec<f32>>> {
        let (rows, tokens) = PICTURES;
        let n = pictures.len();
        anyhow::ensure!(n <= rows, "too many pictures for one batch");
        let mut input_ids = vec![PAD; rows * tokens];
        let mut mask = vec![0i64; rows * tokens];
        let mut features = vec![0f32; rows * SOFT_TOKENS * FEATURES];
        for row in 0..rows {
            let (data, soft) = pictures.get(row).map_or((&[][..], 0), |(d, s)| (d.as_slice(), *s));
            let real = if row < n { picture_ids(soft) } else { vec![BOS, EOS] }; // (rows that only fill the shape)
            let at = row * tokens;
            input_ids[at..at + real.len()].copy_from_slice(&real);
            mask[at..at + real.len()].fill(1);
            input_ids[at + real.len()..at + real.len() + SOFT_TOKENS - soft].fill(IMAGE);
            let f = row * SOFT_TOKENS * FEATURES;
            features[f..f + data.len()].copy_from_slice(data);
        }
        let inputs = ort::inputs![
            "input_ids" => Tensor::from_array(([rows, tokens], input_ids))?,
            "attention_mask" => Tensor::from_array(([rows, tokens], mask))?,
            "image_features" => Tensor::from_array(([rows * SOFT_TOKENS, FEATURES], features))?,
            "video_features" => Tensor::from_array(([0usize, FEATURES], Vec::<f32>::new()))?,
            "audio_features" => Tensor::from_array(([0usize, FEATURES], Vec::<f32>::new()))?,
        ];
        let options = RunOptions::new()?.with_outputs(OutputSelector::no_default().with("sentence_embedding"));
        let session = self.pictures.as_mut().context("no session for pictures")?;
        let outputs = session.run_with_options(inputs, &options)?;
        let (shape, data) = extract_f32(&outputs["sentence_embedding"])?;
        Ok(unit_vectors(&data, shape.get(1).copied().unwrap_or(0) as usize, n))
    }

    /// The sessions' inputs and outputs, with their shapes (for measuring).
    pub fn describe(&mut self) -> Result<serde_json::Value> {
        let io = |s: &Session| {
            serde_json::json!({
                "inputs": s.inputs().iter().map(|i| format!("{} {:?}", i.name(), i.dtype())).collect::<Vec<_>>(),
                "outputs": s.outputs().iter().map(|o| format!("{} {:?}", o.name(), o.dtype())).collect::<Vec<_>>(),
            })
        };
        let text = io(&self.text);
        let vision = io(self.vision()?);
        Ok(serde_json::json!({"model": text, "vision_encoder": vision}))
    }

    /// The vision encoder alone, for several pictures in one call: (features, rows per picture).
    pub fn vision_batch(&mut self, images: &[RgbImage]) -> Result<(Vec<f32>, Vec<usize>)> {
        let n = images.len();
        let mut pixels = Vec::with_capacity(n * MAX_PATCHES * PATCH * PATCH * 3);
        let mut places = Vec::with_capacity(n * MAX_PATCHES * 2);
        let mut softs = Vec::with_capacity(n);
        for image in images {
            let (p, q, soft) = patches(image);
            pixels.extend(p);
            places.extend(q);
            softs.push(soft);
        }
        let vision = self.vision()?;
        let pv = Tensor::from_array(([n, MAX_PATCHES, PATCH * PATCH * 3], pixels))?;
        let pp = Tensor::from_array(([n, MAX_PATCHES, 2usize], places))?;
        let outputs = vision.run(ort::inputs!["pixel_values" => pv, "pixel_position_ids" => pp])?;
        let (shape, data) = extract_f32(&outputs["image_features"])?;
        log::info!("vision batch of {n}: features {shape:?}");
        Ok((data, softs))
    }

    /// The text part of pictures whose features are known, in one call.
    pub fn picture_texts(&mut self, softs: &[usize], features: Vec<f32>) -> Result<Vec<Vec<f32>>> {
        let ids: Vec<Vec<i64>> = softs.iter().map(|&s| picture_ids(s)).collect();
        let rows = features.len() / FEATURES;
        self.run_text(&ids, (features, rows))
    }

    fn run_text(&mut self, ids: &[Vec<i64>], pictures: (Vec<f32>, usize)) -> Result<Vec<Vec<f32>>> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let n = ids.len();
        let longest = ids.iter().map(Vec::len).max().unwrap_or(0);
        let (features, image_tokens) = pictures;
        let fits = |(rows, tokens): (usize, usize)| n <= rows && longest <= tokens;
        let (rows, tokens, batch) = if !self.gpu {
            (n, longest, false)
        } else if fits(SINGLE) {
            (SINGLE.0, SINGLE.1, false)
        } else if fits(BATCH) && image_tokens == 0 && self.batches().is_some() {
            (BATCH.0, BATCH.1, true)
        } else {
            (n, longest.div_ceil(STEP) * STEP, false)
        };
        let filler = [BOS, EOS]; // rows that only fill the shape
        let mut input_ids = vec![PAD; rows * tokens];
        let mut mask = vec![0i64; rows * tokens];
        for row in 0..rows {
            let i = ids.get(row).map_or(&filler[..], Vec::as_slice);
            input_ids[row * tokens..row * tokens + i.len()].copy_from_slice(i);
            mask[row * tokens..row * tokens + i.len()].fill(1);
        }
        let inputs = ort::inputs![
            "input_ids" => Tensor::from_array(([rows, tokens], input_ids))?,
            "attention_mask" => Tensor::from_array(([rows, tokens], mask))?,
            "image_features" => Tensor::from_array(([image_tokens, FEATURES], features))?,
            "video_features" => Tensor::from_array(([0usize, FEATURES], Vec::<f32>::new()))?,
            "audio_features" => Tensor::from_array(([0usize, FEATURES], Vec::<f32>::new()))?,
        ];
        // Only the texts' vectors come back, not every token's.
        let options = RunOptions::new()?.with_outputs(OutputSelector::no_default().with("sentence_embedding"));
        let session = match &mut self.batches {
            Some(s) if batch => s,
            _ => &mut self.text,
        };
        let outputs = session.run_with_options(inputs, &options)?;
        let (shape, data) = extract_f32(&outputs["sentence_embedding"])?;
        Ok(unit_vectors(&data, shape.get(1).copied().unwrap_or(0) as usize, n))
    }
}

/// The first `n` rows of `width` values, cut to EMBED_DIM and made unit length.
fn unit_vectors(data: &[f32], width: usize, n: usize) -> Vec<Vec<f32>> {
    (0..n)
        .map(|row| {
            let mut v: Vec<f32> = data[row * width..row * width + EMBED_DIM.min(width)].to_vec();
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            for x in &mut v {
                *x /= norm;
            }
            v
        })
        .collect()
}

/// A session builder step's result, with its error made sendable (it carries the builder).
fn ok(r: Result<SessionBuilder, ort::Error<SessionBuilder>>) -> Result<SessionBuilder> {
    r.map_err(|e| anyhow!(ort::Error::<()>::from(e).to_string()))
}

/// A tensor's shape and values as f32 (half precision models answer in f16).
fn extract_f32(value: &ort::value::DynValue) -> Result<(Vec<i64>, Vec<f32>)> {
    if let Ok((shape, data)) = value.try_extract_tensor::<f32>() {
        return Ok((shape.iter().copied().collect(), data.to_vec()));
    }
    let (shape, data) = value.try_extract_tensor::<half::f16>()?;
    Ok((shape.iter().copied().collect(), data.iter().map(|x| x.to_f32()).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_are_sized_like_the_models_own_processor() {
        let cases = [
            ((427, 640), (624, 960)), ((150, 200), (672, 912)), ((1200, 300), (1584, 384)), ((100, 4000), (96, 5040)),
            ((4000, 100), (5040, 96)), ((20, 20), (768, 768)), ((3000, 4000), (672, 912)), ((1080, 1920), (576, 1056)),
        ];
        for ((h, w), expected) in cases {
            assert_eq!(target_size_for(h, w, REFERENCE_PATCHES), expected, "{h} x {w}");
        }
        // Half the budget: the same shapes, smaller, never more patches than allowed.
        for ((h, w), _) in cases {
            let (th, tw) = target_size(h, w);
            assert!((th as usize / PATCH) * (tw as usize / PATCH) <= MAX_PATCHES, "{h} x {w}");
            assert!(th % 48 == 0 && tw % 48 == 0);
        }
        assert_eq!(target_size(3000, 4000), (480, 624));
        assert_eq!(picture_ids(2), vec![2, 255_999, 258_880, 258_880, 258_882, 1]);
    }

    const DOCS: [&str; 3] = [
        "title: Mietvertrag | text: Die Miete beträgt 800 Euro im Monat und ist am Ersten fällig.",
        "title: main.py | text: def add(a, b): return a + b",
        "title: Linsensuppe | text: Linsen, Karotten und Zwiebeln eine halbe Stunde kochen lassen.",
    ];

    fn docs() -> Vec<String> {
        DOCS.iter().map(|d| d.to_string()).collect()
    }

    fn dot(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    /// Which passage a search finds first.
    fn found(m: &mut OnnxModel, docs: &[Vec<f32>], query: &str) -> usize {
        let q = m.embed_texts(&[query.to_string()], QUERY_PROMPT).unwrap().remove(0);
        (0..docs.len()).max_by(|&a, &b| dot(&docs[a], &q).total_cmp(&dot(&docs[b], &q))).unwrap()
    }

    #[test]
    fn the_real_model_on_the_processor() {
        if !cached(false) {
            return; // needs the model files (8-bit)
        }
        let mut m = OnnxModel::new(false, 0, 2).unwrap();
        let d = m.embed_texts(&docs(), "").unwrap();
        assert_eq!((d.len(), d[0].len()), (3, EMBED_DIM));
        assert_eq!(found(&mut m, &d, "when is the rent due"), 0);
        assert_eq!(found(&mut m, &d, "soup recipe"), 2);
    }

    /// The graphics card only when asked: it may be busy with something else (QUIEKEL_TEST_GPU=1).
    #[test]
    fn the_real_model_on_the_graphics_card_in_every_shape() {
        let card = crate::machine::graphics_cards().into_iter().max_by_key(|c| c.own_vram);
        let (Some(card), Ok("1")) = (card, std::env::var("QUIEKEL_TEST_GPU").as_deref()) else { return };
        if !cached(true) {
            return;
        }
        let mut m = OnnxModel::new(true, card.index as i32, 4).unwrap();
        let docs = docs();
        let batch = m.embed_texts(&docs, "").unwrap(); // a batch, padded to BATCH
        for (doc, v) in docs.iter().zip(&batch) {
            let alone = m.embed_texts(std::slice::from_ref(doc), "").unwrap().remove(0); // SINGLE
            assert!(dot(v, &alone) > 0.999, "{doc}: {}", dot(v, &alone));
        }
        assert_eq!(found(&mut m, &batch, "when is the rent due"), 0);
        assert_eq!(found(&mut m, &batch, "soup recipe"), 2);
        // Longer than a batch's shape: runs as it is, alone or with others.
        let long = format!("{} {}", DOCS[0], "Die Miete ist pünktlich zu zahlen. ".repeat(80));
        assert!(m.tokens(std::slice::from_ref(&long), "").unwrap()[0].len() > BATCH.1);
        let together = m.embed_texts(&[long.clone(), docs[1].clone()], "").unwrap();
        let alone = m.embed_texts(std::slice::from_ref(&long), "").unwrap().remove(0);
        assert!(dot(&together[0], &alone) > 0.999);
        assert!(dot(&together[1], &batch[1]) > 0.999);
        // Pictures several at a time (their text part in one call, filled up with masked picture
        // tokens) come out as each alone.
        let pictures: Vec<RgbImage> = [(640, 480), (300, 900), (1000, 1000)]
            .iter()
            .enumerate()
            .map(|(k, &(w, h))| RgbImage::from_fn(w, h, |x, y| image::Rgb([(x * 7 + k as u32 * 50) as u8, (y * 3) as u8, ((x + y) / 4) as u8])))
            .collect();
        let together = m.embed_images(&pictures).unwrap();
        for (picture, v) in pictures.iter().zip(&together) {
            let alone = m.embed_images(std::slice::from_ref(picture)).unwrap().remove(0);
            assert!(dot(v, &alone) > 0.9999, "{}", dot(v, &alone));
        }
        // Half precision on the card, 8-bit on the processor: close enough to search each other's index.
        if cached(false) {
            let cpu = OnnxModel::new(false, 0, 2).unwrap().embed_texts(&docs, "").unwrap();
            for (g, c) in batch.iter().zip(&cpu) {
                assert!(dot(g, c) > 0.97);
            }
        }
    }
}

//! ONNX speaker-embedding model wrapper.
//!
//! Model: WeSpeaker ResNet34 (VoxCeleb, English), downloaded from the sherpa-onnx
//! GitHub release `speaker-recognition-models` (Apache-2.0, no HuggingFace gate -
//! unlike `pyannote/embedding`, which is otherwise a good candidate but requires
//! accepting a gated HF agreement, a bad fit for silent/anonymous auto-download).
//!
//! Unlike Parakeet, this model does NOT accept raw waveform samples directly - it
//! expects pre-computed Kaldi-style fbank features, shape `[1, num_frames, feat_dim]`
//! (confirmed by reading sherpa-onnx's own C++ implementation, since the model file
//! itself doesn't document this). We compute those features with `kaldi-native-fbank`
//! (a Rust port of the exact library sherpa-onnx uses in C++, so output should be
//! numerically compatible with what the model was trained/exported to expect).

use anyhow::{anyhow, Result};
use kaldi_native_fbank::fbank::{FbankComputer, FbankOptions};
use kaldi_native_fbank::online::{FeatureComputer, OnlineFeature};
use ndarray::Array3;
use ort::execution_providers::CPUExecutionProvider;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::TensorRef;
use std::path::Path;

/// Standard fbank dimensionality for x-vector/WeSpeaker-family embedding models.
const FBANK_NUM_MEL_BINS: usize = 80;
const MODEL_SAMPLE_RATE: f32 = 16000.0;

pub struct DiarizationModel {
    session: Session,
    // Read from the loaded session at construction time rather than hardcoded -
    // avoids guessing wrong about exact tensor names in a model we can't inspect
    // ahead of time (the model download is user-initiated, not bundled).
    input_name: String,
    output_name: String,
}

impl DiarizationModel {
    pub fn new<P: AsRef<Path>>(model_path: P) -> Result<Self> {
        let providers = vec![CPUExecutionProvider::default().build()];

        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_execution_providers(providers)?
            .commit_from_file(model_path.as_ref())?;

        let input_name = session
            .inputs
            .first()
            .map(|i| i.name.clone())
            .ok_or_else(|| anyhow!("Diarization embedding model has no inputs"))?;
        let output_name = session
            .outputs
            .first()
            .map(|o| o.name.clone())
            .ok_or_else(|| anyhow!("Diarization embedding model has no outputs"))?;

        log::info!(
            "Loaded diarization embedding model: input='{}' ({:?}), output='{}' ({:?})",
            input_name,
            session.inputs.first().map(|i| &i.input_type),
            output_name,
            session.outputs.first().map(|o| &o.output_type),
        );

        Ok(Self {
            session,
            input_name,
            output_name,
        })
    }

    /// Compute Kaldi-style fbank features for a mono 16kHz audio segment.
    /// Returns a flat row-major `[num_frames, feat_dim]` buffer plus the frame count
    /// and the actual per-frame dimension (read from the computed frames themselves,
    /// not assumed - `kaldi_native_fbank`'s default options append extras like energy
    /// beyond the plain `num_bins` mel channels, so the real width isn't guaranteed
    /// to equal `FBANK_NUM_MEL_BINS` even though that's what we asked for).
    fn compute_fbank(samples: &[f32]) -> Result<(Vec<f32>, usize, usize)> {
        let mut opts = FbankOptions::default();
        opts.frame_opts.samp_freq = MODEL_SAMPLE_RATE;
        opts.mel_opts.num_bins = FBANK_NUM_MEL_BINS;
        opts.use_energy = false; // model expects exactly `num_bins` mel channels, no extra energy dim

        let computer = FbankComputer::new(opts)
            .map_err(|e| anyhow!("Failed to create fbank computer: {:?}", e))?;
        let mut online = OnlineFeature::new(FeatureComputer::Fbank(computer));

        online.accept_waveform(MODEL_SAMPLE_RATE, samples);
        online.input_finished();

        let num_frames = online.num_frames_ready();
        if num_frames == 0 {
            return Err(anyhow!("No fbank frames produced (segment too short)"));
        }

        let first_frame = online
            .get_frame(0)
            .ok_or_else(|| anyhow!("Missing fbank frame 0"))?;
        let feat_dim = first_frame.len();

        let mut features = Vec::with_capacity(num_frames * feat_dim);
        for i in 0..num_frames {
            let frame = online
                .get_frame(i)
                .ok_or_else(|| anyhow!("Missing fbank frame {}", i))?;
            if frame.len() != feat_dim {
                return Err(anyhow!(
                    "Inconsistent fbank frame width: frame 0 had {} bins, frame {} had {}",
                    feat_dim, i, frame.len()
                ));
            }
            features.extend_from_slice(frame);
        }

        Ok((features, num_frames, feat_dim))
    }

    /// Extract a fixed-size speaker embedding from a mono 16kHz audio segment.
    /// Expects at least a few hundred ms of audio - very short segments won't
    /// produce a meaningful embedding (caller should filter these out before
    /// clustering, same 800-sample/50ms floor used elsewhere in the VAD pipeline).
    pub fn extract_embedding(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        let (features, num_frames, feat_dim) = Self::compute_fbank(samples)?;

        let feats_array = Array3::from_shape_vec((1, num_frames, feat_dim), features)
            .map_err(|e| anyhow!("Failed to shape fbank features: {}", e))?;

        let input_tensor = TensorRef::from_array_view(feats_array.view())
            .map_err(|e| anyhow!("Failed to build input tensor: {}", e))?;

        let inputs: Vec<(String, ort::session::SessionInputValue)> =
            vec![(self.input_name.clone(), input_tensor.into())];
        let outputs = self
            .session
            .run(inputs)
            .map_err(|e| anyhow!("Diarization embedding inference failed: {}", e))?;

        let embedding_view = outputs
            .get(self.output_name.as_str())
            .ok_or_else(|| anyhow!("Output '{}' not found in model result", self.output_name))?
            .try_extract_array::<f32>()
            .map_err(|e| anyhow!("Failed to extract embedding array: {}", e))?;

        Ok(embedding_view.iter().copied().collect())
    }
}

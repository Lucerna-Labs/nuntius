//! True PCM-mixed crossfade.
//!
//! Architecture:
//! - One persistent ffmpeg encoder spawns at AutoDJ startup. It reads
//!   raw `s16le` PCM (stereo, 44.1 kHz) on stdin and emits MP3 at the
//!   configured station bitrate on stdout. We pump its stdout into the
//!   master broadcast channel.
//! - One ffmpeg decoder per track runs concurrently with the encoder.
//!   It applies the track's measured loudness gain via the `volume`
//!   filter and emits raw PCM on stdout.
//! - Each track plays in two phases: the BODY (decoder PCM → encoder
//!   stdin, real-time paced) and the CROSSFADE (decoder PCM mixed
//!   sample-wise with the next track's decoder, equal-power weights).
//! - Pacing happens on the encoder stdin side: we write 100 ms worth
//!   of PCM (44100 bytes/channel * 2 channels * 2 bytes/sample = 17640
//!   bytes per channel-pair, doubled for stereo) every 100 ms. The
//!   encoder backpressures naturally if it can't keep up.
//!
//! When ffmpeg isn't available we transparently fall back to the
//! original "raw byte pacer" code path (see `stream_track_raw_pacing`
//! in lib.rs).

use bytes::Bytes;
use ccc_database::{CccDB, Track};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::broadcast;

use crate::{playlists, EngineState, NowPlaying, RadioSettings};

const SAMPLE_RATE: u32 = 44_100;
const CHANNELS: u32 = 2;
const SAMPLE_BYTES: u32 = 2; // s16
const PCM_BYTES_PER_SEC: u32 = SAMPLE_RATE * CHANNELS * SAMPLE_BYTES; // 176_400
const CHUNK_MS: u64 = 100;
const CHUNK_BYTES: usize = (PCM_BYTES_PER_SEC as u64 * CHUNK_MS / 1000) as usize;

/// One running ffmpeg encoder + a background pump from its stdout to
/// the master broadcast. Drop = process killed.
pub struct EncoderPipeline {
    pub stdin: ChildStdin,
    _child: Child,
    _pump: tokio::task::JoinHandle<()>,
}

impl EncoderPipeline {
    pub async fn spawn(state: &EngineState) -> Result<Self, String> {
        let snap = state.settings.snapshot();
        let mut cmd = Command::new("ffmpeg");
        cmd.arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-f")
            .arg("s16le")
            .arg("-ar")
            .arg(SAMPLE_RATE.to_string())
            .arg("-ac")
            .arg(CHANNELS.to_string())
            .arg("-i")
            .arg("pipe:0")
            .arg("-c:a")
            .arg("libmp3lame")
            .arg("-b:a")
            .arg(format!("{}k", snap.station_bitrate_kbps))
            .arg("-f")
            .arg("mp3")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("ffmpeg encoder spawn failed: {}", e))?;
        let stdin = child.stdin.take().ok_or("no encoder stdin")?;
        let stdout = child.stdout.take().ok_or("no encoder stdout")?;
        let tx = state.tx.clone();
        let pump = tokio::spawn(pump_encoder_stdout(stdout, tx));
        Ok(Self {
            stdin,
            _child: child,
            _pump: pump,
        })
    }
}

async fn pump_encoder_stdout(mut stdout: ChildStdout, tx: broadcast::Sender<Bytes>) {
    let mut buf = vec![0u8; 4096];
    loop {
        match stdout.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let _ = tx.send(Bytes::copy_from_slice(&buf[..n]));
            }
            Err(_) => break,
        }
    }
}

/// One running track decoder. Drop = process killed.
struct Decoder {
    stdout: ChildStdout,
    _child: Child,
}

fn spawn_decoder(track: &Track, settings: &RadioSettings) -> Result<Decoder, String> {
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(&track.file_path);
    if settings.apply_loudness_normalization {
        if let Some(g) = track.target_gain_db {
            cmd.arg("-af").arg(format!("volume={:+.2}dB", g));
        }
    }
    cmd.arg("-f")
        .arg("s16le")
        .arg("-acodec")
        .arg("pcm_s16le")
        .arg("-ar")
        .arg(SAMPLE_RATE.to_string())
        .arg("-ac")
        .arg(CHANNELS.to_string())
        .arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("ffmpeg decoder spawn for '{}' failed: {}", track.title, e))?;
    let stdout = child.stdout.take().ok_or("no decoder stdout")?;
    Ok(Decoder {
        stdout,
        _child: child,
    })
}

/// Run the AutoDJ continuously with PCM crossfading. Returns when the
/// outer state signals preemption (live source / relay / no library).
/// On any unrecoverable error returns Err so the caller can fall back
/// to the per-track raw pacer.
pub async fn run_crossfaded_autodj(state: EngineState) -> Result<(), String> {
    let db = state.db.clone().ok_or("no DB")?;
    let mut encoder = EncoderPipeline::spawn(&state).await?;
    // Holds the not-yet-finished decoder from the previous crossfade.
    let mut carry: Option<(Track, Decoder)> = None;

    loop {
        if !state.source_alive.load(Ordering::SeqCst) && state.relay_alive.load(Ordering::SeqCst) {
            return Ok(());
        }
        if state.source_alive.load(Ordering::SeqCst) {
            return Ok(());
        }

        let active_id: Option<u64> = playlists::pick_active_playlist(&db).map(|p| p.id);
        let queue = pick_queue(&db, active_id);
        if queue.is_empty() {
            *state.now_playing.write().expect("np poisoned") = None;
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }

        for (i, track) in queue.iter().enumerate() {
            if preempted(&state) {
                return Ok(());
            }
            // Schedule may have transitioned to a different playlist.
            if playlists::pick_active_playlist(&db).map(|p| p.id) != active_id {
                break;
            }

            let snap = state.settings.snapshot();
            let xf = (snap.crossfade_seconds as u64).min(10);

            // Decoder for the body phase: either the carry-over from the
            // previous crossfade, or a fresh spawn.
            let (body_track, mut a_dec) = match carry.take() {
                Some((t, d)) => (t, d),
                None => {
                    let d = match spawn_decoder(track, &snap) {
                        Ok(d) => d,
                        Err(e) => return Err(e),
                    };
                    (track.clone(), d)
                }
            };

            announce_track(&state, &body_track);

            // Body phase: real-time PCM passthrough until <xf> seconds remain.
            let dur = body_track.duration_seconds.max(1);
            let body_secs = dur.saturating_sub(xf);
            copy_paced(&mut a_dec.stdout, &mut encoder.stdin, body_secs, &state).await;

            if preempted(&state) {
                return Ok(());
            }
            if playlists::pick_active_playlist(&db).map(|p| p.id) != active_id {
                break;
            }

            // Crossfade with the next track if there is one and xf > 0.
            let next_track = queue.get(i + 1);
            if let (Some(nt), true) = (next_track, xf > 0) {
                let mut b_dec = match spawn_decoder(nt, &snap) {
                    Ok(d) => d,
                    Err(_) => {
                        // Couldn't open next track's decoder — drain A's tail.
                        copy_paced(&mut a_dec.stdout, &mut encoder.stdin, xf, &state).await;
                        carry = None;
                        continue;
                    }
                };
                mix_crossfade(
                    &mut a_dec.stdout,
                    &mut b_dec.stdout,
                    &mut encoder.stdin,
                    xf,
                    &state,
                )
                .await;
                // a_dec is exhausted; b_dec has consumed its first <xf>
                // seconds. Carry it forward as the next iteration's body
                // decoder, with the next-track metadata.
                carry = Some((nt.clone(), b_dec));
            } else {
                // No next track or crossfade disabled — drain A's tail.
                copy_paced(&mut a_dec.stdout, &mut encoder.stdin, xf, &state).await;
                carry = None;
            }
        }
    }
}

fn pick_queue(db: &Arc<CccDB>, active_id: Option<u64>) -> Vec<Track> {
    use rand::seq::SliceRandom;
    let mut q = match active_id {
        Some(pid) => match db.get_playlist(pid).ok().flatten() {
            Some(_) => db.playlist_tracks(pid).unwrap_or_default(),
            None => Vec::new(),
        },
        None => Vec::new(),
    };
    if q.is_empty() {
        q = db.all_tracks().unwrap_or_default();
        q.shuffle(&mut rand::thread_rng());
    }
    q
}

fn preempted(state: &EngineState) -> bool {
    state.autodj_paused.load(Ordering::SeqCst)
        || state.source_alive.load(Ordering::SeqCst)
        || state.relay_alive.load(Ordering::SeqCst)
        || state.presenter_alive.load(Ordering::SeqCst)
}

fn announce_track(state: &EngineState, track: &Track) {
    let np = NowPlaying {
        source: "autodj",
        track_id: Some(track.id),
        title: track.title.clone(),
        artist: track.artist.clone(),
        started_at: crate::unix_now(),
        duration_seconds: track.duration_seconds,
    };
    *state.now_playing.write().expect("np poisoned") = Some(np.clone());
    crate::push_history(state, np);
    println!(
        "[autodj/xf] now playing: {} - {} ({}s)",
        track.artist, track.title, track.duration_seconds
    );
}

/// Copy PCM from decoder stdout to encoder stdin for `seconds`,
/// 100ms chunks paced by wall clock. Stops early on preemption.
async fn copy_paced(
    src: &mut ChildStdout,
    dst: &mut ChildStdin,
    seconds: u64,
    state: &EngineState,
) {
    if seconds == 0 {
        return;
    }
    let total = CHUNK_BYTES * (seconds * 1000 / CHUNK_MS) as usize;
    let mut buf = vec![0u8; CHUNK_BYTES];
    let chunk_period = Duration::from_millis(CHUNK_MS);
    let mut written: usize = 0;
    while written < total {
        if preempted(state) {
            break;
        }
        let n = read_exact_or_less(src, &mut buf).await;
        if n == 0 {
            break;
        }
        if dst.write_all(&buf[..n]).await.is_err() {
            break;
        }
        written += n;
        tokio::time::sleep(chunk_period).await;
    }
}

/// Mix a fixed-duration crossfade window from the two decoders.
/// Equal-power weights: weight_a = cos(pi/2 * t), weight_b = sin(pi/2 * t)
/// where t is the normalized progress through the window.
async fn mix_crossfade(
    a: &mut ChildStdout,
    b: &mut ChildStdout,
    dst: &mut ChildStdin,
    seconds: u64,
    state: &EngineState,
) {
    if seconds == 0 {
        return;
    }
    let chunks_total = (seconds * 1000 / CHUNK_MS) as usize;
    let chunk_period = Duration::from_millis(CHUNK_MS);
    let mut a_buf = vec![0u8; CHUNK_BYTES];
    let mut b_buf = vec![0u8; CHUNK_BYTES];
    let mut out = vec![0u8; CHUNK_BYTES];
    for i in 0..chunks_total {
        if preempted(state) {
            break;
        }
        let a_n = read_exact_or_less(a, &mut a_buf).await;
        let b_n = read_exact_or_less(b, &mut b_buf).await;

        let progress = (i as f32 + 0.5) / chunks_total as f32;
        let (w_a, w_b) = equal_power_weights(progress);

        // Iterate sample-pair-by-sample-pair (4 bytes = stereo s16).
        let pairs = CHUNK_BYTES / 4;
        for p in 0..pairs {
            let off = p * 4;
            let a_l = if off + 2 <= a_n {
                i16::from_le_bytes([a_buf[off], a_buf[off + 1]])
            } else {
                0
            };
            let a_r = if off + 4 <= a_n {
                i16::from_le_bytes([a_buf[off + 2], a_buf[off + 3]])
            } else {
                0
            };
            let b_l = if off + 2 <= b_n {
                i16::from_le_bytes([b_buf[off], b_buf[off + 1]])
            } else {
                0
            };
            let b_r = if off + 4 <= b_n {
                i16::from_le_bytes([b_buf[off + 2], b_buf[off + 3]])
            } else {
                0
            };
            let mix_l = ((a_l as f32) * w_a + (b_l as f32) * w_b).clamp(-32768.0, 32767.0) as i16;
            let mix_r = ((a_r as f32) * w_a + (b_r as f32) * w_b).clamp(-32768.0, 32767.0) as i16;
            let lb = mix_l.to_le_bytes();
            let rb = mix_r.to_le_bytes();
            out[off] = lb[0];
            out[off + 1] = lb[1];
            out[off + 2] = rb[0];
            out[off + 3] = rb[1];
        }

        if dst.write_all(&out).await.is_err() {
            break;
        }
        tokio::time::sleep(chunk_period).await;
    }
}

/// Best-effort fill-the-buffer read. Decoders output PCM at line rate
/// once they've started, so this normally fills `buf`. Returns the
/// number of bytes actually read; 0 if the decoder ended.
async fn read_exact_or_less(src: &mut ChildStdout, buf: &mut [u8]) -> usize {
    let mut filled = 0;
    while filled < buf.len() {
        match src.read(&mut buf[filled..]).await {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => break,
        }
    }
    filled
}

/// Equal-power crossfade weights. At `progress = 0.0` track A is at
/// full gain and B is silent; at `progress = 1.0` A is silent and B
/// is at full gain; in between, A^2 + B^2 = 1 (constant power).
/// Returns `(weight_a, weight_b)`.
fn equal_power_weights(progress: f32) -> (f32, f32) {
    debug_assert!((0.0..=1.0).contains(&progress));
    let half_pi = std::f32::consts::FRAC_PI_2;
    // At progress=0: (cos(0), sin(0)) = (1, 0)  -> A full, B silent
    // At progress=1: (cos(pi/2), sin(pi/2)) = (0, 1)  -> A silent, B full
    let angle = half_pi * progress;
    (angle.cos(), angle.sin())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_power_endpoints() {
        let (a, b) = equal_power_weights(0.0);
        assert!((a - 1.0).abs() < 1e-6, "a at 0 = {a}");
        assert!(b.abs() < 1e-6, "b at 0 = {b}");
        let (a, b) = equal_power_weights(1.0);
        assert!(a.abs() < 1e-6, "a at 1 = {a}");
        assert!((b - 1.0).abs() < 1e-6, "b at 1 = {b}");
    }

    #[test]
    fn equal_power_conserves_total_power() {
        // A^2 + B^2 should equal 1.0 at every point in the fade.
        // We sample at 11 positions and tolerate a 1e-3 error.
        for i in 0..=10 {
            let p = i as f32 / 10.0;
            let (a, b) = equal_power_weights(p);
            let power = a * a + b * b;
            assert!(
                (power - 1.0).abs() < 1e-3,
                "progress={p} a={a} b={b} power={power}"
            );
        }
    }

    #[test]
    fn equal_power_is_strictly_monotonic() {
        let mut prev_a = f32::INFINITY;
        for i in 0..=20 {
            let p = i as f32 / 20.0;
            let (a, _) = equal_power_weights(p);
            assert!(
                a < prev_a,
                "weight_a should decrease, got {a} after {prev_a}"
            );
            prev_a = a;
        }
    }
}

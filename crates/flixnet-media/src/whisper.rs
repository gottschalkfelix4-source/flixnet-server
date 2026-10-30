//! Speech-to-text with whisper.cpp for files without subtitles (scene search).
//!
//! Whisper runs as an external process (the `whisper-cli` binary of whisper.cpp,
//! configured by path), like ffmpeg: a crash cannot take the server down and no C++
//! toolchain is needed to build Flixnet. Both steps get their arguments from typed
//! values only: ffmpeg first extracts the audio as 16 kHz mono WAV, whisper then
//! writes SubRip next to it, which is parsed like any subtitle file.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio_util::sync::CancellationToken;

use crate::process::command;

/// A ggml model of whisper.cpp with its published checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WhisperModel {
    /// Name in the settings (`base-q5_1`, …).
    pub name: &'static str,
    pub file_name: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// Download location of the official whisper.cpp models.
pub const MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// Multilingual models (German and English), smallest first. Checksums are the LFS
/// SHA-256 values of `ggerganov/whisper.cpp` on Hugging Face.
pub const MODELS: &[WhisperModel] = &[
    WhisperModel {
        name: "tiny",
        file_name: "ggml-tiny.bin",
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
        size: 77_691_713,
    },
    WhisperModel {
        name: "base-q5_1",
        file_name: "ggml-base-q5_1.bin",
        sha256: "422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898",
        size: 59_707_625,
    },
    WhisperModel {
        name: "base",
        file_name: "ggml-base.bin",
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
        size: 147_951_465,
    },
    WhisperModel {
        name: "small-q5_1",
        file_name: "ggml-small-q5_1.bin",
        sha256: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
        size: 190_085_487,
    },
    WhisperModel {
        name: "small",
        file_name: "ggml-small.bin",
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
        size: 487_601_967,
    },
];

/// The default model: small, quantized, good enough for German dialogue.
pub const DEFAULT_MODEL: &str = "base-q5_1";

pub fn model(name: &str) -> Option<&'static WhisperModel> {
    MODELS.iter().find(|m| m.name == name)
}

/// Spoken language for whisper (`de`, `en`, …) or automatic detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhisperLanguage {
    Auto,
    /// ISO 639-1 code, validated to two ASCII lowercase letters.
    Code([u8; 2]),
}

impl WhisperLanguage {
    /// `auto`/empty or a two-letter code; anything else is rejected.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() || value.eq_ignore_ascii_case("auto") {
            return Some(Self::Auto);
        }
        let bytes = value.as_bytes();
        (bytes.len() == 2 && bytes.iter().all(u8::is_ascii_lowercase))
            .then(|| Self::Code([bytes[0], bytes[1]]))
    }

    fn as_str(&self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Code(c) => String::from_utf8_lossy(c).into_owned(),
        }
    }
}

/// ffmpeg arguments writing one audio stream of `input` as 16 kHz mono PCM WAV,
/// the format whisper.cpp expects.
pub fn audio_args(input: &Path, audio_stream: u32, output: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.push(input.as_os_str().to_owned());
    args.push("-map".into());
    args.push(format!("0:a:{audio_stream}").into());
    for arg in [
        "-vn",
        "-sn",
        "-dn",
        "-ac",
        "1",
        "-ar",
        "16000",
        "-c:a",
        "pcm_s16le",
        "-f",
        "wav",
    ] {
        args.push(arg.into());
    }
    args.push(output.as_os_str().to_owned());
    args
}

/// whisper.cpp invocation (typed; nothing from requests).
#[derive(Debug, Clone, PartialEq)]
pub struct WhisperArgs {
    pub model: PathBuf,
    pub audio: PathBuf,
    /// Output path without extension; whisper appends `.srt`.
    pub output_base: PathBuf,
    pub language: WhisperLanguage,
    pub threads: u16,
}

impl WhisperArgs {
    pub fn to_args(&self) -> Vec<OsString> {
        vec![
            "-m".into(),
            self.model.as_os_str().to_owned(),
            "-f".into(),
            self.audio.as_os_str().to_owned(),
            "-l".into(),
            self.language.as_str().into(),
            "-t".into(),
            self.threads.clamp(1, 64).to_string().into(),
            // SubRip for the text, JSON for the detected language (`-np` silences
            // the log that would otherwise name it), no live printing.
            "-osrt".into(),
            "-oj".into(),
            "-np".into(),
            "-of".into(),
            self.output_base.as_os_str().to_owned(),
        ]
    }

    fn output(&self, extension: &str) -> PathBuf {
        let mut name = self.output_base.as_os_str().to_owned();
        name.push(".");
        name.push(extension);
        PathBuf::from(name)
    }

    /// Where whisper writes the subtitles.
    pub fn srt_path(&self) -> PathBuf {
        self.output("srt")
    }

    /// Where whisper writes the JSON result.
    pub fn json_path(&self) -> PathBuf {
        self.output("json")
    }
}

/// The language whisper detected (or was told), from its JSON output
/// (`"result": { "language": "de" }`). whisper.cpp escapes only quotes and
/// backslashes in that file, so a transcript with control characters makes it
/// invalid JSON; then the value is looked up in the text.
pub fn detected_language(json: &str) -> Option<String> {
    let value = match serde_json::from_str::<serde_json::Value>(json) {
        Ok(v) => v["result"]["language"].as_str().map(str::to_owned),
        Err(_) => {
            let result = &json[json.find("\"result\"")?..];
            let after = &result[result.find("\"language\"")? + "\"language\"".len()..];
            let after = after.trim_start().strip_prefix(':')?.trim_start();
            let after = after.strip_prefix('"')?;
            Some(after[..after.find('"')?].to_owned())
        }
    }?;
    // ISO 639-1, or the three-letter codes whisper uses for a few (`haw`, `yue`).
    (matches!(value.len(), 2 | 3) && value.bytes().all(|b| b.is_ascii_lowercase())).then_some(value)
}

/// What a transcription produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    /// SubRip text.
    pub srt: String,
    /// Spoken language as whisper detected it (`de`, `en`, …).
    pub language: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum WhisperError {
    #[error("could not start {program}: {message}")]
    Spawn { program: String, message: String },
    #[error("{program} failed: {message}")]
    Failed { program: String, message: String },
    #[error("cancelled")]
    Cancelled,
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

async fn run(
    program: &Path,
    args: &[OsString],
    cancel: &CancellationToken,
) -> Result<(), WhisperError> {
    let name = program.display().to_string();
    let child = command(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| WhisperError::Spawn {
            program: name.clone(),
            message: err.to_string(),
        })?;
    // Dropping the child (cancel) kills the process.
    let out = tokio::select! {
        out = child.wait_with_output() => out?,
        () = cancel.cancelled() => return Err(WhisperError::Cancelled),
    };
    if out.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(WhisperError::Failed {
            program: name,
            message: stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("unknown error")
                .trim()
                .to_owned(),
        })
    }
}

/// One transcription run.
#[derive(Debug, Clone)]
pub struct Transcription<'a> {
    pub ffmpeg: &'a Path,
    /// The whisper.cpp CLI (`whisper-cli`).
    pub whisper: &'a Path,
    pub model: &'a Path,
    pub input: &'a Path,
    pub language: WhisperLanguage,
    pub threads: u16,
    /// Temporary files go here and are removed afterwards.
    pub work_dir: &'a Path,
}

/// Transcribes the first audio stream of the input. `cancel` stops both processes
/// (used when a transcode starts or the job is cancelled).
pub async fn transcribe(
    t: &Transcription<'_>,
    cancel: &CancellationToken,
) -> Result<Transcript, WhisperError> {
    tokio::fs::create_dir_all(t.work_dir).await?;
    let audio = t.work_dir.join("audio.wav");
    let args = WhisperArgs {
        model: t.model.to_owned(),
        audio: audio.clone(),
        output_base: t.work_dir.join("transcript"),
        language: t.language,
        threads: t.threads,
    };
    let result = async {
        run(t.ffmpeg, &audio_args(t.input, 0, &audio), cancel).await?;
        run(t.whisper, &args.to_args(), cancel).await?;
        // Lossy: whisper.cpp can end a segment inside a multi-byte character (it
        // writes tokens, not characters), which must not cost the whole transcript.
        let srt = String::from_utf8_lossy(&tokio::fs::read(args.srt_path()).await?).into_owned();
        let language = match tokio::fs::read(args.json_path()).await {
            Ok(json) => detected_language(&String::from_utf8_lossy(&json)),
            Err(_) => None,
        };
        Ok(Transcript { srt, language })
    }
    .await;
    let _ = tokio::fs::remove_file(&audio).await;
    let _ = tokio::fs::remove_file(args.srt_path()).await;
    let _ = tokio::fs::remove_file(args.json_path()).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(args: &[OsString]) -> String {
        args.iter()
            .map(|a| a.to_string_lossy().replace('\\', "/"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn audio_is_extracted_as_16khz_mono_wav() {
        let args = audio_args(
            Path::new("/m/Film (2020).mkv"),
            0,
            Path::new("/w/audio.wav"),
        );
        assert_eq!(
            joined(&args),
            "-hide_banner -loglevel error -nostdin -y -i /m/Film (2020).mkv -map 0:a:0 \
             -vn -sn -dn -ac 1 -ar 16000 -c:a pcm_s16le -f wav /w/audio.wav"
        );
        assert!(
            args.iter().any(|a| a == "/m/Film (2020).mkv"),
            "path stays one argument"
        );
    }

    #[test]
    fn whisper_arguments_are_typed() {
        let args = WhisperArgs {
            model: "/config/models/whisper/ggml-base-q5_1.bin".into(),
            audio: "/w/audio.wav".into(),
            output_base: "/w/transcript".into(),
            language: WhisperLanguage::parse("de").unwrap(),
            threads: 0,
        };
        assert_eq!(
            joined(&args.to_args()),
            "-m /config/models/whisper/ggml-base-q5_1.bin -f /w/audio.wav -l de -t 1 -osrt -oj -np -of /w/transcript"
        );
        assert_eq!(args.srt_path(), PathBuf::from("/w/transcript.srt"));
        assert_eq!(args.json_path(), PathBuf::from("/w/transcript.json"));
    }

    /// Output of the official whisper.cpp b5454 (v1.9.5) Windows build
    /// (`whisper-cli -m ggml-base-q5_1.bin -l auto -osrt -oj -np`) for a German test
    /// clip: CRLF line endings, a leading space in every cue, the language only in the
    /// JSON (the log naming it is silenced by `-np`).
    const REAL_SRT: &str = "1\r\n00:00:00,000 --> 00:00:08,000\r\n Willkommen zum Test von FlixNet.\r\n\r\n2\r\n00:00:08,000 --> 00:00:16,000\r\n Heute ist ein ruhiger Abend am See.\r\n\r\n3\r\n00:00:16,000 --> 00:00:25,000\r\n Der Lila Elefand tanzt um miter Nacht auf dem gefrorenen See.\r\n\r\n4\r\n00:00:25,000 --> 00:00:32,000\r\n Danach bestellt der Kapitän Dreiteller Tomaten Suppe.\r\n\r\n";
    const REAL_JSON: &str = "{\r\n\t\"systeminfo\": \"WHISPER : VITISAI = 0 | COREML = 0 | OPENVINO = 0 | CPU : SSE3 = 1 | AVX2 = 1 | OPENMP = 1 | REPACK = 1 | \",\r\n\t\"model\": {\r\n\t\t\"type\": \"base\",\r\n\t\t\"multilingual\": true,\r\n\t\t\"vocab\": 51865,\r\n\t\t\"mels\": 80,\r\n\t\t\"ftype\": 9\r\n\t},\r\n\t\"params\": {\r\n\t\t\"model\": \"../cfg/models/whisper/ggml-base-q5_1.bin\",\r\n\t\t\"language\": \"auto\",\r\n\t\t\"translate\": false\r\n\t},\r\n\t\"result\": {\r\n\t\t\"language\": \"de\"\r\n\t},\r\n\t\"transcription\": [\r\n\t\t{\r\n\t\t\t\"timestamps\": {\r\n\t\t\t\t\"from\": \"00:00:00,000\",\r\n\t\t\t\t\"to\": \"00:00:08,000\"\r\n\t\t\t},\r\n\t\t\t\"offsets\": {\r\n\t\t\t\t\"from\": 0,\r\n\t\t\t\t\"to\": 8000\r\n\t\t\t},\r\n\t\t\t\"text\": \" Willkommen zum Test von FlixNet.\"\r\n\t\t}\r\n\t]\r\n}\r\n";

    #[test]
    fn real_whisper_output_is_understood() {
        let cues = crate::transcript::parse_cues(REAL_SRT);
        let got: Vec<(i64, i64, &str)> = cues
            .iter()
            .map(|c| (c.start_ms, c.end_ms, c.text.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (0, 8000, "Willkommen zum Test von FlixNet."),
                (8000, 16000, "Heute ist ein ruhiger Abend am See."),
                (
                    16000,
                    25000,
                    "Der Lila Elefand tanzt um miter Nacht auf dem gefrorenen See."
                ),
                (
                    25000,
                    32000,
                    "Danach bestellt der Kapitän Dreiteller Tomaten Suppe."
                ),
            ]
        );
        assert_eq!(detected_language(REAL_JSON).as_deref(), Some("de"));
        // whisper.cpp does not escape control characters: invalid JSON, same answer.
        let broken = REAL_JSON.replace("Willkommen", "Will\u{1}kommen");
        assert!(serde_json::from_str::<serde_json::Value>(&broken).is_err());
        assert_eq!(detected_language(&broken).as_deref(), Some("de"));
        // The requested language ("params") is not the detected one.
        let no_result = REAL_JSON.replace("\"result\"", "\"other\"");
        assert_eq!(detected_language(&no_result), None);
        assert_eq!(
            detected_language("{\"result\":{\"language\":\"-l x\"}}"),
            None
        );
        assert_eq!(
            detected_language("{\"result\":{\"language\":\"yue\"}}").as_deref(),
            Some("yue")
        );
    }

    #[test]
    fn languages_are_validated() {
        assert_eq!(WhisperLanguage::parse(""), Some(WhisperLanguage::Auto));
        assert_eq!(WhisperLanguage::parse("Auto"), Some(WhisperLanguage::Auto));
        assert_eq!(WhisperLanguage::parse("en").unwrap().as_str(), "en");
        for bad in ["deu", "DE", "d", "-l", "e n"] {
            assert_eq!(WhisperLanguage::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn models_have_checksums() {
        assert!(model(DEFAULT_MODEL).is_some());
        for m in MODELS {
            assert_eq!(m.sha256.len(), 64, "{}", m.name);
            assert!(m.sha256.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(m.file_name.starts_with("ggml-") && m.file_name.ends_with(".bin"));
        }
    }

    #[tokio::test]
    async fn a_missing_binary_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (ffmpeg, whisper) = (dir.path().join("no-ffmpeg"), dir.path().join("no-whisper"));
        let (model, input, work) = (
            dir.path().join("model.bin"),
            dir.path().join("in.mkv"),
            dir.path().join("work"),
        );
        let err = transcribe(
            &Transcription {
                ffmpeg: &ffmpeg,
                whisper: &whisper,
                model: &model,
                input: &input,
                language: WhisperLanguage::Auto,
                threads: 2,
                work_dir: &work,
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WhisperError::Spawn { .. }), "{err}");
    }
}

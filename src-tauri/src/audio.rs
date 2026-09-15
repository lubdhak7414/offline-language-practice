//! PCM helpers + minimal WAV encoder and query-param decoder.
//!
//! No extra dependencies: all encoding is hand-rolled little-endian so the
//! resulting bytes are playable from a frontend `Blob` (`audio/wav`).

/// Convert `f32` mono samples in `[-1.0, 1.0]` to raw little-endian i16 bytes.
///
/// No header is emitted — see [`encode_wav_mono_16bit`] for a full file.
/// Values outside `[-1, 1]` are clamped.
pub fn f32_to_i16_bytes(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        // 32767.0 keeps -1.0..1.0 symmetric and avoids i16::MIN overflow
        // on rounding (e.g. (-1.0 * 32768.0) edge).
        let v = (clamped * 32767.0).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Encode mono `f32` samples as a 44-byte-header RIFF/WAVE (PCM 16-bit) file.
///
/// Layout: 1 channel, 16-bit, PCM (`audioFormat = 1`), little-endian.
/// `sample_rate` is e.g. `22050` (piper default) or `16000`.
pub fn encode_wav_mono_16bit(samples_f32: &[f32], sample_rate: u32) -> Vec<u8> {
    const NUM_CHANNELS: u16 = 1;
    const BITS_PER_SAMPLE: u16 = 16;
    const HEADER_LEN: usize = 44;

    let pcm = f32_to_i16_bytes(samples_f32);
    let data_len = pcm.len() as u32;
    // RIFF chunk size = 36 + data length (i.e. file_len - 8).
    let chunk_size = 36u32.wrapping_add(data_len);
    let byte_rate = sample_rate
        .wrapping_mul(NUM_CHANNELS as u32)
        .wrapping_mul(BITS_PER_SAMPLE as u32 / 8);
    let block_align = NUM_CHANNELS.wrapping_mul(BITS_PER_SAMPLE / 8);

    let mut out = Vec::with_capacity(HEADER_LEN + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&chunk_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // subchunk1 size (PCM)
    out.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
    out.extend_from_slice(&NUM_CHANNELS.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(&pcm);
    debug_assert_eq!(out.len(), HEADER_LEN + pcm.len());
    out
}

/// Parse the sample rate from a RIFF/WAVE header.
///
/// Validates the `RIFF`/`WAVE` magic, then walks subchunks until the
/// `"fmt "` chunk and reads the 4-byte little-endian rate at payload
/// offset 4 (`audioFormat u16`, `numChannels u16`, then `sampleRate u32`).
/// Returns `None` for non-WAVE input, truncated headers, or a zero rate.
pub fn parse_wav_sample_rate(wav: &[u8]) -> Option<u32> {
    if wav.len() < 12 {
        return None;
    }
    if &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return None;
    }
    let mut off = 12usize;
    while off.saturating_add(8) <= wav.len() {
        let id = &wav[off..off + 4];
        let size = u32::from_le_bytes(wav[off + 4..off + 8].try_into().ok()?) as usize;
        let payload = off.saturating_add(8);
        if id == b"fmt " {
            if size < 16 || payload.saturating_add(16) > wav.len() {
                return None;
            }
            let rate = u32::from_le_bytes(wav[payload + 4..payload + 8].try_into().ok()?);
            return if rate == 0 { None } else { Some(rate) };
        }
        // RIFF chunks are word-aligned: skip a pad byte after odd sizes.
        off = payload.saturating_add(size).saturating_add(size % 2);
    }
    None
}

/// Concatenate mono 16-bit WAV blobs into a single valid WAV.
///
/// Strips each chunk's 44-byte RIFF header, verifies matching sample rate,
/// channel count and bits-per-sample, then returns one WAV with the
/// concatenated PCM. `Err` on empty input, truncated/invalid chunks or
/// format mismatch.
pub fn concat_wav_mono16(chunks: Vec<Vec<u8>>) -> Result<Vec<u8>, String> {
    if chunks.is_empty() {
        return Err("concat_wav_mono16: empty input (no chunks)".to_string());
    }
    let mut sample_rate: Option<u32> = None;
    let mut num_channels: Option<u16> = None;
    let mut bits_per_sample: Option<u16> = None;
    let mut pcm_total: usize = 0;
    let mut payloads: Vec<&[u8]> = Vec::with_capacity(chunks.len());

    for (idx, chunk) in chunks.iter().enumerate() {
        if chunk.len() < 44 {
            return Err(format!(
                "concat_wav_mono16: chunk {idx} too short ({} bytes, need 44-byte header)",
                chunk.len()
            ));
        }
        if &chunk[0..4] != b"RIFF" || &chunk[8..12] != b"WAVE" {
            return Err(format!(
                "concat_wav_mono16: chunk {idx} missing RIFF/WAVE magic"
            ));
        }
        if &chunk[12..16] != b"fmt " || &chunk[36..40] != b"data" {
            return Err(format!(
                "concat_wav_mono16: chunk {idx} is not a 44-byte PCM header"
            ));
        }
        let rate =
            u32::from_le_bytes(chunk[24..28].try_into().map_err(|_| {
                format!("concat_wav_mono16: chunk {idx} has unreadable sample rate")
            })?);
        let channels = u16::from_le_bytes(
            chunk[22..24]
                .try_into()
                .map_err(|_| format!("concat_wav_mono16: chunk {idx} has unreadable channels"))?,
        );
        let bits = u16::from_le_bytes(chunk[34..36].try_into().map_err(|_| {
            format!("concat_wav_mono16: chunk {idx} has unreadable bits-per-sample")
        })?);
        if rate == 0 || channels == 0 || bits == 0 {
            return Err(format!(
                "concat_wav_mono16: chunk {idx} has invalid format (rate={rate}, channels={channels}, bits={bits})"
            ));
        }
        match (sample_rate, num_channels, bits_per_sample) {
            (None, None, None) => {
                sample_rate = Some(rate);
                num_channels = Some(channels);
                bits_per_sample = Some(bits);
            }
            (Some(r), Some(c), Some(b)) => {
                if r != rate || c != channels || b != bits {
                    return Err(format!(
                        "concat_wav_mono16: chunk {idx} format mismatch (rate={rate}, channels={channels}, bits={bits}) vs first (rate={r}, channels={c}, bits={b})"
                    ));
                }
            }
            _ => unreachable!(),
        }
        let payload = &chunk[44..];
        pcm_total = pcm_total
            .checked_add(payload.len())
            .ok_or_else(|| "concat_wav_mono16: concatenated PCM too large".to_string())?;
        payloads.push(payload);
    }

    let data_len_u32: u32 = u32::try_from(pcm_total)
        .map_err(|_| "concat_wav_mono16: concatenated PCM too large for WAV".to_string())?;
    let chunk_size = 36u32
        .checked_add(data_len_u32)
        .ok_or_else(|| "concat_wav_mono16: concatenated WAV too large".to_string())?;

    let mut out = Vec::with_capacity(44 + pcm_total);
    out.extend_from_slice(&chunks[0][0..44]);
    out[4..8].copy_from_slice(&chunk_size.to_le_bytes());
    out[40..44].copy_from_slice(&data_len_u32.to_le_bytes());
    for payload in payloads {
        out.extend_from_slice(payload);
    }
    Ok(out)
}

/// Split `text` into sentence chunks of at most `max_chars` chars.
///
/// Sentences break on `.` `!` `?` `;` and newlines (delimiters stay
/// attached to the preceding fragment); fragments are then greedily
/// re-packed with single-space joins, splitting oversized fragments at
/// word boundaries (ultra-long words are hard-cut by char).
pub fn split_sentences(text: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(1);

    // 1. Break into sentence fragments on delimiters.
    let mut sentences: Vec<String> = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        cur.push(ch);
        if matches!(ch, '.' | '!' | '?' | ';' | '\n') {
            let s = cur.trim().to_string();
            if !s.is_empty() {
                sentences.push(s);
            }
            cur.clear();
        }
    }
    let tail = cur.trim().to_string();
    if !tail.is_empty() {
        sentences.push(tail);
    }

    // 2. Hard-split any oversized fragment at word boundaries.
    let mut pieces: Vec<String> = Vec::with_capacity(sentences.len());
    for sent in sentences {
        if sent.chars().count() <= max_chars {
            pieces.push(sent);
            continue;
        }
        let mut piece = String::new();
        for word in sent.split_whitespace() {
            let glue = usize::from(!piece.is_empty());
            if piece.chars().count() + glue + word.chars().count() > max_chars {
                if !piece.is_empty() {
                    pieces.push(std::mem::take(&mut piece));
                }
                if word.chars().count() > max_chars {
                    let mut rest = word;
                    while rest.chars().count() > max_chars {
                        let cut = rest
                            .char_indices()
                            .nth(max_chars)
                            .map(|(i, _)| i)
                            .unwrap_or(rest.len());
                        pieces.push(rest[..cut].to_string());
                        rest = &rest[cut..];
                    }
                    piece = rest.to_string();
                } else {
                    piece = word.to_string();
                }
            } else {
                if !piece.is_empty() {
                    piece.push(' ');
                }
                piece.push_str(word);
            }
        }
        if !piece.trim().is_empty() {
            pieces.push(piece.trim().to_string());
        }
    }

    // 3. Greedily re-pack pieces into chunks of at most `max_chars` chars.
    let mut out: Vec<String> = Vec::new();
    let mut chunk = String::new();
    for piece in pieces {
        let cur_len = chunk.chars().count();
        let need = piece.chars().count() + usize::from(cur_len > 0);
        if cur_len > 0 && cur_len + need > max_chars {
            out.push(std::mem::take(&mut chunk));
            chunk = piece;
        } else {
            if cur_len > 0 {
                chunk.push(' ');
            }
            chunk.push_str(&piece);
        }
    }
    if !chunk.trim().is_empty() {
        out.push(chunk.trim().to_string());
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let hi = bytes.get(i + 1).copied().and_then(hex_val);
                let lo = bytes.get(i + 2).copied().and_then(hex_val);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h << 4) | l);
                        i += 3;
                    }
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Extract a query parameter value from a URI path+query string.
///
/// Minimal decoder handling `%XX` and `+` (as space). No extra deps.
///
/// Example: `decode_query_param("/tts?text=hello%20world", "text")`
/// returns `Some("hello world")`.
pub fn decode_query_param(uri_path_and_query: &str, key: &str) -> Option<String> {
    let query = uri_path_and_query.split_once('?')?.1;
    // Drop any fragment.
    let query = query.split('#').next().unwrap_or(query);
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        if k == key {
            return Some(percent_decode(v));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_160_samples_at_16k() {
        let samples = vec![0.0f32; 160];
        let wav = encode_wav_mono_16bit(&samples, 16_000);
        assert!(wav.starts_with(b"RIFF"), "missing RIFF magic");
        assert_eq!(wav.len(), 44 + 320);
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        // data chunk marker + length
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 320);
        // sample rate round-trips
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
    }

    #[test]
    fn decode_text_param() {
        assert_eq!(
            decode_query_param("/tts?text=hello%20world", "text"),
            Some("hello world".to_string())
        );
        assert_eq!(
            decode_query_param("/tts?text=a+b&voice=x", "text"),
            Some("a b".to_string())
        );
        assert_eq!(decode_query_param("/tts?voice=x", "text"), None);
    }

    #[test]
    fn clamp_and_endian() {
        let raw = f32_to_i16_bytes(&[1.0, -1.0, 2.0, -2.0, 0.0]);
        assert_eq!(raw.len(), 10);
        let v = |i: usize| i16::from_le_bytes([raw[i], raw[i + 1]]);
        assert_eq!(v(0), 32767);
        assert_eq!(v(2), -32767);
        assert_eq!(v(4), 32767); // clamped
        assert_eq!(v(6), -32767); // clamped
        assert_eq!(v(8), 0);
    }

    #[test]
    fn parse_sample_rate_roundtrip() {
        let wav = encode_wav_mono_16bit(&[0.0f32; 8], 22_050);
        assert_eq!(parse_wav_sample_rate(&wav), Some(22_050));
        let wav16 = encode_wav_mono_16bit(&[0.0f32; 8], 16_000);
        assert_eq!(parse_wav_sample_rate(&wav16), Some(16_000));
    }

    #[test]
    fn parse_sample_rate_rejects_garbage() {
        assert_eq!(parse_wav_sample_rate(&[]), None);
        assert_eq!(parse_wav_sample_rate(b"RIFF"), None);
        assert_eq!(parse_wav_sample_rate(b"RIFF....NOTWAVE...."), None);
        assert_eq!(parse_wav_sample_rate(b"NOPE....WAVE........"), None);
        // Truncated right after the magic: no subchunks to walk.
        assert_eq!(parse_wav_sample_rate(b"RIFF\x00\x00\x00\x00WAVE"), None);
    }

    #[test]
    fn split_short_text_stays_whole() {
        let chunks = split_sentences("Hello world.", 180);
        assert_eq!(chunks, vec!["Hello world.".to_string()]);
    }

    #[test]
    fn split_respects_max_chars() {
        let text = "Hello world. How are you today? Fine; thanks!";
        let chunks = split_sentences(text, 14);
        assert!(!chunks.is_empty());
        for c in &chunks {
            assert!(
                c.chars().count() <= 14,
                "chunk too long ({} chars): {c:?}",
                c.chars().count()
            );
        }
        // No words lost or fabricated: word multiset is preserved.
        let mut before: Vec<&str> = text.split_whitespace().collect();
        let mut after: Vec<&str> = chunks
            .iter()
            .flat_map(|c| c.split_whitespace().collect::<Vec<_>>())
            .collect();
        before.sort_unstable();
        after.sort_unstable();
        assert_eq!(before, after);
    }

    #[test]
    fn split_long_sentence_breaks_at_words() {
        let text = "alpha beta gamma delta epsilon zeta eta theta";
        let chunks = split_sentences(text, 12);
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(c.chars().count() <= 12, "chunk too long: {c:?}");
        }
    }

    #[test]
    fn split_empty_is_empty() {
        assert!(split_sentences("", 180).is_empty());
        assert!(split_sentences("   ", 180).is_empty());
    }

    #[test]
    fn concat_two_chunks_roundtrip() {
        let a = encode_wav_mono_16bit(&[0.0, 0.5, -0.5, 1.0], 16_000);
        let b = encode_wav_mono_16bit(&[0.25, -0.25], 16_000);
        let joined = concat_wav_mono16(vec![a.clone(), b.clone()]).unwrap();
        // Header + concatenated PCM.
        assert_eq!(joined.len(), 44 + 8 + 4);
        assert_eq!(&joined[0..4], b"RIFF");
        assert_eq!(&joined[8..12], b"WAVE");
        assert_eq!(parse_wav_sample_rate(&joined), Some(16_000));
        assert_eq!(
            &joined[44..],
            &[a[44..].to_vec(), b[44..].to_vec()].concat()
        );
        let data_len = u32::from_le_bytes(joined[40..44].try_into().unwrap());
        assert_eq!(data_len as usize, joined.len() - 44);
    }

    #[test]
    fn concat_rejects_empty_and_mismatch() {
        assert!(concat_wav_mono16(vec![]).is_err());
        let a = encode_wav_mono_16bit(&[0.0, 0.5], 16_000);
        let b = encode_wav_mono_16bit(&[0.0, 0.5], 22_050);
        assert!(concat_wav_mono16(vec![a.clone(), b]).is_err());
        assert!(concat_wav_mono16(vec![vec![0u8; 10]]).is_err());
    }
}

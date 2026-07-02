//! Filename sanitization for user-supplied strings being used as
//! filesystem paths. Strips path separators, the usual Windows/POSIX
//! illegal characters, leading dots, and trims whitespace.

/// Returns a filename safe to use on both POSIX and Windows filesystems.
/// Strips path separators and the Windows-illegal set, drops control
/// characters, collapses runs of whitespace into a single `_`, and trims
/// leading/trailing dots and whitespace. Returns "untitled" for inputs
/// that would otherwise produce an empty string.
pub fn sanitize_filename(name: &str) -> String {
    // Step 1: replace illegal chars.
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('_'),
            c if c.is_control() => {} // drop control chars
            c => out.push(c),
        }
    }
    // Step 2: collapse runs of whitespace into a single '_'.
    let mut collapsed = String::with_capacity(out.len());
    let mut prev_ws = false;
    for ch in out.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                collapsed.push('_');
            }
            prev_ws = true;
        } else {
            collapsed.push(ch);
            prev_ws = false;
        }
    }
    // Step 3: strip a *single* leading and trailing run of dots and
    // underscores so "..", ".", "  .." all fall through, but
    // "..etc.." becomes "etc" rather than the empty string.
    let trimmed = collapsed
        .trim_start_matches(|c: char| c == '.' || c == '_' || c.is_whitespace())
        .trim_end_matches(|c: char| c == '.' || c == '_' || c.is_whitespace());
    if trimmed.is_empty() {
        "untitled".into()
    } else {
        trimmed.to_string()
    }
}

/// Stricter, ASCII-only sanitizer used for uploaded media filenames:
/// keeps `[A-Za-z0-9.-_]`, maps every other character (including path
/// separators and whitespace) to `-`, and returns `fallback` when the
/// input is empty. Unlike [`sanitize_filename`] this never collapses
/// runs or trims dots — it preserves the exact historical behavior of
/// the podcast/video upload paths, where the result is always prefixed
/// with a timestamp so leading dots are harmless.
pub fn sanitize_filename_ascii(name: &str, fallback: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        fallback.into()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_path_separators() {
        assert_eq!(sanitize_filename("../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_filename("a/b\\c"), "a_b_c");
    }

    #[test]
    fn drops_control_and_windows_illegal() {
        // Trailing `_` is trimmed because the only reason it's there is
        // that `?` got replaced; without the trim we'd ship dangling
        // underscores on operators' filesystems.
        assert_eq!(sanitize_filename("foo:bar*?"), "foo_bar");
    }

    #[test]
    fn falls_back_for_empty() {
        assert_eq!(sanitize_filename("   ..  "), "untitled");
        assert_eq!(sanitize_filename("..."), "untitled");
    }

    #[test]
    fn collapses_whitespace_runs() {
        assert_eq!(sanitize_filename("foo  bar"), "foo_bar");
    }

    #[test]
    fn ascii_variant_maps_everything_else_to_hyphen() {
        // Byte-exact with the old plugin-podcast/plugin-video-podcast
        // local helpers: separators, spaces, and unicode all become '-',
        // dots/dashes/underscores survive, no trimming or collapsing.
        assert_eq!(
            sanitize_filename_ascii("ep 1/final?.mp3", "audio.bin"),
            "ep-1-final-.mp3"
        );
        assert_eq!(
            sanitize_filename_ascii("..weird__name..", "audio.bin"),
            "..weird__name.."
        );
        assert_eq!(sanitize_filename_ascii("", "audio.bin"), "audio.bin");
        assert_eq!(sanitize_filename_ascii("", "video.bin"), "video.bin");
    }
}
